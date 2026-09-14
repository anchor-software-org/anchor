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

    func testControlFramerRejectsUnboundedVarint() throws {
        var framer = AnchorControlFramer()
        framer.feed(AnchorV1.controlMagic)
        framer.feed(Data([AnchorV1.controlMajor, AnchorV1.controlMinor]))
        framer.feed(Data(repeating: 0x80, count: 10))
        XCTAssertThrowsError(try framer.nextPayload()) { error in
            XCTAssertEqual(error as? AnchorWireError, .malformedVarint)
        }
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
}

private final class RecordingTransport: AnchorQuicTransport, @unchecked Sendable {
    private var writes = [Data]()
    var controlWrites: [Data] { get async { writes } }

    func connect(host: String, port: UInt16, serverName: String,
                 expectedCertificateFingerprint: Data) async throws {}
    func sendControl(_ bytes: Data) async throws { writes.append(bytes) }
    func sendDatagram(_ bytes: Data) async throws {}
    func receiveControl() async throws -> Data { Data() }
    func receiveDatagram() async throws -> Data { Data() }
    func close() {}
}
