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

public struct AnchorQuicConfiguration: Equatable {
    public let alpn: String
    public let maxControlRecordBytes: Int

    public init(alpn: String = AnchorV1.alpn,
                maxControlRecordBytes: Int = AnchorV1.maxControlRecordBytes) {
        self.alpn = alpn
        self.maxControlRecordBytes = maxControlRecordBytes
    }
}
