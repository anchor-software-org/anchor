import Foundation
import SwiftProtobuf

public enum AnchorFeatureCodecError: Error, Equatable {
    case invalidNodeID
    case invalidRevision
    case recordTooLarge
    case missingClipboardContent
    case invalidTransferID
    case invalidFileHash
    case invalidFilename
    case fileTooLarge
    case invalidMediaCommand
    case artworkTooLarge
    case invalidCoordinate
    case invalidRequestID
    case invalidCapabilitySessionID
    case streamBindingMismatch
    case datagramBindingMismatch
}

/// Canonical `org.anchor.clipboard@1` protobuf construction and echo policy.
public enum AnchorClipboardCodec {
    public static func publishText(originNodeID: Data, revision: UInt64, text: String) throws -> Data {
        try publish(originNodeID: originNodeID, revision: revision, content: .textUtf8(text))
    }

    public static func publishPNG(originNodeID: Data, revision: UInt64, png: Data) throws -> Data {
        try publish(originNodeID: originNodeID, revision: revision, content: .png(png))
    }

    public static func clear(originNodeID: Data, revision: UInt64) throws -> Data {
        guard originNodeID.count == 32 else { throw AnchorFeatureCodecError.invalidNodeID }
        var node = ANCHNodeId(); node.value = originNodeID
        var message = ANCHClipboardClipboardClear()
        message.originNodeID = node
        message.revision = revision
        return try checkedSerialization(message)
    }

    public static func decodePublish(_ data: Data) throws -> ANCHClipboardClipboardPublish {
        guard data.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        let message = try ANCHClipboardClipboardPublish(serializedBytes: data)
        guard message.originNodeID.value.count == 32 else {
            throw AnchorFeatureCodecError.invalidNodeID
        }
        guard message.content != nil else {
            throw AnchorFeatureCodecError.missingClipboardContent
        }
        return message
    }

    public static func decodeClear(_ data: Data) throws -> ANCHClipboardClipboardClear {
        guard data.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        let message = try ANCHClipboardClipboardClear(serializedBytes: data)
        guard message.originNodeID.value.count == 32 else {
            throw AnchorFeatureCodecError.invalidNodeID
        }
        return message
    }

    public static func isEcho(_ publish: ANCHClipboardClipboardPublish, localNodeID: Data) -> Bool {
        publish.originNodeID.value == localNodeID
    }

    private static func publish(
        originNodeID: Data,
        revision: UInt64,
        content: ANCHClipboardClipboardPublish.OneOf_Content
    ) throws -> Data {
        guard originNodeID.count == 32 else { throw AnchorFeatureCodecError.invalidNodeID }
        var node = ANCHNodeId(); node.value = originNodeID
        var message = ANCHClipboardClipboardPublish()
        message.originNodeID = node
        message.revision = revision
        message.content = content
        return try checkedSerialization(message)
    }

    private static func checkedSerialization<M: SwiftProtobuf.Message>(_ message: M) throws -> Data {
        let data = try message.serializedData()
        guard data.count <= AnchorV1.maxControlRecordBytes else {
            throw AnchorFeatureCodecError.recordTooLarge
        }
        return data
    }
}

/// Origin-scoped revision ordering for clipboard records. Revisions are not
/// timestamps: each sender owns its own monotonically increasing sequence.
public struct AnchorClipboardRevisionTracker {
    private var greatestRevisionByOrigin: [Data: UInt64] = [:]

    public init() {}

    public mutating func shouldApply(originNodeID: Data, revision: UInt64) throws -> Bool {
        guard originNodeID.count == 32 else { throw AnchorFeatureCodecError.invalidNodeID }
        guard revision > 0 else { throw AnchorFeatureCodecError.invalidRevision }
        guard revision > (greatestRevisionByOrigin[originNodeID] ?? 0) else { return false }
        greatestRevisionByOrigin[originNodeID] = revision
        return true
    }

    public func greatestRevision(for originNodeID: Data) -> UInt64? {
        greatestRevisionByOrigin[originNodeID]
    }
}

public struct AnchorFileOffer: Equatable {
    public let transferID: Data
    public let filename: String
    public let mimeType: String
    public let byteLength: UInt64
    public let sha256: Data

    public init(transferID: Data, filename: String, mimeType: String,
                byteLength: UInt64, sha256: Data) {
        self.transferID = transferID
        self.filename = filename
        self.mimeType = mimeType
        self.byteLength = byteLength
        self.sha256 = sha256
    }
}

public struct AnchorFileDecision: Equatable {
    public let transferID: Data
    public let accepted: Bool

