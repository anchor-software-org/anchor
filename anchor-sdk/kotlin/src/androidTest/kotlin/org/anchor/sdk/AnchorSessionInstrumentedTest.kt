package org.anchor.sdk

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import kotlinx.coroutines.runBlocking
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AnchorSessionInstrumentedTest {
    @Test
    fun sessionHandshakeCapabilityAndStreamWorkWithRust() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        fun copy(name: String): File = File(context.filesDir, "session-$name").also { output ->
            context.assets.open("mtls/$name").use { input -> output.outputStream().use(input::copyTo) }
        }
        val serverCertificate = copy("server-cert.pem")
        val clientCertificate = copy("client-cert.pem")
        val clientKey = copy("client-key.pem")
        // 10.0.2.2 is the emulator's host alias. Physical-device runs can
        // provide `-e anchor.test.host <host>` (or use `adb reverse` and
        // 127.0.0.1) without changing the protocol test itself.
        val host = InstrumentationRegistry.getArguments()
            .getString("anchor.test.host") ?: "10.0.2.2"
        val fingerprint = MessageDigest.getInstance("SHA-256").digest(
            CertificateFactory.getInstance("X.509").generateCertificate(serverCertificate.inputStream()).encoded,
        )
        val transport = MsQuicTransport()
        val session = AnchorSession.connect(
            transport,
            QuicConnectRequest(
                host = host,
                port = 4452,
                serverName = "anchor.test",
                expectedCertificateFingerprint = fingerprint,
                localIdentity = QuicClientIdentity(clientCertificate.path, clientKey.path),
                trustedPeerCertificatePemPath = serverCertificate.path,
            ),
            SessionIdentity(ByteArray(32) { 9 }, "Android session fixture", 2, emptyList()),
        )
        val capability = session.openCapability("io.anchor.desktop", "org.anchor.clipboard", 1)
        // The server emits a ping before the capability acknowledgement; the
        // open call preserves that unrelated event for the normal event loop.
        // This verifies request correlation rather than relying on ordering.
        assertEquals(AnchorSessionEvent.Ping(42), session.nextEvent())
        // A real typed round trip, not just raw bytes: the Rust fixture
        // decodes this with the same ClipboardProtocol helper an application
        // would use, then sends back a typed ClipboardClear acknowledgement
        // instead of only being checkable by reading its stdout.
        val originNodeId = ByteArray(32) { 9 }
        capability.sendRecord(
            ClipboardProtocol.PUBLISH_TYPE_URL,
            ClipboardProtocol.encodeText(originNodeId, revision = 4, text = "anchor-interop"),
        )
        val ack = session.nextEvent()
        check(ack is AnchorSessionEvent.CapabilityRecord && ack.typeUrl == ClipboardProtocol.CLEAR_TYPE_URL)
        val clear = ClipboardProtocol.decodeClear(ack.payload)
        assertEquals(5L, clear.revision)
        assertArrayEquals(ByteArray(32) { 3 }, clear.originNodeId)
        // Clipboard is record-oriented and deliberately does not advertise
        // datagrams. Open the screen capability for the transient datagram
        // path, keeping the clipboard capability as the reliable record path.
        val screen = session.openCapability("io.anchor.desktop", "org.anchor.screen", 1)
        val datagram = screen.openDatagramFlow(ScreenProtocol.FRAME_TYPE_URL)
        datagram.send("transient-clipboard".encodeToByteArray())
        datagram.close()
        val stream = capability.openStream(ClipboardProtocol.PUBLISH_TYPE_URL)
        stream.send("android-stream".encodeToByteArray(), finish = true)
        val response = stream.receive()
        assertEquals("rust-session-stream-response", response.bytes.decodeToString())
        check(response.finished)
        session.close()
    }

    /**
     * Run the Rust fixture with `ANCHOR_TEST_FORCE_MINOR` set to a value ahead
     * of this SDK's own minor for this test (see
     * `anchor-sdk/rust/examples/README.md`). It sends only a raw preface with
     * that inflated minor and never completes a real session.
     */
    @Test
    fun connectFailsWhenPeerMinorIsAheadOfOurs() = runBlocking {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        fun copy(name: String): File = File(context.filesDir, "session-$name").also { output ->
            context.assets.open("mtls/$name").use { input -> output.outputStream().use(input::copyTo) }
        }
        val serverCertificate = copy("server-cert.pem")
        val clientCertificate = copy("client-cert.pem")
        val clientKey = copy("client-key.pem")
        val host = InstrumentationRegistry.getArguments()
            .getString("anchor.test.host") ?: "10.0.2.2"
        val fingerprint = MessageDigest.getInstance("SHA-256").digest(
            CertificateFactory.getInstance("X.509").generateCertificate(serverCertificate.inputStream()).encoded,
        )
        val transport = MsQuicTransport()
        try {
            AnchorSession.connect(
                transport,
                QuicConnectRequest(
                    host = host,
                    port = 4452,
                    serverName = "anchor.test",
                    expectedCertificateFingerprint = fingerprint,
                    localIdentity = QuicClientIdentity(clientCertificate.path, clientKey.path),
                    trustedPeerCertificatePemPath = serverCertificate.path,
                ),
                SessionIdentity(ByteArray(32) { 9 }, "Android session fixture", 2, emptyList()),
            )
            throw AssertionError("connect should have failed against an ahead-of-us protocol minor")
        } catch (error: AnchorSessionException) {
            assertEquals("unsupported control version", error.message)
        }
    }
}
