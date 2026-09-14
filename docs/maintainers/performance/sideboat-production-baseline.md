# Sideboat encoder baseline

This offline baseline tests Anchor's production H.264 encoder and ANFR framing
against a repeatable 1920×1080, 60 FPS scene.

It has two explicit modes:

- `software` (default): requires `libx264`; portable CPU fallback.
- `vaapi`: requires `h264_vaapi`; it fails if VA-API is unavailable or a
  different encoder is selected.

Both modes generate CPU RGB frames. VA-API mode uploads those frames to a
VA-API surface. Neither mode tests Wayland capture or runtime DMA-BUF import.
Use `anchor-desktop/tools/wayland-encode-bench` for the Wayland → DMA-BUF →
VA-API path. This baseline does not test QUIC, Wi-Fi, Android, or real network
performance.

## Requirements

Use the desktop development shell, then return to the repository root:

```sh
cd anchor-desktop
devenv shell
cd ..
```

The baseline also requires `ffmpeg`. `ffplay` is needed only for `--headed`.
The `vaapi` mode additionally needs a usable DRM render device, a VA-API
driver, and FFmpeg support for `h264_vaapi`.

## Run one baseline

```sh
scripts/run-sideboat-production-baseline.sh /tmp/sideboat-baseline \
  --duration 4 \
  --bitrate-mbps 15 \
  --gop 120 \
  --max-frame-kb 120
```

That command records a software (`libx264`) baseline. To require the hardware
encoder, run this on a host with a working DRM render device and VA-API H.264:

```sh
scripts/run-sideboat-production-baseline.sh /tmp/sideboat-vaapi \
  --encoder vaapi \
  --vaapi-device auto \
  --duration 4
```

The defaults are four seconds, 15 Mbps, GOP 120, `ultrafast`, and a 120 KiB
maximum frame size. Override them when testing a tuning change:

```text
--duration SECONDS
--bitrate-mbps MBPS
--gop FRAMES
--preset ultrafast|superfast|veryfast
--max-frame-kb KIB
--encoder software|vaapi
--vaapi-device auto|PATH
```

Add `--headed` to open the visual comparison in `ffplay` after the run:

```sh
scripts/run-sideboat-production-baseline.sh /tmp/sideboat-baseline --headed
```

The command builds and runs the `anchor-desktop` example. It does not read or
write application settings.

## Compare profiles

Run the fixed candidate profiles against the same generated scene:

```sh
scripts/run-sideboat-production-matrix.sh /tmp/sideboat-matrix --duration 4
```

The matrix contains `constrained`, `balanced`, and `high-detail` profiles. It
uses software mode unless passed `--encoder vaapi`; VA-API mode refuses a
fallback. It compares encoded output and framing only; it is not a network
benchmark.

Run the same matrix against VA-API hardware with:

```sh
scripts/run-sideboat-production-matrix.sh /tmp/sideboat-vaapi-matrix \
  --duration 4 \
  --encoder vaapi
```

## Read the results

Each run directory contains:

- `report.json`: selected encoder, input path, settings, frame counts, IDR
  count, output size, and metrics;
- `frame-metrics.csv`: per-frame PSNR, MAE, changed-pixel ratio, and frame count;
- `side-by-side-and-difference.mp4`: source, decoded output, and amplified luma difference;
- `source-lossless.mkv`: lossless generated source;
- `decoded-lossless.mkv`: decoded H.264 output;
- `encoded.h264`: H.264 emitted by the production encoder;
- `decoded.rgb0`: raw decoded pixels.

Check these first:

1. `encoder` must match the requested mode (`libx264` or `h264_vaapi`).
2. `decoded_frames` must equal `source_frames`.
3. `min_psnr_db` and `mean_psnr_db` must not regress against the comparison run.
4. `max_access_unit_bytes` must stay within the configured frame cap.
5. `anfr_fragments` must be non-zero for a run that produced encoded data.
6. Inspect the visual difference video for blocking, missing frames, or scene-change artifacts.

The harness sends encoded access units through the production ANFR and
reliable-stream framing round trip. A clean result establishes an encoder and
framing baseline. It cannot identify physical network stutter; that requires
an end-to-end desktop-to-Android test with timestamps.

## Record a change

Keep the original run directory. Run the same duration and profile before and
after a change, then compare `report.json`, `frame-metrics.csv`, and the visual
difference video. Record the commit, host, FFmpeg version, profile, and any
metric change with the results.
