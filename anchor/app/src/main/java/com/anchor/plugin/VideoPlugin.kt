package com.anchor.plugin

import android.media.MediaCodec
import android.media.MediaCodecList
import android.media.MediaFormat
import android.util.Log
import android.view.Surface
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
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.ScreenProtocol
import org.anchor.sdk.VideoFrameProtocol
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicInteger
import java.util.HashMap
import java.util.Locale

private const val TAG = "anchor"
private const val TIMING_TAG = "anchor.timing"
private const val FRAME_QUEUE_CAPACITY = 3
private const val MAX_INIT_FAILURES = 3
private const val CODEC_METADATA_LIMIT = 64
private const val START_CONFIRMATION_TIMEOUT_MS = 15_000L

class VideoPlugin(
    private val broker: MessageBroker
) : Plugin {

    override val pluginId = "video"

    @Volatile
    private var codec: MediaCodec? = null
    private var surface: Surface? = null
    var streamWidth = 1920
        private set
    var streamHeight = 1080
        private set

    @Volatile
    private var initialized = false

    private var consecutiveInitFailures = 0
    private var initGivenUp = false
    private var lastTriedSize = 0 to 0

    private val _isReceiving = MutableStateFlow(false)
    val isReceiving = _isReceiving.asStateFlow()

    private val _fps = MutableStateFlow(0)
    val fps = _fps.asStateFlow()

    /** Current stream state reported by desktop: idle, starting, streaming, switching, stopping, error */
    private val _streamStatus = MutableStateFlow("idle")
    val streamStatus = _streamStatus.asStateFlow()
    private val _streamError = MutableStateFlow<String?>(null)
    val streamError = _streamError.asStateFlow()

    data class Display(val id: Int, val name: String, val width: Int, val height: Int)
    private val _availableDisplays = MutableStateFlow<List<Display>>(emptyList())
    val availableDisplays = _availableDisplays.asStateFlow()
    private val _selectedDisplayIndex = MutableStateFlow(0)
    val selectedDisplayIndex = _selectedDisplayIndex.asStateFlow()

    /** A transport-complete H.264 access unit timestamped on this device. */
    private data class QueuedFrame(
        val data: ByteArray,
        val transportCompleteNs: Long,
        val sequence: Long? = null,
        val sourcePresentationTimeUs: Long? = null,
        val isKeyframe: Boolean = false,
        val fragmentCount: Int = 0,
    )

    private val pendingFrames = ArrayBlockingQueue<QueuedFrame>(FRAME_QUEUE_CAPACITY)
    /** Input buffer indices offered by MediaCodec while no frame was waiting. */
    private val availableInputBuffers = ConcurrentLinkedQueue<Int>()
    private val queueDropsThisSecond = AtomicInteger(0)
    private val codecDropsThisSecond = AtomicInteger(0)
    private val codecInputsThisSecond = AtomicInteger(0)
    private val renderedOutputsThisSecond = AtomicInteger(0)
    private val decoderStatsLock = Any()
    // Both codec callbacks update these, so guard them with decoderStatsLock.
    private val transportToCodecInputUs = ArrayList<Long>(128)
    private val transportToOutputReadyUs = ArrayList<Long>(128)
    private val transportToRenderSubmitUs = ArrayList<Long>(128)
    private var decoderStatsStartedNs = System.nanoTime()
    private var lastFeedNs = 0L
    private var lastInputQueuedNs = 0L
    private val lastInputSize = AtomicInteger(0)
    private val codecFrameMetadata = HashMap<Long, QueuedFrame>()
    private var sideboatTrace: SideboatTrace? = null
    @Volatile private var sdkCapability: AnchorCapability? = null
    @Volatile private var sdkSession: AnchorSession? = null
    private var sdkScope: CoroutineScope? = null
    private var sdkStreamJob: kotlinx.coroutines.Job? = null
    private var brokerJob: kotlinx.coroutines.Job? = null
    private var startConfirmationJob: Job? = null

    fun setSurface(surface: Surface) {
        // SurfaceView can deliver a second surface-created callback while its
        // existing producer is still valid (notably during Compose layout or
        // system-bar transitions). The codec is already bound to that valid
        // producer. Recreating it here causes an avoidable decoder flush and
        // keyframe recovery that presents as a network stutter.
        if (initialized && this.surface?.isValid == true) {
            Log.d(TAG, "Keeping existing valid video surface; ignoring duplicate callback")
            return
        }
        this.surface = surface
        if (initGivenUp) return
        if (initCodec(surface, streamWidth, streamHeight)) {
            requestKeyframe()
        }
    }

    /** Begin capture + encode + stream atomically. */
    fun startStreaming() {
        Log.i(TAG, "start → desktop")
        _streamError.value = null
        _streamStatus.value = "starting"
        startConfirmationJob?.cancel()
        startConfirmationJob = sdkScope?.launch {
            delay(START_CONFIRMATION_TIMEOUT_MS)
            if (_streamStatus.value == "starting") {
                Log.w(TAG, "Desktop did not confirm stream start within ${START_CONFIRMATION_TIMEOUT_MS}ms")
                _streamError.value = "Desktop did not confirm the stream"
                _streamStatus.value = "error"
            }
        }
        if (sendSdk(ScreenProtocol.START_TYPE_URL, ScreenProtocol.encodeStart())) {
            return
        }
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Device,
                message = AnchorMessage.Json(
                    """{"plugin_id":"wayland","command":"start"}"""
                )
            )
        )
    }

    /** Stop capture + stream atomically. */
    fun stopStreaming() {
        Log.i(TAG, "stop → desktop")
        startConfirmationJob?.cancel()
        if (sendSdk(ScreenProtocol.STOP_TYPE_URL, ScreenProtocol.encodeStop())) {
            _streamError.value = null
            _streamStatus.value = "stopping"
            return
        }
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Device,
                message = AnchorMessage.Json(
                    """{"plugin_id":"wayland","command":"stop"}"""
                )
            )
        )
    }

    /**
     * Pick a screen. If currently streaming the desktop auto-restarts on the new screen.
     * The desktop echoes back the confirmed selected_index via stream_status.
     */
    fun selectOutput(index: Int) {
        // Reflect the tap immediately so the UI highlights the chosen screen
        // even when not streaming. The desktop remains authoritative and will
        // confirm/correct via stream_status (selected_index) once it responds.
        val max = (_availableDisplays.value.size - 1).coerceAtLeast(0)
        _selectedDisplayIndex.value = index.coerceIn(0, max)
        if (sendSdk(ScreenProtocol.SELECT_OUTPUT_TYPE_URL, ScreenProtocol.encodeSelectOutput(index.toString()))) return
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Device,
                message = AnchorMessage.Json(
                    """{"plugin_id":"wayland","command":"select_output","index":$index}"""
                )
            )
        )
    }

    fun requestKeyframe() {
        Log.i(TAG, "Requesting keyframe from desktop")
        if (sendSdk(ScreenProtocol.REQUEST_KEYFRAME_TYPE_URL, ScreenProtocol.encodeRequestKeyframe())) return
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Device,
                message = AnchorMessage.Json(
                    """{"plugin_id":"wayland","command":"request_keyframe"}"""
                )
            )
        )
    }

    fun clearSurface() {
        releaseCodec()
        surface = null
        consecutiveInitFailures = 0
        initGivenUp = false
    }

    private fun releaseCodec() {
        initialized = false
        clearPendingFrames()
        availableInputBuffers.clear()
        val oldCodec = codec
        codec = null
        try {
            oldCodec?.stop()
            oldCodec?.release()
        } catch (_: Exception) {
        }
    }

    private fun initCodec(surface: Surface, width: Int, height: Int): Boolean {
        releaseCodec()
        Log.i(TAG, "initCodec ${width}x${height}")
        lastTriedSize = width to height

        val candidates = decoderCandidates(width, height)
        if (candidates.isEmpty()) {
            Log.e(TAG, "No H.264 decoder reports support for ${width}x${height}")
        }

        var lastError: String? = null
        for (decoderName in candidates) {
            // Try with LOW_LATENCY (if supported), then plain. Some old OMX
            // decoders fail to configure when unknown keys are present.
            val supportsLowLatency = decoderSupportsLowLatency(decoderName)
            val attempts = if (supportsLowLatency) listOf(true, false) else listOf(false)
            for (withLowLatency in attempts) {
                var candidateCodec: MediaCodec? = null
                try {
                    val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                    format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 0)
                    // Realtime priority + an operating-rate hint: some vendor
                    // decoders engage faster scheduling/output paths with
                    // these set. Both are documented MediaFormat keys; codecs
                    // that ignore them are unaffected.
                    format.setInteger(MediaFormat.KEY_PRIORITY, 0)
                    format.setInteger(MediaFormat.KEY_OPERATING_RATE, 120)
                    if (withLowLatency) {
                        format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                    }
                    val c = MediaCodec.createByCodecName(decoderName)
                    candidateCodec = c
                    c.setCallback(object : MediaCodec.Callback() {
                        override fun onInputBufferAvailable(mc: MediaCodec, index: Int) {
                            drainPendingInput(mc, index)
                        }

                        override fun onOutputBufferAvailable(
                            mc: MediaCodec,
                            index: Int,
                            info: MediaCodec.BufferInfo
                        ) {
                            releaseOutput(mc, index, info)
                        }

                        override fun onOutputFormatChanged(mc: MediaCodec, format: MediaFormat) = Unit

                        override fun onError(mc: MediaCodec, error: MediaCodec.CodecException) {
                            Log.e(TAG, "MediaCodec async error: ${error.diagnosticInfo}", error)
                        }
                    })
                    c.configure(format, surface, null, 0)
                    codec = c
                    initialized = true
                    c.start()
                    consecutiveInitFailures = 0
                    Log.i(TAG, "codec started ${width}x${height} via $decoderName (low_latency=$withLowLatency)")
                    return true
                } catch (e: Exception) {
                    lastError = "$decoderName(low_latency=$withLowLatency): ${e.message}"
                    Log.w(TAG, "decoder $decoderName (low_latency=$withLowLatency) failed: ${e.message}")
                    if (codec === candidateCodec) codec = null
                    try { candidateCodec?.release() } catch (_: Exception) {}
                    initialized = false
                }
            }
        }

        consecutiveInitFailures++
        Log.e(TAG, "codec init failed (${consecutiveInitFailures}x): $lastError")
        if (consecutiveInitFailures >= MAX_INIT_FAILURES) {
            initGivenUp = true
            Log.e(TAG, "Giving up on codec init after $consecutiveInitFailures attempts at ${width}x${height}")
            broker.send(
                AnchorEvent(
                    AnchorTarget.Gui,
                    AnchorMessage.Generic(
                        "VideoPlugin: no usable H.264 decoder for ${width}x${height} ($lastError)"
                    )
                )
            )
        }
        return false
    }

    private fun decoderSupportsLowLatency(name: String): Boolean {
        if (android.os.Build.VERSION.SDK_INT < 30) return false
        return try {
            val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
            val info = list.codecInfos.firstOrNull { it.name == name } ?: return false
            val caps = info.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
            caps.isFeatureSupported(android.media.MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)
        } catch (_: Exception) {
            false
        }
    }

    private fun decoderCandidates(width: Int, height: Int): List<String> {
        val hardware = mutableListOf<String>()
        val software = mutableListOf<String>()
        try {
            val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
            for (info in list.codecInfos) {
                if (info.isEncoder) continue
                if (!info.supportedTypes.any { it.equals(MediaFormat.MIMETYPE_VIDEO_AVC, ignoreCase = true) }) continue
                val caps = try {
                    info.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                } catch (_: Exception) { continue }
                val vc = caps.videoCapabilities ?: continue
                if (!vc.isSizeSupported(width, height)) continue
                val isSoftware = if (android.os.Build.VERSION.SDK_INT >= 29) {
                    info.isSoftwareOnly
                } else {
                    val n = info.name.lowercase()
                    n.startsWith("omx.google.") || n.contains(".sw.") || n.contains("software")
                }
                if (isSoftware) software += info.name else hardware += info.name
            }
        } catch (e: Exception) {
            Log.w(TAG, "MediaCodecList enumeration failed: ${e.message}")
        }
        return hardware + software
    }

    override fun start(scope: CoroutineScope) {
        sdkScope = scope
        // MediaCodec's callbacks feed inputs and release decoded frames immediately.
        // Typed SDK records are routed through the broker by NetworkPlugin.
        brokerJob?.cancel()
        brokerJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId &&
                    event.message is AnchorMessage.Json
                ) {
                    handleStreamInfo((event.message as AnchorMessage.Json).payload)
                }
            }
        }
        startSdkStreamReceiver()
    }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkSession = session
        sdkCapability = capability
        Log.i(TAG, "SDK screen capability attached (session=${capability.sessionId})")
        startSdkStreamReceiver()
    }

    /** Start the receiver once both the process scope and capability exist. */
    private fun startSdkStreamReceiver() {
        sdkStreamJob?.cancel()
        val capability = sdkCapability ?: return
        val scope = sdkScope ?: return
        sdkStreamJob = scope.launch(Dispatchers.IO) {
            val trace = SideboatTrace()
            sideboatTrace = trace
            trace.event("run_start", "capability_session_id" to capability.sessionId)
            try {
                val stream = capability.openStream(ScreenProtocol.FRAME_TYPE_URL)
                Log.i(TAG, "SDK screen reliable stream opened (stream=${stream.streamId})")
                trace.event("stream_open", "stream_id" to stream.streamId)
                val assembler = ScreenFrameAssembler(onEvent = { outcome ->
                    trace.event(
                        "assembler_${outcome.outcome.name.lowercase(Locale.US)}",
                        "sequence" to outcome.sequence,
                        "fragment_index" to outcome.fragmentIndex,
                        "fragment_count" to outcome.fragmentCount,
                        "frame_bytes" to outcome.frameBytes,
                        "pending_frames" to outcome.pendingFrames,
                        "pending_bytes" to outcome.pendingBytes,
                        "keyframe" to outcome.isKeyframe,
                        "source_pts_us" to outcome.sourcePresentationTimeUs,
                        "reason" to outcome.reason,
                    )
                })
                val packetReader = ScreenStreamPacketReader()
                var lastIngressMetricsNs = 0L
                while (true) {
                    val chunk = stream.receive()
                    val packets = packetReader.add(chunk.bytes)
                    trace.event(
                        "stream_chunk",
                        "stream_id" to stream.streamId,
                        "bytes" to chunk.bytes.size,
                        "packets" to packets.size,
                        "finished" to chunk.finished,
                    )
                    val nowNs = System.nanoTime()
                    if (nowNs - lastIngressMetricsNs >= 1_000_000_000L) {
                        lastIngressMetricsNs = nowNs
                        val session = sdkSession ?: break
                        val native = sessionIngressMetrics(session)
                        trace.event(
                            "ingress_metrics",
                            "native_queued_events" to native.nativeQueuedEvents,
                            "native_metrics_available" to native.nativeMetricsAvailable,
                            "native_queued_bytes" to native.nativeQueuedBytes,
                            "native_queue_high_water_events" to native.nativeQueueHighWaterEvents,
                            "native_queue_high_water_bytes" to native.nativeQueueHighWaterBytes,
                            "native_enqueued_events" to native.nativeEnqueuedEvents,
                            "native_enqueued_bytes" to native.nativeEnqueuedBytes,
                            "native_dequeued_events" to native.nativeDequeuedEvents,
                            "native_dequeued_bytes" to native.nativeDequeuedBytes,
                            "native_poll_calls" to native.nativePollCalls,
                            "native_polled_events" to native.nativePolledEvents,
                            "native_polled_bytes" to native.nativePolledBytes,
                            "sdk_event_queue_items" to native.sdkEventQueueItems,
                            "sdk_stream_queue_items" to native.sdkStreamQueueItems,
                            "sdk_stream_queue_bytes" to native.sdkStreamQueueBytes,
                            "sdk_stream_queue_high_water_items" to native.sdkStreamQueueHighWaterItems,
                            "sdk_stream_queue_high_water_bytes" to native.sdkStreamQueueHighWaterBytes,
                            "sdk_datagram_queue_items" to native.sdkDatagramQueueItems,
                            "sdk_datagram_queue_bytes" to native.sdkDatagramQueueBytes,
                            "sdk_datagram_queue_high_water_items" to native.sdkDatagramQueueHighWaterItems,
                            "sdk_datagram_queue_high_water_bytes" to native.sdkDatagramQueueHighWaterBytes,
                        )
                    }
                    for (packet in packets) {
                        val completed = assembler.add(packet, stream.streamId, capability.sessionId)
                        if (assembler.takeKeyframeRequest()) {
                            trace.event("keyframe_request_needed", "stream_id" to stream.streamId)
                        }
                        for (assembled in completed) {
                            feedFrame(assembled)
                        }
                    }
                    if (chunk.finished) break
                }
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error
            } catch (error: Exception) {
                Log.w(TAG, "SDK screen reliable stream receiver ended: ${error.message}")
                trace.event("receiver_error", "error" to error.message)
            } finally {
                trace.event("run_end")
            }
        }
    }

    private fun sessionIngressMetrics(session: AnchorSession): SessionIngressMetrics {
        val native = session.transportMetrics()
        val sdk = session.queueMetrics()
        return SessionIngressMetrics(
            nativeMetricsAvailable = native.available,
            nativeQueuedEvents = native.queuedEvents,
            nativeQueuedBytes = native.queuedBytes,
            nativeQueueHighWaterEvents = native.queueHighWaterEvents,
            nativeQueueHighWaterBytes = native.queueHighWaterBytes,
            nativeEnqueuedEvents = native.enqueuedEvents,
            nativeEnqueuedBytes = native.enqueuedBytes,
            nativeDequeuedEvents = native.dequeuedEvents,
            nativeDequeuedBytes = native.dequeuedBytes,
            nativePollCalls = native.pollCalls,
            nativePolledEvents = native.polledEvents,
            nativePolledBytes = native.polledBytes,
            sdkEventQueueItems = sdk.eventQueueItems,
            sdkStreamQueueItems = sdk.streamQueueItems,
            sdkStreamQueueBytes = sdk.streamQueueBytes,
            sdkStreamQueueHighWaterItems = sdk.streamQueueHighWaterItems,
            sdkStreamQueueHighWaterBytes = sdk.streamQueueHighWaterBytes,
            sdkDatagramQueueItems = sdk.datagramQueueItems,
            sdkDatagramQueueBytes = sdk.datagramQueueBytes,
            sdkDatagramQueueHighWaterItems = sdk.datagramQueueHighWaterItems,
            sdkDatagramQueueHighWaterBytes = sdk.datagramQueueHighWaterBytes,
        )
    }

    private data class SessionIngressMetrics(
        val nativeMetricsAvailable: Boolean,
        val nativeQueuedEvents: Long,
        val nativeQueuedBytes: Long,
        val nativeQueueHighWaterEvents: Long,
        val nativeQueueHighWaterBytes: Long,
        val nativeEnqueuedEvents: Long,
        val nativeEnqueuedBytes: Long,
        val nativeDequeuedEvents: Long,
        val nativeDequeuedBytes: Long,
        val nativePollCalls: Long,
        val nativePolledEvents: Long,
        val nativePolledBytes: Long,
        val sdkEventQueueItems: Long,
        val sdkStreamQueueItems: Long,
        val sdkStreamQueueBytes: Long,
        val sdkStreamQueueHighWaterItems: Long,
        val sdkStreamQueueHighWaterBytes: Long,
        val sdkDatagramQueueItems: Long,
        val sdkDatagramQueueBytes: Long,
        val sdkDatagramQueueHighWaterItems: Long,
        val sdkDatagramQueueHighWaterBytes: Long,
    )

    private fun sendSdk(typeUrl: String, payload: ByteArray): Boolean {
        val sdk = sdkCapability ?: return false
        val scope = sdkScope ?: return false
        scope.launch(Dispatchers.IO) {
            runCatching { sdk.sendRecord(typeUrl, payload) }
                .onFailure { error ->
                    Log.w(TAG, "SDK screen control send failed: ${error.message}")
                    _streamError.value = error.message ?: "control send failed"
                    _streamStatus.value = "error"
                }
        }
        return true
    }

    fun handleStreamInfo(payload: String) {
        try {
            val json = Json.parseToJsonElement(payload).jsonObject
            val type = json["type"]?.jsonPrimitive?.content ?: return

            if (type == "stream_status") {
                val state = json["state"]?.jsonPrimitive?.content ?: return
                val error = json["error"]?.jsonPrimitive?.content
                if (state != "starting") startConfirmationJob?.cancel()
                _streamStatus.value = state
                _streamError.value = error
                Log.i(TAG, "stream_status: $state${if (error != null) " error=$error" else ""}")
                // Desktop is authoritative for the selected index.
                json["selected_index"]?.jsonPrimitive?.int?.let { idx ->
                    val max = (_availableDisplays.value.size - 1).coerceAtLeast(0)
                    _selectedDisplayIndex.value = idx.coerceIn(0, max)
                }
                if (state == "idle" || state == "stopping" || state == "error") {
                    _isReceiving.value = false
                    _fps.value = 0
                }
                return
            }

            if (type == "output_list") {
                val arr = json["outputs"]?.jsonArray ?: return
                val displays = arr.map { el ->
                    val obj = el.jsonObject
                    Display(
                        id = obj["id"]?.jsonPrimitive?.int ?: 0,
                        name = obj["name"]?.jsonPrimitive?.content ?: "?",
                        width = obj["width"]?.jsonPrimitive?.int ?: 0,
                        height = obj["height"]?.jsonPrimitive?.int ?: 0
                    )
                }
                _availableDisplays.value = displays
                // Clamp selection to valid range after list change.
                val max = (displays.size - 1).coerceAtLeast(0)
                if (_selectedDisplayIndex.value > max) {
                    _selectedDisplayIndex.value = max
                }
                Log.i(TAG, "output_list: ${displays.size} displays — $displays")
                return
            }

            if (type == "stream_info") {
                val w = json["width"]?.jsonPrimitive?.int ?: return
                val h = json["height"]?.jsonPrimitive?.int ?: return
                val sizeChanged = streamWidth != w || streamHeight != h
                streamWidth = w
                streamHeight = h
                Log.i(TAG, "stream_info received: ${w}x${h}")
                val s = surface ?: return
                if (!sizeChanged && initialized) {
                    return
                }
                if (sizeChanged) {
                    consecutiveInitFailures = 0
                    initGivenUp = false
                }
                if (initGivenUp) return
                if (initCodec(s, w, h)) {
                    requestKeyframe()
                }
            }
        } catch (_: Exception) {
        }
    }

    fun feedFrame(data: ByteArray) {
        acceptFrame(QueuedFrame(data, System.nanoTime()))
    }

    private fun feedFrame(frame: ScreenFrameAssembler.CompletedFrame) {
        acceptFrame(
            QueuedFrame(
                data = frame.data,
                transportCompleteNs = frame.completedNs,
                sequence = frame.sequence,
                sourcePresentationTimeUs = frame.sourcePresentationTimeUs,
                isKeyframe = frame.isKeyframe,
                fragmentCount = frame.fragmentCount,
            ),
        )
    }

    private fun acceptFrame(frame: QueuedFrame) {
        val c = codec ?: return
        if (!initialized) return
        _isReceiving.value = true
        if (_streamStatus.value == "starting" || _streamStatus.value == "switching") {
            Log.i(TAG, "Frame received; confirming stream is active")
            startConfirmationJob?.cancel()
            _streamStatus.value = "streaming"
        }
        val now = System.nanoTime()
        val gapMs = if (lastFeedNs == 0L) 0.0 else (now - lastFeedNs) / 1_000_000.0
        lastFeedNs = now
        sideboatTrace?.event(
            "frame_received",
            "sequence" to frame.sequence,
            "source_pts_us" to frame.sourcePresentationTimeUs,
            "bytes" to frame.data.size,
            "fragments" to frame.fragmentCount,
            "keyframe" to frame.isKeyframe,
            "transport_complete_ns" to frame.transportCompleteNs,
            "receiver_delay_us" to ((now - frame.transportCompleteNs) / 1000).coerceAtLeast(0),
            "queue_frames" to pendingFrames.size,
            "queue_bytes" to pendingBytes(),
        )
        if (gapMs > 40.0) {
            Log.w(
                TIMING_TAG,
                "[video] frame_arrival_gap_ms=${"%.1f".format(gapMs)} size=${frame.data.size} queued=${pendingFrames.size}"
            )
        }

        // In callback mode, consume an input buffer immediately if MediaCodec
        // had already advertised one; otherwise its callback takes the frame.
        val inputIndex = availableInputBuffers.poll()
        if (inputIndex != null) {
            if (!queueInput(c, inputIndex, frame)) {
                codecDropsThisSecond.incrementAndGet()
                sideboatTrace?.event(
                    "frame_dropped",
                    "outcome" to "codec_input_failed",
                    "sequence" to frame.sequence,
                    "bytes" to frame.data.size,
                )
            }
            return
        }

        if (pendingFrames.offer(frame)) {
            sideboatTrace?.event(
                "frame_queued",
                "sequence" to frame.sequence,
                "queue_frames" to pendingFrames.size,
                "queue_bytes" to pendingBytes(),
            )
            return
        }

        val replaced = pendingFrames.poll()
        if (!pendingFrames.offer(frame)) {
            queueDropsThisSecond.incrementAndGet()
            sideboatTrace?.event(
                "frame_dropped",
                "outcome" to "queue_full",
                "sequence" to frame.sequence,
                "bytes" to frame.data.size,
                "queue_frames" to pendingFrames.size,
                "queue_bytes" to pendingBytes(),
            )
            Log.w(TIMING_TAG, "[video] queue_drop size=${frame.data.size} queued=${pendingFrames.size}")
            return
        }
        queueDropsThisSecond.incrementAndGet()
        sideboatTrace?.event(
            "frame_dropped",
            "outcome" to "queue_replace",
            "sequence" to replaced?.sequence,
            "bytes" to replaced?.data?.size,
            "replacement_sequence" to frame.sequence,
            "queue_frames" to pendingFrames.size,
            "queue_bytes" to pendingBytes(),
        )
        Log.w(TIMING_TAG, "[video] queue_replace size=${frame.data.size} queued=${pendingFrames.size}")
    }

    private fun pendingBytes(): Int = pendingFrames.sumOf { it.data.size }

    fun onTransportStopped() {
        startConfirmationJob?.cancel()
        sideboatTrace?.event(
            "transport_stopped",
            "queue_frames" to pendingFrames.size,
            "queue_bytes" to pendingBytes(),
        )
        sdkStreamJob?.cancel()
        sdkStreamJob = null
        _isReceiving.value = false
        _streamStatus.value = "idle"
        _streamError.value = null
        _fps.value = 0
        clearPendingFrames()
    }

    private fun clearPendingFrames() {
        pendingFrames.clear()
        synchronized(decoderStatsLock) { codecFrameMetadata.clear() }
    }

    /** Called by MediaCodec as soon as it has an input buffer available. */
    private fun drainPendingInput(c: MediaCodec, inputIndex: Int) {
        if (c !== codec || !initialized) return
        val frame = pendingFrames.poll()
        if (frame == null) {
            availableInputBuffers.offer(inputIndex)
            return
        }
        if (!queueInput(c, inputIndex, frame)) {
            codecDropsThisSecond.incrementAndGet()
            sideboatTrace?.event(
                "frame_dropped",
                "outcome" to "codec_input_failed",
                "sequence" to frame.sequence,
                "bytes" to frame.data.size,
            )
        }
    }

    /** Queue an access unit without waiting for a future network frame. */
    private fun queueInput(c: MediaCodec, inputIndex: Int, frame: QueuedFrame): Boolean {
        if (c !== codec || !initialized) return false
        try {
            val queuedNs = System.nanoTime()
            val data = frame.data
            val decodeGapMs: Double
            synchronized(decoderStatsLock) {
                decodeGapMs = if (lastInputQueuedNs == 0L) 0.0 else {
                    (queuedNs - lastInputQueuedNs) / 1_000_000.0
                }
                lastInputQueuedNs = queuedNs
            }
            if (decodeGapMs > 40.0) {
                Log.w(
                    TIMING_TAG,
                    "[video] decode_gap_ms=${"%.1f".format(decodeGapMs)} queued=${pendingFrames.size} size=${data.size}"
                )
            }

            val buf = c.getInputBuffer(inputIndex) ?: return false
            buf.clear()
            buf.put(data)
            // Preserve the phone-local transport-complete timestamp through to
            // onOutputBufferAvailable for output-ready and render measurements.
            val codecPresentationTimeUs = frame.transportCompleteNs / 1000
            c.queueInputBuffer(inputIndex, 0, data.size, codecPresentationTimeUs, 0)
            synchronized(decoderStatsLock) {
                codecFrameMetadata[codecPresentationTimeUs] = frame
                // A decoder error must not retain every frame from a run.
                while (codecFrameMetadata.size > CODEC_METADATA_LIMIT) {
                    codecFrameMetadata.remove(codecFrameMetadata.keys.first())
                }
            }
            synchronized(decoderStatsLock) {
                transportToCodecInputUs +=
                    ((queuedNs - frame.transportCompleteNs) / 1000).coerceAtLeast(0)
            }
            sideboatTrace?.event(
                "codec_input",
                "sequence" to frame.sequence,
                "source_pts_us" to frame.sourcePresentationTimeUs,
                "codec_pts_us" to codecPresentationTimeUs,
                "bytes" to data.size,
                "keyframe" to frame.isKeyframe,
                "queue_frames" to pendingFrames.size,
                "queue_bytes" to pendingBytes(),
                "transport_to_codec_input_us" to ((queuedNs - frame.transportCompleteNs) / 1000).coerceAtLeast(0),
            )
            lastInputSize.set(data.size)
            codecInputsThisSecond.incrementAndGet()
            reportDecoderStats(queuedNs)
            return true
        } catch (e: Exception) {
            Log.w(TAG, "MediaCodec queue input failed: ${e.message}")
            return false
        }
    }

    /** Called the instant the decoder makes an output buffer available. */
    private fun releaseOutput(c: MediaCodec, outputIndex: Int, info: MediaCodec.BufferInfo) {
        if (c !== codec || !initialized) return
        try {
            val outputReadyNs = System.nanoTime()
            val frame = synchronized(decoderStatsLock) {
                codecFrameMetadata.remove(info.presentationTimeUs)
            }
            if (info.size > 0 && info.presentationTimeUs > 0) {
                synchronized(decoderStatsLock) {
                    transportToOutputReadyUs +=
                        ((outputReadyNs - info.presentationTimeUs * 1000) / 1000).coerceAtLeast(0)
                }
            }
            c.releaseOutputBuffer(outputIndex, info.size > 0)
            val renderSubmitNs = System.nanoTime()
            if (info.size > 0 && info.presentationTimeUs > 0) {
                synchronized(decoderStatsLock) {
                    transportToRenderSubmitUs +=
                        ((renderSubmitNs - info.presentationTimeUs * 1000) / 1000).coerceAtLeast(0)
                }
                renderedOutputsThisSecond.incrementAndGet()
            }
            sideboatTrace?.event(
                "codec_output",
                "sequence" to frame?.sequence,
                "source_pts_us" to frame?.sourcePresentationTimeUs,
                "codec_pts_us" to info.presentationTimeUs,
                "bytes" to info.size,
                "keyframe" to frame?.isKeyframe,
                "output_ready_ns" to outputReadyNs,
                "render_submit_ns" to renderSubmitNs,
                "transport_to_output_ready_us" to ((outputReadyNs - info.presentationTimeUs * 1000) / 1000).coerceAtLeast(0),
                "transport_to_render_submit_us" to ((renderSubmitNs - info.presentationTimeUs * 1000) / 1000).coerceAtLeast(0),
            )
            reportDecoderStats(renderSubmitNs)
        } catch (e: Exception) {
            Log.w(TAG, "MediaCodec release output failed: ${e.message}")
        }
    }

    /**
     * Low-overhead on-device decode summary. "render submit" is the moment
     * releaseOutputBuffer(render=true) returns; SurfaceFlinger/vsync scanout
     * is intentionally not claimed as part of this measurement.
     */
    private fun reportDecoderStats(nowNs: Long) {
        synchronized(decoderStatsLock) {
            if (nowNs - decoderStatsStartedNs < 1_000_000_000L) return

            val queueDrops = queueDropsThisSecond.getAndSet(0)
            val codecDrops = codecDropsThisSecond.getAndSet(0)
            val codecInputs = codecInputsThisSecond.getAndSet(0)
            val rendered = renderedOutputsThisSecond.getAndSet(0)
            _fps.value = rendered

            Log.i(
                TIMING_TAG,
                "[decode] codec_inputs=$codecInputs render_submits=$rendered queue=${pendingFrames.size} " +
                    "queue_drops=$queueDrops codec_no_input=$codecDrops " +
                    "transport_to_codec_input_ms=${summarizeMs(transportToCodecInputUs)} " +
                    "transport_to_output_ready_ms=${summarizeMs(transportToOutputReadyUs)} " +
                "transport_to_render_submit_ms=${summarizeMs(transportToRenderSubmitUs)} size=${lastInputSize.get()}B"
            )
            sideboatTrace?.event(
                "decode_summary",
                "codec_inputs" to codecInputs,
                "render_submits" to rendered,
                "queue_frames" to pendingFrames.size,
                "queue_bytes" to pendingBytes(),
                "queue_drops" to queueDrops,
                "codec_no_input" to codecDrops,
                "transport_to_codec_input_ms" to summarizeMs(transportToCodecInputUs),
                "transport_to_output_ready_ms" to summarizeMs(transportToOutputReadyUs),
                "transport_to_render_submit_ms" to summarizeMs(transportToRenderSubmitUs),
            )
            if (queueDrops > 0 || codecDrops > 0) {
                Log.w(
                    TAG,
                    "[perf] video backpressure: queue_drops=$queueDrops codec_drops=$codecDrops " +
                        "codec_inputs=$codecInputs render_submits=$rendered"
                )
            }

            transportToCodecInputUs.clear()
            transportToOutputReadyUs.clear()
            transportToRenderSubmitUs.clear()
            decoderStatsStartedNs = nowNs
        }
    }

    private fun summarizeMs(samplesUs: MutableList<Long>): String {
        if (samplesUs.isEmpty()) return "n/a"
        samplesUs.sort()
        fun percentile(fraction: Double): Long {
            val index = ((samplesUs.size - 1) * fraction).toInt()
            return samplesUs[index]
        }
        return String.format(
            Locale.US,
            "p50=%.3f,p95=%.3f,p99=%.3f,max=%.3f",
            percentile(0.50) / 1000.0,
            percentile(0.95) / 1000.0,
            percentile(0.99) / 1000.0,
            samplesUs.last() / 1000.0
        )
    }

    override fun stop() {
        startConfirmationJob?.cancel()
        brokerJob?.cancel()
        brokerJob = null
        sdkStreamJob?.cancel()
        sdkStreamJob = null
        _isReceiving.value = false
        clearPendingFrames()
        releaseCodec()
        surface = null
        initialized = false
    }
}
