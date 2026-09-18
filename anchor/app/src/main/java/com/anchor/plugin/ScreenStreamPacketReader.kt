package com.anchor.plugin

import org.anchor.sdk.VideoFrameProtocol

/**
 * Reassembles the length-prefixed ANFR packets carried by a reliable QUIC
 * stream. MsQuic may split or coalesce writes arbitrarily, so a chunk can hold
 * a fraction of one packet or several packets plus a partial tail.
 *
 * Buffering is a plain byte array with read/write offsets rather than a
 * `Deque<Byte>`. Screen traffic runs at tens of megabytes per second on the
 * MsQuic callback thread, and a per-byte deque costs one boxed insert and one
 * boxed removal for every byte on the wire — several million operations per
 * second, plus a reference array eight times the size of the payload it holds.
 * A byte array reduces the same work to two bulk copies per packet.
 */
internal class ScreenStreamPacketReader {
    private var buffer = ByteArray(INITIAL_CAPACITY)

    /** Offset of the first byte not yet consumed by a completed packet. */
    private var head = 0

    /** Offset one past the last byte written. */
    private var tail = 0

    fun add(chunk: ByteArray): List<ByteArray> {
        append(chunk)
        val packets = mutableListOf<ByteArray>()
        while (tail - head >= LENGTH_PREFIX_BYTES) {
            val length = (buffer[head].toInt() and 0xff) or
                ((buffer[head + 1].toInt() and 0xff) shl 8) or
                ((buffer[head + 2].toInt() and 0xff) shl 16) or
                ((buffer[head + 3].toInt() and 0xff) shl 24)
            if (length <= 0 || length > VideoFrameProtocol.DATAGRAM_BYTES) {
                // The framing is unrecoverable: no later byte can resynchronize
                // a stream whose prefix is already wrong. Drop what is buffered.
                head = 0
                tail = 0
                return emptyList()
            }
            if (tail - head - LENGTH_PREFIX_BYTES < length) {
                // The body may arrive in a later MsQuic callback. Leave the
                // prefix in place rather than consuming and re-inserting it.
                break
            }
            val start = head + LENGTH_PREFIX_BYTES
            packets += buffer.copyOfRange(start, start + length)
            head = start + length
        }
        if (head == tail) {
            head = 0
            tail = 0
        }
        return packets
    }

    private fun append(chunk: ByteArray) {
        if (tail + chunk.size > buffer.size) {
            val used = tail - head
            if (used + chunk.size <= buffer.size) {
                buffer.copyInto(buffer, 0, head, tail)
            } else {
                var capacity = buffer.size
                while (capacity < used + chunk.size) {
                    capacity *= 2
                }
                val grown = ByteArray(capacity)
                buffer.copyInto(grown, 0, head, tail)
                buffer = grown
            }
            head = 0
            tail = used
        }
        chunk.copyInto(buffer, tail)
        tail += chunk.size
    }

    private companion object {
        const val LENGTH_PREFIX_BYTES = 4

        /** Comfortably above one access unit's worth of 1100-byte packets. */
        const val INITIAL_CAPACITY = 64 * 1024
    }
}
