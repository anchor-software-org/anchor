# Video transport benchmark

This tool measures Anchor's UDP fragmentation and reassembly on the local
machine. It sends deterministic byte buffers from one UDP socket to another
over `127.0.0.1`.

It does not measure screen capture, video encoding, Wi-Fi, VPNs, phone radios,
or Android decoding.

## Run

From the repository root:

```bash
cargo run --release --manifest-path \
  anchor-desktop/tools/video-transport-bench/Cargo.toml -- \
  --frames 600 --frame-bytes 27648 --fps 60
```

The defaults are 600 frames, 27,648 bytes per frame, and 60 frames per
second. Run the tests with:

```bash
cargo test --manifest-path anchor-desktop/tools/video-transport-bench/Cargo.toml
```

## Options

```text
--frames N       Number of frames to send. Must be greater than zero.
--frame-bytes N  Bytes in each test frame. Must be greater than zero.
--fps N           Send pacing in frames per second. Use 0 for no pacing.
```

Use `--fps 0` to measure a local throughput ceiling. It does not model an
interactive stream.

## What it measures

Each frame is split into 1,200-byte UDP datagrams. Every datagram has a
12-byte header and up to 1,188 bytes of frame data:

```text
u32 frame ID | u16 fragment index | u16 fragment count | u32 frame size | data
```

The benchmark reports:

- `sender fragment-and-send`: time to split and send each frame.
- `last-fragment arrival and reassembly`: time from sending a frame to
  receiving all its fragments and checking its reconstructed size.
- `received frames`: how many frames were reassembled with the expected size.
- `effective frame rate`, `payload rate`, and `datagram rate`: totals for the
  complete run.

For each timing series, compare the mean with p95 and p99. A large gap means
some frames take much longer than usual. A non-zero invalid count or a run
that does not receive every frame indicates a benchmark failure.

Results describe loopback transport only. They are useful for checking
fragmentation and reassembly changes, but they are not end-to-end video
latency or network performance results.
