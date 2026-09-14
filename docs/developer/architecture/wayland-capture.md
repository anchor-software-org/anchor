# Wayland capture

This document describes the Linux desktop capture path in Anchor. It is a developer
reference, not a setup guide.

The implementation is in `anchor-desktop/src/anchorwayland/`:

- `screencopy.rs` implements the Wayland backend.
- `capture_backend.rs` defines the backend and frame types.
- `wayland_plugin.rs` runs capture, encoding, and stream state.
- `vaapi_encoder.rs` imports frames and encodes H.264.
- `wayland_objects.rs` handles Wayland protocol state and buffer allocation.

## Capture flow

```text
Wayland compositor
        │ wlr-screencopy
        ▼
Reusable DMA-BUF (normal path) or wl_shm buffer (fallback)
        │
        ▼
VaapiEncoder
        │ H.264 access units
        ▼
FrameBroadcaster → connected Android devices
```

When the Wayland plugin starts, it creates a `WaylandScreencopyBackend`. The backend
requires:

- a Wayland session;
- `zwlr_screencopy_manager_v1`;
- at least one output; and
- `zwp_linux_dmabuf_v1` during initialization.

It enumerates outputs and asks the compositor for the selected output's dimensions and
pixel format. It allocates a reusable linear GBM buffer, registers it as a Wayland
`wl_buffer`, and uses it for subsequent screencopy requests.

For each frame, the backend creates a screencopy frame and calls `copy`. The default
mode is `EveryFrame`; `OnlyOnDamage` is available to callers that explicitly select it.
The capture call blocks until the compositor sends `ready`. Output changes cause the
buffer to be recreated. Output add/remove events are checked periodically, and a
removed streamed output stops the stream cleanly.

The cursor is requested as part of the screencopy operation. Capture and encoding are
sequential, so the reusable buffer is not overwritten while an encoded frame uses it.

## Encoding paths

The encoder is created for the captured output's width, height, and format. It probes
encoders in this order:

1. `h264_vaapi`
2. `h264_nvenc`
3. `h264_amf`
4. `libx264`
5. `libvpx-vp9`
6. `libvpx`

The selected encoder and available hardware paths depend on the FFmpeg build and the
host drivers.

### VA-API H.264

When `h264_vaapi` and the VA-API DMA-BUF pipeline are available, Anchor uses this path:

```text
GBM DMA-BUF
  → DRM PRIME descriptor
  → VA-API surface (av_hwframe_map)
  → scale_vaapi to NV12
  → h264_vaapi
```

Pixels are not mapped into CPU memory on this path. The DMA-BUF file descriptor is
duplicated for the FFmpeg descriptor and released when the descriptor is freed.

If DMA-BUF import fails, the encoder disables that path for the session and falls back
to the CPU path. It does not retry a known driver incompatibility for every frame.

### Software encoders

For a software encoder, Anchor first uses the VA-API VideoProc path when available:

```text
GBM DMA-BUF
  → VA-API import
  → scale_vaapi to NV12
  → download to CPU
  → software encoder
```

If that path is unavailable or fails, Anchor maps the GBM buffer, copies each row into a
tightly packed FFmpeg frame, and converts the packed RGB format with `swscale`. The
software encoder then receives NV12 or YUV420P, depending on the encoder.

If the active software encoder has no usable DMA-BUF VideoProc path, the backend can
switch to a reusable `wl_shm` buffer. The compositor writes directly into the mapped
shared-memory buffer, and the CPU encoding path reads it. The current backend still
requires the DMA-BUF protocol at initialization, even though this runtime fallback uses
wl_shm.

## Measure the pipeline

Use the smallest benchmark that covers the question:

- `tools/wayland-capture-bench` measures Wayland screencopy and DMA-BUF capture.
- `tools/wayland-encode-bench` measures Wayland capture through DMA-BUF import,
  VA-API conversion, and `h264_vaapi`. It exits instead of using another encoder.
- `scripts/run-sideboat-production-baseline.sh` measures generated-frame
  encoding and ANFR framing. Its `software` and `vaapi` modes are explicit;
  its VA-API mode uploads CPU frames and does not cover runtime DMA-BUF import.

None of these is an Android or network benchmark. Use an end-to-end run to
measure transport, phone decoding, or display latency.

## Stream and control behavior

`wayland_plugin.rs` limits encoding to the configured FPS. It publishes complete H.264
access units to `FrameBroadcaster`; the broadcaster handles delivery to connected
devices. The capture loop also handles start, stop, output selection, keyframe requests,
device resynchronization, and preview visibility.

When a decoder needs recovery, a keyframe request marks the next frame as an IDR without
recreating the encoder. The encoder is recreated when capture dimensions or the selected
output require it.

The plugin publishes output metadata, stream status, stream dimensions, and output
position to the desktop UI and phone. Input mapping uses the selected output position
sent in stream information.

## Current limitations

- This backend is Linux/Wayland-specific. X11 and non-Linux desktop capture use other
  implementations or are not supported by this path.
- The compositor must support `wlr-screencopy-unstable-v1` and the required DMA-BUF
  globals. A compositor without these globals cannot initialize this backend.
- The default capture mode accepts every compositor frame. Damage-aware capture exists
  in the backend API but is not the production default.
- The reported refresh rate is currently fixed at `60000` mHz; Wayland mode refresh data
  is not read yet.
- Capture uses one reusable buffer and waits synchronously for each compositor response.
  It is not a pipelined multi-buffer capture system.
- VA-API, NVENC, AMF, and FFmpeg filter support depend on installed drivers and build
  options. Software encoding is the compatibility fallback, but it uses more CPU.
- Capture timestamps are placeholders in `CapturedFrame`; compositor presentation times
  are tracked separately by the Wayland state and are only meaningful within one session.
- The stream currently targets H.264 access units. Changing codec or adding a second
  video format requires matching phone-side support.

## Related code

- [Networking](networking.md) — device connections and transport behavior.
- [Protocol transport contract](../protocol/transport-contract.md) — SDK and wire-level
  protocol rules.
- `anchor-desktop/src/anchorwayland/frame_trace.rs` — capture and encode timing traces.
