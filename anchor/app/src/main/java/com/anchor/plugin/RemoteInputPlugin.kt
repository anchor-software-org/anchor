package com.anchor.plugin

import android.accessibilityservice.AccessibilityService
import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.InputProtocol
import org.json.JSONObject

private const val TAG = "anchor.remoteinput"

/**
 * Remote input plugin — receives desktop → phone pointer/key/text events and
 * drives the device through AnchorAccessibilityService.
 *
 * This is the receive direction of the existing `org.anchor.input`
 * capability: the desktop encodes the same InputProtocol messages the phone
 * sends when acting as a touchpad. Absolute pointer coords arrive normalized
 * to [0, 65535] and are mapped onto the default display.
 *
 * Everything is a no-op while the accessibility service is disabled; callers
 * should check AnchorAccessibilityService.isEnabled() before advertising the
 * capability.
 */
class RemoteInputPlugin(private val broker: MessageBroker) : Plugin {

    override val pluginId = "remoteinput"

    private var listenJob: Job? = null
    private var sdkScope: CoroutineScope? = null
    private var sdkCapability: AnchorCapability? = null

    override fun start(scope: CoroutineScope) {
        sdkScope = scope
        listenJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleIncoming(event)
                }
            }
        }
        Log.i(TAG, "RemoteInputPlugin started")
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
        sdkCapability = null
    }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK remote-input capability attached (session=${capability.sessionId})")
    }

    /** Called by the connection's single SDK event reader. */
    fun handleSdkRecord(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event !is AnchorSessionEvent.CapabilityRecord ||
            event.capabilitySessionId != capability.sessionId
        ) return

        runCatching { dispatchSdkRecord(event.typeUrl, event.payload) }
            .onFailure { error -> Log.w(TAG, "SDK input record rejected: ${error.message}") }
    }

    private fun dispatchSdkRecord(typeUrl: String, payload: ByteArray) {
        when (typeUrl) {
            InputProtocol.POINTER_ABSOLUTE_TYPE_URL -> {
                val p = InputProtocol.decodePointerAbsolute(payload)
                moveAbsolute(p.x, p.y)
            }
            InputProtocol.POINTER_BUTTON_TYPE_URL -> {
                val b = InputProtocol.decodePointerButton(payload)
                if (b.buttonValue == InputProtocol.POINTER_BUTTON_LEFT) {
                    val svc = service() ?: return
                    if (b.pressed) svc.pointerDown(svc.pointerPosition().first, svc.pointerPosition().second)
                    else svc.pointerUp()
                }
            }
            InputProtocol.SCROLL_TYPE_URL -> {
                val s = InputProtocol.decodeScroll(payload)
                // Wheel units are 1/120th detents; ~120 px per detent.
                service()?.scroll(s.horizontal120Ths.toFloat(), s.vertical120Ths.toFloat())
            }
            InputProtocol.TEXT_TYPE_URL -> {
                service()?.commitText(InputProtocol.decodeText(payload).textUtf8)
            }
            InputProtocol.KEY_TYPE_URL -> {
                val k = InputProtocol.decodeKey(payload)
                service()?.hidKey(k.hidUsage, k.pressed)
            }
        }
    }

    // ── Legacy broker JSON path ────────────────────────────────────────

    private fun handleIncoming(event: AnchorEvent) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return
        val json = try {
            JSONObject(msg.payload)
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse JSON: ${e.message}")
            return
        }
        val svc = service() ?: return

        when (json.optString("type")) {
            "remote_tap" -> svc.tap(
                denorm(json.optDouble("x", 0.0), svc.displaySize().first),
                denorm(json.optDouble("y", 0.0), svc.displaySize().second)
            )
            "remote_pointer_down" -> svc.pointerDown(
                denorm(json.optDouble("x", 0.0), svc.displaySize().first),
                denorm(json.optDouble("y", 0.0), svc.displaySize().second)
            )
            "remote_pointer_move" -> svc.pointerMove(
                denorm(json.optDouble("x", 0.0), svc.displaySize().first),
                denorm(json.optDouble("y", 0.0), svc.displaySize().second)
            )
            "remote_pointer_up" -> svc.pointerUp()
            "remote_scroll" -> svc.scroll(
                json.optDouble("dx", 0.0).toFloat(),
                json.optDouble("dy", 0.0).toFloat()
            )
            "remote_text" -> svc.commitText(json.optString("text"))
            "remote_back" -> svc.globalAction(AccessibilityService.GLOBAL_ACTION_BACK)
            "remote_home" -> svc.globalAction(AccessibilityService.GLOBAL_ACTION_HOME)
            "remote_recents" -> svc.globalAction(AccessibilityService.GLOBAL_ACTION_RECENTS)
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    private fun moveAbsolute(nx: Int, ny: Int) {
        val svc = service() ?: return
        val (w, h) = svc.displaySize()
        val x = nx.toFloat() / 65535f * (w - 1)
        val y = ny.toFloat() / 65535f * (h - 1)
        // A move while a button is held continues the drag; with no button
        // held it is just a hover position update (gestures only inject on
        // contact, matching dispatchGesture semantics).
        svc.pointerMove(x, y)
    }

    /** JSON path sends normalized [0,1]; map to display pixels. */
    private fun denorm(v: Double, max: Int): Float =
        (v.toFloat() * (max - 1)).coerceIn(0f, (max - 1).toFloat())

    private fun service(): AnchorAccessibilityService? {
        val svc = AnchorAccessibilityService.get()
        if (svc == null) Log.d(TAG, "dropping input: accessibility service not enabled")
        return svc
    }
}
