package com.anchor.plugin

import android.accessibilityservice.AccessibilityService
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.os.SystemClock
import android.util.Log
import androidx.core.app.NotificationCompat
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

private const val TAG = "anchor.remoteinput"
private const val MIRROR_JPEG_QUALITY = 55
private const val MIRROR_MAX_WIDTH = 720
private const val MIRROR_INTERVAL_MS = 80L
private const val CONSENT_CHANNEL_ID = "anchor_capture"
private const val CONSENT_NOTIFICATION_ID = 41
private const val STALE_FRAME_US = 150_000L
private const val KEYFRAME_REQUEST_MIN_NS = 400_000_000L
private const val CONSENT_WATCHDOG_MS = 60_000L

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
class RemoteInputPlugin(
    private val context: Context,
) : Plugin {

    override val pluginId = "remoteinput"

    private var sdkScope: CoroutineScope? = null
    private var sdkCapability: AnchorCapability? = null
    private var mirrorJob: Job? = null
    /** True once the desktop asked for the mirror; drives auto-start on consent. */
    @Volatile private var mirrorWanted = false
    @Volatile private var consentPending = false
    private var videoFlow: AnchorDatagramFlow? = null
    private var videoSequence = 0L
    private var lastKeyRequestNs = 0L
    private var droppedVideoFrames = 0L
    @Volatile private var mirrorCaptureMs = -1L
    @Volatile private var mirrorEncodeMs = -1L
    private val screenshotExecutor = Executors.newSingleThreadExecutor()

    override fun start(scope: CoroutineScope) {
        sdkScope = scope
        Log.i(TAG, "RemoteInputPlugin started")
    }

    override fun stop() {
        sdkCapability = null
        stopMirror()
    }

    /**
     * Transport teardown — the QUIC session is gone, so the remote-control
     * request ends with it: drop the dead flow, detach the encoder sink, and
     * release the projection instead of zombie-encoding into a black hole.
     * The next session must be asked to mirror again explicitly.
     */
    fun onTransportStopped() {
        if (!mirrorWanted && videoFlow == null && mirrorJob == null && sdkCapability == null) return
        Log.i(TAG, "transport stopped — tearing down remote mirror")
        sdkCapability = null
        stopMirror()
    }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkCapability = capability
        videoFlow = null
        // A projection can outlive the session — reattach its sink so the
        // mirror resumes without a new consent dialog.
        val service = ScreenCaptureService.instance
        if (mirrorWanted && service != null) {
            startVideoMirror(capability, service)
        }
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
            InputProtocol.PING_TYPE_URL -> {
                // Latency probe — echo with our own monotonic timestamp so the
                // desktop can age mirror frames stamped on this clock.
                val pingSentUs = InputProtocol.decodePing(payload)
                val cap = sdkCapability ?: return
                sdkScope?.launch {
                    runCatching {
                        cap.sendRecord(
                            InputProtocol.PONG_TYPE_URL,
                            InputProtocol.encodePong(
                                pingSentUs,
                                SystemClock.elapsedRealtimeNanos() / 1000,
                            ),
                        )
                    }
                }
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

    // ── Screen mirror ────────────────────────────────────────────────────
    //
    // Mirror frames ride an ANFR datagram flow labelled
    // InputProtocol.MIRROR_FRAME_TYPE_URL. Two producers feed it:
    //   - MediaProjection → MediaCodec H.264 (primary, one consent tap)
    //   - AccessibilityService.takeScreenshot JPEG stills (fallback)

    /** Debug/test hook: toggle the mirror without a desktop record. */
    fun debugSetMirror(enabled: Boolean) {
        if (enabled) startMirror() else stopMirror()
    }

    private fun startMirror() {
        mirrorWanted = true
        if (videoFlow != null || mirrorJob?.isActive == true) return
        val capability = requireCapability() ?: return
        val service = ScreenCaptureService.instance
        if (service != null) {
            // Video supersedes stills — both ride the same flow type, so the
            // stills loop must stop before the H.264 sink attaches.
            mirrorJob?.cancel()
            mirrorJob = null
            startVideoMirror(capability, service)
            return
        }
        // No projection token — ask for consent once; the video mirror starts
        // automatically when the grant lands. JPEG stills need the a11y
        // service, video doesn't.
        requestCaptureConsent()
    }

    /** Called by ScreenCaptureConsentActivity with the user's choice. */
    fun onConsentResult(granted: Boolean) {
        consentPending = false
        if (!granted) {
            Log.i(TAG, "projection consent denied — falling back to a11y stills")
            if (mirrorWanted) startStillsMirror()
            return
        }
        // The consent trampoline hands the token to ScreenCaptureService;
        // wait for it to come up, then start the video mirror.
        val capability = sdkCapability ?: return
        sdkScope?.launch {
            var waited = 0
            while (ScreenCaptureService.instance == null && waited < 50) {
                delay(100)
                waited++
            }
            val service = ScreenCaptureService.instance
            if (service != null && mirrorWanted) {
                startVideoMirror(capability, service)
            } else if (service == null) {
                Log.w(TAG, "capture service never started after consent")
            }
        }
    }

    private fun requestCaptureConsent() {
        if (consentPending) return
        consentPending = true
        // The trampoline is noHistory — if it's interrupted (another app
        // foregrounds, user dismisses) it dies without delivering a result.
        // Watchdog so consentPending can't wedge the mirror toggle forever.
        sdkScope?.launch {
            delay(CONSENT_WATCHDOG_MS)
            if (consentPending) {
                consentPending = false
                Log.w(TAG, "consent result watchdog expired — re-arming mirror requests")
            }
        }
        val intent = Intent(context, com.anchor.ScreenCaptureConsentActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { context.startActivity(intent) }
            .onSuccess { Log.i(TAG, "capture consent activity launched") }
            .onFailure { error ->
                // Background activity launch was blocked — offer the consent
                // trigger as a notification tap instead.
                Log.i(TAG, "direct consent launch blocked ($error); posting notification")
                postConsentNotification(intent)
            }
    }

    private fun postConsentNotification(consentIntent: Intent) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(
                CONSENT_CHANNEL_ID, "Screen mirroring", NotificationManager.IMPORTANCE_HIGH,
            )
        )
        val pending = PendingIntent.getActivity(
            context, 0, consentIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = NotificationCompat.Builder(context, CONSENT_CHANNEL_ID)
            .setContentTitle("Anchor wants to mirror this screen")
            .setContentText("Tap to allow screen capture")
            .setSmallIcon(android.R.drawable.presence_video_online)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .build()
        runCatching { nm.notify(CONSENT_NOTIFICATION_ID, notification) }
    }

    // ── Screen mirror (MediaProjection + H.264) ─────────────────────────
    //
    // Phase 2: VirtualDisplay feeds a MediaCodec surface encoder; each Annex-B
    // access unit rides the same ANFR datagram flow as the stills path. The
    // service owns the projection; this plugin owns the wire.

    private fun startVideoMirror(
        capability: AnchorCapability,
        service: ScreenCaptureService,
    ) {
        val scope = sdkScope ?: return
        scope.launch {
            val flow = videoFlow ?: runCatching {
                capability.openDatagramFlow(InputProtocol.MIRROR_FRAME_TYPE_URL)
            }.getOrElse { error ->
                Log.w(TAG, "video mirror flow open failed: ${error.message}")
                return@launch
            }
            videoFlow = flow
            videoSequence = 0
            Log.i(TAG, "video mirror flow opened (flow=${flow.flowId}, session=${capability.sessionId})")
            service.frameSink = sink@{ data, isKey, isConfig, ptsUs ->
                // Bounded latency: if the wire fell behind (sendDatagram
                // blocks the encoder thread when msquic's queue is deep), a
                // stale P-frame is worthless — drop it and request a fresh
                // keyframe so the decoder recovers instead of artifacting.
                val ageUs = SystemClock.elapsedRealtimeNanos() / 1_000 - ptsUs
                if (!isKey && !isConfig && ageUs > STALE_FRAME_US) {
                    val now = SystemClock.elapsedRealtimeNanos()
                    if (now - lastKeyRequestNs > KEYFRAME_REQUEST_MIN_NS) {
                        lastKeyRequestNs = now
                        service.requestKeyFrame()
                    }
                    droppedVideoFrames++
                    if (droppedVideoFrames % 90L == 1L) {
                        Log.w(TAG, "video mirror dropping stale frames (total=$droppedVideoFrames)")
                    }
                    return@sink
                }
                sendVideoFrame(flow, capability.sessionId, data, isKey, isConfig, ptsUs)
            }
            // The codec-config AU may have fired before the flow attached —
            // replay it, and ask for an IDR too (sync requests are advisory
            // on some encoders, so both paths are armed).
            service.emitConfigFrame()
            service.requestKeyFrame()
            service.onProjectionStopped = {
                Log.i(TAG, "projection stopped — closing video mirror flow")
                videoFlow = null
                scope.launch { runCatching { flow.close() } }
            }
        }
    }

    /** Fragment a frame onto the wire; returns how many packets landed. */
    private fun sendFramePackets(
        flow: AnchorDatagramFlow,
        capabilitySessionId: Long,
        sequence: Long,
        presentationTimeUs: Long,
        codecConfigId: Long,
        flags: Int,
        data: ByteArray,
    ): Int {
        val packets = runCatching {
            VideoFrameProtocol.fragmentWithParity(
                kind = VideoFrameProtocol.KIND_SCREEN,
                flags = flags,
                capabilitySessionId = capabilitySessionId,
                flowId = flow.flowId,
                sequence = sequence,
                presentationTimeUs = presentationTimeUs,
                codecConfigId = codecConfigId,
                payload = data,
            )
        }.getOrElse { error ->
            Log.w(TAG, "mirror fragment failed: ${error.message}")
            return 0
        }
        var sent = 0
        for (packet in packets) {
            if (runCatching { flow.send(packet) }.isSuccess) sent++
            else Log.w(TAG, "mirror datagram dropped")
        }
        return sent
    }

    private fun sendVideoFrame(
        flow: AnchorDatagramFlow,
        capabilitySessionId: Long,
        data: ByteArray,
        isKey: Boolean,
        isConfig: Boolean,
        presentationTimeUs: Long,
    ) {
        val flags = (if (isKey) VideoFrameProtocol.FLAG_KEYFRAME else 0) or
            (if (isConfig) VideoFrameProtocol.FLAG_CODEC_CONFIG else 0)
        val sequence = videoSequence++
        sendFramePackets(flow, capabilitySessionId, sequence, presentationTimeUs, 1, flags, data)
        if (sequence % 90L == 0L) {
            Log.i(TAG, "video mirror frames sent: sequence=$sequence bytes=${data.size} key=$isKey")
        }
    }

    // ── Screen mirror (accessibility stills path) ──────────────────────
    //
    // Fallback when projection consent is denied or unavailable: JPEG stills
    // via AccessibilityService.takeScreenshot on the same wire format.

    private fun startStillsMirror() {
        if (mirrorJob?.isActive == true) return
        val capability = requireCapability() ?: return
        if (service() == null) {
            Log.w(TAG, "stills mirror requested but accessibility service is not enabled")
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
                    val captureStartUs = SystemClock.elapsedRealtimeNanos() / 1000
                    val jpeg = grabJpeg()
                    if (jpeg != null) {
                        // Stamp the frame at capture start so the desktop's
                        // e2e measure covers capture+encode, not just the wire.
                        sendFailures = if (
                            sendJpegFrame(
                                flow, capability.sessionId, sequence++,
                                captureStartUs, jpeg,
                            )
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
        mirrorWanted = false
        mirrorJob?.cancel()
        mirrorJob = null
        val staleFlow = videoFlow
        videoFlow = null
        staleFlow?.let { flow -> sdkScope?.launch { runCatching { flow.close() } } }
        val service = ScreenCaptureService.instance
        service?.frameSink = null
        service?.onProjectionStopped = null
        if (service != null) ScreenCaptureService.stop(context)
    }

    private suspend fun grabJpeg(): ByteArray? {
        val svc = service() ?: return null
        val requestNanos = SystemClock.elapsedRealtimeNanos()
        var captureMs = -1L
        val bitmap = try {
            suspendCancellableCoroutine<Bitmap?> { cont ->
                svc.grabScreenshot(screenshotExecutor, object :
                    AccessibilityService.TakeScreenshotCallback {
                    override fun onSuccess(result: AccessibilityService.ScreenshotResult) {
                        captureMs = (SystemClock.elapsedRealtimeNanos() - requestNanos) / 1_000_000
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
        val encodeNanos = SystemClock.elapsedRealtimeNanos()
        return try {
            val scaled = if (bitmap.width > MIRROR_MAX_WIDTH) {
                val h = bitmap.height * MIRROR_MAX_WIDTH / bitmap.width
                Bitmap.createScaledBitmap(bitmap, MIRROR_MAX_WIDTH, h, true)
            } else bitmap
            val out = ByteArrayOutputStream(96 * 1024)
            scaled.compress(Bitmap.CompressFormat.JPEG, MIRROR_JPEG_QUALITY, out)
            val encodeMs = (SystemClock.elapsedRealtimeNanos() - encodeNanos) / 1_000_000
            mirrorCaptureMs = captureMs
            mirrorEncodeMs = encodeMs
            out.toByteArray()
        } finally {
            bitmap.recycle()
        }
    }

    private fun sendJpegFrame(
        flow: AnchorDatagramFlow,
        capabilitySessionId: Long,
        sequence: Long,
        presentationTimeUs: Long,
        jpeg: ByteArray,
    ): Boolean {
        val sent = sendFramePackets(
            flow, capabilitySessionId, sequence, presentationTimeUs,
            0, VideoFrameProtocol.FLAG_KEYFRAME, jpeg,
        )
        if (sequence % 20L == 0L) {
            Log.i(
                TAG,
                "mirror frame $sequence: capture=${mirrorCaptureMs}ms encode=${mirrorEncodeMs}ms bytes=${jpeg.size} packets=$sent",
            )
        }
        return sent > 0
    }

    // ── Helpers ────────────────────────────────────────────────────────

    private fun requireCapability(): AnchorCapability? =
        sdkCapability ?: run {
            Log.w(TAG, "mirror requested with no SDK input capability")
            null
        }

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

    private fun service(): AnchorAccessibilityService? {
        val svc = AnchorAccessibilityService.get()
        if (svc == null) Log.d(TAG, "dropping input: accessibility service not enabled")
        return svc
    }
}
