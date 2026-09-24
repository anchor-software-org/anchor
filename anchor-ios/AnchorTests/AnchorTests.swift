import XCTest
import AnchorSDK
import CryptoKit
@testable import Anchor

final class AnchorTests: XCTestCase {

    func testDeviceDisplayDimensionsPreserveAsymmetricNativePixels() throws {
        let dimensions = NetworkPlugin.validatedDisplayDimensions(width: 1366, height: 1024)
        XCTAssertEqual(dimensions.width, 1366)
        XCTAssertEqual(dimensions.height, 1024)

        let state = NetworkPlugin.makeDeviceState(
            deviceName: "Test device",
            batteryPercent: 63,
            charging: true,
            displayWidth: dimensions.width,
            displayHeight: dimensions.height
        )
        let decoded = try ANCHDeviceDeviceState(serializedBytes: state.serializedData())
        XCTAssertEqual(decoded.deviceName, "Test device")
        XCTAssertEqual(decoded.batteryPercent, 63)
        XCTAssertTrue(decoded.charging)
        XCTAssertEqual(decoded.displayWidth, 1366)
        XCTAssertEqual(decoded.displayHeight, 1024)
    }

    func testBatteryPercentageHandlesUnknownBoundsAndRounding() {
        XCTAssertNil(NetworkPlugin.validatedBatteryPercent(level: -1))
        XCTAssertNil(NetworkPlugin.validatedBatteryPercent(level: .nan))
        XCTAssertNil(NetworkPlugin.validatedBatteryPercent(level: .infinity))
        XCTAssertEqual(NetworkPlugin.validatedBatteryPercent(level: 0), 0)
        XCTAssertEqual(NetworkPlugin.validatedBatteryPercent(level: 0.25), 25)
        XCTAssertEqual(NetworkPlugin.validatedBatteryPercent(level: 0.999), 100)
        XCTAssertEqual(NetworkPlugin.validatedBatteryPercent(level: 1), 100)
        XCTAssertEqual(NetworkPlugin.validatedBatteryPercent(level: 1.5), 100)

        let clamped = NetworkPlugin.makeDeviceState(
            deviceName: "Test device",
            batteryPercent: 101,
            charging: false,
            displayWidth: 1024,
            displayHeight: 1366
        )
        XCTAssertEqual(clamped.batteryPercent, 100)
        XCTAssertFalse(clamped.charging)
    }

    func testDeviceDisplayDimensionsRejectUnknownFractionalAndOverflowValues() {
        let invalid: [(Double, Double)] = [
            (0, 1024),
            (1366, 0),
            (-1, 1024),
            (1366.5, 1024),
            (.infinity, 1024),
            (Double(UInt32.max) + 1, 1024),
        ]

        for (width, height) in invalid {
            let dimensions = NetworkPlugin.validatedDisplayDimensions(width: width, height: height)
            XCTAssertEqual(dimensions.width, 0)
            XCTAssertEqual(dimensions.height, 0)
        }

        let maximum = NetworkPlugin.validatedDisplayDimensions(
            width: Double(UInt32.max),
            height: 1
        )
        XCTAssertEqual(maximum.width, UInt32.max)
        XCTAssertEqual(maximum.height, 1)
    }

    func testNotificationHistoryDeduplicatesAndKeepsNewestWithinCapacity() {
        let now = Date()
        let first = DesktopNotification(
            id: "one", applicationName: "Mail", title: "First", body: "Body", postedAt: now
        )
        let second = DesktopNotification(
            id: "two", applicationName: "Chat", title: "Second", body: "Body", postedAt: now
        )
        let updatedFirst = DesktopNotification(
            id: "one", applicationName: "Mail", title: "Updated", body: "New body", postedAt: now
        )
        let third = DesktopNotification(
            id: "three", applicationName: "Calendar", title: "Third", body: "Body", postedAt: now
        )

        var history = NotificationHistory(capacity: 2)
        history.insert(first)
        history.insert(second)
        history.insert(updatedFirst)

        XCTAssertEqual(history.entries.map(\.id), ["one", "two"])
        XCTAssertEqual(history.entries.first?.title, "Updated")

        history.insert(third)
        XCTAssertEqual(history.entries.map(\.id), ["three", "one"])
    }

