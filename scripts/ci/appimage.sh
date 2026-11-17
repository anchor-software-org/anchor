#!/usr/bin/env bash
# Mirrors the "Build AppImage" job in .github/workflows/appimage.yml for the
# host architecture. Requires an Ubuntu-22.04-equivalent dependency set; on a
# devenv shell use `build-image` instead, which runs this inside the Ubuntu
# 22.04 builder container (a Nix-linked binary is refused by the packager).
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
exec "$repo_root/scripts/build-appimage.sh" "$@"
