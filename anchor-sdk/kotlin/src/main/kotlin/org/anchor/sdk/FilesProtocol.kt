package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.files.FileComplete
import org.anchor.sdk.v1.capabilities.files.FileContentStart
import org.anchor.sdk.v1.capabilities.files.FileDecision
import org.anchor.sdk.v1.capabilities.files.FileOffer

/** Typed offer/decision records for reliable file streams. */
object FilesProtocol {
    data class Offer(
        val transferId: ByteArray,
        val filename: String,
        val mimeType: String,
        val byteLength: Long,
        val sha256: ByteArray,
    )
    data class Decision(val transferId: ByteArray, val accepted: Boolean)
    data class ContentStart(val transferId: ByteArray, val quicStreamId: Long)
    data class Complete(val transferId: ByteArray, val sha256: ByteArray)

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.files"
    const val CAPABILITY_MAJOR = 1
    const val OFFER_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.files.FileOffer"
    const val DECISION_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.files.FileDecision"
    const val CONTENT_START_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.files.FileContentStart"
    const val CONTENT_STREAM_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.files.FileContent"
    const val COMPLETE_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.files.FileComplete"
    fun advertisement() = CapabilityAdvertisement.newBuilder().setName(CAPABILITY_NAME).setMajor(1)
        .addRecordTypeUrls(OFFER_TYPE_URL).addRecordTypeUrls(DECISION_TYPE_URL)
        .addRecordTypeUrls(CONTENT_START_TYPE_URL).addRecordTypeUrls(CONTENT_STREAM_TYPE_URL)
        .addRecordTypeUrls(COMPLETE_TYPE_URL).build()
    fun endpointAdvertisement() = EndpointAdvertisement.newBuilder().setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeOffer(id: ByteArray, filename: String, mimeType: String, length: Long, sha256: ByteArray) = FileOffer.newBuilder().setTransferId(ByteString.copyFrom(id)).setFilename(filename).setMimeType(mimeType).setByteLength(length).setSha256(ByteString.copyFrom(sha256)).build().toByteArray()
    fun decodeOffer(bytes: ByteArray): Offer = FileOffer.parseFrom(bytes).let {
        Offer(it.transferId.toByteArray(), it.filename, it.mimeType, it.byteLength, it.sha256.toByteArray())
    }
    fun decodeDecision(bytes: ByteArray): Decision = FileDecision.parseFrom(bytes)
        .let { Decision(it.transferId.toByteArray(), it.accepted) }
    fun decodeContentStart(bytes: ByteArray): ContentStart = FileContentStart.parseFrom(bytes)
        .let { ContentStart(it.transferId.toByteArray(), it.quicStreamId) }
    fun decodeComplete(bytes: ByteArray): Complete = FileComplete.parseFrom(bytes)
        .let { Complete(it.transferId.toByteArray(), it.sha256.toByteArray()) }
    fun encodeDecision(id: ByteArray, accepted: Boolean) = FileDecision.newBuilder().setTransferId(ByteString.copyFrom(id)).setAccepted(accepted).build().toByteArray()
    fun encodeContentStart(id: ByteArray, streamId: Long) = FileContentStart.newBuilder().setTransferId(ByteString.copyFrom(id)).setQuicStreamId(streamId).build().toByteArray()
    fun encodeComplete(id: ByteArray, sha256: ByteArray) = FileComplete.newBuilder().setTransferId(ByteString.copyFrom(id)).setSha256(ByteString.copyFrom(sha256)).build().toByteArray()
}
