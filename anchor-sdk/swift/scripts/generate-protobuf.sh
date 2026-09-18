#!/usr/bin/env bash
set -euo pipefail

# SwiftProtobuf is intentionally a host build dependency, just like protoc is
# for the Rust and Kotlin SDKs. Install protoc-gen-swift from the SwiftProtobuf
# release that your Xcode toolchain supports, then run this script from any
# checkout of Anchor.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
OUT="$ROOT/anchor-sdk/swift/Sources/AnchorSDK/Generated"
mkdir -p "$OUT"

mapfile -t PROTOS < <(find "$ROOT/anchor-sdk/protocol/anchor/v1" -type f -name '*.proto' | sort)
protoc \
  --proto_path="$ROOT/anchor-sdk/protocol" \
  --swift_opt=Visibility=Public \
  --swift_out="$OUT" \
  "${PROTOS[@]}"
