# Verify pairing in Android release builds

Pairing must be tested with a fresh installation of the **minified release
APK**. Debug APKs and installations that already have an SDK identity can
hide release-only failures.

## Regression behind issue #69

On November 1, 2026, the F-Droid 1.0.1 APK reproduced the missing desktop
pairing prompt reported on Pop!_OS 24.04/COSMIC in
[issue #69](https://github.com/anchor-software-org/anchor/issues/69).
Android failed before sending a QUIC pairing request:

```text
SDK identity could not be created for direct pairing: no such algorithm: EC for provider BC
```

`SdkIdentityStore.ensure()` uses an explicit Bouncy Castle provider for EC
key generation, ECDSA signing and X.509 conversion. The provider loads its
algorithm mappings and implementations by class name. R8 removed or renamed
these reflection-only classes because the SDK supplied no consumer keep rules.
The nearby-device path logged the identity failure without displaying a
desktop prompt; direct-address pairing displayed an identity-creation error.
Existing identities skip key generation, which can hide the regression.

The [issue #69 evidence catalog](evidence/issue-69/README.md) includes the
original log excerpts, APK hashes, native-library comparison, packaged-class
checks and the saved UI state from the successful test.

The SDK now exports rules for the EC and X.509 provider classes in
`anchor-sdk/kotlin/consumer-rules.pro`. Keep the explicit provider: switching
back to Android's built-in BC provider can produce private-key encodings that
MsQuic/OpenSSL cannot load.

## Automated checks

Run the Android CI entrypoint with the documented Android SDK/NDK and JDK:

```sh
bash scripts/ci/android.sh
```

It runs app and SDK unit tests, builds debug and minified release APKs, checks
that the release DEX files still define the crypto implementation classes
under their reflection-visible names, and checks packaged native libraries.
The DEX check can also be run independently:

```sh
python3 scripts/ci/verify-android-pairing.py path/to/release.apk
```

This is a structural guard, not a substitute for the device smoke test below.

## Fresh-install device smoke test

1. Use a dedicated emulator or test phone with no existing Anchor identity.
2. Install the minified release APK, using a test signing key when appropriate.
3. Run the released desktop AppImage on the target desktop session.
4. Request pairing from Android, approve the desktop prompt, and confirm the
   Android status becomes **Connected**. Test nearby discovery when the phone
   and desktop share a multicast-capable network; direct-address pairing is
   useful for a VM behind NAT.
5. Restart Android and confirm it reuses its identity and reconnects.

The November 1 reproduction used a checksum-verified official Pop!_OS 24.04
live ISO in libvirt and a separate Android x86_64 emulator. The downloaded
F-Droid APK failed identity creation; a minified APK with the SDK consumer
rules displayed the COSMIC prompt and completed authenticated pairing with
the unchanged desktop 1.0.0 AppImage.
For that Android test, the native libraries were reused unchanged from the
F-Droid APK to isolate the R8 change; normal release/CI builds compile them
from the pinned sources.

The Android fix requires a new published APK. Updating the desktop alone
cannot repair identity generation inside an already-installed broken APK.
