package com.anchor

import com.anchor.data.DeviceListMerger
import com.anchor.data.DiscoveredDevice
import com.anchor.data.TrustedDeviceEntry
import com.anchor.data.decodeCertificateAdvertisement
import com.anchor.data.pairingSafetyNumber
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceListMergerTest {

    private fun saved(id: String, name: String, lastIp: String? = null) =
        TrustedDeviceEntry(
            deviceId = id,
            deviceName = name,
            certificatePem = "",
            pairedAt = 0,
            lastSeen = 0,
            lastIp = lastIp
        )

    private fun dev(id: String?, ip: String, name: String = id ?: "?", wired: Boolean = false) =
        DiscoveredDevice(deviceId = id, name = name, ip = ip, port = 5027, wired = wired)

    @Test
    fun savedDeviceOnlineUsesFreshMdnsIp() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "alpha", lastIp = "10.0.0.1")),
            discovered = listOf(dev("A", "10.0.0.50"))
        )
        assertEquals(1, out.size)
        val entry = out.first()
        assertTrue(entry.isOnline)
        assertTrue(entry.isPaired)
        assertEquals("10.0.0.50", entry.ip) // fresh mDNS IP, not stored 10.0.0.1
    }

    @Test
    fun savedDeviceOnlineUsesAdvertisedHostname() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "Anchor Desktop", lastIp = "10.0.0.1")),
            discovered = listOf(dev("A", "10.0.0.50", name = "studio-pc"))
        )

        assertEquals("studio-pc", out.single().name)
    }

    @Test
    fun savedDeviceOfflineUsesStoredIp() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "alpha", lastIp = "10.0.0.1")),
            discovered = emptyList()
        )
        val entry = out.first()
        assertFalse(entry.isOnline)
        assertTrue(entry.isPaired)
        assertEquals("10.0.0.1", entry.ip)
    }

    @Test
    fun savedDeviceOfflineWithoutStoredIpHasEmptyIp() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "alpha", lastIp = null)),
            discovered = emptyList()
        )
        assertEquals("", out.first().ip)
    }

    @Test
    fun discoveredUnpairedDeviceAppearsAsNew() {
        val out = DeviceListMerger.merge(
            saved = emptyList(),
            discovered = listOf(dev("X", "10.0.0.7", name = "newbox"))
        )
        assertEquals(1, out.size)
        val entry = out.first()
        assertFalse(entry.isPaired)
        assertTrue(entry.isOnline)
        assertEquals("10.0.0.7", entry.ip)
        assertEquals("newbox", entry.name)
    }

    @Test
    fun unpairedNearbyDeviceKeepsItsPairingCertificate() {
        val certificate = byteArrayOf(1, 2, 3)
        val out = DeviceListMerger.merge(
            saved = emptyList(),
            discovered = listOf(
                DiscoveredDevice(
                    deviceId = "X",
                    name = "newbox",
                    ip = "10.0.0.7",
                    port = 5027,
                    certificateDer = certificate,
                )
            )
        )
        assertTrue(out.single().certificateDer!!.contentEquals(certificate))
        assertEquals(5027, out.single().port)
    }

    @Test
    fun certificateAdvertisementChunksReassembleLosslessly() {
        val certificate = ByteArray(900) { (it % 251).toByte() }
        val encoded = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(certificate)
        val attrs = encoded.chunked(220).mapIndexed { index, chunk ->
            "certificate$index" to chunk.toByteArray()
        }.toMap()

        assertTrue(decodeCertificateAdvertisement(attrs)!!.contentEquals(certificate))
    }

    @Test
    fun pairingSafetyNumberIsStableSixDigits() {
        val certificate = ByteArray(128) { it.toByte() }
        val safetyNumber = pairingSafetyNumber(certificate)
        assertEquals(6, safetyNumber!!.length)
        assertTrue(safetyNumber.all(Char::isDigit))
        assertEquals(safetyNumber, pairingSafetyNumber(certificate))
    }

    @Test
    fun discoveredDeviceMatchingSavedIsNotDuplicated() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "alpha", lastIp = "10.0.0.1")),
            discovered = listOf(dev("A", "10.0.0.50"))
        )
        assertEquals(1, out.size) // not 2
    }

    @Test
    fun savedDeviceReachableOnBothPathsPrefersWiredIp() {
        val out = DeviceListMerger.merge(
            saved = listOf(saved("A", "alpha", lastIp = "10.0.0.1")),
            discovered = listOf(
                dev("A", "10.0.0.50"),                          // Wi-Fi
                dev("A", "192.168.42.10", wired = true),        // USB tethering
            )
        )
        val entry = out.single()
        assertEquals("192.168.42.10", entry.ip)
        assertTrue(entry.wired)
    }

    @Test
    fun unpairedDeviceOnBothPathsCollapsesToOneWiredRow() {
        val out = DeviceListMerger.merge(
            saved = emptyList(),
            discovered = listOf(
                dev("X", "10.0.0.7", name = "newbox"),
                dev("X", "192.168.42.9", name = "newbox", wired = true),
            )
        )
        val entry = out.single()
        assertEquals("192.168.42.9", entry.ip)
        assertTrue(entry.wired)
        assertFalse(entry.isPaired)
    }

    @Test
    fun twoNullIdDevicesAreNotCollapsedTogether() {
        val out = DeviceListMerger.merge(
            saved = emptyList(),
            discovered = listOf(dev(null, "10.0.0.7"), dev(null, "10.0.0.8"))
        )
        assertEquals(2, out.size)
    }

    @Test
    fun orderingOnlineBeforeOfflineThenByName() {
        val out = DeviceListMerger.merge(
            saved = listOf(
                saved("A", "zeta", lastIp = "10.0.0.1"),   // offline
                saved("B", "beta", lastIp = "10.0.0.2"),   // online below
                saved("C", "alpha", lastIp = "10.0.0.3")   // offline
            ),
            discovered = listOf(
                dev("B", "10.0.0.20"),               // matches saved -> online paired
                dev("D", "10.0.0.40", name = "delta") // unpaired online
            )
        )
        // Online first (paired before unpaired), then offline by name.
        assertEquals(listOf("beta", "delta", "alpha", "zeta"), out.map { it.name })
        assertEquals(listOf(true, true, false, false), out.map { it.isOnline })
        assertEquals(listOf(true, false, true, true), out.map { it.isPaired })
    }

    @Test
    fun emptyInputsProduceEmptyList() {
        assertTrue(DeviceListMerger.merge(emptyList(), emptyList()).isEmpty())
    }
}
