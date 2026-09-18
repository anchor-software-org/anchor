package com.anchor.core

import java.io.DataInputStream
import java.io.DataOutputStream

/**
 * Wire format helpers for the Anchor protocol.
 * All channels use [4-byte LE length][payload].
 */
object WireFormat {

    private const val MAX_MESSAGE_SIZE = 64 * 1024 * 1024 // 64MB guard

    fun readLengthPrefixed(inp: DataInputStream): ByteArray {
        val b0 = inp.readUnsignedByte()
        val b1 = inp.readUnsignedByte()
        val b2 = inp.readUnsignedByte()
        val b3 = inp.readUnsignedByte()
        val length = b0 or (b1 shl 8) or (b2 shl 16) or (b3 shl 24)

        if (length < 0 || length > MAX_MESSAGE_SIZE) {
            throw java.io.IOException("Oversized message: $length bytes")
        }

        val payload = ByteArray(length)
        inp.readFully(payload)
        return payload
    }

    fun writeLengthPrefixed(out: DataOutputStream, data: ByteArray) {
        val len = data.size
        out.writeByte(len and 0xFF)
        out.writeByte((len shr 8) and 0xFF)
        out.writeByte((len shr 16) and 0xFF)
        out.writeByte((len shr 24) and 0xFF)
        out.write(data)
        out.flush()
    }
}
