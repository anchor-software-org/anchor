import Foundation
import VideoToolbox
import AVFoundation
import CoreMedia
import Combine
import AnchorSDK

class VideoPlugin: Plugin, ObservableObject {
    private static let frameQueueCapacity = 3
    private static let frameArrivalGapWarnNs: UInt64 = 40_000_000
    private static let queueWaitWarnNs: UInt64 = 40_000_000

    let pluginId = "video"

    struct PreviewState {
        var isReceiving = false
        var fps: Int = 0
        var streamWidth: Int = 0
        var streamHeight: Int = 0
    }

    @Published var isReceiving = false
    @Published var fps: Int = 0
    @Published var streamWidth: Int = 0
    @Published var streamHeight: Int = 0

    var displayLayer: AVSampleBufferDisplayLayer?

    private let broker: MessageBroker

    private var formatDescription: CMVideoFormatDescription?
    private var sps: Data?
    private var pps: Data?
    private struct QueuedFrame {
        let data: Data
        let frameId: UInt64
        let recvNs: UInt64
    }
    private let decodeQueue = DispatchQueue(label: "anchor.video.decode", qos: .userInitiated)
    private let pendingFramesLock = NSLock()
    private var pendingFrames: [QueuedFrame] = []
    private var decodeLoopScheduled = false
    private var lastFrameArrivalNs: UInt64 = 0
    private var maxFrameArrivalGapMsThisSecond: Double = 0
    private var maxPendingFramesThisSecond = 0

    private var frameCount = 0
    private var lastFpsTime = CACurrentMediaTime()
    private var queueDropsThisSecond = 0
    private var queueWaitSumMs: Double = 0
    private var queueWaitMaxMs: Double = 0
    private var queueWaitSamples = 0
    private var sourceAgeSumMs: Double = 0
    private var sourceAgeMaxMs: Double = 0
    private var sourceAgeSamples = 0

    init(broker: MessageBroker, previewState: PreviewState = .init()) {
        self.broker = broker
        self.isReceiving = previewState.isReceiving
        self.fps = previewState.fps
        self.streamWidth = previewState.streamWidth
        self.streamHeight = previewState.streamHeight
    }

    func start() {}

    func stop() {
        pendingFramesLock.lock()
        pendingFrames.removeAll()
        decodeLoopScheduled = false
        lastFrameArrivalNs = 0
        maxFrameArrivalGapMsThisSecond = 0
        maxPendingFramesThisSecond = 0
        queueDropsThisSecond = 0
        sourceAgeSumMs = 0
        sourceAgeMaxMs = 0
        sourceAgeSamples = 0
        pendingFramesLock.unlock()
        queueWaitSumMs = 0
        queueWaitMaxMs = 0
        queueWaitSamples = 0
        formatDescription = nil
        sps = nil
        pps = nil
        DispatchQueue.main.async {
            self.isReceiving = false
            self.fps = 0
        }
    }

    func handleStreamInfo(_ payload: String) {
        guard let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        let width = json["width"] as? Int ?? 0
        let height = json["height"] as? Int ?? 0
        DispatchQueue.main.async {
            self.streamWidth = width
            self.streamHeight = height
        }
        NSLog("[anchor] Stream info: \(width)x\(height)")
        // Don't request keyframe here — desktop already sends IDR after encoder init.
    }

