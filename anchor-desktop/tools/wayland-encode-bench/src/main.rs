//! Measures Anchor's production DMA-BUF -> VAAPI -> H.264 path in isolation.
//!
//! It has no broadcaster, packetizer, socket, Android device, preview, or GUI.
//! `--reuse-frame` measures maximum sequential encoder throughput using one
//! immutable captured DMA-BUF. The default obtains a fresh screen frame before
//! each encode and records capture timing separately.

#![allow(dead_code)]

// The production encoder reads only these settings. Keeping them local makes
// each invocation self-describing and prevents a developer's settings.json
// from changing a benchmark result.
mod anchorapp {
    pub mod settings {
        use std::sync::OnceLock;

        static SETTINGS: OnceLock<AppSettings> = OnceLock::new();

        #[derive(Debug)]
        pub struct AppSettings {
            pub capture: CaptureSettings,
            pub encoding: EncodingSettings,
        }

        #[derive(Debug)]
        pub struct CaptureSettings {
            pub stats_log_interval_frames: u64,
        }

        #[derive(Debug, Clone)]
        pub struct EncodingSettings {
            pub gop_size: u32,
            pub max_b_frames: i32,
            pub rate_control: String,
            pub low_power: bool,
            pub x264_preset: String,
            pub x264_tune: String,
            pub hw_pool_size: u32,
            pub async_depth: u32,
            pub profile: String,
            pub level: String,
            pub scale_mode: String,
            pub coder: String,
            pub qp: u32,
            pub maxrate_bps: usize,
            pub bufsize_bits: usize,
            pub max_frame_size_bytes: usize,
            pub x264_threads: u32,
        }

        impl Default for AppSettings {
            fn default() -> Self {
                Self {
                    capture: CaptureSettings { stats_log_interval_frames: u64::MAX },
                    encoding: EncodingSettings {
                        gop_size: 120,
                        max_b_frames: 0,
                        rate_control: "CBR".into(),
                        low_power: true,
                        x264_preset: "ultrafast".into(),
                        x264_tune: "zerolatency".into(),
                        hw_pool_size: 20,
                        async_depth: 0,
                        profile: "auto".into(),
                        level: "auto".into(),
                        scale_mode: "hq".into(),
                        coder: "cabac".into(),
                        qp: 0,
                        maxrate_bps: 0,
                        bufsize_bits: 0,
                        max_frame_size_bytes: 0,
                        x264_threads: 4,
                    },
                }
            }
        }

        pub fn init_for_benchmark(settings: AppSettings) {
            SETTINGS.set(settings).expect("benchmark settings initialized twice");
        }

        pub fn settings() -> &'static AppSettings {
            SETTINGS.get().expect("benchmark settings were not initialized")
        }
    }
}

// The production encoder emits sampled metrics. The benchmark retains timing
// values directly, so it intentionally does not route duplicate log records.
mod metrics {
    pub(crate) fn emit(_payload: serde_json::Value) {}
}

mod anchorwayland;

use anchorapp::settings::{AppSettings, EncodingSettings};
use anchorwayland::capture_backend::{CaptureBackend, CapturedFrame, PixelFormat};
use anchorwayland::screencopy::WaylandScreencopyBackend;
use anchorwayland::vaapi_encoder::{DmabufEncodeTiming, VaapiDeviceCtx, VaapiEncoder};
use ffmpeg_next as ffmpeg;
use std::env;
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const DEFAULT_FRAMES: usize = 600;
const DEFAULT_WARMUP_FRAMES: usize = 60;
const DEFAULT_BITRATE_BPS: usize = 15_000_000;
const DEFAULT_FPS: u32 = 60;

