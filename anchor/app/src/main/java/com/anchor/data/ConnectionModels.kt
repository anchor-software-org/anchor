package com.anchor.data

import kotlinx.serialization.Serializable

@Serializable
data class ConnectionProfile(
    val id: String = "",
    val name: String = "",
    val ip: String = "",
    val port: Int = 0,
    val wired: Boolean = false
)

/**
 * A row in the discovery screen's device list: either a previously-paired
 * device (possibly offline) and/or a device currently seen on mDNS.
 */
data class DeviceListEntry(
    val deviceId: String?,
    val name: String,
    val ip: String,
    val isPaired: Boolean,
    val isOnline: Boolean,
    val port: Int = 5027,
    val certificateDer: ByteArray? = null,
    /** True when [ip] reaches this desktop over USB tethering. */
    val wired: Boolean = false,
)

enum class ConnectionStatus {
    DISCONNECTED,
    CONNECTING,
    /** The five-second QUIC connection completed; the desktop must now approve pairing. */
    PAIRING,
    CONNECTED
}

data class ConnectionState(
    val status: ConnectionStatus = ConnectionStatus.DISCONNECTED,
    val host: String = "",
    val port: Int = 0,
    val error: String? = null
)
