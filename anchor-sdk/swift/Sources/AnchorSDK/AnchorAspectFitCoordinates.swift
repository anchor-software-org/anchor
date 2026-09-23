import Foundation

public struct AnchorNormalizedPoint: Equatable {
    public let x: Double
    public let y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }
}

/// Pure aspect-fit coordinate conversion shared by streamed pointer and draw
/// input. Points in letterbox/pillarbox bars clamp to the nearest video edge.
public enum AnchorAspectFitCoordinates {
    public static func map(
        x: Double,
        y: Double,
        viewWidth: Double,
        viewHeight: Double,
        streamWidth: Double,
        streamHeight: Double
    ) -> AnchorNormalizedPoint {
        guard viewWidth > 0, viewHeight > 0,
              viewWidth.isFinite, viewHeight.isFinite else {
            return AnchorNormalizedPoint(x: 0, y: 0)
        }

        let contentWidth: Double
        let contentHeight: Double
        let offsetX: Double
        let offsetY: Double
        if streamWidth > 0, streamHeight > 0,
           streamWidth.isFinite, streamHeight.isFinite {
            let streamAspect = streamWidth / streamHeight
            let viewAspect = viewWidth / viewHeight
            if viewAspect > streamAspect {
                contentHeight = viewHeight
                contentWidth = viewHeight * streamAspect
                offsetX = (viewWidth - contentWidth) / 2
                offsetY = 0
            } else {
                contentWidth = viewWidth
                contentHeight = viewWidth / streamAspect
                offsetX = 0
                offsetY = (viewHeight - contentHeight) / 2
            }
        } else {
            contentWidth = viewWidth
            contentHeight = viewHeight
            offsetX = 0
            offsetY = 0
        }

        return AnchorNormalizedPoint(
            x: clamp((x - offsetX) / contentWidth),
            y: clamp((y - offsetY) / contentHeight)
        )
    }

    private static func clamp(_ value: Double) -> Double {
        guard value.isFinite else { return 0 }
        return min(1, max(0, value))
    }
}
