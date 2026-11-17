package com.anchor.data

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean

/** A desktop discovered on the local network via mDNS or a wired USB probe. */
data class DiscoveredDevice(
    val deviceId: String?,
    val name: String,
    val ip: String,
    val port: Int,
    /** Public desktop certificate used only to pin a pairing-only connection. */
    val certificateDer: ByteArray? = null,
    /** True when discovered over USB tethering (see [WiredDiscovery]). */
    val wired: Boolean = false,
)

/** Reassembles the bounded TXT chunks emitted by the desktop mDNS advertiser. */
internal fun decodeCertificateAdvertisement(attrs: Map<String, ByteArray>): ByteArray? {
    val encoded = buildString {
        var index = 0
        while (true) {
            val chunk = attrs["certificate$index"] ?: break
            append(String(chunk, Charsets.UTF_8))
            index += 1
        }
    }
    if (encoded.isEmpty()) return null
    return runCatching { java.util.Base64.getUrlDecoder().decode(encoded) }
        .getOrNull()
        ?.takeIf { it.isNotEmpty() && it.size <= 4096 }
}

/** A comparison-only number shown on both acceptance prompts. It is derived
 * from public desktop certificate material; users never type or transmit it. */
internal fun pairingSafetyNumber(certificateDer: ByteArray?): String? {
    if (certificateDer == null) return null
    val digest = java.security.MessageDigest.getInstance("SHA-256").digest(certificateDer)
    val number = ((digest[0].toInt() and 0xff) shl 16) or
        ((digest[1].toInt() and 0xff) shl 8) or
        (digest[2].toInt() and 0xff)
    return "%06d".format(java.util.Locale.ROOT, number % 1_000_000)
}

/**
 * Discovers Anchor desktops on the local network using Android's NsdManager
 * (DNS-SD / mDNS). Desktops advertise `_anchor._udp` on QUIC port 5027 with TXT
 * records `device_id` / `device_name` (see anchor-desktop `device/mdns.rs`).
 *
 * NsdManager only reliably resolves one service at a time, so resolves are
 * serialized through a queue. A WifiManager.MulticastLock is held while
 * discovery is active so multicast packets aren't filtered on Wi-Fi.
 */
class MdnsDiscovery(context: Context) {

    companion object {
        private const val TAG = "anchor.mdns"
        private const val SERVICE_TYPE = "_anchor._udp."
    }

    private val appContext = context.applicationContext
    private val nsdManager = appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val wifiManager =
        appContext.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager

    private val _devices = MutableStateFlow<List<DiscoveredDevice>>(emptyList())
    val devices: StateFlow<List<DiscoveredDevice>> = _devices.asStateFlow()

    private var multicastLock: WifiManager.MulticastLock? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null
    private val running = AtomicBoolean(false)

    // Serialize resolves: NsdManager rejects concurrent resolveService calls.
    private val resolveQueue = ConcurrentLinkedQueue<NsdServiceInfo>()
    private val resolving = AtomicBoolean(false)

    fun start() {
        if (!running.compareAndSet(false, true)) return
        _devices.value = emptyList()

        multicastLock = wifiManager.createMulticastLock("anchor-mdns").apply {
            setReferenceCounted(false)
            try {
                acquire()
            } catch (e: Exception) {
                Log.w(TAG, "Failed to acquire multicast lock: ${e.message}")
            }
        }

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "Discovery start failed: $errorCode")
                running.set(false)
            }

            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "Discovery stop failed: $errorCode")
            }

            override fun onDiscoveryStarted(serviceType: String) {
                Log.i(TAG, "Discovery started for $serviceType")
            }

            override fun onDiscoveryStopped(serviceType: String) {
                Log.i(TAG, "Discovery stopped for $serviceType")
            }

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                Log.d(TAG, "Service found: ${serviceInfo.serviceName}")
                resolveQueue.add(serviceInfo)
                pumpResolveQueue()
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                Log.d(TAG, "Service lost: ${serviceInfo.serviceName}")
                // Drop by matching service name (carried in DiscoveredDevice.name fallback).
                removeByServiceName(serviceInfo.serviceName)
            }
        }
        discoveryListener = listener

        try {
            nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            Log.w(TAG, "discoverServices failed: ${e.message}")
            running.set(false)
            releaseLock()
        }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        discoveryListener?.let {
            try {
                nsdManager.stopServiceDiscovery(it)
            } catch (e: Exception) {
                Log.w(TAG, "stopServiceDiscovery failed: ${e.message}")
            }
        }
        discoveryListener = null
        resolveQueue.clear()
        resolving.set(false)
        releaseLock()
        _devices.value = emptyList()
    }

    private fun releaseLock() {
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }

    private fun pumpResolveQueue() {
        if (!resolving.compareAndSet(false, true)) return
        val next = resolveQueue.poll()
        if (next == null) {
            resolving.set(false)
            return
        }
        nsdManager.resolveService(next, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.w(TAG, "Resolve failed for ${serviceInfo.serviceName}: $errorCode")
                resolving.set(false)
                pumpResolveQueue()
            }

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                onResolved(serviceInfo)
                resolving.set(false)
                pumpResolveQueue()
            }
        })
    }

    private fun onResolved(info: NsdServiceInfo) {
        val ip = info.host?.hostAddress ?: return
        // Ignore IPv6 link-local; we connect over IPv4.
        if (ip.contains(":")) return

        val attrs = info.attributes
        val deviceId = attrs["device_id"]?.let { String(it, Charsets.UTF_8) }
        val deviceName = attrs["device_name"]?.let { String(it, Charsets.UTF_8) }
            ?: info.serviceName
        val certificateDer = decodeCertificateAdvertisement(attrs)
        if (attrs.keys.any { it.startsWith("certificate") } && certificateDer == null) {
            Log.w(TAG, "Ignoring malformed certificate advertisement for $deviceName")
        }

        val device = DiscoveredDevice(
            deviceId = deviceId,
            name = deviceName,
            ip = ip,
            port = info.port,
            certificateDer = certificateDer,
        )
        Log.i(TAG, "Resolved: $deviceName at $ip:${info.port} (id=$deviceId)")

        _devices.value = _devices.value
            .filterNot { it.ip == device.ip || (deviceId != null && it.deviceId == deviceId) } + device
    }

    private fun removeByServiceName(serviceName: String) {
        _devices.value = _devices.value.filterNot { it.name == serviceName }
    }
}
