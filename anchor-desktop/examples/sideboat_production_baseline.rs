//! Offline visual baseline for the Sideboat software and VA-API encoder paths.
//!
//! This example calls `VaapiEncoder::encode_xbgr` directly. Software mode
//! exercises the CPU fallback. VA-API mode exercises
//! CPU RGB conversion, upload to a VA-API surface, and `h264_vaapi` encoding.
//! It does not open Wayland or QUIC sockets, so it cannot measure runtime
//! DMA-BUF import.
//!
//! The source is generated in memory, streamed to a lossless FFV1 reference
//! file, and fed to the encoder one frame at a time. The encoded stream is
//! decoded back to raw RGB and compared with the generator's source pixels.
//! The output directory contains source/decoded videos, a side-by-side video
//! with an amplified difference view, raw H.264, CSV metrics, and JSON.

use anchor::anchorapp::settings::EncodingSettings;
use anchor::anchorwayland::vaapi_encoder::{VaapiDeviceCtx, VaapiEncoder};
use anchor_sdk::video_frame::{self, FLAG_KEYFRAME, FRAME_KIND_SCREEN, FrameHeader};
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;
const DEFAULT_DURATION_SECONDS: u32 = 4;
const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
const BYTES_PER_PIXEL: usize = 4;

#[derive(Clone, Debug)]
struct Options {
    output: PathBuf,
    duration_seconds: u32,
    bitrate_bps: usize,
    gop: u32,
    preset: &'static str,
    max_frame_size_bytes: usize,
    encoder_mode: EncoderMode,
    vaapi_device: String,
    headed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncoderMode {
    Software,
    Vaapi,
}

impl EncoderMode {
    fn parse(value: &str) -> Self {
        match value {
            "software" => Self::Software,
            "vaapi" => Self::Vaapi,
            other => panic!("unsupported --encoder {other:?}; use software or vaapi"),
        }
    }

    fn expected_encoder(self) -> &'static str {
        match self {
            Self::Software => "libx264",
            Self::Vaapi => "h264_vaapi",
        }
    }

    fn input_path(self) -> &'static str {
        match self {
            Self::Software => "generated-rgb0-cpu",
            Self::Vaapi => "generated-rgb0-cpu-upload-to-vaapi",
        }
    }
}

#[derive(Debug, Default)]
struct Metrics {
    source_frames: usize,
    decoded_frames: usize,
    mean_psnr_db: f64,
    min_psnr_db: f64,
    mean_mae: f64,
    max_mae: u64,
    mean_changed_ratio: f64,
    max_changed_ratio: f64,
    anfr_fragments: usize,
}

struct SourceWriter {
    child: Child,
    stdin: ChildStdin,
}

