package org.anchor.sdk

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.MessageDigest
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MsQuicRuntimeInstrumentedTest {
    @Test
    fun loadsTheBundledMsQuicRuntime() {
        assertTrue(MsQuicRuntime.libraryVersion().matches(Regex("\\d+\\.\\d+\\.\\d+\\.\\d+")))
    }

    @Test
    fun completesMutualTlsWithTheQuinnFixtureServer() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val host = InstrumentationRegistry.getArguments()
            .getString("anchor.test.host") ?: "10.0.2.2"
        fun copyFixture(name: String): File = File(context.filesDir, name).also { output ->
            context.assets.open("mtls/$name").use { input -> output.outputStream().use(input::copyTo) }
        }

        val serverCertificate = copyFixture("server-cert.pem")
        val clientCertificate = copyFixture("client-cert.pem")
        val clientKey = copyFixture("client-key.pem")
        val fingerprint = MessageDigest.getInstance("SHA-256").digest(
            java.security.cert.CertificateFactory.getInstance("X.509")
                .generateCertificate(serverCertificate.inputStream())
                .encoded,
        )
        val transport = MsQuicTransport()
        transport.connect(
            QuicConnectRequest(
                host = host,
                port = 4443,
                serverName = "anchor.test",
                expectedCertificateFingerprint = fingerprint,
                localIdentity = QuicClientIdentity(clientCertificate.path, clientKey.path),
                trustedPeerCertificatePemPath = serverCertificate.path,
            ),
        )
        val eventsBeforeData = generateSequence { transport.poll() }
            .take(100)
            .onEach { Thread.sleep(50) }
            .flatten()
            .toList()
        assertTrue(
            "MsQuic did not complete a mutual TLS handshake with Quinn; events=$eventsBeforeData",
            eventsBeforeData.any { it == QuicEvent.Connected },
        )

        val streamId = transport.openBidirectionalStream()
        transport.sendStream(streamId, "msquic-stream-request".encodeToByteArray(), finish = true)
        transport.sendDatagram("msquic-datagram-request".encodeToByteArray())
        val events = generateSequence { transport.poll() }
            .take(100)
            .onEach { Thread.sleep(50) }
            .flatten()
            .toList()
        transport.close()
        assertTrue(
            "MsQuic did not receive the Quinn stream response; events=$events",
            events.any {
                it is QuicEvent.StreamData &&
                    it.streamId == streamId &&
                    it.bytes.contentEquals("quinn-stream-response".encodeToByteArray()) &&
                    it.finished
            },
        )
        assertTrue(
            "MsQuic did not receive a peer-initiated Quinn stream; events=$events",
            events.any {
                it is QuicEvent.StreamData &&
                    it.bytes.contentEquals("quinn-peer-stream".encodeToByteArray()) &&
                    it.finished
            },
        )
        assertTrue(
            "MsQuic did not receive the Quinn datagram response; events=$events",
            events.any {
                it is QuicEvent.Datagram &&
                    it.bytes.contentEquals("quinn-datagram-response".encodeToByteArray())
            },
        )
    }
}