#[derive(Debug)]
struct Config {
    frames: usize,
    warmup_frames: usize,
    bitrate_bps: usize,
    framerate: u32,
    async_depth: u32,
    reuse_frame: bool,
    quality_frames: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            frames: DEFAULT_FRAMES,
            warmup_frames: DEFAULT_WARMUP_FRAMES,
            bitrate_bps: DEFAULT_BITRATE_BPS,
            framerate: DEFAULT_FPS,
            async_depth: 0,
            reuse_frame: false,
            quality_frames: 0,
        }
    }
}

fn usage() -> &'static str {
    "Usage: cargo run --release -- [--frames N] [--warmup N] [--bitrate BPS] [--fps N] [--async-depth N] [--reuse-frame] [--quality-frames N]\n\
     \n\
     Default mode captures a fresh first-output DMA-BUF for every encode and reports capture\n\
     separately. --reuse-frame captures one DMA-BUF and encodes it repeatedly to expose the\n\
     sequential encoder/VPP ceiling without compositor pacing. --quality-frames N maps one\n\
     captured source frame, round-trips it through H.264, and reports RGB PSNR plus luma SSIM\n\
     for N decoded frames. It is deliberately off by default because mapping the DMA-BUF is\n\
     not part of Anchor's real-time encode path."
}

fn parse_positive<T>(name: &str, value: Option<String>) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + From<u8>,
{
    let value = value.ok_or_else(|| format!("{name} needs a value"))?;
    let parsed = value.parse::<T>().map_err(|_| format!("invalid {name} value: {value}"))?;
    if parsed <= T::from(0) {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => config.frames = parse_positive("--frames", args.next())?,
            "--warmup" => config.warmup_frames = parse_positive("--warmup", args.next())?,
            "--bitrate" => config.bitrate_bps = parse_positive("--bitrate", args.next())?,
            "--fps" => config.framerate = parse_positive("--fps", args.next())?,
            "--async-depth" => {
                config.async_depth = args
                    .next()
                    .ok_or("--async-depth needs a value")?
                    .parse()
                    .map_err(|_| "--async-depth must be a non-negative integer")?
            }
            "--reuse-frame" => config.reuse_frame = true,
            "--quality-frames" => {
                config.quality_frames = parse_positive("--quality-frames", args.next())?
            }
            "-h" | "--help" => return Err(usage().into()),
            _ => return Err(format!("unknown argument: {arg}\n\n{}", usage())),
        }
    }

    Ok(config)
}

#[derive(Debug)]
struct Summary {
    count: usize,
    mean: Duration,
    min: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
}

fn percentile_ns(sorted_ns: &[u128], percentile: f64) -> u128 {
    let index = ((sorted_ns.len() - 1) as f64 * percentile).round() as usize;
    sorted_ns[index]
}

fn summarize(samples: &[Duration]) -> Summary {
    assert!(!samples.is_empty());
    let mut sorted_ns = samples.iter().map(Duration::as_nanos).collect::<Vec<_>>();
    sorted_ns.sort_unstable();
    let total_ns: u128 = sorted_ns.iter().sum();

    Summary {
        count: sorted_ns.len(),
        mean: Duration::from_nanos((total_ns / sorted_ns.len() as u128) as u64),
        min: Duration::from_nanos(sorted_ns[0] as u64),
        p50: Duration::from_nanos(percentile_ns(&sorted_ns, 0.50) as u64),
        p95: Duration::from_nanos(percentile_ns(&sorted_ns, 0.95) as u64),
        p99: Duration::from_nanos(percentile_ns(&sorted_ns, 0.99) as u64),
        max: Duration::from_nanos(*sorted_ns.last().unwrap() as u64),
    }
}

fn format_ms(value: Duration) -> String {
    format!("{:.3} ms", value.as_secs_f64() * 1_000.0)
}

fn print_summary(name: &str, summary: &Summary) {
    println!("{name} ({} samples)", summary.count);
    println!(
        "  mean {}  min {}  p50 {}  p95 {}  p99 {}  max {}",
        format_ms(summary.mean),
        format_ms(summary.min),
        format_ms(summary.p50),
        format_ms(summary.p95),
        format_ms(summary.p99),
        format_ms(summary.max),
    );
}