fn main() {
    let options = parse_args();
    fs::create_dir_all(&options.output).expect("create output directory");
    require_command("ffmpeg");
    if options.headed {
        require_command("ffplay");
    }

    let source_video = options.output.join("source-lossless.mkv");
    let encoded_video = options.output.join("encoded.h264");
    let decoded_video = options.output.join("decoded-lossless.mkv");
    let decoded_raw = options.output.join("decoded.rgb0");
    let visual = options.output.join("side-by-side-and-difference.mp4");
    let metrics_csv = options.output.join("frame-metrics.csv");
    let report_json = options.output.join("report.json");

    let encoder_settings = EncodingSettings {
        bitrate_bps: options.bitrate_bps,
        fps: FPS,
        gop_size: options.gop,
        x264_preset: options.preset.to_string(),
        max_frame_size_bytes: options.max_frame_size_bytes,
        ..EncodingSettings::default()
    };

    let vaapi_device = match options.encoder_mode {
        EncoderMode::Software => None,
        EncoderMode::Vaapi => Some(
            VaapiDeviceCtx::try_new(&options.vaapi_device).unwrap_or_else(|| {
                panic!(
                    "VA-API device {} is unavailable or cannot initialize; the vaapi baseline never falls back to software",
                    options.vaapi_device
                )
            }),
        ),
    };
    let mut encoder = VaapiEncoder::new_with_settings(
        vaapi_device.as_ref(),
        WIDTH,
        HEIGHT,
        options.bitrate_bps,
        FPS,
        DRM_FORMAT_XBGR8888,
        &encoder_settings,
    )
    .unwrap_or_else(|error| panic!("requested encoder could not start: {error}"));
    assert_eq!(
        encoder.selected_encoder,
        options.encoder_mode.expected_encoder(),
        "requested {:?} baseline but selected {}",
        options.encoder_mode,
        encoder.selected_encoder,
    );

    let mut source = start_source_writer(&source_video, options.duration_seconds);
    let mut encoded = File::create(&encoded_video).expect("create encoded output");
    let frame_count = options.duration_seconds as usize * FPS as usize;
    let mut encoded_bytes = 0usize;
    let mut keyframes = 0usize;
    let mut max_access_unit_bytes = 0usize;
    let mut anfr_fragments = 0usize;
    let mut frame = vec![0u8; WIDTH as usize * HEIGHT as usize * BYTES_PER_PIXEL];

    for index in 0..frame_count {
        generate_frame(&mut frame, index as u32);
        source.stdin.write_all(&frame).expect("write generated source frame");
        let (packets, timing) = encoder
            .encode_xbgr(&frame, WIDTH as usize * BYTES_PER_PIXEL, DRM_FORMAT_XBGR8888)
            .unwrap_or_else(|error| panic!("encode frame {index}: {error}"));
        let access_unit = packets.concat();
        let access_unit_bytes = access_unit.len();
        let keyframe = contains_idr(&access_unit);
        keyframes += usize::from(keyframe);
        max_access_unit_bytes = max_access_unit_bytes.max(access_unit_bytes);
        if !access_unit.is_empty() {
            let (reassembled, fragments) =
                clean_stream_round_trip(index as u64, keyframe, &access_unit);
            assert_eq!(
                reassembled, access_unit,
                "ANFR stream framing changed encoded access unit {index}"
            );
            anfr_fragments += fragments;
            encoded.write_all(&reassembled).expect("write encoded packet");
            encoded_bytes += reassembled.len();
        }
        if index == 0 || index.is_multiple_of(FPS as usize) {
            eprintln!(
                "frame={index} encoded_bytes={access_unit_bytes} keyframe={keyframe} copy_us={} convert_us={} codec_us={}",
                timing.copy_us, timing.convert_us, timing.codec_us
            );
        }
    }
    for packet in encoder.flush() {
        encoded.write_all(&packet).expect("write flushed packet");
        encoded_bytes += packet.len();
    }
    drop(encoded);
    source.stdin.flush().expect("flush source encoder");
    drop(source.stdin);
    let source_status = source
        .child
        .try_wait()
        .expect("wait for source encoder")
        .unwrap_or_else(|| source.child.wait().expect("wait for source encoder"));
    assert!(source_status.success(), "FFV1 source encoder failed: {source_status}");

    decode_video(&encoded_video, &decoded_video);
    decode_raw(&encoded_video, &decoded_raw);
    let mut metrics = compare_decoded(&decoded_raw, frame_count, &mut frame);
    metrics.anfr_fragments = anfr_fragments;
    write_metrics(&metrics_csv, &metrics, frame_count);
    write_report(
        &report_json,
        &options,
        &metrics,
        ReportArtifacts {
            encoder: &encoder.selected_encoder,
            input_path: options.encoder_mode.input_path(),
            encoded_bytes,
            keyframes,
            max_access_unit_bytes,
            source: &source_video,
            decoded: &decoded_video,
            visual: &visual,
        },
    );
    make_visual(&source_video, &decoded_video, &visual);

    println!("Sideboat encoder baseline complete");
    println!(
        "encoder={} mode={:?} input={} scene=cyberspace frames={}/{}",
        encoder.selected_encoder,
        options.encoder_mode,
        options.encoder_mode.input_path(),
        metrics.decoded_frames,
        frame_count,
    );
    println!(
        "mean_psnr_db={:.2} min_psnr_db={:.2} mean_mae={:.2} max_changed_ratio={:.4}",
        metrics.mean_psnr_db, metrics.min_psnr_db, metrics.mean_mae, metrics.max_changed_ratio
    );
    println!("report={}", report_json.display());
    println!("visual={}", visual.display());

    if options.headed {
        let status = Command::new("ffplay")
            .args(["-autoexit", "-window_title", "Sideboat source / decoded / difference"])
            .arg(&visual)
            .status()
            .expect("launch ffplay");
        assert!(status.success(), "ffplay failed: {status}");
    }
}

