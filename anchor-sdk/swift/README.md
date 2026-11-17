# AnchorSDK for Swift

This package contains the Swift wire-format helpers for Anchor Protocol v1.
It is preview software. It is not the networking layer for the iOS app.

## What is included

The package provides:

- `AnchorControlFramer` for the `ANCR` control stream preface and length-delimited records.
- `AnchorSession` for ordered control records and datagrams through a supplied transport.
- `AnchorVideoFrameCodec` for `ANFR` screen and camera datagram fragmentation and decoding.
- Protocol v1 ALPN, capability names, type-URL constants, and generated
  SwiftProtobuf message types.
- `AnchorQuicTransport`, the interface an app can implement with its QUIC library.

The package does not provide a QUIC implementation, pairing UI, media decoding,
or feature-specific application code.

## Current status

The wire helpers are implemented and covered by unit tests. The package can be
used to build a host-side protocol adapter if the host supplies all missing
pieces.

The package now includes an Apple-platform `Network.framework` QUIC adapter
for a pinned, already-paired peer. The iOS app still uses the legacy
`anchor-ios/Anchor/Plugins/NetworkPlugin.swift` TCP implementation and has not
yet been migrated to the Protocol v1 session or feature adapters. Do not treat
this package as a ready-made iOS client SDK.

## Requirements

- Swift 5.9 or newer.
- iOS 17 or newer, or macOS 14 or newer, when building for Apple platforms.
- Xcode for Apple-platform builds.
- `protoc` and a matching `protoc-gen-swift` when regenerating protobuf types.

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
`anchor-sdk/swift/Sources/AnchorSDK/Generated` and are checked in. The package
declares its SwiftProtobuf runtime dependency; regenerate and commit these
files whenever the canonical schemas change.

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

1. Connect the pinned transport to pairing, certificate identity storage, and
   reconnect handling.
2. Migrate iOS feature plugins from the legacy TCP message flow.
3. Test the complete app on supported iOS devices and against the desktop
   listener.