    public init(transferID: Data, accepted: Bool) {
        self.transferID = transferID
        self.accepted = accepted
    }
}

public struct AnchorFileContentStart: Equatable {
    public let transferID: Data
    public let quicStreamID: UInt64

    public init(transferID: Data, quicStreamID: UInt64) {
        self.transferID = transferID
        self.quicStreamID = quicStreamID
    }
}

public struct AnchorFileComplete: Equatable {
    public let transferID: Data
    public let sha256: Data

    public init(transferID: Data, sha256: Data) {
        self.transferID = transferID
        self.sha256 = sha256
    }
}

/// Validated builders and decoders for `org.anchor.files@1`.
public enum AnchorFilesCodec {
    public static let transferIDByteCount = 16
    public static let sha256ByteCount = 32
    public static let maximumFilenameUTF8Bytes = 255

    public static func offer(_ offer: AnchorFileOffer, maximumByteLength: UInt64 = .max) throws -> Data {
        try validate(offer, maximumByteLength: maximumByteLength)
        var message = ANCHFilesFileOffer()
        message.transferID = offer.transferID
        message.filename = offer.filename
        message.mimeType = offer.mimeType
        message.byteLength = offer.byteLength
        message.sha256 = offer.sha256
        return try message.serializedData()
    }

    public static func decodeOffer(_ data: Data, maximumByteLength: UInt64 = .max) throws -> AnchorFileOffer {
        let message = try ANCHFilesFileOffer(serializedBytes: data)
        let offer = AnchorFileOffer(
            transferID: message.transferID,
            filename: message.filename,
            mimeType: message.mimeType,
            byteLength: message.byteLength,
            sha256: message.sha256
        )
        try validate(offer, maximumByteLength: maximumByteLength)
        return offer
    }

    public static func decision(transferID: Data, accepted: Bool) throws -> Data {
        try validateTransferID(transferID)
        var message = ANCHFilesFileDecision()
        message.transferID = transferID
        message.accepted = accepted
        return try message.serializedData()
    }

    public static func decodeDecision(_ data: Data) throws -> AnchorFileDecision {
        let message = try ANCHFilesFileDecision(serializedBytes: data)
        try validateTransferID(message.transferID)
        return AnchorFileDecision(transferID: message.transferID, accepted: message.accepted)
    }

    public static func contentStart(transferID: Data, quicStreamID: UInt64) throws -> Data {
        try validateTransferID(transferID)
        var message = ANCHFilesFileContentStart()
        message.transferID = transferID
        message.quicStreamID = quicStreamID
        return try message.serializedData()
    }

    public static func decodeContentStart(_ data: Data) throws -> AnchorFileContentStart {
        let message = try ANCHFilesFileContentStart(serializedBytes: data)
        try validateTransferID(message.transferID)
        return AnchorFileContentStart(
            transferID: message.transferID,
            quicStreamID: message.quicStreamID
        )
    }

    public static func complete(transferID: Data, sha256: Data) throws -> Data {
        try validateTransferID(transferID)
        try validateHash(sha256)
        var message = ANCHFilesFileComplete()
        message.transferID = transferID
        message.sha256 = sha256
        return try message.serializedData()
    }

    public static func decodeComplete(_ data: Data) throws -> AnchorFileComplete {
        let message = try ANCHFilesFileComplete(serializedBytes: data)
        try validateTransferID(message.transferID)
        try validateHash(message.sha256)
        return AnchorFileComplete(transferID: message.transferID, sha256: message.sha256)
    }

    private static func validate(_ offer: AnchorFileOffer, maximumByteLength: UInt64) throws {
        try validateTransferID(offer.transferID)
        try validateHash(offer.sha256)
        guard offer.byteLength <= maximumByteLength else { throw AnchorFeatureCodecError.fileTooLarge }
        let filename = offer.filename
        guard !filename.isEmpty,
              filename.utf8.count <= maximumFilenameUTF8Bytes,
              filename != ".", filename != "..",
              !filename.contains("/"), !filename.contains("\\"),
              !filename.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw AnchorFeatureCodecError.invalidFilename
        }
    }

    private static func validateTransferID(_ value: Data) throws {
        guard value.count == transferIDByteCount else {
            throw AnchorFeatureCodecError.invalidTransferID
        }
    }

    private static func validateHash(_ value: Data) throws {
        guard value.count == sha256ByteCount else {
            throw AnchorFeatureCodecError.invalidFileHash
        }
    }
}

public struct AnchorMediaState: Equatable {
    public let title: String
    public let artist: String
    public let album: String
    public let playing: Bool
    public let positionMilliseconds: UInt64
    public let durationMilliseconds: UInt64
    public let artworkJPEG: Data
}

