import Foundation
import Combine
import UIKit

/// Main view model — mirrors Android's MainViewModel.
/// Orchestrates plugins, connection state, pairing, and UI state.
@MainActor
class MainViewModel: ObservableObject {
    struct PreviewState {
        var logs: String = "Preview ready\n"
        var desktopIp: String = "192.168.1.42"
        var connectionState = ConnectionState()
        var pairingState: PairingState = .idle
        var pairedDevices: [PairedDeviceDisplay] = []
        var latencyMs: Int = 0
        var touchInputOnStream: Bool = true
        var touchpadSensitivity: Float = 1.5
        var autoSyncClipboard: Bool = false
        var videoState = VideoPlugin.PreviewState()
        var clipboardHistory: [ClipboardHistoryEntry] = []
    }

    // Core infrastructure
    private let broker = MessageBroker()
    private let trustedStore = TrustedStore()
    let videoPlugin: VideoPlugin
    let inputPlugin: InputPlugin
    private let networkPlugin: NetworkPlugin
    let clipboardPlugin: ClipboardPlugin
    let mediaPlugin: MediaPlugin
    let notificationPlugin: NotificationPlugin
    let commandsPlugin: CommandsPlugin
    let filesPlugin: FilesPlugin

    // Device identity
    private let myDeviceId: String
    private let myDeviceName: String = UIDevice.current.name
    private let isPreviewMode: Bool

    // Persisted connection IP
    @Published var desktopIp: String {
        didSet {
            guard !isPreviewMode else { return }
            UserDefaults.standard.set(desktopIp, forKey: "desktop_ip")
        }
    }

    // UI State
    @Published var logs: String = "Ready to connect\n"
    @Published var connectionState = ConnectionState()
    @Published var pairingState: PairingState = .idle
    @Published var pairedDevices: [PairedDeviceDisplay] = []
    @Published var latencyMs: Int = 0

    // Input settings
    @Published var touchInputOnStream: Bool {
        didSet {
            guard !isPreviewMode else { return }
            UserDefaults.standard.set(touchInputOnStream, forKey: "touch_input_on_stream")
        }
    }
    @Published var touchpadSensitivity: Float {
        didSet {
            guard !isPreviewMode else { return }
            UserDefaults.standard.set(touchpadSensitivity, forKey: "touchpad_sensitivity")
        }
    }
    @Published var autoSyncClipboard: Bool {
        didSet {
            guard !isPreviewMode else {
                clipboardPlugin.autoSyncEnabled = autoSyncClipboard
                return
            }
            UserDefaults.standard.set(autoSyncClipboard, forKey: "auto_sync_clipboard")
            clipboardPlugin.autoSyncEnabled = autoSyncClipboard
        }
    }

    private var cancellables = Set<AnyCancellable>()
    private var connectionTimeoutTask: Task<Void, Never>?
    private var lastConnectedIp: String?
    private var connectedDeviceId: String?
    private var startupAutoConnectTask: Task<Void, Never>?

    private enum Mode {
        case live
        case preview(PreviewState)
    }

    convenience init() {
        self.init(mode: .live)
    }

    private init(mode: Mode) {
        let previewState: PreviewState?

        switch mode {
        case .live:
            isPreviewMode = false
            myDeviceId = Self.getOrCreateDeviceId()
            desktopIp = UserDefaults.standard.string(forKey: "desktop_ip") ?? ""
            if UserDefaults.standard.object(forKey: "touch_input_on_stream") == nil {
                touchInputOnStream = true
            } else {
                touchInputOnStream = UserDefaults.standard.bool(forKey: "touch_input_on_stream")
            }
            touchpadSensitivity = UserDefaults.standard.object(forKey: "touchpad_sensitivity") as? Float ?? 1.5
            autoSyncClipboard = UserDefaults.standard.bool(forKey: "auto_sync_clipboard")
            previewState = nil
        case .preview(let state):
            isPreviewMode = true
            myDeviceId = "preview-ios-device"
            desktopIp = state.desktopIp
            touchInputOnStream = state.touchInputOnStream
            touchpadSensitivity = state.touchpadSensitivity
            autoSyncClipboard = state.autoSyncClipboard
            logs = state.logs
            connectionState = state.connectionState
            pairingState = state.pairingState
            pairedDevices = state.pairedDevices
            latencyMs = state.latencyMs
            previewState = state
        }

        inputPlugin = InputPlugin(broker: broker)
        videoPlugin = VideoPlugin(broker: broker, previewState: previewState?.videoState ?? .init())
        clipboardPlugin = ClipboardPlugin(
            broker: broker,
            initialHistory: previewState?.clipboardHistory ?? [],
            initialChangeCount: previewState == nil ? nil : 0
        )
        mediaPlugin = MediaPlugin(broker: broker)
        notificationPlugin = NotificationPlugin(broker: broker)
        commandsPlugin = CommandsPlugin(broker: broker)
        filesPlugin = FilesPlugin(broker: broker)
        networkPlugin = NetworkPlugin(
            broker: broker,
            videoPlugin: videoPlugin,
            trustedStore: trustedStore,
            deviceId: myDeviceId,
            deviceName: myDeviceName,
            isTablet: UIDevice.current.userInterfaceIdiom == .pad
        )
        clipboardPlugin.autoSyncEnabled = autoSyncClipboard

        guard case .live = mode else { return }

        // Start plugins
        networkPlugin.start()
        videoPlugin.start()
        inputPlugin.start()
        clipboardPlugin.start()
        mediaPlugin.start()
        notificationPlugin.start()
        commandsPlugin.start()
        filesPlugin.start()

        // Refresh paired devices
        refreshPairedDevices()

        // Subscribe to GUI-targeted events
        broker.events
            .receive(on: DispatchQueue.main)
            .sink { [weak self] event in
                switch event.target {
                case .gui, .broadcast:
                    self?.handleGuiEvent(event)
                default:
                    break
                }
            }
            .store(in: &cancellables)

        // Auto-reconnect when app returns to foreground
        NotificationCenter.default.publisher(for: UIApplication.willEnterForegroundNotification)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.handleForegroundReturn()
            }
            .store(in: &cancellables)

