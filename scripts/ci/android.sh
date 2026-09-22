#!/usr/bin/env bash
# Mirrors the "Android APK" job in .github/workflows/verification-artifacts.yml.
# Requires a JDK, protoc, and an Android SDK/NDK (ANDROID_SDK_ROOT).
#
# Unlike a bare gradle invocation this also ensures the MsQuic native
# libraries exist for the shipped ABIs and verifies they land in the APK —
# without them the APK builds fine but crashes on first connect.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
kotlin="$repo_root/anchor-sdk/kotlin"

# ABIs packaged into the APK. Defaults to arm64-v8a, the ABI real phones use;
# CI_MSQUIC_ABIS="arm64-v8a armeabi-v7a x86_64" builds all supported ABIs.
abis=${CI_MSQUIC_ABIS:-arm64-v8a}

missing=0
for abi in $abis; do
    [[ -f "$kotlin/src/main/jniLibs/$abi/libmsquic.so" ]] || missing=1
done

if [[ "$missing" == "1" ]]; then
    sdk="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Android/Sdk}}"
    [[ -d "$sdk" ]] || { echo "ci: Android SDK not found; set ANDROID_SDK_ROOT" >&2; exit 66; }
    ndk="${ANDROID_NDK_ROOT:-}"
    if [[ -z "$ndk" ]]; then
        ndk=$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | sort -V | tail -n 1)
    fi
    [[ -n "$ndk" && -d "$ndk" ]] || { echo "ci: no Android NDK under $sdk/ndk" >&2; exit 66; }
    for abi in $abis; do
        if [[ ! -f "$kotlin/src/main/jniLibs/$abi/libmsquic.so" ]]; then
            echo "==> ci: building MsQuic for $abi"
            "$kotlin/scripts/build-msquic-android.sh" "$ndk" "$abi"
        fi
    done
fi

cd "$repo_root/anchor"
./gradlew :anchorSdk:testDebugUnitTest :app:testDebugUnitTest :app:assembleDebug --no-daemon "$@"

# Verify the packaged APK actually carries the native transport.
apk=$(ls -t app/build/outputs/apk/debug/*.apk | head -n 1)
for abi in $abis; do
    for lib in libmsquic.so libanchor_msquic_jni.so; do
        unzip -l "$apk" | grep -q "lib/$abi/$lib" \
            || { echo "ci: $apk is missing lib/$abi/$lib" >&2; exit 1; }
    done
done
echo "==> ci: $apk verified ($abis native libs present)"
