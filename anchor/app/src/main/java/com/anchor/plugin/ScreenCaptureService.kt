package com.anchor.plugin

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import android.view.WindowManager
import androidx.core.app.NotificationCompat

private const val TAG = "anchor.capture"
private const val CHANNEL_ID = "anchor_capture"
private const val NOTIFICATION_ID = 42
private const val MAX_DIMENSION = 1280
private const val FRAME_RATE = 30
private const val BIT_RATE = 4_000_000
private const val I_FRAME_INTERVAL_S = 1

/**
 * Foreground service holding the MediaProjection token and the H.264
 * capture pipeline:
 *
 *   VirtualDisplay → MediaCodec input surface → Annex-B access units
 *       → [frameSink] → RemoteInputPlugin → ANFR datagrams
 *
 * The plugin owns the wire; this service owns Android's projection lifetime.
 * A projection dies with the token — a new consent dialog is needed to
 * restart capture, so the service only stops on explicit request, token
 * death, or system revocation (onStop callback).
 */
class ScreenCaptureService : Service() {

    /** (payload, isKeyFrame, isCodecConfig, ptsElapsedRealtimeUs) */
    @Volatile
    var frameSink: ((ByteArray, Boolean, Boolean, Long) -> Unit)? = null

    /** Invoked on the encoder thread when the projection is live. */
    @Volatile
    var onProjectionReady: (() -> Unit)? = null

    @Volatile
    var onProjectionStopped: (() -> Unit)? = null

