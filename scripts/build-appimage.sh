#!/usr/bin/env bash
set -euo pipefail

# Build an AppImage for Anchor.
# Requires: cargo, pnpm, curl, and imagemagick (for icon resize)
#
# Output: anchor-desktop/target/appimage/Anchor-<architecture>.AppImage

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DESKTOP_DIR="$PROJECT_DIR/anchor-desktop"
FRONTEND_DIR="$DESKTOP_DIR/frontend"
APPIMAGE_DIR="$DESKTOP_DIR/target/appimage"
APPDIR="$APPIMAGE_DIR/AppDir"
ASSETS_DIR="$PROJECT_DIR/assets"
BUILD_TARGET_DIR="${CARGO_TARGET_DIR:-$DESKTOP_DIR/target}"

if [[ "$BUILD_TARGET_DIR" != /* ]]; then
    BUILD_TARGET_DIR="$DESKTOP_DIR/$BUILD_TARGET_DIR"
fi

# -- Host architecture and tool path --------------------------------------
case "$(uname -m)" in
    x86_64|amd64) HOST_ARCH="x86_64" ;;
    aarch64|arm64) HOST_ARCH="aarch64" ;;
    *)
        echo "Unsupported AppImage architecture: $(uname -m)"
        echo "Supported architectures: x86_64 and aarch64"
        exit 1
        ;;
esac

ARCH="${ARCH:-$HOST_ARCH}"
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)
        echo "Unsupported AppImage architecture: $ARCH"
        exit 1
        ;;
esac

if [ "$ARCH" != "$HOST_ARCH" ]; then
    echo "Cross-architecture AppImage builds are not supported by this script."
    echo "Host architecture: $HOST_ARCH; requested architecture: $ARCH"
    exit 1
fi

APPIMAGETOOL_VERSION="1.9.1"
case "$ARCH" in
    x86_64) APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0" ;;
    aarch64) APPIMAGETOOL_SHA256="f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158" ;;
esac

if [ -z "${APPIMAGETOOL:-}" ]; then
    TOOL_STATE_DIR="${DEVENV_STATE:-${XDG_CACHE_HOME:-$HOME/.cache}/anchor}/appimage-tools"
    APPIMAGETOOL="$TOOL_STATE_DIR/appimagetool-$APPIMAGETOOL_VERSION-$ARCH.AppImage"

    if [ ! -x "$APPIMAGETOOL" ]; then
        mkdir -p "$TOOL_STATE_DIR"
        APPIMAGETOOL_DOWNLOAD="$APPIMAGETOOL.download"
        APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-$ARCH.AppImage"

        echo "==> Downloading appimagetool $APPIMAGETOOL_VERSION for $ARCH..."
        curl -fL --retry 3 "$APPIMAGETOOL_URL" -o "$APPIMAGETOOL_DOWNLOAD"
        echo "$APPIMAGETOOL_SHA256  $APPIMAGETOOL_DOWNLOAD" | sha256sum --check --status
        chmod +x "$APPIMAGETOOL_DOWNLOAD"
        mv "$APPIMAGETOOL_DOWNLOAD" "$APPIMAGETOOL"
    fi
elif [ ! -x "$APPIMAGETOOL" ]; then
    echo "APPIMAGETOOL is not executable: $APPIMAGETOOL"
    exit 1
fi

if [ -n "${APPIMAGE_EMULATOR:-}" ]; then
    if [ ! -x "$APPIMAGE_EMULATOR" ]; then
        echo "APPIMAGE_EMULATOR is not executable: $APPIMAGE_EMULATOR" >&2
        exit 1
    fi
fi

# qemu-user binaries built for binfmt run in preserve-argv0 mode (kernel flag
# P): the argument after the program path becomes the guest argv[0], so it
# must be repeated when invoking a program explicitly.
run_appimage_tool() {
    if [ -n "${APPIMAGE_EMULATOR:-}" ]; then
        "$APPIMAGE_EMULATOR" "$1" "$@"
    else
        "$@"
    fi
}

echo "==> Building $ARCH AppImage on $HOST_ARCH host"

if [ "${APPIMAGE_PACKAGE_ONLY:-0}" = "1" ]; then
    for required in "$APPDIR/AppRun" "$APPDIR/usr/bin/anchor"; do
        if [ ! -e "$required" ]; then
            echo "APPIMAGE_PACKAGE_ONLY=1 but $required is missing." >&2
            echo "Run a full build first to produce the AppDir." >&2
            exit 1
        fi
    done
    echo "==> Reusing existing AppDir (APPIMAGE_PACKAGE_ONLY=1)"
else

# -- Build release application --------------------------------------------
if ! command -v pnpm >/dev/null 2>&1; then
    echo "Missing tool: pnpm"
    exit 1
fi

echo "==> Building Tauri frontend..."
cd "$FRONTEND_DIR"
PNPM_INSTALL_ARGS=(install --frozen-lockfile)
if [ -n "${PNPM_STORE_DIR:-}" ]; then
    PNPM_INSTALL_ARGS+=(--store-dir "$PNPM_STORE_DIR")
fi
pnpm "${PNPM_INSTALL_ARGS[@]}"
pnpm run build

echo "==> Building Anchor (release profile)..."
cd "$DESKTOP_DIR"
cargo build --release --locked --bin anchor

# -- Prepare AppDir -------------------------------------------------------
echo "==> Preparing AppDir..."
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib"
mkdir -p "$APPDIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$APPDIR/usr/share/applications"

cp "$BUILD_TARGET_DIR/release/anchor" "$APPDIR/usr/bin/anchor"

# A portable AppImage must never contain a Nix store interpreter or runtime
# dependency. This catches accidental direct builds from a devenv shell before
# producing an artifact that only starts on the build machine.
if readelf -l "$APPDIR/usr/bin/anchor" | grep -q '/nix/store'; then
    echo "Refusing to package a Nix-linked executable." >&2
    echo "Use 'build-image', which builds inside the Ubuntu 22.04 builder." >&2
    exit 1
fi
if ldd "$APPDIR/usr/bin/anchor" | grep -q '/nix/store'; then
    echo "Refusing to package an executable with Nix runtime dependencies." >&2
    echo "Use 'build-image', which builds inside the Ubuntu 22.04 builder." >&2
    exit 1
fi

# FFmpeg is static, but libx264 is dynamically linked. Preserve the exact
# SONAME used by this build so the AppImage does not depend on the host's x264
# ABI version.
X264_PATH="$(ldd "$APPDIR/usr/bin/anchor" | awk '$1 ~ /^libx264\.so/ { print $3; exit }')"
if [ -z "$X264_PATH" ]; then
    echo "Unable to resolve libx264 for AppImage packaging"
    exit 1
fi
cp -L "$X264_PATH" "$APPDIR/usr/lib/$(basename "$X264_PATH")"

# Bundle adb and its non-glibc dependency closure in an isolated library
# directory. Its Ubuntu libraries must not enter Anchor's GTK/WebKit process.
if ADB_PATH=$(command -v adb); then
    mkdir -p "$APPDIR/usr/lib/adb"
    cp "$ADB_PATH" "$APPDIR/usr/bin/adb.real"
    while read -r lib; do
        case "$(basename "$lib")" in
            libc.so.*|libm.so.*|libpthread.so.*|libdl.so.*|librt.so.*) ;;
            *) cp -Ln "$lib" "$APPDIR/usr/lib/adb/" ;;
        esac
    done < <(ldd "$APPDIR/usr/bin/adb.real" | awk '/=> \// { print $3 }')
    cat > "$APPDIR/usr/bin/adb" << 'ADB_RUN'
#!/usr/bin/env bash
HERE="$(cd -- "$(dirname -- "$0")" && pwd -P)"
export LD_LIBRARY_PATH="$HERE/../lib/adb${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/adb.real" "$@"
ADB_RUN
    chmod +x "$APPDIR/usr/bin/adb"
    "$APPDIR/usr/bin/adb" version
else
    echo "WARN: adb not found on host; AppImage will not bundle adb"
fi

# Desktop entry
cat > "$APPDIR/usr/share/applications/anchor.desktop" << 'EOF'
[Desktop Entry]
Type=Application
Name=Anchor
Comment=Phone companion - screen mirroring, SMS, notifications, media control
Exec=anchor
Icon=anchor
StartupWMClass=com.anchor.desktop
Categories=Utility;
Terminal=false
EOF

# Icon (resize to 256x256 for AppImage compliance)
ICON_SRC="$ASSETS_DIR/anchor_logo_transparent.png"
ICON_DST="$APPDIR/usr/share/icons/hicolor/256x256/apps/anchor.png"

if command -v magick &>/dev/null; then
    magick "$ICON_SRC" -resize 256x256 "$ICON_DST"
elif command -v convert &>/dev/null; then
    convert "$ICON_SRC" -resize 256x256 "$ICON_DST"
elif command -v ffmpeg &>/dev/null; then
    ffmpeg -y -i "$ICON_SRC" -vf scale=256:256 "$ICON_DST" 2>/dev/null
else
    echo "Warning: no imagemagick/ffmpeg found, copying icon as-is (may fail validation)"
    cp "$ICON_SRC" "$ICON_DST"
fi

# Symlinks at AppDir root (AppImage convention)
ln -sf usr/share/applications/anchor.desktop "$APPDIR/anchor.desktop"
ln -sf usr/share/icons/hicolor/256x256/apps/anchor.png "$APPDIR/anchor.png"

# Tauri dynamically links against GTK/WebKit. Copying that stack into an
# AppImage produces a hybrid process when extensions resolve host libraries;
# on current Arch this crashes in ld.so before Anchor starts. Keep the AppDir
# intentionally small and use the host's coherent GTK/WebKit and GPU stack.
cat > "$APPDIR/AppRun" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail
APPDIR="$(cd -- "$(dirname -- "$0")" && pwd -P)"

# AppImages are normally launched directly from a file, so GNOME never sees
# the embedded desktop entry. Register it in the user's XDG directories on
# first launch so the Wayland app ID can be matched to Anchor's icon.
if [[ -n "${APPIMAGE:-}" ]]; then
    XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
    DESKTOP_DIR="$XDG_DATA_HOME/applications"
    ICON_DIR="$XDG_DATA_HOME/icons/hicolor/256x256/apps"
    mkdir -p "$DESKTOP_DIR" "$ICON_DIR"
    awk -v appimage="$APPIMAGE" '$0 == "Exec=anchor" { print "Exec=\"" appimage "\""; next } { print }' \
        "$APPDIR/usr/share/applications/anchor.desktop" > "$DESKTOP_DIR/com.anchor.desktop.desktop"
    cp "$APPDIR/usr/share/icons/hicolor/256x256/apps/anchor.png" "$ICON_DIR/anchor.png"
fi

export PATH="$APPDIR/usr/bin${PATH:+:$PATH}"
export LD_LIBRARY_PATH="$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$APPDIR/usr/bin/anchor" "$@"
EOF
chmod +x "$APPDIR/AppRun"

LD_LIBRARY_PATH="$APPDIR/usr/lib" ldd "$APPDIR/usr/bin/anchor" \
    | grep "libx264.*$APPDIR/usr/lib"

fi

cd "$APPIMAGE_DIR"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" run_appimage_tool "$APPIMAGETOOL" \
    AppDir "Anchor-$ARCH.AppImage"

# Verify the image actually works: extract it (no FUSE needed), confirm the
# packaged binary resolves every library with the bundled lib dir on the path,
# and prove the bundled adb runs using only its private lib closure.
echo "==> Verifying AppImage contents..."
VERIFY_DIR="$APPIMAGE_DIR/verify"
rm -rf "$VERIFY_DIR"
mkdir -p "$VERIFY_DIR"
(cd "$VERIFY_DIR" && APPIMAGE_EXTRACT_AND_RUN=1 run_appimage_tool \
    "$APPIMAGE_DIR/Anchor-$ARCH.AppImage" --appimage-extract >/dev/null)
SQFS="$VERIFY_DIR/squashfs-root"

for required in "$SQFS/AppRun" "$SQFS/usr/bin/anchor" "$SQFS/usr/bin/adb" \
    "$SQFS/usr/share/applications/anchor.desktop" \
    "$SQFS/usr/share/icons/hicolor/256x256/apps/anchor.png"; do
    if [ ! -e "$required" ]; then
        echo "AppImage verification failed: missing $required" >&2
        exit 1
    fi
done

if LD_LIBRARY_PATH="$SQFS/usr/lib" ldd "$SQFS/usr/bin/anchor" | grep "not found"; then
    echo "AppImage verification failed: anchor has unresolved libraries" >&2
    exit 1
fi
"$SQFS/usr/bin/adb" version >/dev/null

rm -rf "$VERIFY_DIR"
echo "==> Done: $(ls -lh "Anchor-$ARCH.AppImage") (verified: extracts cleanly, all libraries resolve, bundled adb runs)"
