package com.anchor.presentation

import android.app.Application
import android.util.Log
import com.anchor.R
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.data.ConnectionStatus
import com.anchor.data.DeviceListEntry
import com.anchor.data.MdnsDiscovery
import com.anchor.data.PairedDeviceDisplay
import com.anchor.data.PairingState
import com.anchor.data.TrustedDeviceEntry
import com.anchor.data.TrustedStore
import com.anchor.AnchorApplication
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive


class MainViewModel(application: Application) : AndroidViewModel(application) {

    // All plugins and broker are Application-scoped singletons.
    // They survive ViewModel destruction (task swipe from recents).
    private val app = application as AnchorApplication
    private val broker = app.broker
    private val trustedStore = app.trustedStore
    private val myDeviceId = app.myDeviceId
    private val myDeviceName = app.myDeviceName

    val videoPlugin = app.videoPlugin
    val inputPlugin = app.inputPlugin
    private val networkPlugin = app.networkPlugin
    val notificationPlugin = app.notificationPlugin
    private val smsPlugin = app.smsPlugin
    val clipboardPlugin = app.clipboardPlugin
    val fileTransferPlugin = app.fileTransferPlugin
    val cameraPlugin = app.cameraPlugin
    val mediaPlugin = app.mediaPlugin
    val commandsPlugin = app.commandsPlugin

    // Persisted connection IP
    private val prefs = application.getSharedPreferences("anchor_prefs", android.content.Context.MODE_PRIVATE)
    private val _desktopIp = MutableStateFlow(
        prefs.getString("desktop_ip", "127.0.0.1")?.takeIf { it.isNotBlank() } ?: "127.0.0.1"
    )
    val desktopIp = _desktopIp.asStateFlow()

    fun setDesktopIp(ip: String) {
        _desktopIp.value = ip
        prefs.edit().putString("desktop_ip", ip).apply()
    }

    // Touch input on video stream — toggled in Settings
    val touchInputOnStream = MutableStateFlow(prefs.getBoolean("touch_input_on_stream", false))
    fun setTouchInputOnStream(enabled: Boolean) {
        touchInputOnStream.value = enabled
        prefs.edit().putBoolean("touch_input_on_stream", enabled).apply()
    }

    // Touchpad sensitivity (0.5 = slow, 1.5 = default, 3.0 = fast)
    val touchpadSensitivity = MutableStateFlow(prefs.getFloat("touchpad_sensitivity", 1.5f))
    fun setTouchpadSensitivity(value: Float) {
        touchpadSensitivity.value = value
        prefs.edit().putFloat("touchpad_sensitivity", value).apply()
    }

    // Haptic feedback on input surfaces (clicks + scroll detents)
    val hapticsEnabled = MutableStateFlow(prefs.getBoolean("haptics_enabled", true))
    fun setHapticsEnabled(enabled: Boolean) {
        hapticsEnabled.value = enabled
        prefs.edit().putBoolean("haptics_enabled", enabled).apply()
    }

    // Sideboat input mode: false = absolute (finger → cursor coord),
    // true = touchpad (relative drag, cursor doesn't track finger position).
    val sideboatTouchpadMode = MutableStateFlow(prefs.getBoolean("sideboat_touchpad_mode", false))
    fun setSideboatTouchpadMode(enabled: Boolean) {
        sideboatTouchpadMode.value = enabled
        prefs.edit().putBoolean("sideboat_touchpad_mode", enabled).apply()
    }

    // Notifications — direction toggles. NotificationPlugin reads the same prefs
    // keys directly (shared "anchor_prefs"), so changes take effect immediately.
    val notifSendEnabled = MutableStateFlow(prefs.getBoolean("notif_send_enabled", true))
    fun setNotifSendEnabled(enabled: Boolean) {
        notifSendEnabled.value = enabled
        prefs.edit().putBoolean("notif_send_enabled", enabled).apply()
    }

