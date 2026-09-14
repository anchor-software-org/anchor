use drm::buffer::DrmModifier;
use gbm::BufferObject;
use std::os::fd::AsFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use wayland_client::Connection;
use wayland_client::EventQueue;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm::{Format as ShmFormat, WlShm};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::Flags;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

#[derive(Debug)]
pub struct DrmBufParams {
    pub fd: OwnedFd,
    pub bo: Arc<BufferObject<()>>,
    pub stride: u32,
    pub offset: u32,
    pub modifier: DrmModifier,
}

impl DrmBufParams {
    /// Fallible clone — duplicates the fd without panicking on failure.
    pub fn try_clone(&self) -> Result<Self, String> {
        Ok(Self {
            fd: self.fd.try_clone().map_err(|e| format!("Failed to dup DMA-BUF fd: {}", e))?,
            bo: Arc::clone(&self.bo),
            stride: self.stride,
            offset: self.offset,
            modifier: self.modifier,
        })
    }

    /// Build from individual parts (used by KMS backend where fields come from CachedFb).
    pub fn try_clone_from(
        fd: &OwnedFd,
        bo: &Arc<BufferObject<()>>,
        stride: u32,
        offset: u32,
        modifier: DrmModifier,
    ) -> Result<Self, String> {
        Ok(Self {
            fd: fd.try_clone().map_err(|e| format!("Failed to dup DMA-BUF fd: {}", e))?,
            bo: Arc::clone(bo),
            stride,
            offset,
            modifier,
        })
    }
}

