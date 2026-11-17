package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.screen.ScreenRequestKeyframe
import org.anchor.sdk.v1.capabilities.screen.ScreenSelectOutput
import org.anchor.sdk.v1.capabilities.screen.ScreenStart
import org.anchor.sdk.v1.capabilities.screen.ScreenStop
import org.anchor.sdk.v1.capabilities.screen.ScreenStatus
import org.anchor.sdk.v1.capabilities.screen.ScreenOutputList

/** Typed control messages for the screen/display capability. */
object ScreenProtocol {
    data class Output(
        val outputId: String,
        val displayName: String,
        val width: Int,
        val height: Int,
        val refreshMilliHz: Int,
    )

    data class Status(
        val stateValue: Int,
        val outputId: String,
        val width: Int,
        val height: Int,
        val fps: Int,
        val bitrateKbps: Int,
    )

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.screen"
    const val CAPABILITY_MAJOR = 1
    const val START_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStart"
    const val STOP_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStop"
    const val SELECT_OUTPUT_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenSelectOutput"
    const val REQUEST_KEYFRAME_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenRequestKeyframe"
    const val OUTPUT_LIST_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenOutputList"
    const val STATUS_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStatus"
    const val FRAME_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.screen.ScreenFrame"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME).setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(START_TYPE_URL).addRecordTypeUrls(STOP_TYPE_URL)
        .addRecordTypeUrls(SELECT_OUTPUT_TYPE_URL).addRecordTypeUrls(REQUEST_KEYFRAME_TYPE_URL)
        .addRecordTypeUrls(OUTPUT_LIST_TYPE_URL).addRecordTypeUrls(STATUS_TYPE_URL)
        .addRecordTypeUrls(FRAME_TYPE_URL).setSupportsDatagrams(true).build()
    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder().setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeStart(maxFps: Int = 0, bitrateKbps: Int = 0, outputId: String = ""): ByteArray = ScreenStart.newBuilder().setMaxFps(maxFps).setTargetBitrateKbps(bitrateKbps).setOutputId(outputId).build().toByteArray()
    fun encodeStop(): ByteArray = ScreenStop.getDefaultInstance().toByteArray()
    fun encodeSelectOutput(outputId: String): ByteArray = ScreenSelectOutput.newBuilder().setOutputId(outputId).build().toByteArray()
    fun encodeRequestKeyframe(): ByteArray = ScreenRequestKeyframe.getDefaultInstance().toByteArray()
    fun decodeStatus(bytes: ByteArray): Status = ScreenStatus.parseFrom(bytes).let {
        Status(it.stateValue, it.outputId, it.width, it.height, it.fps, it.bitrateKbps)
    }
    fun decodeOutputList(bytes: ByteArray): List<Output> = ScreenOutputList.parseFrom(bytes).outputsList.map {
        Output(it.outputId, it.displayName, it.width, it.height, it.refreshMillihz)
    }
}
