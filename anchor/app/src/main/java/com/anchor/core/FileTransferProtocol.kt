package com.anchor.core

import java.io.OutputStream
import java.security.MessageDigest
import java.util.Base64

/**
 * Pure, Android-free file-transfer protocol logic — mirrors the desktop's
 * `Assembler`/helpers so it can be unit-tested on the JVM without a device.
 *
 * Uses `java.util.Base64` (available on API 26+, and in JVM tests) rather than
 * `android.util.Base64` (which is a stub in unit tests). Standard, unwrapped
 * base64 matches the desktop's `base64::STANDARD` byte-for-byte.
 */
object FileTransferProtocol {

    /** Raw bytes per chunk before base64. Small so any one chunk clears the
     *  shared control channel fast, keeping input responsive during a transfer. */
    const val CHUNK_SIZE = 16 * 1024

    private val encoder: Base64.Encoder = Base64.getEncoder()
    private val decoder: Base64.Decoder = Base64.getDecoder()

    fun chunkCount(size: Long, chunkSize: Int = CHUNK_SIZE): Long =
        if (size <= 0L) 0L else (size + chunkSize - 1) / chunkSize

    /** Strip directory components / traversal so a remote name can only land as
     *  a plain file. */
    fun sanitize(raw: String): String {
        val base = raw.substringAfterLast('/').substringAfterLast('\\').trim()
        return if (base.isEmpty() || base == "." || base == "..") "file" else base
    }

    fun toHex(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) sb.append("%02x".format(b.toInt() and 0xFF))
        return sb.toString()
    }

    fun encodeChunk(bytes: ByteArray, offset: Int, len: Int): String =
        encoder.encodeToString(bytes.copyOfRange(offset, offset + len))

    sealed class Outcome {
        data object Complete : Outcome()
        data class Incomplete(val received: Long, val expected: Long) : Outcome()
        data object ChecksumMismatch : Outcome()
    }

    /**
     * Streams decoded chunks into `sink`, tracking the running SHA-256 and chunk
     * count. `chunksExpected == 0` (unknown sender size) skips the count check
     * and relies on the checksum; an empty `expectedSha` skips the checksum.
     */
    class Assembler(private val sink: OutputStream, private val chunksExpected: Long) {
        private val digest = MessageDigest.getInstance("SHA-256")
        private var chunksReceived = 0L

        /** Decode+append one base64 chunk. Throws IllegalArgumentException on bad base64. */
        fun pushChunkB64(dataB64: String) {
            pushBytes(decoder.decode(dataB64))
        }

        /** Append one raw QUIC stream chunk without a base64 round-trip. */
        fun pushBytes(bytes: ByteArray) {
            sink.write(bytes)
            digest.update(bytes)
            chunksReceived++
        }

        fun finish(expectedSha: String): Outcome {
            sink.flush()
            if (chunksExpected != 0L && chunksReceived != chunksExpected) {
                return Outcome.Incomplete(chunksReceived, chunksExpected)
            }
            val actual = toHex(digest.digest())
            return if (expectedSha.isNotEmpty() && !actual.equals(expectedSha, ignoreCase = true)) {
                Outcome.ChecksumMismatch
            } else {
                Outcome.Complete
            }
        }
    }
}
