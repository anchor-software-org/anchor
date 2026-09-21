package org.anchor.sdk

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.anchor.sdk.v1.CapabilityOpened
import org.anchor.sdk.v1.CapabilityClose
import org.anchor.sdk.v1.ControlEnvelope
import org.anchor.sdk.v1.DatagramFlowOpened
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.NodeId
import org.anchor.sdk.v1.PairingApprove
import org.anchor.sdk.v1.PeerDisplayInfo
import org.anchor.sdk.v1.ProtocolVersion
import org.anchor.sdk.v1.SessionHello
import org.anchor.sdk.v1.SessionReady
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AnchorSessionRoutingTest {
    @Test
    fun remotelyClosedCapabilityHandleCannotSendAnotherRecord() = runBlocking {
        val transport = ScriptedTransport()
        val session = connectSession(transport)
        val capability = session.openCapability(
            ClipboardProtocol.ENDPOINT_ID,
            ClipboardProtocol.CAPABILITY_NAME,
            ClipboardProtocol.CAPABILITY_MAJOR,
        )
        transport.enqueue(
            QuicEvent.StreamData(
                1,
                ControlFraming.encode(
                    ControlEnvelope.newBuilder().setCapabilityClose(
                        CapabilityClose.newBuilder().setCapabilitySessionId(capability.sessionId),
                    ).build(),
                    first = false,
                ),
                false,
            ),
        )
        assertTrue(session.nextEvent() is AnchorSessionEvent.CapabilityClosed)

        expectSessionFailure {
            capability.sendRecord(ClipboardProtocol.PUBLISH_TYPE_URL, "after-close".encodeToByteArray())
        }

        assertFalse(
            "a remotely closed capability must not send another record",
            transport.sentEnvelopes.any {
                it.bodyCase == ControlEnvelope.BodyCase.CAPABILITY_RECORD
            },
        )
    }

    @Test
    fun pairingSessionRequiresAnUnpinnedBootstrapRequest() = runBlocking {
        val transport = ScriptedTransport()
        transport.enqueue(QuicEvent.Connected)

        val error = try {
            AnchorPairingSession.connect(
                transport,
                request(),
                PairingHello(
                    nodeId = ByteArray(32) { 1 },
                    displayName = "phone",
                    deviceKindValue = 2,
                    transcriptHash = ByteArray(32) { 2 },
                ),
                timeoutMs = 100,
            )
            null
        } catch (failure: IllegalArgumentException) {
            failure
        }

        assertTrue("pairing must reject a normal pinned connection request", error != null)
        assertEquals(0, transport.connectCalls)
    }

    @Test
    fun pairingApprovalRequiresTheCompleteTrustedIdentityPayload() = runBlocking {
        val transport = ScriptedTransport()
        transport.enqueue(QuicEvent.Connected)
        val pairing = AnchorPairingSession.connect(
            transport,
            pairingRequest(),
            PairingHello(
                nodeId = ByteArray(32) { 1 },
                displayName = "phone",
                deviceKindValue = 2,
                transcriptHash = ByteArray(32) { 2 },
            ),
            timeoutMs = 100,
        )
        val incompleteApproval = ControlEnvelope.newBuilder().setPairingApprove(
            PairingApprove.newBuilder().setTranscriptHash(com.google.protobuf.ByteString.copyFrom(ByteArray(31))),
        ).build()
        transport.enqueue(
            QuicEvent.StreamData(1, ControlFraming.encode(incompleteApproval, first = true), false),
        )

        val error = expectSessionFailure { pairing.awaitResolution(timeoutMs = 100) }
        assertTrue(error.message!!.contains("pairing approval"))
    }

    @Test
    fun pairingCloseIsReportedImmediatelyWhileAwaitingApproval() = runBlocking {
        val transport = ScriptedTransport()
        transport.enqueue(QuicEvent.Connected)
        val pairing = AnchorPairingSession.connect(
            transport,
            pairingRequest(),
            PairingHello(
                nodeId = ByteArray(32) { 1 },
                displayName = "phone",
                deviceKindValue = 2,
                transcriptHash = ByteArray(32) { 2 },
            ),
            timeoutMs = 100,
        )
        transport.enqueue(QuicEvent.Closed(23, "desktop stopped pairing"))

        val error = expectSessionFailure { pairing.awaitResolution(timeoutMs = 100) }
        assertEquals("pairing connection closed: desktop stopped pairing", error.message)
    }

    @Test
    fun finishedReliableStreamDoesNotDeliverDataAfterItsFin() = runBlocking {
        val transport = ScriptedTransport()
        val session = connectSession(transport)
        val stream = AnchorStream(session, 41)

        transport.enqueue(QuicEvent.StreamData(41, "last".encodeToByteArray(), finished = true))
        assertEquals("last", stream.receive().bytes.decodeToString())
        transport.enqueue(QuicEvent.StreamData(41, "after-fin".encodeToByteArray(), finished = false))

        val failure = try {
            withTimeout(100) { stream.receive() }
            null
        } catch (error: AnchorSessionException) {
            error
        }
        assertTrue("a stream must reject data after FIN", failure != null)
    }

    @Test
    fun closedDatagramFlowCannotSendAnotherDatagram() = runBlocking {
        val transport = ScriptedTransport()
        val session = connectSession(transport, CameraProtocol.endpointAdvertisement())
        val capability = session.openCapability(
            CameraProtocol.ENDPOINT_ID,
            CameraProtocol.CAPABILITY_NAME,
            CameraProtocol.CAPABILITY_MAJOR,
        )
        val flow = capability.openDatagramFlow(CameraProtocol.FRAME_TYPE_URL)

        flow.close()
        val error = try {
            flow.send(byteArrayOf(1, 2, 3))
            null
        } catch (failure: IllegalStateException) {
            failure
        }

        assertTrue(
            "a flow closed on the control plane must not put a datagram on the transport",
            error != null && transport.sentDatagrams.isEmpty(),
        )
    }

    @Test
    fun connectionFailureFromTheFirstPollIsReportedInsteadOfTimingOut() = runBlocking {
        val transport = ScriptedTransport()
        transport.enqueue(QuicEvent.Failed("certificate rejected"))

        val error = expectSessionFailure {
            AnchorSession.connect(
                transport,
                request(),
                SessionIdentity(ByteArray(32) { 1 }, "phone", 2, emptyList()),
                timeoutMs = 20,
            )
        }

        assertEquals("certificate rejected", error.message)
    }

    @Test
    fun connectionCloseDuringTheControlHandshakeIsReportedInsteadOfTimingOut() = runBlocking {
        val transport = ScriptedTransport()
        transport.enqueue(QuicEvent.Connected)
        transport.enqueue(QuicEvent.Closed(42, "peer rejected the session"))

        val error = expectSessionFailure {
            AnchorSession.connect(
                transport,
                request(),
                SessionIdentity(ByteArray(32) { 1 }, "phone", 2, emptyList()),
                timeoutMs = 20,
            )
        }

        assertEquals("QUIC transport closed: peer rejected the session", error.message)
    }

    @Test
    fun futureSessionReadyMinorCannotMakeTheSessionUsable() = runBlocking {
        val transport = ScriptedTransport()
        val controlStreamId = 1L
        transport.enqueue(QuicEvent.Connected)
        transport.enqueue(
            QuicEvent.StreamData(
                controlStreamId,
                ControlFraming.encode(peerHello(), first = true) +
                    ControlFraming.encode(sessionReady(major = 1, minor = 1), first = false),
                false,
            ),
        )

        val error = expectSessionFailure {
            AnchorSession.connect(
                transport,
                request(),
                SessionIdentity(ByteArray(32) { 1 }, "phone", 2, emptyList()),
                timeoutMs = 100,
            )
        }

        assertEquals("unsupported protocol version", error.message)
    }

    @Test
    fun unadvertisedRecordTypeIsNeverExposedAsCapabilityEvent() = runBlocking {
        val transport = ScriptedTransport()
        val controlStreamId = 1L
        val peerHello = peerHello()
        val peerReady = sessionReady(major = 1, minor = 0)
        transport.enqueue(QuicEvent.Connected)
        transport.enqueue(
            QuicEvent.StreamData(
                controlStreamId,
                ControlFraming.encode(peerHello, first = true) + ControlFraming.encode(peerReady, first = false),
                false,
            ),
        )

        val session = AnchorSession.connect(
            transport,
            request(),
            SessionIdentity(ByteArray(32) { 1 }, "phone", 2, emptyList()),
            timeoutMs = 100,
        )
        session.openCapability(
            ClipboardProtocol.ENDPOINT_ID,
            ClipboardProtocol.CAPABILITY_NAME,
            ClipboardProtocol.CAPABILITY_MAJOR,
        )

        val malicious = ControlEnvelope.newBuilder().setCapabilityRecord(
            org.anchor.sdk.v1.CapabilityRecord.newBuilder()
                .setCapabilitySessionId(1)
                .setTypeUrl(MediaProtocol.STATE_TYPE_URL)
                .setPayload(com.google.protobuf.ByteString.copyFrom(byteArrayOf(0))),
        ).build()
        transport.enqueue(
            QuicEvent.StreamData(
                controlStreamId,
                ControlFraming.encode(malicious, first = false),
                false,
            ),
        )

        expectSessionFailure {
            session.nextEvent()
            Unit
        }
        Unit
    }

    private fun request() = QuicConnectRequest(
        host = "192.0.2.1",
        port = 4242,
        serverName = "anchor.test",
        expectedCertificateFingerprint = ByteArray(32),
        localIdentity = QuicClientIdentity("client.pem", "client-key.pem"),
        trustedPeerCertificatePemPath = "peer.pem",
    )

    private fun pairingRequest() = QuicConnectRequest(
        host = "192.0.2.1",
        port = 4242,
        serverName = "anchor.test",
        expectedCertificateFingerprint = ByteArray(0),
        localIdentity = QuicClientIdentity("client.pem", "client-key.pem"),
        trustedPeerCertificatePemPath = "",
        pairingBootstrap = true,
    )

    private fun peerHello(): ControlEnvelope = ControlEnvelope.newBuilder().setSessionHello(
        SessionHello.newBuilder()
            .setProtocolVersion(ProtocolVersion.newBuilder().setMajor(1).setMinor(0))
            .setNodeId(NodeId.newBuilder().setValue(com.google.protobuf.ByteString.copyFrom(ByteArray(32) { 9 })))
            .setDisplay(PeerDisplayInfo.newBuilder().setDisplayName("scripted peer").setDeviceKindValue(2))
            .addEndpoints(ClipboardProtocol.endpointAdvertisement())
            .build(),
    ).build()

    private suspend fun connectSession(
        transport: ScriptedTransport,
        endpoint: EndpointAdvertisement = ClipboardProtocol.endpointAdvertisement(),
    ): AnchorSession {
        transport.enqueue(QuicEvent.Connected)
        transport.enqueue(
            QuicEvent.StreamData(
                1,
                ControlFraming.encode(peerHello(endpoint), first = true) +
                    ControlFraming.encode(sessionReady(major = 1, minor = 0), first = false),
                false,
            ),
        )
        return AnchorSession.connect(
            transport,
            request(),
            SessionIdentity(ByteArray(32) { 1 }, "phone", 2, emptyList()),
            timeoutMs = 100,
        )
    }

    private fun peerHello(endpoint: EndpointAdvertisement): ControlEnvelope = ControlEnvelope.newBuilder().setSessionHello(
        SessionHello.newBuilder()
            .setProtocolVersion(ProtocolVersion.newBuilder().setMajor(1).setMinor(0))
            .setNodeId(NodeId.newBuilder().setValue(com.google.protobuf.ByteString.copyFrom(ByteArray(32) { 9 })))
            .setDisplay(PeerDisplayInfo.newBuilder().setDisplayName("scripted peer").setDeviceKindValue(2))
            .addEndpoints(endpoint)
            .build(),
    ).build()

    private fun sessionReady(major: Int, minor: Int): ControlEnvelope =
        ControlEnvelope.newBuilder().setSessionReady(
            SessionReady.newBuilder()
                .setProtocolVersion(ProtocolVersion.newBuilder().setMajor(major).setMinor(minor))
                .build(),
        ).build()

    private suspend fun expectSessionFailure(block: suspend () -> Unit): AnchorSessionException {
        try {
            block()
        } catch (error: AnchorSessionException) {
            return error
        }
        throw AssertionError("expected the session operation to fail")
    }

    private class ScriptedTransport : QuicTransport {
        private val incoming = ArrayDeque<QuicEvent>()
        private val sentControl = ControlFramer()
        val sentDatagrams = mutableListOf<ByteArray>()
        val sentEnvelopes = mutableListOf<ControlEnvelope>()
        var connectCalls = 0

        fun enqueue(event: QuicEvent) {
            incoming.addLast(event)
        }

        override fun connect(request: QuicConnectRequest) {
            connectCalls++
        }

        override fun openBidirectionalStream(): Long = 1

        override fun sendStream(streamId: Long, bytes: ByteArray, finish: Boolean) {
            sentControl.feed(bytes)
            while (true) {
                val envelope = sentControl.next() ?: return
                sentEnvelopes += envelope
                if (envelope.bodyCase == ControlEnvelope.BodyCase.CAPABILITY_OPEN) {
                    enqueue(
                        QuicEvent.StreamData(
                            streamId,
                            ControlFraming.encode(
                                ControlEnvelope.newBuilder().setResponseTo(envelope.requestId)
                                    .setCapabilityOpened(
                                        CapabilityOpened.newBuilder()
                                            .setCapabilitySessionId(envelope.capabilityOpen.capabilitySessionId),
                                    )
                                    .build(),
                                first = false,
                            ),
                            false,
                        ),
                    )
                }
                if (envelope.bodyCase == ControlEnvelope.BodyCase.DATAGRAM_FLOW_OPEN) {
                    enqueue(
                        QuicEvent.StreamData(
                            streamId,
                            ControlFraming.encode(
                                ControlEnvelope.newBuilder().setResponseTo(envelope.requestId)
                                    .setDatagramFlowOpened(
                                        DatagramFlowOpened.newBuilder()
                                            .setFlowId(envelope.datagramFlowOpen.flowId),
                                    )
                                    .build(),
                                first = false,
                            ),
                            false,
                        ),
                    )
                }
            }
        }

        override fun sendDatagram(bytes: ByteArray) {
            sentDatagrams += bytes.copyOf()
        }

        override fun poll(): List<QuicEvent> =
            if (incoming.isEmpty()) emptyList() else listOf(incoming.removeFirst())

        override fun close() = Unit
    }
}
