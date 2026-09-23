import Foundation
import UIKit
import Combine
import Compression
import CryptoKit

/// Clipboard sync plugin — mirrors Rust's ClipboardPlugin.
///
/// Watches UIPasteboard for changes and sends them to the desktop.
/// Receives clipboard content from the desktop and applies it locally.
/// Uses zlib compression for payloads > 1KB.
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

    private static let compressThreshold = 1024 // 1KB

    /// Observable clipboard history for the UI.
    @Published var history: [ClipboardHistoryEntry] = []
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
        // Listen for incoming clipboard messages from desktop
        broker.events
            .filter { event in
                if case .service(let id) = event.target, id == "clipboard" {
                    return true
                }
                return false
            }
            .sink { [weak self] event in
                self?.handleIncoming(event)
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

        // iOS doesn't have a clipboard change notification.
        // Poll UIPasteboard.changeCount every 1 second.
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
        guard autoSyncEnabled else { return }

        let currentCount = UIPasteboard.general.changeCount
        guard currentCount != lastChangeCount else { return }
        lastChangeCount = currentCount

        // Try text first
        if let text = UIPasteboard.general.string, !text.isEmpty {
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

        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let content = text
        let packet: [String: Any] = [
            "plugin_id": "clipboard",
            "type": "clipboard_content",
            "content_type": "text/plain",
            "content": content,
            "timestamp": timestamp,
            "hash": hash
        ]
        if let jsonData = try? JSONSerialization.data(withJSONObject: packet),
           let jsonString = String(data: jsonData, encoding: .utf8) {
            NSLog("[anchor.clipboard] Sending \(text.count) bytes to desktop")
            broker.send(AnchorEvent(target: .device, message: .json(jsonString)))
            lastSentHash = hash
            addToHistory(text: text, source: "local", compressed: false)
        }
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

        let timestamp = Int64(Date().timeIntervalSince1970 * 1000)
        let b64 = pngData.base64EncodedString()
        let content = b64
        let packet: [String: Any] = [
            "plugin_id": "clipboard",
            "type": "clipboard_content",
            "content_type": "image/png",
            "content": content,
            "timestamp": timestamp,
            "hash": hash
        ]
        if let jsonData = try? JSONSerialization.data(withJSONObject: packet),
           let jsonString = String(data: jsonData, encoding: .utf8) {
            NSLog("[anchor.clipboard] Sending image (\(pngData.count) bytes) to desktop")
            broker.send(AnchorEvent(target: .device, message: .json(jsonString)))
            lastSentHash = hash
            addToHistory(imageData: pngData, contentType: "image/png", source: "local", compressed: false)
        }
    }

    // MARK: - Desktop → Local clipboard

    private func handleIncoming(_ event: AnchorEvent) {
        guard case .json(let payload) = event.message,
              let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }

        let msgType = json["type"] as? String
        switch msgType {
        case "clipboard_content":
            handleClipboardContent(json)
        case "clipboard_connect":
            handleClipboardConnect(json)
        default:
            break
        }
    }

    private func handleGuiPayload(_ payload: String) {
        guard payload.contains("\"type\":\"connection_status\""),
              let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        isDesktopConnected = (json["status"] as? String) == "connected"
    }

    private func handleClipboardContent(_ json: [String: Any]) {
        guard let hash = json["hash"] as? String, !hash.isEmpty else { return }

        // Echo guard
        if hash == lastSentHash {
            NSLog("[anchor.clipboard] Skipping remote content (matches lastSentHash)")
            return
        }

        guard let rawContent = json["content"] as? String, !rawContent.isEmpty else { return }
        let contentType = json["content_type"] as? String ?? "text/plain"
        let compressed = json["compressed"] as? String ?? ""

        // Decompress if needed
        let content: String
        switch compressed {
        case "zlib":
            guard let compressedBytes = Data(base64Encoded: rawContent) else {
                NSLog("[anchor.clipboard] Bad base64 for compressed content")
                return
            }
            guard let decompressed = zlibDecompress(compressedBytes) else {
                NSLog("[anchor.clipboard] Zlib decompression failed")
                return
            }
            NSLog("[anchor.clipboard] Decompressed \(compressedBytes.count) -> \(decompressed.count) bytes")
            content = String(data: decompressed, encoding: .utf8) ?? rawContent
        case "zstd":
            // Desktop sends zstd — not natively supported on iOS.
            // Would need bundled C library. For now, log warning.
            NSLog("[anchor.clipboard] Received zstd compressed content — not supported on iOS")
            return
        case "":
            content = rawContent
        default:
            NSLog("[anchor.clipboard] Unknown compression: \(compressed)")
            return
        }

        // Set echo guard BEFORE writing
        lastAppliedHash = hash

        switch contentType {
        case "text/plain":
            DispatchQueue.main.async { [weak self] in
                UIPasteboard.general.string = content
                self?.lastChangeCount = UIPasteboard.general.changeCount
            }
            NSLog("[anchor.clipboard] Applied \(content.count) bytes of text from desktop")
            addToHistory(text: content, source: "desktop", compressed: !compressed.isEmpty)
        case "image/png":
            if let imageData = Data(base64Encoded: content),
               let image = UIImage(data: imageData) {
                DispatchQueue.main.async { [weak self] in
                    UIPasteboard.general.image = image
                    self?.lastChangeCount = UIPasteboard.general.changeCount
                }
                NSLog("[anchor.clipboard] Applied PNG image from desktop")
                addToHistory(imageData: imageData, contentType: "image/png", source: "desktop", compressed: !compressed.isEmpty)
            }
        default:
            NSLog("[anchor.clipboard] Unsupported content_type: \(contentType)")
        }
    }

    private func handleClipboardConnect(_ json: [String: Any]) {
        let remoteTimestamp = json["timestamp"] as? Int64 ?? 0
        let localTimestamp = Int64(Date().timeIntervalSince1970 * 1000)

        if remoteTimestamp > localTimestamp {
            NSLog("[anchor.clipboard] Remote clipboard is newer, applying")
            handleClipboardContent(json)
        } else {
            NSLog("[anchor.clipboard] Local clipboard is newer, keeping")
        }
    }

    // MARK: - Zlib via Apple Compression framework

    private func maybeCompress(_ text: String) -> (String, Bool) {
        let data = Data(text.utf8)
        guard data.count > Self.compressThreshold else { return (text, false) }

        guard let compressed = zlibCompress(data) else {
            NSLog("[anchor.clipboard] Compression failed, sending uncompressed")
            return (text, false)
        }

        let ratio = Int(Double(compressed.count) / Double(data.count) * 100)
        NSLog("[anchor.clipboard] Compressed \(data.count) -> \(compressed.count) bytes (\(ratio)%)")
        return (compressed.base64EncodedString(), true)
    }

    /// Compress using raw DEFLATE (matches Java's Deflater(true) and Rust's flate2 DeflateEncoder).
    private func zlibCompress(_ input: Data) -> Data? {
        // Allocate output buffer (same size as input — compressed should be smaller)
        let dstSize = max(input.count, 64)
        var dst = Data(count: dstSize)
        let compressedSize = dst.withUnsafeMutableBytes { dstPtr in
            input.withUnsafeBytes { srcPtr in
                compression_encode_buffer(
                    dstPtr.bindMemory(to: UInt8.self).baseAddress!, dstSize,
                    srcPtr.bindMemory(to: UInt8.self).baseAddress!, input.count,
                    nil, COMPRESSION_ZLIB
                )
            }
        }
        guard compressedSize > 0 else { return nil }
        return dst.prefix(compressedSize)
    }

    /// Decompress raw DEFLATE data.
    private func zlibDecompress(_ input: Data) -> Data? {
        var dstSize = input.count * 4
        for _ in 0..<5 {
            var dst = Data(count: dstSize)
            let decompressedSize = dst.withUnsafeMutableBytes { dstPtr in
                input.withUnsafeBytes { srcPtr in
                    compression_decode_buffer(
                        dstPtr.bindMemory(to: UInt8.self).baseAddress!, dstSize,
                        srcPtr.bindMemory(to: UInt8.self).baseAddress!, input.count,
                        nil, COMPRESSION_ZLIB
                    )
                }
            }
            if decompressedSize > 0 && decompressedSize < dstSize {
                return dst.prefix(decompressedSize)
            }
            dstSize *= 2
        }
        return nil
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
        if let text = UIPasteboard.general.string, !text.isEmpty {
            sendText(text)
            return
        }
        if let image = UIPasteboard.general.image {
            sendImage(image)
            return
        }
    }

    // MARK: - Helpers

    private func sha256Hex(_ data: Data) -> String {
        let hash = SHA256.hash(data: data)
        return hash.map { String(format: "%02x", $0) }.joined()
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
