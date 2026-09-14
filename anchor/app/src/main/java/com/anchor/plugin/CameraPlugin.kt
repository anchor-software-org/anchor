package com.anchor.plugin

import android.content.Context
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CaptureRequest
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.util.Log
import android.util.Range
import androidx.camera.camera2.interop.Camera2Interop
import androidx.camera.camera2.interop.ExperimentalCamera2Interop
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.lifecycle.LifecycleOwner
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorDatagramFlow
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.CameraProtocol
import org.anchor.sdk.VideoFrameProtocol
import java.nio.ByteBuffer
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

private const val TAG = "anchor.camera"

private const val CAMERA_WIDTH = 1280
private const val CAMERA_HEIGHT = 720
// Defaults used when no config has been pushed from the desktop and the user
// hasn't overridden them via the Android Camera screen.
private const val DEFAULT_CAMERA_FPS = 30
private const val DEFAULT_CAMERA_BITRATE = 2_000_000

/**
 * CameraPlugin — captures the phone camera via CameraX and encodes H.264 upstream to the desktop.
 *
 * Architecture:
 * - CameraX `Preview` use case → PreviewView (handles orientation & front-camera mirroring automatically)
 * - CameraX `ImageAnalysis` use case → receives YUV_420_888 frames → converts to NV12 → feeds MediaCodec
 *   encoder in byte-buffer input mode when streaming is active.
 *
 * This replaces the raw Camera2 implementation to eliminate manual orientation/matrix issues.
 * CameraX handles sensor rotation, display rotation, and front-camera mirroring for the preview.
 * For the encoded stream, frames arrive already rotated to the correct orientation by CameraX's
 * ImageAnalysis, and front-camera frames are NOT mirrored (desktop receives the true image).
 */
