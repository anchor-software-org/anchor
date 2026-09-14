package com.anchor

import android.Manifest
import android.app.Application
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.content.pm.PackageManager
import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.data.ConnectionState
import com.anchor.data.ConnectionStatus
import com.anchor.data.TrustedStore
import com.anchor.plugin.CameraPlugin
import com.anchor.plugin.ClipboardPlugin
import com.anchor.plugin.CommandsPlugin
import com.anchor.plugin.FileTransferPlugin
import com.anchor.plugin.InputPlugin
import com.anchor.plugin.MediaPlugin
import com.anchor.plugin.NetworkPlugin
import com.anchor.plugin.NotificationPlugin
import com.anchor.plugin.SmsPlugin
import com.anchor.plugin.VideoPlugin
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.io.File

/**
 * Application-scoped singletons for plugins and broker.
 *
 * These survive Activity/ViewModel destruction (task swipe) as long as
 * the process is alive (kept alive by AnchorConnectionService).
 */
class AnchorApplication : Application() {

    /** Connects the clipboard provider to a negotiated SDK session. */
    fun attachSdkClipboard(session: AnchorSession, capability: AnchorCapability, originNodeId: ByteArray) {
        clipboardPlugin.attachSdkSession(session, capability, originNodeId, processScope)
    }

    /** Process-level coroutine scope — survives ViewModel clearing. */
    val processScope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    val broker = MessageBroker()
    val trustedStore by lazy { TrustedStore(this) }
    val myDeviceId by lazy { getOrCreateDeviceId() }
    val myDeviceName: String = android.os.Build.MODEL

    val videoPlugin by lazy { VideoPlugin(broker) }
    val inputPlugin by lazy { InputPlugin(broker) }
    val clipboardPlugin by lazy { ClipboardPlugin(broker, this) }
    val notificationPlugin by lazy { NotificationPlugin(broker, this) }
    val smsPlugin by lazy { SmsPlugin(broker, this) }
    val networkPlugin: NetworkPlugin by lazy {
        NetworkPlugin(
            broker = broker,
            videoPlugin = videoPlugin,
            trustedStore = trustedStore,
            deviceId = myDeviceId,
            deviceName = myDeviceName,
            hasSmsPermissions = { hasSmsPermissions() },
            context = this,
            onSdkClipboardReady = { session, capability, originNodeId ->
                clipboardPlugin.attachSdkSession(session, capability, originNodeId, processScope)
            },
            onSdkNotificationsReady = { session, capability ->
                notificationPlugin.attachSdkSession(session, capability)
            },
            onSdkInputReady = { session, capability ->
                inputPlugin.attachSdkSession(session, capability)
            },
            onSdkMediaReady = { session, capability ->
                mediaPlugin.attachSdkSession(session, capability)
            },
            onSdkScreenReady = { session, capability ->
                videoPlugin.attachSdkSession(session, capability)
            },
            onSdkCameraReady = { session, capability ->
                cameraPlugin.attachSdkSession(session, capability)
            },
            onSdkSmsReady = { capability -> smsPlugin.attachSdkSession(capability) },
            onSdkCommandsReady = { capability -> commandsPlugin.attachSdkSession(capability) },
            onSdkFilesReady = { capability -> fileTransferPlugin.attachSdkSession(capability) },
            onSdkTransportStopped = { fileTransferPlugin.onTransportStopped() },
            onSdkRecord = { event ->
                clipboardPlugin.handleSdkRecord(event)
                fileTransferPlugin.handleSdkEvent(event)
                smsPlugin.handleSdkRecord(event)
                commandsPlugin.handleSdkRecord(event)
                cameraPlugin.handleSdkRecord(event)
            },
            onSdkStreamOpen = { session, event ->
                fileTransferPlugin.acceptSdkStream(session, event)
            },
        )
    }
    val cameraPlugin: CameraPlugin by lazy { CameraPlugin(broker, this, networkPlugin) }
    val mediaPlugin by lazy { MediaPlugin(broker, this) }
    val commandsPlugin by lazy { CommandsPlugin(broker) }
    val fileTransferPlugin by lazy { FileTransferPlugin(broker, this) }