fn print_frame_lag(samples: &[i64], configured_fps: u32, calls_without_output: usize) {
    if samples.is_empty() {
        println!(
            "encoder output delay: no packets drained during measured calls ({calls_without_output} calls retained by encoder)"
        );
        return;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mean = sorted.iter().sum::<i64>() as f64 / sorted.len() as f64;
    let p50 = sorted[((sorted.len() - 1) as f64 * 0.50).round() as usize];
    let p95 = sorted[((sorted.len() - 1) as f64 * 0.95).round() as usize];
    let p99 = sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize];

    println!(
        "encoder output delay (submitted input PTS - drained packet PTS; {} packet-producing calls, {calls_without_output} calls without output)",
        sorted.len()
    );
    println!(
        "  mean {:.2} frames  min {}  p50 {}  p95 {}  p99 {}  max {}  (one frame = {:.3} ms at configured {} fps)",
        mean,
        sorted[0],
        p50,
        p95,
        p99,
        sorted[sorted.len() - 1],
        1_000.0 / f64::from(configured_fps),
        configured_fps,
    );
}

fn as_duration(micros: u128) -> Duration {
    Duration::from_micros(micros.min(u64::MAX as u128) as u64)
}

fn encode_frame_data(
    encoder: &mut VaapiEncoder,
    frame: &CapturedFrame,
) -> Result<(Vec<u8>, DmabufEncodeTiming), String> {
    let dma_buf = frame.dma_buf.as_ref().ok_or("Capture did not return a DMA-BUF")?;
    encoder.encode_dmabuf(
        dma_buf.fd.as_raw_fd(),
        dma_buf.stride,
        dma_buf.offset,
        u64::from(dma_buf.modifier),
        frame.format.to_drm_fourcc(),
    )
}

fn encode_frame(
    encoder: &mut VaapiEncoder,
    frame: &CapturedFrame,
) -> Result<(usize, DmabufEncodeTiming), String> {
    let (encoded, timing) = encode_frame_data(encoder, frame)?;
    Ok((encoded.len(), timing))
}

#[derive(Debug)]
struct QualitySample {
    rgb_psnr_db: f64,
    luma_ssim: f64,
}

fn captured_frame_rgb24(frame: &CapturedFrame) -> Result<Vec<u8>, String> {
    let dma_buf = frame.dma_buf.as_ref().ok_or("Quality mode requires a DMA-BUF capture")?;
    let width = frame.width as usize;
    let height = frame.height as usize;
    let packed_row_bytes = width * 4;

    dma_buf
        .bo
        .map(0, 0, frame.width, frame.height, |mapped| {
            if (mapped.stride() as usize) < packed_row_bytes {
                return Err(format!(
                    "Mapped DMA-BUF stride {} is smaller than packed row {}",
                    mapped.stride(),
                    packed_row_bytes
                ));
            }

            let mut rgb = Vec::with_capacity(width * height * 3);
            for row in 0..height {
                let start = row * mapped.stride() as usize;
                let end = start + packed_row_bytes;
                let source = &mapped.buffer()[start..end];
                for pixel in source.chunks_exact(4) {
                    match frame.format {
                        // Little-endian memory is B, G, R, X/A.
                        PixelFormat::Xrgb8888 | PixelFormat::Argb8888 => {
                            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
                        }
                        // Little-endian memory is R, G, B, X/A.
                        PixelFormat::Xbgr8888 | PixelFormat::Abgr8888 => {
                            rgb.extend_from_slice(&[pixel[0], pixel[1], pixel[2]]);
                        }
                    }
                }
            }
            Ok(rgb)
        })
        .map_err(|e| format!("Failed to CPU-map captured DMA-BUF for quality reference: {e}"))?
}

