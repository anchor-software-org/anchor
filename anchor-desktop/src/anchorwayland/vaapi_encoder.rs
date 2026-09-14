//! Hardware-accelerated or software video encoder using ffmpeg-next.
//!
//! Two encoding paths, tried in order:
//!
//! 1. Zero-copy DMA-BUF path (encode_dmabuf):
//!    DMA-BUF fd → av_hwframe_map (DRM PRIME → VAAPI, no CPU touch)
//!    → scale_vaapi filter (XBGR → NV12 on GPU)
//!    → h264_vaapi encode
//!
//! 2. CPU-upload fallback path (encode_xbgr):
//!    bo.map() → row copy → swscale (XBGR→NV12, CPU) → av_hwframe_transfer_data → h264_vaapi
//!    or: bo.map() → row copy → swscale (XBGR→YUV420P, CPU) → software encoder

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;
use std::ffi::CString;
use std::ptr;
use std::sync::Once;
use std::time::Instant;

/// Detailed timing breakdown for zero-copy encode profiling
#[derive(Debug, Clone)]
pub struct DmabufEncodeTiming {
    /// PTS submitted with this input. Together with `output_pts`, this shows
    /// whether a quick encoder API call actually produced the current frame.
    pub input_pts: i64,
    /// PTS of the last packet drained by this call, if one was available.
    pub output_pts: Option<i64>,
    /// Number of packets drained by this call.
    pub output_packet_count: u32,
    pub dup_fd_us: u128,
    pub create_descriptor_us: u128,
    pub hwframe_map_us: u128,     // DMA-BUF import to VAAPI
    pub buffersrc_add_us: u128,   // Push to filter graph
    pub buffersink_get_us: u128,  // scale_vaapi GPU conversion
    pub send_frame_us: u128,      // Submit to encoder
    pub receive_packets_us: u128, // Drain encoder
    pub total_us: u128,
}

static FFMPEG_INIT: Once = Once::new();

fn ensure_ffmpeg_init() {
    FFMPEG_INIT.call_once(|| {
        ffmpeg::init().expect("Failed to initialize ffmpeg");
    });
}

fn av_err_string(errnum: i32) -> String {
    unsafe {
        let mut buf = [0u8; 256];
        ffi::av_strerror(errnum, buf.as_mut_ptr() as *mut std::os::raw::c_char, buf.len());
        std::ffi::CStr::from_ptr(buf.as_ptr() as *const std::os::raw::c_char)
            .to_string_lossy()
            .into_owned()
    }
}

// Common DRM fourcc codes for screencopy buffers.
// The fourcc encodes 4 ASCII chars in little-endian, e.g.:
//   fourcc_code('X', 'B', '2', '4') = 0x34324258
// DRM uses packed-pixel notation where '2'/'4' indicate 8-bit channels.
const DRM_FORMAT_XBGR8888: u32 = 0x34324258; // XB24 — [31:0] X:B:G:R 8:8:8:8 LE → memory [R,G,B,X]
const DRM_FORMAT_XRGB8888: u32 = 0x34325258; // XR24 — [31:0] X:R:G:B 8:8:8:8 LE → memory [B,G,R,X]
const DRM_FORMAT_ABGR8888: u32 = 0x34324742; // AB24 — [31:0] A:B:G:R 8:8:8:8 LE → memory [R,G,B,A]
const DRM_FORMAT_ARGB8888: u32 = 0x34325241; // AR24 — [31:0] A:R:G:B 8:8:8:8 LE → memory [B,G,R,A]

/// Map DRM fourcc to FFmpeg sw_format for DMA-BUF import.
/// Matches FFmpeg's hwcontext_drm.c drm_format_map exactly.
/// DRM names channels MSB→LSB; on little-endian the bytes in memory are reversed.
fn drm_fourcc_to_av_pixfmt(fourcc: u32) -> Result<ffi::AVPixelFormat, String> {
    match fourcc {
        DRM_FORMAT_XBGR8888 => Ok(ffi::AVPixelFormat::AV_PIX_FMT_RGB0), // memory [R,G,B,X]
        DRM_FORMAT_XRGB8888 => Ok(ffi::AVPixelFormat::AV_PIX_FMT_BGR0), // memory [B,G,R,X]
        DRM_FORMAT_ABGR8888 => Ok(ffi::AVPixelFormat::AV_PIX_FMT_RGBA), // memory [R,G,B,A]
        DRM_FORMAT_ARGB8888 => Ok(ffi::AVPixelFormat::AV_PIX_FMT_BGRA), // memory [B,G,R,A]
        _ => Err(format!("Unsupported DRM fourcc for VAAPI import: 0x{:08x}", fourcc)),
    }
}

/// AVBufferRef free callback that drops a heap-allocated AVDRMFrameDescriptor
/// and closes the duplicated DMA-BUF file descriptor.
///
/// FFmpeg calls this when the last reference to the buffer is released.
/// `data` is the pointer originally passed to av_buffer_create(), which here
/// is the raw pointer produced by Box::into_raw::<AVDRMFrameDescriptor>().
unsafe extern "C" fn drm_desc_free(_opaque: *mut std::ffi::c_void, data: *mut u8) {
    unsafe {
        let desc_ptr = data as *mut ffi::AVDRMFrameDescriptor;
        let desc = &*desc_ptr;

        // Close the duplicated DMA-BUF fd to release the kernel reference.
        // This must be done before dropping the descriptor.
        if desc.nb_objects > 0 && desc.objects[0].fd >= 0 {
            libc::close(desc.objects[0].fd);
        }

        drop(Box::from_raw(desc_ptr));
    }
}

/// Owns a single `AV_HWDEVICE_TYPE_VAAPI` device context — and therefore a
/// single `/dev/dri/renderD128` file descriptor — for the lifetime of the
/// capture session.
///
/// Why this exists: `av_hwdevice_ctx_create` opens the DRM render node
/// on every call. Under IDR churn we recreate the encoder often (dropped
/// frames, resolution changes, output switches) — if each encoder created
/// its own device context, `av_buffer_unref` on encoder teardown would
/// *sometimes* fail to close the underlying DRM fd (VAAPI internals hold
/// implicit refs via the encoder's filter graph). Empirically ~0.67 fds
/// leaked per recreate; over hours of runtime the process hits
/// `RLIMIT_NOFILE` and every subsequent open (network sockets, database
/// wal files, etc.) fails.
///
/// Sharing one device context across encoder recreations fixes it by
/// construction: only one `av_hwdevice_ctx_create` ever runs, so only one
/// fd is ever open. Whatever internal ref shenanigans FFmpeg does when we
/// unref `hw_frames_ref` on encoder drop can leak refcounts against this
/// single AVBufferRef, but it can't spawn a new fd.
pub struct VaapiDeviceCtx {
    hw_device_ref: *mut ffi::AVBufferRef,
}

// The context is only ever touched from the Wayland capture thread — the
// raw pointer is not shared across threads.
unsafe impl Send for VaapiDeviceCtx {}

/// Return render nodes in stable order, or an explicit configured node.
fn render_node_candidates(preference: &str, mut discovered: Vec<String>) -> Vec<String> {
    if !preference.eq_ignore_ascii_case("auto") {
        return vec![preference.to_string()];
    }
    discovered.sort();
    discovered
        .into_iter()
        .filter(|path| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect()
}

/// Every `/dev/dri/renderD*` node, in stable order.
fn render_nodes() -> Vec<String> {
    let discovered = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    render_node_candidates("auto", discovered)
}

/// Whether `h264_vaapi` can actually be opened on this device.
///
/// Opening a VAAPI device and initializing a frame context can succeed on a
/// decode/VPP-only GPU. The real encode entrypoint must therefore be probed.
unsafe fn can_encode_h264(device_ref: *mut ffi::AVBufferRef) -> bool {
    unsafe {
        let Ok(name) = CString::new("h264_vaapi") else {
            return false;
        };
        let codec = ffi::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return false;
        }

        let mut frames_ref = ffi::av_hwframe_ctx_alloc(device_ref);
        if frames_ref.is_null() {
            return false;
        }
        let hwf = (*frames_ref).data as *mut ffi::AVHWFramesContext;
        (*hwf).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*hwf).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
        (*hwf).width = 1280;
        (*hwf).height = 720;
        (*hwf).initial_pool_size = 1;
        if ffi::av_hwframe_ctx_init(frames_ref) < 0 {
            ffi::av_buffer_unref(&mut frames_ref);
            return false;
        }

        let ctx = ffi::avcodec_alloc_context3(codec);
        if ctx.is_null() {
            ffi::av_buffer_unref(&mut frames_ref);
            return false;
        }
        (*ctx).width = 1280;
        (*ctx).height = 720;
        (*ctx).pix_fmt = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*ctx).time_base = ffi::AVRational { num: 1, den: 30 };
        (*ctx).hw_frames_ctx = ffi::av_buffer_ref(frames_ref);
        let opened = ffi::avcodec_open2(ctx, codec, ptr::null_mut()) >= 0;

        let mut ctx_free = ctx;
        ffi::avcodec_free_context(&mut ctx_free);
        ffi::av_buffer_unref(&mut frames_ref);
        opened
    }
}

