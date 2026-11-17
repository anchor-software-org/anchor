package com.anchor.data

import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.SocketTimeoutException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Discovers Anchor desktops over a wired USB link.
 *
 * USB tethering gives the desktop an address on a subnet mDNS/NSD cannot see,
 * so this sends `ANCHOR_PROBE_V1` to the tethered subnet's broadcast on UDP
 * 5028; the desktop answers `ANCHOR_HERE_V1\n` + identity JSON (see
 * anchor-desktop `device/probe.rs`). Replies are trusted by source address;
 * the certificate is still verified during pairing.
 */
class WiredDiscovery(private val scope: CoroutineScope) {

    companion object {
        private const val TAG = "anchor.wired"
        internal const val PROBE_PORT = 5028
        internal val PROBE_REQUEST = "ANCHOR_PROBE_V1".toByteArray(Charsets.UTF_8)
        internal const val PROBE_RESPONSE_PREFIX = "ANCHOR_HERE_V1\n"
        private const val PROBE_INTERVAL_MS = 2_000L
        private const val REPLY_WINDOW_MS = 700L
        private const val DEVICE_EXPIRY_MS = 9_000L
        private const val REPLY_BUFFER_BYTES = 8 * 1024
    }

    private val _devices = MutableStateFlow<List<DiscoveredDevice>>(emptyList())
    val devices: StateFlow<List<DiscoveredDevice>> = _devices.asStateFlow()

    /** True while a USB-tethered interface exists, whether or not a desktop
     * has answered a probe yet. Drives the "enable USB tethering" hint. */
    private val _usbTetherActive = MutableStateFlow(false)
    val usbTetherActive: StateFlow<Boolean> = _usbTetherActive.asStateFlow()

    private val running = AtomicBoolean(false)
    private var job: Job? = null
    private val seen = ConcurrentHashMap<String, Pair<DiscoveredDevice, Long>>()

    fun start() {
        if (!running.compareAndSet(false, true)) return
        _devices.value = emptyList()
        seen.clear()
        job = scope.launch(Dispatchers.IO) {
            while (isActive) {
                val interfaces = WiredProbe.tetheredInterfaces()
                _usbTetherActive.value = interfaces.isNotEmpty()
                if (interfaces.isEmpty()) {
                    if (seen.isNotEmpty()) {
                        seen.clear()
                        _devices.value = emptyList()
                    }
                } else {
                    probe(interfaces)
                    pruneStale()
                    _devices.value = seen.values.map { it.first }
                }
                delay(PROBE_INTERVAL_MS)
            }
        }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        job?.cancel()
        job = null
        seen.clear()
        _devices.value = emptyList()
        _usbTetherActive.value = false
    }

    /** Send the probe to each tethered subnet's broadcast address, then collect
     * replies for a bounded window. */
    private fun probe(interfaces: List<NetworkInterface>) {
        val socket = try {
            DatagramSocket().apply {
                broadcast = true
                soTimeout = 150
            }
        } catch (e: Exception) {
            Log.w(TAG, "probe socket failed: ${e.message}")
            return
        }
        try {
            for (iface in interfaces) {
                for (broadcast in WiredProbe.broadcastAddresses(iface)) {
                    runCatching {
                        socket.send(DatagramPacket(PROBE_REQUEST, PROBE_REQUEST.size, broadcast, PROBE_PORT))
                    }.onFailure { Log.d(TAG, "probe to $broadcast failed: ${it.message}") }
                }
            }
            val deadline = System.currentTimeMillis() + REPLY_WINDOW_MS
            val buf = ByteArray(REPLY_BUFFER_BYTES)
            while (System.currentTimeMillis() < deadline) {
                val packet = DatagramPacket(buf, buf.size)
                try {
                    socket.receive(packet)
                } catch (_: SocketTimeoutException) {
                    continue
                }
                val source = packet.address?.hostAddress ?: continue
                WiredProbe.parseResponse(packet.data, packet.length, source)?.let { device ->
                    Log.i(TAG, "Wired desktop: ${device.name} at $source:${device.port} (id=${device.deviceId})")
                    seen[device.deviceId ?: source] = device to System.currentTimeMillis()
                }
            }
        } catch (e: Exception) {
            Log.d(TAG, "probe cycle failed: ${e.message}")
        } finally {
            socket.close()
        }
    }

