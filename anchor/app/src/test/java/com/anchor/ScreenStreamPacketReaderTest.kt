package com.anchor

import com.anchor.plugin.ScreenStreamPacketReader
import org.anchor.sdk.VideoFrameProtocol
import org.junit.Assert.*
import org.junit.Test
import kotlin.random.Random

class ScreenStreamPacketReaderTest {

    private fun framed(payload: ByteArray): ByteArray {
        val out = ByteArray(4 + payload.size)
        val length = payload.size
        out[0] = length.toByte()
        out[1] = (length ushr 8).toByte()
        out[2] = (length ushr 16).toByte()
        out[3] = (length ushr 24).toByte()
        payload.copyInto(out, 4)
        return out
    }

    @Test
    fun readsOnePacketFromOneChunk() {
        val payload = ByteArray(100) { it.toByte() }
        val packets = ScreenStreamPacketReader().add(framed(payload))
        assertEquals(1, packets.size)
        assertArrayEquals(payload, packets[0])
    }

    @Test
    fun readsSeveralPacketsCoalescedIntoOneChunk() {
        val first = ByteArray(50) { 1 }
        val second = ByteArray(70) { 2 }
        val packets = ScreenStreamPacketReader().add(framed(first) + framed(second))
        assertEquals(2, packets.size)
        assertArrayEquals(first, packets[0])
        assertArrayEquals(second, packets[1])
    }

    @Test
    fun waitsForABodySplitAcrossChunks() {
        val reader = ScreenStreamPacketReader()
        val payload = ByteArray(200) { it.toByte() }
        val wire = framed(payload)
        assertTrue(reader.add(wire.copyOfRange(0, 2)).isEmpty())
        assertTrue(reader.add(wire.copyOfRange(2, 4)).isEmpty())
        assertTrue(reader.add(wire.copyOfRange(4, 150)).isEmpty())
        val packets = reader.add(wire.copyOfRange(150, wire.size))
        assertEquals(1, packets.size)
        assertArrayEquals(payload, packets[0])
    }

    @Test
    fun deliversByteAtATime() {
        val reader = ScreenStreamPacketReader()
        val payload = ByteArray(300) { (it * 7).toByte() }
        val wire = framed(payload)
        val collected = mutableListOf<ByteArray>()
        wire.forEach { collected += reader.add(byteArrayOf(it)) }
        assertEquals(1, collected.size)
        assertArrayEquals(payload, collected[0])
    }

    @Test
    fun discardsBufferOnInvalidLengthPrefix() {
        val reader = ScreenStreamPacketReader()
        val tooLong = VideoFrameProtocol.DATAGRAM_BYTES + 1
        val bad = ByteArray(4)
        bad[0] = tooLong.toByte()
        bad[1] = (tooLong ushr 8).toByte()
        bad[2] = (tooLong ushr 16).toByte()
        bad[3] = (tooLong ushr 24).toByte()
        assertTrue(reader.add(bad).isEmpty())

        // The reader resynchronizes on the next well-formed packet.
        val payload = ByteArray(10) { 9 }
        val packets = reader.add(framed(payload))
        assertEquals(1, packets.size)
        assertArrayEquals(payload, packets[0])
    }

    @Test
    fun rejectsZeroLengthPrefix() {
        assertTrue(ScreenStreamPacketReader().add(ByteArray(4)).isEmpty())
    }

    @Test
    fun survivesAChunkLargerThanTheInitialBuffer() {
        val reader = ScreenStreamPacketReader()
        val payload = ByteArray(VideoFrameProtocol.DATAGRAM_BYTES) { it.toByte() }
        val wire = framed(payload)
        val chunk = ByteArray(wire.size * 200)
        repeat(200) { wire.copyInto(chunk, it * wire.size) }
        val packets = reader.add(chunk)
        assertEquals(200, packets.size)
        packets.forEach { assertArrayEquals(payload, it) }
    }

