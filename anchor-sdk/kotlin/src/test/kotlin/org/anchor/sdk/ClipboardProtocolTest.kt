package org.anchor.sdk

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class ClipboardProtocolTest {
    @Test
    fun textRoundTrip() {
        val origin = ByteArray(32) { 7 }
        val decoded = ClipboardProtocol.decodePublish(
            ClipboardProtocol.encodeText(origin, 4, "hello")
        )
        assertArrayEquals(origin, decoded.originNodeId)
        assertEquals(4, decoded.revision)
        assertEquals("hello", decoded.text)
    }

    @Test
    fun clearRoundTrip() {
        val origin = ByteArray(32) { 9 }
        val decoded = ClipboardProtocol.decodeClear(ClipboardProtocol.encodeClear(origin, 8))
        assertArrayEquals(origin, decoded.originNodeId)
        assertEquals(8, decoded.revision)
    }
}
