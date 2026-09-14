import Foundation

/// Fixed binary header shared by screen and camera QUIC datagrams.
public struct AnchorVideoFrameHeader: Equatable {
    public static let headerBytes = 52
    /// Common application datagram size with headroom for QUIC/overlay headers.
    public static let datagramBytes = 1100
    public static let payloadBytes = datagramBytes - headerBytes
    public static let maxFrameBytes = 8 * 1024 * 1024
    public static let screenKind: UInt8 = 1
    public static let cameraKind: UInt8 = 2
    public static let keyframeFlag: UInt16 = 1
    public static let codecConfigFlag: UInt16 = 2

    public let kind: UInt8
    public let flags: UInt16
    public let capabilitySessionID: UInt64
    public let flowID: UInt64
    public let sequence: UInt64
    public let fragmentIndex: UInt16
    public let fragmentCount: UInt16
    public let presentationTimeUs: UInt64
    public let codecConfigID: UInt64

    public init(kind: UInt8, flags: UInt16, capabilitySessionID: UInt64,
                flowID: UInt64, sequence: UInt64, fragmentIndex: UInt16,
                fragmentCount: UInt16, presentationTimeUs: UInt64,
                codecConfigID: UInt64) {
        self.kind = kind
        self.flags = flags
        self.capabilitySessionID = capabilitySessionID
        self.flowID = flowID
        self.sequence = sequence
        self.fragmentIndex = fragmentIndex
        self.fragmentCount = fragmentCount
        self.presentationTimeUs = presentationTimeUs
        self.codecConfigID = codecConfigID
    }
}

public struct AnchorVideoFramePacket: Equatable {
    public let header: AnchorVideoFrameHeader
    public let payload: Data
}

public enum AnchorVideoFrameCodec {
    private static let magic = Data([0x41, 0x4e, 0x46, 0x52]) // ANFR
    private static let version: UInt8 = 1

    public static func fragment(kind: UInt8, flags: UInt16 = 0,
                                capabilitySessionID: UInt64, flowID: UInt64,
                                sequence: UInt64, presentationTimeUs: UInt64,
                                codecConfigID: UInt64, payload: Data) throws -> [Data] {
        guard kind == AnchorVideoFrameHeader.screenKind || kind == AnchorVideoFrameHeader.cameraKind else {
            throw AnchorWireError.invalidFrameKind(kind)
        }
        guard !payload.isEmpty, payload.count <= AnchorVideoFrameHeader.maxFrameBytes else {
            throw AnchorWireError.frameTooLarge
        }
        let count = (payload.count + AnchorVideoFrameHeader.payloadBytes - 1) / AnchorVideoFrameHeader.payloadBytes
        guard count <= Int(UInt16.max) else { throw AnchorWireError.frameTooLarge }
        return (0..<count).map { index in
            let start = index * AnchorVideoFrameHeader.payloadBytes
            let end = min(start + AnchorVideoFrameHeader.payloadBytes, payload.count)
            let header = AnchorVideoFrameHeader(
                kind: kind, flags: flags, capabilitySessionID: capabilitySessionID,
                flowID: flowID, sequence: sequence, fragmentIndex: UInt16(index),
                fragmentCount: UInt16(count), presentationTimeUs: presentationTimeUs,
                codecConfigID: codecConfigID
            )
            return encode(header: header, payload: Data(payload[start..<end]))
        }
    }

    public static func decode(_ datagram: Data) -> AnchorVideoFramePacket? {
        guard datagram.count >= AnchorVideoFrameHeader.headerBytes,
              datagram.prefix(4) == magic,
              datagram[4] == version else { return nil }
        let kind = datagram[5]
        guard kind == AnchorVideoFrameHeader.screenKind || kind == AnchorVideoFrameHeader.cameraKind else { return nil }
        let fragmentIndex = readUInt16(datagram, at: 32)
        let fragmentCount = readUInt16(datagram, at: 34)
        guard fragmentCount > 0, fragmentIndex < fragmentCount else { return nil }
        let header = AnchorVideoFrameHeader(
            kind: kind,
            flags: readUInt16(datagram, at: 6),
            capabilitySessionID: readUInt64(datagram, at: 8),
            flowID: readUInt64(datagram, at: 16),
            sequence: readUInt64(datagram, at: 24),
            fragmentIndex: fragmentIndex,
            fragmentCount: fragmentCount,
            presentationTimeUs: readUInt64(datagram, at: 36),
            codecConfigID: readUInt64(datagram, at: 44)
        )
        return AnchorVideoFramePacket(header: header, payload: Data(datagram.dropFirst(AnchorVideoFrameHeader.headerBytes)))
    }

    private static func encode(header: AnchorVideoFrameHeader, payload: Data) -> Data {
        var result = magic
        result.append(version)
        result.append(header.kind)
        appendUInt16(header.flags, to: &result)
        appendUInt64(header.capabilitySessionID, to: &result)
        appendUInt64(header.flowID, to: &result)
        appendUInt64(header.sequence, to: &result)
        appendUInt16(header.fragmentIndex, to: &result)
        appendUInt16(header.fragmentCount, to: &result)
        appendUInt64(header.presentationTimeUs, to: &result)
        appendUInt64(header.codecConfigID, to: &result)
        result.append(payload)
        return result
    }

    private static func appendUInt16(_ value: UInt16, to data: inout Data) {
        data.append(UInt8(value & 0xff)); data.append(UInt8(value >> 8))
    }

    private static func appendUInt64(_ value: UInt64, to data: inout Data) {
        for shift in stride(from: 0, through: 56, by: 8) { data.append(UInt8((value >> UInt64(shift)) & 0xff)) }
    }

    private static func readUInt16(_ data: Data, at offset: Int) -> UInt16 {
        UInt16(data[offset]) | (UInt16(data[offset + 1]) << 8)
    }

    private static func readUInt64(_ data: Data, at offset: Int) -> UInt64 {
        var value: UInt64 = 0
        for index in 0..<8 { value |= UInt64(data[offset + index]) << UInt64(index * 8) }
        return value
    }
}
