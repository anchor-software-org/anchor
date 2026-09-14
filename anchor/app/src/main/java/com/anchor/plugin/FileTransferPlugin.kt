package com.anchor.plugin

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Log
import android.webkit.MimeTypeMap
import androidx.core.app.NotificationCompat
import com.anchor.R
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.FileTransferProtocol
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.InputStream
import java.security.MessageDigest
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.AnchorStream
import org.anchor.sdk.FilesProtocol

private const val TAG = "anchor.filetransfer"
private const val CHANNEL_ID = "anchor_filetransfer"
private const val HISTORY_CAP = 50

/** A transfer with no traffic for this long is dead (desktop vanished
 *  mid-send); its partial temp file is discarded. */
private const val STALE_AFTER_MS = 60_000L

enum class TransferDirection { SENT, RECEIVED }

/** A recent transfer surfaced in the Files screen. [uri] (when non-null) is
 *  openable via ACTION_VIEW — the MediaStore item for received files, or the
 *  shared source URI for sent ones. */
data class TransferRecord(
    val name: String,
    val direction: TransferDirection,
    val sizeBytes: Long,
    val timestamp: Long,
    val uri: Uri?,
    val mime: String?,
)

/**
 * AirDrop-style file transfer — mirrors the desktop `FileTransferPlugin`.
 *
 * Inbound (desktop → phone): file_offer / file_chunk / file_complete are
 * streamed to a temp file via [FileTransferProtocol.Assembler], verified for
 * completeness + SHA-256, then published to shared Downloads.
 *
 * Outbound (phone → desktop): [sendFile] streams a shared content URI back as
 * the same three messages. Chunks use the broker's *suspending* send, so the
 * 64-slot SharedFlow provides backpressure — small chunks mean at most ~1 MB is
 * ever queued ahead of latency-sensitive input on the shared control channel.
 */
