//! Receives H.264 camera frames from the phone and writes them to a V4L2 loopback device.
//!
//! Pipeline:
//!   phone H.264 stream
//!       └── TCP duplex thread / UDP reassembler  (video_server.rs)
//!               └── mpsc channel  (Arc<Vec<u8>>)
//!                       └── camera_receiver_loop (this file)
//!                               ├── ffmpeg H.264 software decoder
//!                               └── V4L2 loopback writer → /dev/videoX (YUV420P planar)

use std::{
    os::unix::io::RawFd,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;

// ── V4L2 constants ────────────────────────────────────────────────────────────

/// Planar YUV 4:2:0 (Y, Cb, Cr contiguous).  Same format scrcpy & droidcam use.
/// FourCC 'YU12' — V4L2_PIX_FMT_YUV420 in the kernel headers.
const V4L2_PIX_FMT_YUV420: u32 = fourcc(b'Y', b'U', b'1', b'2');
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;

/// Linux `_IOWR(type_char, nr, T)` — computes an ioctl request number for
/// a read-write ioctl of a type whose size is `size` bytes.
const fn iowr(type_char: u8, nr: u8, size: usize) -> u64 {
    (3u64 << 30) | ((size as u64) << 16) | ((type_char as u64) << 8) | (nr as u64)
}

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// FFmpeg packet sizes are signed 32-bit values.
fn ffmpeg_packet_size(len: usize) -> Option<i32> {
    i32::try_from(len).ok()
}

/// Frame geometry is supplied by the decoder and is used in pointer arithmetic.
fn has_valid_frame_dimensions(width: i32, height: i32) -> bool {
    width > 0 && height > 0
}

// v4l2_pix_format (subset we need)
#[repr(C)]
#[derive(Clone, Copy)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32, // V4L2_FIELD_NONE = 1
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32, // V4L2_COLORSPACE_JPEG = 8 (sRGB)
    priv_: u32,
    flags: u32,
    enc_fmt: u32,
    quantization: u32,
    xfer_func: u32,
}

// v4l2_capability — used for VIDIOC_QUERYCAP to identify the driver name
#[repr(C)]
struct V4l2Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

// VIDIOC_QUERYCAP = _IOR('V', 0, struct v4l2_capability)
const fn ior(type_char: u8, nr: u8, size: usize) -> u64 {
    (2u64 << 30) | ((size as u64) << 16) | ((type_char as u64) << 8) | (nr as u64)
}
const VIDIOC_QUERYCAP: u64 = ior(b'V', 0, std::mem::size_of::<V4l2Capability>());

// v4l2_format — the union is at least 200 bytes; we only use the pix variant
#[repr(C)]
struct V4l2Format {
    type_: u32,
    _pad: [u8; 4],
    pix: V4l2PixFormat,
    _rest: [u8; 200 - std::mem::size_of::<V4l2PixFormat>()],
}

// VIDIOC_S_FMT = _IOWR('V', 5, struct v4l2_format)
// Computed from the struct size so this is correct on any Linux architecture.
const VIDIOC_S_FMT: u64 = iowr(b'V', 5, std::mem::size_of::<V4l2Format>());

// ── Public API ────────────────────────────────────────────────────────────────

/// Sender side of the camera frame channel.
pub type CameraFrameSender = SyncSender<Arc<Vec<u8>>>;

pub struct CameraCell {
    pub tx: Option<CameraFrameSender>,
    /// Which device is allowed to stream. `None` means nothing streams;
    /// the user must pick a device in the Settings UI.
    pub selected_device_id: Option<String>,
}

/// Thread-safe cell holding the frame sender and the active-device selection.
pub type CameraTxCell = Arc<Mutex<CameraCell>>;

pub fn new_camera_tx_cell() -> CameraTxCell {
    Arc::new(Mutex::new(CameraCell { tx: None, selected_device_id: None }))
}

/// Per-device handle that re-looks-up the current sender on demand. Lets sessions
/// pick up a late-bound `tx` (set by Settings "Create camera") without reconnecting.
#[derive(Clone)]
pub struct CameraSink {
    pub cell: CameraTxCell,
    pub device_id: String,
}

impl CameraSink {
    pub fn current(&self) -> Option<CameraFrameSender> {
        current_camera_tx(&self.cell, &self.device_id)
    }
}

/// Returns the frame sender. If `selected_device_id` is set, only the matching
/// device may stream. If `None`, any camera-capable device may stream.
pub fn current_camera_tx(cell: &CameraTxCell, device_id: &str) -> Option<CameraFrameSender> {
    let state = cell.lock().unwrap();
    match &state.selected_device_id {
        Some(selected) if selected == device_id => state.tx.clone(),
        None => state.tx.clone(),
        Some(_) => None,
    }
}

