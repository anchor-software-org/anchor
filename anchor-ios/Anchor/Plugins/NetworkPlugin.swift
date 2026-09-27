import Foundation
import Network
import Security
import Combine
import CryptoKit
import QuartzCore
import UIKit
import AnchorSDK
import SwiftProtobuf

/// Preserves submission order across async operations. Creating one Task per
/// input event allows a later button-up to reach the QUIC actor before the
/// earlier button-down, which can leave the host pointer stuck down.
final class OrderedAsyncQueue: @unchecked Sendable {
    private let lock = NSLock()
    private var tail: Task<Void, Never>?
    private var generation: UInt64 = 0

    func enqueue(_ operation: @escaping @Sendable () async -> Void) {
        lock.lock()
        let predecessor = tail
        let operationGeneration = generation
        let task = Task { [weak self] in
            if let predecessor {
                await predecessor.value
            }
            guard let self, self.isCurrent(operationGeneration), !Task.isCancelled else { return }
            await operation()
        }
        tail = task
        lock.unlock()
    }

    func drain() async {
        let task = lock.withLock { tail }
        await task?.value
    }

    func cancelPending() {
        lock.lock()
        generation &+= 1
        tail = nil
        lock.unlock()
    }

    private func isCurrent(_ candidate: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return candidate == generation
    }
}

/// Low-overhead receive-side telemetry. All durations use the iPad monotonic
/// clock. The desktop wall-clock timestamp is reported only as a raw clock
/// offset and as delay above the best offset observed in this run.
private final class SideboatIngressDiagnostics {
    private let startedNs = DispatchTime.now().uptimeNanoseconds
    private var windowStartedNs = DispatchTime.now().uptimeNanoseconds
    private var lastChunkNs: UInt64 = 0
    private var currentAssemblyStartedNs: UInt64?
    private var lastFrameSequence: UInt64?
    private var reportedFirstFrame = false
    private var sourceTimestampTracker = AnchorSourceTimestampTracker()

    private var chunkCount = 0
    private var chunkBytes = 0
    private var largestChunkBytes = 0
    private var maxChunkGapMs = 0.0
    private var packetCount = 0
    private var frameCount = 0
    private var frameBytes = 0
    private var sequenceGaps: UInt64 = 0
    private var maxFramerBytes = 0
    private var assemblySumMs = 0.0
    private var assemblyMaxMs = 0.0
    private var assemblySamples = 0
    private var sourceExcessSumMs = 0.0
    private var sourceExcessMaxMs = 0.0
    private var sourceSamples = 0
    private var latestRawClockOffsetMs: Double?
    private var baselineClockOffsetMs: Double?

    func recordChunk(bytes: Int, bufferedBytes: Int, atNs: UInt64) {
        if lastChunkNs != 0 {
            maxChunkGapMs = max(maxChunkGapMs, Double(atNs - lastChunkNs) / 1_000_000)
        }
        lastChunkNs = atNs
        chunkCount += 1
        chunkBytes += bytes
        largestChunkBytes = max(largestChunkBytes, bytes)
        maxFramerBytes = max(maxFramerBytes, bufferedBytes)
    }

    func recordPacket(atNs: UInt64) {
        packetCount += 1
        if currentAssemblyStartedNs == nil {
            currentAssemblyStartedNs = atNs
        }
    }

    func recordFrame(_ frame: AnchorVideoFrame, atNs: UInt64, receiverWallClockUs: UInt64) {
        frameCount += 1
        frameBytes += frame.payload.count
        if let assemblyStartedNs = currentAssemblyStartedNs {
            let assemblyMs = Double(atNs - assemblyStartedNs) / 1_000_000
            assemblySumMs += assemblyMs
            assemblyMaxMs = max(assemblyMaxMs, assemblyMs)
            assemblySamples += 1
        }
        currentAssemblyStartedNs = nil

        if let previous = lastFrameSequence,
           frame.header.sequence > previous {
            let distance = frame.header.sequence - previous
            if distance > 1 {
                sequenceGaps &+= distance - 1
            }
        }
        lastFrameSequence = frame.header.sequence

        if let observation = sourceTimestampTracker.observe(
            sourceWallClockUs: frame.header.presentationTimeUs,
            receiverWallClockUs: receiverWallClockUs
        ) {
            let excessMs = Double(observation.excessDelayUs) / 1_000
            latestRawClockOffsetMs = Double(observation.rawClockOffsetUs) / 1_000
            baselineClockOffsetMs = Double(observation.baselineClockOffsetUs) / 1_000
            sourceExcessSumMs += excessMs
            sourceExcessMaxMs = max(sourceExcessMaxMs, excessMs)
            sourceSamples += 1
        }

        if !reportedFirstFrame {
            reportedFirstFrame = true
            NSLog(
                "[anchor] [sideboat_ingress] first_access_unit_ms=%.1f sequence=%llu bytes=%d fragments=%d",
                Double(atNs - startedNs) / 1_000_000,
                frame.header.sequence,
                frame.payload.count,
                frame.header.fragmentCount
            )
        }
    }

