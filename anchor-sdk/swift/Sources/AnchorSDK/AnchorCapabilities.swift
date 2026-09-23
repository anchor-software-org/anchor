import Foundation

/// The advertisement shape shared by every v1 endpoint. The protobuf
/// `CapabilityAdvertisement` is generated from the canonical schema; this
/// value type is convenient for transport/session implementations that need to
/// validate a record before decoding it.
public struct AnchorCapabilityDescriptor: Equatable {
    public let name: String
    public let major: UInt32
    public let recordTypeURLs: Set<String>
    public let supportsDatagrams: Bool

    public init(name: String, major: UInt32 = 1, recordTypeURLs: Set<String>, supportsDatagrams: Bool = false) {
        self.name = name
        self.major = major
        self.recordTypeURLs = recordTypeURLs
        self.supportsDatagrams = supportsDatagrams
    }

    public func accepts(typeURL: String) -> Bool { recordTypeURLs.contains(typeURL) }
}

public enum AnchorV1Capabilities {
    public static let all: [AnchorCapabilityDescriptor] = [
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.clipboard, recordTypeURLs: [
            AnchorV1.TypeURL.clipboardPublish, AnchorV1.TypeURL.clipboardClear,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.device, recordTypeURLs: [
            AnchorV1.TypeURL.deviceState,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.notifications, recordTypeURLs: [
            AnchorV1.TypeURL.notificationPosted,
            AnchorV1.TypeURL.notificationDismissed, AnchorV1.TypeURL.notificationInvokeAction,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.input, recordTypeURLs: [
            AnchorV1.TypeURL.inputKey, AnchorV1.TypeURL.inputText,
            AnchorV1.TypeURL.inputPointerAbsolute, AnchorV1.TypeURL.inputPointerRelative,
            AnchorV1.TypeURL.inputPointerButton, AnchorV1.TypeURL.inputScroll,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.media, recordTypeURLs: [
            AnchorV1.TypeURL.mediaState, AnchorV1.TypeURL.mediaCommand,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.screen, recordTypeURLs: [
            AnchorV1.TypeURL.screenStart, AnchorV1.TypeURL.screenStop,
            AnchorV1.TypeURL.screenSelectOutput, AnchorV1.TypeURL.screenRequestKeyframe,
            AnchorV1.TypeURL.screenOutputList, AnchorV1.TypeURL.screenStatus,
            AnchorV1.TypeURL.screenFrame,
        ], supportsDatagrams: false),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.camera, recordTypeURLs: [
            AnchorV1.TypeURL.cameraStart, AnchorV1.TypeURL.cameraStop,
            AnchorV1.TypeURL.cameraStatus, AnchorV1.TypeURL.cameraCodecConfig,
            AnchorV1.TypeURL.cameraKeyframeNeeded, AnchorV1.TypeURL.cameraControl,
            AnchorV1.TypeURL.cameraFrame,
        ], supportsDatagrams: true),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.files, recordTypeURLs: [
            AnchorV1.TypeURL.fileOffer, AnchorV1.TypeURL.fileDecision,
            AnchorV1.TypeURL.fileContentStart, AnchorV1.TypeURL.fileComplete,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.sms, recordTypeURLs: [
            AnchorV1.TypeURL.smsMessage, AnchorV1.TypeURL.smsSend, AnchorV1.TypeURL.smsSendResult,
        ]),
        AnchorCapabilityDescriptor(name: AnchorV1.Capability.commands, recordTypeURLs: [
            AnchorV1.TypeURL.commandRun, AnchorV1.TypeURL.commandList,
            AnchorV1.TypeURL.commandKill, AnchorV1.TypeURL.commandResult,
        ]),
    ]

    public static func descriptor(for name: String, major: UInt32 = 1) -> AnchorCapabilityDescriptor? {
        all.first { $0.name == name && $0.major == major }
    }
}
