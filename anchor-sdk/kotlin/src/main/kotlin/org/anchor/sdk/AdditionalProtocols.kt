package org.anchor.sdk

import org.anchor.sdk.v1.CapabilityAdvertisement
import org.anchor.sdk.v1.EndpointAdvertisement

/** Compatibility aliases retained for callers of the early SDK preview. */
object AdditionalProtocols {
    private fun advertisement(name: String, typeUrl: String, datagrams: Boolean) = CapabilityAdvertisement.newBuilder()
        .setName(name).setMajor(1).addRecordTypeUrls(typeUrl).setSupportsDatagrams(datagrams).build()
    private fun endpointFor(ad: CapabilityAdvertisement) = EndpointAdvertisement.newBuilder().setEndpointId("io.anchor.desktop").addCapabilities(ad).build()

    val screen = advertisement("org.anchor.screen", "type.googleapis.com/anchor.v1.capabilities.screen.ScreenStatus", true)
    val camera = advertisement("org.anchor.camera", "type.googleapis.com/anchor.v1.capabilities.camera.CameraStatus", true)
    val files = FilesProtocol.advertisement()
    val sms = SmsProtocol.advertisement()
    val commands = CommandsProtocol.advertisement()

    fun endpoint(advertisement: CapabilityAdvertisement): EndpointAdvertisement = endpointFor(advertisement)
}
