# Wayland encode benchmark

This tool measures Anchor's Wayland screen-capture and hardware H.264 encode
path:

```text
Wayland screencopy -> DMA-BUF -> VA-API -> h264_vaapi
```

It uses the capture and encoder modules from `anchor-desktop`. It does not
start the Anchor app, a network connection, a broadcaster, or an Android
client.

## Requirements

Run it inside the Wayland session that you want to measure. The session must
provide `zwlr_screencopy_manager_v1` and `zwp_linux_dmabuf_v1`. The machine
also needs a working DRM render device, VA-API H.264 encoding, and the FFmpeg
development libraries used by this crate.

The benchmark has no software-encoding fallback. It exits if VA-API cannot be
initialized or the capture does not provide a DMA-BUF.

## Run

Enter the desktop development shell, then run from the repository root:

```bash
cd anchor-desktop
devenv shell
cd ..
cargo run --release --manifest-path anchor-desktop/tools/wayland-encode-bench/Cargo.toml
```

The default runs 60 warm-up frames followed by 600 measured frames at 60 FPS
and 15,000,000 bits per second. To change these values:

```bash
cargo run --release --manifest-path anchor-desktop/tools/wayland-encode-bench/Cargo.toml -- \
  --frames 1200 --warmup 120 --fps 60 --bitrate 15000000
```

The tool measures the first available Wayland output. It prints capture time
(in normal mode), total encode time, encode sub-stages, output delay, encoded
size, and effective measured FPS. Percentiles are reported for each timing.

## Options

```text
--frames N          Measured frames (default: 600)
--warmup N          Warm-up frames (default: 60)
--bitrate BPS       Target bitrate (default: 15000000)
--fps N             Configured frame rate (default: 60)
--async-depth N     VA-API encoder async depth (default: 0)
--reuse-frame      Encode one captured DMA-BUF repeatedly
--quality-frames N Decode N frames and report PSNR and SSIM
```

Use `--help` to print the same list.

Normal mode captures a new screencopy frame before each measured encode. Use
this mode to include compositor capture pacing in the results; capture time is
reported separately from encode time.

The benchmark verifies that the selected encoder is exactly `h264_vaapi`. It
does not substitute `libx264`, NVENC, or AMF when VA-API initialization or
DMA-BUF import fails.

`--reuse-frame` captures one frame and submits it repeatedly. Use it to measure
the sequential DMA-BUF/VA-API throughput ceiling without compositor pacing. It
is not a realistic screen workload, so do not use its encoded size or quality
to represent normal desktop content.

## Quality measurement

`--quality-frames N` maps one captured DMA-BUF for a reference image, encodes
the same image repeatedly, decodes the H.264 output with FFmpeg, and reports:

- RGB PSNR in decibels;
- luma SSIM over 8x8 windows.

Quality mode is disabled by default because CPU-mapping a DMA-BUF is not part
of Anchor's live encode path. It measures encoder and decoder distortion only.
It does not measure network loss, Android decoding, scaling, compositor output,
or the physical display.

Example:

```bash
cargo run --release --manifest-path anchor-desktop/tools/wayland-encode-bench/Cargo.toml -- \
  --frames 300 --warmup 60 --quality-frames 5
```

## Interpreting results

Compare runs made with the same output, compositor, display mode, driver,
FFmpeg build, bitrate, frame rate, and encoder settings. Record the commit and
hardware with each result. This is a local pipeline benchmark, not an end-to-
end Anchor performance test.
