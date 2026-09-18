import Foundation
import QuartzCore

/// Reassembles H264 frames from fragmented UDP packets.
///
/// UDP packet header (12 bytes, little-endian):
///   [frame_id: u32][fragment_index: u16][fragment_count: u16][frame_size: u32]
class UdpFrameReassembler {

    private static let headerSize = 12
    private static let staleTimeoutSec: Double = 0.1 // 100ms
    private static let keyframeMinIntervalSec: Double = 3.0
    private static let keyframeMinSkippedFrames: UInt32 = 3

    var onFrameComplete: (UInt64, Data) -> Void
    var onFrameLost: () -> Void

    private class FrameAssembly {
        let fragmentCount: Int
        let frameSize: Int
        var fragments: [Data?]
        var receivedCount: Int = 0
        let createdAt: Double

        init(fragmentCount: Int, frameSize: Int) {
            self.fragmentCount = fragmentCount
            self.frameSize = frameSize
            self.fragments = Array(repeating: nil, count: fragmentCount)
            self.createdAt = CACurrentMediaTime()
        }
    }

    private var assemblies: [UInt32: FrameAssembly] = [:]
    private var lastCompletedFrameId: UInt32 = 0
    private var totalFrames: UInt64 = 0
    private var totalLost: UInt64 = 0
    private var lastKeyframeRequestAt: Double = 0

    init(onFrameComplete: @escaping (UInt64, Data) -> Void, onFrameLost: @escaping () -> Void) {
        self.onFrameComplete = onFrameComplete
        self.onFrameLost = onFrameLost
    }

    func onPacket(_ data: Data) {
        guard data.count >= Self.headerSize else { return }

        // Parse header (little-endian)
        let frameId: UInt32 = data.withUnsafeBytes { ptr in
            ptr.load(fromByteOffset: 0, as: UInt32.self).littleEndian
        }
        let fragIndex: UInt16 = data.withUnsafeBytes { ptr in
            ptr.load(fromByteOffset: 4, as: UInt16.self).littleEndian
        }
        let fragCount: UInt16 = data.withUnsafeBytes { ptr in
            ptr.load(fromByteOffset: 6, as: UInt16.self).littleEndian
        }
        let frameSize: UInt32 = data.withUnsafeBytes { ptr in
            ptr.load(fromByteOffset: 8, as: UInt32.self).littleEndian
        }

        guard fragCount > 0, fragIndex < fragCount else { return }
        guard frameId > lastCompletedFrameId || lastCompletedFrameId == 0 else { return } // stale

        let payloadLen = data.count - Self.headerSize
        guard payloadLen > 0 else { return }

        let assembly: FrameAssembly
        if let existing = assemblies[frameId] {
            assembly = existing
        } else {
            let newAssembly = FrameAssembly(fragmentCount: Int(fragCount), frameSize: Int(frameSize))
            assemblies[frameId] = newAssembly
            assembly = newAssembly
        }

        let idx = Int(fragIndex)
        guard idx < assembly.fragments.count else { return }
        guard assembly.fragments[idx] == nil else { return } // duplicate

        // Store fragment payload
        let payload = data.subdata(in: Self.headerSize..<data.count)
        assembly.fragments[idx] = payload
        assembly.receivedCount += 1

        if assembly.receivedCount == assembly.fragmentCount {
            // Frame complete — concatenate fragments
            var frame = Data(capacity: assembly.frameSize)
            for frag in assembly.fragments {
                if let frag = frag {
                    frame.append(frag)
                }
            }

            // Detect skipped frames
            if frameId > lastCompletedFrameId + 1 && lastCompletedFrameId > 0 {
                let skipped = frameId - lastCompletedFrameId - 1
                NSLog("[anchor] [udp] Lost %u frame(s) between %u and %u", skipped, lastCompletedFrameId, frameId)
                totalLost += UInt64(skipped)
                let now = CACurrentMediaTime()
                if skipped >= Self.keyframeMinSkippedFrames &&
                    now - lastKeyframeRequestAt > Self.keyframeMinIntervalSec {
                    lastKeyframeRequestAt = now
                    onFrameLost()
                }
            }

            lastCompletedFrameId = frameId
            totalFrames += 1

            // Purge old assemblies
            assemblies = assemblies.filter { $0.key > frameId }

            onFrameComplete(UInt64(frameId), frame)
        }

        // Purge stale assemblies (older than 100ms)
        let now = CACurrentMediaTime()
        assemblies = assemblies.filter { now - $0.value.createdAt < Self.staleTimeoutSec }
    }

    func stats() -> String {
        "frames=\(totalFrames) lost=\(totalLost) pending=\(assemblies.count)"
    }
}
