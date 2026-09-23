import Foundation

/// A small actor around the ordered control stream and connection datagrams.
/// Generated SwiftProtobuf messages are intentionally accepted as `Data`; this
/// keeps the session usable with the exact generated types from
/// `anchor-sdk/protocol` without duplicating those types in the SDK.
public actor AnchorSession {
    public let transport: any AnchorQuicTransport
    public private(set) var isConnected = false
    private var framer = AnchorControlFramer()
    private var sentPreface = false

    public init(transport: any AnchorQuicTransport) {
        self.transport = transport
    }

    public func connect(host: String, port: UInt16, serverName: String,
                        expectedCertificateFingerprint: Data) async throws {
        try await transport.connect(
            host: host,
            port: port,
            serverName: serverName,
            expectedCertificateFingerprint: expectedCertificateFingerprint
        )
        isConnected = true
        framer = AnchorControlFramer()
        sentPreface = false
    }

    /// Send one serialized `anchor.v1.ControlEnvelope` on the ordered stream.
    /// The caller is responsible for setting request/reply correlation fields
    /// in the protobuf envelope; framing and size limits are enforced here.
    public func sendEnvelope(_ protobuf: Data) async throws {
        let frame: Data
        if sentPreface {
            frame = try AnchorControlFramer.encode(protobuf)
        } else {
            frame = try AnchorControlFramer.encodeFirst(protobuf)
            sentPreface = true
        }
        try await transport.sendControl(frame)
    }

    /// Sends an ordered batch in one QUIC write. SessionHello and SessionReady
    /// use this so peers never observe a partial initial handshake batch.
    public func sendEnvelopes(_ protobufs: [Data]) async throws {
        var batch = Data()
        for protobuf in protobufs {
            if sentPreface {
                batch.append(try AnchorControlFramer.encode(protobuf))
            } else {
                batch.append(try AnchorControlFramer.encodeFirst(protobuf))
                sentPreface = true
            }
        }
        try await transport.sendControl(batch)
    }

    /// Receive and return the next complete serialized control envelope. QUIC
    /// stream reads may split or coalesce records, so callers must not decode a
    /// single transport callback directly as one protobuf message.
    public func receiveEnvelope() async throws -> Data {
        while true {
            if let payload = try framer.nextPayload() { return payload }
            framer.feed(try await transport.receiveControl())
        }
    }

    public func sendDatagram(_ bytes: Data) async throws {
        try await transport.sendDatagram(bytes)
    }

    public func receiveDatagram() async throws -> Data {
        try await transport.receiveDatagram()
    }

    public func close() {
        isConnected = false
        transport.close()
    }
}