/// Spawns the camera receiver thread and returns a sender to push H.264 frames into it.
/// Returns `None` if the V4L2 device cannot be opened.
pub fn spawn_camera_receiver(device_path: &str) -> Option<CameraFrameSender> {
    let fd = open_v4l2_device(device_path)?;
    log::info!("[camera] spawn_camera_receiver: opened {} fd={}", device_path, fd);

    // Set a default format immediately so ffplay/other readers can open the device
    // without getting EBUSY before the first frame arrives.
    set_v4l2_format(fd, 1280, 720);

    // Warm the device — write a single black frame so readers (ffplay/OBS/Zoom)
    // can open without STREAMON/EAGAIN errors. Without this, the loopback device
    // has no buffers and the first reader gets EBUSY/Input/output error.
    warmup_device(fd, 1280, 720);
    log::info!("[camera] warm-up frame written to {}", device_path);

    let (tx, rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(4);
    let path = device_path.to_string();

    thread::spawn(move || {
        if let Err(e) = camera_receiver_loop(fd, rx) {
            log::error!("[camera] receiver loop error: {}", e);
        }
        unsafe { libc::close(fd) };
        log::info!("[camera] receiver thread exited ({})", path);
    });

    Some(tx)
}

/// Opens a video device, calls VIDIOC_QUERYCAP, and returns the driver name string.
pub(super) fn query_v4l2_driver(path: &str) -> Option<String> {
    let c_path = std::ffi::CString::new(path).ok()?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return None;
    }

    let mut cap = V4l2Capability {
        driver: [0; 16],
        card: [0; 32],
        bus_info: [0; 32],
        version: 0,
        capabilities: 0,
        device_caps: 0,
        reserved: [0; 3],
    };
    let ret = unsafe { libc::ioctl(fd, VIDIOC_QUERYCAP, std::ptr::addr_of_mut!(cap)) };
    unsafe { libc::close(fd) };

    if ret < 0 {
        return None;
    }

    let nul = cap.driver.iter().position(|&b| b == 0).unwrap_or(16);
    Some(String::from_utf8_lossy(&cap.driver[..nul]).into_owned())
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn open_v4l2_device(path: &str) -> Option<RawFd> {
    let c_path = match std::ffi::CString::new(path) {
        Ok(p) => p,
        Err(_) => return None,
    };
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        log::warn!("[camera] cannot open {}: {}", path, std::io::Error::last_os_error());
        None
    } else {
        log::info!("[camera] opened V4L2 device {} fd={}", path, fd);
        Some(fd)
    }
}

fn set_v4l2_format(fd: RawFd, width: u32, height: u32) -> bool {
    let pix = V4l2PixFormat {
        width,
        height,
        pixelformat: V4L2_PIX_FMT_YUV420,
        field: 1,
        bytesperline: width, // Y-plane stride; UV strides are implicitly width/2 for 4:2:0
        sizeimage: width * height * 3 / 2,
        colorspace: 8,
        priv_: 0,
        flags: 0,
        enc_fmt: 0,
        quantization: 0,
        xfer_func: 0,
    };

    // Zero-init the full v4l2_format, then fill in type + pix
    let mut fmt = V4l2Format {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        _pad: [0u8; 4],
        pix,
        _rest: [0u8; 200 - std::mem::size_of::<V4l2PixFormat>()],
    };

    let ret = unsafe { libc::ioctl(fd, VIDIOC_S_FMT, std::ptr::addr_of_mut!(fmt)) };
    if ret < 0 {
        log::error!("[camera] VIDIOC_S_FMT failed: {}", std::io::Error::last_os_error());
        false
    } else {
        log::info!("[camera] V4L2 format set: {}x{} YUV420 (YU12)", width, height);
        true
    }
}

/// Write a single black YUV420P frame to the loopback device. This primes the device
/// with a buffer so readers (ffplay/OBS/Zoom) can open without STREAMON/EAGAIN errors.
/// Y = 16 (limited-range black), Cb = Cr = 128 (neutral chroma).
fn warmup_device(fd: RawFd, width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_size = w * h / 4;
    let total = y_size + 2 * uv_size;

    let mut buf: Vec<u8> = Vec::with_capacity(total);
    // Y plane: limited-range black
    buf.resize(y_size, 16u8);
    // U plane: neutral chroma
    buf.resize(y_size + uv_size, 128u8);
    // V plane: neutral chroma
    buf.resize(total, 128u8);

    let ret = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, total) };
    if ret < 0 {
        log::warn!("[camera] warm-up write failed: {}", std::io::Error::last_os_error());
    } else if (ret as usize) != total {
        log::warn!("[camera] warm-up short write: {} of {} bytes", ret, total);
    } else {
        log::info!(
            "[camera] warm-up: wrote {}x{} black YUV420 frame ({} bytes)",
            width,
            height,
            total
        );
    }
}

