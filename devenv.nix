{ pkgs, ... }:

{
  # The Android SDK/NDK are intentionally not fetched by Nix: Google licenses
  # and local emulator/device configuration belong to the developer. The shell
  # discovers an existing SDK and selects its newest installed NDK.
  packages = with pkgs; [
    android-tools
    cmake
    ninja
    clang
    protobuf
    git
    jdk21
    unzip
    zip
  ];

  env.JAVA_HOME = "${pkgs.jdk21}";

  scripts.android-env.exec = ''
    set -euo pipefail
    sdk="''${ANDROID_SDK_ROOT:-}"
    [[ -d "$sdk" ]] || sdk="''${ANDROID_HOME:-}"
    [[ -d "$sdk" ]] || sdk="$HOME/Android/Sdk"
    if [[ ! -d "$sdk" ]]; then
      echo "Android SDK not found at $sdk; set ANDROID_SDK_ROOT" >&2
      exit 66
    fi
    ndk="''${ANDROID_NDK_ROOT:-}"
    if [[ -z "$ndk" ]]; then
      ndk=$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | sort -V | tail -n 1)
    fi
    if [[ -z "$ndk" || ! -d "$ndk" ]]; then
      echo "No Android NDK found under $sdk/ndk; install one or set ANDROID_NDK_ROOT" >&2
      exit 66
    fi
    printf 'ANDROID_SDK_ROOT=%s\nANDROID_NDK_ROOT=%s\nJAVA_HOME=%s\n' "$sdk" "$ndk" "$JAVA_HOME"
  '';

  scripts.android-msquic-build.exec = ''
    set -euo pipefail
    sdk="''${ANDROID_SDK_ROOT:-}"
    [[ -d "$sdk" ]] || sdk="''${ANDROID_HOME:-}"
    [[ -d "$sdk" ]] || sdk="$HOME/Android/Sdk"
    ndk="''${ANDROID_NDK_ROOT:-}"
    if [[ -z "$ndk" ]]; then
      ndk=$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | sort -V | tail -n 1)
    fi
    if [[ -z "$ndk" || ! -d "$ndk" ]]; then
      echo "No Android NDK found; run android-env for diagnostics" >&2
      exit 66
    fi
    abi="''${1:-arm64-v8a}"
    exec "$DEVENV_ROOT/anchor-sdk/kotlin/scripts/build-msquic-android.sh" "$ndk" "$abi"
  '';

  scripts.android-msquic-build-all.exec = ''
    set -euo pipefail
    for abi in arm64-v8a armeabi-v7a x86_64; do
      android-msquic-build "$abi"
    done
  '';

  scripts.android-build.exec = ''
    set -euo pipefail
    if ! find "$DEVENV_ROOT/anchor-sdk/kotlin/src/main/jniLibs" -name '*.so' -print -quit 2>/dev/null | grep -q .; then
      echo "warning: no native libraries under anchor-sdk/kotlin/src/main/jniLibs/" >&2
      echo "the APK builds but crashes on first connect (UnsatisfiedLinkError)." >&2
      echo "run: android-msquic-build-all" >&2
    fi
    cd "$DEVENV_ROOT/anchor"
    exec ./gradlew :app:assembleDebug --no-daemon "$@"
  '';

  scripts.android-sdk-test.exec = ''
    set -euo pipefail
    cd "$DEVENV_ROOT/anchor"
    exec ./gradlew :anchorSdk:testDebugUnitTest --no-daemon "$@"
  '';

  # Run a CI job exactly as GitHub runs it (scripts/ci.sh job list).
  scripts.ci.exec = ''
    exec "$DEVENV_ROOT/scripts/ci.sh" "$@"
  '';

  scripts.android-install.exec = ''
    set -euo pipefail
    cd "$DEVENV_ROOT/anchor"
    apk="app/build/outputs/apk/debug/app-debug.apk"
    [[ -f "$apk" ]] || android-build
    serial="''${ANDROID_SERIAL:-$(adb devices | sed -n '/^[^List][^[:space:]]*[[:space:]]*device$/s/[[:space:]].*$//p' | head -n 1)}"
    if [[ -z "$serial" ]]; then
      echo "No adb device found; set ANDROID_SERIAL or connect a device" >&2
      exit 67
    fi
    adb -s "$serial" install -r "$apk"
    echo "Installed $apk on $serial"
  '';

  enterShell = ''
    if [[ ! -d "''${ANDROID_SDK_ROOT:-}" ]]; then
      if [[ -d "''${ANDROID_HOME:-}" ]]; then
        export ANDROID_SDK_ROOT="$ANDROID_HOME"
      else
        export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
      fi
    fi
    export ANDROID_HOME="$ANDROID_SDK_ROOT"
    if [[ -z "''${ANDROID_NDK_ROOT:-}" && -d "$ANDROID_SDK_ROOT/ndk" ]]; then
      export ANDROID_NDK_ROOT=$(find "$ANDROID_SDK_ROOT/ndk" -mindepth 1 -maxdepth 1 -type d -print | sort -V | tail -n 1)
    fi
    echo "Anchor Android development environment"
    echo "Use android-env, android-msquic-build [ABI], android-build, android-sdk-test, or android-install"
    if [[ ! -f "$DEVENV_ROOT/anchor-sdk/kotlin/src/main/cpp/third_party/msquic/CMakeLists.txt" ]]; then
      echo "note: MsQuic submodule not initialized — run: git submodule update --init --recursive" >&2
    elif ! find "$DEVENV_ROOT/anchor-sdk/kotlin/src/main/jniLibs" -name '*.so' -print -quit 2>/dev/null | grep -q .; then
      echo "note: Android native libraries not built — run: android-msquic-build-all" >&2
    fi
  '';
}