    /**
     * The reader runs on the MsQuic callback thread, so its per-byte cost is
     * on the critical path for every screen frame. This is a coarse guard, not
     * a microbenchmark: it fails only on a regression to per-byte boxing, which
     * is more than an order of magnitude slower than a bulk copy.
     */
    @Test
    fun processesAMegabyteWellUnderAFrameInterval() {
        val payload = ByteArray(VideoFrameProtocol.DATAGRAM_BYTES) { it.toByte() }
        val wire = framed(payload)
        val bytesPerFrame = 1 shl 20
        val packetsPerFrame = bytesPerFrame / wire.size
        val frame = ByteArray(packetsPerFrame * wire.size)
        repeat(packetsPerFrame) { wire.copyInto(frame, it * wire.size) }

        // MsQuic hands over partial writes, so split into realistic chunks.
        val chunkSize = 16 * 1024
        val chunks = (frame.indices step chunkSize).map {
            frame.copyOfRange(it, minOf(it + chunkSize, frame.size))
        }

        repeat(20) { runOnce(chunks, packetsPerFrame) }
        val started = System.nanoTime()
        val iterations = 50
        repeat(iterations) { runOnce(chunks, packetsPerFrame) }
        val perFrameMs = (System.nanoTime() - started) / 1_000_000.0 / iterations

        println("ScreenStreamPacketReader: ${"%.3f".format(perFrameMs)} ms per 1 MiB access unit")
        assertTrue(
            "1 MiB took $perFrameMs ms, which is a large fraction of a 60fps frame interval",
            perFrameMs < 4.0,
        )
    }

    private fun runOnce(chunks: List<ByteArray>, expected: Int) {
        val reader = ScreenStreamPacketReader()
        var count = 0
        chunks.forEach { count += reader.add(it).size }
        assertEquals(expected, count)
    }

    @Test
    fun matchesAPerByteDequeReferenceImplementation() {
        val random = Random(1234)
        val payloads = (0 until 40).map { index ->
            ByteArray(1 + random.nextInt(VideoFrameProtocol.DATAGRAM_BYTES)) { (index + it).toByte() }
        }
        val stream = payloads.fold(ByteArray(0)) { acc, payload -> acc + framed(payload) }

        val reader = ScreenStreamPacketReader()
        val reference = DequeReferenceReader()
        val fromReader = mutableListOf<ByteArray>()
        val fromReference = mutableListOf<ByteArray>()
        var offset = 0
        while (offset < stream.size) {
            val size = minOf(1 + random.nextInt(700), stream.size - offset)
            val chunk = stream.copyOfRange(offset, offset + size)
            fromReader += reader.add(chunk)
            fromReference += reference.add(chunk)
            offset += size
        }

        assertEquals(payloads.size, fromReader.size)
        assertEquals(fromReference.size, fromReader.size)
        fromReader.indices.forEach { assertArrayEquals(fromReference[it], fromReader[it]) }
    }

    /** The previous `ArrayDeque<Byte>` implementation, kept as a test oracle. */
    private class DequeReferenceReader {
        private val bytes = java.util.ArrayDeque<Byte>()

        fun add(chunk: ByteArray): List<ByteArray> {
            chunk.forEach(bytes::addLast)
            val packets = mutableListOf<ByteArray>()
            while (bytes.size >= 4) {
                val length = (bytes.removeFirst().toInt() and 0xff) or
                    ((bytes.removeFirst().toInt() and 0xff) shl 8) or
                    ((bytes.removeFirst().toInt() and 0xff) shl 16) or
                    ((bytes.removeFirst().toInt() and 0xff) shl 24)
                if (length <= 0 || length > VideoFrameProtocol.DATAGRAM_BYTES) {
                    bytes.clear()
                    return emptyList()
                }
                if (bytes.size < length) {
                    bytes.addFirst((length ushr 24).toByte())
                    bytes.addFirst((length ushr 16).toByte())
                    bytes.addFirst((length ushr 8).toByte())
                    bytes.addFirst(length.toByte())
                    break
                }
                packets += ByteArray(length) { bytes.removeFirst() }
            }
            return packets
        }
    }
}
