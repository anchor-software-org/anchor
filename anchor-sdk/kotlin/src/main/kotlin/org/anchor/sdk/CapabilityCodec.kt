package org.anchor.sdk

/**
 * A codec for a capability payload. Use this at the extension boundary for
 * application-defined protobuf (or other binary) messages; Anchor owns only
 * the authenticated routing and advertised type URL.
 */
interface CapabilityCodec<T> {
    val typeUrl: String
    fun encode(value: T): ByteArray
    fun decode(payload: ByteArray): T
}

/** A received application-defined capability payload. */
data class CapabilityPayload(val typeUrl: String, val bytes: ByteArray)

fun <T> AnchorSessionEvent.CapabilityRecord.decode(codec: CapabilityCodec<T>): T {
    require(typeUrl == codec.typeUrl) { "record type URL does not match codec" }
    return codec.decode(payload)
}

/** Sends an application-defined payload whose type URL was advertised by this capability. */
suspend fun <T> AnchorCapability.send(codec: CapabilityCodec<T>, value: T) {
    sendRecord(codec.typeUrl, codec.encode(value))
}
