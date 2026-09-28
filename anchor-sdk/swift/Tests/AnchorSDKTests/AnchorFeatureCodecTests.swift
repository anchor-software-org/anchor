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

    func testClipboardRejectsOversizeAndMissingContent() throws {
        XCTAssertThrowsError(try AnchorClipboardCodec.publishPNG(
            originNodeID: nodeID,
            revision: 1,
            png: Data(repeating: 1, count: AnchorV1.maxControlRecordBytes)
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .recordTooLarge) }

        var missing = ANCHClipboardClipboardPublish()
        var node = ANCHNodeId(); node.value = nodeID
        missing.originNodeID = node
        XCTAssertThrowsError(try AnchorClipboardCodec.decodePublish(missing.serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .missingClipboardContent)
        }
    }

    func testClipboardRevisionTrackerOrdersEachOriginIndependently() throws {
        var tracker = AnchorClipboardRevisionTracker()
        let other = Data(repeating: 0xBB, count: 32)
        XCTAssertTrue(try tracker.shouldApply(originNodeID: nodeID, revision: 2))
        XCTAssertFalse(try tracker.shouldApply(originNodeID: nodeID, revision: 2))
        XCTAssertFalse(try tracker.shouldApply(originNodeID: nodeID, revision: 1))
        XCTAssertTrue(try tracker.shouldApply(originNodeID: other, revision: 1))
        XCTAssertEqual(tracker.greatestRevision(for: nodeID), 2)
        XCTAssertThrowsError(try tracker.shouldApply(originNodeID: nodeID, revision: 0))
    }

    func testFilesCodecRoundTripsAllV1Records() throws {
        XCTAssertTrue(AnchorV1Capabilities.descriptor(for: AnchorV1.Capability.files)?.accepts(
            typeURL: AnchorV1.TypeURL.fileContent
        ) == true)
        let transferID = Data(0..<16)
        let hash = Data(repeating: 0x42, count: 32)
        let offer = AnchorFileOffer(
            transferID: transferID,
            filename: "photo.jpg",
            mimeType: "image/jpeg",
            byteLength: 1_234,
            sha256: hash
        )
        XCTAssertEqual(try AnchorFilesCodec.decodeOffer(AnchorFilesCodec.offer(offer)), offer)
        XCTAssertEqual(
            try AnchorFilesCodec.decodeDecision(AnchorFilesCodec.decision(transferID: transferID, accepted: true)),
            AnchorFileDecision(transferID: transferID, accepted: true)
        )
        XCTAssertEqual(
            try AnchorFilesCodec.decodeContentStart(AnchorFilesCodec.contentStart(transferID: transferID, quicStreamID: 7)),
            AnchorFileContentStart(transferID: transferID, quicStreamID: 7)
        )
        XCTAssertEqual(
            try AnchorFilesCodec.decodeComplete(AnchorFilesCodec.complete(transferID: transferID, sha256: hash)),
            AnchorFileComplete(transferID: transferID, sha256: hash)
        )
    }

    func testFilesCodecRejectsTraversalLengthsAndOversizeOffers() throws {
        let transferID = Data(repeating: 1, count: 16)
        let hash = Data(repeating: 2, count: 32)
        func offer(_ name: String, length: UInt64 = 1) -> AnchorFileOffer {
            AnchorFileOffer(transferID: transferID, filename: name, mimeType: "", byteLength: length, sha256: hash)
        }
        for name in ["", ".", "..", "../x", "a/b", "a\\b", "bad\u{0000}name"] {
            XCTAssertThrowsError(try AnchorFilesCodec.offer(offer(name))) {
                XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidFilename)
            }
        }
        XCTAssertThrowsError(try AnchorFilesCodec.offer(offer("x", length: 11), maximumByteLength: 10)) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .fileTooLarge)
        }
        XCTAssertThrowsError(try AnchorFilesCodec.decision(transferID: Data(repeating: 1, count: 15), accepted: true))
        XCTAssertThrowsError(try AnchorFilesCodec.complete(transferID: transferID, sha256: Data(repeating: 1, count: 31)))
    }

    func testFilesCodecValidatesDecodedOffersInsteadOfTrustingProtobufPayloads() throws {
        let transferID = Data(repeating: 1, count: AnchorFilesCodec.transferIDByteCount)
        let hash = Data(repeating: 2, count: AnchorFilesCodec.sha256ByteCount)

        func encodedOffer(
            transferID: Data = transferID,
            filename: String = "safe.txt",
            byteLength: UInt64 = 12,
            sha256: Data = hash
        ) throws -> Data {
            var message = ANCHFilesFileOffer()
            message.transferID = transferID
            message.filename = filename
            message.byteLength = byteLength
            message.sha256 = sha256
            return try message.serializedData()
        }

        XCTAssertThrowsError(try AnchorFilesCodec.decodeOffer(
            encodedOffer(transferID: Data(repeating: 1, count: 15))
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidTransferID) }
        XCTAssertThrowsError(try AnchorFilesCodec.decodeOffer(
            encodedOffer(filename: "folder/escape.txt")
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidFilename) }
        XCTAssertThrowsError(try AnchorFilesCodec.decodeOffer(
            encodedOffer(filename: String(repeating: "é", count: 128))
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidFilename) }
        XCTAssertThrowsError(try AnchorFilesCodec.decodeOffer(
            encodedOffer(byteLength: 101), maximumByteLength: 100
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .fileTooLarge) }
        XCTAssertThrowsError(try AnchorFilesCodec.decodeOffer(
            encodedOffer(sha256: Data(repeating: 2, count: 31))
        )) { XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidFileHash) }
    }

    func testFilesCodecValidatesDecodedDecisionBindingAndCompletionIdentity() throws {
        var decision = ANCHFilesFileDecision()
        decision.transferID = Data(repeating: 1, count: 17)
        XCTAssertThrowsError(try AnchorFilesCodec.decodeDecision(decision.serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidTransferID)
        }

        var start = ANCHFilesFileContentStart()
        start.transferID = Data(repeating: 1, count: 15)
        start.quicStreamID = 9
        XCTAssertThrowsError(try AnchorFilesCodec.decodeContentStart(start.serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidTransferID)
        }

        var complete = ANCHFilesFileComplete()
        complete.transferID = Data(repeating: 1, count: 16)
        complete.sha256 = Data(repeating: 2, count: 33)
        XCTAssertThrowsError(try AnchorFilesCodec.decodeComplete(complete.serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidFileHash)
        }
    }

    func testMediaCodecRoundTripsCommandsAndBoundsState() throws {
        for command in [
            AnchorMediaCommand.play, .pause, .next, .previous,
            .seek(positionMilliseconds: 900),
        ] {
            XCTAssertEqual(try AnchorMediaCodec.decodeCommand(AnchorMediaCodec.command(command)), command)
        }

        var message = ANCHMediaMediaState()
        message.title = "Title"
        message.artist = "Artist"
        message.album = "Album"
        message.playing = true
        message.positionMs = 1_500
        message.durationMs = 1_000
        message.artworkJpeg = Data([0xFF, 0xD8, 0xFF, 0xD9])
        let state = try AnchorMediaCodec.decodeState(message.serializedData())
        XCTAssertEqual(state.positionMilliseconds, 1_000)
        XCTAssertEqual(state.artworkJPEG, message.artworkJpeg)
    }

    func testMediaCodecRejectsUnknownCommandAndOversizeArtwork() throws {
        XCTAssertThrowsError(try AnchorMediaCodec.decodeCommand(ANCHMediaMediaCommand().serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .invalidMediaCommand)
        }
        var state = ANCHMediaMediaState()
        state.artworkJpeg = Data(repeating: 1, count: AnchorMediaCodec.maximumArtworkJPEGBytes + 1)
        XCTAssertThrowsError(try AnchorMediaCodec.decodeState(state.serializedData())) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .artworkTooLarge)
        }
    }

    func testRelativeAndAbsolutePointerEncoding() throws {
        let relative = AnchorInputCodec.relative(dx: -1.25, dy: 0.875)
        XCTAssertEqual(relative.dx1000Ths, -1250)
        XCTAssertEqual(relative.dy1000Ths, 875)

        let absolute = try AnchorInputCodec.absolute(
            x: -1, y: 1.5, targetOutputName: "HEADLESS-3"
        )
        XCTAssertEqual(absolute.x, 0)
        XCTAssertEqual(absolute.y, 65_535)
        XCTAssertEqual(absolute.targetOutputName, "HEADLESS-3")
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
        XCTAssertEqual(AnchorHIDUsage.keyboard("Super"), 0xE3)
        XCTAssertEqual(AnchorHIDUsage.keyboard("PageUp"), 0x4B)
        XCTAssertEqual(AnchorHIDUsage.keyboard("PageDown"), 0x4E)
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

    func testScreenCapabilityAdvertisesDatagramFrames() {
        let screen = AnchorV1Capabilities.descriptor(for: AnchorV1.Capability.screen)
        XCTAssertEqual(screen?.supportsDatagrams, true)
        XCTAssertTrue(screen?.accepts(typeURL: AnchorV1.TypeURL.screenFrame) == true)
    }

    func testDatagramFlowBindingRoundTripAndMismatchRejection() throws {
        let request = try AnchorDatagramFlowBindingCodec.openEnvelope(
            requestID: 5,
            flowID: 7,
            capabilitySessionID: 3,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )
        XCTAssertEqual(request.requestID, 5)
        XCTAssertEqual(request.datagramFlowOpen.flowID, 7)
        XCTAssertEqual(request.datagramFlowOpen.capabilitySessionID, 3)
        XCTAssertEqual(request.datagramFlowOpen.payloadTypeURL, AnchorV1.TypeURL.screenFrame)

        var opened = ANCHDatagramFlowOpened(); opened.flowID = 7
        var reply = ANCHControlEnvelope(); reply.responseTo = 5; reply.datagramFlowOpened = opened
        XCTAssertNoThrow(try AnchorDatagramFlowBindingCodec.validateOpened(
            reply, requestID: 5, flowID: 7
        ))
        XCTAssertThrowsError(try AnchorDatagramFlowBindingCodec.validateOpened(
            reply, requestID: 6, flowID: 7
        )) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .datagramBindingMismatch)
        }
        XCTAssertThrowsError(try AnchorDatagramFlowBindingCodec.openEnvelope(
            requestID: 5,
            flowID: 0,
            capabilitySessionID: 3,
            payloadTypeURL: AnchorV1.TypeURL.screenFrame
        )) {
            XCTAssertEqual($0 as? AnchorFeatureCodecError, .datagramBindingMismatch)
        }
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
        let selection = try ANCHScreenScreenSelectOutput(
            serializedBytes: AnchorScreenCodec.selectOutput("DP-2")
        )
        XCTAssertEqual(selection.outputID, "DP-2")
    }
}