    func requestKeyframe() {
        broker.send(AnchorEvent(target: .device, message: .json(#"{"plugin_id":"wayland","command":"request_keyframe"}"#)))
    }

    // MARK: - Feed frame (called from NetworkPlugin's H264 read thread)

    func feedFrame(_ data: Data, frameId: UInt64, sourcePresentationTimeUs: UInt64? = nil) {
        // Transport activity is immediate UI state. Do not keep the live video
        // covered by a "Waiting" overlay until the one-second FPS window ends.
        if !isReceiving {
            DispatchQueue.main.async { [weak self] in
                self?.isReceiving = true
            }
        }
        var sourceAgeMs: Double?
        if let sourcePresentationTimeUs {
            let nowUs = UInt64(Date().timeIntervalSince1970 * 1_000_000)
            if nowUs >= sourcePresentationTimeUs {
                let ageMs = Double(nowUs - sourcePresentationTimeUs) / 1_000
                // This metric assumes roughly synchronized host/device clocks.
                // Ignore obvious clock skew rather than polluting latency data.
                if ageMs < 60_000 {
                    sourceAgeMs = ageMs
                }
            }
        }
        let frame = QueuedFrame(
            data: data,
            frameId: frameId,
            recvNs: DispatchTime.now().uptimeNanoseconds
        )

        var shouldScheduleDecode = false
        var arrivalGapMs: Double?
        var pendingDepth = 0
        pendingFramesLock.lock()
        if let sourceAgeMs {
            sourceAgeSumMs += sourceAgeMs
            sourceAgeMaxMs = max(sourceAgeMaxMs, sourceAgeMs)
            sourceAgeSamples += 1
        }
        if lastFrameArrivalNs != 0 {
            let gapNs = frame.recvNs - lastFrameArrivalNs
            let gapMs = Double(gapNs) / 1_000_000.0
            arrivalGapMs = gapMs
            if gapMs > maxFrameArrivalGapMsThisSecond {
                maxFrameArrivalGapMsThisSecond = gapMs
            }
        }
        lastFrameArrivalNs = frame.recvNs
        if pendingFrames.count >= Self.frameQueueCapacity {
            pendingFrames.removeFirst()
            queueDropsThisSecond += 1
        }
        pendingFrames.append(frame)
        pendingDepth = pendingFrames.count
        if pendingDepth > maxPendingFramesThisSecond {
            maxPendingFramesThisSecond = pendingDepth
        }
        if !decodeLoopScheduled {
            decodeLoopScheduled = true
            shouldScheduleDecode = true
        }
        pendingFramesLock.unlock()

        if let arrivalGapMs, UInt64(arrivalGapMs * 1_000_000.0) >= Self.frameArrivalGapWarnNs {
            NSLog(
                "[anchor] [video] frame_arrival_gap_ms=%.1f frame=%llu queued=%d size=%d",
                arrivalGapMs,
                frameId,
                pendingDepth,
                data.count
            )
        }

        if shouldScheduleDecode {
            decodeQueue.async { [weak self] in
                self?.drainPendingFrames()
            }
        }
    }

    private func drainPendingFrames() {
        while true {
            let nextFrame: QueuedFrame?
            let queuedAfterDequeue: Int
            pendingFramesLock.lock()
            if pendingFrames.isEmpty {
                decodeLoopScheduled = false
                pendingFramesLock.unlock()
                return
            }
            nextFrame = pendingFrames.removeFirst()
            queuedAfterDequeue = pendingFrames.count
            pendingFramesLock.unlock()

            guard let nextFrame else { continue }
            let queueWaitMs = Double(DispatchTime.now().uptimeNanoseconds - nextFrame.recvNs) / 1_000_000.0
            queueWaitSumMs += queueWaitMs
            if queueWaitMs > queueWaitMaxMs {
                queueWaitMaxMs = queueWaitMs
            }
            queueWaitSamples += 1
            if UInt64(queueWaitMs * 1_000_000.0) >= Self.queueWaitWarnNs {
                NSLog(
                    "[anchor] [video] queue_wait_ms=%.1f frame=%llu queued=%d",
                    queueWaitMs,
                    nextFrame.frameId,
                    queuedAfterDequeue
                )
            }
            processNALUnits(
                in: nextFrame.data,
                frameId: nextFrame.frameId,
                recvNs: nextFrame.recvNs
            )
        }
    }

    private func takeQueueIngressMetrics() -> (
        queueDrops: Int,
        maxArrivalGapMs: Double,
        maxPendingFrames: Int,
        avgSourceAgeMs: Double,
        maxSourceAgeMs: Double
    ) {
        pendingFramesLock.lock()
        let snapshot = (
            queueDrops: queueDropsThisSecond,
            maxArrivalGapMs: maxFrameArrivalGapMsThisSecond,
            maxPendingFrames: maxPendingFramesThisSecond,
            avgSourceAgeMs: sourceAgeSamples > 0 ? sourceAgeSumMs / Double(sourceAgeSamples) : 0,
            maxSourceAgeMs: sourceAgeMaxMs
        )
        queueDropsThisSecond = 0
        maxFrameArrivalGapMsThisSecond = 0
        maxPendingFramesThisSecond = pendingFrames.count
        sourceAgeSumMs = 0
        sourceAgeMaxMs = 0
        sourceAgeSamples = 0
        pendingFramesLock.unlock()
        return snapshot
    }

    // MARK: - NAL processing

    private func processNALUnits(in data: Data, frameId: UInt64, recvNs: UInt64) {
        guard let accessUnit = try? AnchorH264AccessUnit.parseAnnexB(data) else { return }

        // ANFR payloads are complete access units. Apply parameter-set changes
        // first, then submit all slice NALs as one CMSampleBuffer, matching the
        // access-unit boundary used by Android.
        if let latestSPS = accessUnit.sequenceParameterSets.last {
            sps = latestSPS
        }
        if let latestPPS = accessUnit.pictureParameterSets.last {
            pps = latestPPS
        }
        tryCreateFormatDescription()

        guard !accessUnit.vclNALUnits.isEmpty, formatDescription != nil else { return }

        enqueueAccessUnit(
            nalUnits: accessUnit.sampleNALUnits,
            isKeyframe: accessUnit.isKeyframe,
            frameId: frameId,
            recvNs: recvNs
        )
    }

    // MARK: - Format description

    private var lastSps: Data?
    private var lastPps: Data?

    private func tryCreateFormatDescription() {
        guard let sps = sps, let pps = pps else { return }

        // Skip if SPS/PPS haven't changed — avoids format recreation flashes.
        if sps == lastSps && pps == lastPps && formatDescription != nil {
            return
        }
        lastSps = sps
        lastPps = pps

        let spsBytes = [UInt8](sps)
        let ppsBytes = [UInt8](pps)

        spsBytes.withUnsafeBufferPointer { spsPtr in
            ppsBytes.withUnsafeBufferPointer { ppsPtr in
                let pointers = [spsPtr.baseAddress!, ppsPtr.baseAddress!]
                let sizes = [sps.count, pps.count]

                var newFormat: CMVideoFormatDescription?
                let status = pointers.withUnsafeBufferPointer { pp in
                    sizes.withUnsafeBufferPointer { sp in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: pp.baseAddress!,
                            parameterSetSizes: sp.baseAddress!,
                            nalUnitHeaderLength: 4,
                            formatDescriptionOut: &newFormat
                        )
                    }
                }

                if status == noErr, let fmt = newFormat {
                    self.formatDescription = fmt
                    NSLog("[anchor] H.264 format description created")
                }
            }
        }
    }

    // MARK: - Decode & display (zero-copy path)

    // Sample construction + enqueue timing. This is deliberately not labelled
    // decode time because AVSampleBufferDisplayLayer decodes asynchronously.
    private var enqueueTimeSum: Double = 0
    private var enqueueTimeMax: Double = 0
    private var enqueueCount: Int = 0

    private func enqueueAccessUnit(
        nalUnits: [Data],
        isKeyframe: Bool,
        frameId: UInt64,
        recvNs: UInt64
    ) {
        let enqueueStart = CACurrentMediaTime()

        guard let formatDescription = formatDescription,
              let layer = displayLayer else { return }

        // Build one AVCC sample for the entire access unit.
        let totalLength = nalUnits.reduce(0) { $0 + 4 + $1.count }
        guard totalLength > 4 else { return }

        var blockBuffer: CMBlockBuffer?
        let status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: totalLength,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: totalLength,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return }

        var destinationOffset = 0
        for nal in nalUnits {
            var nalLength = UInt32(nal.count).bigEndian
            let lengthStatus = withUnsafePointer(to: &nalLength) { lenPtr in
                CMBlockBufferReplaceDataBytes(
                    with: lenPtr,
                    blockBuffer: bb,
                    offsetIntoDestination: destinationOffset,
                    dataLength: 4
                )
            }
            guard lengthStatus == kCMBlockBufferNoErr else { return }
            destinationOffset += 4
            let copyStatus = nal.withUnsafeBytes { bytes in
                CMBlockBufferReplaceDataBytes(
                    with: bytes.baseAddress!,
                    blockBuffer: bb,
                    offsetIntoDestination: destinationOffset,
                    dataLength: nal.count
                )
            }
            guard copyStatus == kCMBlockBufferNoErr else { return }
            destinationOffset += nal.count
        }

        var sampleBuffer: CMSampleBuffer?
        var sampleSize = totalLength
        CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: bb,
            formatDescription: formatDescription,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )

        guard let sb = sampleBuffer else { return }

        // Display immediately
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
            CFDictionarySetValue(dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
                Unmanaged.passUnretained(isKeyframe ? kCFBooleanFalse : kCFBooleanTrue).toOpaque()
            )
        }