    private fun pruneStale() {
        val now = System.currentTimeMillis()
        seen.entries.removeAll { now - it.value.second > DEVICE_EXPIRY_MS }
    }
}

/**
 * Android-free helpers for the wired probe path so they can be unit-tested
 * without a device.
 */
internal object WiredProbe {

    /** USB-tethered interfaces: up, non-loopback, IPv4, with a tethered prefix. */
    fun isTetheredInterfaceName(name: String): Boolean {
        val base = name.lowercase().substringBefore('.')
        return base.startsWith("rndis") || base.startsWith("usb") || base.startsWith("ncm")
    }

    fun tetheredInterfaces(): List<NetworkInterface> =
        NetworkInterface.getNetworkInterfaces()?.toList().orEmpty().filter { iface ->
            runCatching { iface.isUp && !iface.isLoopback }.getOrDefault(false) &&
                isTetheredInterfaceName(iface.name) &&
                iface.interfaceAddresses.any { it.address is Inet4Address }
        }

    /** The subnet-directed broadcast address for an interface's IPv4 address.
     * Uses the reported broadcast when present and computes it from the prefix
     * otherwise. */
    fun broadcastAddresses(iface: NetworkInterface): List<InetAddress> =
        iface.interfaceAddresses.mapNotNull { address ->
            val ip = address.address as? Inet4Address ?: return@mapNotNull null
            address.broadcast
                ?: computeBroadcast(ip.address, address.networkPrefixLength.toInt())?.let {
                    runCatching { InetAddress.getByAddress(it) }.getOrNull()
                }
        }

    /** `address | ~mask`: every bit past the prefix set to 1. */
    fun computeBroadcast(address: ByteArray, prefixLength: Int): ByteArray? {
        if (prefixLength < 0 || prefixLength > address.size * 8) return null
        val result = address.copyOf()
        for (bit in prefixLength until address.size * 8) {
            result[bit / 8] = (result[bit / 8].toInt() or (1 shl (7 - bit % 8))).toByte()
        }
        return result
    }

    /** Parse a probe reply into a [DiscoveredDevice], or null if malformed.
     * The IP always comes from the packet's source address — a reply can claim
     * any JSON field, but it cannot claim to live at another address. */
    fun parseResponse(data: ByteArray, length: Int, sourceIp: String): DiscoveredDevice? {
        val text = data.decodeToString(0, length, throwOnInvalidSequence = false)
        if (!text.startsWith(WiredDiscovery.PROBE_RESPONSE_PREFIX)) return null
        val json = runCatching {
            Json.parseToJsonElement(
                text.substring(WiredDiscovery.PROBE_RESPONSE_PREFIX.length)
            ).jsonObject
        }.getOrNull() ?: return null
        val deviceId = json["device_id"]?.jsonPrimitive?.content
            ?.takeIf { it.isNotBlank() && it.length <= 128 } ?: return null
        val port = json["port"]?.jsonPrimitive?.intOrNull?.takeIf { it in 1..65535 }
            ?: return null
        val name = json["device_name"]?.jsonPrimitive?.content
            ?.takeIf { it.isNotBlank() && it.length <= 128 } ?: sourceIp
        val certificateDer = json["certificate"]?.jsonPrimitive?.content
            ?.let { encoded ->
                runCatching { java.util.Base64.getUrlDecoder().decode(encoded) }
                    .getOrNull()
                    ?.takeIf { it.isNotEmpty() && it.size <= 4096 }
            }
        return DiscoveredDevice(
            deviceId = deviceId,
            name = name,
            ip = sourceIp,
            port = port,
            certificateDer = certificateDer,
            wired = true,
        )
    }
}