fn parse_args() -> Options {
    let arguments: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1))
    };
    let preset = match value("--preset").map(String::as_str).unwrap_or("ultrafast") {
        "ultrafast" => "ultrafast",
        "superfast" => "superfast",
        "veryfast" => "veryfast",
        other => panic!("unsupported preset {other:?}; use ultrafast, superfast, or veryfast"),
    };
    Options {
        output: value("--output")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/anchor-sideboat-production-baseline")),
        duration_seconds: value("--duration")
            .map(|value| value.parse().expect("--duration must be an integer"))
            .unwrap_or(DEFAULT_DURATION_SECONDS),
        bitrate_bps: value("--bitrate-mbps")
            .map(|value| {
                value.parse::<usize>().expect("--bitrate-mbps must be an integer") * 1_000_000
            })
            .unwrap_or(15_000_000),
        gop: value("--gop")
            .map(|value| value.parse().expect("--gop must be an integer"))
            .unwrap_or(120),
        preset,
        max_frame_size_bytes: value("--max-frame-kb")
            .map(|value| value.parse::<usize>().expect("--max-frame-kb must be an integer") * 1024)
            .unwrap_or(0),
        encoder_mode: EncoderMode::parse(
            value("--encoder").map(String::as_str).unwrap_or("software"),
        ),
        vaapi_device: value("--vaapi-device").cloned().unwrap_or_else(|| "auto".into()),
        headed: arguments.iter().any(|argument| argument == "--headed"),
    }
}

fn generate_frame(frame: &mut [u8], index: u32) {
    let t = index as usize;
    let phase = t.wrapping_mul(13);
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let offset = (y * WIDTH as usize + x) * BYTES_PER_PIXEL;
            let grid: u8 = if (x / 32 + y / 32 + t / 3).is_multiple_of(2) { 34 } else { 10 };
            let moving_x = (phase + y * 3) % WIDTH as usize;
            let block = (x + WIDTH as usize - moving_x) % (WIDTH as usize) < 360
                && (y + t * 5) % (HEIGHT as usize) < 220;
            let transition = ((t / 45) % 2 == 1) && x > WIDTH as usize / 2;
            let noise = ((x.wrapping_mul(1_103_515_245)
                ^ y.wrapping_mul(2_654_435_761)
                ^ t.wrapping_mul(97))
                >> 13) as u8
                & 15;
            frame[offset] =
                grid.saturating_add(noise / 2).saturating_add(if block { 40 } else { 0 });
            frame[offset + 1] =
                grid.saturating_add(noise).saturating_add(if transition { 55 } else { 0 });
            frame[offset + 2] =
                grid.saturating_add(noise / 3).saturating_add(if block { 90 } else { 0 });
            frame[offset + 3] = 0;
        }
    }
    // High-contrast moving bars approximate a large compositor scene change.
    let bar_x = (t * 29) % WIDTH as usize;
    for y in 80..HEIGHT as usize - 80 {
        for x in bar_x..(bar_x + 8).min(WIDTH as usize) {
            let offset = (y * WIDTH as usize + x) * BYTES_PER_PIXEL;
            frame[offset..offset + 4].copy_from_slice(&[20, 240, 220, 0]);
        }
    }
}