        if layer.status == .failed {
            layer.flush()
        }
        layer.enqueue(sb)
        let decodeDoneNs = DispatchTime.now().uptimeNanoseconds
        emitFrameTiming(
            frameId: frameId,
            recvNs: recvNs,
            decodeDoneNs: decodeDoneNs,
            presentNs: decodeDoneNs
        )

        let enqueueMs = (CACurrentMediaTime() - enqueueStart) * 1000
        enqueueTimeSum += enqueueMs
        if enqueueMs > enqueueTimeMax { enqueueTimeMax = enqueueMs }
        enqueueCount += 1

        // FPS + decode stats
        frameCount += 1
        let now = CACurrentMediaTime()
        let elapsed = now - lastFpsTime
        if elapsed >= 1.0 {
            let currentFps = Int(Double(frameCount) / elapsed)
            let avgEnqueue = enqueueCount > 0 ? enqueueTimeSum / Double(enqueueCount) : 0
            let maxEnqueue = enqueueTimeMax
            let ingress = takeQueueIngressMetrics()
            let avgQueueWait = queueWaitSamples > 0 ? queueWaitSumMs / Double(queueWaitSamples) : 0
            let maxQueueWait = queueWaitMaxMs
            NSLog("[anchor] FPS: %d | enqueue avg=%.2fms max=%.2fms | source_age avg=%.1fms max=%.1fms | queue_drops=%d | queue_wait avg=%.2fms max=%.2fms | arrival_gap_max=%.1fms pending_max=%d | AU=%dB nals=%d | layer=%@",
                  currentFps, avgEnqueue, maxEnqueue, ingress.avgSourceAgeMs, ingress.maxSourceAgeMs,
                  ingress.queueDrops, avgQueueWait, maxQueueWait,
                  ingress.maxArrivalGapMs, ingress.maxPendingFrames, totalLength, nalUnits.count,
                  layer.status == .failed ? "FAILED" : "ok")
            enqueueTimeSum = 0
            enqueueTimeMax = 0
            enqueueCount = 0
            queueWaitSumMs = 0
            queueWaitMaxMs = 0
            queueWaitSamples = 0
            frameCount = 0
            lastFpsTime = now
            DispatchQueue.main.async {
                self.fps = currentFps
                if !self.isReceiving { self.isReceiving = true }
            }
        }
    }

    /// AVSampleBufferDisplayLayer does not expose a precise presentation
    /// callback, so we currently use post-enqueue time as the phone-local
    /// decode/present proxy for desktop perf correlation.
    /// Disabled for now because the desktop branch treats these as generic
    /// JSON packets and logs a warning for every frame.
    private func emitFrameTiming(frameId _: UInt64, recvNs _: UInt64, decodeDoneNs _: UInt64, presentNs _: UInt64) {}
}
