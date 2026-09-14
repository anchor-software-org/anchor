package org.anchor.sdk

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class QueueAccountingTest {
    @Test
    fun tracksOccupancyTrafficAndHighWaterMarks() {
        val accounting = QueueAccounting()

        accounting.enqueue(100)
        accounting.enqueue(40)
        accounting.dequeue(100)
        accounting.enqueue(200)

        assertEquals(2, accounting.currentItems)
        assertEquals(240, accounting.currentBytes)
        assertEquals(2, accounting.highWaterItems)
        assertEquals(240, accounting.highWaterBytes)
        assertEquals(3, accounting.enqueuedItems)
        assertEquals(340, accounting.enqueuedBytes)
        assertEquals(1, accounting.dequeuedItems)
        assertEquals(100, accounting.dequeuedBytes)
    }

    @Test
    fun rejectsAccountingUnderflow() {
        val accounting = QueueAccounting()
        assertThrows(IllegalStateException::class.java) { accounting.dequeue() }

        accounting.enqueue(5)
        assertThrows(IllegalStateException::class.java) { accounting.dequeue(6) }
    }
}
