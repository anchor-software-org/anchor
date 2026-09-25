import CryptoKit
import Foundation
import Network
import Security

/// Errors surfaced by the Apple-platform QUIC adapter before protocol records
/// reach `AnchorSession`.
public enum AnchorNetworkTransportError: Error, Equatable {
    case alreadyConnected
    case invalidPort
    case missingCertificatePin
    case certificatePinMismatch
    case missingPeerCertificate
    case connectionFailed(String)
    case connectionClosed
    case missingControlStream
    case missingStreamIdentifier
}

/// One reliable byte stream opened inside an authenticated QUIC tunnel.
/// The owner negotiates its purpose on the Anchor control stream before using
/// the payload bytes.
public final class AnchorNetworkReliableStream: AnchorReliableStream, @unchecked Sendable {
    public let streamIdentifier: UInt64
    private let connection: NWConnection

    fileprivate init(streamIdentifier: UInt64, connection: NWConnection) {
        self.streamIdentifier = streamIdentifier
        self.connection = connection
    }

    public func receive(maximumLength: Int = 1_048_576) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            connection.receive(minimumIncompleteLength: 1, maximumLength: maximumLength) {
                content, _, isComplete, error in
                if let error {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                } else if let content, !content.isEmpty {
                    continuation.resume(returning: content)
                } else if isComplete {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                } else {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                }
            }
        }
    }

    public func send(_ bytes: Data) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: bytes, completion: .contentProcessed { error in
                if let error { continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription)) }
                else { continuation.resume() }
            })
        }
    }

    public func finish() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: Data(), contentContext: .finalMessage, isComplete: true,
                            completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                } else {
                    continuation.resume()
                }
            })
        }
    }

    public func cancel() { connection.cancel() }
}

/// `Network.framework` implementation of Anchor's QUIC boundary.
///
/// The initial Protocol v1 migration uses a direct QUIC `NWConnection` for the
/// ordered control stream. Datagram flows remain unavailable until Apple's
/// multiplex-group path is enabled and validated against the desktop.
///
/// This transport is for an already paired peer. Pairing bootstrap deliberately
/// belongs to a separate, constrained flow; callers must never pass an empty
/// pin for a normal session.
public final class AnchorNetworkFrameworkQuicTransport: AnchorNetworkQuicTransport, @unchecked Sendable {
    private enum PeerVerification {
        case pinned(Data)
        case pairing
    }
    private let clientIdentity: SecIdentity
    private let queue = DispatchQueue(label: "org.anchor.sdk.network-quic")
    private let stateLock = NSLock()

    private var group: NWConnectionGroup?
    private var controlConnection: NWConnection?
    private var reliableStreams = [AnchorNetworkReliableStream]()
    private var expectedCertificateFingerprint = Data()
    private var peerVerification: PeerVerification = .pairing
    private var peerCertificate = Data()
    private var datagramQueue = [Data]()
    private var datagramWaiter: CheckedContinuation<Data, Error>?
    private var terminalError: Error?

    public init(clientIdentity: SecIdentity) {
        self.clientIdentity = clientIdentity
    }

    deinit {
        close()
    }

    public func connect(host: String, port: UInt16, serverName: String,
                        expectedCertificateFingerprint: Data) async throws {
        guard port != 0 else { throw AnchorNetworkTransportError.invalidPort }
        // A SHA-256 digest of the peer leaf certificate is the normal-session
        // pin. Rejecting an absent pin prevents accidental trust-on-first-use.
        guard expectedCertificateFingerprint.count == SHA256.Digest.byteCount else {
            throw AnchorNetworkTransportError.missingCertificatePin
        }

        try await connect(
            host: host,
            port: port,
            serverName: serverName,
            verification: .pinned(expectedCertificateFingerprint)
        )
    }

    /// Opens the constrained pairing-only connection used for manual/direct
    /// enrollment. The peer leaf is captured but is not trusted persistently;
    /// callers must compare it with PairingApprove and store it only after the
    /// desktop user approves the request.
    public func connectForPairing(host: String, port: UInt16, serverName: String) async throws -> Data {
        guard port != 0 else { throw AnchorNetworkTransportError.invalidPort }
        try await connect(host: host, port: port, serverName: serverName, verification: .pairing)
        let certificate = withState { peerCertificate }
        guard !certificate.isEmpty else {
            close()
            throw AnchorNetworkTransportError.missingPeerCertificate
        }
        return certificate
    }

