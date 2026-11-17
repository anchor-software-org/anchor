#!/usr/bin/env bash
# Build the native Android transport libraries from the pinned source checkout.
#
# This is deliberately separate from Gradle: both local release builds and
# source distributors such as F-Droid must compile MsQuic before Gradle packages
# anchorSdk/src/main/jniLibs into the application APK.
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <android-ndk-root> [abi ...]" >&2
  exit 64
fi

ndk_root=$1
shift
abis=("$@")
if [[ ${#abis[@]} -eq 0 ]]; then
  abis=(arm64-v8a armeabi-v7a x86_64)
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
builder="$repo_root/anchor-sdk/kotlin/scripts/build-msquic-android.sh"

[[ -x "$builder" ]] || {
  echo "native transport builder is not executable: $builder" >&2
  exit 66
}

for abi in "${abis[@]}"; do
  "$builder" "$ndk_root" "$abi"
done
