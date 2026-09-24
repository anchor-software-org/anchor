import CryptoKit
import Foundation
import Network
import Security

enum AnchorPeerDatagramLimit {
    static func accepts(payloadBytes: Int, usableFrameSize: Int) -> Bool {
        payloadBytes > 0 && (usableFrameSize == 0 || payloadBytes <= usableFrameSize)
    }
}

/// A reliable stream that belongs to a modern multiplexed QUIC connection.
@available(macOS 26.0, iOS 26.0, *)
public final class AnchorModernNetworkReliableStream: AnchorReliableStream, @unchecked Sendable {
    public let streamIdentifier: UInt64
    private let stream: Network.QUIC.Stream<Network.QUICStream>

    fileprivate init(_ stream: Network.QUIC.Stream<Network.QUICStream>) {
        self.stream = stream
        self.streamIdentifier = stream.streamID
    }

    public func receive(maximumLength: Int = 1_048_576) async throws -> Data {
        let message = try await stream.receive(atLeast: 1, atMost: maximumLength)
        if message.content.isEmpty, message.metadata.endOfStream {
            throw AnchorNetworkTransportError.connectionClosed
        }
        return message.content
    }

    public func send(_ bytes: Data) async throws {
        try await stream.send(bytes)
    }

    public func finish() async throws {
        try await stream.send(Data(), endOfStream: true)
    }
}

/// Multiplexed QUIC transport built on Apple's iOS/macOS 26 Network API.
///
/// This type is deliberately separate from the iOS 17 `NWConnection` adapter:
/// old OS releases keep their proven control-only path, while systems with the
/// new API can open canonical secondary QUIC streams for Sideboat and files.
@available(macOS 26.0, iOS 26.0, *)
public final class AnchorModernNetworkQuicTransport: AnchorNetworkQuicTransport, @unchecked Sendable {
    private enum PeerVerification {
        case pinned(Data)
        case pairing
    }

    private let clientIdentity: SecIdentity
    private let stateLock = NSLock()
    private var connection: NetworkConnection<Network.QUIC>?
    private var controlStream: Network.QUIC.Stream<Network.QUICStream>?
    private var datagramChannel: Network.QUIC.Datagrams<Network.QUICDatagram>?
    private var reliableStreams = [AnchorModernNetworkReliableStream]()
    private var pendingInboundStreams = [AnchorModernNetworkReliableStream]()
    private var inboundStreamWaiters = [CheckedContinuation<any AnchorReliableStream, Error>]()
    private var inboundStreamsTask: Task<Void, Never>?
    private var verification: PeerVerification = .pairing
    private var peerCertificate = Data()
    private var terminalError: Error?

    public init(clientIdentity: SecIdentity) {
        self.clientIdentity = clientIdentity
    }

    deinit { close() }

    public func connect(host: String, port: UInt16, serverName: String,
                        expectedCertificateFingerprint: Data) async throws {
        guard expectedCertificateFingerprint.count == SHA256.Digest.byteCount else {
            throw AnchorNetworkTransportError.missingCertificatePin
        }
        try await connect(
            host: host,
            port: port,
            verification: .pinned(expectedCertificateFingerprint)
        )
    }

    public func connectForPairing(host: String, port: UInt16, serverName: String) async throws -> Data {
        try await connect(host: host, port: port, verification: .pairing)
        let certificate = withState { peerCertificate }
        guard !certificate.isEmpty else {
            close()
            throw AnchorNetworkTransportError.missingPeerCertificate
        }
        return certificate
    }