public enum AnchorMediaCommand: Equatable {
    case play
    case pause
    case next
    case previous
    case seek(positionMilliseconds: UInt64)
}

/// Validated records for the desktop-controller portion of `org.anchor.media@1`.
public enum AnchorMediaCodec {
    public static let maximumArtworkJPEGBytes = 256 * 1024

    public static func decodeState(_ data: Data) throws -> AnchorMediaState {
        let message = try ANCHMediaMediaState(serializedBytes: data)
        guard message.artworkJpeg.count <= maximumArtworkJPEGBytes else {
            throw AnchorFeatureCodecError.artworkTooLarge
        }
        return AnchorMediaState(
            title: message.title,
            artist: message.artist,
            album: message.album,
            playing: message.playing,
            positionMilliseconds: min(message.positionMs, message.durationMs == 0 ? message.positionMs : message.durationMs),
            durationMilliseconds: message.durationMs,
            artworkJPEG: message.artworkJpeg
        )
    }

    public static func command(_ command: AnchorMediaCommand) throws -> Data {
        var message = ANCHMediaMediaCommand()
        switch command {
        case .play: message.kind = .play
        case .pause: message.kind = .pause
        case .next: message.kind = .next
        case .previous: message.kind = .previous
        case .seek(let position):
            message.kind = .seek
            message.positionMs = position
        }
        return try message.serializedData()
    }

    public static func decodeCommand(_ data: Data) throws -> AnchorMediaCommand {
        let message = try ANCHMediaMediaCommand(serializedBytes: data)
        switch message.kind {
        case .play: return .play
        case .pause: return .pause
        case .next: return .next
        case .previous: return .previous
        case .seek: return .seek(positionMilliseconds: message.positionMs)
        case .unspecified, .UNRECOGNIZED:
            throw AnchorFeatureCodecError.invalidMediaCommand
        }
    }
}

public enum AnchorPointerButton: Equatable {
    case left, middle, right, back, forward
}

/// Canonical fixed-point conversions for `org.anchor.input@1`.
public enum AnchorInputCodec {
    public static func relative(dx: Double, dy: Double) -> ANCHInputInputPointerRelative {
        var message = ANCHInputInputPointerRelative()
        message.dx1000Ths = Int32(clamping: Int64((dx * 1000).rounded()))
        message.dy1000Ths = Int32(clamping: Int64((dy * 1000).rounded()))
        return message
    }

    public static func absolute(
        x: Double, y: Double, targetOutputName: String = ""
    ) throws -> ANCHInputInputPointerAbsolute {
        guard x.isFinite, y.isFinite else { throw AnchorFeatureCodecError.invalidCoordinate }
        var message = ANCHInputInputPointerAbsolute()
        message.x = UInt32(max(0, min(65_535, (x * 65_535).rounded())))
        message.y = UInt32(max(0, min(65_535, (y * 65_535).rounded())))
        message.targetOutputName = targetOutputName
        return message
    }

    public static func button(_ button: AnchorPointerButton, pressed: Bool) -> ANCHInputInputPointerButton {
        var message = ANCHInputInputPointerButton()
        switch button {
        case .left: message.button = .left
        case .middle: message.button = .middle
        case .right: message.button = .right
        case .back: message.button = .back
        case .forward: message.button = .forward
        }
        message.pressed = pressed
        return message
    }
}

/// Converts UI key labels to USB HID keyboard-page usage IDs. Unknown labels
/// stay unknown instead of leaking desktop-specific key names onto the wire.
public enum AnchorHIDUsage {
    public static func keyboard(_ key: String) -> UInt32? {
        let aliases: [String: UInt32] = [
            "backspace": 0x2A, "enter": 0x28, "return": 0x28, "escape": 0x29,
            "tab": 0x2B, "space": 0x2C, "left": 0x50, "right": 0x4F,
            "up": 0x52, "down": 0x51, "control": 0xE0, "ctrl": 0xE0,
            "shift": 0xE1, "alt": 0xE2, "meta": 0xE3, "command": 0xE3, "super": 0xE3,
            "delete": 0x4C, "home": 0x4A, "end": 0x4D,
            "pageup": 0x4B, "pagedown": 0x4E, "pgup": 0x4B, "pgdn": 0x4E,
        ]
        let value = key.lowercased()
        if let usage = aliases[value] { return usage }
        if value.count == 1, let scalar = value.unicodeScalars.first {
            if (97...122).contains(scalar.value) { return 0x04 + scalar.value - 97 }
            if (49...57).contains(scalar.value) { return 0x1E + scalar.value - 49 }
            if scalar.value == 48 { return 0x27 }
        }
        if value.first == "f", let number = Int(value.dropFirst()), (1...12).contains(number) {
            return UInt32(0x3A + number - 1)
        }
        return nil
    }
}