impl VaapiDeviceCtx {
    /// Best-effort init. Returns `None` if no usable VAAPI device exists —
    /// callers fall back to software encoding.
    pub fn try_new(preference: &str) -> Option<Self> {
        ensure_ffmpeg_init();

        let candidates = if preference.eq_ignore_ascii_case("auto") {
            render_nodes()
        } else {
            vec![preference.to_string()]
        };
        if candidates.is_empty() {
            log::warn!("No DRM render nodes found — falling back to software encoding");
            return None;
        }

        let mut vpp_only: Option<Self> = None;
        for path in candidates {
            let Some(device) = Self::open(&path) else {
                continue;
            };
            if unsafe { can_encode_h264(device.as_ptr()) } {
                log::info!("VAAPI device {path} provides H.264 encode — selected");
                return Some(device);
            }
            log::info!("VAAPI device {path} has no H.264 encode entrypoint (decode/VPP only)");
            if vpp_only.is_none() {
                vpp_only = Some(device);
            }
        }

        if vpp_only.is_some() {
            log::warn!(
                "No render node offers H.264 VAAPI encode; keeping a device for colour conversion and encoding in software"
            );
        } else {
            log::warn!("No usable VAAPI device — falling back to software encoding");
        }
        vpp_only
    }

    fn open(path: &str) -> Option<Self> {
        let mut hw_device_ref: *mut ffi::AVBufferRef = ptr::null_mut();
        let ret = unsafe {
            let device_path = CString::new(path).ok()?;
            ffi::av_hwdevice_ctx_create(
                &mut hw_device_ref,
                ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                device_path.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            log::info!("VAAPI device {path} unavailable: {} ({})", ret, av_err_string(ret));
            return None;
        }
        Some(Self { hw_device_ref })
    }

    fn as_ptr(&self) -> *mut ffi::AVBufferRef {
        self.hw_device_ref
    }
}

impl Drop for VaapiDeviceCtx {
    fn drop(&mut self) {
        unsafe {
            if !self.hw_device_ref.is_null() {
                ffi::av_buffer_unref(&mut self.hw_device_ref);
            }
        }
    }
}

pub struct VaapiEncoder {
    encoder: ffmpeg::codec::encoder::video::Encoder,

    hw_frames_ref: *mut ffi::AVBufferRef,
    scaler: ffmpeg::software::scaling::Context,
    scaler_src_format: ffmpeg::format::Pixel,
    cpu_src_frame: ffmpeg::frame::Video,
    cpu_sw_frame: ffmpeg::frame::Video,
    sw_pixel_format: ffmpeg::format::Pixel,
    width: u32,
    height: u32,
    width_i32: i32,
    height_i32: i32,
    pts: i64,
    pub selected_encoder: String,
    pub vpp_available: bool,
    /// Disabled after the driver rejects DMA-BUF import. A rejection such as
    /// ENOSYS is a capability mismatch, not a transient frame failure, so
    /// retrying it every frame only produces an error storm.
    pub dmabuf_import_available: bool,

    import_hw_frames_ref: *mut ffi::AVBufferRef,
    filter_graph: *mut ffi::AVFilterGraph,
    buffersrc_ctx: *mut ffi::AVFilterContext,
    buffersink_ctx: *mut ffi::AVFilterContext,

    // Reusable scratch frames — allocated once, unref'd + repopulated each encode.
    // Avoids 3x av_frame_alloc/free per frame (180 allocs/sec at 60fps).
    scratch_vaapi_frame: *mut ffi::AVFrame,
    scratch_nv12_frame: *mut ffi::AVFrame,

    // Reusable output buffer — avoids per-frame Vec allocation for packet collection.
    packet_buf: Vec<u8>,
    force_next_keyframe: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CpuEncodeTiming {
    pub copy_us: u128,
    pub convert_us: u128,
    pub codec_us: u128,
}

// VaapiEncoder is only ever used from the single Wayland capture thread.
unsafe impl Send for VaapiEncoder {}

/// Resolve libx264's VBV parameters, in kilobits, from the encoding settings.
///
/// x264 has no per-frame byte cap of its own: `vbv-bufsize` *is* the ceiling,
/// because a frame can never exceed the buffer it has to fit inside. So
/// `max_frame_size_bytes` is honoured here by deriving the buffer from it,
/// which keeps that one setting meaningful on both the VAAPI encoder (where it
/// maps to `max_frame_size`) and libx264. Without this, a machine that falls
/// back to software encoding has no frame-size bound at all, and keyframes grow
/// until a single one takes longer to put on the wire than several frame
/// intervals.
///
/// Returns `None` when nothing constrains the encoder, leaving x264's defaults.
fn libx264_vbv_kbits(
    maxrate_bps: usize,
    bufsize_bits: usize,
    max_frame_size_bytes: usize,
    bitrate_bps: usize,
) -> Option<(usize, usize)> {
    // A frame cap implies a rate cap: without one, x264 has no VBV to drain.
    let maxrate_kbps = if maxrate_bps > 0 { maxrate_bps / 1_000 } else { bitrate_bps / 1_000 };
    let bufsize_kbits = if max_frame_size_bytes > 0 {
        max_frame_size_bytes.saturating_mul(8) / 1_000
    } else if bufsize_bits > 0 {
        bufsize_bits / 1_000
    } else {
        return None;
    };
    if maxrate_kbps == 0 || bufsize_kbits == 0 {
        return None;
    }
    Some((maxrate_kbps, bufsize_kbits))
}

fn ffmpeg_dimension(name: &str, value: u32) -> Result<i32, String> {
    let value =
        i32::try_from(value).map_err(|_| format!("{name} exceeds FFmpeg's limit: {value}"))?;
    if value <= 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(value)
}

#[derive(Debug, PartialEq, Eq)]
struct DmabufPlaneLayout {
    object_size: usize,
    offset: isize,
    pitch: isize,
}

/// Validates the single-plane 32-bit RGB layout supplied by the compositor.
fn dmabuf_plane_layout(
    width: u32,
    height: u32,
    stride: u32,
    offset: u32,
) -> Result<DmabufPlaneLayout, String> {
    let minimum_stride =
        width.checked_mul(4).ok_or_else(|| format!("DMA-BUF width is too large: {width}"))?;
    if height == 0 || stride < minimum_stride {
        return Err(format!(
            "Invalid DMA-BUF layout: {width}x{height}, stride={stride}, offset={offset}"
        ));
    }

    let stride = usize::try_from(stride)
        .map_err(|_| "DMA-BUF stride does not fit this platform".to_string())?;
    let height = usize::try_from(height)
        .map_err(|_| "DMA-BUF height does not fit this platform".to_string())?;
    let offset_usize = usize::try_from(offset)
        .map_err(|_| "DMA-BUF offset does not fit this platform".to_string())?;
    let object_size = stride
        .checked_mul(height)
        .and_then(|pixels_size| offset_usize.checked_add(pixels_size))
        .ok_or_else(|| "DMA-BUF size overflows this platform".to_string())?;

    Ok(DmabufPlaneLayout {
        object_size,
        offset: isize::try_from(offset_usize)
            .map_err(|_| "DMA-BUF offset exceeds FFmpeg's limit".to_string())?,
        pitch: isize::try_from(stride)
            .map_err(|_| "DMA-BUF stride exceeds FFmpeg's limit".to_string())?,
    })
}

impl VaapiEncoder {
    /// Build an encoder. Pass `Some(device_ctx)` to enable the VAAPI paths;
    /// `None` forces software encoding. The device context is *borrowed* —
    /// its owner (typically `CaptureState`) keeps it alive across encoder
    /// recreations so we don't leak DRM fds (see `VaapiDeviceCtx` docs).
    pub fn new(
        device_ctx: Option<&VaapiDeviceCtx>,
        width: u32,
        height: u32,
        bitrate_bps: usize,
        framerate: u32,
        compositor_fourcc: u32,
    ) -> Result<Self, String> {
        let encoding = crate::anchorapp::settings::settings().encoding.clone();
        Self::new_with_settings(
            device_ctx,
            width,
            height,
            bitrate_bps,
            framerate,
            compositor_fourcc,
            &encoding,
        )
    }

