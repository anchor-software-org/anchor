package com.anchor.data

import android.content.Context
import android.util.Log
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File
import java.security.MessageDigest

@Serializable
data class TrustedDeviceEntry(
    val deviceId: String,
    val deviceName: String,
    val certificatePem: String,
    val pairedAt: Long,
    val lastSeen: Long,
    // Last IP we successfully connected to this device at. Used to reconnect
    // by name when the device isn't currently discoverable via mDNS. Defaulted
    // so existing on-disk entries (written before this field) still load.
    val lastIp: String? = null
)

class TrustedStore(private val context: Context) {

    private val devicesDir: File
        get() = File(context.filesDir, "trusted_devices").also { it.mkdirs() }

    private val _devices = mutableMapOf<String, TrustedDeviceEntry>()
    val devices: Map<String, TrustedDeviceEntry> get() = _devices

    init {
        load()
    }

    private fun load() {
        devicesDir.listFiles { f -> f.extension == "json" }?.forEach { file ->
            try {
                val entry = Json.decodeFromString<TrustedDeviceEntry>(file.readText())
                _devices[entry.deviceId] = entry
            } catch (e: Exception) {
                Log.w("anchor", "Failed to load trusted device: ${file.name}: ${e.message}")
            }
        }
        Log.i("anchor", "Loaded ${_devices.size} trusted devices")
    }

    fun addDevice(deviceId: String, deviceName: String, certificatePem: String) {
        val now = System.currentTimeMillis() / 1000
        val entry = TrustedDeviceEntry(deviceId, deviceName, certificatePem, now, now)
        _devices[deviceId] = entry
        val file = File(devicesDir, "$deviceId.json")
        file.writeText(Json.encodeToString(TrustedDeviceEntry.serializer(), entry))
        Log.i("anchor", "Paired device: $deviceName ($deviceId)")
    }

    fun removeDevice(deviceId: String): Boolean {
        val removed = _devices.remove(deviceId) != null
        if (removed) {
            File(devicesDir, "$deviceId.json").delete()
            Log.i("anchor", "Unpaired device: $deviceId")
        }
        return removed
    }

    fun isTrusted(deviceId: String): Boolean = _devices.containsKey(deviceId)

    fun certificateMatches(deviceId: String, certificateDer: ByteArray): Boolean {
        val entry = _devices[deviceId] ?: return false
        return try {
            val storedDer = android.util.Base64.decode(
                entry.certificatePem
                    .replace("-----BEGIN CERTIFICATE-----", "")
                    .replace("-----END CERTIFICATE-----", "")
                    .replace("\\n", "")
                    .replace("\n", "")
                    .trim(),
                android.util.Base64.DEFAULT
            )
            MessageDigest.isEqual(storedDer, certificateDer)
        } catch (e: Exception) {
            Log.w("anchor", "Could not validate certificate for $deviceId: ${e.message}")
            false
        }
    }

    fun deviceIdForCertificate(certificateDer: ByteArray): String? =
        _devices.keys.firstOrNull { certificateMatches(it, certificateDer) }

    fun updateLastSeen(deviceId: String) {
        val entry = _devices[deviceId] ?: return
        val updated = entry.copy(lastSeen = System.currentTimeMillis() / 1000)
        _devices[deviceId] = updated
        val file = File(devicesDir, "$deviceId.json")
        file.writeText(Json.encodeToString(TrustedDeviceEntry.serializer(), updated))
    }

    /**
     * Refresh a paired desktop's friendly name from its authenticated SDK
     * session. This makes direct IP pairing display the desktop hostname once
     * the first connection succeeds, without changing its trust identity.
     */
    fun updateDeviceName(deviceId: String, deviceName: String) {
        val entry = _devices[deviceId] ?: return
        val cleanName = deviceName.trim()
        if (cleanName.isEmpty() || cleanName == entry.deviceName) return
        val updated = entry.copy(deviceName = cleanName)
        _devices[deviceId] = updated
        File(devicesDir, "$deviceId.json").writeText(
            Json.encodeToString(TrustedDeviceEntry.serializer(), updated)
        )
        Log.i("anchor", "Updated paired desktop name: $cleanName ($deviceId)")
    }

    /**
     * Record the IP we last reached this device at, so we can reconnect by
     * name even when it isn't advertising via mDNS. Also bumps lastSeen.
     */
    fun updateLastIp(deviceId: String, ip: String) {
        val entry = _devices[deviceId] ?: return
        val updated = entry.copy(lastIp = ip, lastSeen = System.currentTimeMillis() / 1000)
        _devices[deviceId] = updated
        val file = File(devicesDir, "$deviceId.json")
        file.writeText(Json.encodeToString(TrustedDeviceEntry.serializer(), updated))
    }

    companion object {
        fun fingerprint(certDer: ByteArray): String {
            val digest = MessageDigest.getInstance("SHA-256").digest(certDer)
            return digest.joinToString(":") { "%02X".format(it) }
        }
    }
}