    func reportIfDue(atNs: UInt64, bufferedBytes: Int) {
        guard atNs - windowStartedNs >= 1_000_000_000 else { return }
        maxFramerBytes = max(maxFramerBytes, bufferedBytes)
        let windowMs = Double(atNs - windowStartedNs) / 1_000_000
        let avgChunkBytes = chunkCount > 0 ? Double(chunkBytes) / Double(chunkCount) : 0
        let avgAssemblyMs = assemblySamples > 0 ? assemblySumMs / Double(assemblySamples) : 0
        let avgExcessMs = sourceSamples > 0 ? sourceExcessSumMs / Double(sourceSamples) : 0
        let rawOffsetMs = latestRawClockOffsetMs ?? .nan
        let baselineOffsetMs = baselineClockOffsetMs ?? .nan
        NSLog(
            "[anchor] [sideboat_ingress] window_ms=%.0f chunks=%d chunk_bytes=%d chunk_avg_bytes=%.0f chunk_max_bytes=%d chunk_gap_max_ms=%.1f packets=%d frames=%d frame_bytes=%d sequence_gaps=%llu framer_high_water_bytes=%d assembly_avg_ms=%.2f assembly_max_ms=%.2f source_clock_offset_ms=%.1f source_baseline_offset_ms=%.1f source_excess_avg_ms=%.2f source_excess_max_ms=%.2f",
            windowMs,
            chunkCount,
            chunkBytes,
            avgChunkBytes,
            largestChunkBytes,
            maxChunkGapMs,
            packetCount,
            frameCount,
            frameBytes,
            sequenceGaps,
            maxFramerBytes,
            avgAssemblyMs,
            assemblyMaxMs,
            rawOffsetMs,
            baselineOffsetMs,
            avgExcessMs,
            sourceExcessMaxMs
        )
        windowStartedNs = atNs
        chunkCount = 0
        chunkBytes = 0
        largestChunkBytes = 0
        maxChunkGapMs = 0
        packetCount = 0
        frameCount = 0
        frameBytes = 0
        sequenceGaps = 0
        maxFramerBytes = bufferedBytes
        assemblySumMs = 0
        assemblyMaxMs = 0
        assemblySamples = 0
        sourceExcessSumMs = 0
        sourceExcessMaxMs = 0
        sourceSamples = 0
    }
}

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
    private var displayWidth: UInt32 = 0
    private var displayHeight: UInt32 = 0
    private var batteryPercent: UInt32 = 0
    private var batteryCharging = false

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
    private var screenTask: Task<Void, Never>?
    private var screenDatagramFlowID: UInt64?
    // Desktop can retain an output that was removed in an earlier session. Do
    // one explicit selection after the initial output list arrives so a new
    // session never remains on that zero-sized source.
    private var didSelectInitialScreenOutput = false
    private var fileStreamTask: Task<Void, Never>?
    private var v1LatencyTask: Task<Void, Never>?
    private let v1PingLock = NSLock()
    private var v1PendingPings: [UInt64: UInt64] = [:]
    private var nextPingNonce: UInt64 = 1
    private var quicConnected = false
    private var capabilitySessions: [String: UInt64] = [:]
    private var nextRequestId: UInt64 = 1
    private var nextCapabilitySessionId: UInt64 = 1
    private var nextDatagramFlowId: UInt64 = 1
    private var clipboardRevision: UInt64 = 0
    private var scrollAccumulator = AnchorScrollAccumulator()
    private let v1OutboundQueue = OrderedAsyncQueue()
    private let fileCoordinator: FileTransferCoordinator

    private static let desktopEndpoint = "io.anchor.desktop"
    private static let deviceCapability = AnchorV1.Capability.device
    private static let clipboardCapability = "org.anchor.clipboard"
    private static let inputCapability = "org.anchor.input"
    private static let screenCapability = "org.anchor.screen"
    private static let mediaCapability = "org.anchor.media"
    private static let notificationsCapability = "org.anchor.notifications"
    private static let commandsCapability = "org.anchor.commands"
    private static let filesCapability = "org.anchor.files"
    private static let clipboardPublishType = "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardPublish"
    private static let clipboardClearType = "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardClear"
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
        self.fileCoordinator = FileTransferCoordinator { message in
            broker.send(AnchorEvent(target: .service("files"), message: .files(message)))
        }
    }

    // MARK: - Plugin lifecycle

    func start() {
        isRunning = true
        UIDevice.current.isBatteryMonitoringEnabled = true
        refreshBatteryState(publish: false)
        // Listen for outbound Device-targeted events
        broker.events
            .filter { $0.target == .device }
            .sink { [weak self] event in
                self?.handleOutbound(event)
            }
            .store(in: &cancellables)
        Publishers.Merge(
            NotificationCenter.default.publisher(for: UIDevice.batteryLevelDidChangeNotification),
            NotificationCenter.default.publisher(for: UIDevice.batteryStateDidChangeNotification)
        )
        .receive(on: DispatchQueue.main)
        .sink { [weak self] _ in
            self?.refreshBatteryState(publish: true)
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

    func connect(ip: String, port: Int = 5027, displayPixelSize: CGSize,
                 expectedDeviceId: String? = nil, expectedCertificate: Data? = nil) {
        guard quicTask == nil, !quicConnected else { return }
        let dimensions = Self.validatedDisplayDimensions(
            width: Double(displayPixelSize.width),
            height: Double(displayPixelSize.height)
        )
        displayWidth = dimensions.width
        displayHeight = dimensions.height
        broker.send(AnchorEvent(
            target: .gui,
            message: .json(#"{"type":"connection_status","status":"connecting","host":"\#(ip)","port":\#(port)}"#)
        ))
        quicTask = Task { [weak self] in
            await self?.connectV1(
                ip: ip,
                port: UInt16(port),
                expectedDeviceId: expectedDeviceId,
                expectedCertificate: expectedCertificate
            )
        }
    }

    private func connectV1(ip: String, port: UInt16,
                           expectedDeviceId: String? = nil,
                           expectedCertificate: Data? = nil) async {
        do {
            guard let identity = ensureIdentity() else {
                throw AnchorNetworkTransportError.connectionFailed("client identity is unavailable")
            }

            // With an advertised device id, match trust by identity — the
            // wired IP won't equal the stored `lastKnownIp`.
            let trusted = trustedStore.devices.values.first { entry in
                if let expectedDeviceId {
                    return entry.deviceId == expectedDeviceId
                }
                return entry.lastKnownIp == ip
            }
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
                // pairV1 already persisted this entry; a discovered-desktop
                // mismatch must not leave it trusted.
                if let expectedDeviceId, paired.deviceId != expectedDeviceId {
                    trustedStore.removeDevice(paired.deviceId)
                    throw AnchorNetworkTransportError.certificatePinMismatch
                }
                if let expectedCertificate,
                   certificateDER(fromPEM: paired.certificatePem) != expectedCertificate {
                    trustedStore.removeDevice(paired.deviceId)
                    throw AnchorNetworkTransportError.certificatePinMismatch
                }
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

        for capability in [Self.deviceCapability, Self.clipboardCapability, Self.inputCapability, Self.mediaCapability, Self.notificationsCapability, Self.commandsCapability, Self.filesCapability, Self.screenCapability] {
            let advertised = remote.endpoints.contains { endpoint in
                endpoint.endpointID == Self.desktopEndpoint && endpoint.capabilities.contains {
                    $0.name == capability && $0.major == 1
                }
            }
            if advertised {
                try await openV1Capability(capability, session: session)
                if capability == Self.deviceCapability {
                    try await sendV1DeviceState(session: session)
                } else if capability == Self.mediaCapability {
                    broker.send(AnchorEvent(
                        target: .service("media"), message: .media(.availability(true))
                    ))
                } else if capability == Self.notificationsCapability {
                    broker.send(AnchorEvent(
                        target: .service("notifications"), message: .notification(.availability(true))
                    ))
                } else if capability == Self.commandsCapability {
                    broker.send(AnchorEvent(
                        target: .service("commands"), message: .commands(.availability(true))
                    ))
                } else if capability == Self.filesCapability {
                    broker.send(AnchorEvent(
                        target: .service("files"), message: .files(.availability(true))
                    ))
                }
            } else {
                NSLog("[anchor] Desktop did not advertise \(capability)@1")
            }
        }
        if capabilitySessions[Self.screenCapability] != nil {
            try await openV1ScreenDatagramFlow(session: session)
        }
        if capabilitySessions[Self.filesCapability] != nil {
            startV1FileStreamReceiver(transport: transport)
        }
    }

    static func validatedDisplayDimensions(width: Double, height: Double) -> (width: UInt32, height: UInt32) {
        guard width.isFinite,
              height.isFinite,
              width >= 1,
              height >= 1,
              width.rounded() == width,
              height.rounded() == height,
              width <= Double(UInt32.max),
              height <= Double(UInt32.max) else {
            return (0, 0)
        }
        return (UInt32(width), UInt32(height))
    }

    /// Choose a source that can produce a frame. Output lists may still
    /// contain stale or virtual entries with no dimensions, so do not select
    /// those as the automatic initial source.
    static func initialScreenOutputID(_ outputs: [ANCHScreenScreenOutput]) -> String? {
        outputs.first(where: {
            !$0.outputID.isEmpty && $0.width > 0 && $0.height > 0
        })?.outputID
    }

    static func makeDeviceState(
        deviceName: String,
        batteryPercent: UInt32,
        charging: Bool,
        displayWidth: UInt32,
        displayHeight: UInt32
    ) -> ANCHDeviceDeviceState {
        var state = ANCHDeviceDeviceState()
        state.deviceName = deviceName
        state.batteryPercent = min(batteryPercent, 100)
        state.charging = charging
        state.displayWidth = displayWidth
        state.displayHeight = displayHeight
        return state
    }

    static func validatedBatteryPercent(level: Float) -> UInt32? {
        guard level.isFinite, level >= 0 else { return nil }
        return UInt32((min(level, 1) * 100).rounded())
    }

    private func refreshBatteryState(publish: Bool) {
        let device = UIDevice.current
        let percent = Self.validatedBatteryPercent(level: device.batteryLevel) ?? 0
        let charging = device.batteryState == .charging || device.batteryState == .full
        guard percent != batteryPercent || charging != batteryCharging else { return }
        batteryPercent = percent
        batteryCharging = charging

        guard publish,
              let session = quicSession,
              capabilitySessions[Self.deviceCapability] != nil else { return }
        Task { [weak self] in
            do {
                try await self?.sendV1DeviceState(session: session)
            } catch {
                NSLog("[anchor] Device battery update failed: \(error.localizedDescription)")
            }
        }
    }

    private func sendV1DeviceState(session: AnchorSession) async throws {
        guard let capabilityID = capabilitySessions[Self.deviceCapability] else { return }
        let state = Self.makeDeviceState(
            deviceName: deviceName,
            batteryPercent: batteryPercent,
            charging: batteryCharging,
            displayWidth: displayWidth,
            displayHeight: displayHeight
        )
        try await sendV1Record(
            capabilityID,
            type: AnchorV1.TypeURL.deviceState,
            payload: try state.serializedData(),
            session: session
        )
        NSLog(
            "[anchor] Device state sent: battery=%u charging=%d display=%ux%u",
            batteryPercent,
            batteryCharging ? 1 : 0,
            displayWidth,
            displayHeight
        )
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
            if case .capabilityRecord(let record)? = reply.body {
                // A provider may publish initial state immediately after its
                // capability opens (commands sends its list this way). Route
                // that record while continuing to wait for this open reply.
                try await routeV1Record(record)
                continue
            }
            if case .protocolError(let error)? = reply.body {
                throw AnchorNetworkTransportError.connectionFailed("capability rejected: \(error.message)")
            }
            throw AnchorNetworkTransportError.connectionFailed("unexpected reply while opening \(name)")
        }
    }

    private func openV1ScreenDatagramFlow(session: AnchorSession) async throws {
        guard let capabilityID = capabilitySessions[Self.screenCapability] else { return }
        let requestID = nextRequestId
        nextRequestId += 1
        let flowID = nextDatagramFlowId
        nextDatagramFlowId += 2
        let open = try AnchorDatagramFlowBindingCodec.openEnvelope(
            requestID: requestID,
            flowID: flowID,
            capabilitySessionID: capabilityID,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )
        try await session.sendEnvelope(try open.serializedData())

        while true {
            let reply = try ANCHControlEnvelope(serializedBytes: await session.receiveEnvelope())
            if (try? AnchorDatagramFlowBindingCodec.validateOpened(
                reply,
                requestID: requestID,
                flowID: flowID
            )) != nil {
                break
            }
            if case .ping(let ping)? = reply.body {
                var pong = ANCHPong(); pong.nonce = ping.nonce
                var pongEnvelope = ANCHControlEnvelope(); pongEnvelope.pong = pong
                try await session.sendEnvelope(try pongEnvelope.serializedData())
            } else if case .capabilityRecord(let record)? = reply.body {
                try await routeV1Record(record)
            } else if case .protocolError(let error)? = reply.body {
                throw AnchorNetworkTransportError.connectionFailed(
                    "screen datagram flow rejected: \(error.message)"
                )
            } else {
                NSLog("[anchor] Ignoring unrelated record while binding screen datagram flow")
            }
        }

        // On iOS 26, accessing the QUIC datagram channel enables inbound
        // delivery. This must finish before the desktop receives screenStart,
        // or it can send and lose the only requested recovery IDR.
        try await session.prepareDatagramReceive()
        screenDatagramFlowID = flowID
        screenTask = Task { [weak self] in
            await self?.readV1ScreenDatagrams(
                session,
                capabilityID: capabilityID,
                flowID: flowID
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
        NSLog("[anchor] Sideboat datagram flow bound: flow \(flowID)")
    }

    private func readV1ScreenDatagrams(
        _ session: AnchorSession,
        capabilityID: UInt64,
        flowID: UInt64
    ) async {
        var assembler = AnchorScreenDatagramAssembler(
            capabilitySessionID: capabilityID,
            flowID: flowID
        )
        let diagnostics = SideboatIngressDiagnostics()
        let recoveryRequestIntervalNs: UInt64 = 500_000_000
        var lastRecoveryRequestNs = DispatchTime.now().uptimeNanoseconds
        do {
            while !Task.isCancelled {
                let packet = try await session.receiveDatagram()
                let packetReceivedNs = DispatchTime.now().uptimeNanoseconds
                diagnostics.recordChunk(
                    bytes: packet.count,
                    bufferedBytes: 0,
                    atNs: packetReceivedNs
                )
                diagnostics.recordPacket(atNs: packetReceivedNs)
                do {
                    let frames = try assembler.consume(
                        packet,
                        atNanoseconds: packetReceivedNs
                    )
                    for frame in frames {
                        let frameReceivedNs = DispatchTime.now().uptimeNanoseconds
                        let receiverWallClockUs = UInt64(Date().timeIntervalSince1970 * 1_000_000)
                        diagnostics.recordFrame(
                            frame,
                            atNs: frameReceivedNs,
                            receiverWallClockUs: receiverWallClockUs
                        )
                        videoPlugin.feedFrame(
                            frame.payload,
                            frameId: frame.header.sequence,
                            sourcePresentationTimeUs: frame.header.presentationTimeUs
                        )
                    }
                    if assembler.takeKeyframeRequest(),
                       packetReceivedNs >= lastRecoveryRequestNs,
                       packetReceivedNs - lastRecoveryRequestNs >= recoveryRequestIntervalNs {
                        lastRecoveryRequestNs = packetReceivedNs
                        try await sendV1Record(
                            capabilityID,
                            type: AnchorV1.TypeURL.screenRequestKeyframe,
                            payload: try AnchorScreenCodec.requestKeyframe(),
                            session: session
                        )
                        NSLog("[anchor] Sideboat loss recovery requested an IDR")
                    }
                } catch {
                    // Datagram loss, reordering, or another negotiated media
                    // flow must not terminate the screen receiver.
                    NSLog("[anchor] Dropping invalid Sideboat datagram: \(error)")
                }
                diagnostics.reportIfDue(
                    atNs: DispatchTime.now().uptimeNanoseconds,
                    bufferedBytes: 0
                )
            }
        } catch is CancellationError {
            return
        } catch {
            NSLog("[anchor] Sideboat datagram receive failed: \(error)")
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
                    try await routeV1Record(record)
                case .streamOpen(let open):
                    try await acceptV1FileStream(open, requestID: envelope.requestID, session: session)
                case .streamOpened(let opened):
                    await fileCoordinator.recordOpened(
                        responseTo: envelope.responseTo,
                        streamID: opened.quicStreamID
                    )
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

    private func routeV1Record(_ record: ANCHCapabilityRecord) async throws {
        if capabilitySessions[Self.screenCapability] == record.capabilitySessionID {
            if record.typeURL == AnchorV1.TypeURL.screenStatus {
                let status = try ANCHScreenScreenStatus(serializedBytes: record.payload)
                let info: [String: Any] = [
                    "type": "stream_info",
                    "output_id": status.outputID,
                    "width": status.width,
                    "height": status.height,
                    "fps": status.fps,
                    "bitrate_kbps": status.bitrateKbps,
                ]
                let data = try JSONSerialization.data(withJSONObject: info)
                videoPlugin.handleStreamInfo(String(decoding: data, as: UTF8.self))
            } else if record.typeURL == AnchorV1.TypeURL.screenOutputList {
                let outputs = try ANCHScreenScreenOutputList(serializedBytes: record.payload)
                videoPlugin.handleOutputs(outputs.outputs)
                NSLog("[anchor] Sideboat outputs received: \(outputs.outputs.count)")
                if !didSelectInitialScreenOutput,
                   let outputID = Self.initialScreenOutputID(outputs.outputs),
                   let session = quicSession,
                   let capabilityID = capabilitySessions[Self.screenCapability] {
                    didSelectInitialScreenOutput = true
                    try await sendV1Record(
                        capabilityID,
                        type: AnchorV1.TypeURL.screenSelectOutput,
                        payload: try AnchorScreenCodec.selectOutput(outputID),
                        session: session
                    )
                    // The first start used the desktop's default output. Send
                    // a second, explicit start after selecting a known-good
                    // source, then request its recovery IDR.
                    try await sendV1Record(
                        capabilityID,
                        type: AnchorV1.TypeURL.screenStart,
                        payload: try AnchorScreenCodec.start(outputID: outputID),
                        session: session
                    )
                    try await sendV1Record(
                        capabilityID,
                        type: AnchorV1.TypeURL.screenRequestKeyframe,
                        payload: try AnchorScreenCodec.requestKeyframe(),
                        session: session
                    )
                    NSLog("[anchor] Sideboat selected initial output \(outputID)")
                }
            } else {
                NSLog("[anchor] Ignoring unknown screen record \(record.typeURL)")
            }
            return
        }
        if capabilitySessions[Self.mediaCapability] == record.capabilitySessionID {
            guard record.typeURL == AnchorV1.TypeURL.mediaState else {
                NSLog("[anchor] Ignoring unknown media record \(record.typeURL)")
                return
            }
            let state = try AnchorMediaCodec.decodeState(record.payload)
            broker.send(AnchorEvent(target: .service("media"), message: .media(.state(state))))
            return
        }
        if capabilitySessions[Self.notificationsCapability] == record.capabilitySessionID {
            guard record.typeURL == AnchorV1.TypeURL.notificationPosted else {
                NSLog("[anchor] Ignoring unsupported notification record \(record.typeURL)")
                return
            }
            guard let notification = try Self.decodeDesktopNotification(record.payload) else {
                NSLog("[anchor] Ignoring incomplete desktop notification")
                return
            }
            broker.send(AnchorEvent(
                target: .service("notifications"),
                message: .notification(.posted(notification))
            ))
            return
        }
        if capabilitySessions[Self.commandsCapability] == record.capabilitySessionID {
            if record.typeURL == AnchorV1.TypeURL.commandList {
                let commands = try Self.decodeCommandList(record.payload)
                broker.send(AnchorEvent(target: .service("commands"), message: .commands(.list(commands))))
            } else if record.typeURL == AnchorV1.TypeURL.commandResult {
                guard let result = try Self.decodeCommandResult(record.payload) else {
                    NSLog("[anchor] Ignoring command result with unknown status")
                    return
                }
                broker.send(AnchorEvent(target: .service("commands"), message: .commands(.result(result))))
            } else {
                NSLog("[anchor] Ignoring unknown command record \(record.typeURL)")
            }
            return
        }
        if capabilitySessions[Self.filesCapability] == record.capabilitySessionID {
            switch record.typeURL {
            case AnchorV1.TypeURL.fileOffer:
                let offer = try AnchorFilesCodec.decodeOffer(
                    record.payload,
                    maximumByteLength: FileTransferCoordinator.maximumFileBytes
                )
                let accepted = await fileCoordinator.accept(offer)
                guard let session = quicSession else { return }
                try await sendV1Record(
                    record.capabilitySessionID,
                    type: AnchorV1.TypeURL.fileDecision,
                    payload: try AnchorFilesCodec.decision(
                        transferID: offer.transferID,
                        accepted: accepted
                    ),
                    session: session
                )
            case AnchorV1.TypeURL.fileDecision:
                await fileCoordinator.recordDecision(try AnchorFilesCodec.decodeDecision(record.payload))
            case AnchorV1.TypeURL.fileContentStart:
                await fileCoordinator.bind(try AnchorFilesCodec.decodeContentStart(record.payload))
            case AnchorV1.TypeURL.fileComplete:
                await fileCoordinator.complete(try AnchorFilesCodec.decodeComplete(record.payload))
            default:
                NSLog("[anchor] Ignoring unknown files record \(record.typeURL)")
            }
            return
        }
        guard capabilitySessions[Self.clipboardCapability] == record.capabilitySessionID else {
            NSLog("[anchor] Ignoring unbound capability record \(record.typeURL)")
            return
        }
        if record.typeURL == Self.clipboardPublishType {
            let publish = try AnchorClipboardCodec.decodePublish(record.payload)
            guard !AnchorClipboardCodec.isEcho(publish, localNodeID: localNodeId) else { return }
            let content: ClipboardWireContent
            switch publish.content {
            case .textUtf8(let text): content = .text(text)
            case .png(let png): content = .png(png)
            case nil: return
            }
            broker.send(AnchorEvent(
                target: .service("clipboard"),
                message: .clipboard(.publishRemote(
                    originNodeID: publish.originNodeID.value,
                    revision: publish.revision,
                    content: content
                ))
            ))
        } else if record.typeURL == Self.clipboardClearType {
            let clear = try AnchorClipboardCodec.decodeClear(record.payload)
            guard clear.originNodeID.value != localNodeId else { return }
            broker.send(AnchorEvent(
                target: .service("clipboard"),
                message: .clipboard(.clearRemote(
                    originNodeID: clear.originNodeID.value,
                    revision: clear.revision
                ))
            ))
        } else {
            NSLog("[anchor] Ignoring unknown clipboard record \(record.typeURL)")
        }
    }

    static func decodeDesktopNotification(_ payload: Data, now: Date = Date()) throws -> DesktopNotification? {
        guard payload.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        let posted = try ANCHNotificationsNotificationPosted(serializedBytes: payload)
        guard !posted.notificationID.isEmpty,
              !posted.applicationName.isEmpty,
              !(posted.title.isEmpty && posted.body.isEmpty) else { return nil }
        let postedAt = posted.postedAtUnixMs == 0
            ? now
            : Date(timeIntervalSince1970: TimeInterval(posted.postedAtUnixMs) / 1_000)
        return DesktopNotification(
            id: posted.notificationID,
            applicationName: posted.applicationName,
            title: posted.title,
            body: posted.body,
            postedAt: postedAt
        )
    }

    static func decodeCommandList(_ payload: Data) throws -> [RemoteCommandDefinition] {
        guard payload.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        let list = try ANCHCommandsCommandList(serializedBytes: payload)
        return list.commands.compactMap { command in
            guard !command.id.isEmpty else { return nil }
            return RemoteCommandDefinition(
                id: command.id,
                name: command.name.isEmpty ? command.id : command.name,
                description: command.description_p,
                detached: command.detach
            )
        }
    }

    static func decodeCommandResult(_ payload: Data) throws -> RemoteCommandResult? {
        guard payload.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        let result = try ANCHCommandsCommandResult(serializedBytes: payload)
        guard let status = RemoteCommandResult.Status(rawValue: result.status) else { return nil }
        return RemoteCommandResult(
            commandID: result.commandID,
            executionID: result.executionID,
            status: status,
            exitCode: result.exitCode,
            error: result.error
        )
    }

    private var localNodeId: Data {
        Data(SHA256.hash(data: Data(deviceId.utf8)))
    }

    private func iosEndpointAdvertisement() -> ANCHEndpointAdvertisement {
        var endpoint = ANCHEndpointAdvertisement()
        endpoint.endpointID = "io.anchor.ios"
        // This client consumes capabilities exposed by the desktop. It does
        // not expose iPad notification capture or other server-side services.
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
        if quicConnected, case .files(.send(let url)) = event.message {
            v1OutboundQueue.enqueue { [weak self] in
                await self?.sendV1File(url)
            }
            return
        }
        if quicConnected, case .media(.command(let command)) = event.message {
            v1OutboundQueue.enqueue { [weak self] in
                do { try await self?.sendV1Media(command) }
                catch { NSLog("[anchor] v1 media send failed: \(error)") }
            }
            return
        }
        if quicConnected, case .clipboard(let message) = event.message {
            v1OutboundQueue.enqueue { [weak self] in
                do { try await self?.sendV1Clipboard(message) }
                catch { NSLog("[anchor] v1 clipboard send failed: \(error)") }
            }
            return
        }
        if quicConnected, case .commands(let message) = event.message {
            v1OutboundQueue.enqueue { [weak self] in
                do { try await self?.sendV1Command(message) }
                catch { NSLog("[anchor] v1 command send failed: \(error)") }
            }
            return
        }
        if quicConnected, case .json(let payload) = event.message {
            v1OutboundQueue.enqueue { [weak self] in
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
        case .clipboard:
            // Typed clipboard records are Protocol v1-only.
            break
        case .media:
            break
        case .notification:
            // Desktop → iPad only. Notification records are never sent upstream.
            break
        case .commands:
            break
        case .files:
            break
        }
    }

    private func startV1FileStreamReceiver(transport: any AnchorNetworkQuicTransport) {
        fileStreamTask?.cancel()
        fileStreamTask = Task { [weak self] in
            do {
                while !Task.isCancelled {
                    let stream = try await transport.receiveReliableStream()
                    await self?.fileCoordinator.register(stream)
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                NSLog("[anchor] Files inbound stream receiver stopped: \(error)")
            }
        }
    }

    private func acceptV1FileStream(
        _ open: ANCHStreamOpen,
        requestID: UInt64,
        session: AnchorSession
    ) async throws {
        guard requestID != 0,
              capabilitySessions[Self.filesCapability] == open.capabilitySessionID,
              open.payloadTypeURL == AnchorV1.TypeURL.fileContent else {
            throw AnchorNetworkTransportError.connectionFailed("invalid files stream binding")
        }
        var opened = ANCHStreamOpened()
        opened.quicStreamID = open.quicStreamID
        var response = ANCHControlEnvelope()
        response.responseTo = requestID
        response.streamOpened = opened
        try await session.sendEnvelope(try response.serializedData())
    }

    private func sendV1File(_ url: URL) async {
        let transferIDByteCount = AnchorFilesCodec.transferIDByteCount
        var transferID = Data(count: transferIDByteCount)
        let status = transferID.withUnsafeMutableBytes {
            SecRandomCopyBytes(kSecRandomDefault, transferIDByteCount, $0.baseAddress!)
        }
        guard status == errSecSuccess else { return }
        let id = FileTransferCoordinator.identifier(transferID)
        let accessed = url.startAccessingSecurityScopedResource()
        defer { if accessed { url.stopAccessingSecurityScopedResource() } }

        do {
            guard let session = quicSession,
                  let transport = quicTransport,
                  let capabilityID = capabilitySessions[Self.filesCapability] else {
                throw AnchorNetworkTransportError.connectionClosed
            }
            let values = try url.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
            guard values.isRegularFile == true else {
                throw CocoaError(.fileReadUnsupportedScheme)
            }
            let byteLength = UInt64(values.fileSize ?? 0)
            guard byteLength > 0 else { throw FileTransferError.emptyFile }
            guard byteLength <= FileTransferCoordinator.maximumFileBytes else {
                throw FileTransferError.fileTooLarge
            }

            broker.send(AnchorEvent(target: .service("files"), message: .files(.began(
                FileTransferEntry(
                    id: id,
                    name: url.lastPathComponent,
                    direction: .sent,
                    totalBytes: byteLength,
                    transferredBytes: 0,
                    status: .transferring,
                    localURL: url,
                    startedAt: Date()
                )
            ))))

            let hash = try Self.hashFile(url)
            let offer = AnchorFileOffer(
                transferID: transferID,
                filename: url.lastPathComponent,
                mimeType: "application/octet-stream",
                byteLength: byteLength,
                sha256: hash
            )
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.fileOffer,
                payload: try AnchorFilesCodec.offer(
                    offer, maximumByteLength: FileTransferCoordinator.maximumFileBytes
                ),
                session: session
            )
            try await fileCoordinator.waitForDecision(transferID: transferID)

            let stream = try await transport.openReliableStream()
            let requestID = nextRequestId
            nextRequestId += 1
            let open = try AnchorStreamBindingCodec.openEnvelope(
                requestID: requestID,
                streamID: stream.streamIdentifier,
                capabilitySessionID: capabilityID,
                payloadTypeURL: AnchorV1.TypeURL.fileContent
            )
            try await session.sendEnvelope(try open.serializedData())
            try await fileCoordinator.waitForOpened(
                requestID: requestID,
                streamID: stream.streamIdentifier
            )
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.fileContentStart,
                payload: try AnchorFilesCodec.contentStart(
                    transferID: transferID,
                    quicStreamID: stream.streamIdentifier
                ),
                session: session
            )

            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            var sent: UInt64 = 0
            while let chunk = try handle.read(upToCount: 256 * 1024), !chunk.isEmpty {
                try await stream.send(chunk)
                sent += UInt64(chunk.count)
                broker.send(AnchorEvent(
                    target: .service("files"),
                    message: .files(.progress(id: id, transferredBytes: sent))
                ))
            }
            guard sent == byteLength else { throw FileTransferError.unexpectedLength }
            try await stream.finish()
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.fileComplete,
                payload: try AnchorFilesCodec.complete(transferID: transferID, sha256: hash),
                session: session
            )
            broker.send(AnchorEvent(
                target: .service("files"), message: .files(.completed(id: id, localURL: url))
            ))
        } catch {
            broker.send(AnchorEvent(
                target: .service("files"),
                message: .files(.failed(id: id, message: error.localizedDescription))
            ))
            NSLog("[anchor] File send failed for \(url.lastPathComponent): \(error)")
        }
    }

    private static func hashFile(_ url: URL) throws -> Data {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 256 * 1024), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return Data(hasher.finalize())
    }

    private func sendV1Media(_ command: AnchorMediaCommand) async throws {
        guard let session = quicSession,
              let capabilityID = capabilitySessions[Self.mediaCapability] else { return }
        try await sendV1Record(
            capabilityID,
            type: AnchorV1.TypeURL.mediaCommand,
            payload: try AnchorMediaCodec.command(command),
            session: session
        )
    }

    private func sendV1Command(_ message: CommandWireMessage) async throws {
        guard let session = quicSession,
              let capabilityID = capabilitySessions[Self.commandsCapability] else { return }
        switch message {
        case .run(let commandID):
            guard !commandID.isEmpty else { return }
            var run = ANCHCommandsCommandRun()
            run.commandID = commandID
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.commandRun,
                payload: try run.serializedData(),
                session: session
            )
        case .kill(let executionID):
            guard !executionID.isEmpty else { return }
            var kill = ANCHCommandsCommandKill()
            kill.executionID = executionID
            try await sendV1Record(
                capabilityID,
                type: AnchorV1.TypeURL.commandKill,
                payload: try kill.serializedData(),
                session: session
            )
        case .availability, .list, .result:
            break
        }
    }

    private func sendV1Clipboard(_ message: ClipboardWireMessage) async throws {
        guard let session = quicSession,
              let capabilityID = capabilitySessions[Self.clipboardCapability] else { return }
        clipboardRevision &+= 1
        if clipboardRevision == 0 { clipboardRevision = 1 }
        switch message {
        case .publishLocal(.text(let text)):
            try await sendV1Record(
                capabilityID,
                type: Self.clipboardPublishType,
                payload: try AnchorClipboardCodec.publishText(
                    originNodeID: localNodeId, revision: clipboardRevision, text: text
                ),
                session: session
            )
        case .publishLocal(.png(let png)):
            try await sendV1Record(
                capabilityID,
                type: Self.clipboardPublishType,
                payload: try AnchorClipboardCodec.publishPNG(
                    originNodeID: localNodeId, revision: clipboardRevision, png: png
                ),
                session: session
            )
        case .clearLocal:
            try await sendV1Record(
                capabilityID,
                type: Self.clipboardClearType,
                payload: try AnchorClipboardCodec.clear(
                    originNodeID: localNodeId, revision: clipboardRevision
                ),
                session: session
            )
        case .publishRemote, .clearRemote:
            return
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
            guard let command = json["command"] as? String,
                  let capabilityID = capabilitySessions[Self.screenCapability] else { return }
            switch command {
            case "request_keyframe":
                try await sendV1Record(
                    capabilityID,
                    type: AnchorV1.TypeURL.screenRequestKeyframe,
                    payload: try AnchorScreenCodec.requestKeyframe(),
                    session: session
                )
            case "select_output":
                guard let outputID = json["output_id"] as? String, !outputID.isEmpty else { return }
                try await sendV1Record(
                    capabilityID,
                    type: AnchorV1.TypeURL.screenSelectOutput,
                    payload: try AnchorScreenCodec.selectOutput(outputID),
                    session: session
                )
            default:
                return
            }
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
        if capabilitySessions[Self.mediaCapability] != nil {
            broker.send(AnchorEvent(
                target: .service("media"), message: .media(.availability(false))
            ))
        }
        if capabilitySessions[Self.notificationsCapability] != nil {
            broker.send(AnchorEvent(
                target: .service("notifications"), message: .notification(.availability(false))
            ))
        }
        if capabilitySessions[Self.commandsCapability] != nil {
            broker.send(AnchorEvent(
                target: .service("commands"), message: .commands(.availability(false))
            ))
        }
        if capabilitySessions[Self.filesCapability] != nil {
            broker.send(AnchorEvent(
                target: .service("files"), message: .files(.availability(false))
            ))
        }
        stopLatencyPing()
        stopV1Latency()
        v1OutboundQueue.cancelPending()
        quicConnected = false
        quicTransport?.close()
        quicTransport = nil
        quicSession = nil
        screenTask?.cancel()
        screenTask = nil
        screenDatagramFlowID = nil
        didSelectInitialScreenOutput = false
        fileStreamTask?.cancel()
        fileStreamTask = nil
        Task { await fileCoordinator.reset() }
        videoPlugin.stop()
        quicTask?.cancel()
        quicTask = nil
        capabilitySessions.removeAll()
        nextRequestId = 1
        nextCapabilitySessionId = 1
        nextDatagramFlowId = 1
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