fn video_frame_rgb24(
    frame: &ffmpeg::frame::Video,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    if frame.width() != width || frame.height() != height {
        return Err(format!(
            "Decoded frame size {}x{} does not match captured source {}x{}",
            frame.width(),
            frame.height(),
            width,
            height
        ));
    }

    let width = width as usize;
    let height = height as usize;
    let packed_row_bytes = width * 3;
    let stride = frame.stride(0);
    if stride < packed_row_bytes {
        return Err(format!(
            "Decoded RGB stride {stride} is smaller than packed row {packed_row_bytes}"
        ));
    }
    let data = frame.data(0);
    if data.len() < stride * height {
        return Err("Decoded RGB frame data is shorter than its stride and height".into());
    }

    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let start = row * stride;
        rgb.extend_from_slice(&data[start..start + packed_row_bytes]);
    }
    Ok(rgb)
}

fn luma(rgb: &[u8]) -> f64 {
    0.2126 * f64::from(rgb[0]) + 0.7152 * f64::from(rgb[1]) + 0.0722 * f64::from(rgb[2])
}

fn quality_sample(
    reference: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
) -> Result<QualitySample, String> {
    if reference.len() != candidate.len() || reference.len() != width * height * 3 {
        return Err("Reference and decoded RGB buffers must have matching packed dimensions".into());
    }

    let squared_error: u128 = reference
        .iter()
        .zip(candidate)
        .map(|(&left, &right)| {
            let difference = i32::from(left) - i32::from(right);
            u128::from((difference * difference) as u32)
        })
        .sum();
    let mse = squared_error as f64 / reference.len() as f64;
    let rgb_psnr_db =
        if mse == 0.0 { f64::INFINITY } else { 10.0 * ((255.0 * 255.0) / mse).log10() };

    // Standard SSIM constants for an 8-bit signal. Use non-overlapping 8x8
    // luma windows; the edge windows retain their smaller true dimensions.
    const C1: f64 = 6.5025;
    const C2: f64 = 58.5225;
    let mut ssim_total = 0.0;
    let mut windows = 0usize;
    for top in (0..height).step_by(8) {
        for left in (0..width).step_by(8) {
            let bottom = (top + 8).min(height);
            let right = (left + 8).min(width);
            let mut reference_luma = Vec::with_capacity((bottom - top) * (right - left));
            let mut candidate_luma = Vec::with_capacity(reference_luma.capacity());
            for y in top..bottom {
                for x in left..right {
                    let offset = (y * width + x) * 3;
                    reference_luma.push(luma(&reference[offset..offset + 3]));
                    candidate_luma.push(luma(&candidate[offset..offset + 3]));
                }
            }
            let count = reference_luma.len() as f64;
            let reference_mean = reference_luma.iter().sum::<f64>() / count;
            let candidate_mean = candidate_luma.iter().sum::<f64>() / count;
            let mut reference_variance = 0.0;
            let mut candidate_variance = 0.0;
            let mut covariance = 0.0;
            for (&reference_value, &candidate_value) in reference_luma.iter().zip(&candidate_luma) {
                let reference_delta = reference_value - reference_mean;
                let candidate_delta = candidate_value - candidate_mean;
                reference_variance += reference_delta * reference_delta;
                candidate_variance += candidate_delta * candidate_delta;
                covariance += reference_delta * candidate_delta;
            }
            reference_variance /= count;
            candidate_variance /= count;
            covariance /= count;
            ssim_total += ((2.0 * reference_mean * candidate_mean + C1) * (2.0 * covariance + C2))
                / ((reference_mean * reference_mean + candidate_mean * candidate_mean + C1)
                    * (reference_variance + candidate_variance + C2));
            windows += 1;
        }
    }

    Ok(QualitySample { rgb_psnr_db, luma_ssim: ssim_total / windows as f64 })
}

