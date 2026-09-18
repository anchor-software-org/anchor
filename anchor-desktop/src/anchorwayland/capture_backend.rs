//! Abstract capture backend trait for supporting multiple screen capture methods.
//!
//! This module defines the `CaptureBackend` trait which allows anchor to support
//! different capture methods:
//! - Wayland screencopy (wlr-screencopy protocol)
//! - KMS/DRM capture (direct kernel framebuffer access)
//!
//! Each backend implements the same interface, providing frames that can be
//! encoded with zero-copy DMA-BUF or CPU fallback paths.

use crate::anchorwayland::wayland_objects::DrmBufParams;
use std::time::Instant;
use std::{ptr::NonNull, sync::Arc};

/// Reusable compositor-owned shared-memory capture buffer.
pub struct CpuBuffer {
    ptr: NonNull<u8>,
    len: usize,
}

impl CpuBuffer {
    pub fn new(ptr: NonNull<u8>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

unsafe impl Send for CpuBuffer {}
unsafe impl Sync for CpuBuffer {}

impl Drop for CpuBuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

/// Abstract interface for screen capture backends.
///
/// Implementations must be Send to work with the plugin threading model.
pub trait CaptureBackend: Send {
    /// Initialize the capture backend.
    ///
    /// This should:
    /// - Enumerate available outputs
    /// - Set up capture resources
    /// - Verify permissions/capabilities
    ///
    /// Returns Err if the backend cannot be initialized (missing permissions,
    /// unsupported environment, etc.)
    fn init(&mut self) -> Result<(), String>;

    /// Capture a single frame from the specified output.
    ///
    /// This is a blocking call that returns when a frame is ready.
    /// The frame may contain:
    /// - DMA-BUF handle for zero-copy encoding
    /// - Raw pixel data for CPU fallback
    ///
    /// # Arguments
    /// * `output_index` - Index into the outputs list from `get_outputs()`
    ///
    /// # Returns
    /// * `Ok(CapturedFrame)` - Successfully captured frame
    /// * `Err(String)` - Capture failed (compositor disconnected, DRM error, etc.)
    fn capture_frame(&mut self, output_index: usize) -> Result<CapturedFrame, String>;

    /// Get list of available outputs (displays/monitors).
    ///
    /// Called during initialization and when outputs change.
    /// Output indices are stable for the lifetime of the backend.
    fn get_outputs(&self) -> Vec<OutputInfo>;

    /// Re-enumerate available outputs. Called periodically to detect hotplug changes.
    fn refresh_outputs(&mut self) {}

    /// Whether this backend supports zero-copy DMA-BUF encoding.
    ///
    /// If true, `capture_frame()` will populate `CapturedFrame.dma_buf`.
    /// If false, only `CapturedFrame.pixels` will be populated.
    fn supports_zero_copy_encoding(&self) -> bool;

    /// Switch subsequent frames to a CPU-resident capture buffer when supported.
    fn prefer_cpu_capture(&mut self) -> Result<(), String> {
        Err("CPU capture is not supported by this backend".into())
    }

    /// Clean shutdown of the backend.
    ///
    /// Called when stopping capture or switching backends.
    /// Should release all resources (file descriptors, connections, etc.)
    fn shutdown(&mut self);
}

/// A captured frame from any backend.
///
/// Frames can provide either:
/// 1. DMA-BUF handle for zero-copy GPU encoding
/// 2. Raw pixel data for CPU encoding
///
/// The encoder will prefer DMA-BUF if available.
pub struct CapturedFrame {
    /// DMA-BUF parameters for zero-copy encoding.
    ///
    /// Contains:
    /// - File descriptor
    /// - GBM buffer object
    /// - Stride, offset, format modifier
    ///
    /// Present when `CaptureBackend::supports_zero_copy_encoding()` is true.
    pub dma_buf: Option<DrmBufParams>,

    /// Raw pixel data for CPU fallback.
    ///
    /// Layout depends on `format` and `stride`.
    /// Used when DMA-BUF is unavailable or CPU encoding is preferred.
    pub pixels: Option<Vec<u8>>,

    /// Reusable wl_shm mapping. Valid until the next capture into this buffer;
    /// capture and encoding are sequential, so consumers see a stable frame.
    pub cpu_buffer: Option<Arc<CpuBuffer>>,

    /// Frame width in pixels
    pub width: u32,

    /// Frame height in pixels
    pub height: u32,

    /// Row stride in bytes (may be > width * bytes_per_pixel due to alignment)
    pub stride: u32,

    /// Pixel format of the captured data
    pub format: PixelFormat,

    /// Capture timestamp in microseconds (for frame timing)
    pub timestamp_us: u64,
}

impl CapturedFrame {
    /// Create a new frame with just DMA-BUF (zero-copy path)
    pub fn from_dmabuf(
        dma_buf: DrmBufParams,
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    ) -> Self {
        Self {
            dma_buf: Some(dma_buf),
            pixels: None,
            cpu_buffer: None,
            width,
            height,
            stride,
            format,
            timestamp_us: Instant::now().duration_since(Instant::now()).as_micros() as u64,
        }
    }

    /// Create a new frame with raw pixels (CPU fallback path)
    pub fn from_pixels(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    ) -> Self {
        Self {
            dma_buf: None,
            pixels: Some(pixels),
            cpu_buffer: None,
            width,
            height,
            stride,
            format,
            timestamp_us: Instant::now().duration_since(Instant::now()).as_micros() as u64,
        }
    }
}

/// Information about an available output (display/monitor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    /// Stable index for this output (used in `capture_frame()`)
    pub id: usize,

    /// Human-readable name (e.g., "eDP-1", "HDMI-A-1")
    pub name: String,

    /// Output description (e.g., "Dell U2720Q", "Built-in Display")
    pub description: String,

    /// Current width in pixels
    pub width: u32,

    /// Current height in pixels
    pub height: u32,

    /// Refresh rate in millihertz (60000 = 60Hz, 60003 = 60.003Hz)
    pub refresh_rate_mhz: u32,

    /// Physical position in multi-monitor setup
    pub x: i32,
    pub y: i32,
}

/// Pixel format of captured frames.
///
/// Naming follows DRM fourcc conventions:
/// - First letter: padding/alpha channel position (X = unused, A = alpha)
/// - Remaining: RGB component order as seen in memory on little-endian
///
/// For example, XRGB8888:
/// - Memory bytes: [B, G, R, X] (little-endian word = 0xXXRRGGBB)
/// - FFmpeg equivalent: AV_PIX_FMT_BGR0
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// XRGB8888 - 32bpp, memory [B,G,R,X], DRM fourcc 0x34325258
    Xrgb8888,

