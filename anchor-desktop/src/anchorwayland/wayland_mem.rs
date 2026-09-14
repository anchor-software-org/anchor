use drm::Device as DrmDevice;
use drm::control::Device as ControlDevice;
use gbm::{BufferObjectFlags, Device, Format};
use std::fs::File;
use std::fs::OpenOptions;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::io::{AsRawFd, RawFd};

use crate::anchorwayland::wayland_objects::{AnchorState, DrmBufParams};

pub struct Card(File);

impl AsRawFd for Card {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DrmDevice for Card {}

impl ControlDevice for Card {}

/// Map DRM fourcc codes to GBM formats.
/// Common formats compositors use for screencopy:
///   0x34324258 = XB24 = XBGR8888 (Intel Mesa default)
///   0x34325258 = XR24 = XRGB8888 (some compositors)
///   0x34324742 = AB24 = ABGR8888 (with alpha)
///   0x34325241 = AR24 = ARGB8888 (with alpha)
fn drm_fourcc_to_gbm_format(fourcc: u32) -> Result<Format, String> {
    match fourcc {
        0x34324258 => Ok(Format::Xbgr8888), // XB24
        0x34325258 => Ok(Format::Xrgb8888), // XR24
        0x34324742 => Ok(Format::Abgr8888), // AB24
        0x34325241 => Ok(Format::Argb8888), // AR24
        _ => {
            Err(format!("Unsupported DRM fourcc: 0x{:08x}. Add mapping in wayland_mem.rs", fourcc))
        }
    }
}

pub fn try_create_gpu_buf(state: &mut AnchorState) -> Result<DrmBufParams, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/dri/renderD128")
        .map_err(|e| format!("GPU render node /dev/dri/renderD128 not available ({})", e))?;

    let drm: Card = Card(file);
    let gbm = Device::new(drm).map_err(|e| format!("GBM device initialization failed: {}", e))?;

    // Use the format the compositor requested, not a hardcoded one.
    // Mismatch between wl_buffer format and actual GBM format causes color corruption!
    let format = drm_fourcc_to_gbm_format(state.dmabuf_params.buffer_format)?;

    log::info!(
        "[gbm] Creating buffer: {}x{}, format={:?} (DRM fourcc 0x{:08x})",
        state.dmabuf_params.buffer_width,
        state.dmabuf_params.buffer_height,
        format,
        state.dmabuf_params.buffer_format
    );

    let bo = gbm
        .create_buffer_object::<()>(
            state.dmabuf_params.buffer_width,
            state.dmabuf_params.buffer_height,
            format,
            BufferObjectFlags::LINEAR | BufferObjectFlags::RENDERING,
        )
        .map_err(|e| format!("GPU buffer allocation failed: {}", e))?;

    let fd = bo
        .fd()
        .map_err(|e| format!("GPU buffer has no DMA-BUF fd: {}", e))?
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("Failed to own DMA-BUF fd: {}", e))?;
    let stride = bo.stride();
    let offset = 0;
    let raw_modifier = bo.modifier();

    // We always request GBM_BO_USE_LINEAR, so the buffer is laid out linearly in
    // memory. Some Mesa/driver combinations return DRM_FORMAT_MOD_INVALID
    // (0x00ffffffffffffff) or another non-zero sentinel even for linear buffers,
    // which causes av_hwframe_map to reject the DMA-BUF import. Clamp to
    // DRM_FORMAT_MOD_LINEAR (0) so VAAPI accepts it.
    let raw_u64 = u64::from(raw_modifier);
    if raw_u64 != 0 {
        log::debug!(
            "[gbm] modifier 0x{:x} reported for LINEAR buffer — overriding to 0 for VAAPI import",
            raw_u64
        );
    }
    let modifier = drm::buffer::DrmModifier::from(0u64);

    Ok(DrmBufParams { fd, bo: std::sync::Arc::new(bo), stride, offset, modifier })
}
