import Foundation
import UIKit
import Combine
import CryptoKit
import ImageIO
import AnchorSDK

/// Clipboard sync plugin — mirrors Rust's ClipboardPlugin.
///
/// Watches UIPasteboard for changes and sends them to the desktop.
/// Receives clipboard content from the desktop and applies it locally.
final class ClipboardPlugin: Plugin, ObservableObject {
    let pluginId = "clipboard"

    private let broker: MessageBroker
    private var cancellables = Set<AnyCancellable>()
    private var pollTimer: Timer?

    // Echo prevention
    private var lastSentHash: String?
    private var lastAppliedHash: String?
    private var lastChangeCount: Int
    private var isDesktopConnected = false
    private var appliedRevisions = AnchorClipboardRevisionTracker()

    private static let maxContentBytes = AnchorV1.maxControlRecordBytes - 1024
    private static let maxDecodedImagePixels = 40_000_000

    /// Observable clipboard history for the UI.
    @Published var history: [ClipboardHistoryEntry] = []
    @Published private(set) var notice: String?
    private let maxHistorySize = 50

    /// When false, only desktop→iPad sync works. iPad→desktop requires explicit send.
    var autoSyncEnabled: Bool = false

    init(broker: MessageBroker, initialHistory: [ClipboardHistoryEntry] = [], initialChangeCount: Int? = nil) {
        self.broker = broker
        self.lastChangeCount = initialChangeCount ?? UIPasteboard.general.changeCount
        self.history = initialHistory
    }

    // MARK: - Lifecycle

    func start() {
        // Listen for typed Protocol v1 clipboard records from the network.
        broker.events
            .filter { event in
                if case .service(let id) = event.target, id == "clipboard" {
                    return true
                }
                return false
            }
            .sink { [weak self] event in
                guard case .clipboard(let message) = event.message else { return }
                self?.handleIncoming(message)
            }
            .store(in: &cancellables)

        broker.events
            .compactMap { event -> String? in
                guard event.target == .gui,
                      case .json(let payload) = event.message else { return nil }
                return payload
            }
            .sink { [weak self] payload in
                self?.handleGuiPayload(payload)
            }
            .store(in: &cancellables)

        // iOS has no clipboard change notification. Poll only while the app is
        // active, and only when the user explicitly enabled auto-sync.
        DispatchQueue.main.async { [weak self] in
            self?.pollTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
                self?.checkClipboardChange()
            }
        }

