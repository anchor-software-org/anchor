package org.anchor.sdk

import org.anchor.sdk.v1.capabilities.notifications.NotificationPosted
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationsProtocolTest {
    @Test
    fun postedRoundTripUsesCanonicalType() {
        val message = NotificationsProtocol.decodePosted(
            NotificationsProtocol.encodePosted(
                notificationId = "n-1",
                applicationId = "org.example.app",
                applicationName = "Example",
                title = "Hello",
                body = "World",
                postedAtUnixMs = 42,
            ),
        )
        assertEquals("n-1", message.notificationId)
        assertEquals("org.example.app", message.applicationId)
        assertEquals("Example", message.applicationName)
        assertEquals("Hello", message.title)
        assertEquals("World", message.body)
        assertEquals(42, message.postedAtUnixMs)
        assertTrue(NotificationsProtocol.advertisement().recordTypeUrlsList.contains(NotificationsProtocol.POSTED_TYPE_URL))
    }
}
