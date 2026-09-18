package com.anchor.plugin

import android.os.SystemClock
import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.InputProtocol
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.ConcurrentHashMap

private const val TAG = "anchor.input"
private const val TIMING_TAG = "anchor.timing"

class InputPlugin(private val broker: MessageBroker) : Plugin {
    override val pluginId = "input"
    private val outboundSeq = AtomicLong(0)
    private var lastMotionEnqueueNs = 0L
    private var sdkScope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null
    // Keep host-button state across a reconnect. If the transport disappears
    // between a press and its release, the next session clears the stale
    // button before accepting new pointer input.
    private val sdkPressedButtons = ConcurrentHashMap.newKeySet<Int>()
    // Keep pointer motion on its own lane. A high-rate motion stream must not
    // queue a click release behind hundreds of stale motion writes; that was
    // observable as a held button when the touch surface was dragged.
    // Control transitions remain strictly ordered, so press/release and key
    // chords cannot be reordered by parallel IO workers.
    private val sdkMotionDispatcher = Dispatchers.IO.limitedParallelism(1)
    private val sdkControlDispatcher = Dispatchers.IO.limitedParallelism(1)

    override fun start(scope: CoroutineScope) { sdkScope = scope }
    override fun stop() { sdkCapability = null }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK input capability attached (session=${capability.sessionId})")
        val staleButtons = sdkPressedButtons.toList()
        if (staleButtons.isNotEmpty()) {
            sdkScope?.launch(sdkControlDispatcher) {
                staleButtons.forEach { button ->
                    val value = sdkButtonValue(button) ?: return@forEach
                    runCatching {
                        capability.sendRecord(
                            InputProtocol.POINTER_BUTTON_TYPE_URL,
                            InputProtocol.encodePointerButton(value, false),
                        )
                    }.onSuccess {
                        sdkPressedButtons.remove(button)
                        Log.w(TAG, "Released stale SDK input button after reconnect: $button")
                    }.onFailure { error ->
                        Log.w(TAG, "Failed to release stale SDK input button $button: ${error.message}")
                    }
                }
            }
        }
    }

    fun sendMotion(dx: Float, dy: Float) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.motion")
            put("dx", dx.toDouble())
            put("dy", dy.toDouble())
            put("time", System.currentTimeMillis() and 0xFFFFFFFFL)
        })
    }

    fun sendMotionAbsolute(x: Float, y: Float) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.motion_absolute")
            put("x", x.toDouble())
            put("y", y.toDouble())
            put("time", System.currentTimeMillis() and 0xFFFFFFFFL)
        })
    }

    fun sendButton(button: Int = BTN_LEFT, pressed: Boolean) {
        if (pressed) sdkPressedButtons.add(button)
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.button")
            put("button", button)
            put("state", if (pressed) 1 else 0)
            put("time", System.currentTimeMillis() and 0xFFFFFFFFL)
        })
    }

    fun sendAxis(axis: Int = AXIS_VERTICAL, value: Float) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.axis")
            put("axis", axis)
            put("value", value.toDouble())
            put("time", System.currentTimeMillis() and 0xFFFFFFFFL)
        })
    }

    fun sendText(text: String) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.text")
            put("text", text) // JSONObject handles escaping
        })
    }

    fun sendKey(key: String) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.key")
            put("key", key)
        })
    }

    fun sendKeyCombo(modifiers: List<String>, key: String) {
        send(JSONObject().apply {
            put("plugin_id", "input")
            put("type", "anchor.input.key_combo")
            put("modifiers", JSONArray(modifiers))
            put("key", key)
        })
    }

    private fun send(json: JSONObject) {
        val seq = outboundSeq.incrementAndGet()
        val nowMs = SystemClock.elapsedRealtime()
        val nowNs = System.nanoTime()
        val type = json.optString("type", "unknown")
        json.put("client_seq", seq)
        json.put("client_elapsed_ms", nowMs)

        if (type == "anchor.input.motion_absolute") {
            val gapMs = if (lastMotionEnqueueNs == 0L) 0.0 else (nowNs - lastMotionEnqueueNs) / 1_000_000.0
            lastMotionEnqueueNs = nowNs
            if (seq % 30L == 0L || gapMs > 40.0) {
                Log.d(
                    TIMING_TAG,
                    "[input] enqueue type=$type seq=$seq gap_ms=${"%.1f".format(gapMs)} x=${json.optDouble("x", -1.0)} y=${json.optDouble("y", -1.0)}"
                )
            }
        } else {
            Log.d(TIMING_TAG, "[input] enqueue type=$type seq=$seq")
        }

        val sdk = sdkCapability
        if (sdk != null) {
            val operations: List<Pair<String, ByteArray>> = when (type) {
                "anchor.input.motion" -> listOf(InputProtocol.POINTER_RELATIVE_TYPE_URL to
                    InputProtocol.encodePointerRelative(
                        json.optDouble("dx", 0.0).toFloat(),
                        json.optDouble("dy", 0.0).toFloat(),
                    ))
                "anchor.input.motion_absolute" -> listOf(InputProtocol.POINTER_ABSOLUTE_TYPE_URL to
                    InputProtocol.encodePointerAbsolute(
                        (json.optDouble("x", 0.0).let { if (it <= 1.0) it * 65535 else it }).toInt(),
                        (json.optDouble("y", 0.0).let { if (it <= 1.0) it * 65535 else it }).toInt(),
                    ))
                "anchor.input.button" -> listOf(InputProtocol.POINTER_BUTTON_TYPE_URL to
                    InputProtocol.encodePointerButton(
                        when (json.optInt("button", BTN_LEFT)) {
                            BTN_LEFT -> InputProtocol.POINTER_BUTTON_LEFT
                            BTN_MIDDLE -> InputProtocol.POINTER_BUTTON_MIDDLE
                            BTN_RIGHT -> InputProtocol.POINTER_BUTTON_RIGHT
                            BTN_BACK -> InputProtocol.POINTER_BUTTON_BACK
                            BTN_FORWARD -> InputProtocol.POINTER_BUTTON_FORWARD
                            else -> InputProtocol.POINTER_BUTTON_UNSPECIFIED
                        },
                        json.optInt("state", 0) != 0,
                    ))
                "anchor.input.axis" -> listOf(InputProtocol.SCROLL_TYPE_URL to
                    InputProtocol.encodeScroll(
                        if (json.optInt("axis", AXIS_VERTICAL) == AXIS_HORIZONTAL) json.optDouble("value", 0.0).toInt() else 0,
                        if (json.optInt("axis", AXIS_VERTICAL) == AXIS_VERTICAL) json.optDouble("value", 0.0).toInt() else 0,
                    ))
                "anchor.input.text" -> listOf(InputProtocol.TEXT_TYPE_URL to InputProtocol.encodeText(json.optString("text")))
                "anchor.input.key" -> InputProtocol.hidUsageForKey(json.optString("key"))?.let { hid ->
                    listOf(
                        InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(hid, true),
                        InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(hid, false),
                    )
                } ?: emptyList()
                "anchor.input.key_combo" -> {
                    val modifiers = json.optJSONArray("modifiers")?.let { array ->
                        (0 until array.length()).mapNotNull { InputProtocol.hidUsageForKey(array.optString(it)) }
                    } ?: emptyList()
                    val key = InputProtocol.hidUsageForKey(json.optString("key"))
                    if (key == null) emptyList() else buildList {
                        modifiers.forEach { add(InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(it, true)) }
                        add(InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(key, true))
                        add(InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(key, false))
                        modifiers.asReversed().forEach { add(InputProtocol.KEY_TYPE_URL to InputProtocol.encodeKey(it, false)) }
                    }
                }
                else -> emptyList()
            }
            if (operations.isNotEmpty()) {
                val dispatcher = if (type == "anchor.input.motion" ||
                    type == "anchor.input.motion_absolute") {
                    sdkMotionDispatcher
                } else {
                    sdkControlDispatcher
                }
                sdkScope?.launch(dispatcher) {
                    runCatching {
                        operations.forEach { (typeUrl, payload) -> sdk.sendRecord(typeUrl, payload) }
                    }.onSuccess {
                        Log.d(TAG, "SDK input sent type=$type operations=${operations.size}")
                        if (type == "anchor.input.button" && json.optInt("state", 0) == 0) {
                            sdkPressedButtons.remove(json.optInt("button", BTN_LEFT))
                        }
                    }.onFailure { error -> Log.w(TAG, "SDK input send failed: ${error.message}") }
                }
                return
            }
        }

        broker.send(AnchorEvent(
            target = AnchorTarget.Device,
            message = AnchorMessage.Json(json.toString())
        ))
    }

    private fun sdkButtonValue(button: Int): Int? = when (button) {
        BTN_LEFT -> InputProtocol.POINTER_BUTTON_LEFT
        BTN_MIDDLE -> InputProtocol.POINTER_BUTTON_MIDDLE
        BTN_RIGHT -> InputProtocol.POINTER_BUTTON_RIGHT
        BTN_BACK -> InputProtocol.POINTER_BUTTON_BACK
        BTN_FORWARD -> InputProtocol.POINTER_BUTTON_FORWARD
        else -> null
    }

    companion object {
        const val BTN_LEFT = 272
        const val BTN_RIGHT = 273
        const val BTN_MIDDLE = 274
        const val BTN_BACK = 275
        const val BTN_FORWARD = 276
        const val AXIS_VERTICAL = 0
        const val AXIS_HORIZONTAL = 1
    }
}
