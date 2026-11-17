package org.anchor.sdk

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class VideoFrameProtocolTest {
    @Test
    fun fragmentHeaderMatchesDecoderAndReassemblesInOrder() {
        val payload = ByteArray(VideoFrameProtocol.PAYLOAD_BYTES * 2 + 17) { (it and 0xff).toByte() }
        val packets = VideoFrameProtocol.fragment(
            kind = VideoFrameProtocol.KIND_CAMERA,
            flags = VideoFrameProtocol.FLAG_KEYFRAME,
            capabilitySessionId = 9,
            flowId = 4,
            sequence = 12,
            presentationTimeUs = 1234,
            codecConfigId = 1,
            payload = payload,
        )
        assertEquals(3, packets.size)
        val decoded = packets.map { VideoFrameProtocol.decode(it) }
        assertEquals(3, decoded.size)
        decoded.forEachIndexed { index, packet ->
            assertNotNull(packet)
            assertEquals(index, packet!!.header.fragmentIndex)
            assertEquals(3, packet.header.fragmentCount)
            assertEquals(12, packet.header.sequence)
            assertEquals(VideoFrameProtocol.KIND_CAMERA, packet.header.kind)
        }
        assertArrayEquals(payload, decoded.flatMap { it!!.payload.asIterable() }.toByteArray())
    }

    @Test
    fun malformedAndOversizedDatagramsAreRejected() {
        assertNull(VideoFrameProtocol.decode(ByteArray(VideoFrameProtocol.HEADER_BYTES - 1)))
        val payload = ByteArray(VideoFrameProtocol.PAYLOAD_BYTES)
        val packet = VideoFrameProtocol.fragment(
            kind = VideoFrameProtocol.KIND_SCREEN,
            flags = 0,
            capabilitySessionId = 1,
            flowId = 2,
            sequence = 3,
            presentationTimeUs = 0,
            codecConfigId = 0,
            payload = payload,
        ).single()
        packet[34] = 0
        packet[35] = 0
        assertNull(VideoFrameProtocol.decode(packet))
    }

    @Test
    fun decoderRejectsPayloadLargerThanDeclaredDatagramLimit() {
        val packet = VideoFrameProtocol.fragment(
            kind = VideoFrameProtocol.KIND_SCREEN,
            flags = 0,
            capabilitySessionId = 1,
            flowId = 2,
            sequence = 3,
            presentationTimeUs = 4,
            codecConfigId = 5,
            payload = ByteArray(VideoFrameProtocol.PAYLOAD_BYTES),
        ).single()

        assertNull(VideoFrameProtocol.decode(packet + byteArrayOf(0)))
    }

    @Test
    fun fragmentWithParityAppendsOneParityPerGroup() {
        // 40 fragments -> groups of 16 + 16 + 8 -> 3 parity datagrams.
        val payload = ByteArray(VideoFrameProtocol.PAYLOAD_BYTES * 40) { 9 }
        val packets = VideoFrameProtocol.fragmentWithParity(
            kind = VideoFrameProtocol.KIND_SCREEN,
            flags = 0,
            capabilitySessionId = 1,
            flowId = 2,
            sequence = 3,
            presentationTimeUs = 4,
            codecConfigId = 5,
            payload = payload,
        )
        assertEquals(43, packets.size)
        packets.drop(40).forEachIndexed { group, datagram ->
            val packet = VideoFrameProtocol.decode(datagram)
            assertNotNull(packet)
            assertEquals(VideoFrameProtocol.KIND_PARITY, packet!!.header.kind)
            assertEquals(group, packet.header.fragmentIndex)
            assertEquals(40, packet.header.fragmentCount)
            assertEquals(VideoFrameProtocol.PAYLOAD_BYTES, packet.payload.size)
        }
    }

    @Test
    fun parityDatagramReconstructsLostFragmentByteExact() {
        // Reproduce the receiver-side recovery math here to prove the wire
        // format: XOR the parity payload with all present fragments to get
        // the missing one's bytes and the flags-XOR for its length.
        val payload = ByteArray(VideoFrameProtocol.PAYLOAD_BYTES * 16 + 137) { (it % 241).toByte() }
        val packets = VideoFrameProtocol.fragmentWithParity(
            kind = VideoFrameProtocol.KIND_SCREEN,
            flags = 0,
            capabilitySessionId = 1,
            flowId = 2,
            sequence = 3,
            presentationTimeUs = 4,
            codecConfigId = 5,
            payload = payload,
        )
        // 17 fragments -> numGroups = 2, interleaved: group 0 covers the even
        // indices {0,2,...,16}. Fragment 16 (the short tail) is in group 0,
        // whose parity record trails the data at index 17.
        val missingIndex = 16
        val parity = VideoFrameProtocol.decode(packets[17])!!
        assertEquals(0, parity.header.fragmentIndex)
        val acc = parity.payload.copyOf()
        var lengthXor = parity.header.flags
        var i = 0
        while (i < 17) {
            if (i != missingIndex) {
                val fragment = VideoFrameProtocol.decode(packets[i])!!.payload
                lengthXor = lengthXor xor fragment.size
                for (j in fragment.indices) {
                    acc[j] = (acc[j].toInt() xor fragment[j].toInt()).toByte()
                }
            }
            i += 2
        }
        val expectedTail = payload.copyOfRange(16 * VideoFrameProtocol.PAYLOAD_BYTES, payload.size)
        assertEquals(expectedTail.size, lengthXor)
        assertArrayEquals(expectedTail, acc.copyOf(lengthXor))
    }
}
