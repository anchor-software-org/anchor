package org.anchor.sdk

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DeviceProtocolTest {
    @Test
    fun stateRoundTripAndAdvertisementStayCanonical() {
        val bytes = DeviceProtocol.encodeState(
            87,
            charging = true,
            onWifi = true,
            deviceName = "SM-S931W",
            displayWidth = 1440,
            displayHeight = 3120,
        )
        val state = DeviceProtocol.decodeState(bytes)

        assertEquals(87, state.batteryPercent)
        assertTrue(state.charging)
        assertTrue(state.onWifi)
        assertEquals("SM-S931W", state.deviceName)
        assertEquals(1440, state.displayWidth)
        assertEquals(3120, state.displayHeight)
        assertEquals(DeviceProtocol.CAPABILITY_NAME, DeviceProtocol.advertisement().name)
        assertEquals(DeviceProtocol.STATE_TYPE_URL, DeviceProtocol.advertisement().recordTypeUrlsList.single())
    }
}
