package com.anchor

import com.anchor.data.WiredDiscovery
import com.anchor.data.WiredProbe
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

class WiredDiscoveryTest {

    private fun responsePayload(
        deviceId: String? = "dev-1",
        name: String? = "studio-pc",
        port: Int? = 5027,
        certificate: ByteArray? = byteArrayOf(1, 2, 3),
    ): ByteArray {
        val fields = buildMap<String, kotlinx.serialization.json.JsonElement> {
            deviceId?.let { put("device_id", kotlinx.serialization.json.JsonPrimitive(it)) }
            name?.let { put("device_name", kotlinx.serialization.json.JsonPrimitive(it)) }
            port?.let { put("port", kotlinx.serialization.json.JsonPrimitive(it)) }
            certificate?.let {
                put(
                    "certificate",
                    kotlinx.serialization.json.JsonPrimitive(
                        Base64.getUrlEncoder().withoutPadding().encodeToString(it)
                    )
                )
            }
        }
        val body = kotlinx.serialization.json.JsonObject(fields).toString()
        return (WiredDiscovery.PROBE_RESPONSE_PREFIX + body).toByteArray(Charsets.UTF_8)
    }

    // --- Interface name detection ---

    @Test
    fun tetheredInterfaceNamesRecognized() {
        assertTrue(WiredProbe.isTetheredInterfaceName("rndis0"))
        assertTrue(WiredProbe.isTetheredInterfaceName("usb0"))
        assertTrue(WiredProbe.isTetheredInterfaceName("ncm0"))
        assertTrue(WiredProbe.isTetheredInterfaceName("rndis0.1"))
    }

    @Test
    fun nonTetheredInterfacesRejected() {
        assertFalse(WiredProbe.isTetheredInterfaceName("wlan0"))
        assertFalse(WiredProbe.isTetheredInterfaceName("lo"))
        assertFalse(WiredProbe.isTetheredInterfaceName("eth0"))
        assertFalse(WiredProbe.isTetheredInterfaceName("ccmni0"))
        assertFalse(WiredProbe.isTetheredInterfaceName("bt-pan"))
        assertFalse(WiredProbe.isTetheredInterfaceName("tun0"))
    }

    // --- Broadcast address computation ---

    @Test
    fun computeBroadcastSlash24() {
        val address = byteArrayOf(192.toByte(), 168.toByte(), 42.toByte(), 129.toByte())
        val broadcast = WiredProbe.computeBroadcast(address, 24)
        assertEquals(listOf(192, 168, 42, 255), broadcast!!.map { it.toInt() and 0xff })
    }

    @Test
    fun computeBroadcastSlash25() {
        val address = byteArrayOf(10.toByte(), 0.toByte(), 0.toByte(), 10.toByte())
        val broadcast = WiredProbe.computeBroadcast(address, 25)
        assertEquals(listOf(10, 0, 0, 127), broadcast!!.map { it.toInt() and 0xff })
    }

    @Test
    fun computeBroadcastRejectsBadPrefix() {
        val address = byteArrayOf(10, 0, 0, 1)
        assertNull(WiredProbe.computeBroadcast(address, -1))
        assertNull(WiredProbe.computeBroadcast(address, 33))
    }

    // --- Response parsing ---

    @Test
    fun parseResponseReturnsWiredDeviceFromSourceIp() {
        val payload = responsePayload()
        val device = WiredProbe.parseResponse(payload, payload.size, "192.168.42.10")
        assertEquals("dev-1", device!!.deviceId)
        assertEquals("studio-pc", device.name)
        assertEquals("192.168.42.10", device.ip)
        assertEquals(5027, device.port)
        assertTrue(device.wired)
        assertTrue(device.certificateDer!!.contentEquals(byteArrayOf(1, 2, 3)))
    }

    @Test
    fun parseResponseRejectsMissingPrefix() {
        val body = """{"device_id":"dev-1","port":5027}""".toByteArray()
        assertNull(WiredProbe.parseResponse(body, body.size, "192.168.42.10"))
    }

    @Test
    fun parseResponseRejectsMalformedJson() {
        val payload = (WiredDiscovery.PROBE_RESPONSE_PREFIX + "not json").toByteArray()
        assertNull(WiredProbe.parseResponse(payload, payload.size, "192.168.42.10"))
    }

    @Test
    fun parseResponseRejectsMissingIdentityFields() {
        val noId = responsePayload(deviceId = null)
        assertNull(WiredProbe.parseResponse(noId, noId.size, "192.168.42.10"))
        val noPort = responsePayload(port = null)
        assertNull(WiredProbe.parseResponse(noPort, noPort.size, "192.168.42.10"))
    }

    @Test
    fun parseResponseRejectsBadPort() {
        val zero = responsePayload(port = 0)
        assertNull(WiredProbe.parseResponse(zero, zero.size, "192.168.42.10"))
        val big = responsePayload(port = 70000)
        assertNull(WiredProbe.parseResponse(big, big.size, "192.168.42.10"))
    }

    @Test
    fun parseResponseRejectsOversizedCertificate() {
        val payload = responsePayload(certificate = ByteArray(5000))
        val device = WiredProbe.parseResponse(payload, payload.size, "192.168.42.10")
        assertNull(device!!.certificateDer)
    }

    @Test
    fun parseResponseRejectsOversizedDeviceId() {
        val payload = responsePayload(deviceId = "x".repeat(200))
        assertNull(WiredProbe.parseResponse(payload, payload.size, "192.168.42.10"))
    }

    @Test
    fun parseResponseHandlesMissingOptionalFields() {
        val payload = responsePayload(name = null, certificate = null)
        val device = WiredProbe.parseResponse(payload, payload.size, "192.168.42.10")
        assertEquals("192.168.42.10", device!!.name) // falls back to source IP
        assertNull(device.certificateDer)
    }

    @Test
    fun parseResponseTruncatesAtPacketLength() {
        val payload = responsePayload() + byteArrayOf(0)
        val device = WiredProbe.parseResponse(payload, payload.size - 1, "192.168.42.10")
        assertEquals("dev-1", device!!.deviceId)
    }
}
