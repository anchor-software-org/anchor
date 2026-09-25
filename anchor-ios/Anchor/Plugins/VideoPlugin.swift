import Foundation
import VideoToolbox
import AVFoundation
import CoreMedia
import Combine
import AnchorSDK

/// H.264 inter-frames depend on earlier frames. This queue intentionally keeps
/// every access unit in FIFO order; dropping an arbitrary frame produces
/// block corruption in later frames until the next IDR.
struct OrderedFrameQueue<Element> {
    private var storage: [Element] = []
    private var head = 0

    var isEmpty: Bool { storage.isEmpty }
    var count: Int { storage.count - head }
    var first: Element? { isEmpty ? nil : storage[head] }

    mutating func append(_ element: Element) {
        storage.append(element)
    }

    mutating func popFirst() -> Element? {
        guard !storage.isEmpty else { return nil }
        let element = storage[head]
        head += 1

        if head == storage.count {
            storage.removeAll(keepingCapacity: true)
            head = 0
        } else if head >= 64, head * 2 >= storage.count {
            storage.removeFirst(head)
            head = 0
        }
        return element
    }

    mutating func removeAll() {
        storage.removeAll(keepingCapacity: true)
        head = 0
    }
}

/// A decoder flush invalidates every predictive frame until the next IDR.
/// Keeping this gate separate makes it difficult to accidentally reintroduce
/// the block-corruption bug by submitting a P-frame immediately after a flush.
struct KeyframeRecoveryGate {
    private(set) var isWaiting = true

    mutating func reset() {
        isWaiting = true
    }

    mutating func shouldSubmit(isKeyframe: Bool) -> Bool {
        guard isWaiting else { return true }
        guard isKeyframe else { return false }
        isWaiting = false
        return true
    }
}

struct FrameSequenceTracker {
    private var lastFrameId: UInt64?

    mutating func reset() {
        lastFrameId = nil
    }

    /// Returns false when at least one complete access unit was skipped or
    /// reordered. UInt64 rollover remains contiguous.
    mutating func observe(_ frameId: UInt64) -> Bool {
        defer { lastFrameId = frameId }
        guard let lastFrameId else { return true }
        return frameId == lastFrameId &+ 1
    }
}

class VideoPlugin: Plugin, ObservableObject {
    private static let frameArrivalGapWarnNs: UInt64 = 40_000_000
    private static let queueWaitWarnNs: UInt64 = 40_000_000

    let pluginId = "video"

    struct PreviewState {
        var isReceiving = false
        var fps: Int = 0
        var streamWidth: Int = 0
        var streamHeight: Int = 0
    }

    struct StreamOutput: Identifiable, Equatable {
        let id: String
        let name: String
        let width: Int
        let height: Int
    }

    @Published var isReceiving = false
    @Published var fps: Int = 0
    @Published var streamWidth: Int = 0
    @Published var streamHeight: Int = 0
    @Published var availableOutputs: [StreamOutput] = []
    @Published var selectedOutputID: String = ""

    private(set) var displayLayer: AVSampleBufferDisplayLayer?

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
    private var pendingFrames = OrderedFrameQueue<QueuedFrame>()
    private var decodeLoopScheduled = false
    private var renderer: AVSampleBufferVideoRenderer?
    private var rendererRequestActive = false
    private var keyframeRecoveryGate = KeyframeRecoveryGate()
    private var keyframeRequestOutstanding = false
    private var frameSequenceTracker = FrameSequenceTracker()
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
    private var rendererBackpressureStartedNs: UInt64?
    private var rendererBackpressureEvents = 0
    private var rendererBackpressureWaitSumMs: Double = 0
    private var rendererBackpressureWaitMaxMs: Double = 0
    private var rendererFlushes = 0
    private var recoveryDrops = 0
    private var parseFailures = 0
    private var sequenceDiscontinuities = 0