    func testDesktopNotificationRecordDecodesWithoutCreatingAnOutboundPath() throws {
        var posted = ANCHNotificationsNotificationPosted()
        posted.notificationID = "desktop-42"
        posted.applicationName = "Mail"
        posted.title = "New message"
        posted.body = "Hello"
        posted.postedAtUnixMs = 1_700_000_000_123

        let decoded = try XCTUnwrap(NetworkPlugin.decodeDesktopNotification(posted.serializedData()))

        XCTAssertEqual(decoded.id, "desktop-42")
        XCTAssertEqual(decoded.applicationName, "Mail")
        XCTAssertEqual(decoded.title, "New message")
        XCTAssertEqual(decoded.body, "Hello")
        XCTAssertEqual(decoded.postedAt.timeIntervalSince1970, 1_700_000_000.123, accuracy: 0.001)
    }

    func testCommandListAndResultRecordsDecode() throws {
        var definition = ANCHCommandsCommandDefinition()
        definition.id = "lock"
        definition.name = "Lock screen"
        definition.description_p = "Lock the desktop session"
        var list = ANCHCommandsCommandList()
        list.commands = [definition]

        let decodedList = try NetworkPlugin.decodeCommandList(list.serializedData())
        XCTAssertEqual(decodedList, [RemoteCommandDefinition(
            id: "lock",
            name: "Lock screen",
            description: "Lock the desktop session",
            detached: false
        )])

        var result = ANCHCommandsCommandResult()
        result.commandID = "lock"
        result.executionID = "exec-1"
        result.status = "done"
        result.exitCode = 0
        let decodedResult = try XCTUnwrap(NetworkPlugin.decodeCommandResult(result.serializedData()))
        XCTAssertEqual(decodedResult.status, .done)
        XCTAssertEqual(decodedResult.commandID, "lock")
        XCTAssertEqual(decodedResult.executionID, "exec-1")
    }

    func testTrustedStoreFingerprint() {
        // Simple test: SHA-256 of known data should be deterministic
        let data = Data("test certificate data".utf8)
        let fingerprint = TrustedStore.fingerprint(data)
        XCTAssertFalse(fingerprint.isEmpty)
        XCTAssertTrue(fingerprint.contains(":"))

        // Same input should produce same fingerprint
        let fingerprint2 = TrustedStore.fingerprint(data)
        XCTAssertEqual(fingerprint, fingerprint2)
    }

    func testConnectionStateDefaults() {
        let state = ConnectionState()
        XCTAssertEqual(state.status, .disconnected)
        XCTAssertTrue(state.host.isEmpty)
        XCTAssertEqual(state.port, 0)
        XCTAssertNil(state.error)
    }

    func testPairingStateIdle() {
        let state: PairingState = .idle
        if case .idle = state {
            // pass
        } else {
            XCTFail("Expected idle state")
        }
    }

    func testStreamDimensionsFallBackToSelectedOutputWhenStatusIsZeroSized() {
        let outputs = [
            VideoPlugin.StreamOutput(id: "0", name: "Laptop", width: 1920, height: 1080),
            VideoPlugin.StreamOutput(id: "1", name: "External", width: 2560, height: 1440),
        ]

        let dimensions = VideoPlugin.resolveStreamDimensions(
            reportedWidth: 0,
            reportedHeight: 0,
            outputID: "1",
            selectedOutputID: "0",
            outputs: outputs,
            currentWidth: 0,
            currentHeight: 0
        )

        XCTAssertEqual(dimensions.width, 2560)
        XCTAssertEqual(dimensions.height, 1440)
    }

    func testStreamDimensionsPreferReportedSizeAndPreserveKnownSize() {
        let reported = VideoPlugin.resolveStreamDimensions(
            reportedWidth: 1600,
            reportedHeight: 900,
            outputID: "",
            selectedOutputID: "",
            outputs: [],
            currentWidth: 1920,
            currentHeight: 1080
        )
        XCTAssertEqual(reported.width, 1600)
        XCTAssertEqual(reported.height, 900)

        let preserved = VideoPlugin.resolveStreamDimensions(
            reportedWidth: 0,
            reportedHeight: 0,
            outputID: "missing",
            selectedOutputID: "missing",
            outputs: [],
            currentWidth: 1920,
            currentHeight: 1080
        )
        XCTAssertEqual(preserved.width, 1920)
        XCTAssertEqual(preserved.height, 1080)
    }

