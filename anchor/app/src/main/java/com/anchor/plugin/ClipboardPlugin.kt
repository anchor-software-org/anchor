package com.anchor.plugin

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.util.Base64
import android.util.Log
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.ClipboardProtocol
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.security.MessageDigest

private const val TAG = "anchor.clipboard"
private const val COMPRESS_THRESHOLD = 1024 // 1KB

/**
 * Clipboard sync plugin — mirrors Rust's ClipboardPlugin.
 *
 * Watches the local clipboard for changes and sends them to the desktop.
 * Receives clipboard content from the desktop and applies it locally.
 * The desktop-local broker representation compresses text over 1 KiB with zlib. The SDK
 * protobuf path sends typed bytes directly and will gain an explicit content
 * stream for larger payloads in a later protocol revision.
 */
class ClipboardPlugin(
    private val broker: MessageBroker,
    private val context: Context
) : Plugin {

    override val pluginId = "clipboard"

    private var listenJob: Job? = null
    private var sdkScope: CoroutineScope? = null
    private var sdkCapability: AnchorCapability? = null
    private var sdkOriginNodeId: ByteArray? = null
    private var sdkRevision = 0L
    private val clipboardManager by lazy {
        context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    }

    // Echo prevention
    private var lastSentHash: String? = null
    private var lastAppliedHash: String? = null

    // Clipboard history — observable from UI
    private val _history = MutableStateFlow<List<ClipboardHistoryEntry>>(emptyList())
    val history: StateFlow<List<ClipboardHistoryEntry>> = _history.asStateFlow()

    private val maxHistorySize = 50

    override fun start(scope: CoroutineScope) {
        sdkScope = scope
        // Listen for incoming clipboard messages from desktop
        listenJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleIncoming(event, scope)
                }
            }
        }

        // NOTE: Android 10+ blocks background clipboard access, so we do NOT
        // register a ClipboardManager listener. Clipboard sync is receive-only
        // (desktop → phone). The phone can send clipboard when the user
        // explicitly triggers it from the UI (future feature).

        Log.i(TAG, "ClipboardPlugin started (receive-only until an SDK session is attached)")
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
        sdkCapability = null
    }

    /**
     * Attach the negotiated v1 clipboard capability. The session event loop is
     * owned here so clipboard records cannot be mistaken for legacy broker JSON.
     * Calling this again replaces the previous session and is safe on reconnect.
     */
    fun attachSdkSession(
        session: AnchorSession,
        capability: AnchorCapability,
        originNodeId: ByteArray,
        scope: CoroutineScope? = sdkScope,
    ) {
        require(originNodeId.size == 32) { "originNodeId must be exactly 32 bytes" }
        val eventScope = requireNotNull(scope) {
            "ClipboardPlugin must be started before attaching an SDK session"
        }
        sdkCapability = capability
        sdkOriginNodeId = originNodeId.copyOf()
        sdkRevision = 0L
        Log.i(TAG, "SDK clipboard capability attached (session=${capability.sessionId})")
    }

    /** Called by the connection's single SDK event reader. */
    suspend fun handleSdkRecord(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event is AnchorSessionEvent.CapabilityRecord &&
            event.capabilitySessionId == capability.sessionId &&
            event.typeUrl == ClipboardProtocol.PUBLISH_TYPE_URL
        ) {
            runCatching { applySdkPublish(ClipboardProtocol.decodePublish(event.payload)) }
                .onFailure { error -> Log.w(TAG, "SDK clipboard record rejected: ${error.message}") }
        }
    }

    /**
     * Send current clipboard to desktop. Call from UI when user taps "Send Clipboard".
     * Only works when app is in the foreground (Android 10+ restriction).
     */
    fun sendCurrentClipboard() {
        val text = try {
            val clip = clipboardManager.primaryClip ?: return
            if (clip.itemCount == 0) return
            clip.getItemAt(0).coerceToText(context)?.toString() ?: return
        } catch (e: SecurityException) {
            Log.d(TAG, "Clipboard access denied (app not in foreground)")
            return
        } catch (e: Exception) {
            Log.w(TAG, "Clipboard read failed: ${e.message}")
            return
        }
        if (text.isEmpty()) return

        val hash = sha256Hex(text.toByteArray(Charsets.UTF_8))
        if (hash == lastSentHash) return
        lastSentHash = hash

        val timestamp = System.currentTimeMillis()

        val sdk = sdkCapability
        val origin = sdkOriginNodeId
        if (sdk != null && origin != null) {
            val revision = ++sdkRevision
            addToHistory(text, "local", compressed = false)
            sdkScope?.launch(Dispatchers.IO) {
                try {
                    sdk.sendRecord(
                        ClipboardProtocol.PUBLISH_TYPE_URL,
                        ClipboardProtocol.encodeText(origin, revision, text),
                    )
                    Log.i(TAG, "Sent ${text.length} bytes through Anchor SDK")
                } catch (error: Exception) {
                    Log.w(TAG, "SDK clipboard send failed: ${error.message}")
                }
            }
            return
        }

        val (content, compressed) = maybeCompress(text)

        val packet = JSONObject().apply {
            put("plugin_id", "clipboard")
            put("type", "clipboard_content")
            put("content_type", "text/plain")
            put("content", content)
            put("timestamp", timestamp)
            put("hash", hash)
            if (compressed) put("compressed", "zlib")
        }

        Log.i(TAG, "Sending ${text.length} bytes to desktop" +
                if (compressed) " (zlib compressed)" else "")
        addToHistory(text, "local", compressed)

        broker.send(AnchorEvent(
            target = AnchorTarget.Device,
            message = AnchorMessage.Json(packet.toString())
        ))
    }

    // ── Desktop → Local clipboard ──────────────────────────────────────

    private fun handleIncoming(event: AnchorEvent, scope: CoroutineScope) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return

        val json = try {
            JSONObject(msg.payload)
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse JSON: ${e.message}")
            return
        }

        when (json.optString("type")) {
            "clipboard_content" -> scope.launch { handleClipboardContent(json) }
            "clipboard_connect" -> scope.launch { handleClipboardConnect(json) }
        }
    }

    private suspend fun handleClipboardContent(json: JSONObject) {
        val hash = json.optString("hash", "") .ifEmpty { return }

        // Echo guard: don't apply content we just sent
        if (hash == lastSentHash) {
            Log.d(TAG, "Skipping remote content (matches lastSentHash)")
            return
        }

        val rawContent = json.optString("content", "").ifEmpty { return }
        val contentType = json.optString("content_type", "text/plain")
        val compressed = json.optString("compressed", "")

        // Decompress if needed
        val content = when (compressed) {
            "zlib" -> {
                try {
                    val compressedBytes = Base64.decode(rawContent, Base64.DEFAULT)
                    val decompressed = ZlibCodec.decompress(compressedBytes)
                    Log.i(TAG, "zlib decompressed ${compressedBytes.size} -> ${decompressed.size} bytes")
                    String(decompressed, Charsets.UTF_8)
                } catch (e: Exception) {
                    Log.w(TAG, "zlib decompression failed: ${e.message}")
                    return
                }
            }
            "zstd" -> {
                // Desktop sends zstd — we need to handle it too.
                // For now, log a warning. If zstd-jni is added later, decompress here.
                Log.w(TAG, "Received zstd compressed content — not supported on Android, skipping")
                return
            }
            "" -> rawContent
            else -> {
                Log.w(TAG, "Unknown compression: $compressed")
                return
            }
        }

        // Set echo guard BEFORE writing to clipboard
        lastAppliedHash = hash

        when (contentType) {
            "text/plain" -> {
                withContext(Dispatchers.Main) {
                    val clip = ClipData.newPlainText("anchor", content)
                    clipboardManager.setPrimaryClip(clip)
                }
                Log.i(TAG, "Applied ${content.length} bytes of text from desktop")
                addToHistory(content, "desktop", compressed.isNotEmpty())
            }
            "image/png" -> {
                try {
                    val imageBytes = Base64.decode(content, Base64.DEFAULT)
                    val bitmap = android.graphics.BitmapFactory.decodeByteArray(imageBytes, 0, imageBytes.size)
                    if (bitmap != null) {
                        // Write bitmap to a temp file and create a content URI for clipboard
                        val file = java.io.File(context.cacheDir, "clipboard_image.png")
                        file.outputStream().use { out ->
                            bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
                        }
                        val uri = androidx.core.content.FileProvider.getUriForFile(
                            context, "${context.packageName}.fileprovider", file
                        )
                        withContext(Dispatchers.Main) {
                            val clipData = ClipData.newUri(context.contentResolver, "anchor image", uri)
                            clipboardManager.setPrimaryClip(clipData)
                        }
                        Log.i(TAG, "Applied PNG image from desktop (${imageBytes.size} bytes)")
                        addToHistory(
                            text = null,
                            source = "desktop",
                            compressed = compressed.isNotEmpty(),
                            imageBytes = imageBytes,
                            contentType = "image/png"
                        )
                    } else {
                        Log.w(TAG, "Failed to decode PNG image")
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "Image clipboard failed: ${e.message}")
                    addToHistory(text = "[Image — failed to decode]", source = "desktop", compressed = compressed.isNotEmpty())
                }
            }
            else -> {
                Log.d(TAG, "Unsupported content_type: $contentType")
            }
        }
    }

    private suspend fun applySdkPublish(message: ClipboardProtocol.Publication) {
        val origin = message.originNodeId
        if (origin.size != 32) {
            Log.w(TAG, "Ignoring SDK clipboard record with invalid origin node id")
            return
        }
        when {
            message.text != null -> {
                val text = requireNotNull(message.text)
                val hash = sha256Hex(text.toByteArray(Charsets.UTF_8))
                if (hash == lastSentHash) return
                lastAppliedHash = hash
                withContext(Dispatchers.Main) {
                    clipboardManager.setPrimaryClip(ClipData.newPlainText("anchor", text))
                }
                addToHistory(text, "desktop", compressed = false)
                Log.i(TAG, "Applied ${text.length} bytes from Anchor SDK")
            }
            message.png != null -> {
                val imageBytes = requireNotNull(message.png)
                if (sha256Hex(imageBytes) == lastSentHash) return
                try {
                    val bitmap = android.graphics.BitmapFactory.decodeByteArray(imageBytes, 0, imageBytes.size)
                    if (bitmap == null) {
                        Log.w(TAG, "SDK clipboard PNG could not be decoded")
                        return
                    }
                    val file = java.io.File(context.cacheDir, "clipboard_image.png")
                    file.outputStream().use { out ->
                        bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
                    }
                    val uri = androidx.core.content.FileProvider.getUriForFile(
                        context, "${context.packageName}.fileprovider", file
                    )
                    withContext(Dispatchers.Main) {
                        clipboardManager.setPrimaryClip(ClipData.newUri(context.contentResolver, "anchor image", uri))
                    }
                    addToHistory(null, "desktop", compressed = false, imageBytes = imageBytes, contentType = "image/png")
                    Log.i(TAG, "Applied ${imageBytes.size} byte PNG from Anchor SDK")
                } catch (error: Exception) {
                    Log.w(TAG, "SDK clipboard PNG failed: ${error.message}")
                }
            }
        }
    }

    private suspend fun handleClipboardConnect(json: JSONObject) {
        val remoteTimestamp = json.optLong("timestamp", 0)
        val localTimestamp = System.currentTimeMillis()

        if (remoteTimestamp > localTimestamp) {
            Log.i(TAG, "Remote clipboard is newer, applying")
            handleClipboardContent(json)
        } else {
            Log.i(TAG, "Local clipboard is newer, keeping")
        }
    }

    // ── Zstd compression/decompression ─────────────────────────────────

    /**
     * Compress with zlib (deflate) if content > threshold.
     * Returns (content_string, was_compressed).
     * The compressed output is base64-encoded for JSON transport.
     */
    private fun maybeCompress(text: String): Pair<String, Boolean> {
        val bytes = text.toByteArray(Charsets.UTF_8)
        if (bytes.size <= COMPRESS_THRESHOLD) return Pair(text, false)

        return try {
            val compressed = ZlibCodec.compress(bytes)
            val b64 = Base64.encodeToString(compressed, Base64.NO_WRAP)
            Log.d(TAG, "Compressed ${bytes.size} -> ${compressed.size} bytes " +
                    "(${(compressed.size * 100.0 / bytes.size).toInt()}%)")
            Pair(b64, true)
        } catch (e: Exception) {
            Log.w(TAG, "Compression failed: ${e.message}, sending uncompressed")
            Pair(text, false)
        }
    }

    // ── History ──────────────────────────────────────────────────────────

    private fun addToHistory(
        text: String? = null,
        source: String,
        compressed: Boolean,
        imageBytes: ByteArray? = null,
        contentType: String = "text/plain"
    ) {
        val sizeBytes = imageBytes?.size ?: text?.toByteArray(Charsets.UTF_8)?.size ?: 0
        val entry = ClipboardHistoryEntry(
            text = text,
            imageBytes = imageBytes,
            contentType = contentType,
            source = source,
            timestamp = System.currentTimeMillis(),
            compressed = compressed,
            sizeBytes = sizeBytes
        )
        val current = _history.value.toMutableList()
        current.add(0, entry)
        if (current.size > maxHistorySize) current.removeAt(current.lastIndex)
        _history.value = current
    }

    fun clearHistory() {
        _history.value = emptyList()
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    private fun sha256Hex(data: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(data)
        return digest.joinToString("") { "%02x".format(it) }
    }
}

data class ClipboardHistoryEntry(
    val text: String?,
    val imageBytes: ByteArray? = null,  // decoded PNG bytes for image entries
    val contentType: String = "text/plain",  // "text/plain" or "image/png"
    val source: String,       // "local" or "desktop"
    val timestamp: Long,
    val compressed: Boolean,
    val sizeBytes: Int
) {
    val isImage: Boolean get() = contentType == "image/png"
}