    init(broker: MessageBroker, previewState: PreviewState = .init()) {
        self.broker = broker
        self.isReceiving = previewState.isReceiving
        self.fps = previewState.fps
        self.streamWidth = previewState.streamWidth
        self.streamHeight = previewState.streamHeight
    }

    func start() {}

    /// Binds the layer's thread-safe renderer exactly once per actual layer.
    /// Replacing a renderer discards its decoder references, so queued
    /// predictive frames are cleared and the new renderer waits for an IDR.
    @discardableResult
    func bindDisplayLayer(_ layer: AVSampleBufferDisplayLayer) -> Bool {
        guard displayLayer !== layer else { return false }

        let oldRenderer = displayLayer?.sampleBufferRenderer
        let newRenderer = layer.sampleBufferRenderer
        displayLayer = layer
        decodeQueue.async { [weak self] in
            guard let self else { return }
            if self.rendererRequestActive {
                oldRenderer?.stopRequestingMediaData()
            }
            self.rendererRequestActive = false
            self.renderer = newRenderer
            self.keyframeRecoveryGate.reset()
            self.frameSequenceTracker.reset()
            self.clearPendingFramesForRecovery()
            self.requestRecoveryKeyframe(reason: "display_layer_changed")
        }
        return true
    }

    func stop() {
        decodeQueue.async { [weak self] in
            guard let self else { return }
            if self.rendererRequestActive {
                self.renderer?.stopRequestingMediaData()
            }
            self.rendererRequestActive = false
            self.pendingFramesLock.lock()
            self.pendingFrames.removeAll()
            self.decodeLoopScheduled = false
            self.lastFrameArrivalNs = 0
            self.maxFrameArrivalGapMsThisSecond = 0
            self.maxPendingFramesThisSecond = 0
            self.queueDropsThisSecond = 0
            self.sourceAgeSumMs = 0
            self.sourceAgeMaxMs = 0
            self.sourceAgeSamples = 0
            self.pendingFramesLock.unlock()
            self.queueWaitSumMs = 0
            self.queueWaitMaxMs = 0
            self.queueWaitSamples = 0
            self.rendererBackpressureStartedNs = nil
            self.rendererBackpressureEvents = 0
            self.rendererBackpressureWaitSumMs = 0
            self.rendererBackpressureWaitMaxMs = 0
            self.rendererFlushes = 0
            self.recoveryDrops = 0
            self.parseFailures = 0
            self.sequenceDiscontinuities = 0
            self.enqueueTimeSum = 0
            self.enqueueTimeMax = 0
            self.enqueueCount = 0
            self.frameCount = 0
            self.lastFpsTime = CACurrentMediaTime()
            self.keyframeRecoveryGate.reset()
            self.keyframeRequestOutstanding = false
            self.frameSequenceTracker.reset()
            self.formatDescription = nil
            self.sps = nil
            self.pps = nil
        }
        DispatchQueue.main.async {
            self.isReceiving = false
            self.fps = 0
        }
    }

    func handleStreamInfo(_ payload: String) {
        guard let data = payload.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        let reportedWidth = json["width"] as? Int ?? 0
        let reportedHeight = json["height"] as? Int ?? 0
        let outputID = json["output_id"] as? String ?? ""
        DispatchQueue.main.async {
            if !outputID.isEmpty {
                self.selectedOutputID = outputID
            }
            let dimensions = Self.resolveStreamDimensions(
                reportedWidth: reportedWidth,
                reportedHeight: reportedHeight,
                outputID: outputID,
                selectedOutputID: self.selectedOutputID,
                outputs: self.availableOutputs,
                currentWidth: self.streamWidth,
                currentHeight: self.streamHeight
            )
            self.streamWidth = dimensions.width
            self.streamHeight = dimensions.height
            NSLog(
                "[anchor] Stream geometry: reported=%dx%d resolved=%dx%d output=%@",
                reportedWidth,
                reportedHeight,
                dimensions.width,
                dimensions.height,
                outputID
            )
        }
        NSLog("[anchor] Stream info: \(reportedWidth)x\(reportedHeight)")
        // Don't request keyframe here — desktop already sends IDR after encoder init.
    }