        NSLog("[anchor.clipboard] ClipboardPlugin started")
    }

    func stop() {
        pollTimer?.invalidate()
        pollTimer = nil
        isDesktopConnected = false
        cancellables.removeAll()
    }

    // MARK: - Local clipboard → Desktop

    private func checkClipboardChange() {
        guard autoSyncEnabled, UIApplication.shared.applicationState == .active else { return }

        let currentCount = UIPasteboard.general.changeCount
        guard currentCount != lastChangeCount else { return }
        lastChangeCount = currentCount

        // Try text first
        if let text = UIPasteboard.general.string {
            sendText(text)
            return
        }

        // Try image
        if let image = UIPasteboard.general.image {
            sendImage(image)
            return
        }
    }

    private func sendText(_ text: String) {
        let data = Data(text.utf8)
        let hash = sha256Hex(data)

        if hash == lastAppliedHash {
            NSLog("[anchor.clipboard] Skipping echo (matches lastAppliedHash)")
            return
        }
        if hash == lastSentHash { return }
        guard isDesktopConnected else {
            NSLog("[anchor.clipboard] Skipping send; desktop is disconnected")
            return
        }
        guard data.count <= Self.maxContentBytes else {
            notice = "Clipboard text is too large to send (1 MB limit)."
            return
        }
        notice = nil
        NSLog("[anchor.clipboard] Sending \(data.count) bytes to desktop")
        broker.send(AnchorEvent(target: .device, message: .clipboard(.publishLocal(.text(text)))))
        lastSentHash = hash
        addToHistory(text: text, source: "local", compressed: false)
    }

    private func sendImage(_ image: UIImage) {
        guard let pngData = image.pngData() else { return }
        let hash = sha256Hex(pngData)

        if hash == lastAppliedHash { return }
        if hash == lastSentHash { return }
        guard isDesktopConnected else {
            NSLog("[anchor.clipboard] Skipping image send; desktop is disconnected")
            return
        }
        guard pngData.count <= Self.maxContentBytes else {
            notice = "Clipboard image is too large to send (1 MB limit)."
            return
        }
        notice = nil
        NSLog("[anchor.clipboard] Sending image (\(pngData.count) bytes) to desktop")
        broker.send(AnchorEvent(target: .device, message: .clipboard(.publishLocal(.png(pngData)))))
        lastSentHash = hash
        addToHistory(imageData: pngData, contentType: "image/png", source: "local", compressed: false)
    }

    // MARK: - Desktop → Local clipboard

    private func handleIncoming(_ message: ClipboardWireMessage) {
        switch message {
        case .publishRemote(let origin, let revision, let content):
            guard (try? appliedRevisions.shouldApply(originNodeID: origin, revision: revision)) == true else {
                NSLog("[anchor.clipboard] Ignoring duplicate or older revision \(revision)")
                return
            }
            applyRemote(content)
        case .clearRemote(let origin, let revision):
            guard (try? appliedRevisions.shouldApply(originNodeID: origin, revision: revision)) == true else {
                return
            }
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.items = []
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
        case .publishLocal, .clearLocal:
            break
        }
    }

    private func handleGuiPayload(_ payload: String) {
        guard payload.contains("\"type\":\"connection_status\""),
              let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        isDesktopConnected = (json["status"] as? String) == "connected"
    }

    private func applyRemote(_ content: ClipboardWireContent) {
        switch content {
        case .text(let text):
            guard text.utf8.count <= Self.maxContentBytes else { return }
            lastAppliedHash = sha256Hex(Data(text.utf8))
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.string = text
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
            addToHistory(text: text, source: "desktop", compressed: false)
        case .png(let imageData):
            guard imageData.count <= Self.maxContentBytes,
                  imagePixelCount(imageData) <= Self.maxDecodedImagePixels,
                  let image = UIImage(data: imageData) else {
                notice = "Desktop clipboard image was rejected because it is too large or invalid."
                return
            }
            lastAppliedHash = sha256Hex(imageData)
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.image = image
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
            addToHistory(imageData: imageData, contentType: "image/png", source: "desktop", compressed: false)
        }
    }

    // MARK: - History

    private func addToHistory(
        text: String? = nil,
        imageData: Data? = nil,
        contentType: String = "text/plain",
        source: String,
        compressed: Bool
    ) {
        let sizeBytes = imageData?.count ?? text?.utf8.count ?? 0
        let entry = ClipboardHistoryEntry(
            text: text,
            imageData: imageData,
            contentType: contentType,
            source: source,
            timestamp: Date(),
            compressed: compressed,
            sizeBytes: sizeBytes
        )
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.history.insert(entry, at: 0)
            if self.history.count > self.maxHistorySize {
                self.history.removeLast()
            }
        }
    }

    func clearHistory() {
        DispatchQueue.main.async { [weak self] in
            self?.history.removeAll()
        }
    }

    /// Copy a history entry to the local clipboard without re-sending to desktop.
    func copyToLocalClipboard(_ entry: ClipboardHistoryEntry) {
        if entry.isImage, let imageData = entry.imageData, let image = UIImage(data: imageData) {
            let hash = sha256Hex(imageData)
            lastAppliedHash = hash
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.image = image
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
        } else if let text = entry.text {
            let hash = sha256Hex(Data(text.utf8))
            lastAppliedHash = hash
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.string = text
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
        }
    }

    /// Send current clipboard to desktop (call from UI).
    /// Explicitly send current clipboard (from UI button). Works for text and images.
    func sendCurrentClipboard() {
        if let text = UIPasteboard.general.string {
            sendText(text)
            return
        }
        if let image = UIPasteboard.general.image {
            sendImage(image)
            return
        }
        guard isDesktopConnected else { return }
        broker.send(AnchorEvent(target: .device, message: .clipboard(.clearLocal)))
    }

    // MARK: - Helpers

    private func sha256Hex(_ data: Data) -> String {
        let hash = SHA256.hash(data: data)
        return hash.map { String(format: "%02x", $0) }.joined()
    }

    private func imagePixelCount(_ data: Data) -> Int {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int,
              width > 0, height > 0,
              width <= Self.maxDecodedImagePixels / height else { return .max }
        return width * height
    }
}

struct ClipboardHistoryEntry: Identifiable {
    let id = UUID()
    let text: String?
    let imageData: Data?      // decoded PNG bytes for image entries
    let contentType: String   // "text/plain" or "image/png"
    let source: String        // "local" or "desktop"
    let timestamp: Date
    let compressed: Bool
    let sizeBytes: Int

    var isImage: Bool { contentType == "image/png" }
}

extension ClipboardPlugin {
    static func preview(
        broker: MessageBroker = MessageBroker(),
        history: [ClipboardHistoryEntry] = [],
        autoSyncEnabled: Bool = false
    ) -> ClipboardPlugin {
        let plugin = ClipboardPlugin(
            broker: broker,
            initialHistory: history,
            initialChangeCount: 0
        )
        plugin.autoSyncEnabled = autoSyncEnabled
        return plugin
    }
}

extension ClipboardHistoryEntry {
    static func previewText(
        _ text: String,
        source: String,
        minutesAgo: Double,
        compressed: Bool = false
    ) -> ClipboardHistoryEntry {
        ClipboardHistoryEntry(
            text: text,
            imageData: nil,
            contentType: "text/plain",
            source: source,
            timestamp: Date().addingTimeInterval(-(minutesAgo * 60)),
            compressed: compressed,
            sizeBytes: text.utf8.count
        )
    }

    static var previewSamples: [ClipboardHistoryEntry] {
        [
            .previewText(
                "journalctl -fu anchor && cargo run --release",
                source: "local",
                minutesAgo: 1,
                compressed: true
            ),
            .previewText(
                "PR note: clipboard sync is stable after the reconnect guard fix.",
                source: "desktop",
                minutesAgo: 4
            ),
            .previewText(
                "Send the build to QA once the previews are in place.",
                source: "desktop",
                minutesAgo: 12
            )
        ]
    }
}
