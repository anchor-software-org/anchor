import Foundation

/// Pairing state machine — mirrors Android's PairingState sealed class.
enum PairingState {
    case idle
    case requested(
        deviceId: String,
        deviceName: String,
        deviceType: String,
        fingerprint: String,
        certificatePem: String
    )
    case accepted
    case rejected
}

/// Display model for paired devices — mirrors Android's PairedDeviceDisplay.
struct PairedDeviceDisplay: Identifiable {
    let deviceId: String
    let deviceName: String
    let fingerprint: String
    let pairedAt: String
    let lastSeen: String
    let isOnline: Bool
    let lastKnownIp: String?

    var id: String { deviceId }
}