class FileTransferPlugin(
    private val broker: MessageBroker,
    private val context: Context,
) : Plugin {

    override val pluginId = "filetransfer"

    private var scope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null

    fun attachSdkSession(capability: AnchorCapability) {
        sdkCapability = capability
        sdkStreams.clear()
        Log.i(TAG, "SDK files capability attached (session=${capability.sessionId})")
    }

    /** Drop stream handles from a dead QUIC session before reconnecting. */
    fun onTransportStopped() {
        sdkCapability = null
        sdkStreams.clear()
    }
    private var listenJob: Job? = null
    private val incoming = HashMap<String, Incoming>()
    /** Streams accepted by the SDK router, keyed until FileContentStart binds them. */
    private val sdkStreams = HashMap<Long, AnchorStream>()

    private val _history = MutableStateFlow<List<TransferRecord>>(emptyList())
    /** Recent transfers, newest first — observed by the Files screen. */
    val history: StateFlow<List<TransferRecord>> = _history.asStateFlow()

    fun clearHistory() {
        _history.value = emptyList()
    }

    private fun record(rec: TransferRecord) {
        _history.value = (listOf(rec) + _history.value).take(HISTORY_CAP)
    }

    private class Incoming(
        val name: String,
        val temp: File,
        val out: FileOutputStream,
        val assembler: FileTransferProtocol.Assembler,
        var lastActivityMs: Long = System.currentTimeMillis(),
    )

    override fun start(scope: CoroutineScope) {
        this.scope = scope
        createChannel()
        listenJob = scope.launch {
            broker.events.collect { event ->
                val target = event.target
                if (target is AnchorTarget.Service && target.id == pluginId) {
                val msg = event.message
                    if (msg is AnchorMessage.Json) {
                        val json = runCatching { JSONObject(msg.payload) }.getOrNull()
                        if (json?.optString("type") == "sdk_sms_attachment") {
                            val partId = json.optLong("part_id", -1)
                            val transferId = json.optString("transfer_id").takeIf { it.isNotBlank() }
                            if (partId > 0) sendFile(Uri.parse("content://mms/part/$partId"), transferId)
                        } else handleIncoming(msg.payload)
                    }
                }
            }
        }
        Log.i(TAG, "FileTransferPlugin started")
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
        incoming.values.forEach { runCatching { it.out.close() }; it.temp.delete() }
        incoming.clear()
        sdkStreams.clear()
        sdkExpectedHashes.clear()
    }

    /** A single SDK event reader calls this for a peer-opened stream. */
    suspend fun acceptSdkStream(session: AnchorSession, event: AnchorSessionEvent.StreamOpenRequested) {
        val capability = sdkCapability
        if (capability == null || capability.sessionId != event.capabilitySessionId) {
            // Keep the control protocol moving for streams belonging to another
            // capability; their provider may consume them independently.
            session.sendStreamOpened(event.requestId, event.quicStreamId)
            return
        }
        sdkStreams[event.quicStreamId] = session.acceptStream(event.requestId, event.quicStreamId)
    }

    /** Handles typed file metadata and consumes a previously accepted stream. */
    suspend fun handleSdkEvent(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event is AnchorSessionEvent.CapabilityRecord &&
            event.capabilitySessionId == capability.sessionId
        ) {
            when (event.typeUrl) {
                FilesProtocol.OFFER_TYPE_URL -> {
                    val offer = runCatching { FilesProtocol.decodeOffer(event.payload) }.getOrNull()
                    val valid = offer != null && offer.transferId.size == 16 &&
                        offer.sha256.size == 32 && offer.filename.isNotBlank()
                    val transferId = offer?.transferId?.let(FileTransferProtocol::toHex) ?: ""
                    if (offer != null && valid) {
                        val json = JSONObject().apply {
                            put("name", offer.filename)
                            put("size", offer.byteLength)
                            // QUIC stream receive boundaries are transport
                            // dependent and do not match the legacy 16 KiB
                            // JSON chunk size. Completion is authenticated by
                            // the offered SHA-256 instead.
                            put("chunks", 0)
                        }
                        beginIncoming(transferId, json)
                        sdkExpectedHashes[transferId] = offer.sha256
                    }
                    runCatching {
                        capability.sendRecord(
                            FilesProtocol.DECISION_TYPE_URL,
                            FilesProtocol.encodeDecision(offer?.transferId ?: ByteArray(0), valid),
                        )
                    }.onFailure { error -> Log.w(TAG, "SDK file decision failed: ${error.message}") }
                }
                FilesProtocol.CONTENT_START_TYPE_URL -> {
                    val start = runCatching { FilesProtocol.decodeContentStart(event.payload) }.getOrNull()
                    if (start != null && start.transferId.size == 16) {
                        val transferId = FileTransferProtocol.toHex(start.transferId)
                        val stream = sdkStreams.remove(start.quicStreamId)
                        if (stream != null) receiveSdkStream(transferId, stream)
                            else Log.w(TAG, "SDK file content has no accepted stream: $transferId")
                    }
                }
                FilesProtocol.COMPLETE_TYPE_URL -> {
                    val complete = runCatching { FilesProtocol.decodeComplete(event.payload) }.getOrNull()
                    if (complete != null && complete.transferId.size == 16 && complete.sha256.size == 32) {
                        val transferId = FileTransferProtocol.toHex(complete.transferId)
                        val offered = sdkExpectedHashes.remove(transferId)
                        if (offered == null || offered.contentEquals(complete.sha256)) {
                            finishIncoming(transferId, JSONObject().put("sha256", FileTransferProtocol.toHex(complete.sha256)))
                        } else {
                            Log.w(TAG, "SDK file hash differs from its offer: $transferId")
                            discardIncoming(transferId)
                        }
                    }
                }
            }
        }
    }

    private val sdkExpectedHashes = HashMap<String, ByteArray>()

    private suspend fun receiveSdkStream(transferId: String, stream: AnchorStream) {
        try {
            while (true) {
                val chunk = stream.receive()
                incoming[transferId]?.let {
                    it.lastActivityMs = System.currentTimeMillis()
                    if (chunk.bytes.isNotEmpty()) it.assembler.pushBytes(chunk.bytes)
                }
                if (chunk.finished) break
            }
        } catch (error: Exception) {
            Log.w(TAG, "SDK file stream failed for $transferId: ${error.message}")
            discardIncoming(transferId)
        }
    }

    private fun discardIncoming(transferId: String) {
        incoming.remove(transferId)?.let {
            runCatching { it.out.close() }
            it.temp.delete()
        }
    }

    // ── Inbound: desktop → Downloads ─────────────────────────────────────

    private fun handleIncoming(payload: String) {
        val json = runCatching { JSONObject(payload) }.getOrNull() ?: return
        val transferId = json.optString("transfer_id").ifEmpty { return }
        reapStale(exclude = transferId)
        when (json.optString("type")) {
            "file_offer" -> beginIncoming(transferId, json)
            "file_chunk" -> appendChunk(transferId, json)
            "file_complete" -> finishIncoming(transferId, json)
        }
    }

    private fun beginIncoming(transferId: String, json: JSONObject) {
        val name = FileTransferProtocol.sanitize(json.optString("name", "file"))
        val chunks = json.optLong("chunks", 0)
        // Drop any stale transfer reusing this id.
        incoming.remove(transferId)?.let { runCatching { it.out.close() }; it.temp.delete() }
        try {
            val temp = File.createTempFile("anchor-ft-", ".part", context.cacheDir)
            val out = FileOutputStream(temp)
            incoming[transferId] = Incoming(
                name = name,
                temp = temp,
                out = out,
                assembler = FileTransferProtocol.Assembler(out, chunks),
            )
            Log.i(TAG, "Receiving \"$name\" ($chunks chunks) from desktop")
        } catch (e: Exception) {
            Log.e(TAG, "Cannot begin transfer $transferId: ${e.message}")
        }
    }

    /** Drops transfers with no traffic for [STALE_AFTER_MS] (runs on the single
     *  event-collect coroutine, so no synchronization needed). */
    private fun reapStale(exclude: String) {
        val now = System.currentTimeMillis()
        val stale = incoming.filterValues { now - it.lastActivityMs > STALE_AFTER_MS }
            .filterKeys { it != exclude }
        for ((id, inc) in stale) {
            Log.w(TAG, "\"${inc.name}\" stalled — discarding partial")
            runCatching { inc.out.close() }
            inc.temp.delete()
            incoming.remove(id)
        }
    }

    private fun appendChunk(transferId: String, json: JSONObject) {
        val inc = incoming[transferId] ?: return
        inc.lastActivityMs = System.currentTimeMillis()
        val data = json.optString("data").ifEmpty { return }
        try {
            inc.assembler.pushChunkB64(data)
        } catch (e: Exception) {
            Log.w(TAG, "Bad chunk for $transferId: ${e.message}")
        }
    }

    private fun finishIncoming(transferId: String, json: JSONObject) {
        val inc = incoming.remove(transferId) ?: return
        val outcome = runCatching { inc.assembler.finish(json.optString("sha256")) }
            .getOrElse {
                Log.w(TAG, "Finalize failed for \"${inc.name}\": ${it.message}")
                FileTransferProtocol.Outcome.ChecksumMismatch
            }
        runCatching { inc.out.close() }

        when (outcome) {
            is FileTransferProtocol.Outcome.Incomplete -> {
                Log.w(TAG, "\"${inc.name}\" incomplete (${outcome.received}/${outcome.expected}) — discarding")
                inc.temp.delete()
            }
            is FileTransferProtocol.Outcome.ChecksumMismatch -> {
                Log.w(TAG, "\"${inc.name}\" checksum mismatch — discarding")
                inc.temp.delete()
            }
            is FileTransferProtocol.Outcome.Complete -> {
                val size = inc.temp.length()
                val savedUri = runCatching { saveToDownloads(inc.temp, inc.name) }.getOrElse {
                    Log.e(TAG, "Cannot save \"${inc.name}\": ${it.message}")
                    null
                }
                inc.temp.delete()
                if (savedUri != null) {
                    Log.i(TAG, "Saved \"${inc.name}\" to Downloads")
                    record(
                        TransferRecord(
                            name = inc.name,
                            direction = TransferDirection.RECEIVED,
                            sizeBytes = size,
                            timestamp = System.currentTimeMillis(),
                            uri = savedUri,
                            mime = guessMime(inc.name),
                        )
                    )
                    notify("File received", "${inc.name} saved to Downloads")
                }
            }
        }
    }

    /** Writes the temp file into shared Downloads. Returns the MediaStore item
     *  URI (openable via ACTION_VIEW) or null on failure. */
    private fun saveToDownloads(temp: File, name: String): Uri? {
        val resolver = context.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, guessMime(name))
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val item = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return null
        try {
            val out = resolver.openOutputStream(item)
                ?: throw IllegalStateException("cannot open output stream")
            out.use { temp.inputStream().use { input -> input.copyTo(it) } }
            values.clear()
            values.put(MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(item, values, null, null)
            return item
        } catch (e: Exception) {
            // Never leave an invisible IS_PENDING orphan behind.
            resolver.delete(item, null, null)
            throw e
        }
    }

    // ── Outbound: share sheet → desktop ──────────────────────────────────

    /** Stream a shared content URI to the desktop. Safe to call off the UI thread.
     *  When [smsTransferId] is provided (MMS attachment flow), it is preserved as
     *  the filename hint so the desktop can correlate the Files transfer back to
     *  the SMS `unique_identifier` without changing the Files wire protocol. */
    fun sendFile(uri: Uri, smsTransferId: String? = null) {
        Log.i(TAG, "sendFile requested for $uri (sdk=${sdkCapability != null}, started=${scope != null})")
        val scope = this.scope
        if (scope == null) {
            Log.w(TAG, "sendFile before plugin started — ignoring")
            return
        }
        scope.launch(Dispatchers.IO) {
            if (sdkCapability != null) {
                sendFileSdk(uri, sdkCapability!!, smsTransferId)
                return@launch
            }
            val resolver = context.contentResolver
            val name = FileTransferProtocol.sanitize(queryName(uri) ?: "file")
            val size = querySize(uri)
            val transferId = "ph-${System.nanoTime()}"

            broker.sendSuspend(device(JSONObject().apply {
                put("plugin_id", pluginId)
                put("type", "file_offer")
                put("transfer_id", transferId)
                put("name", name)
                put("size", size)
                put("chunks", FileTransferProtocol.chunkCount(size))
            }))

            val digest = MessageDigest.getInstance("SHA-256")
            var seq = 0L
            var total = 0L
            try {
                resolver.openInputStream(uri)?.use { input ->
                    val buf = ByteArray(FileTransferProtocol.CHUNK_SIZE)
                    while (true) {
                        val n = readFully(input, buf)
                        if (n <= 0) break
                        total += n
                        digest.update(buf, 0, n)
                        val data = FileTransferProtocol.encodeChunk(buf, 0, n)
                        broker.sendSuspend(device(JSONObject().apply {
                            put("plugin_id", pluginId)
                            put("type", "file_chunk")
                            put("transfer_id", transferId)
                            put("seq", seq)
                            put("data", data)
                        }))
                        seq++
                    }
                } ?: run {
                    Log.e(TAG, "Cannot open $uri for reading")
                    return@launch
                }
            } catch (e: Exception) {
                Log.e(TAG, "sendFile failed for \"$name\": ${e.message}")
                return@launch
            }

            broker.sendSuspend(device(JSONObject().apply {
                put("plugin_id", pluginId)
                put("type", "file_complete")
                put("transfer_id", transferId)
                put("sha256", FileTransferProtocol.toHex(digest.digest()))
            }))
            Log.i(TAG, "Sent \"$name\" to desktop ($seq chunks)")
            record(
                TransferRecord(
                    name = name,
                    direction = TransferDirection.SENT,
                    sizeBytes = total,
                    timestamp = System.currentTimeMillis(),
                    uri = uri,
                    mime = guessMime(name),
                )
            )
        }
    }

    /** Reliable SDK transfer: typed offer/control records plus a bound QUIC stream.
     *  When [smsTransferId] is set, the offer filename is derived from it (preserving
     *  the original extension) so the desktop's pending-attachment map can claim the
     *  correct SMS row even with concurrent requests. */
    private suspend fun sendFileSdk(uri: Uri, sdk: AnchorCapability, smsTransferId: String? = null) {
        val resolver = context.contentResolver
        val rawName = queryName(uri) ?: "file"
        val smsHint = smsTransferId?.let { FileTransferProtocol.sanitize(it) }
        val name = when {
            smsHint != null && smsHint.isNotBlank() -> {
                val origExt = rawName.substringAfterLast('.', "").takeIf { it.isNotEmpty() && rawName.contains('.') }
                    ?: guessExtensionFromUri(uri)
                if (origExt != null && !smsHint.contains('.')) "$smsHint.$origExt" else smsHint
            }
            else -> FileTransferProtocol.sanitize(rawName)
        }
        val temp = runCatching { File.createTempFile("anchor-sdk-send-", ".part", context.cacheDir) }.getOrElse {
            Log.e(TAG, "Cannot stage $name: ${it.message}"); return
        }
        val digest = MessageDigest.getInstance("SHA-256")
        var total = 0L
        try {
            resolver.openInputStream(uri)?.use { input -> temp.outputStream().use { out ->
                val buf = ByteArray(FileTransferProtocol.CHUNK_SIZE)
                while (true) { val n = input.read(buf); if (n < 0) break; if (n == 0) continue; out.write(buf, 0, n); digest.update(buf, 0, n); total += n }
            } } ?: run { Log.e(TAG, "Cannot open $uri for reading"); return }
            val id = ByteArray(16).also { java.security.SecureRandom().nextBytes(it) }
            val hash = digest.digest()
            Log.i(TAG, "SDK file offer name=$name bytes=$total")
            sdk.sendRecord(FilesProtocol.OFFER_TYPE_URL, FilesProtocol.encodeOffer(id, name, guessMime(name), total, hash))
            Log.i(TAG, "SDK opening file stream")
            val stream = withTimeout(15_000) { sdk.openStream(FilesProtocol.CONTENT_STREAM_TYPE_URL) }
            Log.i(TAG, "SDK file stream opened id=${stream.streamId}")
            sdk.sendRecord(FilesProtocol.CONTENT_START_TYPE_URL, FilesProtocol.encodeContentStart(id, stream.streamId))
            temp.inputStream().buffered().use { input ->
                var current = ByteArray(FileTransferProtocol.CHUNK_SIZE)
                var count = input.read(current)
                while (count >= 0) {
                    val next = ByteArray(FileTransferProtocol.CHUNK_SIZE)
                    val nextCount = input.read(next)
                    stream.send(current.copyOf(count), finish = nextCount < 0)
                    if (nextCount < 0) break
                    current = next; count = nextCount
                }
            }
            sdk.sendRecord(FilesProtocol.COMPLETE_TYPE_URL, FilesProtocol.encodeComplete(id, hash))
            Log.i(TAG, "Sent \"$name\" through Anchor SDK ($total bytes)")
            record(TransferRecord(name, TransferDirection.SENT, total, System.currentTimeMillis(), uri, guessMime(name)))
        } catch (error: Exception) {
            Log.e(TAG, "SDK file send failed for \"$name\": ${error.message}")
        } finally { temp.delete() }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    private fun device(json: JSONObject) =
        AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(json.toString()))

    private fun queryName(uri: Uri): String? {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c ->
                if (c.moveToFirst()) {
                    val idx = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (idx >= 0) return c.getString(idx)
                }
            }
        return uri.lastPathSegment
    }

    private fun querySize(uri: Uri): Long {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)
            ?.use { c ->
                if (c.moveToFirst()) {
                    val idx = c.getColumnIndex(OpenableColumns.SIZE)
                    if (idx >= 0 && !c.isNull(idx)) return c.getLong(idx)
                }
            }
        return -1
    }

    private fun readFully(input: InputStream, buf: ByteArray): Int {
        var off = 0
        while (off < buf.size) {
            val r = input.read(buf, off, buf.size - off)
            if (r < 0) break
            off += r
        }
        return off
    }

    private fun guessMime(name: String): String {
        val ext = name.substringAfterLast('.', "").lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext)
            ?: "application/octet-stream"
    }

    private fun guessExtensionFromUri(uri: Uri): String? {
        return try {
            val mime = context.contentResolver.getType(uri) ?: return null
            MimeTypeMap.getSingleton().getExtensionFromMimeType(mime)
        } catch (_: Exception) { null }
    }

    private fun createChannel() {
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "File transfers",
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply { description = "Files received from your computer" }
        nm.createNotificationChannel(channel)
    }

    private fun notify(title: String, text: String) {
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val n = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle(title)
            .setContentText(text)
            .setAutoCancel(true)
            .build()
        nm.notify(System.nanoTime().toInt(), n)
    }
}
