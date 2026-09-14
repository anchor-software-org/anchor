package org.anchor.sdk

import org.anchor.sdk.v1.ControlEnvelope
import org.anchor.sdk.v1.Ping
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class AnchorSessionFramingTest {
    @Test
    fun fragmentedPrefaceAndRecordAreReassembled() {
        val frame = ControlFraming.encode(
            ControlEnvelope.newBuilder().setPing(Ping.newBuilder().setNonce(7)).build(),
            first = true,
        )
        val framer = ControlFramer()
        frame.forEach { framer.feed(byteArrayOf(it)) }

        assertEquals(7, framer.next()!!.ping.nonce)
        assertEquals(null, framer.next())
    }

    @Test
    fun multipleRecordsInOneQuicEventPreserveOrder() {
        val first = ControlFraming.encode(
            ControlEnvelope.newBuilder().setPing(Ping.newBuilder().setNonce(1)).build(),
            first = true,
        )
        val second = ControlFraming.encode(
            ControlEnvelope.newBuilder().setPing(Ping.newBuilder().setNonce(2)).build(),
            first = false,
        )
        val framer = ControlFramer()
        framer.feed(first + second)

        assertEquals(1, framer.next()!!.ping.nonce)
        assertEquals(2, framer.next()!!.ping.nonce)
    }

    @Test
    fun invalidControlPrefaceIsRejected() {
        val frame = ControlFraming.encode(
            ControlEnvelope.newBuilder().setPing(Ping.newBuilder().setNonce(1)).build(),
            first = true,
        )
        frame[0] = 'X'.code.toByte()
        assertThrows(AnchorSessionException::class.java) { ControlFramer().feed(frame) }
    }

    @Test
    fun peerMinorAtOrBelowOursIsAcceptedButAheadIsRejected() {
        val frame = ControlFraming.encode(
            ControlEnvelope.newBuilder().setPing(Ping.newBuilder().setNonce(1)).build(),
            first = true,
        )
        // Byte layout: ANCR(4) + major(1) + minor(1) + varint length + payload.
        assertEquals(0.toByte(), frame[5])
        // A peer on our own minor (the only value representable while our
        // minor is 0) must still decode successfully.
        val framer = ControlFramer()
        framer.feed(frame)
        assertEquals(1, framer.next()!!.ping.nonce)
        // A peer on a newer minor must be rejected, since it may rely on
        // control-layer behavior we don't understand yet.
        frame[5] = 1.toByte()
        assertThrows(AnchorSessionException::class.java) { ControlFramer().feed(frame) }
    }
}
