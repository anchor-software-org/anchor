package org.anchor.sdk

/**
 * A snapshot of the Android adapter's handoff queue.
 *
 * These counters cover bytes copied by the MsQuic callback and waiting for
 * [QuicTransport.poll]. They do not represent MsQuic's congestion window or
 * the peer's stream receive window. Reliable stream payloads are never
 * discarded to keep this queue small; a growing value is an observability
 * signal that the SDK consumer is polling too slowly.
 */
data class QuicTransportMetrics(
    val queuedEvents: Long,
    val queuedBytes: Long,
    val queueHighWaterEvents: Long,
    val queueHighWaterBytes: Long,
    val enqueuedEvents: Long,
    val enqueuedBytes: Long,
    val dequeuedEvents: Long,
    val dequeuedBytes: Long,
    val pollCalls: Long,
    val polledEvents: Long,
    val polledBytes: Long,
    /** False when an installed JNI bridge predates the optional metrics ABI. */
    val available: Boolean = false,
) {
    companion object {
        val ZERO = QuicTransportMetrics(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    }
}
