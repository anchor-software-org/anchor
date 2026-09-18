package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.capabilities.device.DeviceState

/** Canonical v1 device-state capability names and protobuf helpers. */
object DeviceProtocol {
    data class State(
        val batteryPercent: Int,
        val charging: Boolean,
        val onWifi: Boolean,
        val deviceName: String,
        val displayWidth: Int,
        val displayHeight: Int,
    )

    const val ENDPOINT_ID = "io.anchor.desktop"
    const val CAPABILITY_NAME = "org.anchor.device"
    const val CAPABILITY_MAJOR = 1
    const val STATE_TYPE_URL =
        "type.googleapis.com/anchor.v1.capabilities.device.DeviceState"

    fun advertisement(): CapabilityAdvertisement = CapabilityAdvertisement.newBuilder()
        .setName(CAPABILITY_NAME)
        .setMajor(CAPABILITY_MAJOR)
        .addRecordTypeUrls(STATE_TYPE_URL)
        .build()

    fun endpointAdvertisement(): EndpointAdvertisement = EndpointAdvertisement.newBuilder()
        .setEndpointId(ENDPOINT_ID)
        .addCapabilities(advertisement())
        .build()

    fun encodeState(
        batteryPercent: Int,
        charging: Boolean,
        onWifi: Boolean,
        deviceName: String,
        displayWidth: Int = 0,
        displayHeight: Int = 0,
    ): ByteArray {
        require(batteryPercent in 0..100) { "batteryPercent must be in 0..100" }
        require(displayWidth >= 0 && displayHeight >= 0) { "display dimensions must not be negative" }
        return DeviceState.newBuilder()
            .setBatteryPercent(batteryPercent)
            .setCharging(charging)
            .setOnWifi(onWifi)
            .setDeviceName(deviceName)
            .setDisplayWidth(displayWidth)
            .setDisplayHeight(displayHeight)
            .build()
            .toByteArray()
    }

    fun decodeState(bytes: ByteArray): State = DeviceState.parseFrom(bytes).let {
        State(it.batteryPercent, it.charging, it.onWifi, it.deviceName, it.displayWidth, it.displayHeight)
    }
}
