package org.anchor.sdk

import java.io.File
import org.anchor.sdk.v1.ControlEnvelope
import org.junit.Assert.assertEquals
import org.junit.Test

class ProtocolFixtureConformanceTest {
    @Test
    fun decodes_shared_v1_control_fixtures() {
        val expectedBodies = mapOf(
            "pairing-reject" to ControlEnvelope.BodyCase.PAIRING_REJECT,
            "capability-open" to ControlEnvelope.BodyCase.CAPABILITY_OPEN,
            "stream-open" to ControlEnvelope.BodyCase.STREAM_OPEN,
            "datagram-flow-open" to ControlEnvelope.BodyCase.DATAGRAM_FLOW_OPEN,
            "ping" to ControlEnvelope.BodyCase.PING,
            "pong" to ControlEnvelope.BodyCase.PONG,
            "capability-opened" to ControlEnvelope.BodyCase.CAPABILITY_OPENED,
            "stream-opened" to ControlEnvelope.BodyCase.STREAM_OPENED,
            "datagram-flow-opened" to ControlEnvelope.BodyCase.DATAGRAM_FLOW_OPENED,
            "session-close" to ControlEnvelope.BodyCase.SESSION_CLOSE,
        )
        expectedBodies.forEach { (name, expectedBody) ->
            val framer = ControlFramer()
            fixtureBytes(name).forEach { byte -> framer.feed(byteArrayOf(byte)) }
            assertEquals(expectedBody, framer.next()?.bodyCase)
            assertEquals(null, framer.next())
        }
    }

    private fun fixtureBytes(name: String): ByteArray {
        var root: File? = File(requireNotNull(System.getProperty("user.dir")))
        var fixture: File? = null
        while (fixture == null) {
            val currentRoot = root ?: break
            val candidate = File(currentRoot, "anchor-sdk/protocol/fixtures/v1/control/$name.hex")
            if (candidate.isFile) fixture = candidate
            root = currentRoot.parentFile
        }
        val resolvedFixture = checkNotNull(fixture) {
            "Anchor Protocol v1 fixture '$name' was not found"
        }
        val hex = resolvedFixture.readText().trim()
        return ByteArray(hex.length / 2) { index ->
            hex.substring(index * 2, index * 2 + 2).toInt(16).toByte()
        }
    }
}