    private var projection: MediaProjection? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var encoder: MediaCodec? = null
    private var encoderThread: HandlerThread? = null
    @Volatile
    private var codecConfig: ByteArray? = null
    @Volatile
    private var configEmitPending = false
    private var capturedRotation = -1
    private var displayListener: DisplayManager.DisplayListener? = null
    /** nanoTime → elapsedRealtime translation for codec output timestamps. */
    private var clockDeltaUs = 0L

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> {
                val resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0)
                @Suppress("DEPRECATION")
                val resultData = intent.getParcelableExtra<Intent>(EXTRA_RESULT_DATA)
                if (resultData == null) {
                    Log.e(TAG, "capture start without consent token")
                    stopSelf()
                    return START_NOT_STICKY
                }
                startCapture(resultCode, resultData)
            }
            ACTION_STOP -> stopCapture()
        }
        return START_NOT_STICKY
    }

    private fun startCapture(resultCode: Int, resultData: Intent) {
        startForegroundWithType()
        val mpm = getSystemService(MediaProjectionManager::class.java)
        val proj = try {
            mpm.getMediaProjection(resultCode, resultData)
        } catch (e: Exception) {
            Log.e(TAG, "getMediaProjection failed: ${e.message}")
            stopSelf()
            return
        } ?: run {
            Log.e(TAG, "getMediaProjection returned null")
            stopSelf()
            return
        }
        proj.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                Log.i(TAG, "MediaProjection revoked")
                stopCapture()
            }
        }, null)
        projection = proj
        instance = this

        clockDeltaUs = SystemClock.elapsedRealtimeNanos() / 1000 - System.nanoTime() / 1000

        try {
            startEncoder()
            watchRotation()
            onProjectionReady?.invoke()
        } catch (e: Exception) {
            Log.e(TAG, "capture encoder failed: ${e.message}")
            stopCapture()
        }
    }

    /**
     * A MediaProjection token is single-capture on Android 14+: the same
     * instance rejects a second createVirtualDisplay. Rotation therefore
     * resizes the existing virtual display and re-points it at a freshly
     * configured encoder surface instead of rebuilding it.
     */
    private fun watchRotation() {
        val display = runCatching {
            @Suppress("DEPRECATION")
            getSystemService(WindowManager::class.java).defaultDisplay
        }.getOrNull() ?: return
        capturedRotation = display.rotation
        val dm = getSystemService(DisplayManager::class.java)
        val listener = object : DisplayManager.DisplayListener {
            override fun onDisplayAdded(displayId: Int) {}
            override fun onDisplayRemoved(displayId: Int) {}
            override fun onDisplayChanged(displayId: Int) {
                if (displayId != display.displayId) return
                val rotation = display.rotation
                if (rotation == capturedRotation) return
                capturedRotation = rotation
                Log.i(TAG, "display rotated to $rotation — resizing encoder")
                runCatching { restartEncoder() }
                    .onFailure { error ->
                        Log.e(TAG, "rotation restart failed: ${error.message}")
                        stopCapture()
                    }
            }
        }
        displayListener = listener
        dm.registerDisplayListener(listener, null)
    }

    /** Swap the encoder+surface inside the same virtual display. */
    private fun restartEncoder() {
        val display = virtualDisplay ?: return
        releaseEncoder()

        val (width, height) = displaySize()
        val (codec, surface) = createEncoder(width, height)
        encoder = codec
        display.resize(width, height, resources.displayMetrics.densityDpi)
        display.surface = surface
        Log.i(TAG, "capture resized to ${width}x${height} on rotation")
    }

    private fun releaseEncoder() {
        runCatching { encoder?.stop() }
        runCatching { encoder?.release() }
        encoder = null
        codecConfig = null
        encoderThread?.quitSafely()
        encoderThread = null
    }

    private fun startForegroundWithType() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Screen mirroring", NotificationManager.IMPORTANCE_LOW)
        )
        val launch = PendingIntent.getActivity(
            this, 0,
            packageManager.getLaunchIntentForPackage(packageName),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Anchor is mirroring this screen")
            .setSmallIcon(android.R.drawable.presence_video_online)
            .setContentIntent(launch)
            .setOngoing(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(
                NOTIFICATION_ID, notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun displaySize(): Pair<Int, Int> {
        // getRealSize tracks the current orientation; maximumWindowMetrics
        // does not reliably swap width/height on rotation across vendors.
        val size = android.graphics.Point()
        runCatching {
            @Suppress("DEPRECATION")
            getSystemService(WindowManager::class.java).defaultDisplay.getRealSize(size)
        }
        var w = size.x
        var h = size.y
        if (w <= 0 || h <= 0) {
            val bounds = getSystemService(WindowManager::class.java).maximumWindowMetrics.bounds
            w = bounds.width()
            h = bounds.height()
        }
        val scale = minOf(1f, MAX_DIMENSION.toFloat() / maxOf(w, h))
        val fw = w * scale
        val fh = h * scale
        // Codec dims must be even; rounding each independently skews the
        // aspect and leaves letterbox bars inside the AUTO_MIRROR capture.
        // Search nearby even pairs for the closest aspect match.
        val aspect = w.toDouble() / h
        var best = (fw.toInt() and -2) to (fh.toInt() and -2)
        var bestErr = Math.abs(best.first / best.second.toDouble() - aspect)
        for (dw in 0 downTo -10 step 2) {
            val cw = (fw.toInt() + dw) and -2
            if (cw < 16) break
            val ch = (cw / aspect).toInt()
            for (candH in intArrayOf(ch and -2, (ch + 1) and -2)) {
                val err = Math.abs(cw / candH.toDouble() - aspect)
                if (err < bestErr - 1e-9) {
                    bestErr = err
                    best = cw to candH
                }
            }
        }
        return best
    }

    /** Configure+start a surface-input H.264 encoder; returns codec+surface. */
    private fun createEncoder(width: Int, height: Int): Pair<MediaCodec, android.view.Surface> {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height).apply {
            setInteger(
                MediaFormat.KEY_COLOR_FORMAT,
                android.media.MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface,
            )
            setInteger(MediaFormat.KEY_BIT_RATE, BIT_RATE)
            setInteger(MediaFormat.KEY_FRAME_RATE, FRAME_RATE)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, I_FRAME_INTERVAL_S)
            setInteger(MediaFormat.KEY_PRIORITY, 0)
            setInteger(MediaFormat.KEY_OPERATING_RATE, FRAME_RATE)
        }
        val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        val thread = HandlerThread("anchor-capture-encoder").also { it.start() }
        encoderThread = thread
        codec.setCallback(object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}
            override fun onOutputBufferAvailable(
                codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo,
            ) = handleEncoderOutput(codec, index, info)
            override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                Log.e(TAG, "encoder error: ${e.message}")
            }
            override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                Log.i(TAG, "encoder format: $format")
            }
        }, Handler(thread.looper))
        codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        val surface = codec.createInputSurface()
        codec.start()
        Log.i(TAG, "encoder started ${width}x${height}@${FRAME_RATE}fps ${BIT_RATE / 1000}kbps")
        return codec to surface
    }

    /** Re-emit the cached SPS/PPS config AU so a fresh sink can decode. If
     *  the config AU hasn't arrived yet, emit it as soon as it does. */
    fun emitConfigFrame() {
        val cfg = codecConfig
        if (cfg != null) {
            frameSink?.invoke(cfg, false, true, translatePts(System.nanoTime() / 1_000))
        } else {
            configEmitPending = true
        }
    }

    /** Ask the encoder to emit a sync frame ASAP (recovery after drops). */
    fun requestKeyFrame() {
        val codec = encoder ?: return
        encoderThread?.let { thread ->
            Handler(thread.looper).post {
                runCatching {
                    codec.setParameters(
                        Bundle().apply {
                            putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                        },
                    )
                }
            }
        }
    }

    private fun startEncoder() {
        val (width, height) = displaySize()
        val (codec, surface) = createEncoder(width, height)
        encoder = codec
        val dpi = resources.displayMetrics.densityDpi
        virtualDisplay = projection?.createVirtualDisplay(
            "anchor-mirror", width, height, dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface, null, null,
        ) ?: throw IllegalStateException("virtual display without projection")
        Log.i(TAG, "capture started ${width}x${height}@${FRAME_RATE}fps ${BIT_RATE / 1000}kbps")
    }

    private fun handleEncoderOutput(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
        try {
            val data = codec.getOutputBuffer(index)?.takeIf { info.size > 0 }?.let { buf ->
                ByteArray(info.size).also { buf.position(info.offset); buf.get(it) }
            }
            codec.releaseOutputBuffer(index, false)
            if (data == null) return
            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                codecConfig = data
                frameSink?.invoke(data, false, true, translatePts(info.presentationTimeUs))
                // A sink attached before this AU arrived asked to be
                // replayed — deliver now that the config exists.
                if (configEmitPending) {
                    configEmitPending = false
                    frameSink?.invoke(data, false, true, translatePts(info.presentationTimeUs))
                }
                return
            }
            // BUFFER_FLAG_KEY_FRAME is unreliable on some encoders (this
            // Qualcomm part never sets it) — detect the IDR NAL directly so
            // SPS/PPS still gets prepended for late-joining decoders.
            val isKey = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0 ||
                containsIdr(data)
            // Same convention as CameraPlugin: keyframes carry SPS/PPS so a
            // late-joining or loss-recovering decoder can start on the IDR.
            val payload = if (isKey && codecConfig != null && !startsWithParamSet(data)) {
                codecConfig!! + data
            } else data
            frameSink?.invoke(payload, isKey, false, translatePts(info.presentationTimeUs))
        } catch (e: Exception) {
            Log.w(TAG, "encoder output failed: ${e.message}")
            runCatching { codec.releaseOutputBuffer(index, false) }
        }
    }

    /** Codec pts is in the nanoTime domain; the wire contract is elapsedRealtime. */
    private fun translatePts(codecPtsUs: Long): Long = codecPtsUs + clockDeltaUs

    /** True if the Annex-B access unit contains an IDR slice (NAL type 5). */
    private fun containsIdr(data: ByteArray): Boolean {
        var i = 0
        while (i + 4 < data.size) {
            if (data[i] == 0.toByte() && data[i + 1] == 0.toByte()) {
                val nalOff = when {
                    data[i + 2] == 1.toByte() -> i + 3
                    data[i + 2] == 0.toByte() && data[i + 3] == 1.toByte() -> i + 4
                    else -> { i++; continue }
                }
                if (nalOff < data.size && (data[nalOff].toInt() and 0x1F) == 5) return true
                i = nalOff
            } else {
                i++
            }
        }
        return false
    }

    private fun startsWithParamSet(data: ByteArray): Boolean {
        val i = when {
            data.size >= 5 && data[0] == 0.toByte() && data[1] == 0.toByte() &&
                data[2] == 0.toByte() && data[3] == 1.toByte() -> 4
            data.size >= 4 && data[0] == 0.toByte() && data[1] == 0.toByte() &&
                data[2] == 1.toByte() -> 3
            else -> return false
        }
        val nal = data[i].toInt() and 0x1F
        return nal == 7 || nal == 8
    }

    private fun stopCapture() {
        instance = null
        onProjectionStopped?.invoke()
        onProjectionReady = null
        onProjectionStopped = null
        frameSink = null
        runCatching { virtualDisplay?.release() }
        virtualDisplay = null
        releaseEncoder()
        displayListener?.let {
            runCatching {
                getSystemService(DisplayManager::class.java).unregisterDisplayListener(it)
            }
        }
        displayListener = null
        runCatching { projection?.stop() }
        projection = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        Log.i(TAG, "capture stopped")
    }

    override fun onDestroy() {
        if (projection != null) stopCapture()
        super.onDestroy()
    }

    companion object {
        const val ACTION_START = "com.anchor.capture.START"
        const val ACTION_STOP = "com.anchor.capture.STOP"
        const val EXTRA_RESULT_CODE = "result_code"
        const val EXTRA_RESULT_DATA = "result_data"

        /** The one running service, or null. The token is per-process. */
        @Volatile var instance: ScreenCaptureService? = null
            private set

        fun stop(context: Context) {
            context.startService(
                Intent(context, ScreenCaptureService::class.java).setAction(ACTION_STOP)
            )
        }
    }
}
