import Foundation

/// Errors raised before protobuf decoding. The protobuf envelope itself is
/// deliberately passed as opaque bytes so generated SwiftProtobuf types can be
/// supplied by the application without coupling the transport layer to codegen.
public enum AnchorWireError: Error, Equatable {
    case invalidControlMagic
    case unsupportedControlVersion(major: UInt8, minor: UInt8)
    case malformedVarint
    case controlRecordTooLarge
    case invalidFrameMagic
    case unsupportedFrameVersion(UInt8)
    case invalidFrameKind(UInt8)
    case invalidFrameFragments
    case frameTooLarge
}

public enum AnchorV1 {
    public static let alpn = "anchor/1"
    public static let controlMagic = Data([0x41, 0x4e, 0x43, 0x52]) // ANCR
    public static let controlMajor: UInt8 = 1
    public static let controlMinor: UInt8 = 0
    public static let maxControlRecordBytes = 1024 * 1024

    public enum Capability {
        public static let clipboard = "org.anchor.clipboard"
        public static let device = "org.anchor.device"
        public static let notifications = "org.anchor.notifications"
        public static let input = "org.anchor.input"
        public static let media = "org.anchor.media"
        public static let screen = "org.anchor.screen"
        public static let camera = "org.anchor.camera"
        public static let files = "org.anchor.files"
        public static let sms = "org.anchor.sms"
        public static let commands = "org.anchor.commands"
    }

    /// Canonical protobuf type URLs. Keeping these in one catalog prevents a
    /// Swift host from silently drifting from the Rust/Kotlin advertisements.
    public enum TypeURL {
        public static let clipboardPublish = "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardPublish"
        public static let clipboardClear = "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardClear"
        public static let deviceState = "type.googleapis.com/anchor.v1.capabilities.device.DeviceState"
        public static let notificationPosted = "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationPosted"
        public static let notificationDismissed = "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationDismissed"
        public static let notificationInvokeAction = "type.googleapis.com/anchor.v1.capabilities.notifications.NotificationInvokeAction"
        public static let inputKey = "type.googleapis.com/anchor.v1.capabilities.input.InputKey"
        public static let inputText = "type.googleapis.com/anchor.v1.capabilities.input.InputText"
        public static let inputPointerAbsolute = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerAbsolute"
        public static let inputPointerRelative = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerRelative"
        public static let inputPointerButton = "type.googleapis.com/anchor.v1.capabilities.input.InputPointerButton"
        public static let inputScroll = "type.googleapis.com/anchor.v1.capabilities.input.InputScroll"
        public static let mediaState = "type.googleapis.com/anchor.v1.capabilities.media.MediaState"
        public static let mediaCommand = "type.googleapis.com/anchor.v1.capabilities.media.MediaCommand"
        public static let screenFrame = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenFrame"
        public static let screenStart = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStart"
        public static let screenStop = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStop"
        public static let screenSelectOutput = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenSelectOutput"
        public static let screenRequestKeyframe = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenRequestKeyframe"
        public static let screenOutputList = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenOutputList"
        public static let screenStatus = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStatus"
        public static let cameraFrame = "type.googleapis.com/anchor.v1.capabilities.camera.CameraFrame"
        public static let cameraStart = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStart"
        public static let cameraStop = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStop"
        public static let cameraStatus = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStatus"
        public static let cameraCodecConfig = "type.googleapis.com/anchor.v1.capabilities.camera.CameraCodecConfig"
        public static let cameraKeyframeNeeded = "type.googleapis.com/anchor.v1.capabilities.camera.CameraKeyframeNeeded"
        public static let cameraControl = "type.googleapis.com/anchor.v1.capabilities.camera.CameraControl"
        public static let fileOffer = "type.googleapis.com/anchor.v1.capabilities.files.FileOffer"
        public static let fileDecision = "type.googleapis.com/anchor.v1.capabilities.files.FileDecision"
        public static let fileContentStart = "type.googleapis.com/anchor.v1.capabilities.files.FileContentStart"
        public static let fileComplete = "type.googleapis.com/anchor.v1.capabilities.files.FileComplete"
        public static let smsMessage = "type.googleapis.com/anchor.v1.capabilities.sms.SmsMessage"
        public static let smsSend = "type.googleapis.com/anchor.v1.capabilities.sms.SmsSend"
        public static let smsSendResult = "type.googleapis.com/anchor.v1.capabilities.sms.SmsSendResult"
        public static let commandRun = "type.googleapis.com/anchor.v1.capabilities.commands.CommandRun"
        public static let commandList = "type.googleapis.com/anchor.v1.capabilities.commands.CommandList"
        public static let commandKill = "type.googleapis.com/anchor.v1.capabilities.commands.CommandKill"
        public static let commandResult = "type.googleapis.com/anchor.v1.capabilities.commands.CommandResult"
    }
}

