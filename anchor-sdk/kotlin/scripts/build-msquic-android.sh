#!/usr/bin/env bash
# Build the pinned MsQuic + QuicTLS source for one Android ABI.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <android-ndk-root> [arm64-v8a|armeabi-v7a|x86_64]" >&2
  exit 64
fi

ndk_root=$1
abi=${2:-arm64-v8a}
quic_logging=${QUIC_ENABLE_LOGGING:-OFF}
quic_logging_type=${QUIC_LOGGING_TYPE:-}
case "$abi" in
  arm64-v8a|armeabi-v7a|x86_64) ;;
  *) echo "unsupported ABI: $abi" >&2; exit 64 ;;
esac

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
native_root=$(cd "$script_dir/.." && pwd)
msquic_root="$native_root/src/main/cpp/third_party/msquic"
build_root="$native_root/build/msquic/$abi"

for required in \
  "$ndk_root/build/cmake/android.toolchain.cmake" \
  "$msquic_root/CMakeLists.txt" \
  "$msquic_root/submodules/quictls/Configure"; do
  if [[ ! -e "$required" ]]; then
    echo "missing required path: $required" >&2
    echo "initialize MsQuic and its QuicTLS child before building" >&2
    exit 66
  fi
done

export ANDROID_NDK_ROOT="$ndk_root"
export ANDROID_NDK_HOME="$ndk_root"
export PATH="$ndk_root/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH"

cmake -S "$msquic_root" -B "$build_root" \
  -DCMAKE_TOOLCHAIN_FILE="$ndk_root/build/cmake/android.toolchain.cmake" \
  -DANDROID_ABI="$abi" \
  -DANDROID_PLATFORM=android-29 \
  -DQUIC_TLS_LIB=quictls \
  -DQUIC_ENABLE_LOGGING="$quic_logging" \
  -DQUIC_LOGGING_TYPE="$quic_logging_type" \
  -DQUIC_BUILD_TEST=OFF \
  -DQUIC_BUILD_TOOLS=OFF \
  -DQUIC_BUILD_PERF=OFF \
  -DQUIC_BUILD_SHARED=ON \
  -DCMAKE_BUILD_TYPE=Release
cmake --build "$build_root" --target msquic -j"$(nproc)"

output_dir="$native_root/src/main/jniLibs/$abi"
mkdir -p "$output_dir"
cp "$build_root/bin/Release/libmsquic.so" "$output_dir/libmsquic.so"

jni_build="$build_root/anchor-jni"
cmake -S "$native_root/src/main/cpp" -B "$jni_build" \
  -DCMAKE_TOOLCHAIN_FILE="$ndk_root/build/cmake/android.toolchain.cmake" \
  -DANDROID_ABI="$abi" \
  -DANDROID_PLATFORM=android-29 \
  -DCMAKE_BUILD_TYPE=Release
cmake --build "$jni_build" --target anchor_msquic_jni -j"$(nproc)"
cp "$jni_build/libanchor_msquic_jni.so" "$output_dir/libanchor_msquic_jni.so"

echo "built $output_dir/libmsquic.so and libanchor_msquic_jni.so"
