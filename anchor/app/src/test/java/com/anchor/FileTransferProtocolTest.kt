package com.anchor

import com.anchor.core.FileTransferProtocol
import com.anchor.core.FileTransferProtocol.Outcome
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.security.MessageDigest
import java.util.Base64

class FileTransferProtocolTest {

    private fun sha(bytes: ByteArray): String =
        FileTransferProtocol.toHex(MessageDigest.getInstance("SHA-256").digest(bytes))

    /** Chunk `data` the way the sender does, feeding an assembler over a buffer. */
    private fun assemble(data: ByteArray, chunkSize: Int, expected: Long): Pair<ByteArrayOutputStream, Outcome> {
        val sink = ByteArrayOutputStream()
        val asm = FileTransferProtocol.Assembler(sink, expected)
        var off = 0
        while (off < data.size) {
            val n = minOf(chunkSize, data.size - off)
            asm.pushChunkB64(FileTransferProtocol.encodeChunk(data, off, n))
            off += n
        }
        return sink to asm.finish(sha(data))
    }

    @Test
    fun roundTripReconstructsBytes() {
        val data = ByteArray(5000) { (it % 251).toByte() }
        val chunks = FileTransferProtocol.chunkCount(data.size.toLong(), 256)
        val (sink, outcome) = assemble(data, 256, chunks)
        assertArrayEquals(data, sink.toByteArray())
        assertEquals(Outcome.Complete, outcome)
    }

    @Test
    fun missingChunkIsIncomplete() {
        val data = ByteArray(1000) { 9 }
        val chunkSize = 256
        val expected = FileTransferProtocol.chunkCount(data.size.toLong(), chunkSize)
        val sink = ByteArrayOutputStream()
        val asm = FileTransferProtocol.Assembler(sink, expected)
        // Feed one fewer chunk than promised.
        var off = 0
        var sent = 0L
        while (off < data.size && sent < expected - 1) {
            val n = minOf(chunkSize, data.size - off)
            asm.pushChunkB64(FileTransferProtocol.encodeChunk(data, off, n))
            off += n; sent++
        }
        val outcome = asm.finish(sha(data))
        assertEquals(Outcome.Incomplete(expected - 1, expected), outcome)
    }

    @Test
    fun wrongChecksumIsMismatch() {
        val data = ByteArray(500) { 7 }
        val chunks = FileTransferProtocol.chunkCount(data.size.toLong(), 256)
        val sink = ByteArrayOutputStream()
        val asm = FileTransferProtocol.Assembler(sink, chunks)
        var off = 0
        while (off < data.size) {
            val n = minOf(256, data.size - off)
            asm.pushChunkB64(FileTransferProtocol.encodeChunk(data, off, n))
            off += n
        }
        assertEquals(Outcome.ChecksumMismatch, asm.finish("00ff00ff"))
    }

    @Test
    fun zeroExpectedReliesOnChecksum() {
        val data = ByteArray(300) { 3 }
        val (sink, outcome) = assemble(data, 128, 0)
        assertArrayEquals(data, sink.toByteArray())
        assertEquals(Outcome.Complete, outcome)
    }

    @Test
    fun rawQuicChunksAvoidBase64AndStillVerify() {
        val data = ByteArray(17_000) { (it * 13 % 251).toByte() }
        val sink = ByteArrayOutputStream()
        val asm = FileTransferProtocol.Assembler(sink, 2)
        asm.pushBytes(data.copyOfRange(0, 16_384))
        asm.pushBytes(data.copyOfRange(16_384, data.size))
        assertArrayEquals(data, sink.toByteArray())
        assertEquals(Outcome.Complete, asm.finish(sha(data)))
    }

    @Test(expected = IllegalArgumentException::class)
    fun rejectsBadBase64() {
        val asm = FileTransferProtocol.Assembler(ByteArrayOutputStream(), 1)
        asm.pushChunkB64("!!! not base64 !!!")
    }

    @Test
    fun sanitizeStripsPathTraversal() {
        assertEquals("passwd", FileTransferProtocol.sanitize("../../etc/passwd"))
        assertEquals("photo.jpg", FileTransferProtocol.sanitize("/abs/path/photo.jpg"))
        assertEquals("doc.pdf", FileTransferProtocol.sanitize("C:\\Users\\x\\doc.pdf"))
        assertEquals("file", FileTransferProtocol.sanitize(".."))
        assertEquals("file", FileTransferProtocol.sanitize(""))
        assertEquals("plain.png", FileTransferProtocol.sanitize("plain.png"))
    }

    @Test
    fun toHexIsLowercase() {
        assertEquals("000fffab", FileTransferProtocol.toHex(byteArrayOf(0x00, 0x0f, 0xff.toByte(), 0xab.toByte())))
    }

    @Test
    fun chunkCountRoundsUp() {
        assertEquals(0L, FileTransferProtocol.chunkCount(0, 16))
        assertEquals(0L, FileTransferProtocol.chunkCount(-1, 16))
        assertEquals(1L, FileTransferProtocol.chunkCount(1, 16))
        assertEquals(1L, FileTransferProtocol.chunkCount(16, 16))
        assertEquals(2L, FileTransferProtocol.chunkCount(17, 16))
    }

    @Test
    fun encodeMatchesStandardBase64() {
        val bytes = byteArrayOf(1, 2, 3, 4, 5)
        val encoded = FileTransferProtocol.encodeChunk(bytes, 0, bytes.size)
        assertArrayEquals(bytes, Base64.getDecoder().decode(encoded))
        // Standard, unwrapped base64 (matches desktop base64::STANDARD).
        assertTrue(!encoded.contains("\n"))
    }
}
