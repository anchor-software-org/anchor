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
    jq
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

  scripts.ios-sdk-test.exec = ''
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "iOS tests require macOS and Xcode" >&2
      exit 69
    fi
    cd "$DEVENV_ROOT/anchor-sdk/swift"
    exec swift test "$@"
  '';

  scripts.ios-build-for-testing.exec = ''
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "iOS builds require macOS and Xcode" >&2
      exit 69
    fi
    cd "$DEVENV_ROOT/anchor-ios"
    exec xcodebuild \
      -project Anchor.xcodeproj \
      -scheme Anchor \
      -destination "generic/platform=iOS" \
      -derivedDataPath "$DEVENV_ROOT/.devenv/ios-derived-data" \
      build-for-testing "$@"
  '';

  scripts.ios-verify.exec = ''
    set -euo pipefail
    ios-sdk-test
    ios-build-for-testing
    ios-test "$@"
  '';

  scripts.ios-test.exec = ''
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "iOS tests require macOS and Xcode" >&2
      exit 69
    fi
    device_id="''${IOS_DEVICE_ID:-}"
    signing_options=()
    if [[ -z "$device_id" ]]; then
      device_id=$(xcrun simctl list devices available --json | jq -r '
        [.devices[][] | select(.name | test("iPad"))][0].udid // empty
      ')
      if [[ -z "$device_id" ]]; then
        echo "No available iPad simulator is installed; set IOS_DEVICE_ID explicitly" >&2
        exit 67
      fi
      xcrun simctl boot "$device_id" >/dev/null 2>&1 || true
      signing_options=(CODE_SIGNING_ALLOWED=NO)
    fi
    cd "$DEVENV_ROOT/anchor-ios"
    exec xcodebuild \
      -project Anchor.xcodeproj \
      -scheme Anchor \
      -destination "id=$device_id" \
      -derivedDataPath "$DEVENV_ROOT/.devenv/ios-derived-data" \
      "''${signing_options[@]}" \
      test "$@"
  '';

  scripts.ios-install.exec = ''
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "iOS installation requires macOS and Xcode" >&2
      exit 69
    fi
    device_id="''${IOS_DEVICE_ID:-}"
    if [[ -z "$device_id" ]]; then
      echo "Set IOS_DEVICE_ID to the identifier shown by: xcrun devicectl list devices" >&2
      exit 67
    fi
    derived_data="$DEVENV_ROOT/.devenv/ios-derived-data"
    cd "$DEVENV_ROOT/anchor-ios"
    xcodebuild \
      -project Anchor.xcodeproj \
      -scheme Anchor \
      -destination "id=$device_id" \
      -derivedDataPath "$derived_data" \
      build "$@"
    app="$derived_data/Build/Products/Debug-iphoneos/Anchor.app"
    if [[ ! -d "$app" ]]; then
      echo "Built application not found at $app" >&2
      exit 66
    fi
    xcrun devicectl device install app --device "$device_id" "$app"
  '';

  scripts.ios-run.exec = ''
    set -euo pipefail
    ios-install "$@"
    exec xcrun devicectl device process launch \
      --device "$IOS_DEVICE_ID" \
      --terminate-existing \
      com.anchor.software
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
    echo "Anchor development environment"
    echo "Android: android-env, android-msquic-build [ABI], android-build, android-sdk-test, android-install"
    echo "iOS: ios-sdk-test, ios-build-for-testing, ios-verify, ios-test, ios-install, ios-run"
    if [[ ! -f "$DEVENV_ROOT/anchor-sdk/kotlin/src/main/cpp/third_party/msquic/CMakeLists.txt" ]]; then
      echo "note: MsQuic submodule not initialized — run: git submodule update --init --recursive" >&2
    elif ! find "$DEVENV_ROOT/anchor-sdk/kotlin/src/main/jniLibs" -name '*.so' -print -quit 2>/dev/null | grep -q .; then
      echo "note: Android native libraries not built — run: android-msquic-build-all" >&2
    fi
  '';
}