    func testOutputDeletionFallsBackToFirstRemainingOutput() {
        let outputs = [
            VideoPlugin.StreamOutput(id: "0", name: "Laptop", width: 1920, height: 1080),
            VideoPlugin.StreamOutput(id: "2", name: "External", width: 2560, height: 1440),
        ]

        XCTAssertEqual(
            VideoPlugin.resolveSelectedOutputID(current: "1", outputs: outputs),
            "0"
        )
        XCTAssertEqual(
            VideoPlugin.resolveSelectedOutputID(current: "2", outputs: outputs),
            "2"
        )
        XCTAssertEqual(
            VideoPlugin.resolveSelectedOutputID(current: "2", outputs: []),
            ""
        )
    }

    func testOrderedAsyncQueuePreservesButtonEventSubmissionOrder() async {
        let queue = OrderedAsyncQueue()
        let recorded = EventRecorder()

        queue.enqueue {
            try? await Task.sleep(for: .milliseconds(30))
            await recorded.append("down")
        }
        queue.enqueue {
            await recorded.append("up")
        }

        await queue.drain()
        let events = await recorded.events
        XCTAssertEqual(events, ["down", "up"])
    }

    func testVideoFrameQueuePreservesEntireReferenceFrameBurstInOrder() {
        var queue = OrderedFrameQueue<Int>()
        for frameID in 0..<32 {
            queue.append(frameID)
        }

        var drained: [Int] = []
        while let frameID = queue.popFirst() {
            drained.append(frameID)
        }

        XCTAssertEqual(drained, Array(0..<32))
        XCTAssertTrue(queue.isEmpty)
    }

    func testVideoFrameQueueCompactsWithoutChangingFIFOOrder() {
        var queue = OrderedFrameQueue<Int>()
        for frameID in 0..<256 {
            queue.append(frameID)
        }
        for expected in 0..<192 {
            XCTAssertEqual(queue.first, expected)
            XCTAssertEqual(queue.popFirst(), expected)
        }
        for frameID in 256..<384 {
            queue.append(frameID)
        }

        var drained: [Int] = []
        while let frameID = queue.popFirst() {
            drained.append(frameID)
        }
        XCTAssertEqual(drained, Array(192..<384))
        XCTAssertEqual(queue.count, 0)
    }

    func testTouchpadAbsoluteCoordinatesMapAndClampSurface() throws {
        let center = try XCTUnwrap(TouchpadCoordinateMapper.absolute(
            x: 500, y: 250, width: 1_000, height: 500
        ))
        XCTAssertEqual(center.x, 0.5, accuracy: 0.000_001)
        XCTAssertEqual(center.y, 0.5, accuracy: 0.000_001)

        let clamped = try XCTUnwrap(TouchpadCoordinateMapper.absolute(
            x: -20, y: 600, width: 1_000, height: 500
        ))
        XCTAssertEqual(clamped.x, 0, accuracy: 0.000_001)
        XCTAssertEqual(clamped.y, 1, accuracy: 0.000_001)
        XCTAssertNil(TouchpadCoordinateMapper.absolute(x: 1, y: 1, width: 0, height: 500))
    }

    func testPencilHoverRelativeMotionUsesConfiguredSensitivity() {
        let delta = TouchpadCoordinateMapper.relative(
            from: CGPoint(x: 20, y: 30),
            to: CGPoint(x: 26, y: 22),
            sensitivity: 1.5
        )

        XCTAssertEqual(delta.dx, 9)
        XCTAssertEqual(delta.dy, -12)
    }

    func testDrawerSwipeRequiresDeliberateHorizontalEdgeGesture() {
        XCTAssertTrue(DrawerSwipePolicy.shouldOpen(
            start: CGPoint(x: 18, y: 300),
            translation: CGSize(width: 150, height: 30)
        ))
        XCTAssertFalse(DrawerSwipePolicy.shouldOpen(
            start: CGPoint(x: 80, y: 300),
            translation: CGSize(width: 180, height: 10)
        ))
        XCTAssertFalse(DrawerSwipePolicy.shouldOpen(
            start: CGPoint(x: 18, y: 300),
            translation: CGSize(width: 90, height: 5)
        ))
        XCTAssertFalse(DrawerSwipePolicy.shouldOpen(
            start: CGPoint(x: 18, y: 300),
            translation: CGSize(width: 140, height: 120)
        ))
        XCTAssertFalse(DrawerSwipePolicy.shouldOpen(
            start: CGPoint(x: 18, y: 300),
            translation: CGSize(width: -160, height: 0)
        ))
    }

