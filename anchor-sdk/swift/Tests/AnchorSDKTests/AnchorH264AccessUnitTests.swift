import XCTest
@testable import AnchorSDK

final class AnchorH264AccessUnitTests: XCTestCase {
    func testMixedStartCodesCollectParameterSetsAndEveryVCLNAL() throws {
        let sps = Data([0x67, 0x64, 0x00, 0x1F])
        let pps = Data([0x68, 0xEE, 0x3C, 0x80])
        let firstSlice = Data([0x65, 0x88, 0x84])
        let secondSlice = Data([0x41, 0x9A, 0x22, 0x11])
        let bytes = annexB([
            (4, sps),
            (3, pps),
            (4, firstSlice),
            (3, secondSlice),
        ])

        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(bytes)

        XCTAssertEqual(accessUnit.nalUnits, [sps, pps, firstSlice, secondSlice])
        XCTAssertEqual(accessUnit.sequenceParameterSets, [sps])
        XCTAssertEqual(accessUnit.pictureParameterSets, [pps])
        XCTAssertEqual(accessUnit.vclNALUnits, [firstSlice, secondSlice])
        XCTAssertEqual(accessUnit.sampleNALUnits, [firstSlice, secondSlice])
        XCTAssertTrue(accessUnit.isKeyframe)
    }

    func testAVCCPayloadUsesBigEndianLengthsAndPreservesSliceOrder() throws {
        let first = Data([0x41, 0xAA])
        let second = Data([0x41, 0xBB, 0xCC])
        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(annexB([
            (3, first),
            (4, second),
        ]))

        XCTAssertEqual(Array(accessUnit.avccPayload), [
            0x00, 0x00, 0x00, 0x02, 0x41, 0xAA,
            0x00, 0x00, 0x00, 0x03, 0x41, 0xBB, 0xCC,
        ])
        XCTAssertFalse(accessUnit.isKeyframe)
    }

    func testKeyframeDetectionFindsIDRWhenItIsNotFirstVCLNAL() throws {
        let delimiter = Data([0x09, 0xF0])
        let nonIDR = Data([0x41, 0x01])
        let idr = Data([0x65, 0x02])
        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(annexB([
            (4, delimiter),          // access-unit delimiter
            (3, nonIDR),             // non-IDR slice
            (3, idr),                // IDR slice
        ]))

        XCTAssertTrue(accessUnit.isKeyframe)
        XCTAssertEqual(accessUnit.vclNALUnits.map { $0[0] & 0x1F }, [1, 5])
        XCTAssertEqual(accessUnit.sampleNALUnits, [delimiter, nonIDR, idr])
        XCTAssertEqual(Array(accessUnit.avccPayload), [
            0, 0, 0, 2, 0x09, 0xF0,
            0, 0, 0, 2, 0x41, 0x01,
            0, 0, 0, 2, 0x65, 0x02,
        ])
    }

    func testConfigurationOnlyUnitHasNoVideoSamplePayload() throws {
        let sps1 = Data([0x67, 0x01])
        let sps2 = Data([0x67, 0x02])
        let pps1 = Data([0x68, 0x03])
        let pps2 = Data([0x68, 0x04])
        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(annexB([
            (4, sps1), (3, sps2), (4, pps1), (3, pps2),
        ]))

        XCTAssertEqual(accessUnit.sequenceParameterSets, [sps1, sps2])
        XCTAssertEqual(accessUnit.pictureParameterSets, [pps1, pps2])
        XCTAssertTrue(accessUnit.vclNALUnits.isEmpty)
        XCTAssertTrue(accessUnit.sampleNALUnits.isEmpty)
        XCTAssertTrue(accessUnit.avccPayload.isEmpty)
        XCTAssertFalse(accessUnit.isKeyframe)
    }

    func testAnnexBLeadingAndTrailingZerosAreNotPartOfNALUnits() throws {
        var bytes = Data([0, 0])
        bytes.append(annexB([(4, Data([0x65, 0xAB]))]))
        bytes.append(contentsOf: [0, 0])

        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(bytes)

        XCTAssertEqual(accessUnit.vclNALUnits, [Data([0x65, 0xAB])])
    }

    func testRawSingleNALInputBuildsOneAVCCSample() throws {
        let raw = Data([0x65, 0xAA, 0xBB])
        let accessUnit = try AnchorH264AccessUnit.parseAnnexB(raw)

        XCTAssertEqual(accessUnit.vclNALUnits, [raw])
        XCTAssertEqual(accessUnit.sampleNALUnits, [raw])
        XCTAssertEqual(Array(accessUnit.avccPayload), [0, 0, 0, 3, 0x65, 0xAA, 0xBB])
        XCTAssertTrue(accessUnit.isKeyframe)
    }

    func testRejectsEmptyOrPrefixedInput() {
        assertError(Data(), equals: .emptyInput)
        assertError(Data([0x12, 0, 0, 1, 0x65]), equals: .unexpectedBytesBeforeStartCode)
    }

    func testRejectsEmptyNALUnitsAndInvalidHeaders() {
        assertError(Data([0, 0, 0, 1]), equals: .emptyNALUnit)
        assertError(
            Data([0, 0, 1, 0x65, 0, 0, 0, 1]),
            equals: .emptyNALUnit
        )
        assertError(Data([0, 0, 1, 0x80]), equals: .invalidNALHeader)
        assertError(Data([0, 0, 1, 0x00]), equals: .emptyNALUnit)
        assertError(Data([0x00]), equals: .invalidNALHeader)
    }

    private func annexB(_ units: [(startCodeLength: Int, nal: Data)]) -> Data {
        var bytes = Data()
        for unit in units {
            bytes.append(contentsOf: unit.startCodeLength == 4
                ? [0, 0, 0, 1]
                : [0, 0, 1])
            bytes.append(unit.nal)
        }
        return bytes
    }

    private func assertError(
        _ bytes: Data,
        equals expected: AnchorH264AccessUnitError,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertThrowsError(
            try AnchorH264AccessUnit.parseAnnexB(bytes),
            file: file,
            line: line
        ) {
            XCTAssertEqual($0 as? AnchorH264AccessUnitError, expected, file: file, line: line)
        }
    }
}
