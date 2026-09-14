package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.camera.CameraStatus
import org.anchor.sdk.v1.capabilities.camera.CameraStop
import org.anchor.sdk.v1.capabilities.camera.CameraStart
import org.anchor.sdk.v1.capabilities.camera.CameraControl

/** Typed camera control/status records. Encoded frames remain datagrams. */
object CameraProtocol {
    data class StartConfig(
        val maxWidth: Int,
        val maxHeight: Int,
        val maxFps: Int,
        val targetBitrateKbps: Int,
        val cameraId: String,
    )
    data class Status(
        val stateValue: Int,
        val width: Int,
        val height: Int,
        val fps: Int,
        val bitrateKbps: Int,
    )
    data class Control(
        val kindValue: Int,
        val fps: Int,
        val bitrateKbps: Int,
        val zoomRatio: Float,
        val linearZoom: Float,
        val exposure: Int,
        val torchEnabled: Boolean,
    )
    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.camera"
    const val CAPABILITY_MAJOR = 1
    const val START_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStart"
    const val STOP_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStop"
    const val STATUS_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraStatus"
    const val CODEC_CONFIG_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraCodecConfig"
    const val KEYFRAME_NEEDED_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraKeyframeNeeded"
    const val CONTROL_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraControl"
    const val FRAME_TYPE_URL = "type.googleapis.com/anchor.v1.capabilities.camera.CameraFrame"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME).setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(START_TYPE_URL).addRecordTypeUrls(STOP_TYPE_URL)
        .addRecordTypeUrls(STATUS_TYPE_URL).addRecordTypeUrls(CODEC_CONFIG_TYPE_URL)
        .addRecordTypeUrls(KEYFRAME_NEEDED_TYPE_URL).addRecordTypeUrls(CONTROL_TYPE_URL)
        .addRecordTypeUrls(FRAME_TYPE_URL).setSupportsDatagrams(true).build()
    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder().setEndpointId(ENDPOINT_ID).addCapabilities(advertisement()).build()
    fun encodeStart(width: Int, height: Int, fps: Int, bitrateKbps: Int, cameraId: String = ""): ByteArray = CameraStart.newBuilder().setMaxWidth(width).setMaxHeight(height).setMaxFps(fps).setTargetBitrateKbps(bitrateKbps).setCameraId(cameraId).build().toByteArray()
    fun encodeStop(): ByteArray = CameraStop.getDefaultInstance().toByteArray()
    fun encodeStatus(
        stateValue: Int,
        width: Int,
        height: Int,
        fps: Int,
        bitrateKbps: Int,
    ): ByteArray = CameraStatus.newBuilder()
        .setStateValue(stateValue)
        .setWidth(width)
        .setHeight(height)
        .setFps(fps)
        .setBitrateKbps(bitrateKbps)
        .build()
        .toByteArray()
    fun decodeStatus(bytes: ByteArray): Status = CameraStatus.parseFrom(bytes).let {
        Status(it.stateValue, it.width, it.height, it.fps, it.bitrateKbps)
    }
    fun decodeStart(bytes: ByteArray): StartConfig = CameraStart.parseFrom(bytes).let {
        StartConfig(it.maxWidth, it.maxHeight, it.maxFps, it.targetBitrateKbps, it.cameraId)
    }
    fun decodeControl(bytes: ByteArray): Control = CameraControl.parseFrom(bytes).let {
        Control(it.kindValue, it.fps, it.bitrateKbps, it.zoomRatio, it.linearZoom, it.exposure, it.torchEnabled)
    }
    fun encodeControl(
        kindValue: Int,
        fps: Int = 0,
        bitrateKbps: Int = 0,
        zoomRatio: Float = 0f,
        linearZoom: Float = 0f,
        exposure: Int = 0,
        torchEnabled: Boolean = false,
    ): ByteArray = CameraControl.newBuilder()
        .setKindValue(kindValue)
        .setFps(fps)
        .setBitrateKbps(bitrateKbps)
        .setZoomRatio(zoomRatio)
        .setLinearZoom(linearZoom)
        .setExposure(exposure)
        .setTorchEnabled(torchEnabled)
        .build()
        .toByteArray()
}