    func testDecoderRecoveryRejectsPredictiveFramesUntilIDR() {
        var gate = KeyframeRecoveryGate()

        XCTAssertFalse(gate.shouldSubmit(isKeyframe: false))
        XCTAssertFalse(gate.shouldSubmit(isKeyframe: false))
        XCTAssertTrue(gate.shouldSubmit(isKeyframe: true))
        XCTAssertTrue(gate.shouldSubmit(isKeyframe: false))

        gate.reset()
        XCTAssertFalse(gate.shouldSubmit(isKeyframe: false))
        XCTAssertTrue(gate.shouldSubmit(isKeyframe: true))
    }

    func testFrameSequenceTrackerDetectsSkippedAndReorderedAccessUnits() {
        var tracker = FrameSequenceTracker()

        XCTAssertTrue(tracker.observe(41))
        XCTAssertTrue(tracker.observe(42))
        XCTAssertFalse(tracker.observe(44))
        XCTAssertFalse(tracker.observe(43))

        tracker.reset()
        XCTAssertTrue(tracker.observe(UInt64.max))
        XCTAssertTrue(tracker.observe(0))
    }

    func testFileReceiveAssemblerReassemblesFragmentedContentAndVerifiesIntegrity() throws {
        let payload = Data("fragmented file payload".utf8)
        let location = try makeTemporaryFileLocation()
        let assembler = try FileReceiveAssembler(
            temporaryURL: location,
            expectedBytes: UInt64(payload.count),
            expectedHash: Data(SHA256.hash(data: payload))
        )

        try assembler.append(payload.prefix(3))
        try assembler.append(payload.dropFirst(3).prefix(7))
        try assembler.append(payload.dropFirst(10))
        try assembler.finish()

        XCTAssertEqual(assembler.receivedBytes, UInt64(payload.count))
        XCTAssertEqual(try Data(contentsOf: location), payload)
    }

    func testFileReceiveAssemblerRejectsOverflowBeforeWritingPastOfferLength() throws {
        let payload = Data("1234".utf8)
        let location = try makeTemporaryFileLocation()
        let assembler = try FileReceiveAssembler(
            temporaryURL: location,
            expectedBytes: UInt64(payload.count),
            expectedHash: Data(SHA256.hash(data: payload))
        )

        try assembler.append(payload.prefix(3))
        XCTAssertThrowsError(try assembler.append(Data("45".utf8))) {
            XCTAssertEqual($0 as? FileTransferError, .unexpectedLength)
        }
        XCTAssertEqual(assembler.receivedBytes, 3)
        XCTAssertEqual(try Data(contentsOf: location), payload.prefix(3))
    }

    func testFileReceiveAssemblerRejectsTruncationAndChecksumMismatch() throws {
        let payload = Data("complete".utf8)

        let truncatedLocation = try makeTemporaryFileLocation()
        let truncated = try FileReceiveAssembler(
            temporaryURL: truncatedLocation,
            expectedBytes: UInt64(payload.count),
            expectedHash: Data(SHA256.hash(data: payload))
        )
        try truncated.append(payload.dropLast())
        XCTAssertThrowsError(try truncated.finish()) {
            XCTAssertEqual($0 as? FileTransferError, .unexpectedLength)
        }

        let corruptLocation = try makeTemporaryFileLocation()
        let corrupt = try FileReceiveAssembler(
            temporaryURL: corruptLocation,
            expectedBytes: UInt64(payload.count),
            expectedHash: Data(repeating: 0, count: 32)
        )
        try corrupt.append(payload)
        XCTAssertThrowsError(try corrupt.finish()) {
            XCTAssertEqual($0 as? FileTransferError, .checksumMismatch)
        }
    }

