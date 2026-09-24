import Foundation
import Dispatch

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

/// One complete encoded access unit reconstructed from one or more ANFR
/// packets on a reliable stream.
public struct AnchorVideoFrame: Equatable {
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
        try? decodeValidated(datagram)
    }

    /// Strict decoder used by reliable streams, where malformed bytes must end
    /// the binding rather than being silently treated as a lost datagram.
    public static func decodeValidated(_ packet: Data) throws -> AnchorVideoFramePacket {
        guard packet.count >= AnchorVideoFrameHeader.headerBytes else {
            throw AnchorWireError.invalidFrameMagic
        }
        guard packet.prefix(4) == magic else { throw AnchorWireError.invalidFrameMagic }
        guard packet[4] == version else {
            throw AnchorWireError.unsupportedFrameVersion(packet[4])
        }
        let kind = packet[5]
        guard kind == AnchorVideoFrameHeader.screenKind || kind == AnchorVideoFrameHeader.cameraKind else {
            throw AnchorWireError.invalidFrameKind(kind)
        }
        let fragmentIndex = readUInt16(packet, at: 32)
        let fragmentCount = readUInt16(packet, at: 34)
        guard fragmentCount > 0, fragmentIndex < fragmentCount else {
            throw AnchorWireError.invalidFrameFragments
        }
        let header = AnchorVideoFrameHeader(
            kind: kind,
            flags: readUInt16(packet, at: 6),
            capabilitySessionID: readUInt64(packet, at: 8),
            flowID: readUInt64(packet, at: 16),
            sequence: readUInt64(packet, at: 24),
            fragmentIndex: fragmentIndex,
            fragmentCount: fragmentCount,
            presentationTimeUs: readUInt64(packet, at: 36),
            codecConfigID: readUInt64(packet, at: 44)
        )
        return AnchorVideoFramePacket(
            header: header,
            payload: Data(packet.dropFirst(AnchorVideoFrameHeader.headerBytes))
        )
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

/// Byte-stream framing used by `org.anchor.screen@1`. QUIC stream reads can
/// split or coalesce writes, so every ANFR packet carries a little-endian u32
/// length prefix. The packet itself remains bounded to the protocol datagram
/// size even though delivery is reliable.
public struct AnchorVideoStreamFramer {
    private var buffer = Data()
    private var readOffset = 0

    public init() {}

    public var bufferedByteCount: Int { buffer.count - readOffset }

    public mutating func feed(_ bytes: Data) {
        compactIfNeeded(force: readOffset == buffer.count)
        buffer.append(bytes)
    }

    public mutating func nextPacket() throws -> Data? {
        guard bufferedByteCount >= 4 else { return nil }
        let start = buffer.startIndex + readOffset
        let length = Int(buffer[start])
            | (Int(buffer[start + 1]) << 8)
            | (Int(buffer[start + 2]) << 16)
            | (Int(buffer[start + 3]) << 24)
        guard length > 0 else { throw AnchorWireError.invalidStreamPacketLength }
        guard length <= AnchorVideoFrameHeader.datagramBytes else {
            throw AnchorWireError.frameTooLarge
        }
        guard bufferedByteCount >= 4 + length else { return nil }
        let payloadStart = start + 4
        let packet = Data(buffer[payloadStart..<(payloadStart + length)])
        readOffset += 4 + length
        compactIfNeeded(force: readOffset == buffer.count)
        return packet
    }

    /// Avoid quadratic front-removal copies when a single QUIC read contains
    /// dozens of ~1 KiB ANFR packets. Compact only after consuming a useful
    /// chunk, or reset cheaply when the buffer is empty.
    private mutating func compactIfNeeded(force: Bool = false) {
        guard readOffset > 0, force || readOffset >= 64 * 1024 else { return }
        if readOffset == buffer.count {
            buffer.removeAll(keepingCapacity: true)
        } else {
            buffer = Data(buffer.dropFirst(readOffset))
        }
        readOffset = 0
    }

    public static func encode(_ packet: Data) throws -> Data {
        guard !packet.isEmpty else { throw AnchorWireError.invalidStreamPacketLength }
        guard packet.count <= AnchorVideoFrameHeader.datagramBytes else {
            throw AnchorWireError.frameTooLarge
        }
        let length = UInt32(packet.count)
        var framed = Data([
            UInt8(length & 0xff),
            UInt8((length >> 8) & 0xff),
            UInt8((length >> 16) & 0xff),
            UInt8((length >> 24) & 0xff),
        ])
        framed.append(packet)
        return framed
    }
}

/// Reassembles screen access units while enforcing the `StreamOpen` binding.
/// Reliable delivery means fragments must arrive contiguously and in order;
/// accepting a different session/stream or splicing sequences is a protocol
/// violation, not recoverable packet loss.
public struct AnchorScreenFrameAssembler {
    public let capabilitySessionID: UInt64
    public let streamID: UInt64

    private var pendingHeader: AnchorVideoFrameHeader?
    private var pendingPayload = Data()
    private var nextFragmentIndex: UInt16 = 0

    public init(capabilitySessionID: UInt64, streamID: UInt64) {
        self.capabilitySessionID = capabilitySessionID
        self.streamID = streamID
    }

    public mutating func consume(_ bytes: Data) throws -> AnchorVideoFrame? {
        let packet = try AnchorVideoFrameCodec.decodeValidated(bytes)
        let header = packet.header
        guard header.kind == AnchorVideoFrameHeader.screenKind,
              header.capabilitySessionID == capabilitySessionID,
              header.flowID == streamID else {
            throw AnchorWireError.frameBindingMismatch
        }

        if header.fragmentIndex == 0 {
            guard pendingHeader == nil else { throw AnchorWireError.invalidFrameFragments }
            pendingHeader = header
            pendingPayload = packet.payload
            pendingPayload.reserveCapacity(min(
                AnchorVideoFrameHeader.maxFrameBytes,
                Int(header.fragmentCount) * AnchorVideoFrameHeader.payloadBytes
            ))
            nextFragmentIndex = 1
        } else {
            guard let first = pendingHeader,
                  header.fragmentIndex == nextFragmentIndex,
                  header.fragmentCount == first.fragmentCount,
                  header.sequence == first.sequence,
                  header.flags == first.flags,
                  header.presentationTimeUs == first.presentationTimeUs,
                  header.codecConfigID == first.codecConfigID else {
                throw AnchorWireError.invalidFrameFragments
            }
            guard pendingPayload.count + packet.payload.count <= AnchorVideoFrameHeader.maxFrameBytes else {
                throw AnchorWireError.frameTooLarge
            }
            pendingPayload.append(packet.payload)
            nextFragmentIndex += 1
        }

        guard let first = pendingHeader,
              header.fragmentIndex + 1 == header.fragmentCount else { return nil }
        let frame = AnchorVideoFrame(header: first, payload: pendingPayload)
        pendingHeader = nil
        pendingPayload = Data()
        nextFragmentIndex = 0
        return frame
    }

    public mutating func reset() {
        pendingHeader = nil
        pendingPayload = Data()
        nextFragmentIndex = 0
    }
}

/// Reassembles screen access units from unordered QUIC datagrams. A small,
/// bounded reorder window prevents harmless packet reordering from breaking an
/// H.264 reference chain. Real loss clears dependent frames and waits for IDR.
public struct AnchorScreenDatagramAssembler {
    public let capabilitySessionID: UInt64
    public let flowID: UInt64

    private struct PendingFrame {
        let header: AnchorVideoFrameHeader
        let firstSeenNanoseconds: UInt64
        var fragments: [Data?]
        var receivedCount = 0
        var receivedBytes = 0
        var completedAtNanoseconds: UInt64?

        var isComplete: Bool { receivedCount == fragments.count }
        var isKeyframe: Bool {
            header.flags & AnchorVideoFrameHeader.keyframeFlag != 0
        }
    }

    private var pending: [UInt64: PendingFrame] = [:]
    private var retainedBytes = 0
    private var lastDeliveredSequence: UInt64?
    private var waitingForKeyframe = true
    private var keyframeRequestPending = false
    private var gapSinceNanoseconds: UInt64?

    private static let maximumPendingFrames = 8
    private static let maximumFragmentsPerFrame = 4_096
    private static let reorderTimeoutNanoseconds: UInt64 = 50_000_000
    private static let assemblyTimeoutNanoseconds: UInt64 = 250_000_000

    public init(capabilitySessionID: UInt64, flowID: UInt64) {
        self.capabilitySessionID = capabilitySessionID
        self.flowID = flowID
    }

    public mutating func consume(
        _ bytes: Data,
        atNanoseconds now: UInt64 = DispatchTime.now().uptimeNanoseconds
    ) throws -> [AnchorVideoFrame] {
        let packet = try AnchorVideoFrameCodec.decodeValidated(bytes)
        let header = packet.header
        guard header.kind == AnchorVideoFrameHeader.screenKind,
              header.capabilitySessionID == capabilitySessionID,
              header.flowID == flowID else {
            throw AnchorWireError.frameBindingMismatch
        }
        guard header.fragmentCount <= Self.maximumFragmentsPerFrame else {
            throw AnchorWireError.invalidFrameFragments
        }

        expireIncompleteFrames(atNanoseconds: now)

        if let lastDeliveredSequence, header.sequence <= lastDeliveredSequence {
            return []
        }
        if pending[header.sequence] == nil,
           pending.count >= Self.maximumPendingFrames {
            enterRecovery()
        }
        if pending[header.sequence] == nil {
            pending[header.sequence] = PendingFrame(
                header: header,
                firstSeenNanoseconds: now,
                fragments: Array(repeating: nil, count: Int(header.fragmentCount))
            )
        }

        guard var current = pending[header.sequence],
              header.sequence == current.header.sequence,
              header.fragmentCount == current.header.fragmentCount,
              header.flags == current.header.flags,
              header.presentationTimeUs == current.header.presentationTimeUs,
              header.codecConfigID == current.header.codecConfigID else {
            discard(sequence: header.sequence)
            enterRecovery()
            throw AnchorWireError.invalidFrameFragments
        }

        let index = Int(header.fragmentIndex)
        if let existing = current.fragments[index] {
            guard existing == packet.payload else {
                enterRecovery()
                throw AnchorWireError.invalidFrameFragments
            }
            return drainCompletedFrames(atNanoseconds: now)
        }
        guard current.receivedBytes + packet.payload.count <= AnchorVideoFrameHeader.maxFrameBytes,
              retainedBytes + packet.payload.count <= AnchorVideoFrameHeader.maxFrameBytes else {
            enterRecovery()
            throw AnchorWireError.frameTooLarge
        }
        current.fragments[index] = packet.payload
        current.receivedCount += 1
        current.receivedBytes += packet.payload.count
        retainedBytes += packet.payload.count
        if current.isComplete {
            current.completedAtNanoseconds = now
        }
        pending[header.sequence] = current
        return drainCompletedFrames(atNanoseconds: now)
    }

    /// Advances loss timers when no new datagram is available.
    public mutating func poll(
        atNanoseconds now: UInt64 = DispatchTime.now().uptimeNanoseconds
    ) -> [AnchorVideoFrame] {
        expireIncompleteFrames(atNanoseconds: now)
        return drainCompletedFrames(atNanoseconds: now)
    }

    public mutating func takeKeyframeRequest() -> Bool {
        guard keyframeRequestPending else { return false }
        keyframeRequestPending = false
        return true
    }

    private mutating func drainCompletedFrames(atNanoseconds now: UInt64) -> [AnchorVideoFrame] {
        var output: [AnchorVideoFrame] = []

        if waitingForKeyframe,
           !pending.values.contains(where: { $0.isComplete && $0.isKeyframe }) {
            keyframeRequestPending = true
        }

        while !pending.isEmpty {
            let expected = lastDeliveredSequence.map { $0 &+ 1 }
            let expectedReady = expected.flatMap { sequence in
                pending[sequence]?.isComplete == true ? sequence : nil
            }
            let recoveryKeyframe = pending
                .filter { $0.value.isComplete && $0.value.isKeyframe }
                .map(\.key)
                .min()
            let candidate = !waitingForKeyframe ? (expectedReady ?? recoveryKeyframe) : recoveryKeyframe

            if let candidate, let frame = pending[candidate] {
                output.append(assembledFrame(frame))
                lastDeliveredSequence = candidate
                waitingForKeyframe = false
                keyframeRequestPending = false
                gapSinceNanoseconds = nil
                discardThrough(sequence: candidate)
                continue
            }

            if !waitingForKeyframe, gapSinceNanoseconds == nil, let expected {
                gapSinceNanoseconds = pending
                    .filter { $0.key > expected && $0.value.isComplete }
                    .compactMap { $0.value.completedAtNanoseconds }
                    .min()
            }
            if let gapSinceNanoseconds,
               now >= gapSinceNanoseconds,
               now - gapSinceNanoseconds >= Self.reorderTimeoutNanoseconds {
                enterRecovery()
            }
            break
        }
        return output
    }

    private func assembledFrame(_ current: PendingFrame) -> AnchorVideoFrame {
        var payload = Data()
        payload.reserveCapacity(current.receivedBytes)
        for fragment in current.fragments {
            if let fragment { payload.append(fragment) }
        }
        let canonicalHeader = AnchorVideoFrameHeader(
            kind: current.header.kind,
            flags: current.header.flags,
            capabilitySessionID: current.header.capabilitySessionID,
            flowID: current.header.flowID,
            sequence: current.header.sequence,
            fragmentIndex: 0,
            fragmentCount: current.header.fragmentCount,
            presentationTimeUs: current.header.presentationTimeUs,
            codecConfigID: current.header.codecConfigID
        )
        return AnchorVideoFrame(header: canonicalHeader, payload: payload)
    }

    private mutating func expireIncompleteFrames(atNanoseconds now: UInt64) {
        if pending.values.contains(where: {
            !$0.isComplete && now >= $0.firstSeenNanoseconds &&
                now - $0.firstSeenNanoseconds >= Self.assemblyTimeoutNanoseconds
        }) {
            enterRecovery()
        }
    }

    private mutating func discard(sequence: UInt64) {
        if let removed = pending.removeValue(forKey: sequence) {
            retainedBytes -= removed.receivedBytes
        }
    }

    private mutating func discardThrough(sequence: UInt64) {
        for key in pending.keys.filter({ $0 <= sequence }) {
            discard(sequence: key)
        }
    }

    private mutating func enterRecovery() {
        pending.removeAll(keepingCapacity: true)
        retainedBytes = 0
        gapSinceNanoseconds = nil
        waitingForKeyframe = true
        keyframeRequestPending = true
    }

    public mutating func reset() {
        pending.removeAll(keepingCapacity: true)
        retainedBytes = 0
        lastDeliveredSequence = nil
        waitingForKeyframe = true
        keyframeRequestPending = false
        gapSinceNanoseconds = nil
    }
}