    /// XBGR8888 - 32bpp, memory [R,G,B,X], DRM fourcc 0x34324258
    Xbgr8888,

    /// ARGB8888 - 32bpp with alpha, memory [B,G,R,A], DRM fourcc 0x34325241
    Argb8888,

    /// ABGR8888 - 32bpp with alpha, memory [R,G,B,A], DRM fourcc 0x34324742
    Abgr8888,
}

impl PixelFormat {
    /// Get DRM fourcc code for this format
    pub fn to_drm_fourcc(&self) -> u32 {
        match self {
            PixelFormat::Xrgb8888 => 0x34325258,
            PixelFormat::Xbgr8888 => 0x34324258,
            PixelFormat::Argb8888 => 0x34325241,
            PixelFormat::Abgr8888 => 0x34324742,
        }
    }

    /// Create from DRM fourcc code
    pub fn from_drm_fourcc(fourcc: u32) -> Option<Self> {
        match fourcc {
            0x34325258 => Some(PixelFormat::Xrgb8888),
            0x34324258 => Some(PixelFormat::Xbgr8888),
            0x34325241 => Some(PixelFormat::Argb8888),
            0x34324742 => Some(PixelFormat::Abgr8888),
            _ => None,
        }
    }

    /// Bytes per pixel (all formats are 32bpp)
    pub fn bytes_per_pixel(&self) -> u32 {
        4
    }
}
