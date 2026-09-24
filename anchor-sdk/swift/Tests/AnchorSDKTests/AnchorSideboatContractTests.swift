import XCTest
@testable import AnchorSDK

/// Deterministic protocol tests for the reliable-stream screen path. These
/// intentionally stop at the Network.framework boundary: the Apple QUIC
/// implementation needs physical-device interoperability coverage, while the
/// byte and binding contracts below can be exhaustively checked on every host.
final class AnchorSideboatContractTests: XCTestCase {
    func testPeerDatagramLimitDistinguishesUnknownFromTooSmall() {
        XCTAssertTrue(AnchorPeerDatagramLimit.accepts(payloadBytes: 1_100, usableFrameSize: 0))
        XCTAssertTrue(AnchorPeerDatagramLimit.accepts(payloadBytes: 1_100, usableFrameSize: 1_100))
        XCTAssertFalse(AnchorPeerDatagramLimit.accepts(payloadBytes: 1_100, usableFrameSize: 1_099))
        XCTAssertFalse(AnchorPeerDatagramLimit.accepts(payloadBytes: 0, usableFrameSize: 1_100))
    }

    func testANFRHeaderMatchesRustLittleEndianWireLayout() throws {
        let packet = try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.keyframeFlag | AnchorVideoFrameHeader.codecConfigFlag,
            capabilitySessionID: 0x0102_0304_0506_0708,
            flowID: 0x1112_1314_1516_1718,
            sequence: 0x2122_2324_2526_2728,
            presentationTimeUs: 0x3132_3334_3536_3738,
            codecConfigID: 0x4142_4344_4546_4748,
            payload: Data([0xAA, 0xBB])
        ).first)

        XCTAssertEqual(Array(packet), [
            0x41, 0x4E, 0x46, 0x52, // ANFR
            0x01, 0x01,             // version, screen kind
            0x03, 0x00,             // keyframe | codec-config flags
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
            0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21,
            0x00, 0x00, 0x01, 0x00, // fragment index, fragment count
            0x38, 0x37, 0x36, 0x35, 0x34, 0x33, 0x32, 0x31,
            0x48, 0x47, 0x46, 0x45, 0x44, 0x43, 0x42, 0x41,
            0xAA, 0xBB,
        ])
    }

    func testReliableFramerHandlesEveryReadBoundary() throws {
        let first = try makePacket(sequence: 1, payload: Data([1, 2, 3]))
        let second = try makePacket(sequence: 2, payload: Data([4, 5]))
        let bytes = try AnchorVideoStreamFramer.encode(first)
            + AnchorVideoStreamFramer.encode(second)

        for split in 0...bytes.count {
            var framer = AnchorVideoStreamFramer()
            framer.feed(Data(bytes.prefix(split)))
            var packets = [Data]()
            while let packet = try framer.nextPacket() { packets.append(packet) }
            framer.feed(Data(bytes.dropFirst(split)))
            while let packet = try framer.nextPacket() { packets.append(packet) }
            XCTAssertEqual(packets, [first, second], "failed at byte split \(split)")
            XCTAssertEqual(framer.bufferedByteCount, 0)
        }
    }

    func testReliableFramerHandlesOneByteReads() throws {
        let packet = try makePacket(sequence: 7, payload: Data(repeating: 0x5A, count: 80))
        let encoded = try AnchorVideoStreamFramer.encode(packet)
        var framer = AnchorVideoStreamFramer()

        for (index, byte) in encoded.enumerated() {
            framer.feed(Data([byte]))
            if index + 1 < encoded.count {
                XCTAssertNil(try framer.nextPacket())
            }
        }
        XCTAssertEqual(try framer.nextPacket(), packet)
        XCTAssertNil(try framer.nextPacket())
    }

    func testAssemblerResetDropsPartialFrameAndAcceptsFreshKeyframe() throws {
        let partial = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 3, flowID: 4, sequence: 8,
            presentationTimeUs: 10, codecConfigID: 1,
            payload: Data(repeating: 1, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        let keyframePayload = Data([7, 8, 9])
        let keyframe = try makePacket(
            flags: AnchorVideoFrameHeader.keyframeFlag,
            sequence: 9,
            codecConfigID: 2,
            payload: keyframePayload
        )
        var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 3, streamID: 4)

        XCTAssertNil(try assembler.consume(partial[0]))
        assembler.reset()
        let decoded = try XCTUnwrap(assembler.consume(keyframe))
        XCTAssertEqual(decoded.payload, keyframePayload)
        XCTAssertEqual(decoded.header.sequence, 9)
        XCTAssertEqual(decoded.header.codecConfigID, 2)
        XCTAssertEqual(decoded.header.flags, AnchorVideoFrameHeader.keyframeFlag)
    }

    func testAssemblerRejectsWrongSessionStreamAndMediaKind() throws {
        let valid = try makePacket(sequence: 1, payload: Data([1]))
        let wrongSession = try makePacket(
            capabilitySessionID: 99, sequence: 1, payload: Data([1])
        )
        let wrongStream = try makePacket(flowID: 99, sequence: 1, payload: Data([1]))
        let camera = try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.cameraKind,
            capabilitySessionID: 3, flowID: 4, sequence: 1,
            presentationTimeUs: 1, codecConfigID: 1, payload: Data([1])
        ).first)

        for packet in [wrongSession, wrongStream, camera] {
            var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 3, streamID: 4)
            XCTAssertThrowsError(try assembler.consume(packet)) {
                XCTAssertEqual($0 as? AnchorWireError, .frameBindingMismatch)
            }
        }

        var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 3, streamID: 4)
        XCTAssertNoThrow(try assembler.consume(valid))
    }

    func testAssemblerRejectsMetadataChangesWithinAccessUnit() throws {
        let original = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.keyframeFlag,
            capabilitySessionID: 3, flowID: 4, sequence: 5,
            presentationTimeUs: 6, codecConfigID: 7,
            payload: Data(repeating: 1, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        let changed = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.codecConfigFlag,
            capabilitySessionID: 3, flowID: 4, sequence: 5,
            presentationTimeUs: 6, codecConfigID: 8,
            payload: Data(repeating: 2, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 3, streamID: 4)

        XCTAssertNil(try assembler.consume(original[0]))
        XCTAssertThrowsError(try assembler.consume(changed[1])) {
            XCTAssertEqual($0 as? AnchorWireError, .invalidFrameFragments)
        }
    }

    func testDatagramAssemblerReassemblesOutOfOrderFragments() throws {
        let payload = Data((0..<(AnchorVideoFrameHeader.payloadBytes * 2 + 17)).map {
            UInt8($0 % 251)
        })
        let packets = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.keyframeFlag,
            capabilitySessionID: 3,
            flowID: 7,
            sequence: 9,
            presentationTimeUs: 11,
            codecConfigID: 13,
            payload: payload
        )
        var assembler = AnchorScreenDatagramAssembler(capabilitySessionID: 3, flowID: 7)

        XCTAssertTrue(try assembler.consume(packets[2], atNanoseconds: 0).isEmpty)
        XCTAssertTrue(try assembler.consume(packets[0], atNanoseconds: 1).isEmpty)
        let completed = try assembler.consume(packets[1], atNanoseconds: 2)
        XCTAssertEqual(completed.count, 1)
        let frame = try XCTUnwrap(completed.first)
        XCTAssertEqual(frame.payload, payload)
        XCTAssertEqual(frame.header.sequence, 9)
        XCTAssertEqual(frame.header.fragmentIndex, 0)
    }

    func testDatagramAssemblerWaitsForReorderedSequenceWithoutBreakingReferenceChain() throws {
        func packet(sequence: UInt64, flags: UInt16 = 0) throws -> Data {
            try XCTUnwrap(AnchorVideoFrameCodec.fragment(
                kind: AnchorVideoFrameHeader.screenKind,
                flags: flags,
                capabilitySessionID: 3,
                flowID: 7,
                sequence: sequence,
                presentationTimeUs: sequence,
                codecConfigID: 1,
                payload: Data([UInt8(sequence)])
            ).first)
        }
        var assembler = AnchorScreenDatagramAssembler(capabilitySessionID: 3, flowID: 7)

        XCTAssertEqual(
            try assembler.consume(
                packet(sequence: 20, flags: AnchorVideoFrameHeader.keyframeFlag),
                atNanoseconds: 0
            ).map(\.header.sequence),
            [20]
        )
        XCTAssertTrue(try assembler.consume(packet(sequence: 22), atNanoseconds: 1).isEmpty)
        let reordered = try assembler.consume(packet(sequence: 21), atNanoseconds: 49_000_000)
        XCTAssertEqual(reordered.map(\.header.sequence), [21, 22])
        XCTAssertFalse(assembler.takeKeyframeRequest())
    }

    func testDatagramAssemblerRecoversFromRealLossAtTimeoutBoundary() throws {
        func packet(sequence: UInt64, flags: UInt16 = 0) throws -> Data {
            try XCTUnwrap(AnchorVideoFrameCodec.fragment(
                kind: AnchorVideoFrameHeader.screenKind,
                flags: flags,
                capabilitySessionID: 3,
                flowID: 7,
                sequence: sequence,
                presentationTimeUs: sequence,
                codecConfigID: 1,
                payload: Data([UInt8(sequence)])
            ).first)
        }
        var assembler = AnchorScreenDatagramAssembler(capabilitySessionID: 3, flowID: 7)
        _ = try assembler.consume(
            packet(sequence: 20, flags: AnchorVideoFrameHeader.keyframeFlag),
            atNanoseconds: 0
        )
        XCTAssertTrue(try assembler.consume(packet(sequence: 22), atNanoseconds: 0).isEmpty)
        XCTAssertTrue(assembler.poll(atNanoseconds: 49_999_999).isEmpty)
        XCTAssertFalse(assembler.takeKeyframeRequest())
        XCTAssertTrue(assembler.poll(atNanoseconds: 50_000_000).isEmpty)
        XCTAssertTrue(assembler.takeKeyframeRequest())
        XCTAssertTrue(try assembler.consume(packet(sequence: 23), atNanoseconds: 50_000_001).isEmpty)
        let recovered = try assembler.consume(
            packet(sequence: 24, flags: AnchorVideoFrameHeader.keyframeFlag),
            atNanoseconds: 50_000_002
        )
        XCTAssertEqual(recovered.map(\.header.sequence), [24])
        XCTAssertFalse(assembler.takeKeyframeRequest())
    }

    func testDatagramAssemblerExpiresIncompleteFrameAndIgnoresItsLateFragment() throws {
        let keyframe = try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.keyframeFlag,
            capabilitySessionID: 3,
            flowID: 7,
            sequence: 20,
            presentationTimeUs: 20,
            codecConfigID: 1,
            payload: Data([1])
        ).first)
        let incomplete = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 3,
            flowID: 7,
            sequence: 21,
            presentationTimeUs: 21,
            codecConfigID: 1,
            payload: Data(repeating: 2, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        var assembler = AnchorScreenDatagramAssembler(capabilitySessionID: 3, flowID: 7)

        XCTAssertEqual(try assembler.consume(keyframe, atNanoseconds: 0).count, 1)
        XCTAssertTrue(try assembler.consume(incomplete[0], atNanoseconds: 1).isEmpty)
        XCTAssertTrue(assembler.poll(atNanoseconds: 250_000_001).isEmpty)
        XCTAssertTrue(assembler.takeKeyframeRequest())
        XCTAssertTrue(try assembler.consume(incomplete[1], atNanoseconds: 250_000_002).isEmpty)
    }

    func testDatagramAssemblerPreservesOrderAcrossStructuredFragmentShuffles() throws {
        for seed in UInt64(1)...64 {
            var random = seed
            var assembler = AnchorScreenDatagramAssembler(capabilitySessionID: 3, flowID: 7)
            var delivered: [UInt64] = []
            var now: UInt64 = 0

            for windowStart in stride(from: UInt64(0), through: 32, by: 4) {
                var packets: [Data] = []
                for sequence in windowStart...min(windowStart + 3, 32) {
                    packets += try AnchorVideoFrameCodec.fragment(
                        kind: AnchorVideoFrameHeader.screenKind,
                        flags: sequence == 0 ? AnchorVideoFrameHeader.keyframeFlag : 0,
                        capabilitySessionID: 3,
                        flowID: 7,
                        sequence: sequence,
                        presentationTimeUs: sequence,
                        codecConfigID: 1,
                        payload: Data(repeating: UInt8(sequence), count: Int(sequence * 791 % 3_000) + 1)
                    )
                }
                if packets.count > 1 {
                    for index in stride(from: packets.count - 1, through: 1, by: -1) {
                        random = random &* 6_364_136_223_846_793_005 &+ 1
                        packets.swapAt(index, Int(random % UInt64(index + 1)))
                    }
                }
                for packet in packets {
                    delivered += try assembler.consume(packet, atNanoseconds: now).map(\.header.sequence)
                    now += 1_000
                }
            }

            XCTAssertEqual(delivered, Array(UInt64(0)...32), "seed=\(seed)")
            XCTAssertFalse(assembler.takeKeyframeRequest(), "seed=\(seed)")
        }
    }

    func testStreamBindingRejectsEveryMismatchedAcknowledgementField() throws {
        XCTAssertThrowsError(try AnchorStreamBindingCodec.openEnvelope(
            requestID: 0, streamID: 4, capabilitySessionID: 3,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidRequestID)
        }
        XCTAssertThrowsError(try AnchorStreamBindingCodec.openEnvelope(
            requestID: 1, streamID: 4, capabilitySessionID: 0,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidCapabilitySessionID)
        }

        var opened = ANCHStreamOpened()
        opened.quicStreamID = 4
        var reply = ANCHControlEnvelope()
        reply.responseTo = 10
        reply.streamOpened = opened

        XCTAssertNoThrow(try AnchorStreamBindingCodec.validateOpened(
            reply, requestID: 10, streamID: 4
        ))
        XCTAssertThrowsError(try AnchorStreamBindingCodec.validateOpened(
            reply, requestID: 11, streamID: 4
        ))
        XCTAssertThrowsError(try AnchorStreamBindingCodec.validateOpened(
            reply, requestID: 10, streamID: 8
        ))

        var wrongBody = ANCHControlEnvelope()
        wrongBody.responseTo = 10
        wrongBody.pong = ANCHPong()
        XCTAssertThrowsError(try AnchorStreamBindingCodec.validateOpened(
            wrongBody, requestID: 10, streamID: 4
        ))
    }

    private func makePacket(
        flags: UInt16 = 0,
        capabilitySessionID: UInt64 = 3,
        flowID: UInt64 = 4,
        sequence: UInt64,
        codecConfigID: UInt64 = 1,
        payload: Data
    ) throws -> Data {
        try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: flags,
            capabilitySessionID: capabilitySessionID,
            flowID: flowID,
            sequence: sequence,
            presentationTimeUs: sequence * 1_000,
            codecConfigID: codecConfigID,
            payload: payload
        ).first)
    }
}
