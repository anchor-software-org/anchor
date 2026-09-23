import Foundation

public enum AnchorFeatureCodecError: Error, Equatable {
    case invalidNodeID
    case invalidCoordinate
    case invalidRequestID
    case invalidCapabilitySessionID
    case streamBindingMismatch
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
        return try message.serializedData()
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
        return try message.serializedData()
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

    public static func absolute(x: Double, y: Double) throws -> ANCHInputInputPointerAbsolute {
        guard x.isFinite, y.isFinite else { throw AnchorFeatureCodecError.invalidCoordinate }
        var message = ANCHInputInputPointerAbsolute()
        message.x = UInt32(max(0, min(65_535, (x * 65_535).rounded())))
        message.y = UInt32(max(0, min(65_535, (y * 65_535).rounded())))
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
            "shift": 0xE1, "alt": 0xE2, "meta": 0xE3, "command": 0xE3,
            "delete": 0x4C, "home": 0x4A, "end": 0x4D,
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
}