    val notifReceiveEnabled = MutableStateFlow(prefs.getBoolean("notif_receive_enabled", true))
    fun setNotifReceiveEnabled(enabled: Boolean) {
        notifReceiveEnabled.value = enabled
        prefs.edit().putBoolean("notif_receive_enabled", enabled).apply()
    }

    // Package names whose notifications are never forwarded to the desktop.
    val ignoredNotifApps =
        MutableStateFlow(prefs.getStringSet("notif_ignored_apps", emptySet())?.toSet() ?: emptySet())
    fun setAppIgnored(pkg: String, ignored: Boolean) {
        val next = ignoredNotifApps.value.toMutableSet()
        if (ignored) next.add(pkg) else next.remove(pkg)
        ignoredNotifApps.value = next
        // Store a fresh copy — SharedPreferences must not be handed a set it keeps a reference to.
        prefs.edit().putStringSet("notif_ignored_apps", HashSet(next)).apply()
    }

    // UI State
    private val _logs = MutableStateFlow("Ready to connect\n")
    val logs = _logs.asStateFlow()

    // Connection state is owned by AnchorApplication (process-scoped).
    // ViewModel just exposes it so UI can observe.
    val connectionState = app.connectionState
    val latencyMs = app.latencyMs
    val desktopBattery = app.desktopBattery

    // Pairing state
    private val _pairingState = MutableStateFlow<PairingState>(PairingState.Idle)
    val pairingState = _pairingState.asStateFlow()

    // Paired devices list
    private val _pairedDevices = MutableStateFlow<List<PairedDeviceDisplay>>(emptyList())
    val pairedDevices = _pairedDevices.asStateFlow()

    // mDNS discovery of nearby desktops
    private val mdnsDiscovery = MdnsDiscovery(application)

    // Saved (trusted) devices including their last-known IP, kept in sync with
    // the on-disk store via refreshSavedDevices().
    private val _savedDevices = MutableStateFlow<List<TrustedDeviceEntry>>(emptyList())

