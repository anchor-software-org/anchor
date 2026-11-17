# Anchor SDK

Anchor SDK contains native implementations of Anchor Protocol v1.

The SDKs are source code in this repository. Clone the repository at a tagged
release and use the SDK you need:

```text
anchor-sdk/protocol/  Protocol schemas and conformance fixtures
anchor-sdk/rust/      Rust SDK with Quinn transport
anchor-sdk/kotlin/    Kotlin/Android SDK with MsQuic transport
anchor-sdk/swift/     Swift wire layer (preview)
```

## Status

Rust and Kotlin are the supported SDKs. They provide:

- Protocol v1 message types generated from the canonical schemas
- invitation and pairing validation
- pinned-peer identity storage boundaries
- mutual-TLS QUIC transport adapters
- authenticated session setup
- capability negotiation
- typed reliable records, bidirectional streams, and datagrams

The host application supplies the UI, permissions, local storage, and product
policy. The SDK does not provide a complete Anchor application.

Swift is preview-only. It includes generated Protocol v1 message types, shared
wire framing, session ordering, capability constants, and screen/camera
datagram codec. It also provides a pinned-peer `Network.framework` QUIC
transport for Apple platforms. The iOS application has not migrated to this
boundary yet.

## Use the SDK from source

Clone the repository recursively and check out a release tag:

```bash
git clone --recurse-submodules <repository-url>
cd anchor
git checkout <release-tag>
```

### Rust

Run the SDK tests from its directory:

```bash
cd anchor-sdk/rust
cargo test
```

The Rust crate is named `anchor-sdk`. It uses Quinn for QUIC and generates its
Protocol v1 types during the build.

### Kotlin / Android

The Kotlin SDK is a standalone Gradle Android library. It requires Android SDK
and NDK tooling, `protoc`, and a device or emulator for transport tests.

```bash
cd anchor-sdk/kotlin
../../anchor/gradlew test
```

Build the MsQuic native library for an Android ABI before transport tests:

```bash
git submodule update --init src/main/cpp/third_party/msquic
git -C src/main/cpp/third_party/msquic submodule update --init submodules/quictls
./scripts/build-msquic-android.sh "$ANDROID_NDK_ROOT" arm64-v8a
```

The script writes local `.so` build outputs under `src/main/jniLibs/`. These
files are not stored in the repository. The Kotlin SDK requires Android API
30 or newer.

## Local Rust ↔ Kotlin interop test

`anchor-sdk/rust/examples/session_server.rs` and
`anchor-sdk/kotlin/src/androidTest/kotlin/org/anchor/sdk/AnchorSessionInstrumentedTest.kt`
exercise a real session between the two SDKs — pairing/session handshake, a
typed clipboard capability record round trip, a datagram, and a reliable
stream — over actual mTLS QUIC, not shared fixtures decoded independently.
This is a manual, local check; it is not run in CI (it needs an Android
emulator/device and a native MsQuic build, which CI does not provision for
this).

1. Build MsQuic for an emulator/device ABI and start the Rust fixture:

   ```bash
   cd anchor-sdk/rust
   cargo run --example session_server
   ```

   `ANCHOR_TEST_BIND_ADDR` and `ANCHOR_TEST_CERT_DIR` override the default
   bind address (`0.0.0.0:4452`) and mTLS certificate directory (the
   fixtures already checked in under
   `anchor-sdk/kotlin/src/androidTest/assets/mtls/`) if you need to run it
   from a different working directory or against a different address.

2. Run the instrumented test against an emulator (`10.0.2.2` is its default
   host alias for the machine running the Rust fixture) or a physical device:

   ```bash
   cd anchor
   ./gradlew :anchorSdk:connectedAndroidTest \
     -Pandroid.testInstrumentationRunnerArguments.anchor.test.host=<host>
   ```

   Omit the `-P` argument on an emulator to use the `10.0.2.2` default.

3. To check that a peer on a newer, unsupported protocol minor is rejected
   cleanly (see [`protocol/VERSIONING.md`](protocol/VERSIONING.md)), restart
   the Rust fixture with `ANCHOR_TEST_FORCE_MINOR=1` set and run only
   `connectFailsWhenPeerMinorIsAheadOfOurs` (this mode sends a single raw
   preface and never completes a real session, so it cannot be combined with
   step 2 in the same run).

### Swift

Swift development requires macOS/Xcode for the platform transport work. The
package currently contains the platform-neutral wire layer and tests:

```bash
cd anchor-sdk/swift
swift test
```

Generate Swift Protocol v1 types when needed:

```bash
bash scripts/generate-protobuf.sh
```

## Protocol reference

- [Protocol v1 schemas and rules](protocol/README.md)
- [Transport contract](../docs/developer/protocol/transport-contract.md)
- [Protocol v1 fixtures](protocol/fixtures/v1/README.md)

All SDKs must follow the canonical schemas and fixtures. Do not add JSON or
TCP side channels to an Anchor session.