fn drain_decoded_frames(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut Option<ffmpeg::software::scaling::Context>,
    reference_rgb: &[u8],
    width: u32,
    height: u32,
    wanted_frames: usize,
    quality: &mut Vec<QualitySample>,
) -> Result<(), String> {
    let mut decoded = ffmpeg::frame::Video::empty();
    while quality.len() < wanted_frames && decoder.receive_frame(&mut decoded).is_ok() {
        let needs_new_scaler = scaler.as_ref().is_none_or(|current| {
            current.input().format != decoded.format()
                || current.input().width != decoded.width()
                || current.input().height != decoded.height()
        });
        if needs_new_scaler {
            *scaler = Some(
                ffmpeg::software::scaling::Context::get(
                    decoded.format(),
                    decoded.width(),
                    decoded.height(),
                    ffmpeg::format::Pixel::RGB24,
                    width,
                    height,
                    ffmpeg::software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| format!("Could not create decoded-frame RGB scaler: {e}"))?,
            );
        }
        let mut rgb = ffmpeg::frame::Video::empty();
        scaler
            .as_mut()
            .expect("RGB scaler initialized above")
            .run(&decoded, &mut rgb)
            .map_err(|e| format!("Could not convert decoded frame to RGB: {e}"))?;
        let candidate_rgb = video_frame_rgb24(&rgb, width, height)?;
        quality.push(quality_sample(
            reference_rgb,
            &candidate_rgb,
            width as usize,
            height as usize,
        )?);
    }
    Ok(())
}

fn run_quality_round_trip(
    device: &VaapiDeviceCtx,
    frame: &CapturedFrame,
    config: &Config,
) -> Result<(), String> {
    let reference_rgb = captured_frame_rgb24(frame)?;
    let mut quality_encoder = VaapiEncoder::new(
        Some(device),
        frame.width,
        frame.height,
        config.bitrate_bps,
        config.framerate,
        frame.format.to_drm_fourcc(),
    )?;
    quality_encoder.request_keyframe();

    // h264_vaapi has a measured one-frame output pipeline. Feed a few extra
    // copies of the immutable frame so the requested number of access units
    // is available, then flush any final buffered frame.
    let mut access_units = Vec::new();
    for _ in 0..(config.quality_frames + 4) {
        let (encoded, _) = encode_frame_data(&mut quality_encoder, frame)?;
        if !encoded.is_empty() {
            access_units.push(encoded);
        }
    }
    access_units.extend(quality_encoder.flush());

    let codec = ffmpeg::codec::decoder::find(ffmpeg::codec::Id::H264)
        .ok_or("FFmpeg H.264 decoder is unavailable")?;
    let decoder_context = ffmpeg::codec::context::Context::new_with_codec(codec);
    let mut decoder = decoder_context
        .decoder()
        .video()
        .map_err(|e| format!("Could not open H.264 decoder for quality mode: {e}"))?;
    let mut scaler = None;
    let mut quality = Vec::new();

    for access_unit in &access_units {
        let packet = ffmpeg::Packet::copy(access_unit);
        decoder
            .send_packet(&packet)
            .map_err(|e| format!("Could not send encoded H.264 to quality decoder: {e}"))?;
        drain_decoded_frames(
            &mut decoder,
            &mut scaler,
            &reference_rgb,
            frame.width,
            frame.height,
            config.quality_frames,
            &mut quality,
        )?;
        if quality.len() == config.quality_frames {
            break;
        }
    }
    if quality.len() < config.quality_frames {
        decoder.send_eof().map_err(|e| format!("Could not flush H.264 quality decoder: {e}"))?;
        drain_decoded_frames(
            &mut decoder,
            &mut scaler,
            &reference_rgb,
            frame.width,
            frame.height,
            config.quality_frames,
            &mut quality,
        )?;
    }
    if quality.len() != config.quality_frames {
        return Err(format!(
            "Quality mode decoded {} frame(s), expected {}. The encoder did not emit a decodable IDR sequence.",
            quality.len(),
            config.quality_frames
        ));
    }

    let mean_psnr =
        quality.iter().map(|sample| sample.rgb_psnr_db).sum::<f64>() / quality.len() as f64;
    let min_psnr = quality.iter().map(|sample| sample.rgb_psnr_db).fold(f64::INFINITY, f64::min);
    let mean_ssim =
        quality.iter().map(|sample| sample.luma_ssim).sum::<f64>() / quality.len() as f64;
    let min_ssim = quality.iter().map(|sample| sample.luma_ssim).fold(f64::INFINITY, f64::min);
    println!(
        "H.264 quality round trip ({} decoded frame(s), one captured source frame)",
        quality.len()
    );
    println!("  RGB PSNR: mean {:.2} dB; worst {:.2} dB", mean_psnr, min_psnr);
    println!("  luma SSIM: mean {:.5}; worst {:.5}", mean_ssim, min_ssim);
    println!(
        "  Note: this measures encoder/decoder distortion only. It excludes network loss, Android hardware decode, scaling, and physical display output."
    );
    Ok(())
}

