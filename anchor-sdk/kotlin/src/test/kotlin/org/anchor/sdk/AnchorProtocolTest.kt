package org.anchor.sdk

import org.anchor.sdk.v1.ControlEnvelope
import org.anchor.sdk.v1.PairingReject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class AnchorProtocolTest {
    @Test
    fun connectionRequestRequiresBothSidesOfMutualTls() {
        assertThrows(IllegalArgumentException::class.java) {
            QuicConnectRequest(
                host = "192.0.2.5",
                port = 4242,
                serverName = "anchor.test",
                expectedCertificateFingerprint = ByteArray(32),
                localIdentity = QuicClientIdentity("client.pem", "client-key.pem"),
                trustedPeerCertificatePemPath = "",
            )
        }
    }

    @Test
    fun pairingRejectMatchesV1FixtureEnvelope() {
        val envelope = ControlEnvelope.newBuilder().setPairingReject(PairingReject.getDefaultInstance()).build()
        assertEquals("6200", envelope.toByteArray().joinToString("") { "%02x".format(it) })
        assertEquals(ProtocolValidation.Valid, AnchorProtocol.validateControlEnvelope(envelope))
    }

    @Test
    fun validInvitationIsAccepted() {
        val certificate = ByteArray(128) { 5 }
        val invitation = PairingInvitation(
            invitationId = ByteArray(16) { 1 },
            endpoint = "192.0.2.5:4242",
            inviterDeviceId = "desktop-1",
            inviterNodeId = ByteArray(32) { 2 },
            inviterCertificateFingerprint = java.security.MessageDigest.getInstance("SHA-256").digest(certificate),
            inviterCertificateDer = certificate,
            pairingNonce = ByteArray(32) { 4 },
            expiresAtUnixMs = 101,
        )
        assertEquals(ProtocolValidation.Valid, AnchorProtocol.validatePairingInvitation(invitation, 100))
    }

    @Test
    fun invitationRejectsCertificateThatDoesNotMatchPin() {
        val invitation = PairingInvitation(
            invitationId = ByteArray(16) { 1 },
            endpoint = "192.0.2.5:5027",
            inviterDeviceId = "desktop-1",
            inviterNodeId = ByteArray(32) { 2 },
            inviterCertificateFingerprint = ByteArray(32) { 3 },
            inviterCertificateDer = ByteArray(128) { 5 },
            pairingNonce = ByteArray(32) { 4 },
            expiresAtUnixMs = 101,
        )
        assertEquals(ProtocolValidation.CertificateFingerprintMismatch, AnchorProtocol.validatePairingInvitation(invitation, 100))
    }

    @Test
    fun pairingSavesTransportVerifiedPinOnlyAfterApproval() {
        var stored: PairedPeer? = null
        val controller = PairingController(PairedPeerStore { stored = it })
        val hello = PairingHello(
            nodeId = ByteArray(32) { 9 },
            displayName = "Phone",
            deviceKindValue = 0,
            transcriptHash = ByteArray(32) { 7 },
        )
        assertEquals(ProtocolValidation.Valid, controller.receiveHello(hello, ByteArray(32) { 4 }))
        val action = controller.approve()
        assertEquals(7, (action as PairingAction.Approve).transcriptHash[0].toInt())
        assertEquals(4, stored!!.certificateFingerprint[0].toInt())
    }

    @Test
    fun failedApprovalLeavesPairingAwaitingUserDecision() {
        val controller = PairingController(PairedPeerStore { throw IllegalStateException("store unavailable") })
        val hello = PairingHello(
            nodeId = ByteArray(32) { 9 },
            displayName = "Phone",
            deviceKindValue = 2,
            transcriptHash = ByteArray(32) { 7 },
        )
        assertEquals(ProtocolValidation.Valid, controller.receiveHello(hello, ByteArray(32) { 4 }))

        assertEquals(PairingAction.StoreFailed, controller.approve())
        assertTrue(controller.state is PairingState.AwaitingUser)
    }

    @Test
    fun pairingResolutionIsAllowedOnlyOncePerRequest() {
        val controller = PairingController(PairedPeerStore { })
        val hello = PairingHello(
            nodeId = ByteArray(32) { 9 },
            displayName = "Phone",
            deviceKindValue = 2,
            transcriptHash = ByteArray(32) { 7 },
        )
        assertEquals(ProtocolValidation.Valid, controller.receiveHello(hello, ByteArray(32) { 4 }))

        assertTrue(controller.approve() is PairingAction.Approve)
        assertEquals(PairingAction.InvalidState, controller.approve())
        assertEquals(PairingAction.InvalidState, controller.reject())
    }
}
