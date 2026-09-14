#!/usr/bin/env bash
set -euo pipefail
# Build a portable anchor bundle (not AppImage) for local testing.
# All .so deps are copied into lib/ and a wrapper script sets LD_LIBRARY_PATH.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DESKTOP_DIR="$PROJECT_DIR/anchor-desktop"
FRONTEND_DIR="$DESKTOP_DIR/frontend"
BUNDLE_DIR="$DESKTOP_DIR/target/portable-bundle"

echo "==> Building Tauri frontend..."
cd "$FRONTEND_DIR"
pnpm install --frozen-lockfile
pnpm run build

echo "==> Building Anchor (release)..."
cd "$DESKTOP_DIR"
cargo build --release --locked --bin anchor

echo "==> Creating portable bundle at $BUNDLE_DIR..."
rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR/lib"

# Copy binary
cp target/release/anchor "$BUNDLE_DIR/anchor"

# Copy ALL transitive .so deps (follow symlinks, copy real files)
BINARY="$BUNDLE_DIR/anchor"
DEPS=$(ldd "$BINARY" 2>/dev/null | grep -oP '=> \K/\S+' || true)

for lib in $DEPS; do
    if [ -f "$lib" ]; then
        real=$(readlink -f "$lib" 2>/dev/null || echo "$lib")
        name=$(basename "$real")
        if [ ! -f "$BUNDLE_DIR/lib/$name" ]; then
            cp -L "$lib" "$BUNDLE_DIR/lib/$name"
        fi
    fi
done

# Also copy the dynamic linker (needed for ELF interpreter)
INTERP=$(readelf -l "$BINARY" 2>/dev/null | grep "Requesting program interpreter" | grep -oP '/\S+')
if [ -n "$INTERP" ] && [ -f "$INTERP" ]; then
    cp -L "$INTERP" "$BUNDLE_DIR/lib/"
fi

# Create wrapper script
cat > "$BUNDLE_DIR/run-anchor" << 'SCRIPT'
#!/bin/bash
HERE="$(cd "$(dirname "$0")" && pwd)"
export LD_LIBRARY_PATH="$HERE/lib:$LD_LIBRARY_PATH"
exec "$HERE/anchor" "$@"
SCRIPT
chmod +x "$BUNDLE_DIR/run-anchor"

echo "==> Bundle created:"
echo "    Binary:   $BUNDLE_DIR/anchor"
echo "    Wrapper:  $BUNDLE_DIR/run-anchor"
echo "    Libs:     $(ls "$BUNDLE_DIR/lib" | wc -l) .so files"
echo "    Size:     $(du -sh "$BUNDLE_DIR" | cut -f1)"

# Test it
echo "==> Testing bundle..."
cd "$BUNDLE_DIR"
timeout 4 ./run-anchor 2>&1 | head -10 || true
