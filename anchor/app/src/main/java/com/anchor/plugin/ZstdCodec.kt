package com.anchor.plugin

import java.io.ByteArrayOutputStream
import java.util.zip.Deflater
import java.util.zip.Inflater

/**
 * Compression codec using zlib (deflate) — built into Java/Android.
 *
 * Wire format: raw DEFLATE stream (no zlib/gzip headers).
 * The "compressed" field in the JSON packet is set to "zlib".
 *
 * The desktop also supports this format and will decompress accordingly.
 */
object ZlibCodec {

    fun compress(input: ByteArray): ByteArray {
        val deflater = Deflater(Deflater.DEFAULT_COMPRESSION, true) // true = raw deflate (no zlib header)
        deflater.setInput(input)
        deflater.finish()

        val output = ByteArrayOutputStream(input.size)
        val buffer = ByteArray(8192)
        while (!deflater.finished()) {
            val count = deflater.deflate(buffer)
            output.write(buffer, 0, count)
        }
        deflater.end()
        return output.toByteArray()
    }

    fun decompress(input: ByteArray): ByteArray {
        val inflater = Inflater(true) // true = raw deflate
        inflater.setInput(input)

        val output = ByteArrayOutputStream(input.size * 4)
        val buffer = ByteArray(8192)
        while (!inflater.finished()) {
            val count = inflater.inflate(buffer)
            if (count == 0 && inflater.needsInput()) break
            output.write(buffer, 0, count)
        }
        inflater.end()
        return output.toByteArray()
    }
}
