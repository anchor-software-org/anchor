package org.anchor.sdk

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Decoder for Anchor's fixed-header screen/camera frame records. */
object VideoFrameProtocol {
    const val HEADER_BYTES = 52
    // Leave room for QUIC packet protection and overlay-network headers.
    const val DATAGRAM_BYTES = 1100
    const val PAYLOAD_BYTES = DATAGRAM_BYTES - HEADER_BYTES
    const val KIND_SCREEN = 1
    const val KIND_CAMERA = 2
    /**
     * XOR-parity redundancy record covering one group of data fragments. The
     * `flags` field carries the XOR of the covered payload lengths,
     * `fragmentIndex` carries the parity group index, and `fragmentCount` the
     * access unit's real fragment count. Peers that predate this kind drop it
     * during decode-kind filtering, so senders may emit parity unconditionally.
     */
    const val KIND_PARITY = 3
    /**
     * Wire version stamped on parity datagrams, tagging the grouping scheme:
     * v1 covered contiguous 16-fragment groups, v2 interleaves members so a
     * burst of consecutive losses lands in distinct groups. Peers that only
     * know version 1 drop v2 records outright — never mis-reconstruct.
     */
    const val PARITY_WIRE_VERSION = 2
    /** Consecutive data fragments covered by one parity datagram. */
    const val PARITY_GROUP_FRAGMENTS = 16
    const val FLAG_KEYFRAME = 1
    const val FLAG_CODEC_CONFIG = 1 shl 1
    const val MAX_FRAME_BYTES = 8 * 1024 * 1024
    private val MAGIC = byteArrayOf('A'.code.toByte(), 'N'.code.toByte(), 'F'.code.toByte(), 'R'.code.toByte())

    data class Header(
        val kind: Int,
        val flags: Int,
        val capabilitySessionId: Long,
        val flowId: Long,
        val sequence: Long,
        val fragmentIndex: Int,
        val fragmentCount: Int,
        val presentationTimeUs: Long,
        val codecConfigId: Long,
        val version: Int = 1,
    )

    data class Packet(val header: Header, val payload: ByteArray)

    /**
     * Read the routing flow ID out of an ANFR datagram without full header
     * validation. The session uses it to queue inbound datagrams per flow —
     * malformed packets return null and are dropped rather than delivered to
     * a flow that would reject them as a header mismatch.
     */
    fun datagramFlowId(datagram: ByteArray): Long? {
        if (datagram.size < HEADER_BYTES ||
            datagram[0] != MAGIC[0] || datagram[1] != MAGIC[1] ||
            datagram[2] != MAGIC[2] || datagram[3] != MAGIC[3]
        ) {
            return null
        }
        return ByteBuffer.wrap(datagram, 16, 8).order(ByteOrder.LITTLE_ENDIAN).long
    }

    /** Fragment one encoded access unit into MTU-safe Anchor datagrams. */
    fun fragment(
        kind: Int,
        flags: Int,
        capabilitySessionId: Long,
        flowId: Long,
        sequence: Long,
        presentationTimeUs: Long,
        codecConfigId: Long,
        payload: ByteArray,
    ): List<ByteArray> {
        require(kind == KIND_SCREEN || kind == KIND_CAMERA) { "unsupported frame kind" }
        require(payload.isNotEmpty()) { "frame payload must not be empty" }
        require(payload.size <= MAX_FRAME_BYTES) { "frame payload exceeds limit" }
        val count = (payload.size + PAYLOAD_BYTES - 1) / PAYLOAD_BYTES
        require(count <= 0xffff) { "frame has too many fragments" }
        return (0 until count).map { index ->
            val start = index * PAYLOAD_BYTES
            val end = minOf(start + PAYLOAD_BYTES, payload.size)
            ByteBuffer.allocate(HEADER_BYTES + end - start)
                .order(ByteOrder.LITTLE_ENDIAN)
                .put(MAGIC)
                .put(1)
                .put(kind.toByte())
                .putShort(flags.toShort())
                .putLong(capabilitySessionId)
                .putLong(flowId)
                .putLong(sequence)
                .putShort(index.toShort())
                .putShort(count.toShort())
                .putLong(presentationTimeUs)
                .putLong(codecConfigId)
                .put(payload, start, end - start)
                .array()
        }
    }

