package com.anchor.core

import android.util.Log
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow

private const val TIMING_TAG = "anchor.timing"

/**
 * Central message broker — mirrors Rust's MessageHandler.
 *
 * Instead of per-plugin channels with an active router thread,
 * this uses a single SharedFlow that all plugins subscribe to
 * and filter for their own events. This is idiomatic Kotlin
 * and avoids managing per-plugin channel lifecycle.
 */
class MessageBroker {

    private val _events = MutableSharedFlow<AnchorEvent>(
        extraBufferCapacity = 64
    )

    /** All plugins and the ViewModel observe this flow. */
    val events: SharedFlow<AnchorEvent> = _events.asSharedFlow()

    /** Non-suspending send. Drops if buffer is full (unlikely at our scale). */
    fun send(event: AnchorEvent) {
        if (_events.tryEmit(event)) {
            return
        }
        Log.w(TIMING_TAG, "[broker] drop target=${describeTarget(event.target)} message=${describeMessage(event.message)}")
    }

    /** Suspending send with backpressure. */
    suspend fun sendSuspend(event: AnchorEvent) {
        _events.emit(event)
    }

    private fun describeTarget(target: AnchorTarget): String = when (target) {
        is AnchorTarget.Gui -> "gui"
        is AnchorTarget.Network -> "network"
        is AnchorTarget.Device -> "device"
        is AnchorTarget.Service -> "service:${target.id}"
        is AnchorTarget.Broadcast -> "broadcast"
    }

    private fun describeMessage(message: AnchorMessage): String = when (message) {
        is AnchorMessage.Generic -> "generic"
        is AnchorMessage.Binary -> "binary:${message.data.size}B"
        is AnchorMessage.Json -> {
            when {
                message.payload.contains("\"plugin_id\":\"input\"") -> "json:input"
                message.payload.contains("\"command\":\"request_keyframe\"") -> "json:keyframe_request"
                else -> "json"
            }
        }
    }
}
