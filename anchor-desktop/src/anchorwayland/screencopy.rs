//! Wayland screencopy backend using wlr-screencopy-unstable-v1 protocol.
//!
//! This backend captures screen content via the compositor using the
//! zwlr_screencopy_manager_v1 Wayland protocol extension.
//!
//! **Advantages:**
//! - Safe, compositor-friendly
//! - Works on all Wayland compositors that support wlr-screencopy
//! - No special permissions required
//!
//! **Limitations:**
//! - Frame rate limited by compositor (typically ~30fps on Sway)
//! - Adds compositor overhead/latency
//!
//! **How it works:**
//! 1. Connect to Wayland display and enumerate outputs
//! 2. Request screencopy manager from compositor
//! 3. For each frame:
//!    a. Create screencopy frame for selected output
//!    b. Copy to pre-allocated DMA-BUF backed wl_buffer
//!    c. Wait for compositor to signal READY
//!    d. Pass DMA-BUF to encoder for zero-copy encoding

use crate::anchorwayland::capture_backend::{
    CaptureBackend, CapturedFrame, CpuBuffer, OutputInfo, PixelFormat,
};
use crate::anchorwayland::wayland_objects::{BufferState, DrmBufParams, WaylandAnchorObject};
use std::ffi::CString;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Instant;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::Format as ShmFormat;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;

/// How a wlr-screencopy request is delivered by the compositor.
///
/// `EveryFrame` maps to the protocol's `copy` request. `OnlyOnDamage` maps to
/// `copy_with_damage`, which waits for output damage and reports its regions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreencopyMode {
    #[default]
    EveryFrame,
    OnlyOnDamage,
}

#[derive(Debug, PartialEq, Eq)]
struct ShmBufferLayout {
    len: usize,
    len_i32: i32,
    width: i32,
    height: i32,
    stride: i32,
}

/// Validates compositor-provided wl_shm dimensions before allocation or protocol narrowing.
fn shm_buffer_layout(width: u32, height: u32, stride: u32) -> Result<ShmBufferLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!("Invalid wl_shm dimensions: {width}x{height}"));
    }
    let minimum_stride =
        width.checked_mul(4).ok_or_else(|| format!("wl_shm width is too large: {width}"))?;
    if stride < minimum_stride {
        return Err(format!(
            "Invalid wl_shm stride: {stride} bytes cannot hold a {width}-pixel XRGB row"
        ));
    }

    let len = usize::try_from(stride)
        .ok()
        .and_then(|stride| {
            usize::try_from(height).ok().and_then(|height| stride.checked_mul(height))
        })
        .ok_or_else(|| format!("wl_shm buffer size overflows: stride={stride}, height={height}"))?;
    let len_i32 = i32::try_from(len)
        .map_err(|_| format!("wl_shm buffer size exceeds protocol limit: {len}"))?;

    Ok(ShmBufferLayout {
        len,
        len_i32,
        width: i32::try_from(width)
            .map_err(|_| format!("wl_shm width exceeds protocol limit: {width}"))?,
        height: i32::try_from(height)
            .map_err(|_| format!("wl_shm height exceeds protocol limit: {height}"))?,
        stride: i32::try_from(stride)
            .map_err(|_| format!("wl_shm stride exceeds protocol limit: {stride}"))?,
    })
}