        // Mirror Android behavior: try the last known desktop on startup.
        startupAutoConnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            self?.ensureAutoConnect()
        }
    }

    deinit {
        networkPlugin.stop()
        videoPlugin.stop()
        clipboardPlugin.stop()
        mediaPlugin.stop()
        notificationPlugin.stop()
        commandsPlugin.stop()
        filesPlugin.stop()
    }

    // MARK: - Event handling

    private func handleGuiEvent(_ event: AnchorEvent) {
        switch event.message {
        case .json(let payload):
            if payload.contains("\"type\":\"pairing_request\"") {
                parsePairingRequest(payload)
            } else {
                parseJsonMessage(payload)
                logs += "\(payload)\n"
            }
        case .generic(let text):
            logs += "\(text)\n"
        case .binary:
            break
        case .clipboard:
            break
        case .media:
            break
        case .notification:
            break
        case .commands:
            break
        case .files:
            break
        }
    }

    private func parsePairingRequest(_ payload: String) {
        guard let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }

        pairingState = .requested(
            deviceId: json["device_id"] as? String ?? "unknown",
            deviceName: json["device_name"] as? String ?? "Unknown",
            deviceType: json["device_type"] as? String ?? "unknown",
            fingerprint: json["fingerprint"] as? String ?? "N/A",
            certificatePem: (json["certificate_pem"] as? String ?? "").replacingOccurrences(of: "\\n", with: "\n")
        )
    }

    func respondToPairing(accepted: Bool) {
        guard !isPreviewMode else {
            pairingState = accepted ? .accepted : .rejected
            return
        }
        pairingState = accepted ? .accepted : .rejected
        networkPlugin.respondToPairing(accepted: accepted)

        Task {
            try? await Task.sleep(nanoseconds: 500_000_000)
            pairingState = .idle
            if accepted { refreshPairedDevices() }
        }
    }

    func unpairDevice(_ deviceId: String) {
        guard !isPreviewMode else {
            pairedDevices.removeAll { $0.deviceId == deviceId }
            return
        }
        trustedStore.removeDevice(deviceId)
        refreshPairedDevices()
    }

    private func refreshPairedDevices() {
        pairedDevices = trustedStore.devices.values.map { entry in
            let fingerprint: String
            if !entry.certificatePem.isEmpty {
                let base64 = entry.certificatePem
                    .replacingOccurrences(of: "-----BEGIN CERTIFICATE-----", with: "")
                    .replacingOccurrences(of: "-----END CERTIFICATE-----", with: "")
                    .replacingOccurrences(of: "\n", with: "")
                    .trimmingCharacters(in: .whitespaces)
                if let der = Data(base64Encoded: base64) {
                    fingerprint = TrustedStore.fingerprint(der)
                } else {
                    fingerprint = "N/A"
                }
            } else {
                fingerprint = "N/A"
            }

            return PairedDeviceDisplay(
                deviceId: entry.deviceId,
                deviceName: entry.deviceName,
                fingerprint: fingerprint,
                pairedAt: entry.pairedAt,
                lastSeen: entry.lastSeen,
                isOnline: networkPlugin.isConnected && entry.deviceId == connectedDeviceId,
                lastKnownIp: entry.lastKnownIp
            )
        }
    }

    private func parseJsonMessage(_ payload: String) {
        guard let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }

        let type = json["type"] as? String

        if type == "latency_update" {
            if let rtt = json["rtt_ms"] as? Double {
                latencyMs = Int(rtt / 2) // one-way estimate
            }
            return
        }

        if type == "connection_status" {
            let status = json["status"] as? String
            let host = json["host"] as? String ?? ""
            let port = json["port"] as? Int ?? 0
            let error = json["error"] as? String
            let reason = json["reason"] as? String

            switch status {
            case "connecting":
                connectionState = ConnectionState(status: .connecting, host: host, port: port)
            case "connected":
                connectionTimeoutTask?.cancel()
                connectionTimeoutTask = nil
                lastConnectedIp = host
                connectedDeviceId = json["device_id"] as? String
                UserDefaults.standard.set(host, forKey: "last_connected_ip")
                connectionState = ConnectionState(status: .connected, host: host, port: port)
                refreshPairedDevices()
            default:
                connectionTimeoutTask?.cancel()
                connectionTimeoutTask = nil
                connectedDeviceId = nil
                connectionState = ConnectionState(status: .disconnected, host: host, port: port, error: error ?? reason)
                refreshPairedDevices()
            }
        }
    }

    // MARK: - Foreground reconnect

    private func handleForegroundReturn() {
        // If connection was lost while backgrounded, try to reconnect
        ensureAutoConnect()
    }

    // MARK: - Actions

    func connectToDesktop() {
        connectToIp(desktopIp)
    }

    func connectToDevice(_ device: PairedDeviceDisplay) {
        guard let ip = device.lastKnownIp, !ip.isEmpty else { return }
        desktopIp = ip
        connectToIp(ip)
    }

    func connectToIp(_ ip: String, port: Int = 5027,
                     expectedDeviceId: String? = nil,
                     expectedCertificate: Data? = nil) {
        guard !isPreviewMode else { return }
        guard connectionState.status == .disconnected, !ip.isEmpty else { return }
        desktopIp = ip
        lastConnectedIp = ip
        networkPlugin.connect(
            ip: ip,
            port: port,
            displayPixelSize: Self.currentDisplayPixelSize(),
            expectedDeviceId: expectedDeviceId,
            expectedCertificate: expectedCertificate
        )

        // A user-initiated connection must fail promptly when the host is not
        // reachable. Pairing occurs only after a transport connection exists.
        connectionTimeoutTask?.cancel()
        connectionTimeoutTask = Task {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            if connectionState.status == .connecting {
                disconnect()
                connectionState = ConnectionState(
                    status: .disconnected,
                    error: "Connection timed out after 5s"
                )
            }
        }
    }

    private static func currentDisplayPixelSize() -> CGSize {
        let activeScreen = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .sorted { left, right in
                let leftActive = left.activationState == .foregroundActive
                let rightActive = right.activationState == .foregroundActive
                return leftActive && !rightActive
            }
            .first?
            .screen
        return (activeScreen ?? UIScreen.main).nativeBounds.size
    }

    func disconnect() {
        guard !isPreviewMode else {
            connectionState = ConnectionState(status: .disconnected)
            logs += "Preview disconnected\n"
            return
        }
        connectionTimeoutTask?.cancel()
        connectionTimeoutTask = nil
        startupAutoConnectTask?.cancel()
        startupAutoConnectTask = nil
        lastConnectedIp = nil  // Don't auto-reconnect after manual disconnect
        connectedDeviceId = nil
        UserDefaults.standard.removeObject(forKey: "last_connected_ip")
        networkPlugin.disconnect()
        connectionState = ConnectionState(status: .disconnected)
        logs += "Disconnected\n"
        refreshPairedDevices()
    }

    func ensureAutoConnect() {
        guard !isPreviewMode else { return }
        guard connectionState.status == .disconnected, !networkPlugin.isConnected else { return }
        guard let ip = preferredAutoConnectIp() else { return }
        NSLog("[anchor] Auto-connect attempting \(ip)")
        connectToIp(ip)
    }

    private func preferredAutoConnectIp() -> String? {
        if let ip = lastConnectedIp, !ip.isEmpty {
            return ip
        }
        if let ip = UserDefaults.standard.string(forKey: "last_connected_ip"), !ip.isEmpty {
            return ip
        }
        if !desktopIp.isEmpty {
            return desktopIp
        }

        return trustedStore.devices.values
            .sorted { $0.lastSeen > $1.lastSeen }
            .compactMap(\.lastKnownIp)
            .first(where: { !$0.isEmpty })
    }

    func sendCommand(pluginId: String, command: String, data: [String: String] = [:]) {
        guard !isPreviewMode else { return }
        var parts = [#""plugin_id":"\#(pluginId)""#, #""command":"\#(command)""#]
        for (key, value) in data {
            parts.append(#""\#(key)":"\#(value)""#)
        }
        let json = "{\(parts.joined(separator: ","))}"

        broker.send(AnchorEvent(target: .device, message: .json(json)))
        logs += "Sent: \(json)\n"
    }

    // MARK: - Export Logs

    func exportLogsURL() -> URL? {
        let tmpDir = FileManager.default.temporaryDirectory
        let fileURL = tmpDir.appendingPathComponent("anchor-logs.txt")
        do {
            try logs.write(to: fileURL, atomically: true, encoding: .utf8)
            return fileURL
        } catch {
            logs += "Failed to export logs: \(error.localizedDescription)\n"
            return nil
        }
    }

    // MARK: - Device ID persistence

    private static func getOrCreateDeviceId() -> String {
        let key = "anchor_device_id"
        if let existing = UserDefaults.standard.string(forKey: key) {
            return existing
        }
        let id = UUID().uuidString
        UserDefaults.standard.set(id, forKey: key)
        return id
    }
}

extension MainViewModel {
    static func preview(
        logs: String = "Preview ready\n",
        desktopIp: String = "192.168.1.42",
        connectionState: ConnectionState = ConnectionState(),
        pairingState: PairingState = .idle,
        pairedDevices: [PairedDeviceDisplay] = [],
        latencyMs: Int = 0,
        touchInputOnStream: Bool = true,
        touchpadSensitivity: Float = 1.5,
        autoSyncClipboard: Bool = false,
        videoState: VideoPlugin.PreviewState = .init(),
        clipboardHistory: [ClipboardHistoryEntry] = []
    ) -> MainViewModel {
        MainViewModel(mode: .preview(
            PreviewState(
                logs: logs,
                desktopIp: desktopIp,
                connectionState: connectionState,
                pairingState: pairingState,
                pairedDevices: pairedDevices,
                latencyMs: latencyMs,
                touchInputOnStream: touchInputOnStream,
                touchpadSensitivity: touchpadSensitivity,
                autoSyncClipboard: autoSyncClipboard,
                videoState: videoState,
                clipboardHistory: clipboardHistory
            )
        ))
    }

    static func previewConnected(
        clipboardHistory: [ClipboardHistoryEntry] = []
    ) -> MainViewModel {
        preview(
            desktopIp: "192.168.1.42",
            connectionState: ConnectionState(status: .connected, host: "192.168.1.42", port: 5025),
            pairedDevices: samplePairedDevices(connected: true),
            latencyMs: 18,
            touchInputOnStream: true,
            touchpadSensitivity: 1.6,
            videoState: VideoPlugin.PreviewState(
                isReceiving: true,
                fps: 60,
                streamWidth: 2560,
                streamHeight: 1600
            ),
            clipboardHistory: clipboardHistory
        )
    }

    static func previewDisconnected() -> MainViewModel {
        preview(
            desktopIp: "192.168.1.42",
            pairedDevices: samplePairedDevices(connected: false),
            touchInputOnStream: true,
            touchpadSensitivity: 1.6
        )
    }

    static func previewPairingRequest() -> MainViewModel {
        preview(
            desktopIp: "192.168.1.42",
            connectionState: ConnectionState(status: .connecting, host: "192.168.1.42", port: 5025),
            pairingState: .requested(
                deviceId: "desktop-preview",
                deviceName: "Studio Mac",
                deviceType: "desktop",
                fingerprint: "7E:4A:2B:91:11:AE:4C:27:7B:0D:00:88:12:6F:93:AF",
                certificatePem: "preview"
            ),
            pairedDevices: samplePairedDevices(connected: false),
            latencyMs: 12
        )
    }

    private static func samplePairedDevices(connected: Bool) -> [PairedDeviceDisplay] {
        [
            PairedDeviceDisplay(
                deviceId: "desktop-primary",
                deviceName: "Studio Mac",
                fingerprint: "7E:4A:2B:91",
                pairedAt: "2026-04-20T10:00:00Z",
                lastSeen: "2026-04-20T10:00:00Z",
                isOnline: connected,
                lastKnownIp: "192.168.1.42"
            ),
            PairedDeviceDisplay(
                deviceId: "desktop-lab",
                deviceName: "Lab Desktop",
                fingerprint: "A1:BC:39:FF",
                pairedAt: "2026-04-18T08:30:00Z",
                lastSeen: "2026-04-19T16:45:00Z",
                isOnline: false,
                lastKnownIp: "192.168.1.77"
            )
        ]
    }
}
