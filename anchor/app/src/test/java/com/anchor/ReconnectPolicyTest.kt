package com.anchor

import com.anchor.data.DiscoveredDevice
import com.anchor.data.ReconnectPolicy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReconnectPolicyTest {

    private fun dev(id: String?, ip: String, name: String = id ?: "?") =
        DiscoveredDevice(deviceId = id, name = name, ip = ip, port = 5027)

    // --- Priority 1: last device visible on mDNS ---

    @Test
    fun lastDeviceOnMdnsUsesFreshIp() {
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("A", "10.0.0.5")),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.99" // stale stored IP must be ignored
        )
        assertEquals("10.0.0.5", target)
    }

    @Test
    fun lastDeviceTakesPrecedenceOverOtherSaved() {
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("B", "10.0.0.6"), dev("A", "10.0.0.5")),
            savedDeviceIds = setOf("A", "B"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = null
        )
        assertEquals("10.0.0.5", target)
    }

    // --- Priority 2: fall back to any other saved device on mDNS ---

    @Test
    fun lastDeviceOfflineFallsBackToOtherSavedOnMdns() {
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("B", "10.0.0.6")), // A not present
            savedDeviceIds = setOf("A", "B"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.99"
        )
        assertEquals("10.0.0.6", target)
    }

    @Test
    fun noLastDeviceButSavedDeviceOnMdns() {
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("B", "10.0.0.6")),
            savedDeviceIds = setOf("B"),
            lastConnectedDeviceId = null,
            lastConnectedIp = null
        )
        assertEquals("10.0.0.6", target)
    }

    @Test
    fun unpairedDiscoveredDevicesAreNotAutoConnected() {
        // "C" is discovered but NOT in savedDeviceIds → must be ignored.
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("C", "10.0.0.7"), dev(null, "10.0.0.8")),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.99"
        )
        assertEquals("10.0.0.99", target) // falls through to stored IP, not C
    }

    // --- Priority 3: blind stored-IP attempt ---

    @Test
    fun nothingOnMdnsUsesStoredIp() {
        val target = ReconnectPolicy.pickTarget(
            discovered = emptyList(),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.99"
        )
        assertEquals("10.0.0.99", target)
    }

    @Test
    fun storedIpIsNotUsedAfterTheLastDeviceWasUnpaired() {
        val target = ReconnectPolicy.pickTarget(
            discovered = emptyList(),
            savedDeviceIds = emptySet(),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.42"
        )
        assertNull(target)
    }

    // --- Nothing to do ---

    @Test
    fun returnsNullWhenNothingKnown() {
        val target = ReconnectPolicy.pickTarget(
            discovered = emptyList(),
            savedDeviceIds = emptySet(),
            lastConnectedDeviceId = null,
            lastConnectedIp = null
        )
        assertNull(target)
    }

    @Test
    fun emptyStoredIpTreatedAsAbsent() {
        val target = ReconnectPolicy.pickTarget(
            discovered = emptyList(),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = ""
        )
        assertNull(target)
    }

    @Test
    fun discoveredDeviceWithEmptyIpIsSkipped() {
        // A is on mDNS but advertised an empty address → fall back to stored IP.
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev("A", "")),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = "A",
            lastConnectedIp = "10.0.0.99"
        )
        assertEquals("10.0.0.99", target)
    }

    @Test
    fun nullDeviceIdInDiscoveryIsIgnoredForSavedMatch() {
        val target = ReconnectPolicy.pickTarget(
            discovered = listOf(dev(null, "10.0.0.8")),
            savedDeviceIds = setOf("A"),
            lastConnectedDeviceId = null,
            lastConnectedIp = null
        )
        assertNull(target)
    }

    @Test
    fun manualAddressUsesLastPairedDeviceForAlternateAddress() {
        val deviceId = ReconnectPolicy.manualAddressDeviceId(
            address = "desktop.tailnet",
            discovered = emptyList(),
            savedDeviceLastIps = mapOf("A" to "192.168.1.42", "B" to "192.168.1.77"),
            lastConnectedDeviceId = "A",
        )
        assertEquals("A", deviceId)
    }

    @Test
    fun manualAddressUsesExactKnownDeviceBeforeLastDevice() {
        val deviceId = ReconnectPolicy.manualAddressDeviceId(
            address = "10.0.0.8",
            discovered = listOf(dev("B", "10.0.0.8")),
            savedDeviceLastIps = mapOf("A" to "192.168.1.42", "B" to "192.168.1.77"),
            lastConnectedDeviceId = "A",
        )
        assertEquals("B", deviceId)
    }

    @Test
    fun manualAddressUsesSolePairedDevice() {
        val deviceId = ReconnectPolicy.manualAddressDeviceId(
            address = "desktop.tailnet",
            discovered = emptyList(),
            savedDeviceLastIps = mapOf("A" to "192.168.1.42"),
            lastConnectedDeviceId = null,
        )
        assertEquals("A", deviceId)
    }

    @Test
    fun manualAddressWithoutTrustedIdentityMustPair() {
        val deviceId = ReconnectPolicy.manualAddressDeviceId(
            address = "new-desktop.local",
            discovered = emptyList(),
            savedDeviceLastIps = mapOf("A" to "192.168.1.42", "B" to "192.168.1.77"),
            lastConnectedDeviceId = null,
        )
        assertNull(deviceId)
    }
}