    /// Build an encoder with explicit encoding settings.
    ///
    /// The desktop runtime uses [`Self::new`], which reads the application's
    /// global settings. This variant exists for deterministic offline
    /// profiling and tests: it exercises the same encoder construction and
    /// `encode_xbgr` path without reading or changing the user's config file.
    pub fn new_with_settings(
        device_ctx: Option<&VaapiDeviceCtx>,
        width: u32,
        height: u32,
        bitrate_bps: usize,
        framerate: u32,
        compositor_fourcc: u32,
        enc_settings: &crate::anchorapp::settings::EncodingSettings,
    ) -> Result<Self, String> {
        ensure_ffmpeg_init();
        let width_i32 = ffmpeg_dimension("encoder width", width)?;
        let height_i32 = ffmpeg_dimension("encoder height", height)?;
        let framerate_i32 = ffmpeg_dimension("encoder framerate", framerate)?;

        let mut hw_frames_ref: *mut ffi::AVBufferRef = ptr::null_mut();

        // Build a fresh hardware-frames context on top of the shared device.
        // The frames context internally holds its own ref to the device via
        // AVBufferRef refcounting, released when we unref hw_frames_ref on
        // encoder drop — no new fd is opened.
        let vaapi_available = match device_ctx {
            None => false,
            Some(ctx) => unsafe {
                let r = ffi::av_hwframe_ctx_alloc(ctx.as_ptr());
                if r.is_null() {
                    log::warn!("av_hwframe_ctx_alloc failed — will skip VAAPI encoders");
                    false
                } else {
                    let hwf = (*r).data as *mut ffi::AVHWFramesContext;
                    (*hwf).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
                    (*hwf).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
                    (*hwf).width = width_i32;
                    (*hwf).height = height_i32;
                    (*hwf).initial_pool_size = enc_settings.hw_pool_size as _;
                    let ret = ffi::av_hwframe_ctx_init(r);
                    if ret < 0 {
                        log::warn!(
                            "av_hwframe_ctx_init failed: {} ({}) — will skip VAAPI encoders",
                            ret,
                            av_err_string(ret)
                        );
                        ffi::av_buffer_unref(&mut (r as *mut _));
                        false
                    } else {
                        hw_frames_ref = r;
                        true
                    }
                }
            },
        };

        // Borrowed for local use inside the VAAPI setup paths below. Not
        // stored in the encoder — its lifetime is bound to the caller-owned
        // VaapiDeviceCtx.
        let hw_device_ref: *mut ffi::AVBufferRef =
            device_ctx.map(|c| c.as_ptr()).unwrap_or(ptr::null_mut());

        let encoder_names =
            vec!["h264_vaapi", "h264_nvenc", "h264_amf", "libx264", "libvpx-vp9", "libvpx"];
        let mut selected_encoder_name = String::new();
        let mut encoder = None;
        let mut last_error = "No encoders found".to_string();

        for encoder_name in &encoder_names {
            if *encoder_name == "h264_vaapi" && !vaapi_available {
                log::info!("Skipping {} — VAAPI not available", encoder_name);
                continue;
            }

            if let Some(codec) = ffmpeg::encoder::find_by_name(encoder_name) {
                log::info!("Trying encoder: {}", encoder_name);

                let mut encoder_ctx = match ffmpeg::codec::context::Context::new_with_codec(codec)
                    .encoder()
                    .video()
                {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        log::warn!("Failed to create {} context: {}", encoder_name, e);
                        last_error = format!("{}: {}", encoder_name, e);
                        continue;
                    }
                };

                encoder_ctx.set_width(width);
                encoder_ctx.set_height(height);
                encoder_ctx.set_frame_rate(Some(ffmpeg::Rational(framerate_i32, 1)));
                encoder_ctx.set_time_base(ffmpeg::Rational(1, framerate_i32));
                encoder_ctx.set_bit_rate(bitrate_bps);
                encoder_ctx.set_gop(enc_settings.gop_size);
                encoder_ctx.set_max_b_frames(enc_settings.max_b_frames as usize);

                // Set pixel format based on encoder type.
                // libx264 accepts NV12 natively; use it when VAAPI is available so
                // the VideoProc path can feed NV12 directly without an extra swscale.
                //
                // Use the raw C pixel format directly because older FFmpeg exposes
                // it as VAAPI_VLD in the Rust enum, newer as VAAPI.
                let pixel_format = match *encoder_name {
                    "h264_vaapi" => {
                        let raw = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
                        ffmpeg::format::Pixel::from(raw)
                    }
                    "libx264" if vaapi_available => ffmpeg::format::Pixel::NV12,
                    _ => ffmpeg::format::Pixel::YUV420P,
                };
                encoder_ctx.set_format(pixel_format);

                if *encoder_name == "h264_vaapi" {
                    unsafe {
                        (*encoder_ctx.as_mut_ptr()).hw_frames_ctx =
                            ffi::av_buffer_ref(hw_frames_ref);
                    }
                }

                let mut opts = ffmpeg::Dictionary::new();

                let async_depth = enc_settings.async_depth;
                if async_depth > 0 {
                    opts.set("async_depth", &async_depth.to_string());
                }
                if enc_settings.maxrate_bps > 0 {
                    unsafe {
                        (*encoder_ctx.as_mut_ptr()).rc_max_rate = enc_settings.maxrate_bps as i64;
                    }
                }
                if enc_settings.bufsize_bits > 0 {
                    unsafe {
                        (*encoder_ctx.as_mut_ptr()).rc_buffer_size =
                            enc_settings.bufsize_bits as i32;
                    }
                }

                match *encoder_name {
                    "h264_vaapi" => {
                        opts.set("repeat_pps", "1");
                        opts.set("idr_interval", &enc_settings.gop_size.to_string());
                        opts.set("rc_mode", &enc_settings.rate_control);
                        if enc_settings.low_power {
                            opts.set("low_power", "1");
                        }
                        if enc_settings.profile != "auto" {
                            opts.set("profile", &enc_settings.profile);
                        }
                        if enc_settings.level != "auto" {
                            opts.set("level", &enc_settings.level);
                        }
                        if enc_settings.coder != "cabac" {
                            opts.set("coder", &enc_settings.coder);
                        }
                        if enc_settings.qp > 0 {
                            opts.set("qp", &enc_settings.qp.to_string());
                        }
                        // Hard per-frame byte cap — keeps scene-change/IDR
                        // frames from ballooning into hundreds of UDP
                        // fragments on lossy links. A bounded default is
                        // important for QUIC datagrams: one lost fragment
                        // discards the complete access unit. Callers can
                        // override it explicitly with a non-zero setting.
                        let max_frame_size = if enc_settings.max_frame_size_bytes > 0 {
                            enc_settings.max_frame_size_bytes
                        } else {
                            120_000
                        };
                        opts.set("max_frame_size", &max_frame_size.to_string());
                    }
                    "libx264" => {
                        opts.set("preset", &enc_settings.x264_preset);
                        opts.set("tune", &enc_settings.x264_tune);
                        opts.set("repeat-headers", "1");
                        // Keep the inter-frame cadence explicit in libx264's
                        // native option syntax.  AVCodecContext::gop_size is
                        // not consistently forwarded by FFmpeg builds, and a
                        // missing keyint makes x264 choose every frame as an
                        // IDR under the zero-latency tune.  That spends the
                        // entire bitrate budget on intra frames rather than
                        // preserving terminal detail in P frames.
                        let mut x264_params = format!(
                            "keyint={}:min-keyint={}:scenecut=0:open-gop=0",
                            enc_settings.gop_size, enc_settings.gop_size
                        );

                        // Bound the slice count. `tune=zerolatency` makes x264
                        // thread by slice, and left to itself it scales that
                        // with the host's core count -- 17 slices per 1080p
                        // frame on a 32-core machine. Slice boundaries reset
                        // intra prediction and CABAC context, so that inflates
                        // keyframes without making them arrive any sooner.
                        if enc_settings.x264_threads > 0 {
                            x264_params
                                .push_str(&format!(":threads={}", enc_settings.x264_threads));
                        }

                        // FFmpeg's AVCodecContext rate fields do not reliably
                        // reach libx264's VBV controller on every build. Pass
                        // the native x264 parameters as well, in kilobits.
                        if let Some((vbv_maxrate_kbps, vbv_bufsize_kbits)) = libx264_vbv_kbits(
                            enc_settings.maxrate_bps,
                            enc_settings.bufsize_bits,
                            enc_settings.max_frame_size_bytes,
                            bitrate_bps,
                        ) {
                            x264_params.push_str(&format!(
                                ":vbv-maxrate={vbv_maxrate_kbps}:vbv-bufsize={vbv_bufsize_kbits}"
                            ));
                        }
                        opts.set("x264-params", &x264_params);
                        if enc_settings.profile != "auto" {
                            opts.set("profile", &enc_settings.profile);
                        }
                        if enc_settings.level != "auto" {
                            opts.set("level", &enc_settings.level);
                        }
                    }
                    "libvpx-vp9" => {
                        opts.set("deadline", "realtime");
                        opts.set("cpu-used", "8");
                    }
                    "libvpx" => {
                        opts.set("deadline", "realtime");
                        opts.set("cpu-used", "8");
                    }
                    _ => {}
                }

                match encoder_ctx.open_with(opts) {
                    Ok(enc) => {
                        log::info!("Successfully opened {} encoder", encoder_name);
                        selected_encoder_name = encoder_name.to_string();
                        encoder = Some(enc);
                        break;
                    }
                    Err(e) => {
                        log::warn!("Failed to open {}: {}", encoder_name, e);
                        last_error = format!("{}: {}", encoder_name, e);
                    }
                }
            }
        }

        let encoder =
            encoder.ok_or_else(|| format!("All encoders failed. Last error: {}", last_error))?;

        // Free the encoder surface pool if we're NOT using a VAAPI encoder.
        // Keep hw_device_ref alive — we need it for the VideoProc colour-convert
        // pipeline (DMA-BUF → scale_vaapi → NV12 → CPU → libx264).
        if selected_encoder_name != "h264_vaapi" {
            unsafe {
                if !hw_frames_ref.is_null() {
                    ffi::av_buffer_unref(&mut hw_frames_ref);
                    hw_frames_ref = ptr::null_mut();
                }
            }
            log::info!(
                "Using software encoder {} with VAAPI VideoProc for colour conversion",
                selected_encoder_name
            );
        }

        // Determine the software pixel format the encoder expects.
        // libx264 is configured for NV12 when VAAPI is available so that:
        //   (a) the VideoProc path feeds NV12 directly, and
        //   (b) the encode_xbgr fallback uses a BGRZ→NV12 swscale instead of BGRZ→YUV420P.
        let sw_pixel_format = match selected_encoder_name.as_str() {
            "h264_vaapi" => ffmpeg::format::Pixel::NV12,
            "libx264" if vaapi_available => ffmpeg::format::Pixel::NV12,
            _ => ffmpeg::format::Pixel::YUV420P,
        };

