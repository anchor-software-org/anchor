# AnchorSDK for Swift

This package contains the Swift wire-format helpers for Anchor Protocol v1.
It is preview software. It is not the networking layer for the iOS app.

## What is included

The package provides:

- `AnchorControlFramer` for the `ANCR` control stream preface and length-delimited records.
- `AnchorSession` for ordered control records and datagrams through a supplied transport.
- `AnchorVideoFrameCodec` for `ANFR` screen and camera datagram fragmentation and decoding.
- Protocol v1 ALPN, capability names, and protobuf type-URL constants.
- `AnchorQuicTransport`, the interface an app can implement with its QUIC library.

The package does not provide a QUIC implementation, pairing UI, media decoding,
or feature-specific application code. Protobuf message types are generated from
the shared files in `anchor-sdk/protocol`; they are not included in this target.

## Current status

The wire helpers are implemented and covered by unit tests. The package can be
used to build a host-side protocol adapter if the host supplies all missing
pieces.

The iOS app does not use this package yet. It still uses the legacy
`anchor-ios/Anchor/Plugins/NetworkPlugin.swift` TCP implementation. There is no
working `Network.framework` QUIC adapter in this repository, and the iOS app
has not been migrated to the Protocol v1 session and generated message types.
Do not treat this package as a ready-made iOS client SDK.

## Requirements

- Swift 5.9 or newer.
- iOS 17 or newer, or macOS 14 or newer, when building for Apple platforms.
- Xcode for Apple-platform builds.
- `protoc` and a matching `protoc-gen-swift` when generating protobuf types.

The Linux development environment in this repository does not include Swift or
Xcode, so Swift builds are not verified there.

## Run the package tests

From the repository root:

```sh
cd anchor-sdk/swift
swift test
```

The tests cover control framing, malformed input, capability constants, and
video datagram fragmentation and decoding.

## Generate Swift protobuf types

Install `protoc` and a compatible `protoc-gen-swift`, then run this command from
the repository root:

```sh
bash anchor-sdk/swift/scripts/generate-protobuf.sh
```

Generated files are written to
`anchor-sdk/swift/Sources/AnchorSDK/Generated`. The generation script does not
install either tool and does not configure a SwiftProtobuf package dependency.
Add the generated sources and the SwiftProtobuf runtime to the host app as
required by the version of `protoc-gen-swift` being used.

## Use the wire helpers

Implement `AnchorQuicTransport` with a QUIC library, then create a session:

```swift
let session = AnchorSession(transport: transport)

try await session.connect(
    host: host,
    port: port,
    serverName: serverName,
    expectedCertificateFingerprint: certificateFingerprint
)

try await session.sendEnvelope(serializedControlEnvelope)
let bytes = try await session.receiveEnvelope()
```

`sendEnvelope` and `receiveEnvelope` exchange serialized
`anchor.v1.ControlEnvelope` bytes. Decode and construct those protobuf messages
in the host app. The first outgoing envelope includes the `ANCR` preface; later
envelopes use length-delimited records.

For screen or camera payloads, call `AnchorVideoFrameCodec.fragment` before
sending datagrams and `AnchorVideoFrameCodec.decode` after receiving them.
The codec uses the shared 1100-byte datagram size and limits a frame to 8 MiB.

## Scope of the preview

This package is suitable for experimenting with Protocol v1 wire compatibility.
It is not a drop-in replacement for the current iOS networking code. The
following work is still required before it can support the iOS app:

1. Implement and test a `Network.framework` QUIC transport.
2. Connect that transport to pairing, certificate validation, and reconnect
   handling.
3. Generate and integrate the Protocol v1 SwiftProtobuf messages.
4. Migrate iOS feature plugins from the legacy TCP message flow.
5. Test the complete app on supported iOS devices.