    /**
     * Merged discovery list shown on the disconnected screen: every saved
     * device (online if currently on mDNS) plus any unpaired devices seen on
     * the network. Saved devices use their fresh mDNS IP when available, else
     * their stored last IP.
     */
    val deviceList: kotlinx.coroutines.flow.StateFlow<List<DeviceListEntry>> =
        combine(_savedDevices, mdnsDiscovery.devices) { saved, discovered ->
            com.anchor.data.DeviceListMerger.merge(saved, discovered)
        }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5000), emptyList())

    fun startDiscovery() = mdnsDiscovery.start()
    fun stopDiscovery() = mdnsDiscovery.stop()

    init {
        app.ensurePluginsStarted()
        videoPlugin.start(viewModelScope)  // video tied to UI lifecycle
        refreshPairedDevices()

        viewModelScope.launch {
            broker.events.collect { event ->
                when (event.target) {
                    is AnchorTarget.Gui, is AnchorTarget.Broadcast -> handleGuiEvent(event)
                    else -> {}
                }
            }
        }

        // Suppress desktop notifications while streaming (user is viewing desktop)
        viewModelScope.launch {
            videoPlugin.isReceiving.collect { receiving ->
                notificationPlugin.suppressNotifications.set(receiving)
            }
        }

    }

    private fun handleGuiEvent(event: AnchorEvent) {
        when (val msg = event.message) {
            is AnchorMessage.Json -> {
                if (msg.payload.contains("\"type\":\"pairing_request\"")) {
                    parsePairingRequest(msg.payload)
                } else {
                    parseJsonMessage(msg.payload)
                    _logs.value += "${msg.payload}\n"
                }
            }
            is AnchorMessage.Generic -> {
                _logs.value += "${msg.text}\n"
            }
            else -> {}
        }
    }

    private fun parsePairingRequest(payload: String) {
        try {
            val json = Json.parseToJsonElement(payload).jsonObject
            _pairingState.value = PairingState.Requested(
                deviceId = json["device_id"]?.jsonPrimitive?.content ?: "unknown",
                deviceName = json["device_name"]?.jsonPrimitive?.content ?: "Unknown",
                deviceType = json["device_type"]?.jsonPrimitive?.content ?: "unknown",
                fingerprint = json["fingerprint"]?.jsonPrimitive?.content ?: "N/A",
                certificatePem = json["certificate_pem"]?.jsonPrimitive?.content?.replace("\\n", "\n") ?: ""
            )
        } catch (e: Exception) {
            Log.e("anchor", "Failed to parse pairing request: ${e.message}")
        }
    }

    fun respondToPairing(accepted: Boolean) {
        _pairingState.value = if (accepted) PairingState.Accepted else PairingState.Rejected
        networkPlugin.respondToPairing(accepted)

        viewModelScope.launch {
            delay(500)
            _pairingState.value = PairingState.Idle
            if (accepted) refreshPairedDevices()
        }
    }

    fun pairNearbyDesktop(device: DeviceListEntry) {
        networkPlugin.pairNearby(device)
    }

    fun unpairDevice(deviceId: String) {
        trustedStore.removeDevice(deviceId)
        refreshPairedDevices()
    }

    private fun refreshPairedDevices() {
        _savedDevices.value = trustedStore.devices.values.toList()
        _pairedDevices.value = trustedStore.devices.values.map { entry ->
            PairedDeviceDisplay(
                deviceId = entry.deviceId,
                deviceName = entry.deviceName,
                fingerprint = if (entry.certificatePem.isNotEmpty()) {
                    try {
                        val base64 = entry.certificatePem
                            .replace("-----BEGIN CERTIFICATE-----", "")
                            .replace("-----END CERTIFICATE-----", "")
                            .replace("\n", "").trim()
                        val der = android.util.Base64.decode(base64, android.util.Base64.DEFAULT)
                        TrustedStore.fingerprint(der)
                    } catch (_: Exception) { "N/A" }
                } else "N/A",
                pairedAt = entry.pairedAt,
                lastSeen = entry.lastSeen,
                isOnline = networkPlugin.isConnected
            )
        }
    }

    private fun parseJsonMessage(payload: String) {
        // Connection state + latency are now handled at the Application level.
        // ViewModel only needs to handle ViewModel-specific events here.
        try {
            val json = Json.parseToJsonElement(payload).jsonObject
            val type = json["type"]?.jsonPrimitive?.content

            if (type == "connection_status") {
                val status = json["status"]?.jsonPrimitive?.content
                val host = json["host"]?.jsonPrimitive?.content ?: ""
                val deviceId = json["device_id"]?.jsonPrimitive?.content
                when (status) {
                    "connected" -> {
                        reconnectJob?.cancel()
                        reconnectJob = null
                        lastConnectedIp = host
                        lastConnectedDeviceId = deviceId
                        prefs.edit()
                            .putString("last_connected_ip", host)
                            .putString("last_connected_device_id", deviceId)
                            .apply()
                        if (deviceId != null && host.isNotEmpty()) {
                            trustedStore.updateLastIp(deviceId, host)
                        }
                        refreshPairedDevices()
                    }
                    else -> {
                        refreshPairedDevices()
                        startAutoReconnect()
                    }
                }
            }
        } catch (_: Exception) {}
    }

    // Auto-reconnect state
    private var lastConnectedIp: String? = prefs.getString("last_connected_ip", null)
    private var lastConnectedDeviceId: String? = prefs.getString("last_connected_device_id", null)
    private var manualDisconnect = false
    private var reconnectJob: Job? = null
    val autoReconnectEnabled = MutableStateFlow(prefs.getBoolean("auto_reconnect", true))

    fun setAutoReconnect(enabled: Boolean) {
        autoReconnectEnabled.value = enabled
        prefs.edit().putBoolean("auto_reconnect", enabled).apply()
        if (!enabled) {
            reconnectJob?.cancel()
            reconnectJob = null
        }
    }

    /**
     * Pick a single auto-connect target, honoring the user's chosen policy:
     * prefer the last-connected device when it's visible on mDNS (using its
     * fresh IP); otherwise any *other* saved device currently on mDNS; and as a
     * last resort the last-connected device's stored IP (blind attempt).
     * Returns null when there's nothing worth trying.
     */
    private fun pickAutoConnectTarget(): String? =
        com.anchor.data.ReconnectPolicy.pickTarget(
            discovered = mdnsDiscovery.devices.value,
            savedDeviceIds = trustedStore.devices.keys,
            lastConnectedDeviceId = lastConnectedDeviceId?.takeIf { it in trustedStore.devices },
            lastConnectedIp = lastConnectedIp
        )

    // Start mDNS discovery while disconnected; stop it once connected.
    init {
        viewModelScope.launch {
            connectionState.collect { state ->
                if (state.status == ConnectionStatus.CONNECTED) {
                    stopDiscovery()
                } else {
                    startDiscovery()
                }
            }
        }
    }

    // Auto-connect on startup if enabled. Give mDNS a moment to populate so we
    // can prefer a freshly-resolved IP over a stale stored one.
    init {
        if (autoReconnectEnabled.value) {
            viewModelScope.launch {
                delay(1500)
                if (connectionState.value.status == ConnectionStatus.DISCONNECTED) {
                    pickAutoConnectTarget()?.let { target ->
                        Log.i("anchor", "Auto-connect on startup to $target")
                        connectToPc(target)
                    }
                }
            }
        }
    }

    private fun isValidIpOrHost(input: String): Boolean {
        val s = input.trim()
        if (s.isEmpty()) return false
        if (s.contains(":") || s.contains("/") || s.contains(" ")) return false
        // Allow IPv4
        val ipv4Regex = Regex("^((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$")
        if (ipv4Regex.matches(s)) return true
        if (s.equals("localhost", ignoreCase = true)) return true
        // Reject all-digit single-label like "000" or "123" which are not valid IPv4 and will crash native getaddrinfo path
        if (s.matches(Regex("^[0-9]+$"))) return false
        // Allow mDNS/hostnames like "hiatuszen.local" or "my-desktop" with at least 2 chars and valid chars
        // Reject single-char garbage like "g" which will never resolve and currently crashes the QUIC layer
        if (s.length < 2) return false
        val hostRegex = Regex("^[A-Za-z0-9]([A-Za-z0-9\\-\\.]*[A-Za-z0-9])?$")
        if (!hostRegex.matches(s)) return false
        return true
    }

    fun connectToPc(ip: String) {
        try {
            val clean = ip.trim()
            Log.i("anchor", "Connect button requested: host=$clean")
            if (clean.isBlank()) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter an IP address"}"""
                )))
                return
            }
            if (!isValidIpOrHost(clean)) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter a valid IP like 192.168.1.10"}"""
                )))
                return
            }
            if (clean.contains(":") || clean.contains("/")) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter just the IP — no port or scheme"}"""
                )))
                return
            }
            val currentStatus = connectionState.value.status
            if (currentStatus == ConnectionStatus.CONNECTED ||
                currentStatus == ConnectionStatus.CONNECTING ||
                currentStatus == ConnectionStatus.PAIRING
            ) {
                return
            }
            manualDisconnect = false
            lastConnectedIp = clean
            // A host/IP alone does not include the desktop identity needed for
            // certificate-pinned QUIC. Map it to an exact saved address first,
            // otherwise reuse the last (or only) paired desktop so entering an
            // alternate LAN/Tailscale address reconnects instead of re-pairing.
            val desktopId = com.anchor.data.ReconnectPolicy.manualAddressDeviceId(
                address = clean,
                discovered = mdnsDiscovery.devices.value,
                savedDeviceLastIps = trustedStore.devices.mapValues { (_, device) -> device.lastIp },
                lastConnectedDeviceId = lastConnectedDeviceId,
            )
            if (desktopId == null) {
                Log.i("anchor", "Starting direct pairing for new host=$clean")
                networkPlugin.pairDirect(clean)
                return
            }
            networkPlugin.connect(clean, desktopId)
        } catch (e: Exception) {
            Log.e("anchor", "connectToPc failed: ${e.message}", e)
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"${e.message ?: "Connection failed"}"}"""
            )))
        }
    }

    fun disconnect() {
        manualDisconnect = true
        reconnectJob?.cancel()
        reconnectJob = null
        networkPlugin.disconnect()
        videoPlugin.onTransportStopped()
        _logs.value += "Disconnected\n"
        refreshPairedDevices()
        // Keep manualDisconnect true for 10 seconds so auto-reconnect doesn't fire
        // from any lingering disconnect events
        viewModelScope.launch {
            delay(10_000)
            // After 10s, allow auto-reconnect again if user reconnects manually
        }
    }

    private fun startAutoReconnect() {
        if (manualDisconnect) return
        if (!autoReconnectEnabled.value) return
        reconnectJob?.cancel()
        reconnectJob = viewModelScope.launch {
            // Wait longer before first attempt to avoid interfering with user actions
            delay(5000)
            var delayMs = 5000L
            for (attempt in 1..10) {
                val status = connectionState.value.status
                if (status == ConnectionStatus.CONNECTED ||
                    status == ConnectionStatus.CONNECTING ||
                    status == ConnectionStatus.PAIRING
                ) break
                if (manualDisconnect) break
                // Re-evaluate the target each attempt so mDNS results that
                // arrive mid-loop (e.g. the device powering back on) are used.
                val target = pickAutoConnectTarget()
                if (target == null) {
                    Log.i("anchor", "Auto-reconnect: no target available (attempt $attempt)")
                } else {
                    Log.i("anchor", "Auto-reconnect attempt $attempt to $target")
                    connectToPc(target)
                }
                delay(delayMs)
                delayMs = (delayMs * 2).coerceAtMost(30_000)
            }
        }
    }

    fun sendCommand(pluginId: String, command: String, data: Map<String, String> = emptyMap()) {
        val json = buildString {
            append("""{"plugin_id":"$pluginId","command":"$command"""")
            for ((key, value) in data) {
                append(""","$key":"$value"""")
            }
            append("}")
        }
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Network,
                message = AnchorMessage.Json(json)
            )
        )
        _logs.value += "Sent: $json\n"
    }

    override fun onCleared() {
        super.onCleared()
        // Only stop video — it's tied to the UI and pointless offscreen.
        // Network, clipboard, SMS, notifications keep running via processScope
        // while the foreground service keeps the process alive.
        videoPlugin.stop()
        stopDiscovery()
    }

    fun sendTestFoghornNotification() {
        val context = getApplication<Application>().applicationContext
        val channelId = "anchor_test"

        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
            val channel = android.app.NotificationChannel(
                channelId, "Anchor Test", android.app.NotificationManager.IMPORTANCE_DEFAULT
            )
            val nm = context.getSystemService(android.app.NotificationManager::class.java)
            nm.createNotificationChannel(channel)
        }

        val notification = androidx.core.app.NotificationCompat.Builder(context, channelId)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle("Foghorn Test")
            .setContentText("If you see this on desktop, foghorn works!")
            .setPriority(androidx.core.app.NotificationCompat.PRIORITY_DEFAULT)
            .build()

        val nm = context.getSystemService(android.app.NotificationManager::class.java)
        nm.notify(8888, notification)
        Log.i("anchor", "Foghorn: posted test Android notification")
    }
}