    private func connect(host: String, port: UInt16,
                         verification: PeerVerification) async throws {
        guard port != 0, let networkPort = NWEndpoint.Port(rawValue: port) else {
            throw AnchorNetworkTransportError.invalidPort
        }
        let canStart = withState {
            guard connection == nil else { return false }
            self.verification = verification
            peerCertificate = Data()
            terminalError = nil
            return true
        }
        guard canStart else { throw AnchorNetworkTransportError.alreadyConnected }
        guard let identity = sec_identity_create(clientIdentity) else {
            throw AnchorNetworkTransportError.connectionFailed("could not load client identity")
        }

        let quic = Network.QUIC(alpn: [AnchorV1.alpn])
            .maxDatagramFrameSize(AnchorVideoFrameHeader.datagramBytes)
            .tls
            .localIdentity(identity)
            .tls
            .certificateValidator { [weak self] _, trust in
                self?.isExpectedPeer(trust) ?? false
            }
        let endpoint = NWEndpoint.hostPort(host: NWEndpoint.Host(host), port: networkPort)
        let connection = NetworkConnection<Network.QUIC>(to: endpoint) { quic }
        withState { self.connection = connection }

        do {
            try await waitForReady(connection)
            startReceivingInboundStreams(on: connection)
            let control = try await connection.openStream(directionality: .bidirectional)
            withState { controlStream = control }
            NSLog("[anchor.sdk.quic] modern control stream ready: %llu", control.streamID)
        } catch {
            close()
            throw AnchorNetworkTransportError.connectionFailed(error.localizedDescription)
        }
    }

    public func openReliableStream() async throws -> any AnchorReliableStream {
        guard let connection = withState({ connection }) else {
            throw closedOrTerminalError()
        }
        let stream = try await connection.openStream(directionality: .bidirectional)
        let reliable = AnchorModernNetworkReliableStream(stream)
        withState { reliableStreams.append(reliable) }
        return reliable
    }

    public func receiveReliableStream() async throws -> any AnchorReliableStream {
        if let stream = withState({
            pendingInboundStreams.isEmpty ? nil : pendingInboundStreams.removeFirst()
        }) {
            return stream
        }
        if let error = withState({ terminalError }) { throw error }
        return try await withCheckedThrowingContinuation { continuation in
            stateLock.lock()
            if !pendingInboundStreams.isEmpty {
                let stream = pendingInboundStreams.removeFirst()
                stateLock.unlock()
                continuation.resume(returning: stream)
            } else if let terminalError {
                stateLock.unlock()
                continuation.resume(throwing: terminalError)
            } else {
                inboundStreamWaiters.append(continuation)
                stateLock.unlock()
            }
        }
    }

    public func sendControl(_ bytes: Data) async throws {
        guard let stream = withState({ controlStream }) else { throw closedOrTerminalError() }
        try await stream.send(bytes)
    }

    public func receiveControl() async throws -> Data {
        guard let stream = withState({ controlStream }) else { throw closedOrTerminalError() }
        let message = try await stream.receive(
            atLeast: 1,
            atMost: AnchorV1.maxControlRecordBytes + 16
        )
        if message.content.isEmpty, message.metadata.endOfStream {
            throw AnchorNetworkTransportError.connectionClosed
        }
        return message.content
    }

    public func sendDatagram(_ bytes: Data) async throws {
        guard !bytes.isEmpty, bytes.count <= AnchorVideoFrameHeader.datagramBytes else {
            throw AnchorNetworkTransportError.connectionFailed("invalid QUIC datagram size")
        }
        let datagrams = try await resolvedDatagramChannel()
        let usableFrameSize = withState({ connection?.usableDatagramFrameSize ?? 0 })
        guard AnchorPeerDatagramLimit.accepts(
            payloadBytes: bytes.count,
            usableFrameSize: usableFrameSize
        ) else {
            throw AnchorNetworkTransportError.connectionFailed(
                "peer QUIC datagram limit is \(usableFrameSize) bytes"
            )
        }
        try await datagrams.send(bytes)
    }

    public func receiveDatagram() async throws -> Data {
        let datagrams = try await resolvedDatagramChannel()
        return try await datagrams.receive().content
    }