    private func connect(host: String, port: UInt16, serverName: String,
                         verification: PeerVerification) async throws {
        let quicOptions = NWProtocolQUIC.Options(alpn: [AnchorV1.alpn])
        quicOptions.direction = .bidirectional
        quicOptions.maxDatagramFrameSize = AnchorVideoFrameHeader.datagramBytes
        sec_protocol_options_set_tls_server_name(quicOptions.securityProtocolOptions, serverName)
        guard let identity = sec_identity_create(clientIdentity) else {
            throw AnchorNetworkTransportError.connectionFailed("could not load client identity")
        }
        sec_protocol_options_set_local_identity(quicOptions.securityProtocolOptions, identity)
        sec_protocol_options_set_verify_block(
            quicOptions.securityProtocolOptions,
            { [weak self] _, trust, complete in
                complete(self?.isExpectedPeer(trust) ?? false)
            },
            queue
        )

        let endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(host),
            port: NWEndpoint.Port(rawValue: port)!
        )
        let started = withState {
            guard group == nil, controlConnection == nil else { return false }
            if case .pinned(let fingerprint) = verification {
                self.expectedCertificateFingerprint = fingerprint
            } else {
                self.expectedCertificateFingerprint = Data()
            }
            self.peerVerification = verification
            self.peerCertificate = Data()
            terminalError = nil
            datagramQueue.removeAll(keepingCapacity: true)
            return true
        }
        guard started else {
            throw AnchorNetworkTransportError.alreadyConnected
        }

