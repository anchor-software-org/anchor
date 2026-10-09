//! H.264 → JPEG decoder for the remote-mirror path.
//!
//! The phone's MediaProjection producer sends Annex-B access units on the
//! same ANFR datagram flow the JPEG stills used; this decoder converts each
//! decoded frame to JPEG so the UI's `data:image/jpeg` path stays unchanged.
//! Follows the raw-FFI pattern from camera_receiver.rs.

use ffmpeg_next as ffmpeg;
use ffmpeg_next::ffi;

/// Lazily-opened H.264 software decoder. Single-threaded by contract — the
/// mirror reassembly thread owns it exclusively.
pub struct MirrorDecoder {
    ctx: *mut ffi::AVCodecContext,
    pkt: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    sws: *mut ffi::SwsContext,
    sws_signature: (i32, i32, i32),
    rgba: Vec<u8>,
    rgba_dims: (usize, usize),
}

// The decoder only ever runs on the mirror receive thread.
unsafe impl Send for MirrorDecoder {}

impl MirrorDecoder {
    pub fn new() -> Option<Self> {
        static FFMPEG_INIT: std::sync::Once = std::sync::Once::new();
        FFMPEG_INIT.call_once(|| {
            ffmpeg::init().expect("ffmpeg init failed");
        });
        unsafe {
            let codec = ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_H264);
            if codec.is_null() {
                log::error!("mirror: H.264 decoder not found in ffmpeg");
                return None;
            }
            let ctx = ffi::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return None;
            }
            // Screen mirroring tolerates artifacts better than stalls: show
            // partial frames after fragment loss rather than stalling the
            // reference chain until the next keyframe.
            (*ctx).flags |= ffi::AV_CODEC_FLAG_OUTPUT_CORRUPT as i32;
            (*ctx).flags2 |= ffi::AV_CODEC_FLAG2_SHOW_ALL as i32;
            (*ctx).err_recognition = 0;
            if ffi::avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0 {
                ffi::avcodec_free_context(&mut (ctx as *mut _));
                return None;
            }
            let pkt = ffi::av_packet_alloc();
            let frame = ffi::av_frame_alloc();
            if pkt.is_null() || frame.is_null() {
                if !pkt.is_null() {
                    ffi::av_packet_free(&mut (pkt as *mut _));
                }
                if !frame.is_null() {
                    ffi::av_frame_free(&mut (frame as *mut _));
                }
                ffi::avcodec_free_context(&mut (ctx as *mut _));
                return None;
            }
            Some(Self {
                ctx,
                pkt,
                frame,
                sws: std::ptr::null_mut(),
                sws_signature: (0, 0, 0),
                rgba: Vec::new(),
                rgba_dims: (0, 0),
            })
        }
    }

    /// Feed one reassembled Annex-B access unit. Returns a JPEG for each
    /// frame the decoder emits (usually 0 or 1 per AU).
    pub fn decode_to_jpeg(&mut self, access_unit: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let Ok(size) = i32::try_from(access_unit.len()) else {
            return out;
        };
        unsafe {
            (*self.pkt).data = access_unit.as_ptr().cast_mut();
            (*self.pkt).size = size;
            (*self.pkt).dts = ffi::AV_NOPTS_VALUE;
            (*self.pkt).pts = ffi::AV_NOPTS_VALUE;
            if ffi::avcodec_send_packet(self.ctx, self.pkt) < 0 {
                return out;
            }
            loop {
                let ret = ffi::avcodec_receive_frame(self.ctx, self.frame);
                if ret == ffi::AVERROR(libc::EAGAIN) || ret == ffi::AVERROR_EOF {
                    break;
                }
                if ret < 0 {
                    break;
                }
                let _ = ffi::av_frame_apply_cropping(self.frame, 0);
                if let Some(jpeg) = self.frame_to_jpeg() {
                    out.push(jpeg);
                }
                ffi::av_frame_unref(self.frame);
            }
        }
        out
    }

    unsafe fn frame_to_jpeg(&mut self) -> Option<Vec<u8>> {
        unsafe {
            let w = (*self.frame).width;
            let h = (*self.frame).height;
            let fmt = (*self.frame).format;
            if w <= 0 || h <= 0 {
                return None;
            }
            let sig = (w, h, fmt);
            if self.sws_signature != sig {
                // SWS_FAST_BILINEAR — same flag the camera receiver uses.
                const SWS_FLAGS: i32 = 1;
                self.sws = ffi::sws_getCachedContext(
                    self.sws,
                    w,
                    h,
                    std::mem::transmute::<i32, ffi::AVPixelFormat>(fmt),
                    w,
                    h,
                    ffi::AVPixelFormat::AV_PIX_FMT_RGBA,
                    SWS_FLAGS,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                if self.sws.is_null() {
                    log::warn!("mirror: sws_getCachedContext failed for {w}x{h} fmt={fmt}");
                    return None;
                }
                // Honor the stream's tagged colorspace so screen RGB stays
                // correct whether the encoder marks 601 or 709.
                let coeffs = ffi::sws_getCoefficients((*self.frame).colorspace as i32);
                ffi::sws_setColorspaceDetails(
                    self.sws,
                    coeffs,
                    0,
                    coeffs,
                    0,
                    0,
                    1 << 16,
                    1 << 16,
                );
                self.sws_signature = sig;
                self.rgba_dims = (w as usize, h as usize);
                self.rgba.resize(w as usize * h as usize * 4, 0);
            }
            let stride = w as usize * 4;
            let mut dst = [self.rgba.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()];
            let dst_stride = [stride as i32, 0, 0, 0, 0, 0, 0, 0];
            let scaled = ffi::sws_scale(
                self.sws,
                (*self.frame).data.as_ptr().cast(),
                (*self.frame).linesize.as_ptr(),
                0,
                h,
                dst.as_mut_ptr(),
                dst_stride.as_ptr(),
            );
            if scaled <= 0 {
                return None;
            }
            let (w_u, h_u) = self.rgba_dims;
            let image = image::RgbaImage::from_raw(w_u as u32, h_u as u32, self.rgba.clone())?;
            let mut jpeg = Vec::with_capacity(96 * 1024);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75)
                .encode_image(&image)
                .ok()?;
            Some(jpeg)
        }
    }
}

impl Drop for MirrorDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.sws.is_null() {
                ffi::sws_freeContext(self.sws);
            }
            ffi::av_packet_free(&mut (self.pkt as *mut _));
            ffi::av_frame_free(&mut (self.frame as *mut _));
            ffi::avcodec_free_context(&mut (self.ctx as *mut _));
        }
    }
}
