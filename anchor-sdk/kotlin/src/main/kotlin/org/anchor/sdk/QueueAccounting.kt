package org.anchor.sdk

/** A small, allocation-free counter for one FIFO's occupancy and traffic. */
internal class QueueAccounting {
    var currentItems: Long = 0
        private set
    var currentBytes: Long = 0
        private set
    var highWaterItems: Long = 0
        private set
    var highWaterBytes: Long = 0
        private set
    var enqueuedItems: Long = 0
        private set
    var enqueuedBytes: Long = 0
        private set
    var dequeuedItems: Long = 0
        private set
    var dequeuedBytes: Long = 0
        private set

    fun enqueue(bytes: Int = 0) {
        require(bytes >= 0) { "queue item size must not be negative" }
        currentItems++
        currentBytes += bytes
        enqueuedItems++
        enqueuedBytes += bytes
        highWaterItems = maxOf(highWaterItems, currentItems)
        highWaterBytes = maxOf(highWaterBytes, currentBytes)
    }

    fun dequeue(bytes: Int = 0) {
        require(bytes >= 0) { "queue item size must not be negative" }
        check(currentItems > 0) { "cannot dequeue from an empty queue" }
        check(currentBytes >= bytes) { "queue byte accounting underflow" }
        currentItems--
        currentBytes -= bytes
        dequeuedItems++
        dequeuedBytes += bytes
    }
}

/** Snapshot of the three SDK session queues, excluding the native adapter. */
data class AnchorSessionQueueMetrics(
    val eventQueueItems: Long,
    val eventQueueHighWaterItems: Long,
    val streamQueueItems: Long,
    val streamQueueBytes: Long,
    val streamQueueHighWaterItems: Long,
    val streamQueueHighWaterBytes: Long,
    val datagramQueueItems: Long,
    val datagramQueueBytes: Long,
    val datagramQueueHighWaterItems: Long,
    val datagramQueueHighWaterBytes: Long,
    /** Datagrams discarded because the queue hit its bounded capacity. */
    val droppedDatagrams: Long = 0,
    val enqueuedItems: Long,
    val enqueuedBytes: Long,
    val dequeuedItems: Long,
    val dequeuedBytes: Long,
) {
    companion object {
        val ZERO = AnchorSessionQueueMetrics(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    }
}