impl Clone for DrmBufParams {
    fn clone(&self) -> Self {
        self.try_clone().expect("Failed to clone DrmBufParams (fd dup failed)")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BufferState {
    SETUP,   // when setting up
    PARAMS,  // when params and output are setup
    WAITING, // waiting for copy to finish
    READY,   // done copying
    FAILED,  // compositor rejected the frame (output removed, etc.)
}

#[derive(Debug)]
pub struct WaylandAnchorObject {
    pub state: AnchorState,
    pub queue: EventQueue<AnchorState>,
}

impl Default for WaylandAnchorObject {
    fn default() -> Self {
        Self::new()
    }
}

impl WaylandAnchorObject {
    /// Create with hard-coded defaults (for tests). Real init should use `try_new()`.
    pub fn new() -> WaylandAnchorObject {
        let conn = Connection::connect_to_env().unwrap();
        let display = conn.display();
        let event_queue = conn.new_event_queue();

        let qh = event_queue.handle();
        let _registry = display.get_registry(&qh, ());

        let mut wao = WaylandAnchorObject {
            state: AnchorState {
                zwlr_screenshot_manager_v1: None,
                zwlr_linux_dmabuf: None,
                wl_shm: None,
                wl_outputs: Vec::new(),
                done_roundtrip: false,
                dmabuf_params: DmabufParams { buffer_width: 0, buffer_height: 0, buffer_format: 0 },
                shm_params: ShmParams::default(),
                buf_state: BufferState::SETUP,
                last_presentation_time_ns: None,
            },
            queue: event_queue,
        };

        wao.queue.roundtrip(&mut wao.state).unwrap();

        wao
    }

    /// Fallible construction — returns the reason why Wayland capture can't work.
    pub fn try_new() -> Result<WaylandAnchorObject, String> {
        let conn = Connection::connect_to_env().map_err(|_| {
            "Wayland display not available (not running a Wayland session?)".to_string()
        })?;
        let display = conn.display();
        let event_queue = conn.new_event_queue();

        let qh = event_queue.handle();
        let _registry = display.get_registry(&qh, ());

        let mut wao = WaylandAnchorObject {
            state: AnchorState {
                zwlr_screenshot_manager_v1: None,
                zwlr_linux_dmabuf: None,
                wl_shm: None,
                wl_outputs: Vec::new(),
                done_roundtrip: false,
                dmabuf_params: DmabufParams { buffer_width: 0, buffer_height: 0, buffer_format: 0 },
                shm_params: ShmParams::default(),
                buf_state: BufferState::SETUP,
                last_presentation_time_ns: None,
            },
            queue: event_queue,
        };

        wao.queue
            .roundtrip(&mut wao.state)
            .map_err(|e| format!("Wayland compositor connection failed: {}", e))?;

        Ok(wao)
    }

    pub fn initialize_capture_params(&mut self, output_index: usize) -> Result<(), String> {
        let scm: &ZwlrScreencopyManagerV1 =
            self.state.zwlr_screenshot_manager_v1.as_ref().ok_or_else(|| {
                "Capture protocol (zwlr_screencopy_manager_v1) not bound".to_string()
            })?;

        if self.state.wl_outputs.is_empty() {
            return Err("No displays available for capture".to_string());
        }
        if output_index >= self.state.wl_outputs.len() {
            return Err(format!(
                "Output index {} out of range ({} outputs available)",
                output_index,
                self.state.wl_outputs.len()
            ));
        }

        // Trigger parameter negotiation for the specified output.
        // cursor_overlay=1 includes the cursor in the capture.
        let _setup_frame = scm.capture_output(
            1,
            &self.state.wl_outputs[output_index].output,
            &self.queue.handle(),
            (),
        );

        Ok(())
    }

    /**
     * Creates a new screencopy frame for capturing the output.
     * This method uses the screenshot manager to capture the first available output
     * and returns a ZwlrScreencopyFrameV1 object that can be used to copy screen content.
     * # Returns
     * A ZwlrScreencopyFrameV1 representing the screencopy frame for the first output.
     * # Errors
     * Returns an error if the screencopy manager is unavailable or the output index is stale.
     */
    pub fn create_frame(&self, output_index: usize) -> Result<ZwlrScreencopyFrameV1, String> {
        let scm: &ZwlrScreencopyManagerV1 =
            self.state.zwlr_screenshot_manager_v1.as_ref().ok_or_else(|| {
                "Capture protocol (zwlr_screencopy_manager_v1) not bound".to_string()
            })?;
        let output = self
            .state
            .wl_outputs
            .get(output_index)
            .ok_or_else(|| format!("Output index {output_index} is unavailable"))?;
        Ok(scm.capture_output(
            1, // cursor_overlay=1: include cursor
            &output.output,
            &self.queue.handle(),
            (),
        ))
    }

    pub fn try_setup_wlbuffer(&mut self) -> Result<(DrmBufParams, WlBuffer), String> {
        // Clone we need so we can release the immutable borrow before mutable use.
        let dmabuf = self.state.zwlr_linux_dmabuf.clone()
            .ok_or_else(|| "DMA-BUF protocol (zwp_linux_dmabuf_v1) not available — GPU buffer creation not supported".to_string())?;

        let buf_params: DrmBufParams =
            crate::anchorwayland::wayland_mem::try_create_gpu_buf(&mut self.state)?;

        let params = dmabuf.create_params(&self.queue.handle(), ());

        params.add(buf_params.fd.as_fd(), 0, buf_params.offset, buf_params.stride, 0, 0);

        let width = i32::try_from(self.state.dmabuf_params.buffer_width)
            .map_err(|_| "DMA-BUF width exceeds Wayland's i32 limit")?;
        let height = i32::try_from(self.state.dmabuf_params.buffer_height)
            .map_err(|_| "DMA-BUF height exceeds Wayland's i32 limit")?;
        let wl_buffer: WlBuffer = params.create_immed(
            width,
            height,
            self.state.dmabuf_params.buffer_format,
            Flags::empty(),
            &self.queue.handle(),
            (),
        );

        Ok((buf_params, wl_buffer))
    }
}

#[derive(Debug)]
pub struct AnchorState {
    pub zwlr_screenshot_manager_v1: Option<ZwlrScreencopyManagerV1>,
    pub zwlr_linux_dmabuf: Option<ZwpLinuxDmabufV1>,
    pub wl_shm: Option<WlShm>,
    pub wl_outputs: Vec<WloutputInfo>,
    pub dmabuf_params: DmabufParams,
    pub shm_params: ShmParams,
    pub done_roundtrip: bool,
    pub buf_state: BufferState,
    /// Presentation timestamp from the most recently ready screencopy frame.
    /// The wlr-screencopy protocol does not specify its clock epoch, so callers
    /// must only compare timestamps from the same compositor connection.
    pub last_presentation_time_ns: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ShmParams {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: Option<ShmFormat>,
}

#[derive(Debug)]
pub struct DmabufParams {
    pub buffer_width: u32,
    pub buffer_height: u32,
    pub buffer_format: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WloutputInfo {
    pub output: WlOutput,
    pub global_name: u32,
    pub width: i32,
    pub height: i32,
    pub name: String,
    pub description: String,
    pub x: i32,
    pub y: i32,
}
