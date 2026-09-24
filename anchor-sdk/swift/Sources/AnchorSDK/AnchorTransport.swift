import Foundation

/// Minimal platform-neutral boundary used by the session implementation. The
/// iOS host supplies a Network.framework QUIC implementation; protocol code
/// above it only deals in ordered control bytes and negotiated datagrams.
public protocol AnchorQuicTransport: AnyObject {
    func connect(host: String, port: UInt16, serverName: String,
                 expectedCertificateFingerprint: Data) async throws
    func sendControl(_ bytes: Data) async throws
    func sendDatagram(_ bytes: Data) async throws
    func receiveControl() async throws -> Data
    func receiveDatagram() async throws -> Data
    func close()
}

/// Type-erased ordered byte stream returned by a multiplexed QUIC transport.
public protocol AnchorReliableStream: AnyObject {
    var streamIdentifier: UInt64 { get }
    func receive(maximumLength: Int) async throws -> Data
    func send(_ bytes: Data) async throws
    func finish() async throws
}

/// Operations needed by an Anchor host in addition to the control transport.
/// Pairing remains an explicit phase; secondary streams are bound through the
/// Protocol v1 StreamOpen/StreamOpened exchange before carrying feature data.
public protocol AnchorNetworkQuicTransport: AnchorQuicTransport {
    func connectForPairing(host: String, port: UInt16, serverName: String) async throws -> Data
    func openReliableStream() async throws -> any AnchorReliableStream
    func receiveReliableStream() async throws -> any AnchorReliableStream
}

public struct AnchorQuicConfiguration: Equatable {
    public let alpn: String
    public let maxControlRecordBytes: Int

    public init(alpn: String = AnchorV1.alpn,
                maxControlRecordBytes: Int = AnchorV1.maxControlRecordBytes) {
        self.alpn = alpn
        self.maxControlRecordBytes = maxControlRecordBytes
    }
}
