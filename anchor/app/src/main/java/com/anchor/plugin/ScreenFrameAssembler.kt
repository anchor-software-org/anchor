package com.anchor.plugin

import org.anchor.sdk.VideoFrameProtocol
import java.util.TreeMap

/** Reassembles ANFR packets carried by Sideboat's reliable QUIC stream. */
internal class ScreenFrameAssembler(
    private val clock: () -> Long = System::nanoTime,
    private val onEvent: (OutcomeEvent) -> Unit = {},
) {
    enum class Outcome { COMPLETE, DUPLICATE, STALE, GAP, TIMEOUT, AWAITING_IDR, REJECTED, PARITY_RECOVERED }

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

    /** One XOR-parity record: XOR of a fragment group's payloads and of their lengths. */
    private class ParityRecord(
        val createdAtNs: Long,
        val lengthXor: Int,
        val payload: ByteArray,
    )

    private val pending = TreeMap<Long, Partial>()
    // Parity records keyed frame sequence -> group index. Datagrams are
    // unordered, so parity can arrive before the matching Partial exists.
    private val pendingParities = HashMap<Long, MutableMap<Int, ParityRecord>>()
    private var parityCount = 0
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
        if (header.flowId != expectedFlowId ||
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
        if (header.kind == VideoFrameProtocol.KIND_PARITY) {
            // Parity for an already-delivered frame is useless; otherwise stash
            // it and see whether this frame is exactly one fragment short.
            if (header.sequence > lastDeliveredSequence) {
                val nowNs = clock()
                expire(nowNs)
                storeParity(header, packet.payload, nowNs)
            }
            return tryParityRecovery(header.sequence)
        }
        if (header.kind != VideoFrameProtocol.KIND_SCREEN) {
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
        if (partial.completed == null) {
            return tryParityRecovery(header.sequence)
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
                        pendingParities.remove(entry.key)?.let { parityCount -= it.size }
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

    /** Store one parity datagram; short payloads can never reconstruct. */
    private fun storeParity(header: VideoFrameProtocol.Header, payload: ByteArray, nowNs: Long) {
        if (payload.size != VideoFrameProtocol.PAYLOAD_BYTES || parityCount >= MAX_PARITY_RECORDS) {
            return
        }
        val groups = pendingParities.getOrPut(header.sequence) { HashMap() }
        if (groups.put(header.fragmentIndex, ParityRecord(nowNs, header.flags, payload)) == null) {
            parityCount++
        }
    }

    /**
     * Rebuild a frame whose missing fragments are each their parity group's
     * only hole. Single-parity XOR recovers at most one loss per group, but
     * groups stripe fragments `i, i + numGroups, …` — so up to `numGroups`
     * losses recover as long as no group is holed twice.
     */
    private fun tryParityRecovery(sequence: Long): List<CompletedFrame> {
        val partial = pending[sequence] ?: return emptyList()
        if (partial.completed != null) return emptyList()
        while (partial.count < partial.header.fragmentCount) {
            val numGroups =
                (partial.fragments.size + VideoFrameProtocol.PARITY_GROUP_FRAGMENTS - 1) /
                    VideoFrameProtocol.PARITY_GROUP_FRAGMENTS
            if (partial.fragments.size - partial.count > numGroups) return emptyList()
            val groupHoles = IntArray(numGroups)
            for (i in partial.fragments.indices) {
                if (partial.fragments[i] == null) groupHoles[i % numGroups]++
            }
            var target = -1
            for (i in partial.fragments.indices) {
                if (partial.fragments[i] == null &&
                    groupHoles[i % numGroups] == 1 &&
                    pendingParities[sequence]?.containsKey(i % numGroups) == true
                ) {
                    target = i
                    break
                }
            }
            if (target < 0) return emptyList()
            val group = target % numGroups
            val parity = pendingParities[sequence]?.get(group) ?: return emptyList()
            val acc = parity.payload.copyOf()
            var lengthXor = parity.lengthXor
            var i = group
            while (i < partial.fragments.size) {
                partial.fragments[i]?.let { fragment ->
                    lengthXor = lengthXor xor fragment.size
                    for (j in fragment.indices) {
                        acc[j] = (acc[j].toInt() xor fragment[j].toInt()).toByte()
                    }
                }
                i += numGroups
            }
            if (lengthXor <= 0 || lengthXor > VideoFrameProtocol.PAYLOAD_BYTES) return emptyList()
            val recovered = acc.copyOf(lengthXor)
            partial.fragments[target] = recovered
            partial.bytes += recovered.size
            partial.count++
            retainedBytes += recovered.size
            emit(
                Outcome.PARITY_RECOVERED,
                sequence = sequence,
                fragmentIndex = target,
                fragmentCount = partial.header.fragmentCount,
                frameBytes = partial.bytes,
            )
        }
        partial.completedAtNs = clock()
        partial.completed = ByteArray(partial.bytes).also { output ->
            var offset = 0
            partial.fragments.forEach { fragment ->
                checkNotNull(fragment).copyInto(output, offset)
                offset += fragment.size
            }
        }
        return poll()
    }

    private fun expire(nowNs: Long) {
        val timedOut = pending.entries.firstOrNull { nowNs - it.value.firstSeenNs >= ASSEMBLY_TIMEOUT_NS }
        if (timedOut != null) recover(Outcome.TIMEOUT, timedOut.key, "fragment_assembly_timeout")
        val groups = pendingParities.values.iterator()
        while (groups.hasNext()) {
            val records = groups.next()
            parityCount -= records.values.count { nowNs - it.createdAtNs > ASSEMBLY_TIMEOUT_NS }
            records.entries.removeAll { nowNs - it.value.createdAtNs > ASSEMBLY_TIMEOUT_NS }
            if (records.isEmpty()) groups.remove()
        }
    }

    private fun recover(outcome: Outcome, sequence: Long?, reason: String) {
        pending.clear()
        pendingParities.clear()
        parityCount = 0
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
        const val MAX_PARITY_RECORDS = 8192
        const val MAX_RETAINED_BYTES = VideoFrameProtocol.MAX_FRAME_BYTES
        const val REORDER_TIMEOUT_NS = 50_000_000L
        const val ASSEMBLY_TIMEOUT_NS = 250_000_000L
    }
}