/// Length-delimited protobuf envelope accumulator for Anchor's one control
/// stream. Feed arbitrary QUIC stream chunks; `nextPayload()` returns complete
/// protobuf bytes in order and leaves partial data buffered.
public struct AnchorControlFramer {
    public static let maxRecordBytes = AnchorV1.maxControlRecordBytes

    private var buffer = Data()
    private var expectingPreface = true

    public init() {}

    public var bufferedByteCount: Int { buffer.count }

    public mutating func feed(_ bytes: Data) {
        buffer.append(bytes)
    }

    public mutating func nextPayload() throws -> Data? {
        if expectingPreface {
            guard buffer.count >= AnchorV1.controlMagic.count + 2 else { return nil }
            guard buffer.prefix(4) == AnchorV1.controlMagic else {
                throw AnchorWireError.invalidControlMagic
            }
            let major = buffer[buffer.startIndex + 4]
            let minor = buffer[buffer.startIndex + 5]
            guard major == AnchorV1.controlMajor, minor == AnchorV1.controlMinor else {
                throw AnchorWireError.unsupportedControlVersion(major: major, minor: minor)
            }
            buffer.removeFirst(6)
            expectingPreface = false
        }

        if buffer.count >= 10 {
            let varintBytes = buffer.prefix(10)
            let hasContinuation = varintBytes.allSatisfy { ($0 & 0x80) != 0 }
            let tenthByteOverflows = (varintBytes.last ?? 0) & 0x80 != 0 ||
                (varintBytes.last ?? 0) & 0x7f > 1
            if hasContinuation || tenthByteOverflows {
                throw AnchorWireError.malformedVarint
            }
        }
        guard let (length, prefixBytes) = Self.decodeVarint(buffer) else { return nil }
        guard length <= Self.maxRecordBytes else { throw AnchorWireError.controlRecordTooLarge }
        let total = prefixBytes + length
        guard buffer.count >= total else { return nil }
        buffer.removeFirst(prefixBytes)
        let payload = Data(buffer.prefix(length))
        buffer.removeFirst(length)
        return payload
    }

    public static func encodeFirst(_ payload: Data) throws -> Data {
        guard payload.count <= maxRecordBytes else { throw AnchorWireError.controlRecordTooLarge }
        var result = AnchorV1.controlMagic
        result.append(AnchorV1.controlMajor)
        result.append(AnchorV1.controlMinor)
        result.append(encodeVarint(UInt64(payload.count)))
        result.append(payload)
        return result
    }

    public static func encode(_ payload: Data) throws -> Data {
        guard payload.count <= maxRecordBytes else { throw AnchorWireError.controlRecordTooLarge }
        var result = encodeVarint(UInt64(payload.count))
        result.append(payload)
        return result
    }

    private static func encodeVarint(_ value: UInt64) -> Data {
        var value = value
        var result = Data()
        repeat {
            var byte = UInt8(value & 0x7f)
            value >>= 7
            if value != 0 { byte |= 0x80 }
            result.append(byte)
        } while value != 0
        return result
    }

    private static func decodeVarint(_ data: Data) -> (length: Int, bytes: Int)? {
        var value: UInt64 = 0
        var shift: UInt64 = 0
        for (index, byte) in data.prefix(10).enumerated() {
            let bits = UInt64(byte & 0x7f)
            if shift >= 64 || (shift == 63 && bits > 1) { return nil }
            value |= bits << shift
            if byte & 0x80 == 0 {
                guard value <= UInt64(Int.max) else { return nil }
                return (Int(value), index + 1)
            }
            shift += 7
        }
        // A ten-byte continuation is malformed once all bytes are present;
        // callers cannot distinguish it from a partial stream without an
        // additional byte, so leave it buffered until then.
        return nil
    }
}
