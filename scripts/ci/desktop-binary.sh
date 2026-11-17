#!/usr/bin/env bash
# Mirrors the "Desktop binary (Linux)" job in
# .github/workflows/verification-artifacts.yml. Builds the release binary the
# same way the release does: vendored FFmpeg from source, so this is slow on a
# cold target dir.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
desktop="$repo_root/anchor-desktop"

cd "$desktop/frontend"
pnpm install --frozen-lockfile
pnpm run build

cd "$desktop"
cargo build --release --locked --bin anchor

# Verify the artifact: a real ELF with a fully resolvable library closure.
bin="$desktop/target/release/anchor"
file "$bin" | grep -q "ELF" || { echo "ci: $bin is not an ELF binary" >&2; exit 1; }
if ldd "$bin" | grep "not found"; then
    echo "ci: $bin has unresolved library dependencies" >&2
    exit 1
fi
echo "==> ci: $bin verified (ELF, all libraries resolve)"
