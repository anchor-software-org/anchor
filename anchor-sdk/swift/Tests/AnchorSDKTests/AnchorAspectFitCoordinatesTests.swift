import XCTest
@testable import AnchorSDK

final class AnchorAspectFitCoordinatesTests: XCTestCase {
    func testLandscapeStreamInPortraitViewMapsContentCornersAndCenter() {
        // 16:9 content is letterboxed vertically inside this 4:3 view.
        assertPoint(map(x: 0, y: 37.5), x: 0, y: 0)
        assertPoint(map(x: 400, y: 262.5), x: 1, y: 1)
        assertPoint(map(x: 200, y: 150), x: 0.5, y: 0.5)
    }

    func testLetterboxBarsClampToNearestStreamEdge() {
        assertPoint(map(x: 200, y: 0), x: 0.5, y: 0)
        assertPoint(map(x: 200, y: 300), x: 0.5, y: 1)
    }

    func testPortraitStreamInLandscapeViewMapsPillarboxAndCenter() {
        let left = AnchorAspectFitCoordinates.map(
            x: 375, y: 0,
            viewWidth: 1_000, viewHeight: 500,
            streamWidth: 1, streamHeight: 2
        )
        let center = AnchorAspectFitCoordinates.map(
            x: 500, y: 250,
            viewWidth: 1_000, viewHeight: 500,
            streamWidth: 1, streamHeight: 2
        )
        let rightBar = AnchorAspectFitCoordinates.map(
            x: 1_000, y: 250,
            viewWidth: 1_000, viewHeight: 500,
            streamWidth: 1, streamHeight: 2
        )
        assertPoint(left, x: 0, y: 0)
        assertPoint(center, x: 0.5, y: 0.5)
        assertPoint(rightBar, x: 1, y: 0.5)
    }

    func testUnknownStreamSizeFallsBackToFullViewNormalization() {
        let point = AnchorAspectFitCoordinates.map(
            x: 100, y: 75,
            viewWidth: 400, viewHeight: 300,
            streamWidth: 0, streamHeight: 0
        )
        assertPoint(point, x: 0.25, y: 0.25)
    }

    func testStrictMappingRejectsLetterboxBarsAndAcceptsVideoEdges() {
        XCTAssertNil(AnchorAspectFitCoordinates.mapIfInside(
            x: 200, y: 20,
            viewWidth: 400, viewHeight: 300,
            streamWidth: 1_600, streamHeight: 900
        ))
        let topEdge = AnchorAspectFitCoordinates.mapIfInside(
            x: 200, y: 37.5,
            viewWidth: 400, viewHeight: 300,
            streamWidth: 1_600, streamHeight: 900
        )
        XCTAssertNotNil(topEdge)
        assertPoint(topEdge!, x: 0.5, y: 0)
    }

    func testStrictMappingRejectsInputUntilStreamSizeIsKnown() {
        XCTAssertNil(AnchorAspectFitCoordinates.mapIfInside(
            x: 200, y: 150,
            viewWidth: 400, viewHeight: 300,
            streamWidth: 0, streamHeight: 0
        ))
    }

    private func map(x: Double, y: Double) -> AnchorNormalizedPoint {
        AnchorAspectFitCoordinates.map(
            x: x, y: y,
            viewWidth: 400, viewHeight: 300,
            streamWidth: 1_600, streamHeight: 900
        )
    }

    private func assertPoint(
        _ point: AnchorNormalizedPoint,
        x: Double,
        y: Double,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertEqual(point.x, x, accuracy: 0.000_001, file: file, line: line)
        XCTAssertEqual(point.y, y, accuracy: 0.000_001, file: file, line: line)
    }
}
