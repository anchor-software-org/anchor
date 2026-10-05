# F-Droid release handoff

Anchor is suitable for a source-built F-Droid release: `com.anchor.software`
is an application ID owned by the project, the app is GPL-3.0-only, and the
Android build uses only source code and dependencies from Google Maven and
Maven Central. It does not contain analytics, advertising, accounts, or a
network service controlled by Anchor.

The Android application requires a Linux desktop running the free and open
source Anchor companion. The phone communicates directly with a paired peer;
there is no Anchor-hosted service. This is not a `NonFreeNet` or `TetheredNet`
dependency.

## Before opening the F-Droid data merge request

1. Finish the Android release version in `anchor/app/build.gradle.kts` and its
   Fastlane changelog at `fastlane/metadata/android/en-US/changelogs/`.
2. From a clean, recursive checkout, build the release with the NDK below:

   ```bash
   scripts/build-android-native.sh "$ANDROID_NDK_ROOT"
   (cd anchor && ./gradlew :app:assembleRelease --no-daemon)
   ```

   The resulting APK must contain `libmsquic.so` and
   `libanchor_msquic_jni.so` for `arm64-v8a`, `armeabi-v7a`, and `x86_64`.
3. Commit the release and create a signed or annotated release tag named
   `v<versionName>` (for example, `v1.0.0`). Push both the commit and tag.
4. Replace `RELEASE_COMMIT_SHA` in the template below with that tag's full,
   immutable commit SHA, then open a merge request to
   [`fdroiddata`](https://gitlab.com/fdroid/fdroiddata) adding
   `metadata/com.anchor.software.yml`.

F-Droid must initialise submodules recursively: MsQuic has a pinned QuicTLS
submodule. Its native libraries are compiled during the build phase, never
checked into the repository. F-Droid's scanner should therefore see only
source-derived native output.

## Metadata template

Update version, code, and commit for each release. The `ndk` value matches the
`ndkVersion` declared by the Android app. `protoc` is required to generate the
checked-in Protocol v1 schemas during Gradle compilation.

```yaml
Categories:
  - Connectivity
  - System
License: GPL-3.0-only
AuthorName: Anchor Software
WebSite: https://anchor-software.org
SourceCode: https://github.com/anchor-software-org/anchor
IssueTracker: https://github.com/anchor-software-org/anchor/issues
Changelog: https://github.com/anchor-software-org/anchor/releases

AutoName: Anchor

RepoType: git
Repo: https://github.com/anchor-software-org/anchor.git

Builds:
  - versionName: 1.0.1
    versionCode: 101
    commit: RELEASE_COMMIT_SHA
    subdir: anchor
    submodules: true
    sudo:
      - apt-get update
      - apt-get install -y protobuf-compiler cmake ninja-build
    gradle:
      - yes
    output: app/build/outputs/apk/release/app-release-unsigned.apk
    build: ../scripts/build-android-native.sh $$NDK$$
    ndk: 30.0.16138531

AutoUpdateMode: Version v%v
UpdateCheckMode: Tags ^v[0-9]+(?:\\.[0-9]+)*$
UpdateCheckData: anchor/app/build.gradle.kts|versionCode = ([0-9]+)|.|versionName = "([^"]+)"
CurrentVersion: 1.0.1
CurrentVersionCode: 101
```

The `sudo` block is only needed if those tools are absent from the active
F-Droid buildserver image. Prefer removing it when the buildserver already
provides them. Do not use `scanignore` for compiled libraries; the build
creates them from the recursively pinned source tree.