        let scaler_src_format = ffmpeg::format::Pixel::from(
            drm_fourcc_to_av_pixfmt(compositor_fourcc)
                .map_err(|e| format!("Unsupported CPU fallback format: {e}"))?,
        );
        let scaler = ffmpeg::software::scaling::Context::get(
            scaler_src_format,
            width,
            height,
            sw_pixel_format,
            width,
            height,
            ffmpeg::software::scaling::Flags::FAST_BILINEAR,
        )
        .map_err(|e| format!("swscale context creation failed: {}", e))?;
        let cpu_src_frame = ffmpeg::frame::Video::new(scaler_src_format, width, height);
        let cpu_sw_frame = ffmpeg::frame::Video::new(sw_pixel_format, width, height);

        // Set up whenever a VAAPI device is available, regardless of which encoder was
        // selected. For h264_vaapi this is the zero-copy encode path. For software
        // encoders (e.g. libx264) this is the VideoProc colour-convert path:
        //   DMA-BUF → VAAPI import → scale_vaapi (XBGR→NV12 on GPU) → av_hwframe_transfer_data → CPU
        let (import_hw_frames_ref, filter_graph, buffersrc_ctx, buffersink_ctx) = if vaapi_available
        {
            match Self::setup_dmabuf_pipeline(
                hw_device_ref,
                width,
                height,
                framerate,
                compositor_fourcc,
            ) {
                Ok(pipeline) => {
                    if selected_encoder_name == "h264_vaapi" {
                        log::info!(
                            "Zero-copy DMA-BUF pipeline initialized for fourcc 0x{:08x}",
                            compositor_fourcc
                        );
                    } else {
                        log::info!(
                            "VideoProc DMA-BUF→NV12 pipeline initialized (GPU colour convert for {})",
                            selected_encoder_name
                        );
                    }
                    pipeline
                }
                Err(e) => {
                    log::warn!(
                        "DMA-BUF pipeline setup failed: {} — CPU colour convert path will be used",
                        e
                    );
                    (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
                }
            }
        } else {
            (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
        };

        log::info!(
            "Encoder initialized [{}]: {}x{} @ {} fps, {} bps, sw_fmt={:?}",
            selected_encoder_name,
            width,
            height,
            framerate,
            bitrate_bps,
            sw_pixel_format,
        );

        let vpp_available = !filter_graph.is_null() && selected_encoder_name != "h264_vaapi";

        // Pre-allocate scratch frames — reused every encode instead of alloc/free per frame.
        let scratch_vaapi_frame = unsafe { ffi::av_frame_alloc() };
        let scratch_nv12_frame = unsafe { ffi::av_frame_alloc() };
        if scratch_vaapi_frame.is_null() || scratch_nv12_frame.is_null() {
            return Err("Failed to pre-allocate scratch frames".into());
        }

        Ok(VaapiEncoder {
            encoder,
            hw_frames_ref,
            scaler,
            scaler_src_format,
            cpu_src_frame,
            cpu_sw_frame,
            sw_pixel_format,
            width,
            height,
            width_i32,
            height_i32,
            pts: 0,
            selected_encoder: selected_encoder_name,
            vpp_available,
            dmabuf_import_available: true,
            import_hw_frames_ref,
            filter_graph,
            buffersrc_ctx,
            buffersink_ctx,
            scratch_vaapi_frame,
            scratch_nv12_frame,
            packet_buf: Vec::with_capacity(256 * 1024), // 256KB initial capacity
            force_next_keyframe: false,
        })
    }

    pub fn request_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    fn apply_keyframe_request_raw(&mut self, frame: *mut ffi::AVFrame) {
        unsafe {
            if self.force_next_keyframe {
                log::info!("Forcing next encoded frame to keyframe");
                (*frame).pict_type = ffi::AVPictureType::AV_PICTURE_TYPE_I;
                self.force_next_keyframe = false;
            } else {
                (*frame).pict_type = ffi::AVPictureType::AV_PICTURE_TYPE_NONE;
            }
        }
    }

    fn apply_keyframe_request_video(&mut self, frame: &mut ffmpeg::frame::Video) {
        unsafe { self.apply_keyframe_request_raw(frame.as_mut_ptr()) }
    }

    /// Build the DMA-BUF → VAAPI → scale_vaapi → NV12 filter pipeline.
    ///
    /// This is called once during encoder construction and kept alive for the
    /// lifetime of the encoder. Returns (import_frames_ref, graph, src_ctx, sink_ctx).
    fn setup_dmabuf_pipeline(
        hw_device_ref: *mut ffi::AVBufferRef,
        width: u32,
        height: u32,
        framerate: u32,
        compositor_fourcc: u32,
    ) -> Result<
        (
            *mut ffi::AVBufferRef,     // import_hw_frames_ref
            *mut ffi::AVFilterGraph,   // filter_graph
            *mut ffi::AVFilterContext, // buffersrc_ctx
            *mut ffi::AVFilterContext, // buffersink_ctx
        ),
        String,
    > {
        let width_i32 = ffmpeg_dimension("DMA-BUF import width", width)?;
        let height_i32 = ffmpeg_dimension("DMA-BUF import height", height)?;

        // Map the compositor's DRM fourcc to the FFmpeg sw_format for import.
        // This MUST match or colors will be corrupted!
        let sw_format = drm_fourcc_to_av_pixfmt(compositor_fourcc)?;

        log::info!(
            "[dmabuf] Setting up import pipeline: DRM fourcc 0x{:08x} → FFmpeg sw_format {}",
            compositor_fourcc,
            sw_format as i32
        );

        unsafe {
            // We need a second AVHWFramesContext whose sw_format matches the compositor's
            // DRM fourcc. This tells the VAAPI driver what pixel format to expect when
            // we import the DMA-BUF. Using the wrong sw_format causes color corruption!
            //
            // For example:
            //   - Compositor sends XBGR8888 (0x34324258) → use AV_PIX_FMT_BGR0
            //   - Compositor sends XRGB8888 (0x34325258) → use AV_PIX_FMT_0RGB
            //
            // initial_pool_size=0 means: don't pre-allocate any surfaces.
            // We're not drawing from a pool here — each frame is imported on-the-fly
            // from an externally-owned DMA-BUF file descriptor.
            let import_frames_ref = ffi::av_hwframe_ctx_alloc(hw_device_ref);
            if import_frames_ref.is_null() {
                return Err("av_hwframe_ctx_alloc (import) failed".into());
            }
            let ctx = (*import_frames_ref).data as *mut ffi::AVHWFramesContext;
            (*ctx).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI; // the HW surface type
            (*ctx).sw_format = sw_format; // the pixel layout of the DMA-BUF (from compositor!)
            (*ctx).width = width_i32;
            (*ctx).height = height_i32;
            (*ctx).initial_pool_size = 0; // import-only — no pool needed

            let ret = ffi::av_hwframe_ctx_init(import_frames_ref);
            if ret < 0 {
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("av_hwframe_ctx_init (import) failed: {}", av_err_string(ret)));
            }

            // A filter graph is a directed graph of AVFilterContexts. We'll build:
            //   buffersrc → scale_vaapi → buffersink
            //
            // avfilter_graph_alloc() just allocates the container. No filters are
            // inside it yet.
            let graph = ffi::avfilter_graph_alloc();
            if graph.is_null() {
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err("avfilter_graph_alloc failed".into());
            }

            // "buffer" is the source filter for video frames. It accepts AVFrames
            // pushed by the application (us) and passes them downstream.
            //
            // We MUST NOT use avfilter_graph_create_filter here. That function
            // allocates AND initializes in one shot — the filter's init callback runs
            // immediately, before we can call av_buffersrc_parameters_set. VAAPI pixel
            // formats fail av_pix_fmt_desc_get validation inside that init, causing
            // "Invalid argument". The FFmpeg docs themselves warn:
            //   "Since the filter is initialized after this function successfully
            //    returns, you MUST NOT set any further options on it. If you need to
            //    do that, call avfilter_graph_alloc_filter(), followed by setting the
            //    options, followed by avfilter_init_dict() instead."
            //
            // Correct three-step sequence for hardware filters:
            //   C1. avfilter_graph_alloc_filter — allocate only, no init
            //   C2. av_buffersrc_parameters_set — attach hw_frames_ctx before init
            //   C3. avfilter_init_str           — run init with params already stored
            let src_filter_name = CString::new("buffer").unwrap();
            let src_filter = ffi::avfilter_get_by_name(src_filter_name.as_ptr());
            if src_filter.is_null() {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err("buffersrc filter not found".into());
            }

            // The returned pointer is graph-owned — avfilter_graph_free() will free it.
            let src_inst_name = CString::new("in").unwrap();
            let src_ctx =
                ffi::avfilter_graph_alloc_filter(graph, src_filter, src_inst_name.as_ptr());
            if src_ctx.is_null() {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err("avfilter_graph_alloc_filter (buffersrc) failed".into());
            }

            // av_buffersrc_parameters_set stores the params in the filter's private state.
            // The filter's init callback (called below in C3) reads them and applies them,
            // overriding any conflicting values from the option string.
            // hw_frames_ctx carries: device, sw_format (BGR0), width, height.
            // format=VAAPI tells buffersrc what kind of frames to expect on its input pad.
            let par = ffi::av_buffersrc_parameters_alloc();
            if par.is_null() {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err("av_buffersrc_parameters_alloc failed".into());
            }
            (*par).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            // av_buffer_ref increments the refcount — we still own import_frames_ref.
            (*par).hw_frames_ctx = ffi::av_buffer_ref(import_frames_ref);
            let ret = ffi::av_buffersrc_parameters_set(src_ctx, par);
            // av_free releases the AVBufferSrcParameters struct itself. The hw_frames_ctx
            // reference inside is now owned by the filter's private state.
            ffi::av_free(par as *mut _);
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("av_buffersrc_parameters_set failed: {}", av_err_string(ret)));
            }

