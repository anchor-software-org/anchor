import Foundation
import CryptoKit

/// Persistent trust store — mirrors Android's TrustedStore.
/// Stores paired device entries as JSON files in the app's Documents directory.
class TrustedStore {

    struct TrustedDeviceEntry: Codable {
        let deviceId: String
        let deviceName: String
        let certificatePem: String
        let pairedAt: String
        var lastSeen: String
        var lastKnownIp: String?
    }

    private(set) var devices: [String: TrustedDeviceEntry] = [:]
    private let storeDir: URL

    init() {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
        storeDir = docs.appendingPathComponent("trusted_devices", isDirectory: true)
        try? FileManager.default.createDirectory(at: storeDir, withIntermediateDirectories: true)
        loadAll()
    }

    // MARK: - Public API

    func isTrusted(_ deviceId: String) -> Bool {
        devices[deviceId] != nil
    }

    func addDevice(_ deviceId: String, name: String, certificatePem: String, ip: String? = nil) {
        let now = ISO8601DateFormatter().string(from: Date())
        let entry = TrustedDeviceEntry(
            deviceId: deviceId,
            deviceName: name,
            certificatePem: certificatePem,
            pairedAt: now,
            lastSeen: now,
            lastKnownIp: ip
        )
        devices[deviceId] = entry
        save(entry)
    }

    func removeDevice(_ deviceId: String) {
        devices.removeValue(forKey: deviceId)
        let file = storeDir.appendingPathComponent("\(deviceId).json")
        try? FileManager.default.removeItem(at: file)
    }

    func updateLastSeen(_ deviceId: String, ip: String? = nil) {
        guard var entry = devices[deviceId] else { return }
        entry.lastSeen = ISO8601DateFormatter().string(from: Date())
        if let ip = ip { entry.lastKnownIp = ip }
        devices[deviceId] = entry
        save(entry)
    }

    // MARK: - Fingerprint

    static func fingerprint(_ derBytes: Data) -> String {
        let hash = SHA256.hash(data: derBytes)
        return hash.map { String(format: "%02X", $0) }
            .joined(separator: ":")
    }

    // MARK: - Persistence

    private func loadAll() {
        guard let files = try? FileManager.default.contentsOfDirectory(at: storeDir, includingPropertiesForKeys: nil) else { return }
        let decoder = JSONDecoder()
        for file in files where file.pathExtension == "json" {
            guard let data = try? Data(contentsOf: file),
                  let entry = try? decoder.decode(TrustedDeviceEntry.self, from: data) else { continue }
            devices[entry.deviceId] = entry
        }
    }

    private func save(_ entry: TrustedDeviceEntry) {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .prettyPrinted
        guard let data = try? encoder.encode(entry) else { return }
        let file = storeDir.appendingPathComponent("\(entry.deviceId).json")
        try? data.write(to: file)
    }
}