fn camera_receiver_loop(fd: RawFd, rx: Receiver<Arc<Vec<u8>>>) -> Result<(), String> {
    // One-time ffmpeg init (idempotent)
    static FFMPEG_INIT: std::sync::Once = std::sync::Once::new();
    FFMPEG_INIT.call_once(|| {
        ffmpeg::init().expect("ffmpeg init failed");
    });

    // Open H.264 software decoder via raw ffi (same pattern as vaapi_encoder.rs)
    let codec = unsafe { ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_H264) };
    if codec.is_null() {
        return Err("H.264 decoder not found in ffmpeg".into());
    }

    let ctx = unsafe { ffi::avcodec_alloc_context3(codec) };
    if ctx.is_null() {
        return Err("avcodec_alloc_context3 failed".into());
    }

    let ret = unsafe { ffi::avcodec_open2(ctx, codec, std::ptr::null_mut()) };
    if ret < 0 {
        unsafe { ffi::avcodec_free_context(&mut (ctx as *mut _)) };
        return Err(format!("avcodec_open2 failed: {}", ret));
    }

    let pkt = unsafe { ffi::av_packet_alloc() };
    let frame = unsafe { ffi::av_frame_alloc() };
    // scale_frame is reused for sws_scale destination when scaling/format-conversion
    // is needed. It holds a YUV420P temp frame at inner_w × inner_h.
    let scale_frame = unsafe { ffi::av_frame_alloc() };

    if pkt.is_null() || frame.is_null() || scale_frame.is_null() {
        unsafe {
            if !pkt.is_null() {
                ffi::av_packet_free(&mut (pkt as *mut _));
            }
            if !frame.is_null() {
                ffi::av_frame_free(&mut (frame as *mut _));
            }
            if !scale_frame.is_null() {
                ffi::av_frame_free(&mut (scale_frame as *mut _));
            }
            ffi::avcodec_free_context(&mut (ctx as *mut _));
        }
        return Err("av_alloc failed".into());
    }

    let mut sws_ctx: *mut ffi::SwsContext = std::ptr::null_mut();
    let mut sws_signature: Option<(i32, i32, i32)> = None; // log dedup: (src_w, src_h, src_fmt)
    let mut sws_ctx_sig: Option<(i32, i32, i32)> = None; // tracks geometry/fmt used to create sws_ctx

    // Output (V4L2) dimensions are FIXED — 1280x720. We scale any incoming
    // resolution to this. V4L2 format is only set once at startup, so readers
    // (ffplay/OBS/Zoom) get a stable device even if the phone rotates or switches
    // cameras mid-stream.
    let out_w: i32 = 1280;
    let out_h: i32 = 720;
    let out_w_u = out_w as usize;
    let out_h_u = out_h as usize;

    // YUV420P canvas: [Y plane: out_w*out_h] [U plane: out_w*out_h/4] [V plane: out_w*out_h/4]
    let canvas_total = out_w_u * out_h_u * 3 / 2;
    let y_plane_size = out_w_u * out_h_u;
    let uv_plane_size = out_w_u * out_h_u / 4; // each chroma plane
    let u_plane_off = y_plane_size;
    let v_plane_off = y_plane_size + uv_plane_size;
    // Canvas strides (no padding — contiguous planes)
    let y_canvas_stride = out_w_u;
    let uv_canvas_stride = out_w_u / 2;

    let mut canvas: Vec<u8> = vec![0u8; canvas_total];

    let mut frames_written: u64 = 0;
    let mut frames_decoded: u64 = 0;
    let mut packets_received: u64 = 0;
    let mut packets_since_last_frame: u64 = 0;
    let mut write_errors: u64 = 0;
    let mut last_stats_log = std::time::Instant::now();

    log::info!("[camera] decoder loop starting (output fixed at {}x{} YUV420)", out_w, out_h);

    // ── helpers ────────────────────────────────────────────────────────────

    /// Fill only the letterbox margins of one contiguous plane. The pasted
    /// content rect [off_x, off_y, inner_w × inner_h] is left untouched, so a
    /// full-canvas content rect costs nothing and a pillarboxed portrait
    /// stream fills just the side columns rather than the whole canvas.
    fn fill_plane_margins(
        plane: &mut [u8],
        stride: usize,
        plane_h: usize,
        off_x: usize,
        off_y: usize,
        inner_w: usize,
        inner_h: usize,
        val: u8,
    ) {
        plane[..off_y * stride].fill(val);
        plane[(off_y + inner_h) * stride..plane_h * stride].fill(val);
        // `off_x` is rounded down to an even column, so the right margin can
        // be nonzero even when `off_x == 0`; fill both margins per row.
        for row in off_y..off_y + inner_h {
            let start = row * stride;
            plane[start..start + off_x].fill(val);
            plane[start + off_x + inner_w..start + stride].fill(val);
        }
    }

    /// Copy one plane from an AVFrame (which may have row padding in
    /// `linesize`) into the contiguous canvas at a column/row offset, writing
    /// each row REVERSED — the horizontal mirror is fused into the paste so
    /// the decoded frame is never mutated and no `av_frame_make_writable`
    /// copy is needed.
    ///
    /// # Safety
    /// `src_data` must point to `copy_h` rows of `src_stride` bytes, where each
    /// row's first `copy_w` bytes are valid. The canvas must be large enough
    /// for the target region.
    #[allow(clippy::too_many_arguments)]
    unsafe fn copy_plane_mirrored(
        canvas: &mut [u8],
        canvas_base: usize,
        canvas_stride: usize,
        dst_col: usize,
        dst_row: usize,
        src_data: *const u8,
        src_stride: i32,
        copy_w: usize,
        copy_h: usize,
    ) -> bool {
        let Ok(src_stride) = usize::try_from(src_stride) else {
            return false;
        };
        if src_data.is_null()
            || copy_w > src_stride
            || copy_w > canvas_stride.saturating_sub(dst_col)
        {
            return false;
        }

        if copy_h == 0 {
            return true;
        }
        let Some(last_row) = dst_row.checked_add(copy_h - 1) else {
            return false;
        };
        let Some(end_offset) = last_row
            .checked_mul(canvas_stride)
            .and_then(|offset| offset.checked_add(dst_col))
            .and_then(|offset| offset.checked_add(copy_w))
            .and_then(|offset| canvas_base.checked_add(offset))
        else {
            return false;
        };
        if end_offset > canvas.len() || copy_h.saturating_sub(1).checked_mul(src_stride).is_none() {
            return false;
        }

        for row in 0..copy_h {
            unsafe {
                let src = std::slice::from_raw_parts(src_data.add(row * src_stride), copy_w);
                let dst = std::slice::from_raw_parts_mut(
                    canvas
                        .as_mut_ptr()
                        .add(canvas_base + (dst_row + row) * canvas_stride + dst_col),
                    copy_w,
                );
                for (d, s) in dst.iter_mut().zip(src.iter().rev()) {
                    *d = *s;
                }
            }
        }
        true
    }

    /// Clamp `cols` columns of a canvas plane to a neutral chroma value for
    /// `rows` rows starting at (col, row). The Android NV12 helper corrupts
    /// the source's rightmost chroma column; after the mirrored paste that
    /// lands on the left edge of the output.
    fn clamp_canvas_columns(
        canvas: &mut [u8],
        canvas_base: usize,
        canvas_stride: usize,
        col: usize,
        row: usize,
        cols: usize,
        rows: usize,
        val: u8,
    ) {
        for r in row..row + rows {
            let start = canvas_base + r * canvas_stride + col;
            if start + cols <= canvas.len() {
                canvas[start..start + cols].fill(val);
            }
        }
    }

    for frame_data in rx.iter() {
        // Camera frames are replaceable state: if decoding fell behind,
        // discard queued older frames so the preview shows the newest
        // capture instead of playing back a backlog.
        let mut frame_data = frame_data;
        while let Ok(newer) = rx.try_recv() {
            frame_data = newer;
        }
        packets_received += 1;
        packets_since_last_frame += 1;
        if packets_received <= 5 || packets_since_last_frame > 120 {
            log::debug!(
                "[camera] H.264 packet #{} ({} bytes, {} pkts since last frame)",
                packets_received,
                frame_data.len(),
                packets_since_last_frame
            );
            packets_since_last_frame = 0;
        } else if packets_received.is_multiple_of(60) {
            log::debug!("[camera] H.264 packet #{} ({} bytes)", packets_received, frame_data.len());
        }

        unsafe {
            let Some(packet_size) = ffmpeg_packet_size(frame_data.len()) else {
                log::warn!(
                    "[camera] dropping packet {}: {} bytes exceeds FFmpeg's maximum packet size",
                    packets_received,
                    frame_data.len()
                );
                continue;
            };
            (*pkt).data = frame_data.as_ptr().cast_mut();
            (*pkt).size = packet_size;

            let ret = ffi::avcodec_send_packet(ctx, pkt);
            if ret < 0 {
                log::warn!(
                    "[camera] avcodec_send_packet error {} (packet {} of {} bytes)",
                    ret,
                    packets_received,
                    frame_data.len()
                );
                continue;
            }

            loop {
                let ret = ffi::avcodec_receive_frame(ctx, frame);
                if ret == ffi::AVERROR(libc::EAGAIN) || ret == ffi::AVERROR_EOF {
                    break;
                }
                if ret < 0 {
                    log::warn!("[camera] avcodec_receive_frame: {}", ret);
                    break;
                }

                // Apply H.264 crop FIRST so width/height reflect display, not coded
                // dimensions. Coded dims are 16-aligned, with garbage right/bottom
                // columns; without cropping, those garbage columns survive into the
                // mirror+scale and produce a green column at the left after mirror.
                let coded_w = (*frame).width;
                let coded_h = (*frame).height;
                let crop_l = (*frame).crop_left as i32;
                let crop_r = (*frame).crop_right as i32;
                let crop_t = (*frame).crop_top as i32;
                let crop_b = (*frame).crop_bottom as i32;
                let crop_ret = ffi::av_frame_apply_cropping(frame, 0);
                if frames_decoded < 3 {
                    log::debug!(
                        "[camera] frame: coded={}x{} crop=L{}/R{}/T{}/B{} → after_apply={}x{} ret={}",
                        coded_w,
                        coded_h,
                        crop_l,
                        crop_r,
                        crop_t,
                        crop_b,
                        (*frame).width,
                        (*frame).height,
                        crop_ret
                    );
                }

                let w = (*frame).width;
                let h = (*frame).height;
                if crop_ret < 0 || !has_valid_frame_dimensions(w, h) {
                    log::warn!(
                        "[camera] dropping invalid decoded frame: crop_ret={} dimensions={}x{}",
                        crop_ret,
                        w,
                        h
                    );
                    ffi::av_frame_unref(frame);
                    continue;
                }
                let raw_src_fmt = (*frame).format; // typically AV_PIX_FMT_YUV420P, NV12, or YUVJ420P

                // The decoded frame stays read-only: the horizontal mirror is
                // fused into the canvas paste below, so the refcounted decoder
                // buffers are never mutated and no av_frame_make_writable copy
                // (~1.4MiB at 720p) is needed per frame.
                // ── format / letterbox ─────────────────────────────────────

                // Treat YUVJ420P (deprecated, full-range JPEG YUV) as YUV420P + src_range=1.
                // Without this remap, swscaler logs a "deprecated pixel format" warning per frame.
                let yuvj420p = ffi::AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32;
                let yuv420p = ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32;
                let yuvj422p = ffi::AVPixelFormat::AV_PIX_FMT_YUVJ422P as i32;
                let yuv422p = ffi::AVPixelFormat::AV_PIX_FMT_YUV422P as i32;
                let yuvj444p = ffi::AVPixelFormat::AV_PIX_FMT_YUVJ444P as i32;
                let yuv444p = ffi::AVPixelFormat::AV_PIX_FMT_YUV444P as i32;
                let (src_fmt, src_full_range) = if raw_src_fmt == yuvj420p {
                    (yuv420p, 1i32)
                } else if raw_src_fmt == yuvj422p {
                    (yuv422p, 1i32)
                } else if raw_src_fmt == yuvj444p {
                    (yuv444p, 1i32)
                } else {
                    (raw_src_fmt, 0i32)
                };

                // V4L2 output is fixed at 1280x720. We aspect-fit the incoming
                // content into that canvas (letterbox) so portrait frames from the
                // phone don't get stretched horizontally.
                let in_aspect = w as f64 / h as f64;
                let out_aspect = out_w as f64 / out_h as f64;
                let (inner_w, inner_h) = if in_aspect > out_aspect {
                    // Content is wider than canvas — fit width, letterbox top/bottom
                    let h2 = (out_w as f64 / in_aspect).round() as i32;
                    (out_w, (h2 & !1).max(2)) // ensure even for 4:2:0 chroma
                } else {
                    // Content is taller than canvas — fit height, pillarbox left/right
                    let w2 = (out_h as f64 * in_aspect).round() as i32;
                    ((w2 & !1).max(2), out_h) // ensure even for 4:2:0 chroma
                };
                let off_x = ((out_w - inner_w) / 2) & !1; // keep even for chroma alignment
                let off_y = (out_h - inner_h) / 2;

                let new_sig = (w, h, src_fmt);
                if sws_signature != Some(new_sig) {
                    log::debug!(
                        "[camera] input {}x{} fmt={} → letterbox {}x{} at ({}, {}) inside {}x{}",
                        w,
                        h,
                        raw_src_fmt,
                        inner_w,
                        inner_h,
                        off_x,
                        off_y,
                        out_w,
                        out_h
                    );
                    sws_signature = Some(new_sig);
                }

                // ── fill letterbox margins ─────────────────────────────────
                // Only the margins need neutral black — the paste region is
                // fully overwritten below, so a full-canvas frame skips the
                // ~1.4MiB memset entirely.
                fill_plane_margins(
                    &mut canvas[..y_plane_size],
                    y_canvas_stride,
                    out_h_u,
                    off_x as usize,
                    off_y as usize,
                    inner_w as usize,
                    inner_h as usize,
                    16,
                );
                fill_plane_margins(
                    &mut canvas[u_plane_off..v_plane_off],
                    uv_canvas_stride,
                    out_h_u / 2,
                    off_x as usize / 2,
                    off_y as usize / 2,
                    inner_w as usize / 2,
                    inner_h as usize / 2,
                    128,
                );
                fill_plane_margins(
                    &mut canvas[v_plane_off..],
                    uv_canvas_stride,
                    out_h_u / 2,
                    off_x as usize / 2,
                    off_y as usize / 2,
                    inner_w as usize / 2,
                    inner_h as usize / 2,
                    128,
                );

                // ── scale + paste ──────────────────────────────────────────

                // Identity path: source is already YUV420P and fits the letterbox
                // rect exactly → skip sws_scale entirely, just copy planes.
                let is_identity = w == inner_w && h == inner_h && src_fmt == yuv420p;

                if is_identity {
                    // Mirror-fused copy of Y, U, V planes from the read-only
                    // decoded frame into the canvas at (off_x, off_y).
                    let copied = copy_plane_mirrored(
                        &mut canvas,
                        0, // Y plane starts at offset 0
                        y_canvas_stride,
                        off_x as usize,
                        off_y as usize,
                        (*frame).data[0],
                        (*frame).linesize[0],
                        w as usize,
                        h as usize,
                    )
                    // Copy U plane at (off_x/2, off_y/2) — chroma is subsampled 2×.
                    && copy_plane_mirrored(
                        &mut canvas,
                        u_plane_off,
                        uv_canvas_stride,
                        (off_x / 2) as usize,
                        (off_y / 2) as usize,
                        (*frame).data[1],
                        (*frame).linesize[1],
                        (w / 2) as usize,
                        (h / 2) as usize,
                    )
                    // Copy V plane.
                    && copy_plane_mirrored(
                        &mut canvas,
                        v_plane_off,
                        uv_canvas_stride,
                        (off_x / 2) as usize,
                        (off_y / 2) as usize,
                        (*frame).data[2],
                        (*frame).linesize[2],
                        (w / 2) as usize,
                        (h / 2) as usize,
                    );
                    if !copied {
                        log::warn!("[camera] dropping frame with an invalid YUV420P plane layout");
                        ffi::av_frame_unref(frame);
                        continue;
                    }
                } else {
                    // Scaling or format-conversion path: sws_scale into a temp
                    // YUV420P frame at inner_w × inner_h, then per-plane memcpy
                    // into the canvas at the letterbox offset.
                    //
                    // Dst format is YUV420P (planar), NOT YUYV422 (packed).
                    // This avoids libswscale's SIMD chroma-packing path which has
                    // a known artifact class at the leftmost output column
                    // (Cb=Cr=0 → green in BT.601 limited range).
                    let dst_fmt = ffi::AVPixelFormat::AV_PIX_FMT_YUV420P;

                    if sws_ctx_sig != Some(new_sig) {
                        sws_ctx_sig = Some(new_sig);
                        // SWS_FAST_BILINEAR (1) — matches droidcam's approach.
                        // No SWS_ACCURATE_RND needed; we're doing planar→planar.
                        const SWS_FLAGS: i32 = 1;
                        sws_ctx = ffi::sws_getCachedContext(
                            sws_ctx,
                            w,
                            h,
                            std::mem::transmute::<i32, ffi::AVPixelFormat>(src_fmt),
                            inner_w,
                            inner_h,
                            dst_fmt,
                            SWS_FLAGS,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null(),
                        );
                        if sws_ctx.is_null() {
                            log::warn!("[camera] sws_getCachedContext failed");
                            ffi::av_frame_unref(frame);
                            continue;
                        }

                        // BT.601 coefficients; src range follows the decoded pixel format.
                        let coeffs = ffi::sws_getCoefficients(ffi::SWS_CS_ITU601);
                        ffi::sws_setColorspaceDetails(
                            sws_ctx,
                            coeffs,
                            src_full_range,
                            coeffs,
                            0,
                            0,
                            1 << 16,
                            1 << 16,
                        );
                    }

                    ffi::av_frame_unref(scale_frame);
                    (*scale_frame).format = dst_fmt as i32;
                    (*scale_frame).width = inner_w;
                    (*scale_frame).height = inner_h;
                    let ret = ffi::av_frame_get_buffer(scale_frame, 0);
                    if ret < 0 {
                        log::warn!("[camera] av_frame_get_buffer (scale): {}", ret);
                        ffi::av_frame_unref(frame);
                        continue;
                    }

                    ffi::sws_scale(
                        sws_ctx,
                        (*frame).data.as_ptr() as *const *const u8,
                        (*frame).linesize.as_ptr(),
                        0,
                        h,
                        (*scale_frame).data.as_ptr() as *mut *mut u8,
                        (*scale_frame).linesize.as_ptr(),
                    );

                    // Paste scale_frame planes into canvas at the letterbox
                    // offset, mirroring each row — the scale output is ours,
                    // but fusing the mirror here keeps the paste a single
                    // pass either way.
                    let s_w = inner_w as usize;
                    let s_h = inner_h as usize;
                    let copied = copy_plane_mirrored(
                        &mut canvas,
                        0,
                        y_canvas_stride,
                        off_x as usize,
                        off_y as usize,
                        (*scale_frame).data[0],
                        (*scale_frame).linesize[0],
                        s_w,
                        s_h,
                    ) && copy_plane_mirrored(
                        &mut canvas,
                        u_plane_off,
                        uv_canvas_stride,
                        (off_x / 2) as usize,
                        (off_y / 2) as usize,
                        (*scale_frame).data[1],
                        (*scale_frame).linesize[1],
                        s_w / 2,
                        s_h / 2,
                    ) && copy_plane_mirrored(
                        &mut canvas,
                        v_plane_off,
                        uv_canvas_stride,
                        (off_x / 2) as usize,
                        (off_y / 2) as usize,
                        (*scale_frame).data[2],
                        (*scale_frame).linesize[2],
                        s_w / 2,
                        s_h / 2,
                    );

                    ffi::av_frame_unref(scale_frame);
                    if !copied {
                        log::warn!(
                            "[camera] dropping frame with an invalid scaled YUV420P plane layout"
                        );
                        ffi::av_frame_unref(frame);
                        continue;
                    }
                }

                // The Android NV12 helper corrupts the rightmost chroma
                // column of the source; after the mirrored paste that column
                // lands at the left edge of the pasted region. Scaling can
                // smear it across a couple of output columns, so clamp two.
                for plane_base in [u_plane_off, v_plane_off] {
                    clamp_canvas_columns(
                        &mut canvas,
                        plane_base,
                        uv_canvas_stride,
                        (off_x / 2) as usize,
                        (off_y / 2) as usize,
                        2,
                        (inner_h / 2) as usize,
                        128,
                    );
                }

                // ── write to V4L2 ──────────────────────────────────────────

                // v4l2loopback expects ONE write() per frame with all three
                // planes contiguous (droidcam-style). YUV420P is 1.5 bytes/pixel.
                let ret = libc::write(fd, canvas.as_ptr().cast(), canvas_total);
                if ret < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::EAGAIN) {
                        log::warn!("[camera] V4L2 write error: {}", err);
                    } else if frames_written < 5 {
                        log::debug!(
                            "[camera] V4L2 write EAGAIN (no reader attached) frame={}",
                            frames_written
                        );
                    }
                } else if usize::try_from(ret) != Ok(canvas_total) {
                    log::warn!("[camera] V4L2 short write: {} of {} bytes", ret, canvas_total);
                    write_errors += 1;
                } else if frames_written < 5 {
                    log::debug!("[camera] V4L2 write ok: {} bytes (frame {})", ret, frames_written);
                }

                ffi::av_frame_unref(frame);

                frames_written += 1;
                frames_decoded += 1;
                packets_since_last_frame = 0;
                if frames_decoded <= 5 {
                    log::debug!(
                        "[camera] decoded frame {} → written (in={}x{}, out={}x{})",
                        frames_decoded,
                        w,
                        h,
                        out_w,
                        out_h
                    );
                } else if frames_decoded.is_multiple_of(60) {
                    log::debug!(
                        "[camera] decoded {} frames, written {} (in={}x{}, out={}x{})",
                        frames_decoded,
                        frames_written,
                        w,
                        h,
                        out_w,
                        out_h
                    );
                }

                let now = std::time::Instant::now();
                if now.duration_since(last_stats_log).as_secs() >= 10 {
                    log::debug!(
                        "[camera] stats: decoded={} written={} packets={} write_errs={} pending_pkts={}",
                        frames_decoded,
                        frames_written,
                        packets_received,
                        write_errors,
                        packets_since_last_frame
                    );
                    crate::metrics::emit(serde_json::json!({
                        "t": "camera",
                        "decoded": frames_decoded,
                        "written": frames_written,
                        "packets": packets_received,
                        "write_errs": write_errors,
                        "pending_pkts": packets_since_last_frame,
                    }));
                    last_stats_log = now;
                }
            }
        }
    }

    // Cleanup
    unsafe {
        if !sws_ctx.is_null() {
            ffi::sws_freeContext(sws_ctx);
        }
        ffi::av_frame_free(&mut (frame as *mut _));
        ffi::av_frame_free(&mut (scale_frame as *mut _));
        ffi::av_packet_free(&mut (pkt as *mut _));
        ffi::avcodec_free_context(&mut (ctx as *mut _));
    }

    log::info!(
        "[camera] loop exited: written={} decoded={} packets={} write_errors={}",
        frames_written,
        frames_decoded,
        packets_received,
        write_errors
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn ffmpeg_packet_size_rejects_unrepresentable_lengths() {
        assert_eq!(ffmpeg_packet_size(0), Some(0));
        assert_eq!(ffmpeg_packet_size(i32::MAX as usize), Some(i32::MAX));
        assert_eq!(ffmpeg_packet_size(i32::MAX as usize + 1), None);
    }

    #[test]
    fn decoded_frame_dimensions_must_be_positive() {
        assert!(has_valid_frame_dimensions(1, 1));
        assert!(!has_valid_frame_dimensions(0, 720));
        assert!(!has_valid_frame_dimensions(1280, 0));
        assert!(!has_valid_frame_dimensions(-1, 720));
    }

    #[test]
    fn new_cell_has_no_tx_and_no_selection() {
        let cell = new_camera_tx_cell();
        let state = cell.lock().unwrap();
        assert!(state.tx.is_none());
        assert!(state.selected_device_id.is_none());
    }

    #[test]
    fn current_tx_ungated_when_no_selection() {
        let cell = new_camera_tx_cell();
        let (tx, _rx) = mpsc::sync_channel(1);
        cell.lock().unwrap().tx = Some(tx.clone());

        assert!(current_camera_tx(&cell, "device-a").is_some());
        assert!(current_camera_tx(&cell, "device-b").is_some());
        assert!(current_camera_tx(&cell, "unknown-phone").is_some());
    }

    #[test]
    fn current_tx_returns_none_when_tx_is_none() {
        let cell = new_camera_tx_cell();
        assert!(current_camera_tx(&cell, "device-a").is_none());

        cell.lock().unwrap().selected_device_id = Some("device-a".into());
        assert!(current_camera_tx(&cell, "device-a").is_none());
    }

    #[test]
    fn current_tx_gated_when_selection_matches() {
        let cell = new_camera_tx_cell();
        let (tx, _rx) = mpsc::sync_channel(1);
        {
            let mut state = cell.lock().unwrap();
            state.tx = Some(tx.clone());
            state.selected_device_id = Some("chosen-device".into());
        }

        assert!(current_camera_tx(&cell, "chosen-device").is_some());
        assert!(current_camera_tx(&cell, "other-device").is_none());
    }

    #[test]
    fn current_tx_blocks_when_selection_differs() {
        let cell = new_camera_tx_cell();
        let (tx, _rx) = mpsc::sync_channel(1);
        {
            let mut state = cell.lock().unwrap();
            state.tx = Some(tx);
            state.selected_device_id = Some("phone-a".into());
        }

        assert!(current_camera_tx(&cell, "phone-a").is_some());
        assert!(current_camera_tx(&cell, "phone-b").is_none());
    }

    #[test]
    fn current_tx_clones_independent_arcs() {
        let cell = new_camera_tx_cell();
        let (tx, rx) = mpsc::sync_channel(4);
        cell.lock().unwrap().tx = Some(tx);

        let tx1 = current_camera_tx(&cell, "dev").unwrap();
        let tx2 = current_camera_tx(&cell, "dev").unwrap();

        // Both clones share the same underlying sender
        tx1.try_send(Arc::new(vec![1])).unwrap();
        assert_eq!(*rx.recv().unwrap(), vec![1]);

        tx2.try_send(Arc::new(vec![2])).unwrap();
        assert_eq!(*rx.recv().unwrap(), vec![2]);
    }

    #[test]
    fn current_tx_selection_change_takes_effect_immediately() {
        let cell = new_camera_tx_cell();
        let (tx, _rx) = mpsc::sync_channel(1);
        cell.lock().unwrap().tx = Some(tx);

        assert!(current_camera_tx(&cell, "dev-a").is_some());

        cell.lock().unwrap().selected_device_id = Some("dev-a".into());
        assert!(current_camera_tx(&cell, "dev-a").is_some());
        assert!(current_camera_tx(&cell, "dev-b").is_none());

        cell.lock().unwrap().selected_device_id = None;
        assert!(current_camera_tx(&cell, "dev-b").is_some());
    }
}
