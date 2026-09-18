# Wayland capture benchmark

This tool measures Anchor's Wayland screen-capture stage:

```text
wlr-screencopy request -> GBM DMA-BUF -> ready event
```

It uses the capture modules shipped by Anchor. It does not start the desktop
UI, encoder, network server, or Android client.

## Requirements

- A running Wayland session.
- A compositor that provides `zwlr_screencopy_manager_v1` and
  `zwp_linux_dmabuf_v1`.
- Rust and Cargo.
- The native Wayland, GBM, and DRM libraries required by the crate's Cargo
  dependencies.

Run the benchmark from the Wayland session you want to measure. Cargo uses the
current `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` values.

## Run

From this directory:

```bash
cargo run --release -- --frames 600 --warmup 60
```

Use a time limit instead of a frame count:

```bash
cargo run --release -- --seconds 10 --warmup 60
```

Useful options:

```text
--frames N       Measure N frames (default: 600)
--seconds S      Measure for S seconds; cannot be combined with --frames
--warmup N       Capture N frames before measuring (default: 60)
--damage-only    Wait for output damage with copy_with_damage
```

The default mode requests every compositor frame. `--damage-only` is useful
for idle-desktop tests, but it can wait indefinitely while the output remains
unchanged.

## Results

The output includes:

- capture-call latency from request to compositor completion;
- completion and compositor presentation cadence;
- effective capture rate;
- frame dimensions, stride, and pixel format;
- latency percentiles and slow capture calls.

The benchmark reuses one DMA-BUF and drops each frame after sampling its
metadata. This measures the sequential capture ceiling, not queued capture
throughput.

## Limitations

- It measures only the first compositor-announced output. Output index zero is
  not guaranteed to be the primary display.
- DMA-BUF avoids CPU readback, but the compositor may still copy into Anchor's
  linear GBM buffer.
- It does not measure encoding, transport, decoding, network loss, or display
  presentation latency.
- Results depend on the compositor, GPU driver, display refresh rate, and
  current desktop activity.