    func testFileTransferHistoryIsBoundedNewestFirstAndTracksTerminalState() {
        let broker = MessageBroker()
        let plugin = FilesPlugin(broker: broker)
        plugin.start()

        for index in 0..<52 {
            broker.send(AnchorEvent(target: .service("files"), message: .files(.began(
                FileTransferEntry(
                    id: "file-\(index)", name: "file-\(index).txt", direction: .received,
                    totalBytes: 10, transferredBytes: 0, status: .transferring,
                    localURL: nil, startedAt: Date(timeIntervalSince1970: TimeInterval(index))
                )
            ))))
        }
        broker.send(AnchorEvent(target: .service("files"), message: .files(
            .progress(id: "file-51", transferredBytes: 99)
        )))
        let finalURL = URL(fileURLWithPath: "/tmp/file-51.txt")
        broker.send(AnchorEvent(target: .service("files"), message: .files(
            .completed(id: "file-51", localURL: finalURL)
        )))

        let delivered = expectation(description: "files events delivered on main queue")
        DispatchQueue.main.async {
            XCTAssertEqual(plugin.transfers.count, 50)
            XCTAssertEqual(plugin.transfers.first?.id, "file-51")
            XCTAssertEqual(plugin.transfers.last?.id, "file-2")
            XCTAssertEqual(plugin.transfers.first?.transferredBytes, 10)
            XCTAssertEqual(plugin.transfers.first?.status, .completed)
            XCTAssertEqual(plugin.transfers.first?.localURL, finalURL)

            plugin.clearCompleted()
            XCTAssertEqual(plugin.transfers.count, 49)
            XCTAssertTrue(plugin.transfers.allSatisfy { $0.status == .transferring })
            delivered.fulfill()
        }
        wait(for: [delivered], timeout: 2)
    }

    func testFileTransferCoordinatorRequiresMatchingDecisionAndStreamOpenedBinding() async throws {
        let coordinator = FileTransferCoordinator { _ in }
        let transferID = Data(repeating: 7, count: AnchorFilesCodec.transferIDByteCount)

        await coordinator.recordDecision(AnchorFileDecision(transferID: transferID, accepted: true))
        try await coordinator.waitForDecision(transferID: transferID, timeoutNanoseconds: 1_000_000)

        await coordinator.recordDecision(AnchorFileDecision(transferID: transferID, accepted: false))
        do {
            try await coordinator.waitForDecision(transferID: transferID, timeoutNanoseconds: 1_000_000)
            XCTFail("Expected a rejected file decision")
        } catch {
            XCTAssertEqual(error as? FileTransferError, .rejected)
        }

        await coordinator.recordOpened(responseTo: 41, streamID: 12)
        try await coordinator.waitForOpened(requestID: 41, streamID: 12, timeoutNanoseconds: 1_000_000)

        await coordinator.recordOpened(responseTo: 42, streamID: 99)
        do {
            try await coordinator.waitForOpened(requestID: 42, streamID: 12, timeoutNanoseconds: 1_000_000)
            XCTFail("Expected a mismatched stream binding")
        } catch {
            XCTAssertEqual(error as? AnchorFeatureCodecError, .streamBindingMismatch)
        }
    }

    func testFileTransferCoordinatorRejectsInvalidAndDuplicateLiveOffers() async {
        let coordinator = FileTransferCoordinator { _ in }
        let transferID = Data(repeating: 8, count: AnchorFilesCodec.transferIDByteCount)
        let hash = Data(repeating: 9, count: AnchorFilesCodec.sha256ByteCount)
        let valid = AnchorFileOffer(
            transferID: transferID, filename: "safe.txt", mimeType: "text/plain",
            byteLength: 4, sha256: hash
        )
        let empty = AnchorFileOffer(
            transferID: Data(repeating: 1, count: 16), filename: "empty.txt", mimeType: "text/plain",
            byteLength: 0, sha256: hash
        )
        let oversized = AnchorFileOffer(
            transferID: Data(repeating: 2, count: 16), filename: "large.bin", mimeType: "application/octet-stream",
            byteLength: FileTransferCoordinator.maximumFileBytes + 1, sha256: hash
        )

        let acceptedValid = await coordinator.accept(valid)
        let acceptedDuplicate = await coordinator.accept(valid)
        let acceptedEmpty = await coordinator.accept(empty)
        let acceptedOversized = await coordinator.accept(oversized)
        XCTAssertTrue(acceptedValid)
        XCTAssertFalse(acceptedDuplicate)
        XCTAssertFalse(acceptedEmpty)
        XCTAssertFalse(acceptedOversized)
    }