fn output_dimensions(width: i32, height: i32) -> Option<(u32, u32)> {
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// Wayland screencopy backend implementation.
pub struct WaylandScreencopyBackend {
    /// Main Wayland anchor object (handles connection, state, queue)
    wao: WaylandAnchorObject,

    /// Current buffer parameters (DMA-BUF info)
    buf_params: Option<DrmBufParams>,

    /// Wayland buffer object for screencopy target
    wl_buffer: Option<WlBuffer>,
    cpu_buffer: Option<Arc<CpuBuffer>>,
    cpu_capture: bool,
    cpu_capture_unavailable: bool,

    /// Whether the backend is initialized
    initialized: bool,

    /// Last known buffer format/size for change detection
    last_format: Option<u32>,
    last_width: Option<u32>,
    last_height: Option<u32>,

    /// Currently initialized output index
    current_output_index: usize,

    /// Whether capture requests wait for output damage rather than accepting
    /// the next compositor frame.
    mode: ScreencopyMode,
}

impl Default for WaylandScreencopyBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WaylandScreencopyBackend {
    fn release_wl_buffer(&mut self) {
        if let Some(buffer) = self.wl_buffer.take() {
            buffer.destroy();
        }
    }

    /// Create a new uninitialized Wayland screencopy backend.
    ///
    /// Construction will fail early (returning Err) if:
    /// - No Wayland display (X11 or console session)
    /// - Compositor roundtrip fails
    pub fn try_new() -> Result<Self, String> {
        let wao = WaylandAnchorObject::try_new()?;
        Ok(Self {
            wao,
            buf_params: None,
            wl_buffer: None,
            cpu_buffer: None,
            cpu_capture: false,
            cpu_capture_unavailable: false,
            initialized: false,
            last_format: None,
            last_width: None,
            last_height: None,
            current_output_index: 0,
            mode: ScreencopyMode::EveryFrame,
        })
    }

    /// Create with hard-coded defaults (for tests only — will panic if Wayland is unavailable).
    pub fn new() -> Self {
        Self {
            wao: WaylandAnchorObject::new(),
            buf_params: None,
            wl_buffer: None,
            cpu_buffer: None,
            cpu_capture: false,
            cpu_capture_unavailable: false,
            initialized: false,
            last_format: None,
            last_width: None,
            last_height: None,
            current_output_index: 0,
            mode: ScreencopyMode::EveryFrame,
        }
    }

    /// Select protocol delivery semantics before starting capture.
    ///
    /// The mode can also be changed while idle.
    pub fn set_mode(&mut self, mode: ScreencopyMode) {
        self.mode = mode;
    }

    pub fn mode(&self) -> ScreencopyMode {
        self.mode
    }

    /// Returns the wlr-screencopy `ready` presentation timestamp for the last
    /// completed frame. Its epoch is compositor-defined; only differences
    /// between values from this same capture session are meaningful.
    pub fn last_presentation_time_ns(&self) -> Option<u64> {
        self.wao.state.last_presentation_time_ns
    }

    /// Wait for compositor to provide buffer parameters
    fn wait_for_buffer_params(&mut self) -> Result<(), String> {
        loop {
            self.wao
                .queue
                .blocking_dispatch(&mut self.wao.state)
                .map_err(|e| format!("Wayland dispatch error: {}", e))?;

            if self.wao.state.buf_state == BufferState::PARAMS {
                break;
            }
        }
        Ok(())
    }

    /// Setup wl_buffer for screencopy target
    fn try_setup_buffer(&mut self) -> Result<(), String> {
        let (buf_params, wl_buffer) = self.wao.try_setup_wlbuffer()?;
        self.buf_params = Some(buf_params);
        self.wl_buffer = Some(wl_buffer);
        Ok(())
    }

    fn setup_cpu_buffer(&mut self) -> Result<(), String> {
        let shm = self.wao.state.wl_shm.clone().ok_or("wl_shm is not available")?;
        let params = &self.wao.state.shm_params;
        let format = params.format.ok_or("Compositor did not advertise a wl_shm format")?;
        shm_pixel_format(format)?;
        let layout = shm_buffer_layout(params.width, params.height, params.stride)?;
        let len = layout.len;

        let name = CString::new("anchor-capture").unwrap();
        let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if raw_fd < 0 {
            return Err(format!("memfd_create failed: {}", std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let file_len = libc::off_t::try_from(len)
            .map_err(|_| format!("wl_shm buffer size exceeds file limit: {len}"))?;
        if unsafe { libc::ftruncate(raw_fd, file_len) } != 0 {
            return Err(format!("ftruncate failed: {}", std::io::Error::last_os_error()));
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                raw_fd,
                0,
            )
        };
        let ptr = NonNull::new(ptr.cast::<u8>())
            .filter(|_| ptr != libc::MAP_FAILED)
            .ok_or_else(|| format!("mmap failed: {}", std::io::Error::last_os_error()))?;

        let pool = shm.create_pool(fd.as_fd(), layout.len_i32, &self.wao.queue.handle(), ());
        let wl_buffer = pool.create_buffer(
            0,
            layout.width,
            layout.height,
            layout.stride,
            format,
            &self.wao.queue.handle(),
            (),
        );
        pool.destroy();
        self.release_wl_buffer();
        self.wl_buffer = Some(wl_buffer);
        self.cpu_buffer = Some(Arc::new(CpuBuffer::new(ptr, len)));
        self.buf_params = None;
        self.cpu_capture = true;
        log::info!("Switched Wayland capture to reusable wl_shm buffer ({} bytes)", len);
        Ok(())
    }

    /// Reinitialize buffer for a different output
    fn reinit_buffer(&mut self, output_index: usize) -> Result<(), String> {
        let cpu_capture = self.cpu_capture;
        self.release_wl_buffer();
        self.cpu_buffer = None;
        self.wao.state.shm_params = Default::default();
        self.wao.state.buf_state = BufferState::SETUP;
        self.wao
            .initialize_capture_params(output_index)
            .map_err(|e| format!("Failed to reinitialize capture params: {}", e))?;

        self.wait_for_buffer_params()?;
        if cpu_capture {
            self.setup_cpu_buffer()?;
        } else {
            self.try_setup_buffer()?;
        }
        Ok(())
    }
}

impl CaptureBackend for WaylandScreencopyBackend {
    fn init(&mut self) -> Result<(), String> {
        log::info!("Initializing Wayland screencopy backend...");

        // Initial dispatch to process compositor globals.
        // The roundtrip in try_new() already set done_roundtrip if outputs exist;
        // if not, we wait for one event then give up — no outputs means nothing to capture.
        let t = Instant::now();
        loop {
            if self.wao.state.done_roundtrip {
                break;
            }
            self.wao
                .queue
                .blocking_dispatch(&mut self.wao.state)
                .map_err(|e| format!("Initial dispatch error: {}", e))?;
        }
        log::debug!("Initial roundtrip took: {:?}", t.elapsed());

        // Verify screencopy manager is available
        if self.wao.state.zwlr_screenshot_manager_v1.is_none() {
            return Err(
                "Screen capture protocol (zwlr_screencopy_manager_v1) not available — your compositor doesn't support wlr-screencopy".to_string(),
            );
        }

        // Verify at least one output is present
        if self.wao.state.wl_outputs.is_empty() {
            return Err("No displays available for capture".to_string());
        }

        // Verify DMA-BUF protocol is available (needed for GPU zero-copy)
        if self.wao.state.zwlr_linux_dmabuf.is_none() {
            return Err(
                "DMA-BUF protocol (zwp_linux_dmabuf_v1) not available — GPU buffer sharing not supported".to_string(),
            );
        }

        // Initialize capture params for output 0
        let t = Instant::now();
        self.wao.state.buf_state = BufferState::SETUP;
        self.wao
            .initialize_capture_params(0)
            .map_err(|e| format!("Failed to initialize capture params: {}", e))?;
        log::debug!("Capture params init took: {:?}", t.elapsed());

        // Wait for buffer parameters from compositor
        self.wait_for_buffer_params()?;

        // Setup wl_buffer
        self.try_setup_buffer()?;

        self.initialized = true;
        self.current_output_index = 0; // Initialized for output 0
        log::info!("Wayland screencopy backend initialized successfully");
        Ok(())
    }

    fn capture_frame(&mut self, output_index: usize) -> Result<CapturedFrame, String> {
        if !self.initialized {
            return Err("Backend not initialized".to_string());
        }

        // If output changed, reinitialize buffer for new output
        if output_index != self.current_output_index {
            log::info!(
                "Output changed from {} to {}, reinitializing buffer",
                self.current_output_index,
                output_index
            );
            self.reinit_buffer(output_index)?;
            self.current_output_index = output_index;
        }

        let wl_buffer = self.wl_buffer.as_ref().ok_or("No wl_buffer available")?;

        // Phase 1: Create screencopy frame and request copy
        let frame: ZwlrScreencopyFrameV1 = self.wao.create_frame(output_index)?;
        self.wao.state.buf_state = BufferState::WAITING;
        self.wao.state.last_presentation_time_ns = None;
        match self.mode {
            ScreencopyMode::EveryFrame => frame.copy(wl_buffer),
            ScreencopyMode::OnlyOnDamage => frame.copy_with_damage(wl_buffer),
        }

        // Phase 2: Wait for compositor to signal READY
        loop {
            self.wao
                .queue
                .blocking_dispatch(&mut self.wao.state)
                .map_err(|e| format!("Dispatch error during capture: {}", e))?;

            if self.wao.state.buf_state == BufferState::READY {
                break;
            }
            if self.wao.state.buf_state == BufferState::FAILED {
                self.wao.state.buf_state = BufferState::PARAMS;
                return Err("Screencopy frame failed".to_string());
            }
        }

        // Reset buffer state for next capture
        // The buffer is reused, but we need to reset the state machine
        self.wao.state.buf_state = BufferState::PARAMS;

        // Phase 3: Extract frame info
        let (width, height, stride, format) = if self.cpu_capture {
            let params = &self.wao.state.shm_params;
            let format = params.format.ok_or("Missing wl_shm format")?;
            (params.width, params.height, params.stride, shm_pixel_format(format)?)
        } else {
            let params = self.buf_params.as_ref().ok_or("No buffer params")?;
            let fourcc = self.wao.state.dmabuf_params.buffer_format;
            let format = PixelFormat::from_drm_fourcc(fourcc)
                .ok_or_else(|| format!("Unsupported pixel format: 0x{fourcc:08x}"))?;
            (
                self.wao.state.dmabuf_params.buffer_width,
                self.wao.state.dmabuf_params.buffer_height,
                params.stride,
                format,
            )
        };
        let format_fourcc = format.to_drm_fourcc();

        // Log format changes (but not every frame)
        let format_changed = self.last_format != Some(format_fourcc)
            || self.last_width != Some(width)
            || self.last_height != Some(height);

        if format_changed {
            log::info!("Compositor buffer: {}x{} format=0x{:08x}", width, height, format_fourcc);
            self.last_format = Some(format_fourcc);
            self.last_width = Some(width);
            self.last_height = Some(height);
        }

        // Create CapturedFrame with DMA-BUF reference
        // Note: We're reusing the same buf_params each frame - the DMA-BUF fd
        // stays valid and the compositor writes new content to the same buffer.
        // The Arc<BufferObject> allows us to safely share the buffer.
        Ok(CapturedFrame {
            dma_buf: if self.cpu_capture {
                None
            } else {
                Some(self.buf_params.as_ref().ok_or("No buffer params")?.try_clone()?)
            },
            pixels: None,
            cpu_buffer: self.cpu_buffer.clone(),
            width,
            height,
            stride,
            format,
            timestamp_us: Instant::now().duration_since(Instant::now()).as_micros() as u64,
        })
    }

    fn get_outputs(&self) -> Vec<OutputInfo> {
        self.wao
            .state
            .wl_outputs
            .iter()
            .enumerate()
            .filter_map(|(i, wl_out)| {
                let Some((width, height)) = output_dimensions(wl_out.width, wl_out.height) else {
                    log::warn!(
                        "Ignoring Wayland output {} with invalid dimensions {}x{}",
                        wl_out.name,
                        wl_out.width,
                        wl_out.height
                    );
                    return None;
                };
                Some(OutputInfo {
                    id: i,
                    name: wl_out.name.clone(),
                    description: wl_out.description.clone(),
                    width,
                    height,
                    refresh_rate_mhz: 60000,
                    x: wl_out.x,
                    y: wl_out.y,
                })
            })
            .collect()
    }

    fn refresh_outputs(&mut self) {
        // Two roundtrips: first processes registry events (new/removed globals),
        // second receives output events (name, mode, done) for newly bound outputs.
        for _ in 0..2 {
            if let Err(e) = self.wao.queue.roundtrip(&mut self.wao.state) {
                log::warn!("Wayland roundtrip failed during output refresh: {}", e);
                break;
            }
        }
    }

    fn supports_zero_copy_encoding(&self) -> bool {
        !self.cpu_capture
    }

    fn prefer_cpu_capture(&mut self) -> Result<(), String> {
        if self.cpu_capture {
            return Ok(());
        }
        if self.cpu_capture_unavailable {
            return Ok(());
        }
        if let Err(error) = self.setup_cpu_buffer() {
            self.cpu_capture_unavailable = true;
            return Err(error);
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        log::info!("Shutting down Wayland screencopy backend");
        self.buf_params = None;
        self.release_wl_buffer();
        self.cpu_buffer = None;
        self.cpu_capture = false;
        self.cpu_capture_unavailable = false;
        self.initialized = false;
        self.current_output_index = 0;
    }
}

fn shm_pixel_format(format: ShmFormat) -> Result<PixelFormat, String> {
    match format {
        ShmFormat::Xrgb8888 => Ok(PixelFormat::Xrgb8888),
        ShmFormat::Argb8888 => Ok(PixelFormat::Argb8888),
        ShmFormat::Xbgr8888 => Ok(PixelFormat::Xbgr8888),
        ShmFormat::Abgr8888 => Ok(PixelFormat::Abgr8888),
        other => Err(format!("Unsupported wl_shm format: {other:?}")),
    }
}

impl Drop for WaylandScreencopyBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_shm_formats_without_channel_swaps() {
        assert_eq!(shm_pixel_format(ShmFormat::Xrgb8888).unwrap(), PixelFormat::Xrgb8888);
        assert_eq!(shm_pixel_format(ShmFormat::Argb8888).unwrap(), PixelFormat::Argb8888);
        assert_eq!(shm_pixel_format(ShmFormat::Xbgr8888).unwrap(), PixelFormat::Xbgr8888);
        assert_eq!(shm_pixel_format(ShmFormat::Abgr8888).unwrap(), PixelFormat::Abgr8888);
    }

    #[test]
    fn shm_buffer_layout_accepts_representable_xrgb_buffers() {
        assert_eq!(
            shm_buffer_layout(1920, 1080, 7680).unwrap(),
            ShmBufferLayout {
                len: 8_294_400,
                len_i32: 8_294_400,
                width: 1920,
                height: 1080,
                stride: 7680,
            }
        );
    }

    #[test]
    fn shm_buffer_layout_rejects_invalid_or_unrepresentable_geometry() {
        assert!(shm_buffer_layout(0, 1080, 7680).is_err());
        assert!(shm_buffer_layout(1920, 0, 7680).is_err());
        assert!(shm_buffer_layout(1920, 1080, 7679).is_err());
        assert!(shm_buffer_layout(i32::MAX as u32 + 1, 1, u32::MAX).is_err());
    }

    #[test]
    fn output_dimensions_reject_negative_wayland_modes() {
        assert_eq!(output_dimensions(1920, 1080), Some((1920, 1080)));
        assert_eq!(output_dimensions(-1, 1080), None);
        assert_eq!(output_dimensions(1920, -1), None);
    }
}
