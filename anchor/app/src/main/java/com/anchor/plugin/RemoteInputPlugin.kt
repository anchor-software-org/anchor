package com.anchor.plugin

import android.accessibilityservice.AccessibilityService
import android.graphics.Bitmap
import android.os.SystemClock
import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executors
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorDatagramFlow
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.InputProtocol
import org.anchor.sdk.VideoFrameProtocol
import org.json.JSONObject

private const val TAG = "anchor.remoteinput"
private const val MIRROR_JPEG_QUALITY = 55
private const val MIRROR_MAX_WIDTH = 720
private const val MIRROR_INTERVAL_MS = 350L

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
    private var mirrorJob: Job? = null
    private val screenshotExecutor = Executors.newSingleThreadExecutor()

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
        mirrorJob?.cancel()
        mirrorJob = null
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
            InputProtocol.REMOTE_CONTROL_TYPE_URL -> {
                when (InputProtocol.decodeRemoteControl(payload)) {
                    InputProtocol.REMOTE_CONTROL_MIRROR_START -> startMirror()
                    InputProtocol.REMOTE_CONTROL_MIRROR_STOP -> stopMirror()
                    InputProtocol.REMOTE_CONTROL_BACK ->
                        service()?.globalAction(AccessibilityService.GLOBAL_ACTION_BACK)
                    InputProtocol.REMOTE_CONTROL_HOME ->
                        service()?.globalAction(AccessibilityService.GLOBAL_ACTION_HOME)
                    InputProtocol.REMOTE_CONTROL_RECENTS ->
                        service()?.globalAction(AccessibilityService.GLOBAL_ACTION_RECENTS)
                }
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
            "remote_mirror" ->
                if (json.optBoolean("enabled", true)) startMirror() else stopMirror()
        }
    }

    // ── Screen mirror (accessibility stills path) ──────────────────────
    //
    // Phase 1 of remote screen mirroring: AccessibilityService.takeScreenshot
    // JPEG stills over an ANFR datagram flow labelled
    // InputProtocol.MIRROR_FRAME_TYPE_URL. Zero consent beyond the a11y
    // service itself — MediaProjection/H.264 replaces the producer later
    // without changing the wire path.

    /** Debug/test hook: toggle the stills mirror without a desktop record. */
    fun debugSetMirror(enabled: Boolean) {
        if (enabled) startMirror() else stopMirror()
    }

    private fun startMirror() {
        if (mirrorJob?.isActive == true) return
        val capability = sdkCapability ?: run {
            Log.w(TAG, "mirror requested with no SDK input capability")
            return
        }
        if (service() == null) {
            Log.w(TAG, "mirror requested but accessibility service is not enabled")
            return
        }
        val scope = sdkScope ?: return
        mirrorJob = scope.launch {
            val flow = try {
                capability.openDatagramFlow(InputProtocol.MIRROR_FRAME_TYPE_URL)
            } catch (e: Exception) {
                Log.w(TAG, "mirror datagram flow open failed: ${e.message}")
                return@launch
            }
            Log.i(TAG, "mirror flow opened (flow=${flow.flowId}, session=${capability.sessionId})")
            var sequence = 0L
            var sendFailures = 0
            try {
                while (isActive) {
                    val jpeg = grabJpeg()
                    if (jpeg != null) {
                        sendFailures = if (
                            sendJpegFrame(flow, capability.sessionId, sequence++, jpeg)
                        ) 0 else sendFailures + 1
                        // A dead session fails every send — stop retrying.
                        if (sendFailures >= 5) {
                            Log.w(TAG, "mirror stopping: datagram flow is failing")
                            break
                        }
                    }
                    delay(MIRROR_INTERVAL_MS)
                }
            } finally {
                runCatching { flow.close() }
                Log.i(TAG, "mirror flow closed")
            }
        }
    }

    private fun stopMirror() {
        mirrorJob?.cancel()
        mirrorJob = null
    }

    private suspend fun grabJpeg(): ByteArray? {
        val svc = service() ?: return null
        val bitmap = try {
            suspendCancellableCoroutine<Bitmap?> { cont ->
                svc.grabScreenshot(screenshotExecutor, object :
                    AccessibilityService.TakeScreenshotCallback {
                    override fun onSuccess(result: AccessibilityService.ScreenshotResult) {
                        val hardware = Bitmap.wrapHardwareBuffer(
                            result.hardwareBuffer, result.colorSpace
                        )
                        result.hardwareBuffer.close()
                        cont.resume(hardware?.copy(Bitmap.Config.ARGB_8888, false))
                    }

                    override fun onFailure(errorCode: Int) {
                        cont.resumeWithException(
                            IllegalStateException("takeScreenshot failed: $errorCode")
                        )
                    }
                })
            }
        } catch (e: Exception) {
            Log.w(TAG, "screenshot failed: ${e.message}")
            return null
        } ?: return null
        return try {
            val scaled = if (bitmap.width > MIRROR_MAX_WIDTH) {
                val h = bitmap.height * MIRROR_MAX_WIDTH / bitmap.width
                Bitmap.createScaledBitmap(bitmap, MIRROR_MAX_WIDTH, h, true)
            } else bitmap
            val out = ByteArrayOutputStream(96 * 1024)
            scaled.compress(Bitmap.CompressFormat.JPEG, MIRROR_JPEG_QUALITY, out)
            out.toByteArray()
        } finally {
            bitmap.recycle()
        }
    }

    private fun sendJpegFrame(
        flow: AnchorDatagramFlow,
        capabilitySessionId: Long,
        sequence: Long,
        jpeg: ByteArray,
    ): Boolean {
        val packets = VideoFrameProtocol.fragmentWithParity(
            VideoFrameProtocol.KIND_SCREEN,
            VideoFrameProtocol.FLAG_KEYFRAME,
            capabilitySessionId,
            flow.flowId,
            sequence,
            SystemClock.elapsedRealtimeNanos() / 1000,
            0,
            jpeg,
        )
        var sent = 0
        for (packet in packets) {
            val ok = runCatching { flow.send(packet) }.isSuccess
            if (ok) sent++ else Log.w(TAG, "mirror datagram dropped")
        }
        return sent > 0
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