fn benchmark_settings(config: &Config) -> AppSettings {
    let mut settings = AppSettings::default();
    settings.encoding = EncodingSettings { async_depth: config.async_depth, ..settings.encoding };
    settings
}

fn run(config: Config) -> Result<(), String> {
    anchorapp::settings::init_for_benchmark(benchmark_settings(&config));

    let mut capture = WaylandScreencopyBackend::try_new()?;
    capture.init()?;
    let output = capture.get_outputs().into_iter().next().ok_or("No Wayland outputs available")?;

    // The encoder's DMA-BUF filter graph depends on the compositor's actual
    // format, so capture one frame before construction and retain it for warmup.
    let first_frame = capture.capture_frame(0)?;
    let device = VaapiDeviceCtx::try_new("auto").ok_or(
        "VAAPI device initialization failed; this benchmark intentionally has no software fallback",
    )?;
    let mut encoder = VaapiEncoder::new(
        Some(&device),
        first_frame.width,
        first_frame.height,
        config.bitrate_bps,
        config.framerate,
        first_frame.format.to_drm_fourcc(),
    )?;
    if encoder.selected_encoder != "h264_vaapi" {
        return Err(format!(
            "VAAPI H.264 was not selected (got {}). This benchmark refuses software or non-VAAPI fallback paths.",
            encoder.selected_encoder
        ));
    }

    println!("Anchor Wayland VAAPI encode benchmark");
    println!("  output 0: {} ({})", output.name, output.description);
    println!(
        "  mode: {}x{}; input: {}",
        first_frame.width,
        first_frame.height,
        if config.reuse_frame {
            "one retained DMA-BUF"
        } else {
            "fresh screencopy frame per encode"
        }
    );
    println!(
        "  encoder: {}; bitrate: {} bps; configured fps: {}; async depth: {}",
        encoder.selected_encoder, config.bitrate_bps, config.framerate, config.async_depth
    );
    println!("  warmup frames: {}; measured frames: {}", config.warmup_frames, config.frames);

    let mut retained = Some(first_frame);
    for _ in 0..config.warmup_frames {
        let fresh = if config.reuse_frame { None } else { Some(capture.capture_frame(0)?) };
        let frame = fresh.as_ref().or(retained.as_ref()).expect("retained first frame missing");
        let _ = encode_frame(&mut encoder, frame)?;
    }

    let mut capture_times = Vec::new();
    let mut total_times = Vec::new();
    let mut dup_times = Vec::new();
    let mut descriptor_times = Vec::new();
    let mut map_times = Vec::new();
    let mut filter_in_times = Vec::new();
    let mut filter_out_times = Vec::new();
    let mut send_times = Vec::new();
    let mut receive_times = Vec::new();
    let mut output_lag_frames = Vec::new();
    let mut calls_without_output = 0usize;
    let mut encoded_bytes = Vec::new();
    let started = Instant::now();

    for _ in 0..config.frames {
        let fresh = if config.reuse_frame {
            None
        } else {
            let capture_started = Instant::now();
            let frame = capture.capture_frame(0)?;
            capture_times.push(capture_started.elapsed());
            Some(frame)
        };
        let frame = fresh.as_ref().or(retained.as_ref()).expect("retained first frame missing");
        let (size, timing) = encode_frame(&mut encoder, frame)?;
        encoded_bytes.push(size);
        total_times.push(as_duration(timing.total_us));
        dup_times.push(as_duration(timing.dup_fd_us));
        descriptor_times.push(as_duration(timing.create_descriptor_us));
        map_times.push(as_duration(timing.hwframe_map_us));
        filter_in_times.push(as_duration(timing.buffersrc_add_us));
        filter_out_times.push(as_duration(timing.buffersink_get_us));
        send_times.push(as_duration(timing.send_frame_us));
        receive_times.push(as_duration(timing.receive_packets_us));
        if let Some(output_pts) = timing.output_pts {
            output_lag_frames.push(timing.input_pts - output_pts);
        } else {
            calls_without_output += 1;
        }
    }

    let elapsed = started.elapsed();
    let flushed_bytes: usize = encoder.flush().iter().map(Vec::len).sum();
    let emitted_bytes: usize = encoded_bytes.iter().sum::<usize>() + flushed_bytes;
    let average_bytes = emitted_bytes as f64 / config.frames as f64;

    println!();
    if !capture_times.is_empty() {
        print_summary("capture before encode", &summarize(&capture_times));
    }
    print_summary("DMA-BUF encode total", &summarize(&total_times));
    print_frame_lag(&output_lag_frames, config.framerate, calls_without_output);
    print_summary("  duplicate DMA-BUF fd", &summarize(&dup_times));
    print_summary("  create DRM descriptor", &summarize(&descriptor_times));
    print_summary("  map DRM PRIME to VAAPI", &summarize(&map_times));
    print_summary("  filter input", &summarize(&filter_in_times));
    print_summary("  scale_vaapi output", &summarize(&filter_out_times));
    print_summary("  encoder submit", &summarize(&send_times));
    print_summary("  packet drain", &summarize(&receive_times));
    println!(
        "\neffective measured encode rate: {:.2} fps",
        config.frames as f64 / elapsed.as_secs_f64()
    );
    println!(
        "encoded output: {:.1} KiB/frame average; {} KiB total (including {} KiB flush)",
        average_bytes / 1024.0,
        emitted_bytes / 1024,
        flushed_bytes / 1024
    );
    if config.quality_frames > 0 {
        println!();
        run_quality_round_trip(
            &device,
            retained.as_ref().expect("retained first frame is available for quality mode"),
            &config,
        )?;
    }
    println!(
        "Interpretation: this measures the production encode path only. Fresh-input mode includes a separately reported screencopy wait; reuse mode is not a visual-quality workload unless --quality-frames is explicitly requested."
    );

    drop(retained.take());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::quality_sample;

    #[test]
    fn quality_of_identical_rgb_is_lossless() {
        let image = vec![16, 32, 48, 64, 80, 96, 128, 144, 160, 192, 208, 224];
        let quality = quality_sample(&image, &image, 2, 2).expect("identical image is valid");
        assert!(quality.rgb_psnr_db.is_infinite());
        assert!((quality.luma_ssim - 1.0).abs() < 1e-12);
    }

    #[test]
    fn quality_of_changed_rgb_is_finite() {
        let reference = vec![0; 12];
        let candidate = vec![255; 12];
        let quality =
            quality_sample(&reference, &candidate, 2, 2).expect("matching images are valid");
        assert!(quality.rgb_psnr_db.is_finite());
        assert!(quality.luma_ssim < 1.0);
    }
}

fn main() -> ExitCode {
    match parse_args().and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) if message == usage() => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("wayland-encode-bench: {message}");
            ExitCode::FAILURE
        }
    }
}
