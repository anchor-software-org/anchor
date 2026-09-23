import Foundation
import Network
import Security
import Combine
import CryptoKit
import QuartzCore
import UIKit
import AnchorSDK
import SwiftProtobuf

/// Dual-channel TCP client plugin — mirrors Android's NetworkPlugin / Rust's DeviceManager.
///
/// Port 5025: JSON control channel (bidirectional plugin messages)
/// Port 5026: H264 video channel (desktop -> iOS)
///
/// Wire format on both channels: [4-byte LE length][payload]
/// Transport: TLS with mutual certificate authentication.
final class NetworkPlugin: Plugin, ObservableObject, @unchecked Sendable {
    private static let udpPacketGapWarnNs: UInt64 = 40_000_000
    private static let udpStatsIntervalNs: UInt64 = 1_000_000_000

    let pluginId = "network"

    private let broker: MessageBroker
    private let videoPlugin: VideoPlugin
    private let trustedStore: TrustedStore
    private let deviceId: String
    private let deviceName: String
    private let deviceKind: ANCHDeviceKind

    // JSON channel (port 5025)
    private var jsonConnection: NWConnection?
    // H264 channel (port 5026)
    private var h264Connection: NWConnection?

    private var cancellables = Set<AnyCancellable>()
    private var isRunning = false
    private var reassembler: UdpFrameReassembler?
    private var udpConnectionReady = false
    private var udpReadySent = false
    private var lastUdpPacketRecvNs: UInt64 = 0
    private var udpPacketsThisWindow: Int = 0
    private var udpStatsWindowStartNs: UInt64 = 0

    // Pairing response — ViewModel sends accept/reject via this continuation
    private var pairingContinuation: CheckedContinuation<Bool, Never>?

    // Captured during TLS verify block (NWConnection doesn't expose peer certs after handshake)
    private var peerCertificateData: Data?

    // Protocol v1 QUIC connection. The legacy fields below remain temporarily
    // for feature-by-feature migration, but connect() no longer opens them.
    private var quicTransport: (any AnchorNetworkQuicTransport)?
    private var quicSession: AnchorSession?
    private var quicTask: Task<Void, Never>?
    private var screenStream: (any AnchorReliableStream)?
    private var screenTask: Task<Void, Never>?
    private var v1LatencyTask: Task<Void, Never>?
    private let v1PingLock = NSLock()
    private var v1PendingPings: [UInt64: UInt64] = [:]
    private var nextPingNonce: UInt64 = 1
    private var quicConnected = false
    private var capabilitySessions: [String: UInt64] = [:]
    private var nextRequestId: UInt64 = 1
    private var nextCapabilitySessionId: UInt64 = 1
    private var clipboardRevision: UInt64 = 0
    private var scrollAccumulator = AnchorScrollAccumulator()

    private static let desktopEndpoint = "io.anchor.desktop"
    private static let clipboardCapability = "org.anchor.clipboard"
    private static let inputCapability = "org.anchor.input"
    private static let screenCapability = "org.anchor.screen"
    private static let clipboardPublishType = "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardPublish"
    private static let inputKeyType = "type.googleapis.com/anchor.v1.capabilities.input.InputKey"
    private static let inputTextType = "type.googleapis.com/anchor.v1.capabilities.input.InputText"
    private static let inputAbsoluteType = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerAbsolute"
    private static let inputRelativeType = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerRelative"
    private static let inputButtonType = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerButton"
    private static let inputScrollType = "type.googleapis.com/anchor.v1.capabilities.input.InputScroll"

    var isConnected: Bool {
        quicConnected
    }

    init(broker: MessageBroker, videoPlugin: VideoPlugin, trustedStore: TrustedStore,
         deviceId: String, deviceName: String, isTablet: Bool) {
        self.broker = broker
        self.videoPlugin = videoPlugin
        self.trustedStore = trustedStore
        self.deviceId = deviceId
        self.deviceName = deviceName
        self.deviceKind = isTablet ? .tablet : .phone
    }

    // MARK: - Plugin lifecycle

    func start() {
        isRunning = true
        // Listen for outbound Device-targeted events
        broker.events
            .filter { $0.target == .device }
            .sink { [weak self] event in
                self?.handleOutbound(event)
            }
            .store(in: &cancellables)
    }

    func stop() {
        isRunning = false
        cancellables.removeAll()
        closeAll()
    }

    // MARK: - Pairing response from ViewModel

    func respondToPairing(accepted: Bool) {
        pairingContinuation?.resume(returning: accepted)
        pairingContinuation = nil
    }

    // MARK: - Connect