    public func close() {
        stateLock.lock()
        // The modern API ties channel lifetime to its owning references. Drop
        // child streams first, then the connection, and make later operations
        // observe a deterministic terminal error.
        reliableStreams.removeAll()
        pendingInboundStreams.removeAll()
        let waiters = inboundStreamWaiters
        inboundStreamWaiters.removeAll()
        inboundStreamsTask?.cancel()
        inboundStreamsTask = nil
        controlStream = nil
        datagramChannel = nil
        connection = nil
        terminalError = AnchorNetworkTransportError.connectionClosed
        stateLock.unlock()
        waiters.forEach { $0.resume(throwing: AnchorNetworkTransportError.connectionClosed) }
    }

    private func startReceivingInboundStreams(on connection: NetworkConnection<Network.QUIC>) {
        inboundStreamsTask = Task { [weak self] in
            guard let self else { return }
            do {
                try await connection.inboundStreams { stream in
                    self.enqueueInboundStream(AnchorModernNetworkReliableStream(stream))
                }
            } catch is CancellationError {
                return
            } catch {
                self.failInboundStreams(error)
            }
        }
    }

    private func resolvedDatagramChannel() async throws
        -> Network.QUIC.Datagrams<Network.QUICDatagram> {
        if let channel = withState({ datagramChannel }) { return channel }
        guard let connection = withState({ connection }) else {
            throw closedOrTerminalError()
        }
        let channel = try await connection.datagrams
        return withState {
            if let existing = datagramChannel { return existing }
            datagramChannel = channel
            return channel
        }
    }

    private func enqueueInboundStream(_ stream: AnchorModernNetworkReliableStream) {
        stateLock.lock()
        reliableStreams.append(stream)
        let waiter = inboundStreamWaiters.isEmpty ? nil : inboundStreamWaiters.removeFirst()
        if waiter == nil { pendingInboundStreams.append(stream) }
        stateLock.unlock()
        waiter?.resume(returning: stream)
    }

    private func failInboundStreams(_ error: Error) {
        stateLock.lock()
        let waiters = inboundStreamWaiters
        inboundStreamWaiters.removeAll()
        terminalError = error
        stateLock.unlock()
        waiters.forEach { $0.resume(throwing: error) }
    }

    private func waitForReady(_ connection: NetworkConnection<Network.QUIC>) async throws {
        try await withCheckedThrowingContinuation { continuation in
            let waiter = AnchorNetworkReadyWaiter(continuation)
            connection.onStateUpdate { _, state in
                NSLog("[anchor.sdk.quic] modern connection state: %@", String(describing: state))
                switch state {
                case .ready:
                    waiter.resume()
                case .failed(let error):
                    waiter.resume(throwing: error)
                case .cancelled:
                    waiter.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                default:
                    break
                }
            }
            _ = connection.start()
        }
    }

    private func isExpectedPeer(_ trust: sec_trust_t) -> Bool {
        let peerTrust = sec_trust_copy_ref(trust).takeRetainedValue()
        guard let certificate = (SecTrustCopyCertificateChain(peerTrust) as? [SecCertificate])?.first else {
            return false
        }
        let certificateData = SecCertificateCopyData(certificate) as Data
        let fingerprint = Data(SHA256.hash(data: certificateData))
        stateLock.lock()
        peerCertificate = certificateData
        let verification = self.verification
        stateLock.unlock()
        switch verification {
        case .pinned(let expected): return fingerprint == expected
        case .pairing: return true
        }
    }

    private func closedOrTerminalError() -> Error {
        withState { terminalError ?? AnchorNetworkTransportError.connectionClosed }
    }

    private func withState<T>(_ body: () -> T) -> T {
        stateLock.lock()
        defer { stateLock.unlock() }
        return body()
    }
}

@available(macOS 26.0, iOS 26.0, *)
private final class AnchorNetworkReadyWaiter: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?

    init(_ continuation: CheckedContinuation<Void, Error>) {
        self.continuation = continuation
    }

    func resume() {
        take()?.resume()
    }

    func resume(throwing error: Error) {
        take()?.resume(throwing: error)
    }

    private func take() -> CheckedContinuation<Void, Error>? {
        lock.lock()
        defer { lock.unlock() }
        let result = continuation
        continuation = nil
        return result
    }
}
