package com.anchor.plugin

import android.content.ContentResolver
import android.content.Context
import android.database.ContentObserver
import android.graphics.Bitmap
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.util.Base64
import android.util.Log
import java.io.ByteArrayOutputStream
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.SmsProtocol
import org.anchor.sdk.AnchorSessionEvent

private const val TAG = "anchor"

/**
 * Replace any control characters (< 0x20) with a space.
 * org.json sometimes emits these bare inside string values, which serde_json rejects.
 */
private fun String.sanitizeForJson(): String =
    this.map { if (it.code < 0x20) ' ' else it }.joinToString("")

// Android message type constants (Telephony.TextBasedSmsColumns.MESSAGE_TYPE_*)
private const val MSG_TYPE_INBOX = 1
private const val MSG_TYPE_SENT = 2

private const val PACKET_SMS_MESSAGES = "anchor.sms.messages"
private const val PACKET_REQUEST_CONVERSATIONS = "anchor.sms.request_conversations"
private const val PACKET_REQUEST_CONVERSATION = "anchor.sms.request_conversation"
private const val PACKET_SMS_REQUEST = "anchor.sms.request"

class SmsPlugin(
    private val broker: MessageBroker,
    private val context: Context
) : Plugin {

    override val pluginId = "smsplugin"

    private var listenJob: Job? = null
    private val observerHandler = Handler(Looper.getMainLooper())
    private var smsObserver: ContentObserver? = null
    private var mmsObserver: ContentObserver? = null
    @Volatile private var sdkCapability: AnchorCapability? = null

    fun attachSdkSession(capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK SMS capability attached (session=${capability.sessionId})")
    }

    /** Handle a desktop-originated typed SMS send without routing through JSON. */
    suspend fun handleSdkRecord(event: AnchorSessionEvent) {
        val capability = sdkCapability ?: return
        if (event !is AnchorSessionEvent.CapabilityRecord ||
            event.capabilitySessionId != capability.sessionId
        ) return
        when (event.typeUrl) {
            SmsProtocol.CONVERSATION_LIST_REQUEST_TYPE_URL -> {
                runCatching { SmsProtocol.decodeConversationListRequest(event.payload) }
                    .onFailure { Log.w(TAG, "SMS: malformed SDK conversation-list request: ${it.message}") }
                    .onSuccess { sendAllConversationHeads() }
                return
            }
            SmsProtocol.CONVERSATION_REQUEST_TYPE_URL -> {
                val request = runCatching { SmsProtocol.decodeConversationRequest(event.payload) }.getOrElse {
                    Log.w(TAG, "SMS: malformed SDK conversation request: ${it.message}")
                    return
                }
                if (request.conversationId <= 0 || request.limit !in 1..250) {
                    Log.w(TAG, "SMS: rejecting invalid SDK conversation request")
                    return
                }
                sendConversation(
                    request.conversationId,
                    request.beforeTimestampUnixMs,
                    request.limit.toLong(),
                )
                return
            }
            SmsProtocol.ATTACHMENT_REQUEST_TYPE_URL -> {
                val request = runCatching { SmsProtocol.decodeAttachmentRequest(event.payload) }.getOrElse {
                    Log.w(TAG, "SMS: malformed SDK attachment request: ${it.message}")
                    return
                }
                if (request.partId <= 0 || request.transferId.isBlank()) return
                broker.send(AnchorEvent(AnchorTarget.Service("filetransfer"), AnchorMessage.Json(
                    JSONObject().put("type", "sdk_sms_attachment")
                        .put("part_id", request.partId).put("transfer_id", request.transferId).toString(),
                )))
                return
            }
            SmsProtocol.SEND_TYPE_URL -> Unit
            else -> return
        }
        val request = runCatching { SmsProtocol.decodeSend(event.payload) }.getOrElse {
            Log.w(TAG, "SMS: malformed SDK send request: ${it.message}")
            return
        }
        if (request.address.isBlank() || request.body.isEmpty()) {
            Log.w(TAG, "SMS: ignoring empty SDK send request")
            return
        }
        var accepted = false
        withContext(Dispatchers.Main) {
            try {
                sendSingleSms(request.address, request.body, null)
                accepted = true
            } catch (error: Exception) {
                Log.e(TAG, "SMS: typed SDK send failed: ${error.message}")
            }
        }
        runCatching {
            capability.sendRecord(
                SmsProtocol.SEND_RESULT_TYPE_URL,
                SmsProtocol.encodeSendResult(request.clientMessageId, accepted),
            )
        }.onFailure { Log.w(TAG, "SMS: SDK send result failed: ${it.message}") }
    }
    private var refreshConversations: Runnable? = null
    private val observationLock = Any()
    private var lastObservedSmsId = 0L
    private var lastObservedMmsId = 0L

    override fun start(scope: CoroutineScope) {
        listenJob = scope.launch {
            broker.events.collect { event ->
                if (event.target is AnchorTarget.Service &&
                    (event.target as AnchorTarget.Service).id == pluginId
                ) {
                    handleIncoming(event, scope)
                }
            }
        }
        // A request from the desktop populates its cache, but afterwards the
        // phone must also publish changes made by another person or app.  The
        // SMS/MMS providers can issue several notifications for one message,
        // so coalesce them before sending snapshots for the changed threads.
        scope.launch(Dispatchers.IO) {
            initialiseObservedMessageIds()
            // If permission was granted while a desktop was already connected,
            // its initial request happened before this plugin existed. Publish a
            // snapshot now so SMS becomes available without a reconnect.
            sendAllConversationHeads()
        }
        refreshConversations = Runnable {
            scope.launch(Dispatchers.IO) { sendChangedConversations() }
        }
        val observer = object : ContentObserver(observerHandler) {
            override fun onChange(selfChange: Boolean) {
                scheduleConversationRefresh()
            }

            override fun onChange(selfChange: Boolean, uri: Uri?) {
                scheduleConversationRefresh()
            }
        }
        smsObserver = observer
        mmsObserver = observer
        context.contentResolver.registerContentObserver(Uri.parse("content://sms"), true, observer)
        context.contentResolver.registerContentObserver(Uri.parse("content://mms"), true, observer)
        Log.i(TAG, "SmsPlugin started")
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
        refreshConversations?.let(observerHandler::removeCallbacks)
        refreshConversations = null
        smsObserver?.let(context.contentResolver::unregisterContentObserver)
        smsObserver = null
        // Both provider registrations use the same observer instance.
        mmsObserver = null
    }

    private fun scheduleConversationRefresh() {
        val refresh = refreshConversations ?: return
        observerHandler.removeCallbacks(refresh)
        observerHandler.postDelayed(refresh, 350)
    }

    /**
     * Keep a provider-local watermark so a notification only transfers the
     * conversations that actually changed. Sending all conversation heads is
     * insufficient here: it contains only the newest row and loses a burst of
     * incoming messages before the desktop can render them.
     */
    private fun initialiseObservedMessageIds() {
        synchronized(observationLock) {
            lastObservedSmsId = newestMessageId(Uri.parse("content://sms"))
            lastObservedMmsId = newestMessageId(Uri.parse("content://mms"))
        }
    }

    private suspend fun sendChangedConversations() {
        val changedThreads = synchronized(observationLock) {
            val sms = changedThreadIds(Uri.parse("content://sms"), lastObservedSmsId)
            val mms = changedThreadIds(Uri.parse("content://mms"), lastObservedMmsId)
            lastObservedSmsId = sms.newestId
            lastObservedMmsId = mms.newestId
            sms.threadIds + mms.threadIds
        }

        for (threadId in changedThreads) {
            // Include enough recent history to cover a burst, instead of only
            // the newest conversation head that triggered the observer.
            sendConversation(threadId, -1, 50)
        }
        if (changedThreads.isNotEmpty()) {
            Log.i(TAG, "SMS: published changed conversations $changedThreads")
        }
    }

    private data class ChangedThreads(val newestId: Long, val threadIds: Set<Long>)

    private fun newestMessageId(uri: Uri): Long =
        context.contentResolver.query(uri, arrayOf("_id"), null, null, "_id DESC")?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getLong(0) else 0L
        } ?: 0L

    private fun changedThreadIds(uri: Uri, afterId: Long): ChangedThreads {
        var newestId = afterId
        val threadIds = linkedSetOf<Long>()
        context.contentResolver.query(
            uri,
            arrayOf("_id", "thread_id"),
            "_id > ?",
            arrayOf(afterId.toString()),
            "_id ASC"
        )?.use { cursor ->
            val idColumn = cursor.getColumnIndex("_id")
            val threadColumn = cursor.getColumnIndex("thread_id")
            while (cursor.moveToNext()) {
                if (idColumn >= 0) newestId = maxOf(newestId, cursor.getLong(idColumn))
                if (threadColumn >= 0) threadIds.add(cursor.getLong(threadColumn))
            }
        }
        return ChangedThreads(newestId, threadIds)
    }

    private fun handleIncoming(event: AnchorEvent, scope: CoroutineScope) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return

        val json = try {
            JSONObject(msg.payload)
        } catch (e: Exception) {
            Log.w(TAG, "SMS: failed to parse JSON: ${e.message}")
            return
        }

        when (json.optString("type")) {
            PACKET_REQUEST_CONVERSATIONS -> {
                scope.launch(Dispatchers.IO) { sendAllConversationHeads() }
            }
            PACKET_REQUEST_CONVERSATION -> {
                val threadId = json.optLong("threadID", -1)
                val rangeStart = json.optLong("rangeStartTimestamp", -1)
                val numberToRequest = json.optLong("numberToRequest", 25)
                if (threadId != -1L) {
                    scope.launch(Dispatchers.IO) {
                        sendConversation(threadId, rangeStart, numberToRequest)
                    }
                }
            }
            PACKET_SMS_REQUEST -> {
                scope.launch(Dispatchers.IO) { handleSendRequest(json, scope) }
            }
        }
    }


    /**
     * Query the most-recent message from every SMS/MMS thread and push them
     * to the desktop as a single anchor.sms.messages packet.
     */
    private suspend fun sendAllConversationHeads() {
        val cr = context.contentResolver
        val messages = mutableListOf<JSONObject>()

        val smsCursor = cr.query(
            Uri.parse("content://sms"),
            arrayOf("thread_id"),
            null, null,
            "date DESC"
        )

        val threadIds = linkedSetOf<Long>() // preserves insertion order, deduplicates
        smsCursor?.use { cursor ->
            val col = cursor.getColumnIndex("thread_id")
            if (col >= 0) {
                while (cursor.moveToNext()) {
                    threadIds.add(cursor.getLong(col))
                }
            }
        }

        val mmsCursor = cr.query(
            Uri.parse("content://mms"),
            arrayOf("thread_id"),
            null, null,
            "date DESC"
        )
        mmsCursor?.use { cursor ->
            val col = cursor.getColumnIndex("thread_id")
            if (col >= 0) {
                while (cursor.moveToNext()) {
                    threadIds.add(cursor.getLong(col))
                }
            }
        }

        for (threadId in threadIds) {
            val msg = queryLatestMessageInThread(cr, threadId)
            if (msg != null) messages.add(msg)
        }

        if (messages.isEmpty()) return

        val packet = JSONObject().apply {
            put("plugin_id", pluginId)
            put("type", PACKET_SMS_MESSAGES)
            put("version", 2)
            put("messages", JSONArray(messages))
        }

        val sdk = sdkCapability
        if (sdk != null) {
            sendTypedConversationList(sdk, messages)
            Log.i(TAG, "SMS: sent ${messages.size} typed SDK conversation heads")
            return
        }
        broker.send(AnchorEvent(
            target = AnchorTarget.Device,
            message = AnchorMessage.Json(packet.toString())
        ))

        Log.i(TAG, "SMS: sent ${messages.size} conversation heads")
    }

    /**
     * Send all messages in a thread, optionally filtered by timestamp range.
     */
    private suspend fun sendConversation(
        threadId: Long,
        rangeStartTimestamp: Long,
        numberToRequest: Long
    ) {
        val cr = context.contentResolver
        val messages = querySmsMessages(cr, threadId, rangeStartTimestamp, numberToRequest) +
                       queryMmsMessages(cr, threadId, rangeStartTimestamp, numberToRequest)

        // Sort by date descending, then _id descending for stability when
        // several messages share the same millisecond (rapid sends).
        val sorted = messages.sortedWith(
            compareByDescending<JSONObject> { it.optLong("date") }
                .thenByDescending { it.optLong("_id", it.optLong("event", 0)) }
        ).take(numberToRequest.toInt())

        // Always send a packet even if empty — desktop uses it to clear the loading flag.
        val packet = JSONObject().apply {
            put("plugin_id", pluginId)
            put("type", PACKET_SMS_MESSAGES)
            put("version", 2)
            put("messages", JSONArray(sorted))
        }

        val sdk = sdkCapability
        if (sdk != null) {
            sendTypedConversationSnapshot(sdk, threadId, sorted, numberToRequest)
            Log.i(TAG, "SMS: sent ${sorted.size} typed SDK messages for thread $threadId")
            return
        }
        broker.send(AnchorEvent(
            target = AnchorTarget.Device,
            message = AnchorMessage.Json(packet.toString())
        ))

        Log.i(TAG, "SMS: sent ${sorted.size} messages for thread $threadId")
    }


    private suspend fun handleSendRequest(json: JSONObject, scope: CoroutineScope) {
        val addresses = json.optJSONArray("addresses") ?: return
        val body = json.optString("messageBody", "")
        val subId = if (json.has("sub_id")) json.getLong("sub_id") else null
        val threadId = if (json.has("threadID")) json.getLong("threadID") else -1L

        val recipients = (0 until addresses.length())
            .mapNotNull { addresses.optJSONObject(it)?.optString("address") }
            .filter { it.isNotEmpty() }

        if (recipients.isEmpty()) {
            Log.w(TAG, "SMS send request has no valid recipients")
            return
        }

        var sendOk = false
        withContext(Dispatchers.Main) {
            try {
                if (recipients.size == 1) {
                    sendSingleSms(recipients[0], body, subId)
                } else {
                    sendMultiSms(recipients, body, subId)
                }
                sendOk = true
            } catch (e: Exception) {
                Log.e(TAG, "SMS: failed to send: ${e.message}")
            }
        }

        // Push the updated conversation back so the desktop shows the sent message.
        // No delay needed — sendTextMessage() writes to the sent box synchronously.
        if (sendOk && threadId > 0) {
            Log.i(TAG, "SMS: pushing updated conversation for thread $threadId after send")
            sendConversation(threadId, -1, 10)
        }
    }

    private suspend fun sendTypedConversationList(sdk: AnchorCapability, messages: List<JSONObject>) {
        val payload = SmsProtocol.encodeConversationList(messages.map(::toTypedConversationMessage))
        runCatching { sdk.sendRecord(SmsProtocol.CONVERSATION_LIST_TYPE_URL, payload) }
            .onFailure { Log.w(TAG, "SDK SMS conversation list send failed: ${it.message}") }
    }

    private suspend fun sendTypedConversationSnapshot(
        sdk: AnchorCapability,
        threadId: Long,
        messages: List<JSONObject>,
        requestedLimit: Long,
    ) {
        val payload = SmsProtocol.encodeConversationSnapshot(
            conversationId = threadId,
            messages = messages.map(::toTypedConversationMessage),
            hasMoreBefore = messages.size >= requestedLimit,
        )
        runCatching { sdk.sendRecord(SmsProtocol.CONVERSATION_SNAPSHOT_TYPE_URL, payload) }
            .onFailure { Log.w(TAG, "SDK SMS conversation snapshot send failed: ${it.message}") }
    }

    private fun toTypedConversationMessage(json: JSONObject): SmsProtocol.ConversationMessageRecord {
        val addresses = json.optJSONArray("addresses")
        val typedAddresses = buildList {
            if (addresses != null) {
                for (index in 0 until addresses.length()) {
                    val address = addresses.optJSONObject(index) ?: continue
                    add(SmsProtocol.Address(address.optString("address", ""), address.optString("name", "")))
                }
            }
        }
        val attachments = json.optJSONArray("attachments")
        val typedAttachments = buildList {
            if (attachments != null) {
                for (index in 0 until attachments.length()) {
                    val attachment = attachments.optJSONObject(index) ?: continue
                    val thumbnail = attachment.optString("encoded_thumbnail", "")
                    val thumbnailBytes = if (thumbnail.isEmpty()) ByteArray(0) else runCatching {
                        Base64.decode(thumbnail, Base64.DEFAULT)
                    }.getOrDefault(ByteArray(0))
                    add(
                        SmsProtocol.Attachment(
                            partId = attachment.optLong("part_id", 0),
                            mimeType = attachment.optString("mime_type", ""),
                            thumbnailJpeg = thumbnailBytes,
                            transferId = attachment.optString("unique_identifier", ""),
                        ),
                    )
                }
            }
        }
        return SmsProtocol.ConversationMessageRecord(
            messageId = json.optLong("_id", json.optLong("event", 0)),
            conversationId = json.optLong("thread_id", json.optLong("threadID", 0)),
            event = json.optInt("event", 0),
            body = json.optString("body", ""),
            addresses = typedAddresses,
            timestampUnixMs = json.optLong("date", json.optLong("timestamp", 0)),
            messageType = json.optInt("type", 0),
            read = json.optBoolean("read", false),
            subscriptionId = json.optLong("sub_id", 0),
            attachments = typedAttachments,
        )
    }

    private fun sendSingleSms(recipient: String, body: String, subId: Long?) {
        val smsManager = getSmsManager(subId)
        val parts = smsManager.divideMessage(body)
        if (parts.size == 1) {
            smsManager.sendTextMessage(recipient, null, body, null, null)
        } else {
            smsManager.sendMultipartTextMessage(recipient, null, parts, null, null)
        }
        Log.i(TAG, "SMS: sent to $recipient")
    }

    private fun sendMultiSms(recipients: List<String>, body: String, subId: Long?) {
        // Group MMS send — requires SEND_SMS permission
        val smsManager = getSmsManager(subId)
        for (recipient in recipients) {
            val parts = smsManager.divideMessage(body)
            if (parts.size == 1) {
                smsManager.sendTextMessage(recipient, null, body, null, null)
            } else {
                smsManager.sendMultipartTextMessage(recipient, null, parts, null, null)
            }
        }
        Log.i(TAG, "SMS: sent to ${recipients.size} recipients")
    }

    @Suppress("DEPRECATION")
    private fun getSmsManager(subId: Long?): android.telephony.SmsManager {
        return if (subId != null && android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.LOLLIPOP_MR1) {
            android.telephony.SmsManager.getSmsManagerForSubscriptionId(subId.toInt())
        } else {
            android.telephony.SmsManager.getDefault()
        }
    }


    private fun lookupContactName(address: String): String? {
        if (address.isBlank()) return null
        return try {
            val uri = android.provider.ContactsContract.PhoneLookup.CONTENT_FILTER_URI
                .buildUpon()
                .appendPath(address)
                .build()
            context.contentResolver.query(
                uri,
                arrayOf(android.provider.ContactsContract.PhoneLookup.DISPLAY_NAME),
                null, null, null
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    cursor.getString(0)?.sanitizeForJson()?.takeIf { it.isNotBlank() }
                } else null
            }
        } catch (e: Exception) {
            Log.w(TAG, "Contact lookup failed for $address: ${e.message}")
            null
        }
    }

    /**
     * Look up the contact thumbnail photo for a phone number.
     * Returns a base64-encoded JPEG string (~64×64 px), or null if no photo is found.
     */
    private fun lookupContactPhoto(address: String): String? {
        if (address.isBlank()) return null
        return try {
            val lookupUri = android.provider.ContactsContract.PhoneLookup.CONTENT_FILTER_URI
                .buildUpon()
                .appendPath(address)
                .build()
            val contactId = context.contentResolver.query(
                lookupUri,
                arrayOf(android.provider.ContactsContract.PhoneLookup._ID),
                null, null, null
            )?.use { cursor ->
                if (cursor.moveToFirst()) cursor.getLong(0) else null
            } ?: return null

            val contactUri = android.content.ContentUris.withAppendedId(
                android.provider.ContactsContract.Contacts.CONTENT_URI,
                contactId
            )
            val photoStream = android.provider.ContactsContract.Contacts
                .openContactPhotoInputStream(context.contentResolver, contactUri, false)
                ?: return null

            val bitmap = android.graphics.BitmapFactory.decodeStream(photoStream)
            photoStream.close()
            if (bitmap == null) return null
            val scaled = Bitmap.createScaledBitmap(bitmap, 64, 64, true)
            val baos = ByteArrayOutputStream()
            scaled.compress(Bitmap.CompressFormat.JPEG, 85, baos)
            if (scaled != bitmap) scaled.recycle()
            bitmap.recycle()
            Base64.encodeToString(baos.toByteArray(), Base64.NO_WRAP)
        } catch (e: Exception) {
            Log.w(TAG, "Contact photo lookup failed for $address: ${e.message}")
            null
        }
    }


    private fun queryLatestMessageInThread(cr: ContentResolver, threadId: Long): JSONObject? {
        val smsCursor = cr.query(
            Uri.parse("content://sms"),
            arrayOf("_id", "thread_id", "address", "body", "date", "type", "read", "sub_id"),
            "thread_id = ?",
            arrayOf(threadId.toString()),
            "date DESC, _id DESC LIMIT 1"
        )

        smsCursor?.use { cursor ->
            if (cursor.moveToFirst()) {
                return buildSmsJson(cursor, threadId)
            }
        }

        val mmsCursor = cr.query(
            Uri.parse("content://mms"),
            arrayOf("_id", "thread_id", "date", "msg_box", "read", "sub_id"),
            "thread_id = ?",
            arrayOf(threadId.toString()),
            "date DESC, _id DESC LIMIT 1"
        )

        mmsCursor?.use { cursor ->
            if (cursor.moveToFirst()) {
                return buildMmsJson(cr, cursor, threadId)
            }
        }

        return null
    }

    private fun querySmsMessages(
        cr: ContentResolver,
        threadId: Long,
        rangeStartTimestamp: Long,
        limit: Long
    ): List<JSONObject> {
        val selection = buildString {
            append("thread_id = ?")
            if (rangeStartTimestamp > 0) append(" AND date < ?")
        }
        val selectionArgs = if (rangeStartTimestamp > 0) {
            arrayOf(threadId.toString(), rangeStartTimestamp.toString())
        } else {
            arrayOf(threadId.toString())
        }

        val cursor = cr.query(
            Uri.parse("content://sms"),
            arrayOf("_id", "thread_id", "address", "body", "date", "type", "read", "sub_id"),
            selection,
            selectionArgs,
            "date DESC, _id DESC LIMIT $limit"
        ) ?: return emptyList()

        return cursor.use {
            val results = mutableListOf<JSONObject>()
            while (it.moveToNext()) {
                results.add(buildSmsJson(it, threadId))
            }
            results
        }
    }

    private fun queryMmsMessages(
        cr: ContentResolver,
        threadId: Long,
        rangeStartTimestamp: Long,
        limit: Long
    ): List<JSONObject> {
        val selection = buildString {
            append("thread_id = ?")
            if (rangeStartTimestamp > 0) append(" AND date < ?")
        }
        val selectionArgs = if (rangeStartTimestamp > 0) {
            arrayOf(threadId.toString(), (rangeStartTimestamp / 1000).toString())
        } else {
            arrayOf(threadId.toString())
        }

        val cursor = cr.query(
            Uri.parse("content://mms"),
            arrayOf("_id", "thread_id", "date", "msg_box", "read", "sub_id"),
            selection,
            selectionArgs,
            "date DESC, _id DESC LIMIT $limit"
        ) ?: return emptyList()

        return cursor.use {
            val results = mutableListOf<JSONObject>()
            while (it.moveToNext()) {
                results.add(buildMmsJson(cr, it, threadId))
            }
            results
        }
    }

    private fun buildSmsJson(cursor: android.database.Cursor, threadId: Long): JSONObject {
        val id = cursor.getLong(cursor.getColumnIndexOrThrow("_id"))
        val address = (cursor.getString(cursor.getColumnIndexOrThrow("address")) ?: "").sanitizeForJson()
        val body = (cursor.getString(cursor.getColumnIndexOrThrow("body")) ?: "").sanitizeForJson()
        val date = cursor.getLong(cursor.getColumnIndexOrThrow("date"))
        val type = cursor.getInt(cursor.getColumnIndexOrThrow("type"))
        val read = cursor.getInt(cursor.getColumnIndexOrThrow("read")) != 0
        val subIdCol = cursor.getColumnIndex("sub_id")
        val subId = if (subIdCol >= 0) cursor.getLong(subIdCol) else -1L

        // event flag 1 = TEXT_MESSAGE
        val event = 1

        val contactName = lookupContactName(address)
        val contactPhoto = lookupContactPhoto(address)
        val addrObj = JSONObject().put("address", address)
        if (contactName != null) addrObj.put("name", contactName)
        if (contactPhoto != null) addrObj.put("photo", contactPhoto)
        val addresses = JSONArray().apply { put(addrObj) }

        return JSONObject().apply {
            put("_id", id)
            put("thread_id", threadId)
            put("event", event)
            put("body", body)
            put("addresses", addresses)
            put("date", date)
            put("type", type)
            put("read", read)
            if (subId >= 0) put("sub_id", subId)
            put("attachments", JSONArray())
        }
    }

    private fun buildMmsJson(
        cr: ContentResolver,
        cursor: android.database.Cursor,
        threadId: Long
    ): JSONObject {
        val id = cursor.getLong(cursor.getColumnIndexOrThrow("_id"))
        // MMS dates are in seconds; convert to ms to match SMS
        val date = cursor.getLong(cursor.getColumnIndexOrThrow("date")) * 1000L
        val msgBox = cursor.getInt(cursor.getColumnIndexOrThrow("msg_box"))
        val read = cursor.getInt(cursor.getColumnIndexOrThrow("read")) != 0
        val subIdCol = cursor.getColumnIndex("sub_id")
        val subId = if (subIdCol >= 0) cursor.getLong(subIdCol) else -1L

        // msg_box 1 = inbox, 2 = sent — same values as SMS type
        val type = msgBox

        // Addresses come from a separate table for MMS
        val addresses = queryMmsAddresses(cr, id)

        // Parts (text body + attachments)
        val (body, attachments) = queryMmsParts(cr, id)

        // event flag 2 = MULTIMEDIA_MESSAGE
        val event = 2

        return JSONObject().apply {
            put("_id", id)
            put("thread_id", threadId)
            put("event", event)
            put("body", body)
            put("addresses", addresses)
            put("date", date)
            put("type", type)
            put("read", read)
            if (subId >= 0) put("sub_id", subId)
            put("attachments", attachments)
        }
    }

    private fun queryMmsAddresses(cr: ContentResolver, mmsId: Long): JSONArray {
        val cursor = cr.query(
            Uri.parse("content://mms/$mmsId/addr"),
            arrayOf("address", "type"),
            null, null, null
        ) ?: return JSONArray()

        val result = JSONArray()
        cursor.use {
            val addressCol = it.getColumnIndex("address")
            val typeCol = it.getColumnIndex("type")
            while (it.moveToNext()) {
                val address = if (addressCol >= 0) it.getString(addressCol) else continue
                val addrType = if (typeCol >= 0) it.getInt(typeCol) else 0
                // type 137 = FROM, 151 = TO — skip self-referential "insert-address-token"
                if (address == "insert-address-token") continue
                val contactName = lookupContactName(address)
                val contactPhoto = lookupContactPhoto(address)
                result.put(JSONObject().apply {
                    put("address", address)
                    put("type", addrType)
                    if (contactName != null) put("name", contactName)
                    if (contactPhoto != null) put("photo", contactPhoto)
                })
            }
        }
        return result
    }

    /**
     * Read an MMS image part, scale it to a thumbnail, and return base64 JPEG.
     * Returns null if the part can't be read or decoded.
     */
    private fun encodeMmsThumbnail(cr: ContentResolver, partId: Long): String? {
        return try {
            val uri = Uri.parse("content://mms/part/$partId")
            val inputStream = cr.openInputStream(uri) ?: return null
            val bitmap = android.graphics.BitmapFactory.decodeStream(inputStream)
            inputStream.close()
            if (bitmap == null) return null
            val maxDim = 240
            val scale = maxDim.toFloat() / maxOf(bitmap.width, bitmap.height).toFloat()
            val w = (bitmap.width * scale).toInt().coerceAtLeast(1)
            val h = (bitmap.height * scale).toInt().coerceAtLeast(1)
            val scaled = Bitmap.createScaledBitmap(bitmap, w, h, true)
            val baos = ByteArrayOutputStream()
            scaled.compress(Bitmap.CompressFormat.JPEG, 85, baos)
            if (scaled != bitmap) scaled.recycle()
            bitmap.recycle()
            Base64.encodeToString(baos.toByteArray(), Base64.NO_WRAP)
        } catch (e: Exception) {
            Log.w(TAG, "encodeMmsThumbnail failed for part $partId: ${e.message}")
            null
        }
    }

    /**
     * Returns (bodyText, attachmentsJsonArray).
     * Text parts are concatenated; binary parts become attachment entries.
     */
    private fun queryMmsParts(cr: ContentResolver, mmsId: Long): Pair<String, JSONArray> {
        val cursor = cr.query(
            Uri.parse("content://mms/part"),
            arrayOf("_id", "ct", "text", "cl"),
            "mid = ?",
            arrayOf(mmsId.toString()),
            null
        ) ?: return Pair("", JSONArray())

        val bodyParts = mutableListOf<String>()
        val attachments = JSONArray()

        cursor.use {
            val idCol = it.getColumnIndex("_id")
            val ctCol = it.getColumnIndex("ct")
            val textCol = it.getColumnIndex("text")
            val clCol = it.getColumnIndex("cl")

            while (it.moveToNext()) {
                val partId = if (idCol >= 0) it.getLong(idCol) else continue
                val contentType = if (ctCol >= 0) it.getString(ctCol) ?: "" else ""
                val contentLocation = if (clCol >= 0) it.getString(clCol) ?: "" else ""

                when {
                    contentType == "text/plain" -> {
                        val text = if (textCol >= 0) (it.getString(textCol) ?: "").sanitizeForJson() else ""
                        if (text.isNotEmpty()) bodyParts.add(text)
                    }
                    contentType.startsWith("image/") -> {
                        val thumbnail = encodeMmsThumbnail(cr, partId)
                        attachments.put(JSONObject().apply {
                            put("part_id", partId)
                            put("mime_type", contentType)
                            put("unique_identifier", contentLocation.ifEmpty { "mms_part_$partId" })
                            if (thumbnail != null) put("encoded_thumbnail", thumbnail)
                        })
                    }
                    contentType.startsWith("video/") ||
                    contentType.startsWith("audio/") -> {
                        attachments.put(JSONObject().apply {
                            put("part_id", partId)
                            put("mime_type", contentType)
                            put("unique_identifier", contentLocation.ifEmpty { "mms_part_$partId" })
                        })
                    }
                    // smil, application/*, etc. — skip
                }
            }
        }

        return Pair(bodyParts.joinToString(" "), attachments)
    }
}