    func connect(ip: String, port: Int = 5027) {
        guard quicTask == nil, !quicConnected else { return }
        broker.send(AnchorEvent(
            target: .gui,
            message: .json(#"{"type":"connection_status","status":"connecting","host":"\#(ip)","port":\#(port)}"#)
        ))
        quicTask = Task { [weak self] in
            await self?.connectV1(ip: ip, port: UInt16(port))
        }
    }

    private func connectV1(ip: String, port: UInt16) async {
        do {
            guard let identity = ensureIdentity() else {
                throw AnchorNetworkTransportError.connectionFailed("client identity is unavailable")
            }

            let trusted = trustedStore.devices.values.first { $0.lastKnownIp == ip }
            let paired: TrustedStore.TrustedDeviceEntry
            if let trusted, let certificate = certificateDER(fromPEM: trusted.certificatePem) {
                paired = trusted
                try await establishV1(
                    ip: ip,
                    port: port,
                    identity: identity,
                    expectedCertificate: certificate
                )
            } else {
                paired = try await pairV1(ip: ip, port: port, identity: identity)
                try await establishV1(
                    ip: ip,
                    port: port,
                    identity: identity,
                    expectedCertificate: certificateDER(fromPEM: paired.certificatePem)!
                )
            }

            trustedStore.updateLastSeen(paired.deviceId, ip: ip)
            quicConnected = true
            NSLog("[anchor] Protocol v1 QUIC session ready: \(paired.deviceName) (\(paired.deviceId))")
            broker.send(AnchorEvent(
                target: .gui,
                message: .json(#"{"type":"connection_status","status":"connected","host":"\#(ip)","port":\#(port),"device_id":"\#(paired.deviceId)"}"#)
            ))
            if let session = quicSession {
                startV1Latency(session: session)
            }
            await readV1Loop()
        } catch is CancellationError {
            closeAll()
        } catch {
            NSLog("[anchor] Protocol v1 QUIC failed: \(error)")
            closeAll()
            let message = String(describing: error)
                .replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "\"", with: "\\\"")
            broker.send(AnchorEvent(
                target: .gui,
                message: .json(#"{"type":"connection_status","status":"disconnected","host":"\#(ip)","port":\#(port),"error":"\#(message)"}"#)
            ))
        }
    }

    private func pairV1(ip: String, port: UInt16, identity: SecIdentity) async throws -> TrustedStore.TrustedDeviceEntry {
        NSLog("[anchor] Starting Protocol v1 direct-address pairing with \(ip):\(port)")
        let transport = makeV1Transport(identity: identity)
        quicTransport = transport
        let peerCertificate = try await transport.connectForPairing(
            host: ip,
            port: port,
            serverName: "anchor.local"
        )
        let session = AnchorSession(transport: transport)
        quicSession = session

        var nonce = Data(count: 32)
        let randomStatus = nonce.withUnsafeMutableBytes {
            SecRandomCopyBytes(kSecRandomDefault, 32, $0.baseAddress!)
        }
        guard randomStatus == errSecSuccess else {
            throw AnchorNetworkTransportError.connectionFailed("could not generate pairing nonce")
        }
        guard let localCertificate = certificateDER(from: identity) else {
            throw AnchorNetworkTransportError.connectionFailed("client certificate is unavailable")
        }
        var transcript = Data("anchor/direct-pairing/v1".utf8)
        transcript.append(Data(ip.utf8))
        transcript.append(nonce)
        transcript.append(Data(SHA256.hash(data: localCertificate)))
        let transcriptHash = Data(SHA256.hash(data: transcript))

        var node = ANCHNodeId()
        node.value = Data(SHA256.hash(data: Data(deviceId.utf8)))
        var display = ANCHPeerDisplayInfo()
        display.displayName = deviceName
        display.deviceKind = deviceKind
        var hello = ANCHPairingHello()
        hello.nodeID = node
        hello.display = display
        hello.transcriptHash = transcriptHash
        var envelope = ANCHControlEnvelope()
        envelope.pairingHello = hello
        try await session.sendEnvelope(try envelope.serializedData())

        let resolution = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
        guard case .pairingApprove(let approval)? = resolution.body else {
            throw AnchorNetworkTransportError.connectionFailed("desktop rejected pairing")
        }
        guard approval.transcriptHash == transcriptHash else {
            throw AnchorNetworkTransportError.connectionFailed("pairing transcript mismatch")
        }
        guard !approval.approverDeviceID.isEmpty,
              approval.approverCertificateDer == peerCertificate else {
            throw AnchorNetworkTransportError.certificatePinMismatch
        }

        let desktopName = "Anchor Desktop"
        let pem = certificatePEM(peerCertificate)
        trustedStore.addDevice(
            approval.approverDeviceID,
            name: desktopName,
            certificatePem: pem,
            ip: ip
        )
        await session.close()
        quicSession = nil
        quicTransport = nil
        NSLog("[anchor] Protocol v1 pairing approved for \(approval.approverDeviceID)")
        return trustedStore.devices[approval.approverDeviceID]!
    }

    private func establishV1(ip: String, port: UInt16, identity: SecIdentity,
                             expectedCertificate: Data) async throws {
        let transport = makeV1Transport(identity: identity)
        let session = AnchorSession(transport: transport)
        quicTransport = transport
        quicSession = session
        try await session.connect(
            host: ip,
            port: port,
            serverName: "anchor.local",
            expectedCertificateFingerprint: Data(SHA256.hash(data: expectedCertificate))
        )

        var version = ANCHProtocolVersion()
        version.major = 1
        version.minor = 0
        var node = ANCHNodeId()
        node.value = Data(SHA256.hash(data: Data(deviceId.utf8)))
        var display = ANCHPeerDisplayInfo()
        display.displayName = deviceName
        display.deviceKind = deviceKind
        var hello = ANCHSessionHello()
        hello.protocolVersion = version
        hello.nodeID = node
        hello.display = display
        hello.endpoints = [iosEndpointAdvertisement()]
        var helloEnvelope = ANCHControlEnvelope()
        helloEnvelope.sessionHello = hello
        var ready = ANCHSessionReady()
        ready.protocolVersion = version
        var readyEnvelope = ANCHControlEnvelope()
        readyEnvelope.sessionReady = ready
        try await session.sendEnvelopes([
            try helloEnvelope.serializedData(),
            try readyEnvelope.serializedData()
        ])

        let peerHello = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
        guard case .sessionHello(let remote)? = peerHello.body,
              remote.protocolVersion.major == 1 else {
            throw AnchorNetworkTransportError.connectionFailed("expected compatible SessionHello")
        }
        let peerReady = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
        guard case .sessionReady? = peerReady.body else {
            throw AnchorNetworkTransportError.connectionFailed("expected SessionReady")
        }
        NSLog("[anchor] QUIC peer hello: \(remote.display.displayName), \(remote.endpoints.count) endpoint(s)")

        for capability in [Self.clipboardCapability, Self.inputCapability, Self.screenCapability] {
            let advertised = remote.endpoints.contains { endpoint in
                endpoint.endpointID == Self.desktopEndpoint && endpoint.capabilities.contains {
                    $0.name == capability && $0.major == 1
                }
            }
            if advertised {
                try await openV1Capability(capability, session: session)
            } else {
                NSLog("[anchor] Desktop did not advertise \(capability)@1")
            }
        }
        if capabilitySessions[Self.screenCapability] != nil {
            try await openV1ScreenStream(session: session, transport: transport)
        }
    }

    private func makeV1Transport(identity: SecIdentity) -> any AnchorNetworkQuicTransport {
        if #available(iOS 26.0, *) {
            return AnchorModernNetworkQuicTransport(clientIdentity: identity)
        }
        return AnchorNetworkFrameworkQuicTransport(clientIdentity: identity)
    }

    private func openV1Capability(_ name: String, session: AnchorSession) async throws {
        let requestId = nextRequestId
        nextRequestId += 1
        let capabilitySessionId = nextCapabilitySessionId
        nextCapabilitySessionId += 2

        var open = ANCHCapabilityOpen()
        open.capabilitySessionID = capabilitySessionId
        open.endpointID = Self.desktopEndpoint
        open.capabilityName = name
        open.capabilityMajor = 1
        var envelope = ANCHControlEnvelope()
        envelope.requestID = requestId
        envelope.capabilityOpen = open
        try await session.sendEnvelope(try envelope.serializedData())

        while true {
            let reply = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
            if case .capabilityOpened(let opened)? = reply.body,
               reply.responseTo == requestId,
               opened.capabilitySessionID == capabilitySessionId {
                capabilitySessions[name] = capabilitySessionId
                NSLog("[anchor] Opened \(name)@1 as capability session \(capabilitySessionId)")
                return
            }
            if case .ping(let ping)? = reply.body {
                var pong = ANCHPong(); pong.nonce = ping.nonce
                var pongEnvelope = ANCHControlEnvelope(); pongEnvelope.pong = pong
                try await session.sendEnvelope(try pongEnvelope.serializedData())
                continue
            }
            if case .protocolError(let error)? = reply.body {
                throw AnchorNetworkTransportError.connectionFailed("capability rejected: \(error.message)")
            }
            throw AnchorNetworkTransportError.connectionFailed("unexpected reply while opening \(name)")
        }
    }

    private func openV1ScreenStream(
        session: AnchorSession,
        transport: any AnchorNetworkQuicTransport
    ) async throws {
        guard let capabilityID = capabilitySessions[Self.screenCapability] else { return }
        let stream = try await transport.openReliableStream()
        let requestID = nextRequestId
        nextRequestId += 1
        let open = try AnchorStreamBindingCodec.openEnvelope(
            requestID: requestID,
            streamID: stream.streamIdentifier,
            capabilitySessionID: capabilityID,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )
        try await session.sendEnvelope(try open.serializedData())

        while true {
            let reply = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
            if (try? AnchorStreamBindingCodec.validateOpened(
                reply,
                requestID: requestID,
                streamID: stream.streamIdentifier
            )) != nil {
                break
            }
            if case .ping(let ping)? = reply.body {
                var pong = ANCHPong(); pong.nonce = ping.nonce
                var pongEnvelope = ANCHControlEnvelope(); pongEnvelope.pong = pong
                try await session.sendEnvelope(try pongEnvelope.serializedData())
            } else if case .capabilityRecord(let record)? = reply.body {
                try routeV1Record(record)
            } else if case .protocolError(let error)? = reply.body {
                throw AnchorNetworkTransportError.connectionFailed(
                    "screen stream rejected: \(error.message)"
                )
            } else {
                NSLog("[anchor] Ignoring unrelated record while binding screen stream")
            }
        }

        // Apple's modern QUIC API allocates the ID immediately but may defer
        // exposing the stream to the peer until its local send half emits a
        // STREAM frame. The binding is already accepted at this point; wake
        // the reverse half so the desktop's accept_bi() can attach its video
        // sender. The desktop deliberately drains this unused viewer-to-host
        // half, while screen bytes arrive on the independent host-to-viewer
        // half below.
        try await stream.send(Data([0]))
        screenStream = stream
        screenTask = Task { [weak self] in
            await self?.readV1ScreenStream(
                stream,
                capabilityID: capabilityID,
                streamID: stream.streamIdentifier
            )
        }
        try await sendV1Record(
            capabilityID,
            type: AnchorV1.TypeURL.screenStart,
            payload: try AnchorScreenCodec.start(),
            session: session
        )
        try await sendV1Record(
            capabilityID,
            type: AnchorV1.TypeURL.screenRequestKeyframe,
            payload: try AnchorScreenCodec.requestKeyframe(),
            session: session
        )
        NSLog("[anchor] Sideboat stream bound: QUIC stream \(stream.streamIdentifier)")
    }

    private func readV1ScreenStream(
        _ stream: any AnchorReliableStream,
        capabilityID: UInt64,
        streamID: UInt64
    ) async {
        var framer = AnchorVideoStreamFramer()
        var assembler = AnchorScreenFrameAssembler(
            capabilitySessionID: capabilityID,
            streamID: streamID
        )
        do {
            while !Task.isCancelled {
                framer.feed(try await stream.receive(maximumLength: 64 * 1024))
                while let packet = try framer.nextPacket() {
                    if let frame = try assembler.consume(packet) {
                        videoPlugin.feedFrame(
                            frame.payload,
                            frameId: frame.header.sequence,
                            sourcePresentationTimeUs: frame.header.presentationTimeUs
                        )
                    }
                }
            }
        } catch is CancellationError {
            return
        } catch {
            NSLog("[anchor] Sideboat stream failed: \(error)")
            assembler.reset()
            if let session = quicSession,
               let payload = try? AnchorScreenCodec.requestKeyframe() {
                try? await sendV1Record(
                    capabilityID,
                    type: AnchorV1.TypeURL.screenRequestKeyframe,
                    payload: payload,
                    session: session
                )
            }
        }
    }

    private func readV1Loop() async {
        guard let session = quicSession else { return }
        do {
            while !Task.isCancelled {
                let envelope = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
                switch envelope.body {
                case .ping(let ping):
                    var pong = ANCHPong()
                    pong.nonce = ping.nonce
                    var reply = ANCHControlEnvelope()
                    reply.pong = pong
                    try await session.sendEnvelope(try reply.serializedData())
                case .pong(let pong):
                    if let sentAt = takeV1Ping(nonce: pong.nonce) {
                        let elapsed = DispatchTime.now().uptimeNanoseconds &- sentAt
                        let rttMs = Double(elapsed) / 1_000_000
                        broker.send(AnchorEvent(
                            target: .gui,
                            message: .json(#"{"type":"latency_update","rtt_ms":\#(rttMs)}"#)
                        ))
                    }
                case .sessionClose:
                    throw AnchorNetworkTransportError.connectionClosed
                case .capabilityRecord(let record):
                    try routeV1Record(record)
                default:
                    NSLog("[anchor] Protocol v1 control record: \(String(describing: envelope.body))")
                }
            }
        } catch {
            guard quicConnected else { return }
            NSLog("[anchor] Protocol v1 session closed: \(error)")
            closeAll()
            broker.send(AnchorEvent(
                target: .gui,
                message: .json(#"{"type":"connection_status","status":"disconnected","reason":"closed_by_desktop"}"#)
            ))
        }
    }

    private func routeV1Record(_ record: ANCHCapabilityRecord) throws {
        if capabilitySessions[Self.screenCapability] == record.capabilitySessionID {
            if record.typeURL == AnchorV1.TypeURL.screenStatus {
                let status = try ANCHScreenScreenStatus(serializedBytes: record.payload)
                let info: [String: Any] = [
                    "type": "stream_info",
                    "width": status.width,
                    "height": status.height,
                    "fps": status.fps,
                    "bitrate_kbps": status.bitrateKbps,
                ]
                let data = try JSONSerialization.data(withJSONObject: info)
                videoPlugin.handleStreamInfo(String(decoding: data, as: UTF8.self))
            } else if record.typeURL == AnchorV1.TypeURL.screenOutputList {
                let outputs = try ANCHScreenScreenOutputList(serializedBytes: record.payload)
                NSLog("[anchor] Sideboat outputs received: \(outputs.outputs.count)")
            } else {
                NSLog("[anchor] Ignoring unknown screen record \(record.typeURL)")
            }
            return
        }
        guard record.typeURL == Self.clipboardPublishType,
              capabilitySessions[Self.clipboardCapability] == record.capabilitySessionID else {
            NSLog("[anchor] Ignoring unbound capability record \(record.typeURL)")
            return
        }
        let publish = try ANCHClipboardClipboardPublish(serializedBytes: record.payload)
        guard !AnchorClipboardCodec.isEcho(publish, localNodeID: localNodeId) else { return }

        let contentType: String
        let content: String
        let hashData: Data
        switch publish.content {
        case .textUtf8(let text):
            contentType = "text/plain"; content = text; hashData = Data(text.utf8)
        case .png(let png):
            contentType = "image/png"; content = png.base64EncodedString(); hashData = png
        case nil:
            return
        }
        let hash = SHA256.hash(data: hashData).map { String(format: "%02x", $0) }.joined()
        let packet: [String: Any] = [
            "plugin_id": "clipboard", "type": "clipboard_content",
            "content_type": contentType, "content": content,
            "timestamp": Int64(Date().timeIntervalSince1970 * 1000), "hash": hash
        ]
        let data = try JSONSerialization.data(withJSONObject: packet)
        broker.send(AnchorEvent(target: .service("clipboard"), message: .json(String(decoding: data, as: UTF8.self))))
    }

    private var localNodeId: Data {
        Data(SHA256.hash(data: Data(deviceId.utf8)))
    }

    private func iosEndpointAdvertisement() -> ANCHEndpointAdvertisement {
        var endpoint = ANCHEndpointAdvertisement()
        endpoint.endpointID = "io.anchor.ios"
        // Advertise only schemas the migration will service; SMS and Android-
        // style notification access are intentionally absent on iOS.
        endpoint.capabilities = []
        return endpoint
    }

    private func certificateDER(from identity: SecIdentity) -> Data? {
        var certificate: SecCertificate?
        guard SecIdentityCopyCertificate(identity, &certificate) == errSecSuccess,
              let certificate else { return nil }
        return SecCertificateCopyData(certificate) as Data
    }

    private func certificateDER(fromPEM pem: String) -> Data? {
        let base64 = pem
            .replacingOccurrences(of: "-----BEGIN CERTIFICATE-----", with: "")
            .replacingOccurrences(of: "-----END CERTIFICATE-----", with: "")
            .components(separatedBy: .whitespacesAndNewlines)
            .joined()
        return Data(base64Encoded: base64)
    }

    private func certificatePEM(_ der: Data) -> String {
        "-----BEGIN CERTIFICATE-----\n" +
            der.base64EncodedString(options: [.lineLength64Characters, .endLineWithLineFeed]) +
            "-----END CERTIFICATE-----\n"
    }

    // Retained only while feature messages are moved to typed capability
    // records. No production call site opens this legacy transport anymore.
    private func connectLegacy(ip: String, port: Int = 5025) {
        guard jsonConnection == nil else { return }
        udpConnectionReady = false
        udpReadySent = false

        broker.send(AnchorEvent(
            target: .gui,
            message: .json(#"{"type":"connection_status","status":"connecting","host":"\#(ip)","port":\#(port)}"#)
        ))

        let tlsParams = createTLSParameters()

        // JSON channel
        let jsonEndpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(ip),
            port: NWEndpoint.Port(integerLiteral: UInt16(port))
        )
        let jsonConn = NWConnection(to: jsonEndpoint, using: tlsParams)
        self.jsonConnection = jsonConn

        jsonConn.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                Task { await self.onJsonReady(ip: ip, port: port) }
            case .failed(let err):
                self.broker.send(AnchorEvent(
                    target: .gui,
                    message: .json(#"{"type":"connection_status","status":"disconnected","error":"\#(err.localizedDescription)"}"#)
                ))
                self.closeAll()
            case .cancelled:
                break
            default:
                break
            }
        }
        jsonConn.start(queue: DispatchQueue(label: "anchor.json"))

        // H264 channel — UDP (no TLS, direct datagram)
        let h264Endpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host(ip),
            port: NWEndpoint.Port(integerLiteral: UInt16(port + 1))
        )
        let udpParams = NWParameters.udp
        udpParams.requiredLocalEndpoint = NWEndpoint.hostPort(
            host: NWEndpoint.Host("0.0.0.0"),
            port: NWEndpoint.Port(integerLiteral: UInt16(port + 1))
        )
        let h264Conn = NWConnection(to: h264Endpoint, using: udpParams)
        self.h264Connection = h264Conn

        h264Conn.stateUpdateHandler = { [weak self] state in
            if case .ready = state {
                NSLog("[anchor] H264 UDP: connection ready")
                self?.udpConnectionReady = true
                self?.sendUdpVideoReadyIfPossible(port: port + 1)
                self?.readUdpH264Loop()
            }
        }
        h264Conn.start(queue: DispatchQueue(label: "anchor.h264"))
    }

    // MARK: - JSON channel ready — identity exchange + pairing

    private func onJsonReady(ip: String, port: Int) async {
        guard let conn = jsonConnection else { return }

        // Build identity packet (matches Android exactly)
        let identityJson: [String: Any] = [
            "type": "identity",
            "device_id": deviceId,
            "device_name": deviceName,
            "device_type": "ios",
            "capabilities": ["sms", "video", "clipboard"],
            "protocol_version": 1,
            "video_transport": "udp"
        ]

        guard let identityData = try? JSONSerialization.data(withJSONObject: identityJson) else { return }

        // Send identity
        writeLengthPrefixed(conn, data: identityData)
        NSLog("[anchor] Sent identity: \(deviceName) (\(deviceId))")

        // Read desktop identity
        guard let desktopData = await readLengthPrefixedAsync(conn),
              let desktopJson = try? JSONSerialization.jsonObject(with: desktopData) as? [String: Any] else {
            NSLog("[anchor] Failed to read desktop identity")
            closeAll()
            return
        }

        let desktopId = desktopJson["device_id"] as? String ?? "unknown"
        let desktopName = desktopJson["device_name"] as? String ?? "unknown"
        NSLog("[anchor] Desktop identity: \(desktopName) (\(desktopId))")

        // Pairing check
        if trustedStore.isTrusted(desktopId) {
            NSLog("[anchor] Auto-connected to trusted desktop: \(desktopName)")
            trustedStore.updateLastSeen(desktopId)
        } else {
            // Compute fingerprint from desktop's TLS peer certificate
            let fingerprint = extractPeerFingerprint(conn) ?? "N/A"
            NSLog("[anchor] Unknown desktop, requesting pairing (fp: \(fingerprint))")

            // Send pairing request to UI
            broker.send(AnchorEvent(
                target: .gui,
                message: .json(#"{"type":"pairing_request","device_id":"\#(desktopId)","device_name":"\#(desktopName)","device_type":"desktop","fingerprint":"\#(fingerprint)"}"#)
            ))

            // Wait for user response (60s timeout)
            let accepted: Bool = await withCheckedContinuation { continuation in
                self.pairingContinuation = continuation

                // Timeout after 60 seconds
                DispatchQueue.global().asyncAfter(deadline: .now() + 60) { @Sendable [weak self] in
                    self?.pairingContinuation?.resume(returning: false)
                    self?.pairingContinuation = nil
                }
            }

            if !accepted {
                NSLog("[anchor] Pairing rejected for \(desktopName)")
                disconnect()
                return
            }

            // Extract PEM from peer cert
            let certPem = extractPeerCertPEM(conn) ?? ""
            trustedStore.addDevice(desktopId, name: desktopName, certificatePem: certPem)
            NSLog("[anchor] Paired with desktop: \(desktopName) (\(desktopId))")
        }

        // Notify connected
        broker.send(AnchorEvent(
            target: .gui,
            message: .json(#"{"type":"connection_status","status":"connected","host":"\#(ip)","port":\#(port)}"#)
        ))

        sendUdpVideoReadyIfPossible(port: port + 1)

        // Start JSON read loop
        readJsonLoop()

        // Start periodic latency ping
        startLatencyPing()
    }

    // MARK: - Read loops

    private func readJsonLoop() {
        guard let conn = jsonConnection else { return }

        readLengthPrefixed(conn) { [weak self] data in
            guard let self = self, let data = data else {
                self?.broker.send(AnchorEvent(
                    target: .gui,
                    message: .json(#"{"type":"connection_status","status":"disconnected","reason":"closed_by_desktop"}"#)
                ))
                self?.closeAll()
                return
            }

            if let text = String(data: data, encoding: .utf8) {
                self.routeIncomingMessage(text)
            }

            // Continue reading
            self.readJsonLoop()
        }
    }

    private func sendUdpVideoReadyIfPossible(port: Int) {
        guard udpConnectionReady else {
            NSLog("[anchor] H264 UDP: deferring udp_video_ready until UDP is ready")
            return
        }
        guard !udpReadySent else { return }
        guard let conn = jsonConnection, conn.state == .ready else {
            NSLog("[anchor] H264 UDP: deferring udp_video_ready until JSON is ready")
            return
        }
        let readyMsg = "{\"type\":\"udp_video_ready\",\"port\":\(port)}"
        udpReadySent = true
        writeLengthPrefixed(conn, data: Data(readyMsg.utf8))
        NSLog("[anchor] H264 UDP: sent udp_video_ready (port \(port))")
    }

    private func readUdpH264Loop() {
        guard let conn = h264Connection else { return }
        lastUdpPacketRecvNs = 0
        udpPacketsThisWindow = 0
        udpStatsWindowStartNs = 0

        let ra = UdpFrameReassembler(
            onFrameComplete: { [weak self] frameId, data in
                self?.videoPlugin.feedFrame(data, frameId: frameId)
            },
            onFrameLost: { [weak self] in
                NSLog("[anchor] [udp] Frame loss detected, requesting keyframe")
                self?.broker.send(AnchorEvent(
                    target: .device,
                    message: .json("{\"plugin_id\":\"wayland\",\"command\":\"request_keyframe\"}")
                ))
            }
        )
        self.reassembler = ra

        readNextDatagram(conn, reassembler: ra)
    }

    private func readNextDatagram(_ conn: NWConnection, reassembler: UdpFrameReassembler) {
        conn.receiveMessage { [weak self] data, _, _, error in
            guard let self = self, let data = data, error == nil else {
                NSLog("[anchor] H264 UDP channel closed: \(error?.localizedDescription ?? "nil")")
                return
            }
            let nowNs = DispatchTime.now().uptimeNanoseconds
            if self.lastUdpPacketRecvNs != 0 {
                let gapNs = nowNs - self.lastUdpPacketRecvNs
                if gapNs >= Self.udpPacketGapWarnNs {
                    NSLog(
                        "[anchor] [udp] packet_gap_ms=%.1f len=%d %@",
                        Double(gapNs) / 1_000_000.0,
                        data.count,
                        reassembler.stats()
                    )
                }
            }
            self.lastUdpPacketRecvNs = nowNs

            if self.udpStatsWindowStartNs == 0 {
                self.udpStatsWindowStartNs = nowNs
            }
            self.udpPacketsThisWindow += 1
            if nowNs - self.udpStatsWindowStartNs >= Self.udpStatsIntervalNs {
                NSLog(
                    "[anchor] [udp] packets_per_s=%d %@",
                    self.udpPacketsThisWindow,
                    reassembler.stats()
                )
                self.udpPacketsThisWindow = 0
                self.udpStatsWindowStartNs = nowNs
            }
            reassembler.onPacket(data)
            self.readNextDatagram(conn, reassembler: reassembler)
        }
    }

    // MARK: - Message routing (incoming JSON)

    private func routeIncomingMessage(_ text: String) {
        guard let data = text.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            broker.send(AnchorEvent(target: .gui, message: .generic("Parse error: \(text)")))
            return
        }

        let msgType = json["type"] as? String
        let pluginId = json["plugin_id"] as? String

        if msgType == "pong" {
            // Latency measurement response
            if let sentAt = json["sent_at"] as? Double {
                let now = CACurrentMediaTime() * 1000
                let rtt = now - sentAt
                // Ignore pongs from the desktop's own pings (wrong clock domain)
                guard rtt >= 0 && rtt < 30000 else { return }
                NSLog("[anchor] Network RTT: %.1fms", rtt)
                DispatchQueue.main.async {
                    self.broker.send(AnchorEvent(target: .gui, message: .json(
                        "{\"type\":\"latency_update\",\"rtt_ms\":\(rtt)}"
                    )))
                }
            }
            return
        } else if msgType == "stream_info" {
            videoPlugin.handleStreamInfo(text)
        } else if let pluginId = pluginId {
            broker.send(AnchorEvent(target: .service(pluginId), message: .json(text)))
        } else {
            broker.send(AnchorEvent(target: .gui, message: .json(text)))
        }
    }

    // MARK: - Outbound (Device target -> desktop)

    private func handleOutbound(_ event: AnchorEvent) {
        if quicConnected, case .json(let payload) = event.message {
            Task { [weak self] in
                do { try await self?.sendV1Payload(payload) }
                catch { NSLog("[anchor] v1 feature send failed: \(error)") }
            }
            return
        }
        guard let conn = jsonConnection else { return }
        switch event.message {
        case .json(let payload):
            writeLengthPrefixed(conn, data: Data(payload.utf8))
        case .generic(let text):
            writeLengthPrefixed(conn, data: Data(text.utf8))
        case .binary:
            break
        }
    }

    private func sendV1Payload(_ payload: String) async throws {
        guard let session = quicSession,
              let data = payload.data(using: .utf8),
              let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let plugin = json["plugin_id"] as? String else { return }
        switch plugin {
        case "clipboard": try await sendV1Clipboard(json, session: session)
        case "input": try await sendV1Input(json, session: session)
        case "wayland":
            guard json["command"] as? String == "request_keyframe",
                  let capabilityID = capabilitySessions[Self.screenCapability] else { return }
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.screenRequestKeyframe,
                payload: try AnchorScreenCodec.requestKeyframe(),
                session: session
            )
        default: NSLog("[anchor] Feature \(plugin) has not migrated to Protocol v1 yet")
        }
    }

    private func sendV1Clipboard(_ json: [String: Any], session: AnchorSession) async throws {
        guard let capabilityId = capabilitySessions[Self.clipboardCapability],
              json["type"] as? String == "clipboard_content",
              let contentType = json["content_type"] as? String,
              let content = json["content"] as? String else { return }
        clipboardRevision += 1
        let encoded: Data
        if contentType == "text/plain" {
            encoded = try AnchorClipboardCodec.publishText(
                originNodeID: localNodeId, revision: clipboardRevision, text: content
            )
        } else if contentType == "image/png", let png = Data(base64Encoded: content) {
            encoded = try AnchorClipboardCodec.publishPNG(
                originNodeID: localNodeId, revision: clipboardRevision, png: png
            )
        } else { return }
        try await sendV1Record(capabilityId, type: Self.clipboardPublishType,
                               payload: encoded, session: session)
    }

    private func sendV1Input(_ json: [String: Any], session: AnchorSession) async throws {
        guard let capabilityId = capabilitySessions[Self.inputCapability],
              let type = json["type"] as? String else { return }
        func number(_ key: String) -> Double { (json[key] as? NSNumber)?.doubleValue ?? 0 }

        switch type {
        case "anchor.input.motion":
            let message = AnchorInputCodec.relative(dx: number("dx"), dy: number("dy"))
            try await sendV1Record(capabilityId, type: Self.inputRelativeType, payload: try message.serializedData(), session: session)
        case "anchor.input.motion_absolute":
            let message = try AnchorInputCodec.absolute(x: number("x"), y: number("y"))
            try await sendV1Record(capabilityId, type: Self.inputAbsoluteType, payload: try message.serializedData(), session: session)
        case "anchor.input.button":
            let button: AnchorPointerButton
            switch Int(number("button")) { case 272: button = .left; case 273: button = .right; case 274: button = .middle; default: return }
            let message = AnchorInputCodec.button(button, pressed: Int(number("state")) != 0)
            try await sendV1Record(capabilityId, type: Self.inputButtonType, payload: try message.serializedData(), session: session)
        case "anchor.input.axis":
            let value = number("value")
            guard let message = Int(number("axis")) == 1
                    ? scrollAccumulator.consume(horizontalSteps: value, verticalSteps: 0)
                    : scrollAccumulator.consume(horizontalSteps: 0, verticalSteps: value) else { return }
            try await sendV1Record(capabilityId, type: Self.inputScrollType, payload: try message.serializedData(), session: session)
        case "anchor.input.text":
            var message = ANCHInputInputText(); message.textUtf8 = json["text"] as? String ?? ""
            try await sendV1Record(capabilityId, type: Self.inputTextType, payload: try message.serializedData(), session: session)
        case "anchor.input.key":
            if let usage = AnchorHIDUsage.keyboard(json["key"] as? String ?? "") { try await sendKeyClick(usage, capabilityId: capabilityId, session: session) }
        case "anchor.input.key_combo":
            let modifiers = (json["modifiers"] as? [String] ?? []).compactMap(AnchorHIDUsage.keyboard)
            guard let key = AnchorHIDUsage.keyboard(json["key"] as? String ?? "") else { return }
            for usage in modifiers { try await sendV1Key(usage, pressed: true, capabilityId: capabilityId, session: session) }
            try await sendKeyClick(key, capabilityId: capabilityId, session: session)
            for usage in modifiers.reversed() { try await sendV1Key(usage, pressed: false, capabilityId: capabilityId, session: session) }
        default: break
        }
    }

    private func sendV1Record(_ capabilityId: UInt64, type: String, payload: Data, session: AnchorSession) async throws {
        var record = ANCHCapabilityRecord(); record.capabilitySessionID = capabilityId; record.typeURL = type; record.payload = payload
        var envelope = ANCHControlEnvelope(); envelope.capabilityRecord = record
        try await session.sendEnvelope(try envelope.serializedData())
    }

    private func sendKeyClick(_ usage: UInt32, capabilityId: UInt64, session: AnchorSession) async throws {
        try await sendV1Key(usage, pressed: true, capabilityId: capabilityId, session: session)
        try await sendV1Key(usage, pressed: false, capabilityId: capabilityId, session: session)
    }

    private func sendV1Key(_ usage: UInt32, pressed: Bool, capabilityId: UInt64, session: AnchorSession) async throws {
        var message = ANCHInputInputKey(); message.hidUsage = usage; message.pressed = pressed
        try await sendV1Record(capabilityId, type: Self.inputKeyType, payload: try message.serializedData(), session: session)
    }

    // MARK: - Latency measurement

    private var pingTimer: DispatchSourceTimer?

    private func startLatencyPing() {
        pingTimer?.cancel()
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue.global())
        timer.schedule(deadline: .now() + 1, repeating: 2.0)
        timer.setEventHandler { [weak self] in
            guard let self = self, let conn = self.jsonConnection else { return }
            let now = CACurrentMediaTime() * 1000 // ms
            let ping = "{\"type\":\"ping\",\"sent_at\":\(now)}"
            self.writeLengthPrefixed(conn, data: Data(ping.utf8))
        }
        timer.resume()
        pingTimer = timer
    }

    private func stopLatencyPing() {
        pingTimer?.cancel()
        pingTimer = nil
    }

    private func startV1Latency(session: AnchorSession) {
        v1LatencyTask?.cancel()
        v1LatencyTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let nonce = self.allocateV1Ping()
                var ping = ANCHPing(); ping.nonce = nonce
                var envelope = ANCHControlEnvelope(); envelope.ping = ping
                do {
                    try await session.sendEnvelope(try envelope.serializedData())
                } catch {
                    self.takeV1Ping(nonce: nonce)
                    return
                }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }

    private func allocateV1Ping() -> UInt64 {
        v1PingLock.lock()
        defer { v1PingLock.unlock() }
        let nonce = nextPingNonce
        nextPingNonce &+= 1
        v1PendingPings[nonce] = DispatchTime.now().uptimeNanoseconds
        // A peer that never replies must not create an unbounded diagnostics
        // queue. At a two-second cadence, sixteen entries retain ample history.
        if v1PendingPings.count > 16, let oldest = v1PendingPings.keys.min() {
            v1PendingPings.removeValue(forKey: oldest)
        }
        return nonce
    }

    @discardableResult
    private func takeV1Ping(nonce: UInt64) -> UInt64? {
        v1PingLock.lock()
        defer { v1PingLock.unlock() }
        return v1PendingPings.removeValue(forKey: nonce)
    }

    private func stopV1Latency() {
        v1LatencyTask?.cancel()
        v1LatencyTask = nil
        v1PingLock.lock()
        v1PendingPings.removeAll()
        v1PingLock.unlock()
    }

    // MARK: - Disconnect

    func disconnect() {
        stopLatencyPing()
        closeAll()
        broker.send(AnchorEvent(
            target: .gui,
            message: .json(#"{"type":"connection_status","status":"disconnected","reason":"user_disconnect"}"#)
        ))
    }

    private func closeAll() {
        stopLatencyPing()
        stopV1Latency()
        quicConnected = false
        quicTransport?.close()
        quicTransport = nil
        quicSession = nil
        screenTask?.cancel()
        screenTask = nil
        screenStream = nil
        videoPlugin.stop()
        quicTask?.cancel()
        quicTask = nil
        capabilitySessions.removeAll()
        nextRequestId = 1
        nextCapabilitySessionId = 1
        udpConnectionReady = false
        udpReadySent = false
        jsonConnection?.cancel()
        jsonConnection = nil
        h264Connection?.cancel()
        h264Connection = nil
    }

    // MARK: - Wire protocol: [4-byte LE length][payload]

    private func writeLengthPrefixed(_ connection: NWConnection, data: Data) {
        var length = UInt32(data.count).littleEndian
        var header = Data(bytes: &length, count: 4)
        header.append(data)
        connection.send(content: header, completion: .contentProcessed { error in
            if let error = error {
                NSLog("[anchor] Write error: \(error)")
            }
        })
    }

    private func readLengthPrefixed(_ connection: NWConnection, completion: @escaping (Data?) -> Void) {
        // Read 4-byte header
        connection.receive(minimumIncompleteLength: 4, maximumLength: 4) { headerData, _, _, error in
            guard let headerData = headerData, headerData.count == 4, error == nil else {
                completion(nil)
                return
            }

            let length = headerData.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian }
            guard length > 0, length < 16_000_000 else {
                completion(nil)
                return
            }

            // Read payload
            self.readExact(connection, remaining: Int(length), accumulated: Data()) { payload in
                completion(payload)
            }
        }
    }

    /// Read exactly `remaining` bytes, accumulating partial reads.
    private func readExact(_ connection: NWConnection, remaining: Int, accumulated: Data, completion: @escaping (Data?) -> Void) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: remaining) { data, _, _, error in
            guard let data = data, error == nil else {
                completion(nil)
                return
            }
            var acc = accumulated
            acc.append(data)
            let left = remaining - data.count
            if left <= 0 {
                completion(acc)
            } else {
                self.readExact(connection, remaining: left, accumulated: acc, completion: completion)
            }
        }
    }

    /// Async wrapper for readLengthPrefixed
    private func readLengthPrefixedAsync(_ connection: NWConnection) async -> Data? {
        await withCheckedContinuation { continuation in
            readLengthPrefixed(connection) { data in
                continuation.resume(returning: data)
            }
        }
    }

    // MARK: - TLS

    private func createTLSParameters() -> NWParameters {
        let tlsOptions = NWProtocolTLS.Options()
        let secOptions = tlsOptions.securityProtocolOptions

        // Set our client identity
        if let identity = ensureIdentity() {
            if let secIdentity = sec_identity_create(identity) {
                sec_protocol_options_set_local_identity(secOptions, secIdentity)
            }
        }

        // Trust all server certs (pairing handles verification)
        // Capture peer certificate during handshake for fingerprint/PEM extraction
        sec_protocol_options_set_verify_block(secOptions, { [weak self] _, trust, completionHandler in
            let peerTrust = sec_trust_copy_ref(trust).takeRetainedValue()
            if let certs = SecTrustCopyCertificateChain(peerTrust) as? [SecCertificate],
               let cert = certs.first {
                self?.peerCertificateData = SecCertificateCopyData(cert) as Data
            }
            completionHandler(true)
        }, DispatchQueue.global())

        // Set min TLS version
        sec_protocol_options_set_min_tls_protocol_version(secOptions, .TLSv12)

        // Low-latency TCP options
        let tcpOptions = NWProtocolTCP.Options()
        tcpOptions.noDelay = true           // disable Nagle's algorithm
        tcpOptions.enableFastOpen = true    // TCP Fast Open for reconnects

        let params = NWParameters(tls: tlsOptions, tcp: tcpOptions)

        return params
    }

    // MARK: - Identity (Keychain)

    private static let keychainTag = "com.anchor.identity.v3"
    private static let keychainKeyTag = "com.anchor.identity.v3.key"

    private func ensureIdentity() -> SecIdentity? {
        // Try to load existing identity and verify it can sign
        if let identity = loadIdentity() {
            if validateIdentity(identity) {
                NSLog("[anchor] Loaded existing identity from Keychain (validated)")
                return identity
            }
            NSLog("[anchor] Existing identity is broken, regenerating...")
        }

        // Clean up any partial/broken state
        cleanupKeychain()

        // Generate new EC P-256 key pair + self-signed cert
        NSLog("[anchor] Generating new identity keypair + certificate")
        guard let identity = generateIdentity() else {
            NSLog("[anchor] ERROR: Identity generation failed — TLS will have no client cert")
            return nil
        }
        NSLog("[anchor] Identity generated and stored in Keychain")
        return identity
    }

    /// Verify the identity's private key can actually sign data.
    private func validateIdentity(_ identity: SecIdentity) -> Bool {
        var privateKey: SecKey?
        let status = SecIdentityCopyPrivateKey(identity, &privateKey)
        guard status == errSecSuccess, let key = privateKey else {
            NSLog("[anchor] validateIdentity: can't extract private key (status: \(status))")
            return false
        }
        // Try a test signature
        let testData = Data("anchor-test".utf8) as CFData
        var error: Unmanaged<CFError>?
        guard let _ = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256, testData, &error) else {
            NSLog("[anchor] validateIdentity: test sign failed: \(error?.takeRetainedValue().localizedDescription ?? "?")")
            return false
        }
        return true
    }

    private func loadIdentity() -> SecIdentity? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassIdentity,
            kSecAttrLabel as String: Self.keychainTag,
            kSecReturnRef as String: true
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecSuccess {
            return (item as! SecIdentity)
        }
        NSLog("[anchor] No existing identity in Keychain (status: \(status))")
        return nil
    }

    private func cleanupKeychain() {
        // Delete keys by tag
        let keyQuery: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: Self.keychainKeyTag.data(using: .utf8)!
        ]
        let keyDel = SecItemDelete(keyQuery as CFDictionary)
        NSLog("[anchor] Cleanup keys by tag: \(keyDel)")

        // Delete keys by label
        let keyQuery2: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrLabel as String: Self.keychainTag
        ]
        let keyDel2 = SecItemDelete(keyQuery2 as CFDictionary)
        NSLog("[anchor] Cleanup keys by label: \(keyDel2)")

        // Delete cert
        let certQuery: [String: Any] = [
            kSecClass as String: kSecClassCertificate,
            kSecAttrLabel as String: Self.keychainTag
        ]
        let certDel = SecItemDelete(certQuery as CFDictionary)
        NSLog("[anchor] Cleanup cert: \(certDel)")
    }

    private func generateIdentity() -> SecIdentity? {
        // 1. Generate a TRANSIENT EC P-256 key (NOT stored in Keychain yet)
        let keyParams: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256
        ]

        var error: Unmanaged<CFError>?
        guard let privateKey = SecKeyCreateRandomKey(keyParams as CFDictionary, &error) else {
            NSLog("[anchor] Key generation failed: \(error?.takeRetainedValue().localizedDescription ?? "unknown")")
            return nil
        }
        NSLog("[anchor] EC P-256 key generated (transient)")

        // Verify the key can sign
        let testData = Data("test".utf8) as CFData
        var signError: Unmanaged<CFError>?
        if SecKeyCreateSignature(privateKey, .ecdsaSignatureMessageX962SHA256, testData, &signError) == nil {
            NSLog("[anchor] ERROR: freshly generated key can't sign: \(signError?.takeRetainedValue().localizedDescription ?? "?")")
            return nil
        }
        NSLog("[anchor] Key signing verified OK")

        guard let publicKey = SecKeyCopyPublicKey(privateKey) else {
            NSLog("[anchor] Failed to extract public key")
            return nil
        }

        // 2. Manually store ONLY the private key in the Keychain
        let addKeyQuery: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecValueRef as String: privateKey,
            kSecAttrApplicationTag as String: Self.keychainKeyTag.data(using: .utf8)!,
            kSecAttrIsPermanent as String: true
        ]
        let keyAddStatus = SecItemAdd(addKeyQuery as CFDictionary, nil)
        if keyAddStatus != errSecSuccess && keyAddStatus != errSecDuplicateItem {
            NSLog("[anchor] Failed to store private key in Keychain: \(keyAddStatus)")
            return nil
        }
        NSLog("[anchor] Private key stored in Keychain")

        // 3. Build self-signed X.509 certificate in DER format
        let cn = "anchor-\(UUID().uuidString)"
        guard let certDER = buildCertificateDER(cn: cn, publicKey: publicKey, privateKey: privateKey) else {
            NSLog("[anchor] Failed to build certificate DER")
            return nil
        }
        NSLog("[anchor] Certificate DER built (\(certDER.count) bytes)")

        guard let cert = SecCertificateCreateWithData(nil, certDER as CFData) else {
            NSLog("[anchor] SecCertificateCreateWithData failed — DER is malformed")
            let hex = certDER.prefix(64).map { String(format: "%02x", $0) }.joined(separator: " ")
            NSLog("[anchor] DER prefix: \(hex)")
            return nil
        }
        NSLog("[anchor] SecCertificate created: \(SecCertificateCopySubjectSummary(cert) as String? ?? "?")")

        // 3. Add certificate to Keychain
        let addCertQuery: [String: Any] = [
            kSecClass as String: kSecClassCertificate,
            kSecValueRef as String: cert,
            kSecAttrLabel as String: Self.keychainTag
        ]
        let addStatus = SecItemAdd(addCertQuery as CFDictionary, nil)
        if addStatus != errSecSuccess && addStatus != errSecDuplicateItem {
            NSLog("[anchor] Failed to add cert to Keychain: \(addStatus)")
            return nil
        }
        NSLog("[anchor] Certificate added to Keychain")

        // 4. Load the identity (Keychain matches cert's public key hash to private key)
        guard let identity = loadIdentity() else {
            NSLog("[anchor] Failed to load identity after adding cert — key/cert mismatch?")
            // Debug: check if key and cert are both present
            debugKeychainState()
            return nil
        }
        return identity
    }

    private func debugKeychainState() {
        // Check if the key exists
        let keyQuery: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: Self.keychainKeyTag.data(using: .utf8)!,
            kSecReturnAttributes as String: true
        ]
        var keyItem: CFTypeRef?
        let keyStatus = SecItemCopyMatching(keyQuery as CFDictionary, &keyItem)
        NSLog("[anchor] Key in Keychain: \(keyStatus == errSecSuccess) (status: \(keyStatus))")

        // Check if the cert exists
        let certQuery: [String: Any] = [
            kSecClass as String: kSecClassCertificate,
            kSecAttrLabel as String: Self.keychainTag,
            kSecReturnAttributes as String: true
        ]
        var certItem: CFTypeRef?
        let certStatus = SecItemCopyMatching(certQuery as CFDictionary, &certItem)
        NSLog("[anchor] Cert in Keychain: \(certStatus == errSecSuccess) (status: \(certStatus))")

        if let certAttrs = certItem as? [String: Any],
           let keyAttrs = keyItem as? [String: Any] {
            let certPKHash = certAttrs[kSecAttrPublicKeyHash as String]
            let keyAppLabel = keyAttrs[kSecAttrApplicationLabel as String]
            NSLog("[anchor] Cert publicKeyHash: \(String(describing: certPKHash))")
            NSLog("[anchor] Key applicationLabel: \(String(describing: keyAppLabel))")
        }
    }

    // MARK: - X.509 Certificate Builder

    /// Build a minimal self-signed X.509 v3 certificate in DER format.
    /// v3 is required by rustls/webpki on the desktop side.
    private func buildCertificateDER(cn: String, publicKey: SecKey, privateKey: SecKey) -> Data? {
        guard let pubKeyData = SecKeyCopyExternalRepresentation(publicKey, nil) as Data? else {
            NSLog("[anchor] Failed to export public key")
            return nil
        }
        NSLog("[anchor] Public key: \(pubKeyData.count) bytes")

        // Signature algorithm: ecdsaWithSHA256 (1.2.840.10045.4.3.2)
        let sigAlgOID: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02]
        let sigAlgSequence = derSequence(derOID(sigAlgOID))

        // --- TBS Certificate ---
        var tbs = Data()

        // Version: v3 — explicitly tagged [0] EXPLICIT INTEGER 2
        tbs.append(contentsOf: [0xA0, 0x03, 0x02, 0x01, 0x02])

        // Serial number: random positive integer
        var serialBytes = [UInt8](repeating: 0, count: 8)
        _ = SecRandomCopyBytes(kSecRandomDefault, serialBytes.count, &serialBytes)
        serialBytes[0] &= 0x7F  // ensure positive
        if serialBytes[0] == 0 { serialBytes[0] = 1 }  // ensure non-zero leading byte
        tbs.append(derInteger(Data(serialBytes)))

        // Signature algorithm (must match outer signature)
        tbs.append(sigAlgSequence)

        // Issuer: CN=<cn>
        let issuer = derName(cn: cn)
        tbs.append(issuer)

        // Validity
        tbs.append(derValidity(years: 10))

        // Subject (same as issuer for self-signed)
        tbs.append(issuer)

        // Subject Public Key Info
        tbs.append(derSubjectPublicKeyInfo(pubKeyData))

        // v3 requires extensions — add Subject Alternative Name (SAN) with DNS name
        // webpki/rustls requires at least a SAN extension
        let sanExtension = buildSANExtension(dnsName: "anchor.local")
        // Extensions are [3] EXPLICIT SEQUENCE { extensions }
        let extensionsSeq = derSequence(sanExtension)
        let extensionsTagged = Data([0xA3]) + derLength(extensionsSeq.count) + extensionsSeq
        tbs.append(extensionsTagged)

        let tbsCertDER = derSequence(tbs)

        // --- Sign the TBS certificate ---
        var signError: Unmanaged<CFError>?
        guard let signatureData = SecKeyCreateSignature(
            privateKey,
            .ecdsaSignatureMessageX962SHA256,
            tbsCertDER as CFData,
            &signError
        ) as Data? else {
            NSLog("[anchor] Signing failed: \(signError?.takeRetainedValue().localizedDescription ?? "unknown")")
            return nil
        }
        NSLog("[anchor] TBS signed (\(signatureData.count) bytes)")

        // --- Full certificate: SEQUENCE { tbsCert, sigAlg, sigValue } ---
        var fullCert = Data()
        fullCert.append(tbsCertDER)
        fullCert.append(sigAlgSequence)
        fullCert.append(derBitString(signatureData))

        return derSequence(fullCert)
    }

    /// Build Subject Alternative Name extension with a DNS name.
    /// OID: 2.5.29.17 (subjectAltName)
    private func buildSANExtension(dnsName: String) -> Data {
        let sanOID: [UInt8] = [0x55, 0x1D, 0x11] // 2.5.29.17
        // DNS name is context tag [2] implicit
        let dnsBytes = Data(dnsName.utf8)
        let dnsNameTagged = Data([0x82]) + derLength(dnsBytes.count) + dnsBytes
        let sanValue = derSequence(dnsNameTagged)
        // Extension: SEQUENCE { OID, OCTET STRING { value } }
        return derSequence(derOID(sanOID) + derOctetString(sanValue))
    }

    // MARK: - DER Encoding Helpers

    private func derSequence(_ content: Data) -> Data {
        return Data([0x30]) + derLength(content.count) + content
    }

    private func derInteger(_ value: Data) -> Data {
        // Strip leading zeros but keep one if needed for positive sign
        var bytes = [UInt8](value)
        while bytes.count > 1 && bytes[0] == 0 && bytes[1] & 0x80 == 0 {
            bytes.removeFirst()
        }
        // Add leading zero if high bit set (to keep positive)
        if let first = bytes.first, first & 0x80 != 0 {
            bytes.insert(0, at: 0)
        }
        return Data([0x02]) + derLength(bytes.count) + Data(bytes)
    }

    private func derBitString(_ content: Data) -> Data {
        // BIT STRING: tag + length + unusedBits(0) + content
        let inner = Data([0x00]) + content
        return Data([0x03]) + derLength(inner.count) + inner
    }

    private func derOctetString(_ content: Data) -> Data {
        return Data([0x04]) + derLength(content.count) + content
    }

    private func derOID(_ oid: [UInt8]) -> Data {
        return Data([0x06, UInt8(oid.count)] + oid)
    }

    private func derUTF8String(_ string: String) -> Data {
        let bytes = Data(string.utf8)
        return Data([0x0C]) + derLength(bytes.count) + bytes
    }

    private func derUTCTime(_ date: Date) -> Data {
        let fmt = DateFormatter()
        fmt.dateFormat = "yyMMddHHmmss'Z'"
        fmt.timeZone = TimeZone(identifier: "UTC")
        let str = fmt.string(from: date)
        let bytes = Data(str.utf8)
        return Data([0x17]) + derLength(bytes.count) + bytes
    }

    private func derLength(_ length: Int) -> Data {
        if length < 0x80 {
            return Data([UInt8(length)])
        } else if length < 0x100 {
            return Data([0x81, UInt8(length)])
        } else {
            return Data([0x82, UInt8(length >> 8), UInt8(length & 0xFF)])
        }
    }

    private func derSet(_ content: Data) -> Data {
        return Data([0x31]) + derLength(content.count) + content
    }

    // MARK: - X.509 Structure Builders

    private func derName(cn: String) -> Data {
        // RDNSequence -> SET { SEQUENCE { OID(CN), UTF8String(cn) } }
        let cnOID: [UInt8] = [0x55, 0x04, 0x03]  // 2.5.4.3
        let attrTypeAndValue = derSequence(derOID(cnOID) + derUTF8String(cn))
        let rdn = derSet(attrTypeAndValue)
        return derSequence(rdn)
    }

    private func derValidity(years: Int) -> Data {
        let now = Date()
        let expiry = Calendar.current.date(byAdding: .year, value: years, to: now)!
        return derSequence(derUTCTime(now) + derUTCTime(expiry))
    }

    private func derSubjectPublicKeyInfo(_ rawECPubKey: Data) -> Data {
        // AlgorithmIdentifier: ecPublicKey + prime256v1
        let ecPubKeyOID: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01] // 1.2.840.10045.2.1
        let prime256v1OID: [UInt8] = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07] // 1.2.840.10045.3.1.7
        let algId = derSequence(derOID(ecPubKeyOID) + derOID(prime256v1OID))

        // SubjectPublicKey: the raw 65-byte uncompressed point as BIT STRING
        let pubKeyBits: Data
        if rawECPubKey.count == 65 && rawECPubKey[0] == 0x04 {
            pubKeyBits = rawECPubKey
        } else {
            // Shouldn't happen for P-256, but handle gracefully
            pubKeyBits = Data([0x04]) + rawECPubKey
        }

        return derSequence(algId + derBitString(pubKeyBits))
    }

    // MARK: - Peer certificate extraction (uses data captured in TLS verify block)

    private func extractPeerFingerprint(_ connection: NWConnection) -> String? {
        guard let der = peerCertificateData else { return nil }
        return TrustedStore.fingerprint(der)
    }

    private func extractPeerCertPEM(_ connection: NWConnection) -> String? {
        guard let der = peerCertificateData else { return nil }
        let base64 = der.base64EncodedString(options: [.lineLength64Characters, .endLineWithLineFeed])
        return "-----BEGIN CERTIFICATE-----\n\(base64)\n-----END CERTIFICATE-----"
    }
}
