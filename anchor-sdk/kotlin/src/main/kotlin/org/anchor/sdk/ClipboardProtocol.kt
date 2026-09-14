package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.NodeId
import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.clipboard.ClipboardClear
import org.anchor.sdk.v1.capabilities.clipboard.ClipboardPublish

/** Canonical v1 clipboard capability names and protobuf helpers. */
object ClipboardProtocol {
    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.clipboard"
    const val CAPABILITY_MAJOR = 1
    const val PUBLISH_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardPublish"
    const val CLEAR_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.clipboard.ClipboardClear"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME)
        .setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(PUBLISH_TYPE_URL)
        .addRecordTypeUrls(CLEAR_TYPE_URL)
        .build()

    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder()
        .setEndpointId(ENDPOINT_ID)
        .addCapabilities(advertisement())
        .build()

    fun encodeText(originNodeId: ByteArray, revision: Long, text: String): ByteArray {
        require(originNodeId.size == 32) { "originNodeId must be exactly 32 bytes" }
        return ClipboardPublish.newBuilder()
            .setOriginNodeId(NodeId.newBuilder().setValue(ByteString.copyFrom(originNodeId)))
            .setRevision(revision)
            .setTextUtf8(text)
            .build()
            .toByteArray()
    }

    fun encodePng(originNodeId: ByteArray, revision: Long, png: ByteArray): ByteArray {
        require(originNodeId.size == 32) { "originNodeId must be exactly 32 bytes" }
        return ClipboardPublish.newBuilder()
            .setOriginNodeId(NodeId.newBuilder().setValue(ByteString.copyFrom(originNodeId)))
            .setRevision(revision)
            .setPng(ByteString.copyFrom(png))
            .build()
            .toByteArray()
    }

    data class Publication(
        val originNodeId: ByteArray,
        val revision: Long,
        val text: String? = null,
        val png: ByteArray? = null,
    )

    data class Clear(val originNodeId: ByteArray, val revision: Long)

    fun decodePublish(bytes: ByteArray): Publication {
        val publish = ClipboardPublish.parseFrom(bytes)
        return Publication(
            originNodeId = publish.originNodeId.value.toByteArray(),
            revision = publish.revision,
            text = publish.textUtf8.takeIf { publish.hasTextUtf8() },
            png = publish.png.toByteArray().takeIf { publish.hasPng() },
        )
    }

    fun encodeClear(originNodeId: ByteArray, revision: Long): ByteArray {
        require(originNodeId.size == 32) { "originNodeId must be exactly 32 bytes" }
        return ClipboardClear.newBuilder()
            .setOriginNodeId(NodeId.newBuilder().setValue(ByteString.copyFrom(originNodeId)))
            .setRevision(revision)
            .build()
            .toByteArray()
    }

    fun decodeClear(bytes: ByteArray): Clear = ClipboardClear.parseFrom(bytes).let {
        Clear(it.originNodeId.value.toByteArray(), it.revision)
    }
}
