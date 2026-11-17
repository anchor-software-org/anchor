#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

msquic=anchor-sdk/kotlin/src/main/cpp/third_party/msquic
if git submodule status -- "$msquic" | awk '$1 ~ /^-/' | grep -q .; then
  echo "Missing required SDK submodule '$msquic'. Run: git submodule update --init --recursive" >&2
  exit 2
fi
if git -C "$msquic" submodule status -- submodules/quictls | awk '$1 ~ /^-/' | grep -q .; then
  echo "Missing required SDK submodule 'quictls'. Run: git submodule update --init --recursive" >&2
  exit 2
fi

cargo test --manifest-path anchor-sdk/rust/Cargo.toml --locked
(
  cd anchor
  ./gradlew :anchorSdk:testDebugUnitTest --no-daemon
)
