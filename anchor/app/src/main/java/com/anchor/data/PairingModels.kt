package com.anchor.data

sealed class PairingState {
    data object Idle : PairingState()
    data class Requested(
        val deviceId: String,
        val deviceName: String,
        val deviceType: String,
        val fingerprint: String,
        val certificatePem: String
    ) : PairingState()
    data object Accepted : PairingState()
    data object Rejected : PairingState()
}

data class PairedDeviceDisplay(
    val deviceId: String,
    val deviceName: String,
    val fingerprint: String,
    val pairedAt: Long,
    val lastSeen: Long,
    val isOnline: Boolean
)