    func handleOutputs(_ outputs: [ANCHScreenScreenOutput]) {
        let mapped = outputs.map {
            StreamOutput(
                id: $0.outputID,
                name: $0.displayName.isEmpty ? $0.outputID : $0.displayName,
                width: Int($0.width),
                height: Int($0.height)
            )
        }
        DispatchQueue.main.async {
            self.availableOutputs = mapped
            self.selectedOutputID = Self.resolveSelectedOutputID(
                current: self.selectedOutputID,
                outputs: mapped
            )
            let dimensions = Self.resolveStreamDimensions(
                reportedWidth: 0,
                reportedHeight: 0,
                outputID: self.selectedOutputID,
                selectedOutputID: self.selectedOutputID,
                outputs: mapped,
                currentWidth: self.streamWidth,
                currentHeight: self.streamHeight
            )
            self.streamWidth = dimensions.width
            self.streamHeight = dimensions.height
        }
    }

    static func resolveSelectedOutputID(current: String, outputs: [StreamOutput]) -> String {
        if outputs.contains(where: { $0.id == current }) {
            return current
        }
        return outputs.first?.id ?? ""
    }

    static func resolveStreamDimensions(
        reportedWidth: Int,
        reportedHeight: Int,
        outputID: String,
        selectedOutputID: String,
        outputs: [StreamOutput],
        currentWidth: Int,
        currentHeight: Int
    ) -> (width: Int, height: Int) {
        if reportedWidth > 0, reportedHeight > 0 {
            return (reportedWidth, reportedHeight)
        }

        let desiredOutputID = outputID.isEmpty ? selectedOutputID : outputID
        let output = outputs.first(where: { $0.id == desiredOutputID })
            ?? (outputs.count == 1 ? outputs[0] : nil)
        if let output, output.width > 0, output.height > 0 {
            return (output.width, output.height)
        }

        if currentWidth > 0, currentHeight > 0 {
            return (currentWidth, currentHeight)
        }
        return (0, 0)
    }

