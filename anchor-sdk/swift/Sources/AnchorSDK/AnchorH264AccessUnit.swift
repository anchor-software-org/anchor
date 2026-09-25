import Foundation

public enum AnchorH264AccessUnitError: Error, Equatable {
    case emptyInput
    case unexpectedBytesBeforeStartCode
    case emptyNALUnit
    case invalidNALHeader
    case nalUnitTooLarge
}

/// One H.264 Annex-B access unit split into parameter sets and video slices.
///
/// VideoToolbox expects all slices for one picture in a single sample. The
/// `avccPayload` contains the complete sample (excluding SPS/PPS) in source
/// order, with a four-byte big-endian length in place of each Annex-B start
/// code. SPS and PPS are exposed separately for the format description.
public struct AnchorH264AccessUnit: Equatable {
    public let nalUnits: [Data]
    public let sequenceParameterSets: [Data]
    public let pictureParameterSets: [Data]
    public let vclNALUnits: [Data]
    public let sampleNALUnits: [Data]
    public let avccPayload: Data
    public let isKeyframe: Bool

    private init(nalUnits: [Data]) throws {
        self.nalUnits = nalUnits
        self.sequenceParameterSets = nalUnits.filter { ($0[0] & 0x1F) == 7 }
        self.pictureParameterSets = nalUnits.filter { ($0[0] & 0x1F) == 8 }
        self.vclNALUnits = nalUnits.filter { (1...5).contains($0[0] & 0x1F) }
        self.sampleNALUnits = nalUnits.filter {
            let type = $0[0] & 0x1F
            return type != 7 && type != 8
        }
        self.isKeyframe = vclNALUnits.contains { ($0[0] & 0x1F) == 5 }

        var avcc = Data()
        for nal in sampleNALUnits {
            guard let length = UInt32(exactly: nal.count) else {
                throw AnchorH264AccessUnitError.nalUnitTooLarge
            }
            avcc.append(UInt8((length >> 24) & 0xFF))
            avcc.append(UInt8((length >> 16) & 0xFF))
            avcc.append(UInt8((length >> 8) & 0xFF))
            avcc.append(UInt8(length & 0xFF))
            avcc.append(nal)
        }
        self.avccPayload = avcc
    }

    /// Parse one complete H.264 access unit. Annex-B three- and four-byte start
    /// codes are accepted and may be mixed. A single raw NAL is also accepted
    /// for transports that already preserve NAL boundaries. Leading/trailing
    /// Annex-B zero bytes are discarded; empty NALs are rejected.
    public static func parseAnnexB(_ bytes: Data) throws -> AnchorH264AccessUnit {
        guard !bytes.isEmpty else { throw AnchorH264AccessUnitError.emptyInput }
        let source = [UInt8](bytes)
        guard let first = findStartCode(in: source, from: 0) else {
            try validateHeader(source[0])
            return try AnchorH264AccessUnit(nalUnits: [bytes])
        }
        guard source[..<first.offset].allSatisfy({ $0 == 0 }) else {
            throw AnchorH264AccessUnitError.unexpectedBytesBeforeStartCode
        }

        var units = [Data]()
        var start = first
        while true {
            let payloadStart = start.offset + start.length
            let next = findStartCode(in: source, from: payloadStart)
            var payloadEnd = next?.offset ?? source.count
            while payloadEnd > payloadStart, source[payloadEnd - 1] == 0 {
                payloadEnd -= 1
            }
            guard payloadEnd > payloadStart else {
                throw AnchorH264AccessUnitError.emptyNALUnit
            }

            let nal = Data(source[payloadStart..<payloadEnd])
            try validateHeader(nal[0])
            units.append(nal)

            guard let next else { break }
            start = next
        }
        return try AnchorH264AccessUnit(nalUnits: units)
    }

    private static func validateHeader(_ header: UInt8) throws {
        guard header & 0x80 == 0, header & 0x1F != 0 else {
            throw AnchorH264AccessUnitError.invalidNALHeader
        }
    }

    private static func findStartCode(
        in bytes: [UInt8],
        from start: Int
    ) -> (offset: Int, length: Int)? {
        guard start < bytes.count else { return nil }
        var index = start
        while index + 2 < bytes.count {
            if index + 3 < bytes.count,
               bytes[index] == 0, bytes[index + 1] == 0,
               bytes[index + 2] == 0, bytes[index + 3] == 1 {
                return (index, 4)
            }
            if bytes[index] == 0, bytes[index + 1] == 0, bytes[index + 2] == 1 {
                return (index, 3)
            }
            index += 1
        }
        return nil
    }
}
