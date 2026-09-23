import XCTest
@testable import AnchorSDK

final class AnchorFeatureCodecTests: XCTestCase {
    private let nodeID = Data(repeating: 0xA5, count: 32)

    func testClipboardTextPreservesEmptyStringAndRevision() throws {
        let bytes = try AnchorClipboardCodec.publishText(originNodeID: nodeID, revision: 4, text: "")
        let message = try ANCHClipboardClipboardPublish(serializedBytes: bytes)
        XCTAssertEqual(message.originNodeID.value, nodeID)
        XCTAssertEqual(message.revision, 4)
        XCTAssertEqual(message.content, .textUtf8(""))
        XCTAssertTrue(AnchorClipboardCodec.isEcho(message, localNodeID: nodeID))
    }

    func testClipboardPNGRoundTripDoesNotBase64EncodeWirePayload() throws {
        let png = Data([0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF])
        let bytes = try AnchorClipboardCodec.publishPNG(originNodeID: nodeID, revision: 5, png: png)
        let message = try ANCHClipboardClipboardPublish(serializedBytes: bytes)
        XCTAssertEqual(message.content, .png(png))
        XCTAssertFalse(AnchorClipboardCodec.isEcho(message, localNodeID: Data(repeating: 1, count: 32)))
    }

    func testClipboardClearAndNodeIDValidation() throws {
        let bytes = try AnchorClipboardCodec.clear(originNodeID: nodeID, revision: 6)
        let message = try ANCHClipboardClipboardClear(serializedBytes: bytes)
        XCTAssertEqual(message.originNodeID.value, nodeID)
        XCTAssertEqual(message.revision, 6)
        XCTAssertThrowsError(try AnchorClipboardCodec.publishText(originNodeID: Data([1]), revision: 1, text: "x")) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidNodeID)
        }
    }

    func testRelativeAndAbsolutePointerEncoding() throws {
        let relative = AnchorInputCodec.relative(dx: -1.25, dy: 0.875)
        XCTAssertEqual(relative.dx1000Ths, -1250)
        XCTAssertEqual(relative.dy1000Ths, 875)

        let absolute = try AnchorInputCodec.absolute(x: -1, y: 1.5)
        XCTAssertEqual(absolute.x, 0)
        XCTAssertEqual(absolute.y, 65_535)
        XCTAssertThrowsError(try AnchorInputCodec.absolute(x: .nan, y: 0))
    }

    func testPointerButtonMapping() {
        XCTAssertEqual(AnchorInputCodec.button(.left, pressed: true).button, .left)
        XCTAssertEqual(AnchorInputCodec.button(.middle, pressed: false).button, .middle)
        XCTAssertEqual(AnchorInputCodec.button(.right, pressed: true).button, .right)
    }

    func testHIDUsageMappingCoversUIKeysAndRejectsUnknownNames() {
        XCTAssertEqual(AnchorHIDUsage.keyboard("a"), 0x04)
        XCTAssertEqual(AnchorHIDUsage.keyboard("1"), 0x1E)
        XCTAssertEqual(AnchorHIDUsage.keyboard("0"), 0x27)
        XCTAssertEqual(AnchorHIDUsage.keyboard("BackSpace"), 0x2A)
        XCTAssertEqual(AnchorHIDUsage.keyboard("F12"), 0x45)
        XCTAssertNil(AnchorHIDUsage.keyboard("desktop-keycode-999"))
    }

    func testScrollAccumulatorPreservesSubStepMotionAndAxes() {
        var accumulator = AnchorScrollAccumulator()
        XCTAssertNil(accumulator.consume(horizontalSteps: 0, verticalSteps: 0.004))
        let first = accumulator.consume(horizontalSteps: 0, verticalSteps: 0.005)
        XCTAssertEqual(first?.vertical120Ths, 1)
        XCTAssertEqual(accumulator.consume(horizontalSteps: 0, verticalSteps: 0.008)?.vertical120Ths, 1)
        let second = accumulator.consume(horizontalSteps: -0.01, verticalSteps: 0)
        XCTAssertEqual(second?.horizontal120Ths, -1)
    }

    func testScreenCapabilityUsesReliableStreamContract() {
        let screen = AnchorV1Capabilities.descriptor(for: AnchorV1.Capability.screen)
        XCTAssertEqual(screen?.supportsDatagrams, false)
        XCTAssertTrue(screen?.accepts(typeURL: AnchorV1.TypeURL.screenFrame) == true)
    }

    func testStreamOpenBindingRoundTripAndMismatchRejection() throws {
        let request = try AnchorStreamBindingCodec.openEnvelope(
            requestID: 4,
            streamID: 8,
            capabilitySessionID: 3,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )
        XCTAssertEqual(request.requestID, 4)
        XCTAssertEqual(request.streamOpen.quicStreamID, 8)
        XCTAssertEqual(request.streamOpen.capabilitySessionID, 3)
        XCTAssertEqual(request.streamOpen.payloadTypeURL, AnchorV1.TypeURL.screenFrame)

        var opened = ANCHStreamOpened(); opened.quicStreamID = 8
        var reply = ANCHControlEnvelope(); reply.responseTo = 4; reply.streamOpened = opened
        XCTAssertNoThrow(try AnchorStreamBindingCodec.validateOpened(reply, requestID: 4, streamID: 8))
        XCTAssertThrowsError(try AnchorStreamBindingCodec.validateOpened(reply, requestID: 5, streamID: 8)) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .streamBindingMismatch)
        }
    }

    func testScreenControlBuildersPreserveViewerPreferences() throws {
        let start = try ANCHScreenScreenStart(serializedBytes: AnchorScreenCodec.start(
            maxFPS: 60, targetBitrateKbps: 8_000, outputID: "2"
        ))
        XCTAssertEqual(start.maxFps, 60)
        XCTAssertEqual(start.targetBitrateKbps, 8_000)
        XCTAssertEqual(start.outputID, "2")
        XCTAssertNoThrow(try ANCHScreenScreenStop(serializedBytes: AnchorScreenCodec.stop()))
        XCTAssertNoThrow(try ANCHScreenScreenRequestKeyframe(
            serializedBytes: AnchorScreenCodec.requestKeyframe()
        ))
    }
}