    func selectOutput(_ outputID: String) {
        guard availableOutputs.contains(where: { $0.id == outputID }) else { return }
        DispatchQueue.main.async {
            self.selectedOutputID = outputID
        }
        broker.send(AnchorEvent(
            target: .device,
            message: .json(#"{"plugin_id":"wayland","command":"select_output","output_id":"\#(outputID)"}"#)
        ))
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
            guard let renderer else {
                clearPendingFramesForRecovery()
                pendingFramesLock.lock()
                decodeLoopScheduled = false
                pendingFramesLock.unlock()
                return
            }

            guard renderer.isReadyForMoreMediaData else {
                beginRendererBackpressure()
                pendingFramesLock.lock()
                decodeLoopScheduled = false
                pendingFramesLock.unlock()
                requestRendererMediaData(renderer)
                return
            }
            endRendererBackpressure()

            let nextFrame: QueuedFrame?
            let queuedAfterDequeue: Int
            pendingFramesLock.lock()
            if pendingFrames.isEmpty {
                decodeLoopScheduled = false
                pendingFramesLock.unlock()
                stopRequestingRendererMediaData(renderer)
                return
            }
            nextFrame = pendingFrames.popFirst()
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
                recvNs: nextFrame.recvNs,
                renderer: renderer
            )
        }
    }

    private func requestRendererMediaData(_ requestedRenderer: AVSampleBufferVideoRenderer) {
        guard !rendererRequestActive else { return }
        rendererRequestActive = true
        requestedRenderer.requestMediaDataWhenReady(on: decodeQueue) { [weak self, weak requestedRenderer] in
            guard let self,
                  let requestedRenderer,
                  self.renderer === requestedRenderer else { return }
            self.drainPendingFrames()
        }
    }

    private func stopRequestingRendererMediaData(_ requestedRenderer: AVSampleBufferVideoRenderer) {
        guard rendererRequestActive, renderer === requestedRenderer else { return }
        requestedRenderer.stopRequestingMediaData()
        rendererRequestActive = false
    }

    private func beginRendererBackpressure() {
        guard rendererBackpressureStartedNs == nil else { return }
        rendererBackpressureStartedNs = DispatchTime.now().uptimeNanoseconds
        rendererBackpressureEvents += 1
    }

    private func endRendererBackpressure() {
        guard let startedNs = rendererBackpressureStartedNs else { return }
        let waitMs = Double(DispatchTime.now().uptimeNanoseconds - startedNs) / 1_000_000.0
        rendererBackpressureWaitSumMs += waitMs
        rendererBackpressureWaitMaxMs = max(rendererBackpressureWaitMaxMs, waitMs)
        rendererBackpressureStartedNs = nil
    }

    private func clearPendingFramesForRecovery() {
        pendingFramesLock.lock()
        recoveryDrops += pendingFrames.count
        pendingFrames.removeAll()
        pendingFramesLock.unlock()
    }

    private func requestRecoveryKeyframe(reason: String) {
        guard !keyframeRequestOutstanding else { return }
        keyframeRequestOutstanding = true
        NSLog("[anchor] [video] requesting recovery IDR reason=%@", reason)
        requestKeyframe()
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

    private func processNALUnits(
        in data: Data,
        frameId: UInt64,
        recvNs: UInt64,
        renderer: AVSampleBufferVideoRenderer
    ) {
        if !frameSequenceTracker.observe(frameId) {
            sequenceDiscontinuities += 1
            keyframeRecoveryGate.reset()
            requestRecoveryKeyframe(reason: "sequence_discontinuity")
        }

        let accessUnit: AnchorH264AccessUnit
        do {
            accessUnit = try AnchorH264AccessUnit.parseAnnexB(data)
        } catch {
            parseFailures += 1
            keyframeRecoveryGate.reset()
            requestRecoveryKeyframe(reason: "malformed_access_unit")
            return
        }

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

        if renderer.status == .failed {
            let description = renderer.error?.localizedDescription ?? "unknown"
            NSLog("[anchor] [video] renderer failed; flushing error=%@", description)
            renderer.flush()
            rendererFlushes += 1
            keyframeRecoveryGate.reset()
            requestRecoveryKeyframe(reason: "renderer_failed")
        }

        guard keyframeRecoveryGate.shouldSubmit(isKeyframe: accessUnit.isKeyframe) else {
            recoveryDrops += 1
            requestRecoveryKeyframe(reason: "waiting_for_idr")
            return
        }
        if accessUnit.isKeyframe {
            keyframeRequestOutstanding = false
        }

        let didEnqueue = enqueueAccessUnit(
            avccPayload: accessUnit.avccPayload,
            nalUnitCount: accessUnit.sampleNALUnits.count,
            isKeyframe: accessUnit.isKeyframe,
            frameId: frameId,
            recvNs: recvNs,
            renderer: renderer
        )
        if !didEnqueue {
            keyframeRecoveryGate.reset()
            requestRecoveryKeyframe(reason: "sample_construction_failed")
        }
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
                    let presentationSize = CMVideoFormatDescriptionGetPresentationDimensions(
                        fmt,
                        usePixelAspectRatio: true,
                        useCleanAperture: true
                    )
                    let decodedWidth = Int(presentationSize.width.rounded())
                    let decodedHeight = Int(presentationSize.height.rounded())
                    if decodedWidth > 0, decodedHeight > 0 {
                        DispatchQueue.main.async {
                            self.streamWidth = decodedWidth
                            self.streamHeight = decodedHeight
                        }
                    }
                    NSLog(
                        "[anchor] H.264 format description created geometry=%dx%d",
                        decodedWidth,
                        decodedHeight
                    )
                }
            }
        }
    }

    // MARK: - Decode & display (single-copy path)

    // Sample construction + enqueue timing. This is deliberately not labelled
    // decode time because AVSampleBufferDisplayLayer decodes asynchronously.
    private var enqueueTimeSum: Double = 0
    private var enqueueTimeMax: Double = 0
    private var enqueueCount: Int = 0

    private func enqueueAccessUnit(
        avccPayload: Data,
        nalUnitCount: Int,
        isKeyframe: Bool,
        frameId: UInt64,
        recvNs: UInt64,
        renderer: AVSampleBufferVideoRenderer
    ) -> Bool {
        let enqueueStart = CACurrentMediaTime()

        guard let formatDescription = formatDescription else { return false }

        // AnchorH264AccessUnit already converted the complete access unit to
        // AVCC. Copy it into CoreMedia once instead of rewriting every NAL and
        // length prefix a second time here.
        let totalLength = avccPayload.count
        guard totalLength > 4 else { return false }

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
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return false }

        let copyStatus = avccPayload.withUnsafeBytes { bytes in
            CMBlockBufferReplaceDataBytes(
                with: bytes.baseAddress!,
                blockBuffer: bb,
                offsetIntoDestination: 0,
                dataLength: totalLength
            )
        }
        guard copyStatus == kCMBlockBufferNoErr else { return false }

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

        guard let sb = sampleBuffer else { return false }

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

        renderer.enqueue(sb)
        let renderSubmitNs = DispatchTime.now().uptimeNanoseconds
        emitFrameTiming(
            frameId: frameId,
            recvNs: recvNs,
            renderSubmitNs: renderSubmitNs
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
            let avgRendererWait = rendererBackpressureEvents > 0
                ? rendererBackpressureWaitSumMs / Double(rendererBackpressureEvents)
                : 0
            NSLog("[anchor] FPS: %d | enqueue avg=%.2fms max=%.2fms | source_age avg=%.1fms max=%.1fms | queue_drops=%d | queue_wait avg=%.2fms max=%.2fms | arrival_gap_max=%.1fms pending_max=%d | renderer_backpressure=%d avg=%.2fms max=%.2fms | recovery_flushes=%d recovery_drops=%d parse_failures=%d sequence_discontinuities=%d | AU=%dB nals=%d | renderer=%@",
                  currentFps, avgEnqueue, maxEnqueue, ingress.avgSourceAgeMs, ingress.maxSourceAgeMs,
                  ingress.queueDrops, avgQueueWait, maxQueueWait,
                  ingress.maxArrivalGapMs, ingress.maxPendingFrames,
                  rendererBackpressureEvents, avgRendererWait, rendererBackpressureWaitMaxMs,
                  rendererFlushes, recoveryDrops, parseFailures, sequenceDiscontinuities,
                  totalLength, nalUnitCount, renderer.status == .failed ? "FAILED" : "ok")
            enqueueTimeSum = 0
            enqueueTimeMax = 0
            enqueueCount = 0
            queueWaitSumMs = 0
            queueWaitMaxMs = 0
            queueWaitSamples = 0
            rendererBackpressureEvents = 0
            rendererBackpressureWaitSumMs = 0
            rendererBackpressureWaitMaxMs = 0
            rendererFlushes = 0
            recoveryDrops = 0
            parseFailures = 0
            sequenceDiscontinuities = 0
            frameCount = 0
            lastFpsTime = now
            DispatchQueue.main.async {
                self.fps = currentFps
                if !self.isReceiving { self.isReceiving = true }
            }
        }
        return true
    }

    /// Disabled until the desktop accepts an explicitly named render-submit
    /// event. Post-enqueue time is not decode completion or presentation.
    private func emitFrameTiming(frameId _: UInt64, recvNs _: UInt64, renderSubmitNs _: UInt64) {}
}
