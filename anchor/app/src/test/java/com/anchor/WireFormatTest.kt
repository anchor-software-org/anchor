package com.anchor

import com.anchor.core.WireFormat
import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException

class WireFormatTest {

    @Test
    fun roundtrip() {
        val data = "hello world".toByteArray()

        val buf = ByteArrayOutputStream()
        WireFormat.writeLengthPrefixed(DataOutputStream(buf), data)

        val result = WireFormat.readLengthPrefixed(
            DataInputStream(ByteArrayInputStream(buf.toByteArray()))
        )
        assertArrayEquals(data, result)
    }

    @Test
    fun emptyPayload() {
        val data = ByteArray(0)

        val buf = ByteArrayOutputStream()
        WireFormat.writeLengthPrefixed(DataOutputStream(buf), data)

        // Should be exactly 4 bytes (length header only)
        assertEquals(4, buf.size())

        val result = WireFormat.readLengthPrefixed(
            DataInputStream(ByteArrayInputStream(buf.toByteArray()))
        )
        assertEquals(0, result.size)
    }

    @Test
    fun lengthHeaderIsLittleEndian() {
        val data = ByteArray(0x0201) // 513 bytes

        val buf = ByteArrayOutputStream()
        WireFormat.writeLengthPrefixed(DataOutputStream(buf), data)

        val header = buf.toByteArray().take(4)
        // 513 = 0x0201 -> LE: [0x01, 0x02, 0x00, 0x00]
        assertEquals(0x01, header[0].toInt() and 0xFF)
        assertEquals(0x02, header[1].toInt() and 0xFF)
        assertEquals(0x00, header[2].toInt() and 0xFF)
        assertEquals(0x00, header[3].toInt() and 0xFF)
    }

    @Test
    fun largePayload() {
        val data = ByteArray(100_000) { (it % 256).toByte() }

        val buf = ByteArrayOutputStream()
        WireFormat.writeLengthPrefixed(DataOutputStream(buf), data)

        val result = WireFormat.readLengthPrefixed(
            DataInputStream(ByteArrayInputStream(buf.toByteArray()))
        )
        assertArrayEquals(data, result)
    }

    @Test(expected = IOException::class)
    fun rejectsOversizedMessage() {
        // Forge a header claiming 128MB
        val len = 128 * 1024 * 1024
        val header = byteArrayOf(
            (len and 0xFF).toByte(),
            ((len shr 8) and 0xFF).toByte(),
            ((len shr 16) and 0xFF).toByte(),
            ((len shr 24) and 0xFF).toByte()
        )
        WireFormat.readLengthPrefixed(
            DataInputStream(ByteArrayInputStream(header))
        )
    }

    @Test
    fun multipleMessagesInSequence() {
        val msg1 = "first".toByteArray()
        val msg2 = "second".toByteArray()
        val msg3 = "third".toByteArray()

        val buf = ByteArrayOutputStream()
        val out = DataOutputStream(buf)
        WireFormat.writeLengthPrefixed(out, msg1)
        WireFormat.writeLengthPrefixed(out, msg2)
        WireFormat.writeLengthPrefixed(out, msg3)

        val inp = DataInputStream(ByteArrayInputStream(buf.toByteArray()))
        assertArrayEquals(msg1, WireFormat.readLengthPrefixed(inp))
        assertArrayEquals(msg2, WireFormat.readLengthPrefixed(inp))
        assertArrayEquals(msg3, WireFormat.readLengthPrefixed(inp))
    }
}
