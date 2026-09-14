package com.anchor.core

/**
 * Routing targets — mirrors Rust's AnchorTarget enum.
 * Determines which plugin(s) receive the event.
 */
sealed class AnchorTarget {
    data object Gui : AnchorTarget()
    data object Network : AnchorTarget()
    data object Device : AnchorTarget() // Forward to connected device via network plugin
    data class Service(val id: String) : AnchorTarget()
    data object Broadcast : AnchorTarget()
}

/**
 * Message payload — mirrors Rust's AnchorMessage enum.
 * Json is the primary format for structured data between plugins.
 */
sealed class AnchorMessage {
    data class Generic(val text: String) : AnchorMessage()
    data class Json(val payload: String) : AnchorMessage()
    data class Binary(val data: ByteArray) : AnchorMessage() {
        override fun equals(other: Any?) = other is Binary && data.contentEquals(other.data)
        override fun hashCode() = data.contentHashCode()
    }
}

/**
 * A routable event — mirrors Rust's AnchorEvent struct.
 * All inter-plugin communication goes through these.
 */
data class AnchorEvent(
    val target: AnchorTarget,
    val message: AnchorMessage
)