            // avfilter_init_str parses the colon-separated option string and runs the
            // filter's init callback. The callback applies the params stored in C2,
            // which take precedence over the pix_fmt in the args string.
            let src_args = CString::new(format!(
                "width={}:height={}:pix_fmt=vaapi:time_base=1/{}:frame_rate={}/1",
                width, height, framerate, framerate,
            ))
            .unwrap();
            let ret = ffi::avfilter_init_str(src_ctx, src_args.as_ptr());
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("buffersrc init failed: {}", av_err_string(ret)));
            }

            // ── Step D: scale_vaapi filter ────────────────────────────────────────
            //
            // scale_vaapi is a GPU-side VPP (Video Post Processing) filter that runs
            // entirely on the VAAPI device. Here we use it purely for colour conversion:
            //   XBGR (BGR0) VAAPI surface  →  NV12 VAAPI surface
            //
            // "format=nv12" in the args tells scale_vaapi what output pixel format to
            // produce. h264_vaapi requires NV12 input, so this is mandatory.
            //
            // We must also set hw_device_ctx on the filter context so scale_vaapi
            // knows which VAAPI device to run on. Without this it errors at graph config.
            let scale_filter_name = CString::new("scale_vaapi").unwrap();
            let scale_filter = ffi::avfilter_get_by_name(scale_filter_name.as_ptr());
            if scale_filter.is_null() {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(
                    "scale_vaapi filter not found — is ffmpeg built with VAAPI support?".into()
                );
            }