/// Accumulates high-resolution scroll deltas so sub-step motion is not lost
/// when the protocol emits signed 1/120th wheel units.
public struct AnchorScrollAccumulator {
    private var horizontalRemainder = 0.0
    private var verticalRemainder = 0.0

    public init() {}

    public mutating func consume(horizontalSteps: Double, verticalSteps: Double) -> ANCHInputInputScroll? {
        horizontalRemainder += horizontalSteps * 120
        verticalRemainder += verticalSteps * 120
        let horizontal = Int32(horizontalRemainder.rounded(.towardZero))
        let vertical = Int32(verticalRemainder.rounded(.towardZero))
        horizontalRemainder -= Double(horizontal)
        verticalRemainder -= Double(vertical)
        guard horizontal != 0 || vertical != 0 else { return nil }
        var message = ANCHInputInputScroll()
        message.horizontal120Ths = horizontal
        message.vertical120Ths = vertical
        return message
    }
}

/// Canonical control-plane binding for a secondary QUIC stream.
public enum AnchorStreamBindingCodec {
    public static func openEnvelope(
        requestID: UInt64,
        streamID: UInt64,
        capabilitySessionID: UInt64,
        payloadTypeURL: String
    ) throws -> ANCHControlEnvelope {
        guard requestID != 0 else { throw AnchorFeatureCodecError.invalidRequestID }
        guard capabilitySessionID != 0 else {
            throw AnchorFeatureCodecError.invalidCapabilitySessionID
        }
        var open = ANCHStreamOpen()
        open.quicStreamID = streamID
        open.capabilitySessionID = capabilitySessionID
        open.payloadTypeURL = payloadTypeURL
        var envelope = ANCHControlEnvelope()
        envelope.requestID = requestID
        envelope.streamOpen = open
        return envelope
    }

    public static func validateOpened(
        _ envelope: ANCHControlEnvelope,
        requestID: UInt64,
        streamID: UInt64
    ) throws {
        guard envelope.responseTo == requestID,
              case .streamOpened(let opened)? = envelope.body,
              opened.quicStreamID == streamID else {
            throw AnchorFeatureCodecError.streamBindingMismatch
        }
    }
}

public enum AnchorDatagramFlowBindingCodec {
    public static func openEnvelope(
        requestID: UInt64,
        flowID: UInt64,
        capabilitySessionID: UInt64,
        payloadTypeURL: String
    ) throws -> ANCHControlEnvelope {
        guard requestID != 0 else { throw AnchorFeatureCodecError.invalidRequestID }
        guard capabilitySessionID != 0 else {
            throw AnchorFeatureCodecError.invalidCapabilitySessionID
        }
        guard flowID != 0, !payloadTypeURL.isEmpty else {
            throw AnchorFeatureCodecError.datagramBindingMismatch
        }
        var open = ANCHDatagramFlowOpen()
        open.capabilitySessionID = capabilitySessionID
        open.flowID = flowID
        open.payloadTypeURL = payloadTypeURL
        var envelope = ANCHControlEnvelope()
        envelope.requestID = requestID
        envelope.datagramFlowOpen = open
        return envelope
    }

    public static func validateOpened(
        _ envelope: ANCHControlEnvelope,
        requestID: UInt64,
        flowID: UInt64
    ) throws {
        guard envelope.responseTo == requestID,
              case .datagramFlowOpened(let opened)? = envelope.body,
              opened.flowID == flowID else {
            throw AnchorFeatureCodecError.datagramBindingMismatch
        }
    }
}

/// Typed builders for the iOS Sideboat viewer control records.
public enum AnchorScreenCodec {
    public static func start(maxFPS: UInt32 = 0, targetBitrateKbps: UInt32 = 0,
                             outputID: String = "") throws -> Data {
        var message = ANCHScreenScreenStart()
        message.maxFps = maxFPS
        message.targetBitrateKbps = targetBitrateKbps
        message.outputID = outputID
        return try message.serializedData()
    }

    public static func stop() throws -> Data {
        try ANCHScreenScreenStop().serializedData()
    }

    public static func requestKeyframe() throws -> Data {
        try ANCHScreenScreenRequestKeyframe().serializedData()
    }

    public static func selectOutput(_ outputID: String) throws -> Data {
        var message = ANCHScreenScreenSelectOutput()
        message.outputID = outputID
        return try message.serializedData()
    }
}