        do {
            // Keep the proven single-stream path as the production transport.
            // NWConnectionGroup is isolated behind openReliableStream until a
            // physical-device probe proves its tunnel lifecycle; swapping the
            // main session to an unverified group would regress every feature.
            let connection = NWConnection(to: endpoint, using: NWParameters(quic: quicOptions))
            withState { controlConnection = connection }
            try await waitForReady(connection)
        } catch {
            close()
            throw error
        }
    }

    /// Open another client-initiated bidirectional stream on the same pinned
    /// QUIC tunnel and return its canonical QUIC stream ID.
    public func openReliableStream() async throws -> any AnchorReliableStream {
        guard let group = withState({ group }) else {
            throw AnchorNetworkTransportError.connectionFailed("multiplexed QUIC tunnel is unavailable")
        }
        guard let connection = NWConnection(from: group) else {
            throw AnchorNetworkTransportError.missingControlStream
        }
        try await waitForReady(connection)
        guard let metadata = connection.metadata(definition: NWProtocolQUIC.definition) as? NWProtocolQUIC.Metadata else {
            connection.cancel()
            throw AnchorNetworkTransportError.missingStreamIdentifier
        }
        let stream = AnchorNetworkReliableStream(
            streamIdentifier: metadata.streamIdentifier,
            connection: connection
        )
        withState { reliableStreams.append(stream) }
        return stream
    }

    public func receiveReliableStream() async throws -> any AnchorReliableStream {
        throw AnchorNetworkTransportError.connectionFailed("inbound multiplexed QUIC streams are unavailable")
    }

    public func sendControl(_ bytes: Data) async throws {
        guard let controlConnection else { throw closedOrTerminalError() }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            controlConnection.send(content: bytes, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                } else {
                    continuation.resume()
                }
            })
        }
    }

    public func sendDatagram(_ bytes: Data) async throws {
        throw AnchorNetworkTransportError.connectionFailed("QUIC datagram flows are not enabled")
    }

    public func receiveControl() async throws -> Data {
        guard let controlConnection else { throw closedOrTerminalError() }
        return try await withCheckedThrowingContinuation { continuation in
            controlConnection.receive(
                minimumIncompleteLength: 1,
                maximumLength: AnchorV1.maxControlRecordBytes + 16
            ) { content, _, isComplete, error in
                if let error {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                } else if let content, !content.isEmpty {
                    continuation.resume(returning: content)
                } else if isComplete {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                } else {
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                }
            }
        }
    }

    public func receiveDatagram() async throws -> Data {
        if let datagram = withState({
            datagramQueue.isEmpty ? nil : datagramQueue.removeFirst()
        }) {
            return datagram
        }
        if let terminalError = withState({ terminalError }) {
            throw terminalError
        }

        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, Error>) in
            stateLock.lock()
            if !datagramQueue.isEmpty {
                let datagram = datagramQueue.removeFirst()
                stateLock.unlock()
                continuation.resume(returning: datagram)
            } else if let terminalError {
                stateLock.unlock()
                continuation.resume(throwing: terminalError)
            } else if datagramWaiter != nil {
                stateLock.unlock()
                continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed("concurrent datagram receive"))
            } else {
                datagramWaiter = continuation
                stateLock.unlock()
            }
        }
    }

    public func close() {
        stateLock.lock()
        let group = self.group
        let controlConnection = self.controlConnection
        let reliableStreams = self.reliableStreams
        self.group = nil
        self.controlConnection = nil
        self.reliableStreams.removeAll()
        let waiter = datagramWaiter
        datagramWaiter = nil
        datagramQueue.removeAll(keepingCapacity: false)
        terminalError = AnchorNetworkTransportError.connectionClosed
        stateLock.unlock()

        waiter?.resume(throwing: AnchorNetworkTransportError.connectionClosed)
        reliableStreams.forEach { $0.cancel() }
        controlConnection?.cancel()
        group?.cancel()
    }

    private func waitForReady(_ group: NWConnectionGroup) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            group.stateUpdateHandler = { state in
                NSLog("[anchor.sdk.quic] group state: %@", String(describing: state))
                switch state {
                case .ready:
                    group.stateUpdateHandler = nil
                    continuation.resume()
                case .failed(let error):
                    group.stateUpdateHandler = nil
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                case .cancelled:
                    group.stateUpdateHandler = nil
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                default:
                    break
                }
            }
            group.start(queue: queue)
        }
    }

    private func waitForReady(_ connection: NWConnection) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.stateUpdateHandler = { state in
                NSLog("[anchor.sdk.quic] control stream state: %@", String(describing: state))
                switch state {
                case .ready:
                    connection.stateUpdateHandler = nil
                    continuation.resume()
                case .failed(let error):
                    connection.stateUpdateHandler = nil
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionFailed(error.localizedDescription))
                case .cancelled:
                    connection.stateUpdateHandler = nil
                    continuation.resume(throwing: AnchorNetworkTransportError.connectionClosed)
                default:
                    break
                }
            }
            connection.start(queue: queue)
        }
    }

    private func installDatagramReceiver(on group: NWConnectionGroup) {
        group.setReceiveHandler(
            maximumMessageSize: AnchorVideoFrameHeader.datagramBytes,
            rejectOversizedMessages: true
        ) { [weak self] _, content, _ in
            guard let content else { return }
            self?.stateLock.lock()
            let waiter = self?.datagramWaiter
            self?.datagramWaiter = nil
            if waiter == nil {
                self?.datagramQueue.append(content)
            }
            self?.stateLock.unlock()
            waiter?.resume(returning: content)
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
        let expected = expectedCertificateFingerprint
        let verification = peerVerification
        stateLock.unlock()
        switch verification {
        case .pinned:
            return fingerprint == expected
        case .pairing:
            return true
        }
    }

    private func fail(_ error: Error) {
        stateLock.lock()
        terminalError = error
        let waiter = datagramWaiter
        datagramWaiter = nil
        stateLock.unlock()
        waiter?.resume(throwing: error)
    }

    private func closedOrTerminalError() -> Error {
        stateLock.lock()
        defer { stateLock.unlock() }
        return terminalError ?? AnchorNetworkTransportError.connectionClosed
    }

    private func withState<T>(_ body: () -> T) -> T {
        stateLock.lock()
        defer { stateLock.unlock() }
        return body()
    }
}
