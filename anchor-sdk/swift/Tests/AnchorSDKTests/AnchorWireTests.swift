import XCTest
@testable import AnchorSDK

final class AnchorWireTests: XCTestCase {
    func testControlFramerHandlesPrefaceAndFragmentation() throws {
        let payload = Data([0x0a, 0x03, 0x66, 0x6f, 0x6f])
        let encoded = try AnchorControlFramer.encodeFirst(payload)
        var framer = AnchorControlFramer()
        for (index, byte) in encoded.enumerated() {
            framer.feed(Data([byte]))
            if index + 1 < encoded.count {
                XCTAssertNil(try framer.nextPayload())
            }
        }
        XCTAssertEqual(try framer.nextPayload(), payload)
        XCTAssertNil(try framer.nextPayload())
    }

    func testVideoDatagramRoundTripAndFragmentation() throws {
        let payload = Data(repeating: 0x2a, count: AnchorVideoFrameHeader.payloadBytes * 2 + 7)
        let packets = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.cameraKind,
            flags: AnchorVideoFrameHeader.keyframeFlag,
            capabilitySessionID: 9, flowID: 4, sequence: 12,
            presentationTimeUs: 1234, codecConfigID: 1, payload: payload
        )
        XCTAssertEqual(packets.count, 3)
        let decoded = packets.compactMap(AnchorVideoFrameCodec.decode)
        XCTAssertEqual(decoded.map(\.header.fragmentIndex), [0, 1, 2])
        XCTAssertEqual(decoded.flatMap { Array($0.payload) }, Array(payload))
    }

    func testVideoStreamFramerHandlesFragmentedAndCoalescedReads() throws {
        let first = try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 7, flowID: 4, sequence: 1,
            presentationTimeUs: 10, codecConfigID: 2, payload: Data([1, 2, 3])
        ).first)
        let second = try XCTUnwrap(AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 7, flowID: 4, sequence: 2,
            presentationTimeUs: 20, codecConfigID: 2, payload: Data([4, 5])
        ).first)
        let stream = try AnchorVideoStreamFramer.encode(first)
            + AnchorVideoStreamFramer.encode(second)
        var framer = AnchorVideoStreamFramer()
        framer.feed(Data(stream.prefix(3)))
        XCTAssertNil(try framer.nextPacket())
        framer.feed(Data(stream.dropFirst(3)))
        XCTAssertEqual(try framer.nextPacket(), first)
        XCTAssertEqual(try framer.nextPacket(), second)
        XCTAssertNil(try framer.nextPacket())
    }

    func testVideoStreamFramerRejectsEmptyAndOversizedRecordsBeforeAllocation() throws {
        var empty = AnchorVideoStreamFramer()
        empty.feed(Data(repeating: 0, count: 4))
        XCTAssertThrowsError(try empty.nextPacket()) {
            XCTAssertEqual($0 as? AnchorWireError, .invalidStreamPacketLength)
        }

        var oversized = AnchorVideoStreamFramer()
        let length = UInt32(AnchorVideoFrameHeader.datagramBytes + 1)
        oversized.feed(Data([
            UInt8(length & 0xff), UInt8((length >> 8) & 0xff),
            UInt8((length >> 16) & 0xff), UInt8((length >> 24) & 0xff),
        ]))
        XCTAssertThrowsError(try oversized.nextPacket()) {
            XCTAssertEqual($0 as? AnchorWireError, .frameTooLarge)
        }
    }

    func testScreenAssemblerReassemblesFrameAndEnforcesStreamBinding() throws {
        let payload = Data(repeating: 0x5a, count: AnchorVideoFrameHeader.payloadBytes + 11)
        let packets = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            flags: AnchorVideoFrameHeader.keyframeFlag,
            capabilitySessionID: 9, flowID: 12, sequence: 3,
            presentationTimeUs: 40, codecConfigID: 5, payload: payload
        )
        var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 9, streamID: 12)
        XCTAssertNil(try assembler.consume(packets[0]))
        let frame = try XCTUnwrap(assembler.consume(packets[1]))
        XCTAssertEqual(frame.payload, payload)
        XCTAssertEqual(frame.header.sequence, 3)
        XCTAssertEqual(frame.header.flags, AnchorVideoFrameHeader.keyframeFlag)

        var wrongBinding = AnchorScreenFrameAssembler(capabilitySessionID: 10, streamID: 12)
        XCTAssertThrowsError(try wrongBinding.consume(packets[0])) {
            XCTAssertEqual($0 as? AnchorWireError, .frameBindingMismatch)
        }
    }

    func testScreenAssemblerRejectsMalformedOrSplicedFragments() throws {
        var assembler = AnchorScreenFrameAssembler(capabilitySessionID: 9, streamID: 12)
        XCTAssertThrowsError(try assembler.consume(Data(repeating: 0, count: 52))) {
            XCTAssertEqual($0 as? AnchorWireError, .invalidFrameMagic)
        }

        let first = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 9, flowID: 12, sequence: 1,
            presentationTimeUs: 1, codecConfigID: 1,
            payload: Data(repeating: 1, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        let other = try AnchorVideoFrameCodec.fragment(
            kind: AnchorVideoFrameHeader.screenKind,
            capabilitySessionID: 9, flowID: 12, sequence: 2,
            presentationTimeUs: 2, codecConfigID: 1,
            payload: Data(repeating: 2, count: AnchorVideoFrameHeader.payloadBytes + 1)
        )
        XCTAssertNil(try assembler.consume(first[0]))
        XCTAssertThrowsError(try assembler.consume(other[1])) {
            XCTAssertEqual($0 as? AnchorWireError, .invalidFrameFragments)
        }
    }

    func testControlFramerRejectsUnboundedVarint() throws {
        var framer = AnchorControlFramer()
        framer.feed(AnchorV1.controlMagic)
        framer.feed(Data([AnchorV1.controlMajor, AnchorV1.controlMinor]))
        framer.feed(Data(repeating: 0x80, count: 10))
        XCTAssertThrowsError(try framer.nextPayload()) { error in
            XCTAssertEqual(error as? AnchorWireError, .malformedVarint)
        }
    }

    func testControlFramerDoesNotTreatPayloadBytesAsVarintBytes() throws {
        let payload = Data([0x0c] + Array(repeating: 0xff, count: 11))
        var framer = AnchorControlFramer()
        framer.feed(try AnchorControlFramer.encodeFirst(payload))
        XCTAssertEqual(try framer.nextPayload(), payload)
    }

    func testCapabilityCatalogContainsAllV1Capabilities() {
        XCTAssertEqual(AnchorV1Capabilities.all.map(\.name), [
            AnchorV1.Capability.clipboard,
            AnchorV1.Capability.device,
            AnchorV1.Capability.notifications,
            AnchorV1.Capability.input,
            AnchorV1.Capability.media,
            AnchorV1.Capability.screen,
            AnchorV1.Capability.camera,
            AnchorV1.Capability.files,
            AnchorV1.Capability.sms,
            AnchorV1.Capability.commands,
        ])
        XCTAssertTrue(AnchorV1Capabilities.descriptor(for: AnchorV1.Capability.camera)?.supportsDatagrams == true)
        XCTAssertTrue(AnchorV1Capabilities.descriptor(for: AnchorV1.Capability.files)?.accepts(typeURL: AnchorV1.TypeURL.fileOffer) == true)
    }

    func testSessionAddsPrefaceOnlyToFirstEnvelope() async throws {
        let transport = RecordingTransport()
        let session = AnchorSession(transport: transport)
        try await session.sendEnvelope(Data([1]))
        try await session.sendEnvelope(Data([2]))

        let writes = await transport.controlWrites
        XCTAssertEqual(writes.count, 2)
        XCTAssertEqual(Array(writes[0].prefix(6)), [0x41, 0x4e, 0x43, 0x52, 1, 0])
        XCTAssertEqual(Array(writes[1].prefix(2)), [1, 2])
    }

    func testSessionHandshakeBatchUsesOneOrderedWrite() async throws {
        let transport = RecordingTransport()
        let session = AnchorSession(transport: transport)
        try await session.sendEnvelopes([Data([1]), Data([2])])

        let writes = await transport.controlWrites
        XCTAssertEqual(writes.count, 1)
        var framer = AnchorControlFramer()
        framer.feed(writes[0])
        XCTAssertEqual(try framer.nextPayload(), Data([1]))
        XCTAssertEqual(try framer.nextPayload(), Data([2]))
    }

    func testGeneratedClipboardRecordRoundTrip() throws {
        var nodeID = ANCHNodeId()
        nodeID.value = Data(repeating: 0x42, count: 32)

        var publish = ANCHClipboardClipboardPublish()
        publish.originNodeID = nodeID
        publish.revision = 7
        publish.textUtf8 = "Anchor clipboard"

        var record = ANCHCapabilityRecord()
        record.capabilitySessionID = 9
        record.typeURL = AnchorV1.TypeURL.clipboardPublish
        record.payload = try publish.serializedData()

        var envelope = ANCHControlEnvelope()
        envelope.capabilityRecord = record
        let decodedEnvelope = try ANCHControlEnvelope(serializedBytes: envelope.serializedData())
        XCTAssertEqual(decodedEnvelope.capabilityRecord.capabilitySessionID, 9)
        XCTAssertEqual(decodedEnvelope.capabilityRecord.typeURL, AnchorV1.TypeURL.clipboardPublish)

        let decodedPublish = try ANCHClipboardClipboardPublish(
            serializedBytes: decodedEnvelope.capabilityRecord.payload
        )
        XCTAssertEqual(decodedPublish.originNodeID.value, nodeID.value)
        XCTAssertEqual(decodedPublish.revision, 7)
        XCTAssertEqual(decodedPublish.textUtf8, "Anchor clipboard")
    }

    func testSessionPreparesDatagramReceiveBeforeMediaFlowStarts() async throws {
        let transport = RecordingTransport()
        let session = AnchorSession(transport: transport)

        try await session.prepareDatagramReceive()

        XCTAssertEqual(transport.datagramPrepareCount, 1)
    }
}

private final class RecordingTransport: AnchorQuicTransport, @unchecked Sendable {
    private var writes = [Data]()
    private(set) var datagramPrepareCount = 0
    var controlWrites: [Data] { get async { writes } }

    func connect(host: String, port: UInt16, serverName: String,
                 expectedCertificateFingerprint: Data) async throws {}
    func sendControl(_ bytes: Data) async throws { writes.append(bytes) }
    func sendDatagram(_ bytes: Data) async throws {}
    func receiveControl() async throws -> Data { Data() }
    func receiveDatagram() async throws -> Data { Data() }
    func prepareDatagramReceive() async throws { datagramPrepareCount += 1 }
    func close() {}
}
