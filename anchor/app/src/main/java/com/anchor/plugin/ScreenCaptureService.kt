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
    private var codecConfig: ByteArray? = null
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
            onProjectionReady?.invoke()
        } catch (e: Exception) {
            Log.e(TAG, "capture encoder failed: ${e.message}")
            stopCapture()
        }
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
        val wm = getSystemService(WindowManager::class.java)
        val bounds = wm.maximumWindowMetrics.bounds
        var w = bounds.width()
        var h = bounds.height()
        val scale = minOf(1f, MAX_DIMENSION.toFloat() / maxOf(w, h))
        w = (w * scale).toInt() and 0x7FFFFFFE
        h = (h * scale).toInt() and 0x7FFFFFFE
        return w to h
    }

    private fun startEncoder() {
        val (width, height) = displaySize()
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
        encoder = codec

        val dpi = resources.displayMetrics.densityDpi
        virtualDisplay = projection?.createVirtualDisplay(
            "anchor-mirror", width, height, dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface, null, Handler(thread.looper),
        ) ?: throw IllegalStateException("virtual display without projection")
        Log.i(TAG, "capture started ${width}x${height}@${FRAME_RATE}fps ${BIT_RATE / 1000}kbps")
    }

    private fun handleEncoderOutput(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
        try {
            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                val buf = codec.getOutputBuffer(index)
                if (buf != null && info.size > 0) {
                    val cfg = ByteArray(info.size)
                    buf.position(info.offset)
                    buf.get(cfg)
                    codecConfig = cfg
                    frameSink?.invoke(cfg, false, true, translatePts(info.presentationTimeUs))
                }
                codec.releaseOutputBuffer(index, false)
                return
            }
            if (info.size <= 0) {
                codec.releaseOutputBuffer(index, false)
                return
            }
            val buf = codec.getOutputBuffer(index)
            if (buf == null) {
                codec.releaseOutputBuffer(index, false)
                return
            }
            val data = ByteArray(info.size)
            buf.position(info.offset)
            buf.get(data)
            codec.releaseOutputBuffer(index, false)

            val isKey = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
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
        runCatching { encoder?.stop() }
        runCatching { encoder?.release() }
        encoder = null
        codecConfig = null
        runCatching { projection?.stop() }
        projection = null
        encoderThread?.quitSafely()
        encoderThread = null
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
