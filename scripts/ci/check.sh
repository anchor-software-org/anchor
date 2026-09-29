#!/usr/bin/env bash
# Mirrors the "fmt + clippy + test" job in .github/workflows/ci-rust.yml.
# Runnable anywhere the system dependencies are present (devenv shell or the
# ci-builder container). Set VENDOR_FFMPEG=1 to build FFmpeg from source like
# release builds instead of linking the pinned prebuilt like PR CI does.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
desktop="$repo_root/anchor-desktop"
frontend="$desktop/frontend"

FFMPEG_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-28-13-06/ffmpeg-n8.1.3-6-gff48edd8b2-linux64-gpl-shared-8.1.tar.xz"
FFMPEG_SHA256="57523ecdd62e78eda9a7571e431be6c40e39cd371a8c6e7ddfbf3c2676487d58"

features=()
if [[ "${VENDOR_FFMPEG:-0}" == "1" ]]; then
    echo "==> FFmpeg mode: vendored source build (release parity)"
else
    echo "==> FFmpeg mode: pinned prebuilt (CI parity; VENDOR_FFMPEG=1 for source)"
    ffmpeg_dir="${XDG_CACHE_HOME:-$HOME/.cache}/anchor/ffmpeg-prebuilt"
    if [[ ! -f "$ffmpeg_dir/lib/libavcodec.so" ]]; then
        mkdir -p "$ffmpeg_dir"
        tmp="$ffmpeg_dir/ffmpeg-prebuilt.tar.xz"
        curl -fsSL -o "$tmp" "$FFMPEG_URL"
        echo "$FFMPEG_SHA256  $tmp" | sha256sum -c -
        tar xf "$tmp" -C "$ffmpeg_dir" --strip-components=1
        rm "$tmp"
    fi
    export FFMPEG_DIR="$ffmpeg_dir"
    export LD_LIBRARY_PATH="$ffmpeg_dir/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    features=(--no-default-features --features ffmpeg-prebuilt)
fi

cd "$frontend"
pnpm install --frozen-lockfile
pnpm run check
pnpm run build

cd "$desktop"
# Keep RUSTFLAGS identical across every cargo invocation: a different value
# per step re-fingerprints every crate and forces full rebuilds between steps.
# Cargo caps lints on dependencies, so -D warnings only bites workspace code.
export RUSTFLAGS="-D warnings"
cargo fmt --all --check
cargo clippy --all-targets --locked "${features[@]}"
cargo check --locked "${features[@]}"
cargo test --locked "${features[@]}" -- --nocapture
unset RUSTFLAGS
cargo test --manifest-path tools/frame-queue-bench/Cargo.toml
cargo check --manifest-path tools/wayland-encode-bench/Cargo.toml
