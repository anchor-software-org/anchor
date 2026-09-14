package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.sms.SmsMessage
import org.anchor.sdk.v1.capabilities.sms.SmsSend
import org.anchor.sdk.v1.capabilities.sms.SmsSendResult
import org.anchor.sdk.v1.capabilities.sms.ConversationRequest as ProtoConversationRequest
import org.anchor.sdk.v1.capabilities.sms.ConversationListRequest as ProtoConversationListRequest
import org.anchor.sdk.v1.capabilities.sms.AttachmentRequest
import org.anchor.sdk.v1.capabilities.sms.ConversationList
import org.anchor.sdk.v1.capabilities.sms.ConversationMessage
import org.anchor.sdk.v1.capabilities.sms.ConversationSnapshot
import org.anchor.sdk.v1.capabilities.sms.SmsAddress
import org.anchor.sdk.v1.capabilities.sms.SmsAttachment

/** Typed SMS records. Platform permissions remain outside the SDK. */
object SmsProtocol {
    data class Message(
        val messageId: String, val conversationId: String, val address: String,
        val body: String, val timestampUnixMs: Long, val outgoing: Boolean,
    )
    data class Send(val clientMessageId: String, val address: String, val body: String)
    data class SendResult(val clientMessageId: String, val acceptedByPlatform: Boolean)
    data class ConversationRequest(val conversationId: Long, val beforeTimestampUnixMs: Long, val limit: Int)
    data object ConversationListRequest
    data class AttachmentRequestRecord(val partId: Long, val transferId: String)
    data class Address(val address: String, val displayName: String)
    data class Attachment(
        val partId: Long,
        val mimeType: String,
        val thumbnailJpeg: ByteArray,
        val transferId: String,
    )
    data class ConversationMessageRecord(
        val messageId: Long,
        val conversationId: Long,
        val event: Int,
        val body: String,
        val addresses: List<Address>,
        val timestampUnixMs: Long,
        val messageType: Int,
        val read: Boolean,
        val subscriptionId: Long,
        val attachments: List<Attachment>,
    )
    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.sms"
    const val CAPABILITY_MAJOR = 1
    const val MESSAGE_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.SmsMessage"
    const val SEND_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.SmsSend"
    const val SEND_RESULT_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.SmsSendResult"
    const val CONVERSATION_REQUEST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.ConversationRequest"
    const val CONVERSATION_LIST_REQUEST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.ConversationListRequest"
    const val CONVERSATION_SNAPSHOT_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.ConversationSnapshot"
    const val CONVERSATION_LIST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.ConversationList"
    const val ATTACHMENT_REQUEST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.sms.AttachmentRequest"
    fun advertisement() = CapabilityAdvertisement.newBuilder().setName(CAPABILITY_NAME).setMajor(1)
        .addRecordTypeUrls(MESSAGE_TYPE_URL).addRecordTypeUrls(SEND_TYPE_URL).addRecordTypeUrls(SEND_RESULT_TYPE_URL)
        .addRecordTypeUrls(CONVERSATION_REQUEST_TYPE_URL).addRecordTypeUrls(CONVERSATION_LIST_REQUEST_TYPE_URL)
        .addRecordTypeUrls(CONVERSATION_SNAPSHOT_TYPE_URL).addRecordTypeUrls(CONVERSATION_LIST_TYPE_URL)
        .addRecordTypeUrls(ATTACHMENT_REQUEST_TYPE_URL).build()
    fun endpointAdvertisement() = EndpointAdvertisement.newBuilder().setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeMessage(id: String, conversationId: String, address: String, body: String, timestampMs: Long, outgoing: Boolean) = SmsMessage.newBuilder().setMessageId(id).setConversationId(conversationId).setAddress(address).setBody(body).setTimestampUnixMs(timestampMs).setOutgoing(outgoing).build().toByteArray()
    fun encodeSend(clientId: String, address: String, body: String) = SmsSend.newBuilder().setClientMessageId(clientId).setAddress(address).setBody(body).build().toByteArray()
    fun encodeSendResult(clientId: String, accepted: Boolean) = SmsSendResult.newBuilder().setClientMessageId(clientId).setAcceptedByPlatform(accepted).build().toByteArray()
    fun decodeSend(bytes: ByteArray): Send = SmsSend.parseFrom(bytes)
        .let { Send(it.clientMessageId, it.address, it.body) }
    fun decodeMessage(bytes: ByteArray): Message = SmsMessage.parseFrom(bytes)
        .let { Message(it.messageId, it.conversationId, it.address, it.body, it.timestampUnixMs, it.outgoing) }
    fun decodeSendResult(bytes: ByteArray): SendResult = SmsSendResult.parseFrom(bytes)
        .let { SendResult(it.clientMessageId, it.acceptedByPlatform) }
    fun decodeConversationRequest(bytes: ByteArray): ConversationRequest = ProtoConversationRequest.parseFrom(bytes)
        .let { ConversationRequest(it.conversationId, it.beforeTimestampUnixMs, it.limit) }
    fun decodeConversationListRequest(bytes: ByteArray): ConversationListRequest {
        ProtoConversationListRequest.parseFrom(bytes)
        return ConversationListRequest
    }
    fun decodeAttachmentRequest(bytes: ByteArray): AttachmentRequestRecord {
        val request = AttachmentRequest.parseFrom(bytes)
        return AttachmentRequestRecord(request.partId, request.transferId)
    }
    fun encodeConversationList(messages: List<ConversationMessageRecord>): ByteArray =
        ConversationList.newBuilder().addAllLatestMessages(messages.map(::conversationMessage)).build().toByteArray()
    fun encodeConversationSnapshot(
        conversationId: Long,
        messages: List<ConversationMessageRecord>,
        hasMoreBefore: Boolean,
    ): ByteArray = ConversationSnapshot.newBuilder()
        .setConversationId(conversationId)
        .addAllMessages(messages.map(::conversationMessage))
        .setHasMoreBefore(hasMoreBefore)
        .build()
        .toByteArray()

    private fun conversationMessage(message: ConversationMessageRecord): ConversationMessage =
        ConversationMessage.newBuilder()
            .setMessageId(message.messageId)
            .setConversationId(message.conversationId)
            .setEvent(message.event)
            .setBody(message.body)
            .addAllAddresses(message.addresses.map {
                SmsAddress.newBuilder().setAddress(it.address).setDisplayName(it.displayName).build()
            })
            .setTimestampUnixMs(message.timestampUnixMs)
            .setMessageType(message.messageType)
            .setRead(message.read)
            .setSubscriptionId(message.subscriptionId)
            .addAllAttachments(message.attachments.map {
                SmsAttachment.newBuilder()
                    .setPartId(it.partId)
                    .setMimeType(it.mimeType)
                    .setThumbnailJpeg(ByteString.copyFrom(it.thumbnailJpeg))
                    .setTransferId(it.transferId)
                    .build()
            })
            .build()
}
