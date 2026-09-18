package com.anchor.plugin

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.os.Build
import android.provider.Settings
import android.util.Log
import androidx.core.app.NotificationCompat
import com.anchor.R
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.NotificationsProtocol
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

private const val TAG = "anchor"
private const val CHANNEL_ID = "anchor_desktop_notifications"
private const val CHANNEL_NAME = "Desktop Notifications"

class NotificationPlugin(
    private val broker: MessageBroker,
    private val context: Context
) : Plugin {

    override val pluginId = "foghorn"

    private var listenJob: Job? = null
    private var sdkScope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null
    private val notificationId = AtomicInteger(9000)

    // Same backing store the Settings UI writes to, so toggles apply live.
    private val prefs = context.getSharedPreferences("anchor_prefs", Context.MODE_PRIVATE)

    /** When true, suppress local notification posting (user is viewing desktop stream). */
    val suppressNotifications = AtomicBoolean(false)

    override fun start(scope: CoroutineScope) {
        sdkScope = scope
        createNotificationChannel()
        logListenerState("start-before-callback")

        // Register callback for local notifications from NotificationListenerService
        AnchorNotificationListener.onNotificationPostedCallback = cb@{ pkg, appName, title, body, iconBase64 ->
            if (!prefs.getBoolean("notif_send_enabled", true)) {
                Log.d(TAG, "Foghorn: send disabled, dropping notification from $appName")
                return@cb
            }
            val ignored = prefs.getStringSet("notif_ignored_apps", emptySet()) ?: emptySet()
            if (pkg in ignored) {
                Log.d(TAG, "Foghorn: $pkg is ignored, dropping notification")
                return@cb
            }
            Log.i(TAG, "Foghorn: forwarding Android notification to desktop from $appName: $title")
            val sdk = sdkCapability
            if (sdk != null) {
                val postedAt = System.currentTimeMillis()
                val posted = NotificationsProtocol.encodePosted(
                    notificationId = "android:${pkg}:${postedAt}",
                    applicationId = pkg,
                    applicationName = appName,
                    title = title,
                    body = body,
                    postedAtUnixMs = postedAt,
                )
                sdkScope?.launch(Dispatchers.IO) {
                    runCatching {
                        sdk.sendRecord(
                            NotificationsProtocol.POSTED_TYPE_URL,
                            posted,
                        )
                    }.onSuccess {
                        Log.i(TAG, "Sent notification through Anchor SDK: $title")
                    }.onFailure { error ->
                        Log.w(TAG, "SDK notification send failed: ${error.message}")
                    }
                }
                return@cb
            }
            val iconField = if (iconBase64 != null) ""","icon_base64":"$iconBase64"""" else ""
            val json = """{"plugin_id":"foghorn","type":"notification","source":"android","app_package":"$pkg","app_name":"${appName.replace("\"", "\\\"")}","title":"${title.replace("\"", "\\\"")}","body":"${body.replace("\"", "\\\"")}","timestamp":${System.currentTimeMillis() / 1000}$iconField}"""
            broker.send(
                AnchorEvent(
                    target = AnchorTarget.Device,
                    message = AnchorMessage.Json(json)
                )
            )
        }
        logListenerState("start-after-callback")

        // Listen for incoming desktop notifications
        listenJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleIncoming(event)
                }
            }
        }

        Log.i(TAG, "NotificationPlugin (foghorn) started")
    }

    /** Attach the negotiated SDK notification capability for typed delivery. */
    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        // The capability retains the session; the parameter documents that
        // this is tied to the current authenticated connection.
        sdkCapability = capability
        Log.i(TAG, "SDK notifications capability attached (session=${capability.sessionId})")
    }

    private fun handleIncoming(event: AnchorEvent) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return

        try {
            val json = Json.parseToJsonElement(msg.payload).jsonObject
            val type = json["type"]?.jsonPrimitive?.content ?: return
            if (type != "notification") return

            val source = json["source"]?.jsonPrimitive?.content ?: return
            // Only show notifications from the desktop
            if (source != "desktop") return

            // Respect the user's "show desktop notifications" toggle.
            if (!prefs.getBoolean("notif_receive_enabled", true)) {
                Log.d(TAG, "Foghorn: receive disabled, dropping desktop notification")
                return
            }

            // Don't show if user is actively streaming (they can see the desktop)
            if (suppressNotifications.get()) {
                Log.d(TAG, "Foghorn: suppressed notification (streaming active)")
                return
            }

            val appName = json["app_name"]?.jsonPrimitive?.content ?: "Desktop"
            val title = json["title"]?.jsonPrimitive?.content ?: ""
            val body = json["body"]?.jsonPrimitive?.content ?: ""

            Log.i(TAG, "Foghorn: desktop notification — $appName: $title")
            showNotification(appName, title, body)
        } catch (e: Exception) {
            Log.e(TAG, "Foghorn: failed to parse notification: ${e.message}")
        }
    }

    private fun showNotification(appName: String, title: String, body: String) {
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle("$appName — $title")
            .setContentText(body)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .setAutoCancel(true)
            .build()

        nm.notify(notificationId.incrementAndGet(), notification)
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                CHANNEL_NAME,
                NotificationManager.IMPORTANCE_DEFAULT
            ).apply {
                description = "Notifications forwarded from your desktop"
            }
            val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            nm.createNotificationChannel(channel)
        }
    }

    override fun stop() {
        logListenerState("stop-before-clear")
        AnchorNotificationListener.onNotificationPostedCallback = null
        sdkCapability = null
        listenJob?.cancel()
        listenJob = null
    }

    private fun logListenerState(reason: String) {
        val flat = Settings.Secure.getString(
            context.contentResolver,
            "enabled_notification_listeners"
        ) ?: ""
        val component = ComponentName(context, AnchorNotificationListener::class.java).flattenToString()
        val enabled = flat.split(':').any { it == component }
        Log.i(
            TAG,
            "Foghorn listener state [$reason]: enabled=$enabled callbackSet=${AnchorNotificationListener.onNotificationPostedCallback != null} component=$component"
        )
    }
}