    /**
     * Fragment one access unit and append `ceil(count / PARITY_GROUP_FRAGMENTS)`
     * XOR-parity datagrams. Groups are interleaved — group `g` covers fragment
     * indices `g, g + numGroups, g + 2·numGroups, …` — so a burst of up to
     * `numGroups − 1` consecutive losses lands in distinct groups and every
     * loss stays single-XOR recoverable. Each parity payload is the bytewise
     * XOR of the group's payloads zero-padded to [PAYLOAD_BYTES]; its `flags`
     * field is the XOR of the covered payload lengths so a receiver can
     * rebuild a lost fragment byte-exact, including a short final fragment.
     * Parity packets are emitted after the data burst. Only useful on lossy
     * datagram flows — skip it on reliable streams.
     */
    fun fragmentWithParity(
        kind: Int,
        flags: Int,
        capabilitySessionId: Long,
        flowId: Long,
        sequence: Long,
        presentationTimeUs: Long,
        codecConfigId: Long,
        payload: ByteArray,
    ): List<ByteArray> {
        val packets = fragment(
            kind, flags, capabilitySessionId, flowId, sequence,
            presentationTimeUs, codecConfigId, payload,
        )
        val count = packets.size
        val numGroups = (count + PARITY_GROUP_FRAGMENTS - 1) / PARITY_GROUP_FRAGMENTS
        val parity = ArrayList<ByteArray>(numGroups)
        for (groupIndex in 0 until numGroups) {
            val acc = ByteArray(PAYLOAD_BYTES)
            var lengthXor = 0
            var i = groupIndex
            while (i < count) {
                val packet = packets[i]
                val payloadLength = packet.size - HEADER_BYTES
                lengthXor = lengthXor xor payloadLength
                var j = 0
                while (j < payloadLength) {
                    acc[j] = (acc[j].toInt() xor packet[HEADER_BYTES + j].toInt()).toByte()
                    j++
                }
                i += numGroups
            }
            parity += ByteBuffer.allocate(HEADER_BYTES + PAYLOAD_BYTES)
                .order(ByteOrder.LITTLE_ENDIAN)
                .put(MAGIC)
                .put(PARITY_WIRE_VERSION.toByte())
                .put(KIND_PARITY.toByte())
                .putShort(lengthXor.toShort())
                .putLong(capabilitySessionId)
                .putLong(flowId)
                .putLong(sequence)
                .putShort(groupIndex.toShort())
                .putShort(count.toShort())
                .putLong(presentationTimeUs)
                .putLong(codecConfigId)
                .put(acc)
                .array()
        }
        return packets + parity
    }

    fun decode(datagram: ByteArray): Packet? {
        if (datagram.size < HEADER_BYTES) return null
        if (datagram.size > DATAGRAM_BYTES) return null
        if (!datagram.copyOfRange(0, 4).contentEquals(MAGIC)) return null
        val version = datagram[4].toInt()
        if (version != 1 && version != PARITY_WIRE_VERSION) return null
        val buffer = ByteBuffer.wrap(datagram).order(ByteOrder.LITTLE_ENDIAN)
        buffer.position(5)
        val kind = buffer.get().toInt() and 0xff
        if (kind != KIND_SCREEN && kind != KIND_CAMERA && kind != KIND_PARITY) return null
        val flags = buffer.short.toInt() and 0xffff
        val capabilitySessionId = buffer.long
        val flowId = buffer.long
        val sequence = buffer.long
        val fragmentIndex = buffer.short.toInt() and 0xffff
        val fragmentCount = buffer.short.toInt() and 0xffff
        if (fragmentCount == 0 || fragmentIndex >= fragmentCount) return null
        val presentationTimeUs = buffer.long
        val codecConfigId = buffer.long
        return Packet(
            Header(kind, flags, capabilitySessionId, flowId, sequence, fragmentIndex,
                fragmentCount, presentationTimeUs, codecConfigId, version),
            datagram.copyOfRange(HEADER_BYTES, datagram.size),
        )
    }
}
