package com.anchor

import com.anchor.plugin.ScreenFrameAssembler
import com.anchor.plugin.DecoderFrameQueue
import org.anchor.sdk.VideoFrameProtocol as V
import org.junit.Assert.*
import org.junit.Test

class ScreenFrameAssemblerTest {
    private var now = 0L
    // Name the clock argument: a trailing lambda would otherwise bind to the
    // assembler's optional event callback rather than its first parameter.
    private val reader = ScreenFrameAssembler(clock = { now })
    private fun packets(seq: Long, idr: Boolean = false, size: Int = 10) = V.fragment(
        V.KIND_SCREEN, if (idr) V.FLAG_KEYFRAME else 0, 1, 2, seq, 0, 0,
        ByteArray(size) { seq.toByte() },
    )
    private fun send(seq: Long, idr: Boolean = false) = reader.add(packets(seq, idr).single(), 2, 1)
    private fun start() { assertEquals(0L, send(0, true).single().sequence) }

    @Test fun predictiveStartupWaitsForIdr() {
        assertTrue(send(0).isEmpty())
        assertTrue(reader.takeKeyframeRequest())
        assertEquals(1L, send(1, true).single().sequence)
        assertFalse(reader.takeKeyframeRequest())
    }

    @Test fun reorderedCompleteFramesAreDeliveredInEncoderOrder() {
        start()
        assertTrue(send(2).isEmpty())
        now += 20_000_000
        val frames = send(1)
        assertEquals(listOf(1L, 2L), frames.map { it.sequence })
        assertArrayEquals(ByteArray(10) { 2 }, frames.last().data)
        assertFalse(reader.takeKeyframeRequest())
    }

    @Test fun reorderedFragmentsAndDuplicatesDoNotLoseAFrame() {
        start()
        val fragments = packets(1, size = 3000)
        assertTrue(reader.add(fragments[2], 2, 1).isEmpty())
        assertTrue(reader.add(fragments[2], 2, 1).isEmpty())
        assertTrue(reader.add(fragments[0], 2, 1).isEmpty())
        assertArrayEquals(ByteArray(3000) { 1 }, reader.add(fragments[1], 2, 1).single().data)
        assertTrue(reader.add(fragments[1], 2, 1).isEmpty())
    }

    @Test fun missingReferenceTimesOutEvenWithoutFurtherPackets() {
        start()
        assertTrue(send(2).isEmpty())
        now = 49_000_000
        assertTrue(reader.poll().isEmpty())
        assertFalse(reader.takeKeyframeRequest())
        now = 50_000_000
        assertTrue(reader.poll().isEmpty())
        assertTrue(reader.takeKeyframeRequest())
        reader.poll()
        assertTrue(reader.takeKeyframeRequest()) // caller throttles retries
        assertTrue(send(3).isEmpty())
        assertEquals(4L, send(4, true).single().sequence)
        assertFalse(reader.takeKeyframeRequest())
        assertEquals(5L, send(5).single().sequence)
    }

    @Test fun missingFinalFragmentHasAnIndependentAssemblyDeadline() {
        start()
        reader.add(packets(1, size = 2000)[0], 2, 1)
        now = 100_000_000
        reader.poll()
        assertFalse(reader.takeKeyframeRequest())
        now = 250_000_000
        reader.poll()
        assertTrue(reader.takeKeyframeRequest())
    }

    @Test fun largeIdrMayTakeLongerThanReorderAllowanceToAssemble() {
        start()
        val fragments = packets(1, true, 2000)
        reader.add(fragments[0], 2, 1)
        now = 100_000_000
        reader.poll()
        assertEquals(1L, reader.add(fragments[1], 2, 1).single().sequence)
        assertFalse(reader.takeKeyframeRequest())
    }

    @Test fun recoveryIdrSkipsMissingFramesWithoutRequestingAnotherIdr() {
        start()
        send(2)
        assertEquals(3L, send(3, true).single().sequence)
        assertFalse(reader.takeKeyframeRequest())
        assertTrue(send(1).isEmpty())
    }

    @Test fun wrongFlowAndConflictingFragmentMetadataAreRejected() {
        start()
        assertTrue(reader.add(packets(1).single(), 9, 1).isEmpty())
        reader.add(packets(1, size = 2000)[0], 2, 1)
        assertTrue(reader.add(packets(1, true, 2000)[1], 2, 1).isEmpty())
        assertTrue(reader.takeKeyframeRequest())
    }

    @Test fun boundedPendingFramesRecoverRatherThanGrowWithoutLimit() {
        start()
        for (seq in 2L..10L) send(seq)
        assertTrue(reader.takeKeyframeRequest())
        assertEquals(11L, send(11, true).single().sequence)
    }

    @Test fun decoderOverflowDropsTheDependentChainUntilIdr() {
        val queue = DecoderFrameQueue<Pair<Int, Boolean>>(2) { it.second }
        assertFalse(queue.offer(0 to false))
        assertTrue(queue.offer(1 to true))
        assertTrue(queue.offer(2 to false))
        assertFalse(queue.offer(3 to false))
        assertNull(queue.poll())
        assertFalse(queue.offer(4 to false))
        assertTrue(queue.offer(5 to true))
        assertTrue(queue.offer(6 to false))
        assertEquals(5, queue.poll()!!.first)
        assertEquals(6, queue.poll()!!.first)
    }

    @Test fun decoderResetRequiresAnIdrAndOverflowCanRecoverImmediatelyWithOne() {
        val queue = DecoderFrameQueue<Boolean>(1) { it }
        queue.offer(true)
        assertTrue(queue.offer(true))
        assertEquals(true, queue.poll())
        queue.clear()
        assertFalse(queue.offer(false))
    }
}