fn start_source_writer(path: &Path, duration_seconds: u32) -> SourceWriter {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb0",
        "-s",
        "1920x1080",
        "-r",
        "60",
        "-i",
        "pipe:0",
        "-frames:v",
    ]);
    command.arg((duration_seconds * FPS).to_string());
    command.args(["-c:v", "ffv1", "-level", "3", "-pix_fmt", "yuv444p"]);
    command.arg(path);
    let mut child = command.stdin(Stdio::piped()).spawn().expect("start FFV1 source encoder");
    let stdin = child.stdin.take().expect("open FFV1 source stdin");
    SourceWriter { child, stdin }
}

fn compare_decoded(path: &Path, source_frames: usize, frame: &mut [u8]) -> Metrics {
    let mut decoded = File::open(path).expect("open decoded raw video");
    let frame_bytes = frame.len();
    let mut decoded_frame = vec![0u8; frame_bytes];
    let mut metrics = Metrics { source_frames, min_psnr_db: f64::INFINITY, ..Metrics::default() };
    loop {
        let mut read = 0;
        while read < frame_bytes {
            let count = decoded.read(&mut decoded_frame[read..]).expect("read decoded frame");
            if count == 0 {
                break;
            }
            read += count;
        }
        if read == 0 {
            break;
        }
        if read != frame_bytes {
            eprintln!("warning: truncated decoded frame: {read}/{frame_bytes} bytes");
            break;
        }
        generate_frame(frame, metrics.decoded_frames as u32);
        let mut sum_squared = 0f64;
        let mut sum_abs = 0u64;
        let mut changed = 0usize;
        let mut samples = 0usize;
        let (source_pixels, _) = frame.as_chunks::<4>();
        let (decoded_pixels, _) = decoded_frame.as_chunks::<4>();
        for (source, decoded) in source_pixels.iter().zip(decoded_pixels.iter()) {
            for channel in 0..3 {
                let difference = i32::from(source[channel]) - i32::from(decoded[channel]);
                let absolute = difference.unsigned_abs() as u64;
                sum_squared += (absolute * absolute) as f64;
                sum_abs += absolute;
                changed += usize::from(absolute > 8);
                samples += 1;
            }
        }
        let mse = sum_squared / samples as f64;
        let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() };
        let mae = sum_abs as f64 / samples as f64;
        let changed_ratio = changed as f64 / (WIDTH as usize * HEIGHT as usize * 3) as f64;
        metrics.mean_psnr_db += psnr;
        metrics.min_psnr_db = metrics.min_psnr_db.min(psnr);
        metrics.mean_mae += mae;
        metrics.max_mae = metrics.max_mae.max(sum_abs / samples as u64);
        metrics.mean_changed_ratio += changed_ratio;
        metrics.max_changed_ratio = metrics.max_changed_ratio.max(changed_ratio);
        metrics.decoded_frames += 1;
    }
    if metrics.decoded_frames > 0 {
        let count = metrics.decoded_frames as f64;
        metrics.mean_psnr_db /= count;
        metrics.mean_mae /= count;
        metrics.mean_changed_ratio /= count;
    } else {
        metrics.min_psnr_db = 0.0;
    }
    metrics
}

fn decode_video(input: &Path, output: &Path) {
    run_ffmpeg(
        [
            "-f",
            "h264",
            "-i",
            input.to_str().unwrap(),
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv444p",
            output.to_str().unwrap(),
        ],
        "decode visual video",
    );
}

fn decode_raw(input: &Path, output: &Path) {
    run_ffmpeg(
        [
            "-f",
            "h264",
            "-i",
            input.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb0",
            output.to_str().unwrap(),
        ],
        "decode raw video",
    );
}

fn make_visual(source: &Path, decoded: &Path, output: &Path) {
    run_ffmpeg(
        [
            "-i",
            source.to_str().unwrap(),
            "-i",
            decoded.to_str().unwrap(),
            "-filter_complex",
            "[0:v]split=2[src_top][src_diff];[1:v]split=2[decoded_top][decoded_diff];[src_top][decoded_top]hstack=inputs=2[top];[src_diff]format=gray[src_luma];[decoded_diff]format=gray[decoded_luma];[src_luma][decoded_luma]blend=all_mode=difference,lut=y='min(val*24,255)',scale=3840:1080[bottom];[top][bottom]vstack=inputs=2,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            output.to_str().unwrap(),
        ],
        "make visual comparison",
    );
}

