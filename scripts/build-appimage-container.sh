#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd -P)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BUILDER_DIR="$SCRIPT_DIR/appimage-builder"
ENGINE="${CONTAINER_ENGINE:-docker}"

case "$(uname -m)" in
    x86_64|amd64) HOST_ARCH="x86_64" ;;
    aarch64|arm64) HOST_ARCH="aarch64" ;;
    *)
        echo "Unsupported AppImage architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

TARGET_ARCH="${ARCH:-$HOST_ARCH}"
case "$TARGET_ARCH" in
    x86_64|amd64)
        TARGET_ARCH="x86_64"
        CONTAINER_PLATFORM="linux/amd64"
        ;;
    aarch64|arm64)
        TARGET_ARCH="aarch64"
        CONTAINER_PLATFORM="linux/arm64"
        ;;
    *)
        echo "Unsupported AppImage architecture: $TARGET_ARCH" >&2
        echo "Supported architectures: x86_64 and aarch64" >&2
        exit 1
        ;;
esac

if ! command -v "$ENGINE" >/dev/null 2>&1; then
    echo "Missing container engine: $ENGINE" >&2
    echo "Install Docker, or set CONTAINER_ENGINE to a Docker-compatible engine." >&2
    exit 1
fi

CACHE_BASE="${ANCHOR_APPIMAGE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/anchor/appimage-builder}"
CACHE_ROOT="$CACHE_BASE/$TARGET_ARCH"
mkdir -p "$CACHE_ROOT"/{cargo,frontend-node-modules,home,pnpm,state}

IMAGE="anchor-appimage-builder:ubuntu-22.04-$TARGET_ARCH"

echo "==> Preparing cached Ubuntu 22.04 $TARGET_ARCH AppImage builder..."
"$ENGINE" build \
    --platform "$CONTAINER_PLATFORM" \
    --tag "$IMAGE" \
    "$BUILDER_DIR"

RUN_ARGS=(
    run
    --rm
    --init
    --platform "$CONTAINER_PLATFORM"
    --user "$(id -u):$(id -g)"
    --env ARCH="$TARGET_ARCH"
    --env HOME=/cache/home
    --env CARGO_HOME=/cache/cargo
    --env CARGO_TARGET_DIR="/workspace/anchor-desktop/target/appimage-ubuntu-22.04-$TARGET_ARCH"
    --env DEVENV_STATE=/cache/state
    --env PNPM_STORE_DIR=/cache/pnpm
    --env CI=true
    --volume "$PROJECT_DIR:/workspace"
    --volume "$CACHE_ROOT:/cache"
    --volume "$CACHE_ROOT/frontend-node-modules:/workspace/anchor-desktop/frontend/node_modules"
    --workdir /workspace
)

if [ -t 0 ]; then
    RUN_ARGS+=(--interactive)
fi
if [ -t 1 ]; then
    RUN_ARGS+=(--tty)
fi

echo "==> Building portable $TARGET_ARCH AppImage in Ubuntu 22.04..."
"$ENGINE" "${RUN_ARGS[@]}" "$IMAGE" \
    /workspace/scripts/build-appimage.sh "$@"