            // Configure scale_vaapi for RGB→NV12 conversion on GPU.
            // Note: in_range/out_range options are not supported by scale_vaapi.
            // The driver should handle full-range RGB→NV12 conversion automatically.
            let enc_settings = &crate::anchorapp::settings::settings().encoding;
            let mut scale_parts: Vec<String> = vec!["format=nv12".to_string()];
            if enc_settings.scale_mode != "hq" {
                scale_parts.push(format!("mode={}", enc_settings.scale_mode));
            }
            let scale_args_str = scale_parts.join(":");
            log::info!("scale_vaapi args: {}", scale_args_str);
            let scale_args = CString::new(scale_args_str).unwrap();
            let scale_inst_name = CString::new("scale").unwrap();
            let mut scale_ctx: *mut ffi::AVFilterContext = ptr::null_mut();
            let ret = ffi::avfilter_graph_create_filter(
                &mut scale_ctx,
                scale_filter,
                scale_inst_name.as_ptr(),
                scale_args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("scale_vaapi create failed: {}", av_err_string(ret)));
            }

            // Attach the VAAPI device to scale_vaapi. av_buffer_ref gives the
            // filter its own reference; the shared VaapiDeviceCtx keeps the
            // underlying AVBufferRef alive until the capture session ends.
            (*scale_ctx).hw_device_ctx = ffi::av_buffer_ref(hw_device_ref);

            // ── Step E: buffersink filter ─────────────────────────────────────────
            //
            // "buffersink" is the consumer side of the graph. We call
            // av_buffersink_get_frame() on it each iteration to pull the NV12 VAAPI
            // frame that scale_vaapi produced.
            let sink_filter_name = CString::new("buffersink").unwrap();
            let sink_filter = ffi::avfilter_get_by_name(sink_filter_name.as_ptr());
            if sink_filter.is_null() {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err("buffersink filter not found".into());
            }

            let sink_inst_name = CString::new("out").unwrap();
            let mut sink_ctx: *mut ffi::AVFilterContext = ptr::null_mut();
            let ret = ffi::avfilter_graph_create_filter(
                &mut sink_ctx,
                sink_filter,
                sink_inst_name.as_ptr(),
                ptr::null(), // no args needed for sink
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("buffersink create failed: {}", av_err_string(ret)));
            }

            // ── Step F: Link the filters ──────────────────────────────────────────
            //
            // avfilter_link(src, src_pad, dst, dst_pad) connects one filter output
            // to another filter input. All our filters are single-input single-output
            // so the pad indices are both 0.
            let ret = ffi::avfilter_link(src_ctx, 0, scale_ctx, 0);
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("link src→scale failed: {}", av_err_string(ret)));
            }
            let ret = ffi::avfilter_link(scale_ctx, 0, sink_ctx, 0);
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!("link scale→sink failed: {}", av_err_string(ret)));
            }

            // ── Step G: Configure the graph ───────────────────────────────────────
            //
            // avfilter_graph_config() walks the graph, negotiates formats between
            // filters, and finalises the pipeline. If any filter is misconfigured
            // (e.g. scale_vaapi can't find the device, format mismatch) it fails here.
            // This is where the previous implementation was likely breaking.
            let ret = ffi::avfilter_graph_config(graph, ptr::null_mut());
            if ret < 0 {
                ffi::avfilter_graph_free(&mut (graph as *mut _));
                ffi::av_buffer_unref(&mut (import_frames_ref as *mut _));
                return Err(format!(
                    "avfilter_graph_config failed: {} — check VAAPI driver support",
                    av_err_string(ret)
                ));
            }

            Ok((import_frames_ref, graph, src_ctx, sink_ctx))
        } // end unsafe
    }

    /// Zero-copy encode: import the DMA-BUF directly into VAAPI without touching
    /// any pixels on the CPU, then do GPU colour conversion and GPU encode.
    ///
    /// Returns Err if the pipeline isn't available or if the driver rejects the
    /// import — the caller should fall back to encode_xbgr().
    ///
    /// Arguments:
    ///   dmabuf_fd     — raw file descriptor of the GBM BufferObject (stays open by caller)
    ///   stride        — row stride in bytes (from GBM bo)
    ///   offset        — plane offset within the buffer (always 0 for single-plane formats)
    ///   modifier      — DRM format modifier (e.g. DRM_FORMAT_MOD_LINEAR = 0)
    ///   drm_fourcc    — DRM fourcc pixel format (must match the GBM buffer format!)
    pub fn encode_dmabuf(
        &mut self,
        dmabuf_fd: i32,
        stride: u32,
        offset: u32,
        modifier: u64,
        drm_fourcc: u32,
    ) -> Result<(Vec<u8>, DmabufEncodeTiming), String> {
        let total_start = Instant::now();
        // If setup_dmabuf_pipeline failed during new(), filter_graph is null.
        // Return an error so the caller immediately falls back to the CPU path.
        if self.filter_graph.is_null() {
            return Err("DMA-BUF pipeline not initialized".into());
        }

        // Debug first frame to verify buffer parameters
        if self.pts == 0 {
            log::info!(
                "[dmabuf] encode_dmabuf called: fd={}, {}x{}, stride={}, offset={}, modifier=0x{:x}, fourcc=0x{:08x}",
                dmabuf_fd,
                self.width,
                self.height,
                stride,
                offset,
                modifier,
                drm_fourcc
            );
            if modifier != 0 {
                log::warn!(
                    "[dmabuf] Non-LINEAR modifier detected (0x{:x}) — may cause import issues",
                    modifier
                );
            }
        }

        // Validate that we support this fourcc
        drm_fourcc_to_av_pixfmt(drm_fourcc).map_err(|e| format!("Cannot import DMA-BUF: {}", e))?;
        let layout = dmabuf_plane_layout(self.width, self.height, stride, offset)?;

        let mut timing = DmabufEncodeTiming {
            input_pts: self.pts,
            output_pts: None,
            output_packet_count: 0,
            dup_fd_us: 0,
            create_descriptor_us: 0,
            hwframe_map_us: 0,
            buffersrc_add_us: 0,
            buffersink_get_us: 0,
            send_frame_us: 0,
            receive_packets_us: 0,
            total_us: 0,
        };

        unsafe {
            // ── Step 1: Duplicate the DMA-BUF fd for thread-safe reference counting ──
            let t = Instant::now();
            let dup_fd = libc::dup(dmabuf_fd);
            if dup_fd < 0 {
                return Err(format!(
                    "Failed to duplicate DMA-BUF fd: {}",
                    std::io::Error::last_os_error()
                ));
            }
            timing.dup_fd_us = t.elapsed().as_micros();

            // ── Step 2: Describe the DMA-BUF with AVDRMFrameDescriptor ────────
            let t = Instant::now();
            let drm_desc = Box::into_raw(Box::new(ffi::AVDRMFrameDescriptor {
                nb_objects: 1,
                objects: {
                    // The array has 4 slots (AV_DRM_MAX_PLANES) but we only fill index 0.
                    // std::mem::zeroed() zero-fills the unused slots safely.
                    let mut objs = std::mem::zeroed::<[ffi::AVDRMObjectDescriptor; 4]>();
                    objs[0] = ffi::AVDRMObjectDescriptor {
                        fd: dup_fd, // Use the duplicated fd for proper refcounting
                        // Size covers the plane data and its offset into the DMA-BUF.
                        size: layout.object_size,
                        // format_modifier encodes tiling (e.g. Intel X-tile, Y-tile).
                        // For LINEAR buffers (what GBM LINEAR flag gives us), this is 0.
                        format_modifier: modifier,
                    };
                    objs
                },
                nb_layers: 1,
                layers: {
                    let mut layers = std::mem::zeroed::<[ffi::AVDRMLayerDescriptor; 4]>();
                    layers[0] = ffi::AVDRMLayerDescriptor {
                        // DRM fourcc — MUST match the actual GBM buffer format!
                        // Using the wrong fourcc causes color corruption.
                        format: drm_fourcc,
                        nb_planes: 1,
                        planes: {
                            let mut planes = std::mem::zeroed::<[ffi::AVDRMPlaneDescriptor; 4]>();
                            planes[0] = ffi::AVDRMPlaneDescriptor {
                                object_index: 0, // refers to objects[0] above
                                offset: layout.offset,
                                pitch: layout.pitch, // bytes per row
                            };
                            planes
                        },
                    };
                    layers
                },
            }));

            // ── Step 2: Wrap descriptor in an AVFrame (DRM PRIME format) ──────
            //
            // AV_PIX_FMT_DRM_PRIME is FFmpeg's pixel format for frames whose
            // data is described by an AVDRMFrameDescriptor. The actual pixels
            // never leave the DMA-BUF — this frame is purely a metadata wrapper.
            //
            // data[0] holds the raw pointer to our AVDRMFrameDescriptor.
            // We don't set buf[0] (the AVBufferRef for automatic free), so FFmpeg
            // will NOT free the descriptor when av_frame_free() is called.
            // We manually drop(Box::from_raw(drm_desc)) after we're done.
            let drm_frame = ffi::av_frame_alloc();
            if drm_frame.is_null() {
                drop(Box::from_raw(drm_desc));
                return Err("av_frame_alloc (drm) failed".into());
            }
            (*drm_frame).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*drm_frame).width = self.width_i32;
            (*drm_frame).height = self.height_i32;
            (*drm_frame).data[0] = drm_desc as *mut u8;
            // Wrap the descriptor in an AVBufferRef so this frame is reference-counted.
            // av_frame_ref() in ff_hwframe_map_create requires buf[0] != NULL; without it,
            // av_hwframe_map returns EINVAL even though vaCreateSurfaces() succeeds.
            (*drm_frame).buf[0] = ffi::av_buffer_create(
                drm_desc as *mut u8,
                std::mem::size_of::<ffi::AVDRMFrameDescriptor>() as _,
                Some(drm_desc_free),
                ptr::null_mut(),
                0,
            );
            if (*drm_frame).buf[0].is_null() {
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                drop(Box::from_raw(drm_desc));
                return Err("av_buffer_create (drm_desc) failed".into());
            }
            // From this point on, drm_frame owns drm_desc via buf[0].
            // av_frame_free(drm_frame) will call drm_desc_free automatically.
            // Do NOT call drop(Box::from_raw(drm_desc)) after this point.
            timing.create_descriptor_us = t.elapsed().as_micros();

            // ── Step 3: Prepare scratch VAAPI frame for mapping ────────────────
            let vaapi_frame = self.scratch_vaapi_frame;
            ffi::av_frame_unref(vaapi_frame);
            (*vaapi_frame).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            (*vaapi_frame).width = self.width_i32;
            (*vaapi_frame).height = self.height_i32;
            (*vaapi_frame).hw_frames_ctx = ffi::av_buffer_ref(self.import_hw_frames_ref);

            // ── Step 4: Map DRM PRIME → VAAPI (the zero-copy step) ───────────
            let t = Instant::now();
            let ret = ffi::av_hwframe_map(vaapi_frame, drm_frame, ffi::AV_HWFRAME_MAP_READ as i32);
            timing.hwframe_map_us = t.elapsed().as_micros();
            if ret < 0 {
                log::error!(
                    "[dmabuf] av_hwframe_map failed: {} ({}) - DMA-BUF import rejected by driver. \
                    Check: 1) modifier is LINEAR, 2) format matches (fourcc=0x{:08x}), \
                    3) VAAPI driver supports DMA-BUF import",
                    ret,
                    av_err_string(ret),
                    drm_fourcc
                );
                ffi::av_frame_unref(vaapi_frame);
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                return Err(format!("av_hwframe_map failed: {} ({})", ret, av_err_string(ret)));
            }

            // Debug: Log the imported surface details on first frame
            if self.pts == 0 {
                log::info!(
                    "[dmabuf] Imported VAAPI surface: {}x{}, format={}, stride={}, offset={}, modifier=0x{:x}",
                    self.width,
                    self.height,
                    (*vaapi_frame).format,
                    stride,
                    offset,
                    modifier
                );
            }

            // ── Step 5: Push the VAAPI frame into the filter graph ────────────
            (*vaapi_frame).pts = self.pts;
            let t = Instant::now();
            let ret = ffi::av_buffersrc_add_frame_flags(
                self.buffersrc_ctx,
                vaapi_frame,
                ffi::AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            );
            timing.buffersrc_add_us = t.elapsed().as_micros();
            if ret < 0 {
                log::error!(
                    "[dmabuf] av_buffersrc_add_frame failed at pts={}: {} ({}) - filter rejected frame",
                    self.pts,
                    ret,
                    av_err_string(ret)
                );
                ffi::av_frame_unref(vaapi_frame);
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                return Err(format!("av_buffersrc_add_frame failed: {}", av_err_string(ret)));
            }

            // ── Step 6: Pull the NV12 VAAPI frame out of scale_vaapi ─────────
            let nv12_frame = self.scratch_nv12_frame;
            ffi::av_frame_unref(nv12_frame);
            let t = Instant::now();
            let ret = ffi::av_buffersink_get_frame(self.buffersink_ctx, nv12_frame);
            timing.buffersink_get_us = t.elapsed().as_micros();
            if ret < 0 {
                log::error!(
                    "[dmabuf] av_buffersink_get_frame failed at pts={}: {} ({}) - scale_vaapi conversion failed",
                    self.pts,
                    ret,
                    av_err_string(ret)
                );
                ffi::av_frame_unref(nv12_frame);
                ffi::av_frame_unref(vaapi_frame);
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                return Err(format!("av_buffersink_get_frame failed: {}", av_err_string(ret)));
            }

            // ── Step 7: Send the NV12 VAAPI frame to h264_vaapi ──────────────
            (*nv12_frame).pts = self.pts;
            self.apply_keyframe_request_raw(nv12_frame);
            self.pts += 1;
            let t = Instant::now();
            let ret = ffi::avcodec_send_frame(self.encoder.as_mut_ptr(), nv12_frame);
            timing.send_frame_us = t.elapsed().as_micros();

            // Free drm_frame (owns the DMA-BUF fd via buf[0] callback).
            // Scratch frames (vaapi_frame, nv12_frame) are NOT freed — reused next call.
            ffi::av_frame_free(&mut (drm_frame as *mut _));

            if ret < 0 {
                log::error!(
                    "[dmabuf] avcodec_send_frame failed at pts={}: {} ({})",
                    self.pts - 1,
                    ret,
                    av_err_string(ret)
                );
                return Err(format!("avcodec_send_frame failed: {} ({})", ret, av_err_string(ret)));
            }

            // ── Step 8: Collect encoded H.264 packets into reusable buffer ───
            let t = Instant::now();
            self.packet_buf.clear();
            let mut packet = ffmpeg::Packet::empty();
            while self.encoder.receive_packet(&mut packet).is_ok() {
                timing.output_packet_count = timing.output_packet_count.saturating_add(1);
                timing.output_pts = packet.pts();
                if let Some(data) = packet.data() {
                    self.packet_buf.extend_from_slice(data);
                }
            }
            timing.receive_packets_us = t.elapsed().as_micros();

            if self.pts > 1 && self.packet_buf.is_empty() {
                log::warn!(
                    "[dmabuf] No packets received for frame {} - encoder may be buffering (can cause instability)",
                    self.pts - 1
                );
            }

            timing.total_us = total_start.elapsed().as_micros();

            // Log detailed timing every 60 frames for profiling
            if self.pts
                % crate::anchorapp::settings::settings().capture.stats_log_interval_frames as i64
                == 0
            {
                log::info!(
                    "[perf-dmabuf] frame={} | dup={}us desc={}us map={}us filt_in={}us filt_out={}us send={}us recv={}us | TOTAL={}us packets={}",
                    self.pts - 1,
                    timing.dup_fd_us,
                    timing.create_descriptor_us,
                    timing.hwframe_map_us,
                    timing.buffersrc_add_us,
                    timing.buffersink_get_us,
                    timing.send_frame_us,
                    timing.receive_packets_us,
                    timing.total_us,
                    self.packet_buf.len(),
                );
                crate::metrics::emit(serde_json::json!({
                    "t": "encode",
                    "frame": self.pts - 1,
                    "dup_us": timing.dup_fd_us,
                    "desc_us": timing.create_descriptor_us,
                    "map_us": timing.hwframe_map_us,
                    "filt_in_us": timing.buffersrc_add_us,
                    "filt_out_us": timing.buffersink_get_us,
                    "send_us": timing.send_frame_us,
                    "recv_us": timing.receive_packets_us,
                    "total_us": timing.total_us,
                    "packets": self.packet_buf.len(),
                }));
            }

            // Take ownership of the buffer; it will be re-allocated on next frame's clear().
            // This avoids a copy — the Vec moves to the caller, and packet_buf becomes empty.
            Ok((std::mem::take(&mut self.packet_buf), timing))
        }
    }

    /// GPU colour-convert path for software encoders (e.g. libx264).
    ///
    /// Uses the same DMA-BUF → VAAPI import → scale_vaapi pipeline as the zero-copy
    /// path, but instead of sending the NV12 surface to h264_vaapi it downloads it
    /// to a CPU frame and feeds it to the software encoder.
    ///
    /// Compared to encode_xbgr this avoids:
    ///   - bo.map()  (no CPU mmap of the 7.9 MB XBGR buffer)
    ///   - BGRZ→NV12 swscale on CPU
    ///   - transferring 7.9 MB across the CPU/GPU boundary (NV12 = 3.1 MB instead)
    pub fn encode_dmabuf_vpp(
        &mut self,
        dmabuf_fd: i32,
        stride: u32,
        offset: u32,
        modifier: u64,
        drm_fourcc: u32,
    ) -> Result<Vec<Vec<u8>>, String> {
        if self.filter_graph.is_null() {
            return Err("VideoProc pipeline not initialized".into());
        }

        drm_fourcc_to_av_pixfmt(drm_fourcc).map_err(|e| format!("Cannot import DMA-BUF: {}", e))?;
        let layout = dmabuf_plane_layout(self.width, self.height, stride, offset)?;

        unsafe {
            // Steps 1-4: identical to encode_dmabuf — duplicate fd, build DRM descriptor,
            // allocate VAAPI frame, map DRM PRIME → VAAPI.
            let dup_fd = libc::dup(dmabuf_fd);
            if dup_fd < 0 {
                return Err(format!("dup(dmabuf_fd) failed: {}", std::io::Error::last_os_error()));
            }

            let drm_desc = Box::into_raw(Box::new(ffi::AVDRMFrameDescriptor {
                nb_objects: 1,
                objects: {
                    let mut objs = std::mem::zeroed::<[ffi::AVDRMObjectDescriptor; 4]>();
                    objs[0] = ffi::AVDRMObjectDescriptor {
                        fd: dup_fd,
                        size: layout.object_size,
                        format_modifier: modifier,
                    };
                    objs
                },
                nb_layers: 1,
                layers: {
                    let mut layers = std::mem::zeroed::<[ffi::AVDRMLayerDescriptor; 4]>();
                    layers[0] = ffi::AVDRMLayerDescriptor {
                        format: drm_fourcc,
                        nb_planes: 1,
                        planes: {
                            let mut planes = std::mem::zeroed::<[ffi::AVDRMPlaneDescriptor; 4]>();
                            planes[0] = ffi::AVDRMPlaneDescriptor {
                                object_index: 0,
                                offset: layout.offset,
                                pitch: layout.pitch,
                            };
                            planes
                        },
                    };
                    layers
                },
            }));

            let drm_frame = ffi::av_frame_alloc();
            if drm_frame.is_null() {
                drop(Box::from_raw(drm_desc));
                return Err("av_frame_alloc (drm) failed".into());
            }
            (*drm_frame).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*drm_frame).width = self.width_i32;
            (*drm_frame).height = self.height_i32;
            (*drm_frame).data[0] = drm_desc as *mut u8;
            (*drm_frame).buf[0] = ffi::av_buffer_create(
                drm_desc as *mut u8,
                std::mem::size_of::<ffi::AVDRMFrameDescriptor>() as _,
                Some(drm_desc_free),
                ptr::null_mut(),
                0,
            );
            if (*drm_frame).buf[0].is_null() {
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                drop(Box::from_raw(drm_desc));
                return Err("av_buffer_create (drm_desc) failed".into());
            }

            let vaapi_frame = ffi::av_frame_alloc();
            if vaapi_frame.is_null() {
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                return Err("av_frame_alloc (vaapi) failed".into());
            }
            (*vaapi_frame).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            (*vaapi_frame).width = self.width_i32;
            (*vaapi_frame).height = self.height_i32;
            (*vaapi_frame).hw_frames_ctx = ffi::av_buffer_ref(self.import_hw_frames_ref);

            let ret = ffi::av_hwframe_map(vaapi_frame, drm_frame, ffi::AV_HWFRAME_MAP_READ as i32);
            if ret < 0 {
                ffi::av_frame_free(&mut (vaapi_frame as *mut _));
                ffi::av_frame_free(&mut (drm_frame as *mut _));
                return Err(format!("av_hwframe_map failed: {} ({})", ret, av_err_string(ret)));
            }

            // Step 5: push XBGR VAAPI surface into scale_vaapi filter.
            (*vaapi_frame).pts = self.pts;
            let ret = ffi::av_buffersrc_add_frame_flags(
                self.buffersrc_ctx,
                vaapi_frame,
                ffi::AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            );
            ffi::av_frame_free(&mut (vaapi_frame as *mut _));
            ffi::av_frame_free(&mut (drm_frame as *mut _));
            if ret < 0 {
                return Err(format!("av_buffersrc_add_frame failed: {}", av_err_string(ret)));
            }

            // Step 6: pull NV12 VAAPI surface out of scale_vaapi filter.
            let nv12_vaapi = ffi::av_frame_alloc();
            if nv12_vaapi.is_null() {
                return Err("av_frame_alloc (nv12_vaapi) failed".into());
            }
            let ret = ffi::av_buffersink_get_frame(self.buffersink_ctx, nv12_vaapi);
            if ret < 0 {
                ffi::av_frame_free(&mut (nv12_vaapi as *mut _));
                return Err(format!("av_buffersink_get_frame failed: {}", av_err_string(ret)));
            }

            // Step 7: download NV12 from GPU → CPU.
            // av_hwframe_transfer_data allocates the dst frame's buffers automatically
            // when dst->format == AV_PIX_FMT_NONE (set via av_frame_alloc).
            let cpu_frame = ffi::av_frame_alloc();
            if cpu_frame.is_null() {
                ffi::av_frame_free(&mut (nv12_vaapi as *mut _));
                return Err("av_frame_alloc (cpu_frame) failed".into());
            }
            let ret = ffi::av_hwframe_transfer_data(cpu_frame, nv12_vaapi, 0);
            ffi::av_frame_free(&mut (nv12_vaapi as *mut _));
            if ret < 0 {
                ffi::av_frame_free(&mut (cpu_frame as *mut _));
                return Err(format!(
                    "av_hwframe_transfer_data failed: {} ({})",
                    ret,
                    av_err_string(ret)
                ));
            }

            // Step 8: send NV12 CPU frame to the software encoder (libx264).
            (*cpu_frame).pts = self.pts;
            self.apply_keyframe_request_raw(cpu_frame);
            self.pts += 1;
            let ret = ffi::avcodec_send_frame(self.encoder.as_mut_ptr(), cpu_frame);
            ffi::av_frame_free(&mut (cpu_frame as *mut _));
            if ret < 0 {
                return Err(format!("avcodec_send_frame failed: {} ({})", ret, av_err_string(ret)));
            }

            // Step 9: drain encoded packets.
            let mut packets = Vec::new();
            let mut packet = ffmpeg::Packet::empty();
            while self.encoder.receive_packet(&mut packet).is_ok() {
                if let Some(data) = packet.data() {
                    packets.push(data.to_vec());
                }
            }
            Ok(packets)
        }
    }

    /// Fallback: encode one frame of raw XBGR8888 pixel data from a mapped GBM buffer.
    ///
    /// `xbgr_data`: raw pixel bytes from bo.map(), layout [R,G,B,X] per pixel
    /// `stride`: row stride in bytes (may be > width*4)
    pub fn encode_xbgr(
        &mut self,
        xbgr_data: &[u8],
        stride: usize,
        drm_fourcc: u32,
    ) -> Result<(Vec<Vec<u8>>, CpuEncodeTiming), String> {
        let w = self.width;
        let h = self.height;
        let mut timing = CpuEncodeTiming::default();
        let source_format = ffmpeg::format::Pixel::from(drm_fourcc_to_av_pixfmt(drm_fourcc)?);
        if source_format != self.scaler_src_format {
            self.scaler = ffmpeg::software::scaling::Context::get(
                source_format,
                w,
                h,
                self.sw_pixel_format,
                w,
                h,
                ffmpeg::software::scaling::Flags::FAST_BILINEAR,
            )
            .map_err(|e| format!("swscale context recreation failed: {e}"))?;
            self.scaler_src_format = source_format;
            self.cpu_src_frame = ffmpeg::frame::Video::new(source_format, w, h);
            log::info!("CPU fallback input format changed to {source_format:?}");
        }

        // --- Step 1: Wrap raw packed RGB pixels into an AVFrame ---
        let stage = std::time::Instant::now();
        let frame_stride = self.cpu_src_frame.stride(0);
        {
            let dst = self.cpu_src_frame.data_mut(0);
            let row_bytes = (w as usize) * 4;
            for y in 0..h as usize {
                let src_start = y * stride;
                let dst_start = y * frame_stride;
                dst[dst_start..dst_start + row_bytes]
                    .copy_from_slice(&xbgr_data[src_start..src_start + row_bytes]);
            }
        }
        timing.copy_us = stage.elapsed().as_micros();

        // --- Step 2: swscale XBGR → target format ---
        let stage = std::time::Instant::now();
        self.scaler
            .run(&self.cpu_src_frame, &mut self.cpu_sw_frame)
            .map_err(|e| format!("swscale run failed: {}", e))?;
        timing.convert_us = stage.elapsed().as_micros();

        // --- Step 3: Encode ---
        let stage = std::time::Instant::now();
        if !self.hw_frames_ref.is_null() {
            // VAAPI path: Upload to GPU surface then encode
            let mut hw_frame = ffmpeg::frame::Video::empty();
            unsafe {
                let ret = ffi::av_hwframe_get_buffer(self.hw_frames_ref, hw_frame.as_mut_ptr(), 0);
                if ret < 0 {
                    return Err(format!(
                        "av_hwframe_get_buffer failed: {} ({})",
                        ret,
                        av_err_string(ret)
                    ));
                }

                let ret = ffi::av_hwframe_transfer_data(
                    hw_frame.as_mut_ptr(),
                    self.cpu_sw_frame.as_ptr(),
                    0,
                );
                if ret < 0 {
                    return Err(format!(
                        "av_hwframe_transfer_data failed: {} ({})",
                        ret,
                        av_err_string(ret)
                    ));
                }
            }

            hw_frame.set_pts(Some(self.pts));
            self.apply_keyframe_request_video(&mut hw_frame);
            self.pts += 1;

            self.encoder.send_frame(&hw_frame).map_err(|e| format!("send_frame failed: {}", e))?;
        } else {
            // Software path: Send CPU frame directly to encoder
            self.cpu_sw_frame.set_pts(Some(self.pts));
            if self.force_next_keyframe {
                self.cpu_sw_frame.set_kind(ffmpeg::picture::Type::I);
                self.force_next_keyframe = false;
            } else {
                // cpu_sw_frame is reused.  Leaving its previous I picture
                // type in place turns one recovery request into an IDR on
                // every subsequent frame, which exhausts the bitrate budget
                // and makes detailed scenes steadily degrade.
                self.cpu_sw_frame.set_kind(ffmpeg::picture::Type::None);
            }
            self.pts += 1;

            self.encoder
                .send_frame(&self.cpu_sw_frame)
                .map_err(|e| format!("send_frame failed: {}", e))?;
        }

        let mut packets = Vec::new();
        let mut packet = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                packets.push(data.to_vec());
            }
        }
        timing.codec_us = stage.elapsed().as_micros();

        Ok((packets, timing))
    }

    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        let _ = self.encoder.send_eof();
        let mut packets = Vec::new();
        let mut packet = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                packets.push(data.to_vec());
            }
        }
        packets
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            // avfilter_graph_free frees the entire graph including all filter
            // contexts inside it (src_ctx, scale_ctx, sink_ctx). We do NOT call
            // avfilter_free() on them individually — that would double-free.
            if !self.filter_graph.is_null() {
                ffi::avfilter_graph_free(&mut self.filter_graph);
            }
            if !self.import_hw_frames_ref.is_null() {
                ffi::av_buffer_unref(&mut self.import_hw_frames_ref);
            }
            if !self.hw_frames_ref.is_null() {
                ffi::av_buffer_unref(&mut self.hw_frames_ref);
            }
            // NOTE: hw_device_ref is NOT unref'd here — it's owned by the
            // caller's VaapiDeviceCtx and shared across encoder recreations.
            if !self.scratch_vaapi_frame.is_null() {
                ffi::av_frame_free(&mut self.scratch_vaapi_frame);
            }
            if !self.scratch_nv12_frame.is_null() {
                ffi::av_frame_free(&mut self.scratch_nv12_frame);
            }
        }
        log::info!("{} encoder destroyed", self.selected_encoder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_render_node_selection_is_sorted_and_filters_non_render_nodes() {
        let candidates = render_node_candidates(
            "auto",
            vec![
                "/dev/dri/card0".into(),
                "/dev/dri/renderD129".into(),
                "/dev/dri/renderD128".into(),
            ],
        );
        assert_eq!(candidates, ["/dev/dri/renderD128", "/dev/dri/renderD129"]);
    }

    #[test]
    fn explicit_render_node_preference_is_not_reordered_or_replaced() {
        let candidates = render_node_candidates(
            "/dev/dri/renderD129",
            vec!["/dev/dri/renderD128".into(), "/dev/dri/renderD129".into()],
        );
        assert_eq!(candidates, ["/dev/dri/renderD129"]);
    }

    fn annex_b_contains_idr(data: &[u8]) -> bool {
        data.windows(5).any(|window| {
            (window[..4] == [0, 0, 0, 1] && window[4] & 0x1f == 5)
                || (window[..3] == [0, 0, 1] && window[3] & 0x1f == 5)
        })
    }

    fn noisy_xbgr_frame(width: usize, height: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        let mut pixels = vec![0u8; width * height * 4];
        let (pixel_chunks, _) = pixels.as_chunks_mut::<4>();
        for pixel in pixel_chunks {
            // Deterministic high-frequency input. This makes a sticky I-frame
            // bug visible as a long run of IDRs rather than being hidden by a
            // static desktop frame that compresses nearly to nothing.
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            pixel[0] = (state >> 24) as u8;
            pixel[1] = (state >> 16) as u8;
            pixel[2] = (state >> 8) as u8;
            pixel[3] = 0;
        }
        pixels
    }

    #[test]
    fn ffmpeg_dimension_accepts_positive_signed_values() {
        assert_eq!(ffmpeg_dimension("width", 1920), Ok(1920));
        assert_eq!(ffmpeg_dimension("maximum", i32::MAX as u32), Ok(i32::MAX));
    }

    #[test]
    fn ffmpeg_dimension_rejects_zero_and_values_outside_ffmpeg_range() {
        assert!(ffmpeg_dimension("width", 0).is_err());
        assert!(ffmpeg_dimension("width", i32::MAX as u32 + 1).is_err());
    }

    #[test]
    fn dmabuf_plane_layout_accounts_for_the_plane_offset() {
        assert_eq!(
            dmabuf_plane_layout(1920, 1080, 7680, 4096).unwrap(),
            DmabufPlaneLayout { object_size: 8_298_496, offset: 4096, pitch: 7680 }
        );
    }

    #[test]
    fn dmabuf_plane_layout_rejects_short_or_overflowing_rows() {
        assert!(dmabuf_plane_layout(1920, 1080, 7679, 0).is_err());
        assert!(dmabuf_plane_layout(u32::MAX, 1, u32::MAX, 0).is_err());
    }

    #[test]
    fn noisy_cpu_frames_return_to_predictive_after_recovery_idr() {
        // This is the CPU fallback used when h264_vaapi cannot open. Reuse
        // the same frame object as production does, request an IDR mid-run,
        // then prove subsequent noisy frames are predictive rather than
        // inheriting AV_PICTURE_TYPE_I forever.
        crate::anchorapp::settings::init_settings();
        let width = 160u32;
        let height = 90u32;
        let mut encoder =
            VaapiEncoder::new(None, width, height, 2_000_000, 60, DRM_FORMAT_XBGR8888)
                .expect("software H.264 encoder must be available for the regression test");
        assert_eq!(encoder.selected_encoder, "libx264");

        let mut access_units = Vec::new();
        for frame_number in 0..24u32 {
            if frame_number == 4 {
                encoder.request_keyframe();
            }
            let pixels = noisy_xbgr_frame(width as usize, height as usize, frame_number + 1);
            let (packets, _) = encoder
                .encode_xbgr(&pixels, width as usize * 4, DRM_FORMAT_XBGR8888)
                .expect("encode deterministic noisy frame");
            access_units.extend(packets);
        }
        access_units.extend(encoder.flush());

        let keyframes = access_units
            .iter()
            .enumerate()
            .filter_map(|(index, access_unit)| annex_b_contains_idr(access_unit).then_some(index))
            .collect::<Vec<_>>();
        assert!(keyframes.len() >= 2, "initial and requested IDRs must be emitted");
        assert!(
            keyframes.len() <= 3,
            "one recovery request must not make every noisy frame an IDR: {keyframes:?}"
        );
        assert!(
            access_units.iter().skip(6).any(|access_unit| !annex_b_contains_idr(access_unit)),
            "frames after recovery must return to predictive encoding"
        );
    }

    #[test]
    fn max_frame_size_bounds_libx264_vbv_buffer() {
        // 120 KB per frame -> a 960 kbit buffer, since a frame cannot exceed
        // the VBV buffer it has to fit inside.
        let (maxrate, bufsize) = libx264_vbv_kbits(15_000_000, 0, 120_000, 12_000_000).unwrap();
        assert_eq!(maxrate, 15_000);
        assert_eq!(bufsize, 960);
    }

    #[test]
    fn max_frame_size_falls_back_to_the_bitrate_as_its_rate_cap() {
        // A frame cap with no explicit maxrate still needs a rate for the VBV
        // to drain at, or x264 gets a buffer it can never refill.
        let (maxrate, bufsize) = libx264_vbv_kbits(0, 0, 100_000, 12_000_000).unwrap();
        assert_eq!(maxrate, 12_000);
        assert_eq!(bufsize, 800);
    }

    #[test]
    fn max_frame_size_takes_precedence_over_a_looser_buffer() {
        // The regression this guards: bufsize_bits set to 60 Mbit reads as
        // "VBV configured" while permitting a 7.5 MB frame. An explicit frame
        // cap must win over it.
        let (_, bufsize) = libx264_vbv_kbits(60_000_000, 60_000_000, 120_000, 25_000_000).unwrap();
        assert_eq!(bufsize, 960);
    }

    #[test]
    fn explicit_buffer_is_used_when_no_frame_cap_is_set() {
        let (maxrate, bufsize) = libx264_vbv_kbits(15_000_000, 2_000_000, 0, 12_000_000).unwrap();
        assert_eq!(maxrate, 15_000);
        assert_eq!(bufsize, 2_000);
    }

    #[test]
    fn unconstrained_settings_leave_x264_defaults_alone() {
        assert!(libx264_vbv_kbits(0, 0, 0, 12_000_000).is_none());
    }
}