fn write_metrics(path: &Path, metrics: &Metrics, source_frames: usize) {
    let mut output = String::from(
        "source_frames,decoded_frames,anfr_fragments,mean_psnr_db,min_psnr_db,mean_mae,max_mae,mean_changed_ratio,max_changed_ratio\n",
    );
    let _ = writeln!(
        output,
        "{source_frames},{},{},{:.4},{:.4},{:.4},{},{:.8},{:.8}",
        metrics.decoded_frames,
        metrics.anfr_fragments,
        metrics.mean_psnr_db,
        metrics.min_psnr_db,
        metrics.mean_mae,
        metrics.max_mae,
        metrics.mean_changed_ratio,
        metrics.max_changed_ratio
    );
    fs::write(path, output).expect("write CSV metrics");
}

struct ReportArtifacts<'a> {
    encoder: &'a str,
    input_path: &'a str,
    encoded_bytes: usize,
    keyframes: usize,
    max_access_unit_bytes: usize,
    source: &'a Path,
    decoded: &'a Path,
    visual: &'a Path,
}

fn write_report(path: &Path, options: &Options, metrics: &Metrics, artifacts: ReportArtifacts<'_>) {
    let json = format!(
        "{{\n  \"encoder\": \"{}\",\n  \"input_path\": \"{}\",\n  \"source\": \"cyberspace\",\n  \"width\": {WIDTH},\n  \"height\": {HEIGHT},\n  \"fps\": {FPS},\n  \"duration_seconds\": {},\n  \"bitrate_bps\": {},\n  \"gop\": {},\n  \"preset\": \"{}\",\n  \"max_frame_size_bytes\": {},\n  \"source_frames\": {},\n  \"decoded_frames\": {},\n  \"encoded_bytes\": {},\n  \"keyframes\": {},\n  \"anfr_fragments\": {},\n  \"max_access_unit_bytes\": {},\n  \"mean_psnr_db\": {:.4},\n  \"min_psnr_db\": {:.4},\n  \"mean_mae\": {:.4},\n  \"max_mae\": {},\n  \"mean_changed_ratio\": {:.8},\n  \"max_changed_ratio\": {:.8},\n  \"source_video\": {:?},\n  \"decoded_video\": {:?},\n  \"visual\": {:?}\n}}\n",
        artifacts.encoder,
        artifacts.input_path,
        options.duration_seconds,
        options.bitrate_bps,
        options.gop,
        options.preset,
        options.max_frame_size_bytes,
        metrics.source_frames,
        metrics.decoded_frames,
        artifacts.encoded_bytes,
        artifacts.keyframes,
        metrics.anfr_fragments,
        artifacts.max_access_unit_bytes,
        metrics.mean_psnr_db,
        metrics.min_psnr_db,
        metrics.mean_mae,
        metrics.max_mae,
        metrics.mean_changed_ratio,
        metrics.max_changed_ratio,
        artifacts.source,
        artifacts.decoded,
        artifacts.visual
    );
    fs::write(path, json).expect("write JSON report");
}

fn contains_idr(packet: &[u8]) -> bool {
    packet.windows(5).any(|window| {
        (window[..4] == [0, 0, 0, 1] && window[4] & 0x1f == 5)
            || (window[..3] == [0, 0, 1] && window[3] & 0x1f == 5)
    })
}

