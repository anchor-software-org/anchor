package com.anchor.plugin

import android.app.Notification
import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.BitmapDrawable
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Base64
import android.util.Log
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executors

private const val TAG = "anchor"
private const val DEDUP_WINDOW_MS = 5_000L

/**
 * System-level notification listener. Requires user to grant access in
 * Settings -> Notifications -> Notification access -> Anchor.
 *
 * When a notification is posted, it forwards the details to NotificationPlugin
 * via a static callback. NotificationPlugin then sends it to the desktop.
 */
class AnchorNotificationListener : NotificationListenerService() {

    companion object {
        /** Static callback set by NotificationPlugin when it starts.
         *  Parameters: (packageName, appName, title, body, iconBase64OrNull) */
        var onNotificationPostedCallback: ((String, String, String, String, String?) -> Unit)? = null

        fun isNotificationAccessEnabled(context: Context): Boolean {
            val flat = Settings.Secure.getString(
                context.contentResolver,
                "enabled_notification_listeners"
            ) ?: return false
            val expected = ComponentName(context, AnchorNotificationListener::class.java).flattenToString()
            return flat.split(':').any { it == expected }
        }
    }

    // Dedup: pkg -> (title, body, sentAt)
    private val lastSent = mutableMapOf<String, Triple<String, String, Long>>()
    private val iconExecutor = Executors.newSingleThreadExecutor()

    override fun onCreate() {
        super.onCreate()
        Log.i(
            TAG,
            "Foghorn listener created; accessEnabled=${isNotificationAccessEnabled(applicationContext)} callbackSet=${onNotificationPostedCallback != null}"
        )
    }

    override fun onListenerConnected() {
        super.onListenerConnected()
        Log.i(
            TAG,
            "Foghorn listener connected; accessEnabled=${isNotificationAccessEnabled(applicationContext)} callbackSet=${onNotificationPostedCallback != null}"
        )
    }

    override fun onListenerDisconnected() {
        Log.w(
            TAG,
            "Foghorn listener disconnected; accessEnabled=${isNotificationAccessEnabled(applicationContext)} callbackSet=${onNotificationPostedCallback != null}"
        )
        super.onListenerDisconnected()
    }

    override fun onDestroy() {
        Log.w(
            TAG,
            "Foghorn listener destroyed; accessEnabled=${isNotificationAccessEnabled(applicationContext)} callbackSet=${onNotificationPostedCallback != null}"
        )
        iconExecutor.shutdownNow()
        super.onDestroy()
    }

    override fun onNotificationPosted(sbn: StatusBarNotification?) {
        if (sbn == null) return

        Log.d(
            TAG,
            "Foghorn listener event pkg=${sbn.packageName} id=${sbn.id} tag=${sbn.tag ?: ""} callbackSet=${onNotificationPostedCallback != null}"
        )

        // Skip notifications posted by foghorn itself (desktop → phone) to prevent loops.
        // Allow other Anchor notifications through (e.g. test notifications).
        val notification = sbn.notification ?: return
        if (sbn.packageName == applicationContext.packageName &&
            android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O &&
            (notification.channelId == "anchor_desktop_notifications" ||
             notification.channelId == "anchor_media")) {
            Log.d(TAG, "Foghorn: skipping Anchor self-notification (channel=${notification.channelId})")
            return
        }
        val extras = notification.extras ?: return

        val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString() ?: ""
        val body = extras.getCharSequence(Notification.EXTRA_TEXT)?.toString() ?: ""

        // Skip empty notifications
        if (title.isEmpty() && body.isEmpty()) {
            Log.d(TAG, "Foghorn: skipping empty notification from ${sbn.packageName}")
            return
        }

        // Deduplicate: drop if same content from same app within the window.
        val now = System.currentTimeMillis()
        val last = lastSent[sbn.packageName]
        if (last != null && last.first == title && last.second == body && now - last.third < DEDUP_WINDOW_MS) {
            Log.d(TAG, "Foghorn: deduped notification from ${sbn.packageName}: $title")
            return
        }
        lastSent[sbn.packageName] = Triple(title, body, now)

        val appName = try {
            packageManager.getApplicationLabel(
                packageManager.getApplicationInfo(sbn.packageName, 0)
            ).toString()
        } catch (_: Exception) {
            sbn.packageName
        }

        iconExecutor.execute {
            val iconBase64 = try {
                val drawable = packageManager.getApplicationIcon(sbn.packageName)
                val bitmap = if (drawable is BitmapDrawable) {
                    drawable.bitmap
                } else {
                    val bmp = Bitmap.createBitmap(
                        drawable.intrinsicWidth.coerceAtLeast(1),
                        drawable.intrinsicHeight.coerceAtLeast(1),
                        Bitmap.Config.ARGB_8888
                    )
                    val canvas = Canvas(bmp)
                    drawable.setBounds(0, 0, canvas.width, canvas.height)
                    drawable.draw(canvas)
                    bmp
                }
                val out = ByteArrayOutputStream()
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
                Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP)
            } catch (_: Exception) {
                null
            }

            Log.i(TAG, "Foghorn: local notification from $appName: $title")
            if (onNotificationPostedCallback == null) {
                Log.w(TAG, "Foghorn: dropping notification because callback is not set")
            }
            onNotificationPostedCallback?.invoke(sbn.packageName, appName, title, body, iconBase64)
        }
    }
}