    // --- Connection state (process-scoped, survives ViewModel death) ---
    private val _connectionState = MutableStateFlow(ConnectionState())
    val connectionState = _connectionState.asStateFlow()

    private val _latencyMs = MutableStateFlow(0)
    val latencyMs = _latencyMs.asStateFlow()

    private val _desktopBattery = MutableStateFlow<Pair<Int, String>?>(null)
    val desktopBattery = _desktopBattery.asStateFlow()

    // Held while connected — prevents Android from raising the DTIM sleep multiplier,
    // which otherwise causes 100-200ms spikes on enterprise/university WiFi.
    // WIFI_MODE_FULL_LOW_LATENCY: screen-on + foreground → full low-latency mode;
    // screen-off or background → falls back to FULL_HIGH_PERF (power save still off).
    // See: https://developer.android.com/reference/android/net/wifi/WifiManager#WIFI_MODE_FULL_LOW_LATENCY
    private var wifiLock: WifiManager.WifiLock? = null

    private fun acquireWifiLock() {
        if (wifiLock?.isHeld == true) return
        val wm = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
        wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "anchor:stream")
            .also { it.acquire() }
        Log.i("Anchor", "WiFi low-latency lock acquired")
    }

    private fun releaseWifiLock() {
        if (wifiLock?.isHeld == true) {
            wifiLock!!.release()
            Log.i("Anchor", "WiFi low-latency lock released")
        }
        wifiLock = null
    }

    private var pluginsStarted = false
    private var smsPluginStarted = false

    fun hasSmsPermissions(): Boolean =
        checkSelfPermission(Manifest.permission.READ_SMS) == PackageManager.PERMISSION_GRANTED &&
            checkSelfPermission(Manifest.permission.SEND_SMS) == PackageManager.PERMISSION_GRANTED

    fun hasContactsPermission(): Boolean =
        checkSelfPermission(Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED

    @Synchronized
    fun ensureSmsPluginStarted() {
        if (smsPluginStarted || !hasSmsPermissions()) return
        smsPluginStarted = true
        smsPlugin.start(processScope)
    }

    fun ensurePluginsStarted() {
        if (pluginsStarted) return
        pluginsStarted = true
        networkPlugin.processScope = processScope
        networkPlugin.start(processScope)
        notificationPlugin.start(processScope)
        ensureSmsPluginStarted()
        clipboardPlugin.start(processScope)
        inputPlugin.start(processScope)
        cameraPlugin.start(processScope)
        mediaPlugin.start(processScope)
        commandsPlugin.start(processScope)
        fileTransferPlugin.start(processScope)

        // Debug hook for emulator verification: `adb shell am broadcast -a com.anchor.TEST_SEND_SMS_ATTACHMENT --es hint mms_part_test_123 --es content "hello"`
        registerReceiver(
            object : BroadcastReceiver() {
                override fun onReceive(context: Context?, intent: Intent?) {
                    if (intent?.action != "com.anchor.TEST_SEND_SMS_ATTACHMENT") return
                    val hint = intent.getStringExtra("hint") ?: "mms_part_test"
                    val content = intent.getStringExtra("content") ?: "test attachment payload"
                    val uri = try {
                        val tmp = File(cacheDir, "anchor_test_attachment_${System.nanoTime()}.bin")
                        tmp.writeText(content)
                        android.net.Uri.fromFile(tmp)
                    } catch (e: Exception) {
                        Log.e("Anchor", "TEST_SEND_SMS_ATTACHMENT: failed to create temp file: ${e.message}")
                        return
                    }
                    Log.i("Anchor", "TEST_SEND_SMS_ATTACHMENT: sending $uri with hint=$hint")
                    fileTransferPlugin.sendFile(uri, hint)
                }
            },
            IntentFilter("com.anchor.TEST_SEND_SMS_ATTACHMENT"),
            Context.RECEIVER_EXPORTED,
        )
        // Video is started per-ViewModel (tied to UI lifecycle)

        // Battery monitoring — send to desktop when connected
        val batteryReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
                val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, 100) ?: 100
                val status = intent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1
                if (level < 0 || scale <= 0) return
                val pct = (level * 100 / scale)
                val charging = status == BatteryManager.BATTERY_STATUS_CHARGING
                        || status == BatteryManager.BATTERY_STATUS_FULL
                networkPlugin.sendDeviceState(pct, charging)
            }
        }
        registerReceiver(batteryReceiver, IntentFilter(Intent.ACTION_BATTERY_CHANGED))

        // Listen to broker events for connection state changes at process level
        processScope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Gui || event.target is AnchorTarget.Broadcast) {
                    when (val msg = event.message) {
                        is AnchorMessage.Json -> handleConnectionEvent(msg.payload)
                        else -> {}
                    }
                }
            }
        }
    }

    /** Send current battery state immediately (used on connect). */
    private fun triggerBatteryBroadcast() {
        val intent = registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, 100) ?: 100
        val status = intent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1
        if (level < 0 || scale <= 0) return
        val pct = (level * 100 / scale)
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING
                || status == BatteryManager.BATTERY_STATUS_FULL
        Log.i("Anchor", "Battery: $pct% ${if (charging) "charging" else "discharging"} — sending over SDK")
        networkPlugin.sendDeviceState(pct, charging)
    }

    private fun handleConnectionEvent(payload: String) {
        try {
            val json = Json.parseToJsonElement(payload).jsonObject
            val type = json["type"]?.jsonPrimitive?.content ?: return

            if (type == "latency_update") {
                val rtt = json["rtt_ms"]?.jsonPrimitive?.content?.toDoubleOrNull()
                if (rtt != null) _latencyMs.value = (rtt / 2).toInt()
                return
            }

            if (type == "battery_update") {
                val deviceId = json["device_id"]?.jsonPrimitive?.content ?: return
                if (deviceId == "desktop") {
                    val level = json["level"]?.jsonPrimitive?.content?.toIntOrNull() ?: return
                    val statusText = json["status_text"]?.jsonPrimitive?.content ?: "Battery"
                    _desktopBattery.value = Pair(level, statusText)
                }
                return
            }

            if (type == "connection_status") {
                val status = json["status"]?.jsonPrimitive?.content
                val host = json["host"]?.jsonPrimitive?.content ?: ""
                val port = json["port"]?.jsonPrimitive?.content?.toIntOrNull() ?: 0
                val error = json["error"]?.jsonPrimitive?.content
                val reason = json["reason"]?.jsonPrimitive?.content

                when (status) {
                    "connected" -> {
                        _connectionState.value = ConnectionState(
                            status = ConnectionStatus.CONNECTED, host = host, port = port
                        )
                        AnchorConnectionService.start(this, host)
                        acquireWifiLock()
                        // Send battery immediately on connect
                        triggerBatteryBroadcast()
                    }
                    "connecting" -> {
                        // Starting a (possibly new) connection — drop any
                        // per-device cached state from the previous session so
                        // a different desktop's battery / media doesn't bleed
                        // through while the new one comes up.
                        clearPerDeviceCache()
                        _connectionState.value = ConnectionState(
                            status = ConnectionStatus.CONNECTING, host = host, port = port
                        )
                    }
                    "pairing" -> {
                        _connectionState.value = ConnectionState(
                            status = ConnectionStatus.PAIRING, host = host, port = port
                        )
                    }
                    else -> {
                        clearPerDeviceCache()
                        _connectionState.value = ConnectionState(
                            status = ConnectionStatus.DISCONNECTED,
                            host = host, port = port,
                            error = error ?: reason
                        )
                        releaseWifiLock()
                        AnchorConnectionService.stop(this)
                    }
                }
            }
        } catch (_: Exception) {}
    }

    /** Wipe any state we cached from the *previous* connected desktop. */
    private fun clearPerDeviceCache() {
        _desktopBattery.value = null
        mediaPlugin.clearRemoteState()
    }

    private fun getOrCreateDeviceId(): String {
        val file = File(filesDir, "anchor_device_id")
        return if (file.exists()) {
            file.readText().trim()
        } else {
            val id = java.util.UUID.randomUUID().toString()
            file.writeText(id)
            id
        }
    }
}
