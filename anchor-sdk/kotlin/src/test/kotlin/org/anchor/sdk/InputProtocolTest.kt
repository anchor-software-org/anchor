package org.anchor.sdk

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class InputProtocolTest {
    @Test
    fun keyNamesMapToCanonicalUsbHidUsages() {
        assertEquals(0x04, InputProtocol.hidUsageForKey("a"))
        assertEquals(0x27, InputProtocol.hidUsageForKey("0"))
        assertEquals(0x28, InputProtocol.hidUsageForKey("Enter"))
        assertEquals(0xe0, InputProtocol.hidUsageForKey("Ctrl"))
        assertEquals(0x45, InputProtocol.hidUsageForKey("F12"))
        assertNull(InputProtocol.hidUsageForKey("not-a-key"))
    }

    @Test
    fun keyRoundTripPreservesUsageAndTransition() {
        val decoded = InputProtocol.decodeKey(InputProtocol.encodeKey(0x04, pressed = true))
        assertEquals(0x04, decoded.hidUsage)
        assertEquals(true, decoded.pressed)
    }
}
