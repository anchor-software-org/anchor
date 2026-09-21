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
}
