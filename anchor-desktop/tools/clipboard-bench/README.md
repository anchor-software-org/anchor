# Clipboard benchmark

This standalone Rust program measures the CPU work used to build an Anchor
clipboard packet:

1. Hash the content with SHA-256.
2. Convert text or image bytes to the packet representation.
3. Compress text larger than 1 KiB with deflate.
4. Base64-encode binary content.
5. Serialize the JSON packet.

It does not read the desktop clipboard, use Wayland, watch for changes, or
send data over the network. The inputs are synthetic and repeatable.

## Run

From the repository root:

```bash
cargo run --release --manifest-path anchor-desktop/tools/clipboard-bench/Cargo.toml -- \
  --iterations 1000
```

`--iterations` sets the number of samples for each case. It defaults to `1000`
and values below `1` are treated as `1`.

## Output

The benchmark runs these cases:

- `text-128B`
- `text-16KiB`
- `text-1MiB`
- `image-256KiB`

Each line shows the input size, JSON packet size, and elapsed time statistics:

- `mean`: average sample time
- `p50`: median sample time
- `p95`: 95th-percentile sample time
- `p99`: 99th-percentile sample time

Times are CPU-side packet-preparation time in milliseconds. Compare runs on the
same machine with the same build mode. Lower values are better. A higher p95 or
p99 with a stable mean indicates occasional slow samples.

## Limits

The image input is arbitrary bytes labeled as `image_png`; it is not a decoded
PNG. Text inputs contain repeated `a` bytes, so compression results will not
match varied real-world text. The benchmark does not measure clipboard access,
transport, encryption, allocation outside packet preparation, or receiver-side
work.
