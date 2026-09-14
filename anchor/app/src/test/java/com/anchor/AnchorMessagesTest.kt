package com.anchor

import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import org.junit.Assert.*
import org.junit.Test

class AnchorMessagesTest {

    @Test
    fun targetEquality() {
        assertEquals(AnchorTarget.Gui, AnchorTarget.Gui)
        assertEquals(AnchorTarget.Device, AnchorTarget.Device)
        assertNotEquals(AnchorTarget.Gui, AnchorTarget.Device)
    }

    @Test
    fun serviceTargetEquality() {
        assertEquals(AnchorTarget.Service("sms"), AnchorTarget.Service("sms"))
        assertNotEquals(AnchorTarget.Service("sms"), AnchorTarget.Service("video"))
    }

    @Test
    fun jsonMessageEquality() {
        val msg1 = AnchorMessage.Json("""{"type":"test"}""")
        val msg2 = AnchorMessage.Json("""{"type":"test"}""")
        assertEquals(msg1, msg2)
    }

    @Test
    fun binaryMessageEquality() {
        val msg1 = AnchorMessage.Binary(byteArrayOf(1, 2, 3))
        val msg2 = AnchorMessage.Binary(byteArrayOf(1, 2, 3))
        assertEquals(msg1, msg2)

        val msg3 = AnchorMessage.Binary(byteArrayOf(4, 5, 6))
        assertNotEquals(msg1, msg3)
    }

    @Test
    fun eventEquality() {
        val e1 = AnchorEvent(AnchorTarget.Gui, AnchorMessage.Generic("hello"))
        val e2 = AnchorEvent(AnchorTarget.Gui, AnchorMessage.Generic("hello"))
        assertEquals(e1, e2)
    }

    @Test
    fun eventDifferentTargets() {
        val e1 = AnchorEvent(AnchorTarget.Gui, AnchorMessage.Generic("hello"))
        val e2 = AnchorEvent(AnchorTarget.Device, AnchorMessage.Generic("hello"))
        assertNotEquals(e1, e2)
    }

    @Test
    fun eventDifferentMessages() {
        val e1 = AnchorEvent(AnchorTarget.Gui, AnchorMessage.Generic("hello"))
        val e2 = AnchorEvent(AnchorTarget.Gui, AnchorMessage.Generic("world"))
        assertNotEquals(e1, e2)
    }
}
