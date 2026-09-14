package com.anchor.plugin

import org.anchor.sdk.VideoFrameProtocol
import java.util.TreeMap

/** Reassembles ANFR packets carried by Sideboat's reliable QUIC stream. */
internal class ScreenFrameAssembler(
    private val clock: () -> Long = System::nanoTime,
    private val onEvent: (OutcomeEvent) -> Unit = {},
) {
    enum class Outcome { COMPLETE, DUPLICATE, STALE, GAP, TIMEOUT, AWAITING_IDR, REJECTED }

    data class OutcomeEvent(
        val outcome: Outcome,
        val sequence: Long? = null,
        val fragmentIndex: Int? = null,
        val fragmentCount: Int? = null,
        val frameBytes: Int = 0,
        val pendingFrames: Int,
        val pendingBytes: Int,
        val isKeyframe: Boolean = false,
        val sourcePresentationTimeUs: Long? = null,
        val reason: String? = null,
        val atNs: Long,
    )

    data class CompletedFrame(
        val sequence: Long,
        val data: ByteArray,
        val isKeyframe: Boolean,
        val sourcePresentationTimeUs: Long,
        val codecConfigId: Long,
        val firstSeenNs: Long,
        val completedNs: Long,
        val fragmentCount: Int,
    )

    private class Partial(val header: VideoFrameProtocol.Header, val firstSeenNs: Long) {
        val fragments = arrayOfNulls<ByteArray>(header.fragmentCount)
        var count = 0
        var bytes = 0
        var completed: ByteArray? = null
        var completedAtNs = 0L
    }

    private val pending = TreeMap<Long, Partial>()
    private var lastDeliveredSequence = -1L
    private var waitingForIdr = true
    private var keyframeRequestPending = false
    private var gapSinceNs: Long? = null
    private var retainedBytes = 0

    fun takeKeyframeRequest(): Boolean = keyframeRequestPending.also { keyframeRequestPending = false }

    fun add(bytes: ByteArray, expectedFlowId: Long, expectedCapabilityId: Long): List<CompletedFrame> {
        val packet = VideoFrameProtocol.decode(bytes)
        if (packet == null) {
            emit(Outcome.REJECTED, reason = "invalid_anfr")
            return emptyList()
        }
        val header = packet.header
        if (header.kind != VideoFrameProtocol.KIND_SCREEN ||
            header.flowId != expectedFlowId ||
            header.capabilitySessionId != expectedCapabilityId ||
            header.fragmentCount > MAX_FRAGMENTS
        ) {
            emit(
                Outcome.REJECTED,
                sequence = header.sequence,
                fragmentIndex = header.fragmentIndex,
                fragmentCount = header.fragmentCount,
                reason = "header_mismatch",
            )
            return emptyList()
        }
        if (header.sequence <= lastDeliveredSequence) {
            emit(
                Outcome.STALE,
                sequence = header.sequence,
                fragmentIndex = header.fragmentIndex,
                fragmentCount = header.fragmentCount,
                isKeyframe = header.flags and VideoFrameProtocol.FLAG_KEYFRAME != 0,
                reason = "before_last_delivered",
            )
            return emptyList()
        }

        val nowNs = clock()
        expire(nowNs)
        if (!pending.containsKey(header.sequence) && pending.size >= MAX_PENDING_FRAMES) {
            recover(Outcome.GAP, header.sequence, "pending_frame_limit")
        }
        val partial = pending.getOrPut(header.sequence) { Partial(header, nowNs) }
        if (partial.header.fragmentCount != header.fragmentCount ||
            partial.header.flags != header.flags ||
            partial.header.presentationTimeUs != header.presentationTimeUs ||
            partial.header.codecConfigId != header.codecConfigId
        ) {
            pending.remove(header.sequence)?.let { retainedBytes -= it.bytes }
            recover(Outcome.REJECTED, header.sequence, "conflicting_fragment_metadata")
            return emptyList()
        }
        if (partial.fragments[header.fragmentIndex] != null) {
            emit(
                Outcome.DUPLICATE,
                sequence = header.sequence,
                fragmentIndex = header.fragmentIndex,
                fragmentCount = header.fragmentCount,
                frameBytes = partial.bytes,
                isKeyframe = header.flags and VideoFrameProtocol.FLAG_KEYFRAME != 0,
                sourcePresentationTimeUs = header.presentationTimeUs,
            )
            return poll()
        }
        if (retainedBytes + packet.payload.size > MAX_RETAINED_BYTES) {
            recover(Outcome.TIMEOUT, header.sequence, "retained_byte_limit")
            return emptyList()
        }
        partial.fragments[header.fragmentIndex] = packet.payload
        partial.bytes += packet.payload.size
        partial.count++
        retainedBytes += packet.payload.size

        if (partial.count == partial.header.fragmentCount && partial.completed == null) {
            partial.completedAtNs = nowNs
            partial.completed = ByteArray(partial.bytes).also { output ->
                var offset = 0
                partial.fragments.forEach { fragment ->
                    checkNotNull(fragment).copyInto(output, offset)
                    offset += fragment.size
                }
            }
        }
        return poll()
    }

    /** Poll without input so a missing final fragment can trigger recovery. */
    fun poll(): List<CompletedFrame> {
        val output = ArrayList<CompletedFrame>()
        if (waitingForIdr && pending.values.none { it.isCompleteKeyframe() }) {
            if (!keyframeRequestPending) keyframeRequestPending = true
            if (gapSinceNs == null) {
                gapSinceNs = clock()
                emit(Outcome.AWAITING_IDR, reason = "predictive_frame_without_idr")
            }
        }
        while (pending.isNotEmpty()) {
            val next = pending[lastDeliveredSequence + 1]
            val idr = pending.entries.firstOrNull { it.value.isCompleteKeyframe() }
            val candidate = if (!waitingForIdr && next?.completed != null) {
                lastDeliveredSequence + 1
            } else idr?.key
            if (candidate != null) {
                val partial = pending.getValue(candidate)
                val data = checkNotNull(partial.completed)
                val keyframe = partial.isKeyframe()
                output += CompletedFrame(
                    sequence = candidate,
                    data = data,
                    isKeyframe = keyframe,
                    sourcePresentationTimeUs = partial.header.presentationTimeUs,
                    codecConfigId = partial.header.codecConfigId,
                    firstSeenNs = partial.firstSeenNs,
                    completedNs = partial.completedAtNs,
                    fragmentCount = partial.header.fragmentCount,
                )
                emit(
                    Outcome.COMPLETE,
                    sequence = candidate,
                    fragmentCount = partial.header.fragmentCount,
                    frameBytes = data.size,
                    isKeyframe = keyframe,
                    sourcePresentationTimeUs = partial.header.presentationTimeUs,
                )
                lastDeliveredSequence = candidate
                waitingForIdr = false
                keyframeRequestPending = false
                gapSinceNs = null
                val iterator = pending.entries.iterator()
                while (iterator.hasNext()) {
                    val entry = iterator.next()
                    if (entry.key <= lastDeliveredSequence) {
                        retainedBytes -= entry.value.bytes
                        iterator.remove()
                    }
                }
                continue
            }

            val nowNs = clock()
            val overtaking = pending.entries.filter {
                it.key > lastDeliveredSequence + 1 && it.value.completed != null
            }
            if (!waitingForIdr && gapSinceNs == null && overtaking.isNotEmpty()) {
                gapSinceNs = overtaking.minOf { it.value.completedAtNs }
            }
            if (gapSinceNs?.let { nowNs - it >= REORDER_TIMEOUT_NS } == true) {
                recover(Outcome.GAP, pending.firstKey(), "missing_sequence")
            } else if (pending.values.any { nowNs - it.firstSeenNs >= ASSEMBLY_TIMEOUT_NS }) {
                val timedOut = pending.entries.firstOrNull { nowNs - it.value.firstSeenNs >= ASSEMBLY_TIMEOUT_NS }
                recover(Outcome.TIMEOUT, timedOut?.key, "fragment_assembly_timeout")
            }
            break
        }
        return output
    }

    private fun expire(nowNs: Long) {
        val timedOut = pending.entries.firstOrNull { nowNs - it.value.firstSeenNs >= ASSEMBLY_TIMEOUT_NS }
        if (timedOut != null) recover(Outcome.TIMEOUT, timedOut.key, "fragment_assembly_timeout")
    }

    private fun recover(outcome: Outcome, sequence: Long?, reason: String) {
        pending.clear()
        retainedBytes = 0
        gapSinceNs = null
        val wasWaiting = waitingForIdr
        waitingForIdr = true
        keyframeRequestPending = true
        emit(outcome, sequence = sequence, reason = reason)
        if (!wasWaiting) emit(Outcome.AWAITING_IDR, sequence = sequence, reason = reason)
    }

    private fun Partial.isKeyframe(): Boolean =
        header.flags and VideoFrameProtocol.FLAG_KEYFRAME != 0

    private fun Partial.isCompleteKeyframe(): Boolean = completed != null && isKeyframe()

    private fun emit(
        outcome: Outcome,
        sequence: Long? = null,
        fragmentIndex: Int? = null,
        fragmentCount: Int? = null,
        frameBytes: Int = 0,
        isKeyframe: Boolean = false,
        sourcePresentationTimeUs: Long? = null,
        reason: String? = null,
    ) {
        onEvent(
            OutcomeEvent(
                outcome = outcome,
                sequence = sequence,
                fragmentIndex = fragmentIndex,
                fragmentCount = fragmentCount,
                frameBytes = frameBytes,
                pendingFrames = pending.size,
                pendingBytes = retainedBytes,
                isKeyframe = isKeyframe,
                sourcePresentationTimeUs = sourcePresentationTimeUs,
                reason = reason,
                atNs = clock(),
            ),
        )
    }

    private companion object {
        const val MAX_FRAGMENTS = 4096
        const val MAX_PENDING_FRAMES = 8
        const val MAX_RETAINED_BYTES = VideoFrameProtocol.MAX_FRAME_BYTES
        const val REORDER_TIMEOUT_NS = 50_000_000L
        const val ASSEMBLY_TIMEOUT_NS = 250_000_000L
    }
}
