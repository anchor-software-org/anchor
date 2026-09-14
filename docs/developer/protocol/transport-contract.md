# Anchor Protocol v1 transport contract

This page defines the transport rules used by the Anchor SDKs.

## Connection and security

Anchor sessions use QUIC over UDP with TLS 1.3 and the `anchor/1` ALPN. There
is no TCP or raw-UDP fallback.

The platform pairing store decides which peer certificates are trusted. A
certificate must match the stored pairing pin before the SDK exposes a normal
session to the application. A `node_id`, device name, endpoint ID, or pairing
QR code is not an identity proof.

Pairing is a separate connection phase. It exchanges `PairingHello` and an
explicit approve or reject decision. A pairing connection cannot open
capabilities.

## Control stream

Each normal session has one bidirectional control stream. The first bytes are:

```text
ANCR | major (1 byte) | minor (1 byte)
```

Each following record is an unsigned-varint length followed by one protobuf
`anchor.v1.ControlEnvelope`. A record is limited to 1 MiB. The Rust and Kotlin
SDKs enforce these limits before decoding or allocating the protobuf message.

The peers exchange `SessionHello`, then `SessionReady`. Capabilities are not
available until both messages have been accepted. A request sets a non-zero
`request_id`; its reply sets `response_to` to that ID. Notifications set both
fields to zero. An envelope cannot set both fields.

The control stream carries session messages, capability negotiation, typed
capability records, stream bindings, datagram-flow negotiation, ping/pong, and
close or error messages.

## Capabilities

`SessionHello` advertises endpoints. Each endpoint lists capabilities with:

- a reverse-DNS endpoint ID;
- a capability name and major version;
- the exact protobuf type URLs accepted by that capability;
- whether the capability supports datagrams.

An application opens a capability only when the peer advertises the requested
endpoint, name, and major version. The peer must explicitly accept the
request. The SDK rejects records whose type URL was not advertised.

Capability records are protobuf payloads carried inside `CapabilityRecord` on
the reliable control stream. The negotiated type URL identifies their schema;
there is no generic JSON or untyped application channel.

## Reliable streams

An application may open a QUIC bidirectional stream for a negotiated capability
type URL. Before sending application bytes, it sends `StreamOpen` and waits for
`StreamOpened`. The stream ID, capability session ID, and payload type URL are
bound in that exchange.

QUIC streams are ordered, reliable, flow-controlled byte streams. They do not
preserve message boundaries, so a capability that sends multiple messages must
define its own framing. Closing or resetting the QUIC stream is the transport
completion signal; `StreamClose` is a protocol notice.

## Datagram flows

Datagrams require both a capability advertisement and an explicit
`DatagramFlowOpen`/`DatagramFlowOpened` exchange. Each flow has a capability
session ID, flow ID, and negotiated payload type URL.

QUIC datagrams are unordered and unreliable. They are connection-scoped, so a
receiver must inspect the capability's payload header and discard packets for
other flows. Datagram payloads are not protobuf-framed; each capability defines
its bounded binary format, sequencing, and recovery behavior.

Send queues are bounded. A full queue is an observable send failure, not an
instruction to evict old data silently. Applications should drop or refresh
replaceable data such as live media when the queue is full.

## Session loss

QUIC close and network failure end the session and invalidate its capability,
stream, and datagram-flow handles. Reconnect, state replay, and transfer
resumption are host-application policy; the transport does not retry
non-idempotent records automatically.

## Non-goals

- TCP compatibility or a raw-UDP production mode.
- Generic JSON, protobuf `Any`, maps, or anonymous application bytes in the
  control plane.
- Implicit capability approval based only on authentication.
- Trust based on a claimed node ID, device name, or endpoint ID.
- Reliable delivery, ordering, or retransmission for datagrams.
- Unbounded queues or silent dropping by the SDK's transport layer.
- Automatic replay of capability records after reconnect.