@OptIn(ExperimentalCamera2Interop::class)
class CameraPlugin(
    private val broker: MessageBroker,
    private val context: Context,
    private val networkPlugin: NetworkPlugin
) : Plugin {

    override val pluginId = "cameraplugin"


    private val _isStreaming = MutableStateFlow(false)
    val isStreaming = _isStreaming.asStateFlow()

    private val _isPreviewing = MutableStateFlow(false)
    val isPreviewing = _isPreviewing.asStateFlow()

    private val _isSwitching = MutableStateFlow(false)
    val isSwitching = _isSwitching.asStateFlow()

    private val _fps = MutableStateFlow(0)
    val fps = _fps.asStateFlow()

    // Lens facing: CameraCharacteristics.LENS_FACING_BACK or _FRONT
    private val _selectedFacing = MutableStateFlow(CameraCharacteristics.LENS_FACING_BACK)
    val selectedFacing = _selectedFacing.asStateFlow()

    /** Sensor mounting orientation (0/90/180/270) of the currently open camera. */
    private val _sensorOrientation = MutableStateFlow(0)
    val sensorOrientation = _sensorOrientation.asStateFlow()

    // Precedence: Android override > desktop push > default.
    // Once the user touches the Android slider, `userOverride = true` and the
    // desktop's pushes are ignored until the user resets.

    private val _streamFps = MutableStateFlow(DEFAULT_CAMERA_FPS)
    val streamFps = _streamFps.asStateFlow()

    private val _streamBitrate = MutableStateFlow(DEFAULT_CAMERA_BITRATE)
    val streamBitrate = _streamBitrate.asStateFlow()

    /** True once the user has manually changed fps/bitrate from the Android UI. */
    private val _userOverride = MutableStateFlow(false)
    val userOverride = _userOverride.asStateFlow()


    private var cameraProvider: ProcessCameraProvider? = null
    private var lastPreviewView: PreviewView? = null
    private var lifecycleOwner: LifecycleOwner? = null

    /** Camera reference for controls (zoom, torch, exposure). */
    private var cameraRef: androidx.camera.core.Camera? = null
    /** ImageAnalysis reference for updating rotation when device rotates. */
    private var imageAnalysisRef: ImageAnalysis? = null
    /** Listens to device orientation changes while the camera is active. */
    private var orientationListener: android.view.OrientationEventListener? = null
    /** Last physical device orientation (0=up, 90=right edge down, ...). Set by the
     *  OrientationEventListener; used to seed targetRotation on rebind. */
    @Volatile private var _lastDeviceOrientation: Int = 0

    private var encoder: MediaCodec? = null
    private var codecConfig: ByteArray? = null
    /** Actual encoder dimensions — set on first frame from CameraX. */
    private var encoderWidth: Int = CAMERA_WIDTH
    private var encoderHeight: Int = CAMERA_HEIGHT
    /** True when startStreaming was called but encoder not yet created (waiting for first frame). */
    private var streamingRequested: Boolean = false

    private var controlJob: Job? = null
    private var pluginScope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null
    @Volatile private var sdkFrameFlow: AnchorDatagramFlow? = null
    private var sdkFrameFlowJob: Job? = null
    private val sdkFrameSequence = AtomicLong(0)

    /** Serializes all camera state transitions. */
    private val stateMutex = Mutex()

    /** Executor for ImageAnalysis callbacks (off main thread). */
    private val analysisExecutor = Executors.newSingleThreadExecutor()

    private var framesThisSecond = 0
    private var lastFpsTime = System.nanoTime()


    override fun start(scope: CoroutineScope) {
        pluginScope = scope
        controlJob = scope.launch {
            broker.events.collect { event ->
                // Handle control messages targeted at this plugin (Service target)
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleControlMessage(event.message)
                    return@collect
                }
                // Also handle Device-targeted messages with our plugin_id
                // (desktop sends stream params as Device target over TCP)
                if (event.target is AnchorTarget.Device) {
                    val msg = event.message
                    if (msg is AnchorMessage.Json) {
                        try {
                            val json = org.json.JSONObject(msg.payload)
                            if (json.optString("plugin_id") == pluginId) {
                                handleControlMessage(msg)
                                return@collect
                            }
                        } catch (_: Exception) {}
                    }
                }
                // Handle disconnect events
                if (event.target is AnchorTarget.Gui) {
                    val msg = event.message
                    if (msg is AnchorMessage.Json) {
                        try {
                            val json = org.json.JSONObject(msg.payload)
                            if (json.optString("type") == "connection_status" &&
                                json.optString("status") == "disconnected"
                            ) {
                                if (_isStreaming.value) {
                                    Log.i(TAG, "Connection lost — stopping stream")
                                    stopStreaming()
                                }
                            }
                        } catch (_: Exception) {}
                    }
                }
            }
        }
    }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK camera capability attached (session=${capability.sessionId})")
        sdkFrameFlowJob?.cancel()
        sdkFrameFlow = null
        val scope = pluginScope ?: return
        sdkFrameFlowJob = scope.launch(Dispatchers.IO) {
            try {
                val flow = capability.openDatagramFlow(CameraProtocol.FRAME_TYPE_URL)
                sdkFrameFlow = flow
                Log.i(TAG, "SDK camera datagram flow opened (flow=${flow.flowId})")
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error
            } catch (error: Exception) {
                Log.w(TAG, "SDK camera datagram flow unavailable: ${error.message}")
            }
        }
    }

    /** Apply a desktop-originated typed camera control. */
    suspend fun handleSdkRecord(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event !is AnchorSessionEvent.CapabilityRecord ||
            event.capabilitySessionId != capability.sessionId ||
            event.typeUrl != CameraProtocol.CONTROL_TYPE_URL
        ) return
        val control = runCatching { CameraProtocol.decodeControl(event.payload) }.getOrElse {
            Log.w(TAG, "Malformed SDK camera control: ${it.message}")
            return
        }
        when (control.kindValue) {
            1 -> startStreaming()
            2 -> stopStreaming()
            3 -> switchCamera()
            4 -> applyDesktopStreamParams(control.fps, control.bitrateKbps)
            5 -> setZoomRatio(control.zoomRatio)
            6 -> setLinearZoom(control.linearZoom)
            7 -> setTorchEnabled(control.torchEnabled)
            8 -> setExposureCompensation(control.exposure)
            else -> Log.w(TAG, "Unknown SDK camera control kind=${control.kindValue}")
        }
        Log.i(TAG, "Applied typed SDK camera control kind=${control.kindValue}")
    }

    private fun handleControlMessage(message: AnchorMessage) {
        if (message !is AnchorMessage.Json) return
        try {
            val json = org.json.JSONObject(message.payload)
            when (json.optString("command")) {
                "start" -> startStreaming()
                "stop"  -> stopStreaming()
                "switch_camera" -> switchCamera()
                "set_zoom_ratio" -> setZoomRatio(json.optDouble("value", 1.0).toFloat())
                "set_linear_zoom" -> setLinearZoom(json.optDouble("value", 0.0).toFloat())
                "set_torch" -> setTorchEnabled(json.optBoolean("enabled", false))
                "set_exposure" -> setExposureCompensation(json.optInt("value", 0))
                "set_stream_params" -> {
                    val fps = json.optInt("fps", -1)
                    val kbps = json.optInt("bitrate_kbps", -1)
                    applyDesktopStreamParams(fps, kbps)
                }
            }
        } catch (e: Exception) {
            Log.e(TAG, "Failed to parse control message: ${e.message}")
        }
    }

    /**
     * Apply FPS + bitrate pushed from the desktop. Ignored if the user has
     * overridden these values from the Android Camera screen.
     */
    private fun applyDesktopStreamParams(fps: Int, bitrateKbps: Int) {
        if (_userOverride.value) {
            Log.i(TAG, "Ignoring desktop stream params — Android override active")
            return
        }
        val newFps = if (fps in 1..120) fps else _streamFps.value
        val newBitrate = if (bitrateKbps in 100..50_000) bitrateKbps * 1000 else _streamBitrate.value
        setStreamParamsInternal(newFps, newBitrate, origin = "desktop")
    }

    /** Called from the Android Camera screen. Marks the user as overriding. */
    fun setStreamParamsFromUi(fps: Int, bitrateBps: Int) {
        _userOverride.value = true
        setStreamParamsInternal(
            fps.coerceIn(1, 120),
            bitrateBps.coerceIn(100_000, 50_000_000),
            origin = "android-ui",
        )
    }

    /** Reset so desktop pushes take effect again. */
    fun clearUserOverride() {
        _userOverride.value = false
        Log.i(TAG, "Cleared Android override — desktop stream params will apply")
    }

    private fun setStreamParamsInternal(fps: Int, bitrateBps: Int, origin: String) {
        val changed = fps != _streamFps.value || bitrateBps != _streamBitrate.value
        _streamFps.value = fps
        _streamBitrate.value = bitrateBps
        if (!changed) return

        Log.i(TAG, "Stream params ($origin): ${fps}fps, ${bitrateBps / 1000}kbps")

        // Rebind camera to apply new FPS range to the capture session.
        // Keep the on-screen Preview use case attached if a PreviewView is
        // currently bound — otherwise the phone's local preview goes dark
        // whenever fps/bitrate changes mid-stream.
        val provider = cameraProvider
        val owner = lifecycleOwner
        if (_isStreaming.value && provider != null && owner != null) {
            val preview = lastPreviewView
            if (preview != null) {
                bindCameraUseCases(provider, owner, preview)
            } else {
                rebindWithoutPreview(provider, owner)
            }
        }

        // If encoder is currently running, restart it to pick up the new params.
        val scope = pluginScope ?: return
        if (encoder != null) {
            scope.launch(Dispatchers.IO) {
                stateMutex.withLock {
                    if (encoder != null) {
                        Log.i(TAG, "Restarting encoder to apply new stream params")
                        stopEncoder()
                        startEncoder()
                    }
                }
            }
        }
    }

    override fun stop() {
        controlJob?.cancel()
        controlJob = null
        sdkFrameFlowJob?.cancel()
        sdkFrameFlowJob = null
        sdkFrameFlow = null
        val scope = pluginScope
        if (scope != null) {
            scope.launch(Dispatchers.Main) {
                stateMutex.withLock { tearDown() }
            }
        }
    }


    /**
     * Bind CameraX preview to the given PreviewView. Called from the CameraScreen composable.
     * CameraX handles orientation and front-camera mirroring automatically.
     */
    fun bindPreviewView(previewView: PreviewView, context: Context) {
        val scope = pluginScope ?: return
        // Avoid redundant rebinds for the same view when provider is already set up
        if (lastPreviewView === previewView && cameraProvider != null && !_isSwitching.value) return
        lastPreviewView = previewView

        scope.launch(Dispatchers.Main) {
            stateMutex.withLock {
                try {
                    val owner = (context as? LifecycleOwner) ?: run {
                        Log.e(TAG, "Context is not a LifecycleOwner")
                        return@withLock
                    }
                    lifecycleOwner = owner

                    val provider = ProcessCameraProvider.getInstance(context).get()
                    cameraProvider = provider

                    bindCameraUseCases(provider, owner, previewView)
                } catch (e: Exception) {
                    Log.e(TAG, "CameraX bind failed: ${e.message}")
                }
            }
        }
    }

    /** Cleanup — unbind the PreviewView but keep streaming if active. */
    fun closePreview() {
        val scope = pluginScope ?: return
        scope.launch(Dispatchers.Main) {
            stateMutex.withLock {
                // If streaming, keep the ImageAnalysis use case alive but drop the preview.
                // This means frames continue to be encoded and sent while the camera screen is gone.
                if (_isStreaming.value) {
                    Log.i(TAG, "closePreview: streaming active — keeping ImageAnalysis alive")
                    // Rebind with only ImageAnalysis, no preview.
                    val provider = cameraProvider
                    val owner = lifecycleOwner
                    if (provider != null && owner != null) {
                        try {
                            rebindWithoutPreview(provider, owner)
                        } catch (e: Exception) {
                            Log.w(TAG, "closePreview rebind failed: ${e.message}")
                        }
                    }
                    lastPreviewView = null
                    _isPreviewing.value = false
                } else {
                    // Not streaming — safe to tear everything down.
                    tearDown()
                }
            }
        }
    }

    /** Rebind only the ImageAnalysis use case, no preview. Used when user leaves the camera tab. */
    private fun rebindWithoutPreview(
        provider: ProcessCameraProvider,
        owner: LifecycleOwner
    ) {
        provider.unbindAll()

        val cameraSelector = CameraSelector.Builder()
            .requireLensFacing(
                if (_selectedFacing.value == CameraCharacteristics.LENS_FACING_FRONT)
                    CameraSelector.LENS_FACING_FRONT
                else
                    CameraSelector.LENS_FACING_BACK
            )
            .build()

        val resolutionSelector = buildImageAnalysisResolutionSelector()

        val imageAnalysis = ImageAnalysis.Builder()
            .setResolutionSelector(resolutionSelector)
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
            .setOutputImageRotationEnabled(true)
            .apply {
                try {
                    Camera2Interop.Extender(this).setCaptureRequestOption(
                        CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                        Range(_streamFps.value, _streamFps.value)
                    )
                } catch (e: Exception) {
                    Log.w(TAG, "Failed to set camera FPS range: ${e.message}")
                }
            }
            .build()
            .apply { targetRotation = orientationToSurfaceRotation(_lastDeviceOrientation) }

        imageAnalysis.setAnalyzer(analysisExecutor) { imageProxy ->
            processFrameAsync(imageProxy)
        }

        val camera = provider.bindToLifecycle(owner, cameraSelector, imageAnalysis)
        _sensorOrientation.value = camera.cameraInfo.sensorRotationDegrees
        // Save the ImageAnalysis ref so we can update targetRotation when device rotates
        this.imageAnalysisRef = imageAnalysis
        setupOrientationListener()
        Log.i(TAG, "Rebound: ImageAnalysis only (no preview) — still streaming")
    }

    /** Start encoding and sending H.264 frames to the desktop. */
    fun startStreaming() {
        val scope = pluginScope ?: run { Log.e(TAG, "startStreaming before start()"); return }
        scope.launch(Dispatchers.IO) {
            stateMutex.withLock {
                if (_isStreaming.value) return@withLock

                // Wait briefly for video transport readiness.
                var waited = 0
                while (!networkPlugin.isVideoTransportReady && waited < 50) {
                    delay(100); waited++
                }
                if (!networkPlugin.isVideoTransportReady) {
                    Log.w(TAG, "Video transport not ready after 5s — aborting")
                    broker.send(
                        AnchorEvent(
                            AnchorTarget.Gui,
                            AnchorMessage.Generic("Camera: video transport not ready")
                        )
                    )
                    return@withLock
                }

                // Defer encoder creation until first frame arrives — we don't know
                // the exact resolution CameraX will deliver until then.
                streamingRequested = true
                _isStreaming.value = true
                Log.i(TAG, "Streaming requested — encoder will init on first frame")
            }
        }
    }

    /** Stop encoding. Preview continues. */
    fun stopStreaming() {
        val scope = pluginScope ?: return
        scope.launch(Dispatchers.IO) {
            stateMutex.withLock {
                if (!_isStreaming.value && encoder == null) return@withLock
                Log.i(TAG, "Stopping stream")
                streamingRequested = false
                _isStreaming.value = false
                stopEncoder()
            }
        }
    }

    /** Switch between front and back camera. */
    fun switchCamera() {
        val scope = pluginScope ?: return
        scope.launch(Dispatchers.Main) {
            stateMutex.withLock {
                if (_isSwitching.value) return@withLock
                _isSwitching.value = true
                try {
                    val newFacing = if (_selectedFacing.value == CameraCharacteristics.LENS_FACING_BACK)
                        CameraCharacteristics.LENS_FACING_FRONT
                    else
                        CameraCharacteristics.LENS_FACING_BACK

                    val wasStreaming = _isStreaming.value
                    if (wasStreaming) {
                        _isStreaming.value = false
                        stopEncoder()
                    }

                    _selectedFacing.value = newFacing

                    // Rebind with new camera selector
                    val provider = cameraProvider
                    val owner = lifecycleOwner
                    val preview = lastPreviewView
                    if (provider != null && owner != null && preview != null) {
                        bindCameraUseCases(provider, owner, preview)
                    }

                    // Restart encoder if we were streaming
                    if (wasStreaming) {
                        startEncoder()
                        _isStreaming.value = true
                    }
                } finally {
                    _isSwitching.value = false
                }
            }
        }
    }


    /**
     * Bind Preview + ImageAnalysis use cases to the given lifecycle owner.
     * Must be called on the main thread.
     */
    private fun bindCameraUseCases(
        provider: ProcessCameraProvider,
        owner: LifecycleOwner,
        previewView: PreviewView
    ) {
        provider.unbindAll()

        val cameraSelector = CameraSelector.Builder()
            .requireLensFacing(
                if (_selectedFacing.value == CameraCharacteristics.LENS_FACING_FRONT)
                    CameraSelector.LENS_FACING_FRONT
                else
                    CameraSelector.LENS_FACING_BACK
            )
            .build()

        // Preview use case — CameraX handles rotation and front-camera mirroring.
        val preview = Preview.Builder()
            .build()
            .also { it.surfaceProvider = previewView.surfaceProvider }

        // ImageAnalysis use case — 16:9 @ 1280x720 so landscape stays landscape
        // and portrait-rotated frames are 720x1280 (not 4:3 like 960x720).
        val resolutionSelector = buildImageAnalysisResolutionSelector()

        val imageAnalysis = ImageAnalysis.Builder()
            .setResolutionSelector(resolutionSelector)
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
            .setOutputImageRotationEnabled(true)
            .apply {
                try {
                    Camera2Interop.Extender(this).setCaptureRequestOption(
                        CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                        Range(_streamFps.value, _streamFps.value)
                    )
                } catch (e: Exception) {
                    Log.w(TAG, "Failed to set camera FPS range: ${e.message}")
                }
            }
            .build()
            .apply { targetRotation = orientationToSurfaceRotation(_lastDeviceOrientation) }

        imageAnalysis.setAnalyzer(analysisExecutor) { imageProxy ->
            processFrameAsync(imageProxy)
        }

        try {
            val camera = provider.bindToLifecycle(owner, cameraSelector, preview, imageAnalysis)

            // Extract sensor orientation from the camera info
            val cameraInfo = camera.cameraInfo
            _sensorOrientation.value = cameraInfo.sensorRotationDegrees

            // Save refs for later camera control + rotation updates
            this.cameraRef = camera
            this.imageAnalysisRef = imageAnalysis

            _isPreviewing.value = true
            Log.i(TAG, "CameraX bound: facing=${_selectedFacing.value}, sensor=${_sensorOrientation.value}, rotation=${getDisplayRotation()}")
            setupOrientationListener()
        } catch (e: Exception) {
            Log.e(TAG, "CameraX bindToLifecycle failed: ${e.message}")
        }
    }

    /** Reusable ResolutionSelector for ImageAnalysis — forces 16:9 aspect ratio
     *  and targets 1280x720, so a portrait-rotated frame is 720x1280 and a
     *  landscape one is 1280x720, not some 4:3 fallback like 960x720. */
    private fun buildImageAnalysisResolutionSelector()
            : androidx.camera.core.resolutionselector.ResolutionSelector {
        return androidx.camera.core.resolutionselector.ResolutionSelector.Builder()
            .setAspectRatioStrategy(
                androidx.camera.core.resolutionselector.AspectRatioStrategy.RATIO_16_9_FALLBACK_AUTO_STRATEGY
            )
            .setResolutionStrategy(
                androidx.camera.core.resolutionselector.ResolutionStrategy(
                    android.util.Size(CAMERA_WIDTH, CAMERA_HEIGHT),
                    androidx.camera.core.resolutionselector.ResolutionStrategy.FALLBACK_RULE_CLOSEST_LOWER_THEN_HIGHER,
                )
            )
            .build()
    }
    private fun setupOrientationListener() {
        // If we already have a listener, keep it — it closes over the field,
        // not a specific ImageAnalysis instance, so it automatically picks up
        // new refs after a rebind. Just seed the targetRotation immediately
        // based on the last-known physical orientation.
        imageAnalysisRef?.let { ia ->
            val lastOrientation = _lastDeviceOrientation
            val rotation = orientationToSurfaceRotation(lastOrientation)
            if (ia.targetRotation != rotation) {
                ia.targetRotation = rotation
                Log.d(TAG, "[orient] seeded targetRotation=$rotation from orientation=$lastOrientation°")
            }
        }

        if (orientationListener != null) return
        val listener = object : android.view.OrientationEventListener(context) {
            override fun onOrientationChanged(orientation: Int) {
                if (orientation == ORIENTATION_UNKNOWN) return
                _lastDeviceOrientation = orientation
                val rotation = orientationToSurfaceRotation(orientation)
                imageAnalysisRef?.let { ia ->
                    if (ia.targetRotation != rotation) {
                        ia.targetRotation = rotation
                        Log.d(TAG, "[orient] targetRotation=$rotation (device=$orientation°)")
                    }
                }
            }
        }
        if (listener.canDetectOrientation()) {
            listener.enable()
            orientationListener = listener
        }
    }

    /** Map OrientationEventListener's degrees (0=portrait up, 90=landscape left edge down) to
     *  Surface.ROTATION_* that ImageAnalysis expects. */
    private fun orientationToSurfaceRotation(orientation: Int): Int = when {
        orientation >= 315 || orientation < 45 -> android.view.Surface.ROTATION_0
        orientation < 135 -> android.view.Surface.ROTATION_270
        orientation < 225 -> android.view.Surface.ROTATION_180
        else -> android.view.Surface.ROTATION_90
    }


    /** Queue of available encoder input buffer indices (populated by async callback). */
    private val availableInputBuffers = java.util.concurrent.LinkedBlockingQueue<Int>()

    /**
     * Called on the analysis executor thread for each camera frame.
     * Picks an available input buffer index from the queue and fills it with NV12 data.
     * Front-camera frames are NOT mirrored — the desktop receives the true image.
     */
    private fun processFrameAsync(imageProxy: ImageProxy) {
        try {
            if (!streamingRequested) {
                return
            }
            val imgW = imageProxy.width
            val imgH = imageProxy.height

            // Lazy-init encoder with actual frame dimensions on first frame.
            // Also recreate if the resolution changed (e.g. after switching cameras).
            if (encoder == null || encoderWidth != imgW || encoderHeight != imgH) {
                encoderWidth = imgW
                encoderHeight = imgH
                Log.i(TAG, "Initializing encoder at actual frame size: ${imgW}x${imgH}")
                stopEncoder()
                startEncoder()
                // Signal the actual resolution to the desktop so it can (re)configure V4L2.
                notifyStreamResolution(imgW, imgH)
            }

            val codec = encoder ?: return

            // Poll for an available input buffer (non-blocking)
            val inputIndex = availableInputBuffers.poll() ?: return

            val inputBuffer = codec.getInputBuffer(inputIndex) ?: run {
                codec.queueInputBuffer(inputIndex, 0, 0, 0, 0)
                return
            }

            // Ensure buffer is large enough for NV12 data (width * height * 1.5)
            val requiredSize = imgW * imgH * 3 / 2
            if (inputBuffer.capacity() < requiredSize) {
                Log.w(TAG, "Input buffer too small: ${inputBuffer.capacity()} < $requiredSize")
                codec.queueInputBuffer(inputIndex, 0, 0, 0, 0)
                return
            }

            val dataSize = yuvToNv12(imageProxy, inputBuffer)
            val presentationTimeUs = imageProxy.imageInfo.timestamp / 1000 // ns → µs
            codec.queueInputBuffer(inputIndex, 0, dataSize, presentationTimeUs, 0)
        } catch (e: Exception) {
            Log.w(TAG, "processFrameAsync error: ${e::class.simpleName}: ${e.message}", e)
        } finally {
            imageProxy.close()
        }
    }

    /** Send a camera_stream_info JSON to the desktop so it can (re)configure V4L2. */
    private fun notifyStreamResolution(width: Int, height: Int) {
        sdkCapability?.let { sdk ->
            val status = CameraProtocol.encodeStatus(
                stateValue = 3,
                width = width,
                height = height,
                fps = _streamFps.value,
                bitrateKbps = _streamBitrate.value / 1000,
            )
            pluginScope?.launch(Dispatchers.IO) {
                runCatching { sdk.sendRecord(CameraProtocol.STATUS_TYPE_URL, status) }
                    .onFailure { error -> Log.w(TAG, "SDK camera status send failed: ${error.message}") }
            }
            Log.i(TAG, "Sent typed SDK camera status: ${width}x${height}")
            return
        }
        val json = """{"plugin_id":"cameraplugin","type":"camera_stream_info","width":$width,"height":$height}"""
        broker.send(AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(json)))
        Log.i(TAG, "Sent camera_stream_info: ${width}x${height}")
    }

    /**
     * Convert an ImageProxy in YUV_420_888 format to NV12 (Y plane + interleaved UV).
     * NV12 is COLOR_FormatYUV420SemiPlanar which most hardware encoders accept.
     *
     * Returns the number of bytes written.
     */
    private fun yuvToNv12(image: ImageProxy, output: ByteBuffer): Int {
        val width = image.width
        val height = image.height
        val yPlane = image.planes[0]
        val uPlane = image.planes[1]
        val vPlane = image.planes[2]

        val yBuffer = yPlane.buffer
        val uBuffer = uPlane.buffer
        val vBuffer = vPlane.buffer

        val yRowStride = yPlane.rowStride
        val uvRowStride = uPlane.rowStride
        val uvPixelStride = uPlane.pixelStride

        output.clear()

        // Copy Y plane row by row (handles row stride padding)
        if (yRowStride == width) {
            // No padding — bulk copy
            val ySize = width * height
            yBuffer.position(0)
            yBuffer.limit(ySize)
            output.put(yBuffer)
        } else {
            // Row-by-row copy to skip padding bytes
            val rowBytes = ByteArray(width)
            for (row in 0 until height) {
                yBuffer.position(row * yRowStride)
                yBuffer.get(rowBytes, 0, width)
                output.put(rowBytes)
            }
        }

        // Copy UV planes interleaved as NV12.
        // Android's COLOR_FormatYUV420SemiPlanar expects: YYYY...UVUV... (NV12 layout).
        // If uvPixelStride == 2, the UV data is already interleaved in the U plane buffer
        // and we can do a fast row-by-row copy.
        val uvHeight = height / 2

        if (uvPixelStride == 2) {
            // UV data is already interleaved. The U plane buffer contains U0,V0,U1,V1,...
            // For width pixels, there are width/2 U-V pairs = width bytes of interleaved UV per row.
            // But the actual interleaved data is (width - 1) bytes because the last V shares
            // position with the next U. We copy exactly width bytes per row for NV12.
            val uvRowBytes = width  // NV12 expects width bytes per UV row
            val rowData = ByteArray(uvRowBytes)
            for (row in 0 until uvHeight) {
                uBuffer.position(row * uvRowStride)
                val remaining = uBuffer.remaining()
                val toCopy = minOf(uvRowBytes, remaining)
                if (toCopy < uvRowBytes) {
                    // Last row might be short — zero-fill the rest
                    rowData.fill(0)
                    uBuffer.get(rowData, 0, toCopy)
                } else {
                    uBuffer.get(rowData, 0, uvRowBytes)
                }
                // Only write what fits in the output
                val outputRemaining = output.remaining()
                if (outputRemaining < uvRowBytes) break
                output.put(rowData, 0, uvRowBytes)
            }
        } else {
            // Pixel stride is 1 — U and V are in separate planes. Interleave manually.
            val uvRow = ByteArray(width)
            for (row in 0 until uvHeight) {
                for (col in 0 until width / 2) {
                    val uIndex = row * uvRowStride + col * uvPixelStride
                    val vIndex = row * vPlane.rowStride + col * vPlane.pixelStride
                    uvRow[col * 2] = uBuffer.get(uIndex)
                    uvRow[col * 2 + 1] = vBuffer.get(vIndex)
                }
                output.put(uvRow, 0, width)
            }
        }

        output.flip()
        return output.remaining()
    }


    /**
     * Start the H.264 encoder in byte-buffer input mode.
     * Frames are fed via queueInputBuffer from the ImageAnalysis callback.
     * Output is handled via async callbacks.
     */
    private fun startEncoder() {
        if (encoder != null) return
        try {
            val format = MediaFormat.createVideoFormat(
                MediaFormat.MIMETYPE_VIDEO_AVC, encoderWidth, encoderHeight
            ).apply {
                setInteger(
                    MediaFormat.KEY_COLOR_FORMAT,
                    MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar
                )
                setInteger(MediaFormat.KEY_BIT_RATE, _streamBitrate.value)
                setInteger(MediaFormat.KEY_FRAME_RATE, _streamFps.value)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
                // Ensure input buffers are large enough for NV12 frames
                setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, encoderWidth * encoderHeight * 3 / 2)
                if (android.os.Build.VERSION.SDK_INT >= 30) {
                    setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                }
                // Do not request KEY_PREPEND_HEADER_TO_SYNC_FRAMES here. Several
                // MediaCodec implementations (including the Android emulator's
                // c2.android.avc.encoder) reject that optional key with BAD_VALUE.
                // We already prepend the captured SPS/PPS ourselves in
                // handleEncoderOutput when a keyframe does not contain them.
            }

            val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)

            // Use async callback for output buffers and input buffer availability.
            codec.setCallback(object : MediaCodec.Callback() {
                override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {
                    // Store available index; processFrameAsync() picks it up from the queue.
                    availableInputBuffers.offer(index)
                }

                override fun onOutputBufferAvailable(
                    codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo
                ) {
                    handleEncoderOutput(codec, index, info)
                }

                override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                    Log.e(TAG, "Encoder error: ${e.message}")
                }

                override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                    Log.i(TAG, "Encoder output format: $format")
                }
            })

            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            codec.start()
            encoder = codec
            Log.i(TAG, "Encoder started ${encoderWidth}x${encoderHeight}@${_streamFps.value}fps ${_streamBitrate.value / 1000}kbps (byte-buffer mode)")
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start encoder: ${e.message}")
            broker.send(
                AnchorEvent(
                    AnchorTarget.Gui,
                    AnchorMessage.Generic("Camera encoder init failed: ${e.message}")
                )
            )
            try { encoder?.release() } catch (_: Exception) {}
            encoder = null
        }
    }

    /**
     * Handle encoded output from the MediaCodec encoder.
     */
    private fun handleEncoderOutput(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
        try {
            // Codec config (SPS/PPS) — forward to desktop immediately
            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                val buf = codec.getOutputBuffer(index)
                if (buf != null && info.size > 0) {
                    val cfg = ByteArray(info.size)
                    buf.position(info.offset)
                    buf.get(cfg)
                    codecConfig = cfg
                    Log.i(TAG, "Forwarding CSD (${cfg.size} bytes) to desktop decoder")
                    sendEncodedFrame(
                        cfg,
                        isKeyFrame = false,
                        presentationTimeUs = info.presentationTimeUs,
                        codecConfigId = 1,
                        codecConfig = true,
                    )
                }
                codec.releaseOutputBuffer(index, false)
                return
            }

            if (info.size > 0) {
                val buf = codec.getOutputBuffer(index) ?: run {
                    codec.releaseOutputBuffer(index, false)
                    return
                }
                val frameData = ByteArray(info.size)
                buf.position(info.offset)
                buf.get(frameData)
                codec.releaseOutputBuffer(index, false)

                val isKeyFrame = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                if (isKeyFrame) {
                    Log.d(TAG, "keyframe ${frameData.size}B startsWithSPS=${startsWithParamSet(frameData)}")
                }

                // If the keyframe already has SPS/PPS prepended (KEY_PREPEND_HEADER_TO_SYNC_FRAMES
                // honored), send as-is. Otherwise prepend our captured CSD so the IDR is decodable
                // on its own (covers stream resync/reconnect mid-stream).
                val data = if (isKeyFrame && codecConfig != null && !startsWithParamSet(frameData)) {
                    codecConfig!! + frameData
                } else {
                    frameData
                }

                sendEncodedFrame(
                    data,
                    isKeyFrame = isKeyFrame,
                    presentationTimeUs = info.presentationTimeUs,
                    codecConfigId = if (codecConfig != null) 1 else 0,
                    codecConfig = false,
                )

                // FPS counter
                framesThisSecond++
                val now = System.nanoTime()
                if (now - lastFpsTime >= 1_000_000_000L) {
                    _fps.value = framesThisSecond
                    framesThisSecond = 0
                    lastFpsTime = now
                }
            } else {
                codec.releaseOutputBuffer(index, false)
            }
        } catch (e: Exception) {
            Log.w(TAG, "handleEncoderOutput error: ${e.message}")
            try { codec.releaseOutputBuffer(index, false) } catch (_: Exception) {}
        }
    }

    /** Send encoded camera bytes over the negotiated QUIC datagram flow. */
    private fun sendEncodedFrame(
        data: ByteArray,
        isKeyFrame: Boolean,
        presentationTimeUs: Long,
        codecConfigId: Long,
        codecConfig: Boolean,
    ) {
        val flow = sdkFrameFlow
        val capability = sdkCapability
        if (flow == null || capability == null) {
            // Send through the existing network path until the SDK flow is ready.
            networkPlugin.sendCameraFrame(data)
            return
        }
        val flags = (if (isKeyFrame) VideoFrameProtocol.FLAG_KEYFRAME else 0) or
            (if (codecConfig) VideoFrameProtocol.FLAG_CODEC_CONFIG else 0)
        val sequence = sdkFrameSequence.getAndIncrement()
        try {
            VideoFrameProtocol.fragment(
                kind = VideoFrameProtocol.KIND_CAMERA,
                flags = flags,
                capabilitySessionId = capability.sessionId,
                flowId = flow.flowId,
                sequence = sequence,
                presentationTimeUs = presentationTimeUs,
                codecConfigId = codecConfigId,
                payload = data,
            ).forEach(flow::send)
            if (sequence % 60L == 0L) {
                Log.i(TAG, "SDK camera frame datagrams sent: sequence=$sequence bytes=${data.size}")
            }
        } catch (error: Exception) {
            Log.w(TAG, "SDK camera datagram send failed: ${error.message}")
        }
    }

    private fun stopEncoder() {
        val codec = encoder
        encoder = null
        availableInputBuffers.clear()
        try { codec?.stop() } catch (e: Exception) { Log.w(TAG, "encoder.stop: ${e.message}") }
        try { codec?.release() } catch (e: Exception) { Log.w(TAG, "encoder.release: ${e.message}") }
        codecConfig = null
        _fps.value = 0
    }


    private fun tearDown() {
        stopEncoder()
        _isStreaming.value = false
        try { cameraProvider?.unbindAll() } catch (_: Exception) {}
        orientationListener?.disable()
        orientationListener = null
        cameraProvider = null
        cameraRef = null
        imageAnalysisRef = null
        lastPreviewView = null
        lifecycleOwner = null
        _isPreviewing.value = false
    }


    /** Set zoom ratio (1.0 = no zoom). Clamped to camera's supported range. */
    fun setZoomRatio(ratio: Float) {
        val cam = cameraRef ?: return
        val state = cam.cameraInfo.zoomState.value ?: return
        val clamped = ratio.coerceIn(state.minZoomRatio, state.maxZoomRatio)
        cam.cameraControl.setZoomRatio(clamped)
        Log.i(TAG, "Zoom set to ${clamped}x (range ${state.minZoomRatio}–${state.maxZoomRatio})")
    }

    /** Set linear zoom [0.0, 1.0]. */
    fun setLinearZoom(value: Float) {
        val cam = cameraRef ?: return
        cam.cameraControl.setLinearZoom(value.coerceIn(0f, 1f))
    }

    /** Toggle torch (flashlight) — only works on back camera with flash. */
    fun setTorchEnabled(enabled: Boolean) {
        val cam = cameraRef ?: return
        if (!cam.cameraInfo.hasFlashUnit()) {
            Log.w(TAG, "No flash unit available")
            return
        }
        cam.cameraControl.enableTorch(enabled)
        Log.i(TAG, "Torch ${if (enabled) "on" else "off"}")
    }

    /** Set exposure compensation. value is in units of EV step (typically -6..+6). */
    fun setExposureCompensation(value: Int) {
        val cam = cameraRef ?: return
        val range = cam.cameraInfo.exposureState.exposureCompensationRange
        val clamped = value.coerceIn(range.lower, range.upper)
        cam.cameraControl.setExposureCompensationIndex(clamped)
        Log.i(TAG, "Exposure set to $clamped (range $range)")
    }


    /** Returns the current display rotation (Surface.ROTATION_0/90/180/270). */
    private fun getDisplayRotation(): Int {
        return try {
            val wm = context.getSystemService(Context.WINDOW_SERVICE) as android.view.WindowManager
            @Suppress("DEPRECATION")
            wm.defaultDisplay.rotation
        } catch (_: Exception) {
            android.view.Surface.ROTATION_0
        }
    }

    /** Returns true if [data] starts with an Annex-B NAL whose type is SPS (7) or PPS (8). */
    private fun startsWithParamSet(data: ByteArray): Boolean {
        val nalHeaderIndex = when {
            data.size >= 5 && data[0] == 0.toByte() && data[1] == 0.toByte()
                    && data[2] == 0.toByte() && data[3] == 1.toByte() -> 4
            data.size >= 4 && data[0] == 0.toByte() && data[1] == 0.toByte()
                    && data[2] == 1.toByte() -> 3
            else -> return false
        }
        val nalType = data[nalHeaderIndex].toInt() and 0x1F
        return nalType == 7 || nalType == 8
    }
}
