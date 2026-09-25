import Foundation

/// Interprets a sender wall-clock timestamp without pretending the sender and
/// receiver clocks are synchronized.
///
/// The first/best observed offset contains both clock skew and the minimum
/// pipeline delay seen during this run. Subtracting that baseline cannot yield
/// absolute one-way latency, but it does expose latency growth and jitter above
/// the best observed frame even when the clocks differ by many seconds.
public struct AnchorSourceTimestampTracker: Sendable {
    public struct Observation: Equatable, Sendable {
        /// `receiverWallClockUs - sourceWallClockUs`. This is not latency.
        public let rawClockOffsetUs: Int64
        /// Smallest raw offset observed since initialization/reset.
        public let baselineClockOffsetUs: Int64
        /// Delay above the best observed offset. This is relative, not absolute.
        public let excessDelayUs: UInt64
        public let establishedNewBaseline: Bool
    }

    private var baselineClockOffsetUs: Int64?

    public init() {}

    public mutating func observe(
        sourceWallClockUs: UInt64,
        receiverWallClockUs: UInt64
    ) -> Observation? {
        guard sourceWallClockUs > 0,
              sourceWallClockUs <= UInt64(Int64.max),
              receiverWallClockUs <= UInt64(Int64.max) else {
            return nil
        }

        let rawOffset = Int64(receiverWallClockUs) - Int64(sourceWallClockUs)
        let isNewBaseline = baselineClockOffsetUs.map { rawOffset < $0 } ?? true
        if isNewBaseline {
            baselineClockOffsetUs = rawOffset
        }
        guard let baselineClockOffsetUs else { return nil }
        // `rawOffset >= baseline`, but their mathematical difference can be
        // larger than Int64.max when clocks sit on opposite signed extremes.
        // Two's-complement subtraction preserves that non-negative distance
        // across the full accepted UInt64 range without a signed overflow.
        let excessDelayUs = UInt64(bitPattern: rawOffset)
            &- UInt64(bitPattern: baselineClockOffsetUs)
        return Observation(
            rawClockOffsetUs: rawOffset,
            baselineClockOffsetUs: baselineClockOffsetUs,
            excessDelayUs: excessDelayUs,
            establishedNewBaseline: isNewBaseline
        )
    }

    public mutating func reset() {
        baselineClockOffsetUs = nil
    }
}
