#!/usr/bin/env bash
set -euo pipefail

output_dir="/tmp/anchor-sideboat-production-baseline"
if [[ "${1:-}" != --* && -n "${1:-}" ]]; then
  output_dir="$1"
  shift
fi

# This runs the desktop crate's real encoder selection. It defaults to the
# portable libx264 fallback; pass --encoder vaapi to require h264_vaapi. It is
# separate from run-sideboat-quality-baseline.sh, whose external FFmpeg encoder
# is useful for transport/loss experiments but does not exercise VaapiEncoder.
exec cargo run --release --manifest-path anchor-desktop/Cargo.toml \
  --example sideboat_production_baseline -- \
  --output "$output_dir" "$@"
