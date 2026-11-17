# Issue #69: pairing evidence

The F-Droid 1.0.1 APK failed on a fresh installation before sending the
desktop a pairing request. Adding the SDK consumer keep rules restored
identity generation and pairing with the unchanged desktop 1.0.0 AppImage.

This reproduces the missing-popup symptom from
[issue #69](https://github.com/anchor-software-org/anchor/issues/69). The
reporter's Android version and logs were not supplied, so their particular
installation still needs confirmation after the update.

## Test environment and artifacts

Tests were captured on November 1, 2026: an official Pop!_OS 24.04 live ISO
in libvirt, COSMIC package `0.1~1786994555~24.04~8806436`, and a separate
Android x86_64 emulator. Pairing used the desktop's IPv4 address across NAT.

[artifacts.json](artifacts.json) records the APK/AppImage download URLs,
SHA-256 hashes, source commits and per-library hashes. The fixed APK was
R8-minified and signed with a temporary test key. It is not a signed
production release.

## 1. Reproduce with the distributed APK

| Android build, fresh identity | Desktop | Result |
| --- | --- | --- |
| GitHub 1.0.0 APK | Released 1.0.0 AppImage on COSMIC | Prompt shown; authenticated connection established |
| F-Droid 1.0.1 APK | Same released AppImage on COSMIC | Identity creation fails; no prompt |
| Fixed minified APK | Same released AppImage on COSMIC | Prompt shown; authenticated connection established |

[fdroid-before.txt](fdroid-before.txt) contains the original logcat lines
showing an installation with zero trusted devices, a direct pairing attempt,
and the failure:

```text
SDK identity could not be created for direct pairing: no such algorithm: EC for provider BC
```

These are selected lines from the original capture, not reconstructed output.
The error is raised inside `SdkIdentityStore.ensure()` before
`AnchorPairingSession.connect()` runs. Discovery can therefore find a desktop
while the pairing request never reaches it. This is shared Android release
code, not a COSMIC-specific protocol path.

## 2. Check the packaged cause

[packaged-provider-check.txt](packaged-provider-check.txt) records the DEX
class-definition check against the three APKs. The downloaded F-Droid APK is
missing the reflection-visible names for the EC/X.509 mappings, EC key
generator and ECDSA signer. The check fails on that APK and passes on both
the unminified GitHub APK and the fixed minified APK.

The Android application, Kotlin SDK and protocol source trees have no
differences between F-Droid's build commit
`11c53e011cca8392f4993820c57d819cc05cfad1` and this fix's base
`79e1ba6f91dafdc2ea5212c31310c3e3513f2c5f`. The runtime fix adds only the four
provider keep rules and their Gradle registration.

All six packaged MsQuic/JNI libraries, across arm64-v8a, armeabi-v7a and
x86_64, are **byte-for-byte identical** between the downloaded F-Droid APK
and the fixed test APK; see the hashes in `artifacts.json`. Reusing them
isolated the R8 change from native transport changes. Normal release builds
still build these libraries from the pinned sources.

## 3. Verify the result

[fixed-after.txt](fixed-after.txt) records a new installation with zero
trusted devices, followed by successful pairing and an authenticated QUIC
SDK session. The desktop was the unchanged published 1.0.0 AppImage.

The [Android UI dump from that test](android-original-desktop-ui.xml) also
records the **Connected** status after approval.

## 4. Guard against another release regression

[android-ci-summary.txt](android-ci-summary.txt) records the passing Android
CI entrypoint: 42 SDK tests, 104 app tests, debug/release packaging, native
library verification and the minified-release DEX check. Debug tests alone
would not detect R8 removing a reflection-only class.

To repeat the packaged-class check:

```sh
python3 scripts/ci/verify-android-pairing.py path/to/release.apk
```

## Limits

- This verifies fresh-install pairing by address. Nearby discovery on a
  shared multicast network was not tested across the emulator/VM NAT.
- The packaged-class check proves the required names survive R8; the device
  test verifies that identity generation and authenticated pairing work.
- COSMIC screen capture remains unavailable through the current
  wlr-screencopy backend. Successful pairing does not establish capture support.
- The separate desktop prompt-timeout fix is not included in this PR and
  was not required for the successful test against desktop 1.0.0.
