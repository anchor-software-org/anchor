import XCTest
@testable import AnchorSDK

final class AnchorSourceTimestampTrackerTests: XCTestCase {
    func testFixedClockSkewIsRemovedFromRelativeDelay() throws {
        var tracker = AnchorSourceTimestampTracker()

        let first = try XCTUnwrap(tracker.observe(
            sourceWallClockUs: 1_000_000,
            receiverWallClockUs: 24_000_000
        ))
        let delayed = try XCTUnwrap(tracker.observe(
            sourceWallClockUs: 2_000_000,
            receiverWallClockUs: 25_012_000
        ))

        XCTAssertEqual(first.rawClockOffsetUs, 23_000_000)
        XCTAssertEqual(first.excessDelayUs, 0)
        XCTAssertEqual(delayed.baselineClockOffsetUs, 23_000_000)
        XCTAssertEqual(delayed.excessDelayUs, 12_000)
    }

    func testNegativeClockOffsetAndNewBestSampleAreSupported() throws {
        var tracker = AnchorSourceTimestampTracker()
        _ = tracker.observe(sourceWallClockUs: 10_000, receiverWallClockUs: 9_000)

        let better = try XCTUnwrap(tracker.observe(
            sourceWallClockUs: 20_000,
            receiverWallClockUs: 18_500
        ))
        let slower = try XCTUnwrap(tracker.observe(
            sourceWallClockUs: 30_000,
            receiverWallClockUs: 28_750
        ))

        XCTAssertEqual(better.baselineClockOffsetUs, -1_500)
        XCTAssertTrue(better.establishedNewBaseline)
        XCTAssertEqual(slower.excessDelayUs, 250)
    }

    func testZeroOverflowAndResetAreHandledExplicitly() throws {
        var tracker = AnchorSourceTimestampTracker()
        XCTAssertNil(tracker.observe(sourceWallClockUs: 0, receiverWallClockUs: 1))
        XCTAssertNil(tracker.observe(
            sourceWallClockUs: UInt64(Int64.max) + 1,
            receiverWallClockUs: 1
        ))

        _ = tracker.observe(sourceWallClockUs: 100, receiverWallClockUs: 150)
        tracker.reset()
        let reset = try XCTUnwrap(tracker.observe(sourceWallClockUs: 200, receiverWallClockUs: 400))
        XCTAssertEqual(reset.baselineClockOffsetUs, 200)
        XCTAssertEqual(reset.excessDelayUs, 0)
        XCTAssertTrue(reset.establishedNewBaseline)
    }

    func testRelativeDelayDoesNotOverflowAcrossSignedOffsetExtremes() throws {
        var tracker = AnchorSourceTimestampTracker()
        let maximum = UInt64(Int64.max)
        _ = tracker.observe(sourceWallClockUs: maximum, receiverWallClockUs: 0)

        let opposite = try XCTUnwrap(tracker.observe(
            sourceWallClockUs: 1,
            receiverWallClockUs: maximum
        ))

        XCTAssertEqual(opposite.rawClockOffsetUs, Int64.max - 1)
        XCTAssertEqual(opposite.baselineClockOffsetUs, -Int64.max)
        XCTAssertEqual(opposite.excessDelayUs, UInt64.max - 2)
    }
}
