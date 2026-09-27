# Networking architecture

Anchor uses one authenticated network protocol for desktop and Android
connections. Protocol v1 is carried by QUIC over UDP.

## Network endpoint

The desktop listens on UDP port `5027` on all interfaces. Android discovers
nearby desktops with DNS-SD/mDNS service type `_anchor._udp`. The advertisement
includes the desktop name, device ID, protocol version, and public certificate
material used during pairing.

ADB is used for device detection and setup only. It is not a transport for
Anchor sessions.

## Wired (USB) discovery

On Android a wired link uses USB tethering (RNDIS/NCM): the user enables
tethering while the phone is plugged in, Android runs a DHCP server on the
new `rndis*`/`usb*`/`ncm*` interface, the desktop receives a lease, and port
`5027` is immediately reachable over the wire. On iOS the equivalent is
Personal Hotspot over USB (a `bridge100` interface on the phone) or a Mac
sharing its connection to the device over USB (`en2`+). In every case the
session transport is unchanged.

Neither Android's NSD nor iOS can see the tethered downstream link, so wired
discovery uses a directed UDP probe instead of mDNS:

- The phone sends `ANCHOR_PROBE_V1` to the subnet's broadcast address on UDP
  port `5028`. Android probes only tethered interfaces; iOS — which has no
  mDNS discovery at all — probes every broadcast-capable interface and marks
  replies from tethered ones as wired.
- The desktop probe responder (`device/probe.rs`) answers with
  `ANCHOR_HERE_V1\n` followed by a JSON object carrying the same public
  identity the mDNS advertisement publishes: `device_id`, `device_name`,
  `port`, and the base64url-encoded public certificate used to pin pairing.
- Replies are trusted by packet source address. Certificate material is public
  and still verified against the pairing pin before a session is trusted.

When a desktop is reachable over both Wi-Fi and USB, the clients prefer the
wired address for new connections and reconnects. Wired discovery does not
migrate an already-established session.

## Security and connection setup

Each device has a self-signed certificate and a device ID. Pairing approves a
peer and stores its certificate fingerprint. Later connections accept only the
pinned certificate.

QUIC uses TLS 1.3 and the `anchor/1` ALPN. The session is not ready for
capability traffic until both peers complete the Protocol v1 control
handshake:

1. Verify the pairing invitation and certificate pin.
2. Establish the QUIC connection.
3. Open the control stream and exchange `SessionHello`.
4. Exchange `SessionReady`.
5. Advertise and open capabilities.

Pairing is a separate first-session flow. A received `PairingHello` must be
shown to the user and approved before the peer becomes trusted.

## Data lanes

The protocol has two data lanes:

- The reliable lane uses QUIC bidirectional streams. The control stream carries
  length-delimited Protocol Buffer `ControlEnvelope` records. Capability data
  that must arrive in order uses additional streams opened through
  `StreamOpen` and `StreamOpened` records.
- The transient lane uses negotiated QUIC DATAGRAM packets. Packets can be
  lost or reordered, so callers must use sequence or configuration IDs and
  refresh state when required. Datagram payloads are bounded application
  packets, not Protocol Buffer control records.

Screen and camera video both use the datagram lane when the peer advertises
datagram support. `ANFR` frames are fragmented into bounded datagrams with
sequence numbers and XOR parity groups, so a single lost datagram can be
rebuilt instead of stalling the picture. When the peer does not support
datagrams, screen video falls back to a reliable stream; on the stream each
`ANFR` frame packet is prefixed with a 32-bit little-endian length because
QUIC streams do not retain application message boundaries.

## Capabilities

Peers advertise capabilities during the session handshake. The current desktop
SDK server binds these capabilities:

- device state
- clipboard
- commands
- files
- input
- media
- notifications
- screen sharing
- SMS
- camera

Each capability has a Protocol Buffer schema and a stable type URL. The SDK
checks the advertised capability and type URL before opening a stream or
dispatching a record.

## SDK boundaries

The Rust SDK provides Protocol v1 validation, generated message types, pairing
helpers, session state, and the Quinn QUIC adapter. The Kotlin SDK exposes the
same protocol through its MsQuic transport boundary. Product code owns UI,
discovery, trusted-device storage, permissions, and capability behavior.

See the [Protocol v1 transport contract](../protocol/transport-contract.md) for
wire-level rules and the [Rust SDK](../../../anchor-sdk/README.md) for the
developer-facing SDK entry points.

## Invariants

- Production sessions use QUIC over UDP; there is no TCP fallback.
- TLS certificate pins, not device names or claimed IDs, establish trust.
- Protocol major version mismatches are rejected.
- Control records are Protocol Buffers and are size limited before decoding.
- Capability code must observe stream backpressure and datagram send failure.
- Non-idempotent capability records are not retried implicitly.
