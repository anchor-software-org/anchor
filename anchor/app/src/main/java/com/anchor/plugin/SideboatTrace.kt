package com.anchor.plugin

import android.util.Log
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong

/**
 * Structured, bounded Sideboat receiver trace.
 *
 * `clock_ns` is Android's monotonic clock and must not be compared directly
 * with desktop monotonic timestamps.  The ANFR presentation timestamp is
 * retained separately as `source_pts_us` for frame correlation.
 */
internal class SideboatTrace(
    private val tag: String = "anchor.timing",
    private val log: (String) -> Unit = { message -> Log.i(tag, message) },
) {
    val runId: String = UUID.randomUUID().toString().replace("-", "")
    private val eventSequence = AtomicLong()
    private val recent = ArrayDeque<String>(RECENT_EVENT_LIMIT)

    fun event(name: String, vararg fields: Pair<String, Any?>) {
        val values = buildList {
            add("schema=sideboat-trace-v1")
            add("run_id=$runId")
            add("event_seq=${eventSequence.incrementAndGet()}")
            add("event=$name")
            add("clock=android_monotonic_ns")
            add("clock_ns=${System.nanoTime()}")
            fields.forEach { (key, value) ->
                if (value != null) add("$key=${value.toString().replace(WHITESPACE, "_")}")
            }
        }.joinToString(" ")
        synchronized(recent) {
            if (recent.size == RECENT_EVENT_LIMIT) recent.removeFirst()
            recent.addLast(values)
        }
        log(values)
    }

    /** Snapshot for an in-process diagnostic surface; never grows with a run. */
    fun recentEvents(): List<String> = synchronized(recent) { recent.toList() }

    private companion object {
        const val RECENT_EVENT_LIMIT = 256
        val WHITESPACE = Regex("\\s+")
    }
}