    func testFileTransferCoordinatorReceivesStreamToVerifiedDestinationEndToEnd() async throws {
        let payload = Data("streamed over quic".utf8)
        let transferID = Data(UUID().uuidString.utf8.prefix(16))
        let filename = "anchor-test-\(UUID().uuidString).txt"
        let offer = AnchorFileOffer(
            transferID: transferID,
            filename: filename,
            mimeType: "text/plain",
            byteLength: UInt64(payload.count),
            sha256: Data(SHA256.hash(data: payload))
        )
        let recorder = FileMessageRecorder()
        let coordinator = FileTransferCoordinator { recorder.append($0) }
        let stream = StubFileStream(
            streamIdentifier: 27,
            receiveChunks: [payload.prefix(5), payload.dropFirst(5)]
        )

        let accepted = await coordinator.accept(offer)
        XCTAssertTrue(accepted)
        await coordinator.register(stream)
        await coordinator.complete(AnchorFileComplete(transferID: transferID, sha256: offer.sha256))
        await coordinator.bind(AnchorFileContentStart(transferID: transferID, quicStreamID: 27))

        let destination = try await waitForCompletedFile(in: recorder)
        addTeardownBlock { try? FileManager.default.removeItem(at: destination) }
        XCTAssertEqual(destination.lastPathComponent, filename)
        XCTAssertEqual(try Data(contentsOf: destination), payload)
        XCTAssertTrue(recorder.messages.contains { message in
            if case .progress(let id, let bytes) = message {
                return id == FileTransferCoordinator.identifier(transferID) && bytes == UInt64(payload.count)
            }
            return false
        })
    }

    func testFileTransferCoordinatorFailsClosedOnTruncatedStream() async throws {
        let payload = Data("expected".utf8)
        let transferID = Data(repeating: 3, count: 16)
        let recorder = FileMessageRecorder()
        let coordinator = FileTransferCoordinator { recorder.append($0) }
        let offer = AnchorFileOffer(
            transferID: transferID, filename: "truncated.txt", mimeType: "text/plain",
            byteLength: UInt64(payload.count), sha256: Data(SHA256.hash(data: payload))
        )

        let accepted = await coordinator.accept(offer)
        XCTAssertTrue(accepted)
        await coordinator.bind(AnchorFileContentStart(transferID: transferID, quicStreamID: 31))
        await coordinator.register(StubFileStream(
            streamIdentifier: 31, receiveChunks: [payload.prefix(2), Data()]
        ))

        let failure = try await waitForFileFailure(in: recorder)
        XCTAssertEqual(failure, FileTransferError.unexpectedLength.localizedDescription)
    }

    private func waitForCompletedFile(in recorder: FileMessageRecorder) async throws -> URL {
        for _ in 0..<200 {
            if let result = recorder.completedURL { return result }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw FileTestError.timedOut
    }

    private func waitForFileFailure(in recorder: FileMessageRecorder) async throws -> String {
        for _ in 0..<200 {
            if let result = recorder.failure { return result }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw FileTestError.timedOut
    }

    private func makeTemporaryFileLocation() throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("anchor-file-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory.appendingPathComponent("payload.part")
    }
}

private actor EventRecorder {
    private(set) var events: [String] = []

    func append(_ event: String) {
        events.append(event)
    }
}

private enum FileTestError: Error {
    case timedOut
}

private final class FileMessageRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var storedMessages: [FileWireMessage] = []

    func append(_ message: FileWireMessage) {
        lock.lock()
        storedMessages.append(message)
        lock.unlock()
    }

    var messages: [FileWireMessage] {
        lock.lock()
        defer { lock.unlock() }
        return storedMessages
    }

    var completedURL: URL? {
        messages.compactMap { message in
            if case .completed(_, let url) = message { return url }
            return nil
        }.last ?? nil
    }

    var failure: String? {
        messages.compactMap { message in
            if case .failed(_, let reason) = message { return reason }
            return nil
        }.last
    }
}

private final class StubFileStream: AnchorReliableStream, @unchecked Sendable {
    let streamIdentifier: UInt64
    private let lock = NSLock()
    private var receiveChunks: [Data]

    init(streamIdentifier: UInt64, receiveChunks: [Data]) {
        self.streamIdentifier = streamIdentifier
        self.receiveChunks = receiveChunks
    }

    func receive(maximumLength: Int) async throws -> Data {
        let next = lock.withLock {
            receiveChunks.isEmpty ? Data() : receiveChunks.removeFirst()
        }
        XCTAssertLessThanOrEqual(next.count, maximumLength)
        return next
    }

    func send(_ bytes: Data) async throws {}
    func finish() async throws {}
}
