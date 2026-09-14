package com.anchor.core

import kotlinx.coroutines.CoroutineScope

/**
 * Plugin interface — mirrors Rust's Plugin trait.
 *
 * Each plugin receives messages through the broker's SharedFlow
 * (filtering for its own target) and sends messages back via broker.send().
 */
interface Plugin {
    /** Unique identifier for routing via AnchorTarget.Service(id) */
    val pluginId: String

    /** Launch the plugin's coroutines within the given scope. */
    fun start(scope: CoroutineScope)

    /** Clean shutdown — cancel internal work, close connections. */
    fun stop()
}