/// Exercise the same ANFR fragment headers and reliable-stream length records
/// that Sideboat carries over QUIC. This clean baseline deliberately adds no
/// loss or reordering; impaired transport belongs in a separate experiment.
fn clean_stream_round_trip(sequence: u64, keyframe: bool, access_unit: &[u8]) -> (Vec<u8>, usize) {
    let flags = if keyframe { FLAG_KEYFRAME } else { 0 };
    let packets = video_frame::fragment_frame_with_flags(
        FRAME_KIND_SCREEN,
        flags,
        1,
        1,
        sequence,
        sequence,
        0,
        access_unit,
    )
    .expect("fragment production access unit");
    let mut stream = Vec::with_capacity(
        packets.len() * video_frame::STREAM_PACKET_LENGTH_BYTES + access_unit.len(),
    );
    for packet in &packets {
        stream.extend_from_slice(
            &video_frame::encode_stream_packet(packet).expect("prefix ANFR stream record"),
        );
    }

    let mut offset = 0usize;
    let mut reassembled = Vec::with_capacity(access_unit.len());
    let mut expected_fragment_index = 0u16;
    while offset < stream.len() {
        let prefix_end = offset + video_frame::STREAM_PACKET_LENGTH_BYTES;
        let length = u32::from_le_bytes(
            stream[offset..prefix_end].try_into().expect("four-byte stream record length"),
        ) as usize;
        let record_end = prefix_end + length;
        let (header, payload) =
            FrameHeader::decode(&stream[prefix_end..record_end]).expect("decode ANFR record");
        assert_eq!(header.kind, FRAME_KIND_SCREEN);
        assert_eq!(header.flags, flags);
        assert_eq!(header.sequence, sequence);
        assert_eq!(header.fragment_index, expected_fragment_index);
        assert_eq!(header.fragment_count as usize, packets.len());
        reassembled.extend_from_slice(payload);
        expected_fragment_index += 1;
        offset = record_end;
    }
    assert_eq!(expected_fragment_index as usize, packets.len());
    (reassembled, packets.len())
}

fn run_ffmpeg<const N: usize>(args: [&str; N], operation: &str) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap_or_else(|error| panic!("{operation}: could not run ffmpeg: {error}"));
    assert!(status.success(), "{operation}: ffmpeg exited with {status}");
}

fn require_command(command: &str) {
    let status = Command::new(command)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("{command} is required: {error}"));
    assert!(status.success(), "{command} is required");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_scene_is_deterministic_and_changes_over_time() {
        let mut first = vec![0u8; WIDTH as usize * HEIGHT as usize * BYTES_PER_PIXEL];
        let mut repeat = vec![0u8; first.len()];
        let mut second = vec![0u8; first.len()];
        generate_frame(&mut first, 17);
        generate_frame(&mut repeat, 17);
        generate_frame(&mut second, 18);
        assert_eq!(first, repeat);
        assert_ne!(first, second);
    }

    #[test]
    fn idr_detection_accepts_annex_b_four_byte_start_code() {
        assert!(contains_idr(&[0, 0, 0, 1, 5, 1]));
        assert!(contains_idr(&[0, 0, 1, 5, 1]));
        assert!(!contains_idr(&[0, 0, 0, 1, 1, 1]));
    }

    #[test]
    fn encoder_modes_name_the_required_encoder_and_input_path() {
        assert_eq!(EncoderMode::parse("software"), EncoderMode::Software);
        assert_eq!(EncoderMode::Software.expected_encoder(), "libx264");
        assert_eq!(EncoderMode::Software.input_path(), "generated-rgb0-cpu");
        assert_eq!(EncoderMode::parse("vaapi"), EncoderMode::Vaapi);
        assert_eq!(EncoderMode::Vaapi.expected_encoder(), "h264_vaapi");
        assert_eq!(EncoderMode::Vaapi.input_path(), "generated-rgb0-cpu-upload-to-vaapi");
    }

    #[test]
    fn anfr_stream_round_trip_preserves_a_multifragment_access_unit() {
        let input = (0..(video_frame::FRAME_PAYLOAD_BYTES * 3 + 19))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let (output, fragment_count) = clean_stream_round_trip(42, true, &input);
        assert_eq!(output, input);
        assert_eq!(fragment_count, 4);
    }
}
