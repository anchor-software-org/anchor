# Anchor Kotlin SDK

The Kotlin SDK is an Android library for connecting to an Anchor peer and
exchanging Anchor Protocol v1 messages. It includes protobuf types, pairing
state, certificate-pin checks, the authenticated `AnchorSession`, reliable
QUIC streams, datagrams, and helpers for clipboard, camera, commands, device
status, files, input, media, notifications, screen sharing, SMS, and video
frames.

The SDK does not provide application UI, discovery, permissions, or storage
for feature data. Your Android application supplies those pieces.

## Status and compatibility

The SDK targets Android API 30 or newer and Java/Kotlin JVM target 11. The
project uses compile SDK 36, Android Gradle Plugin 8.13.2, and Kotlin 2.0.21.
Anchor Protocol v1 is the wire contract. The Kotlin SDK is consumed from this
repository; it is not published to a package registry.

## Use from a checkout

Clone the repository with submodules:

```bash
git clone --recurse-submodules <anchor-repository-url>
cd anchor
```

In the application's `settings.gradle.kts`:

```kotlin
include(":anchorSdk")
project(":anchorSdk").projectDir = file("anchor-sdk/kotlin")
```

Then add the module:

```kotlin
dependencies {
    implementation(project(":anchorSdk"))
}
```

The build generates Java protobuf sources from `anchor-sdk/protocol/` with
the `protoc` executable on your `PATH`. Use a `protoc` version compatible with
the protobuf Java lite runtime in `build.gradle.kts`.

## Build and test

From `anchor-sdk/kotlin`:

```bash
gradle test
```

The JVM tests cover protocol framing, session state, pairing validation,
identity handling, and transport-independent queues. Android instrumentation
tests require a connected emulator or device:

```bash
gradle connectedAndroidTest
```

## Build the native transport

`MsQuicTransport` uses the pinned MsQuic and QuicTLS sources. If they were not
initialized by the clone, run this from the repository root:

```bash
git submodule update --init anchor-sdk/kotlin/src/main/cpp/third_party/msquic
git -C anchor-sdk/kotlin/src/main/cpp/third_party/msquic \
  submodule update --init submodules/quictls
```

Install CMake, `nproc`, and an Android NDK. The script supports
`arm64-v8a`, `armeabi-v7a`, and `x86_64`, and uses Android API 29 for native
compilation:

```bash
./anchor-sdk/kotlin/scripts/build-msquic-android.sh \
  "$ANDROID_NDK_ROOT" arm64-v8a
```

The script writes `libmsquic.so` and `libanchor_msquic_jni.so` to
`src/main/jniLibs/<abi>/`. These local build outputs are not checked in. Build
each ABI that the application ships.

Gradle packages whatever `jniLibs/` contains without checking it — an APK
built before this step succeeds, then fails at runtime with
`UnsatisfiedLinkError` when `MsQuicRuntime` calls `System.loadLibrary`. If an
installed app crashes on first connect, missing native libraries are the first
thing to check.

## Connect and exchange data

Create a `QuicConnectRequest` with the peer address, server name, the SHA-256
fingerprint stored during pairing, the app certificate and private-key PEM
paths, and the exact self-signed peer certificate PEM path. The trusted
certificate must be the peer leaf certificate, not a CA bundle.

```kotlin
val transport = MsQuicTransport()
val session = AnchorSession.connect(
    transport = transport,
    request = request,
    local = SessionIdentity(
        nodeId = localNodeId,
        displayName = "My phone",
        deviceKindValue = 2,
        endpoints = listOf(ClipboardProtocol.endpointAdvertisement()),
    ),
)

val capability = session.openCapability(
    endpointId = "io.anchor.desktop",
    capabilityName = ClipboardProtocol.CAPABILITY_NAME,
    capabilityMajor = 1,
)
capability.sendRecord(
    ClipboardProtocol.PUBLISH_TYPE_URL,
    ClipboardProtocol.encodeText(localNodeId, revision = 1, text = "Hello"),
)
session.close()
```

`AnchorSession.connect` waits for QUIC and the Protocol v1 hello/ready
exchange. Read incoming work with `nextEvent()`. Opened capabilities provide
record, stream, and datagram operations. Use `AnchorPairingSession` with a
`pairingBootstrap` request for the constrained pre-trust exchange; do not use
a bootstrap connection for normal session traffic.

## Security requirements

Keep certificate and private-key files in app-private storage. A normal
connection requires a 32-byte SHA-256 certificate fingerprint and the matching
peer leaf certificate. The transport verifies the certificate before it emits
`QuicEvent.Connected`. Do not trust a connection based only on an IP address
or hostname.

## Source layout

- `src/main/kotlin/org/anchor/sdk/`: Kotlin API and transport adapter.
- `src/main/cpp/`: JNI bridge.
- `scripts/build-msquic-android.sh`: native build script.
- `../protocol/`: canonical Protocol v1 schemas.

See [`anchor-sdk/protocol/README.md`](../protocol/README.md) for protocol
details and [`docs/developer/protocol/transport-contract.md`](../../docs/developer/protocol/transport-contract.md)
for the shared transport contract.
