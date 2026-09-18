package org.anchor.sdk

import com.google.protobuf.ByteString
import org.anchor.sdk.v1.ControlEnvelope
import org.anchor.sdk.v1.NodeId
import org.anchor.sdk.v1.PeerDisplayInfo
import org.anchor.sdk.v1.ProtocolVersion
import java.security.MessageDigest
import java.security.SecureRandom

/** Application-facing invitation data. The generated protobuf stays inside the SDK. */
data class PairingInvitation(
    val invitationId: ByteArray,
    val endpoint: String,
    val inviterNodeId: ByteArray,
    val inviterCertificateFingerprint: ByteArray,
    val expiresAtUnixMs: Long,
    val pairingNonce: ByteArray,
    val inviterCertificateDer: ByteArray,
    val inviterDeviceId: String,
)

/** Application-facing pairing hello data. The generated protobuf stays inside the SDK. */
data class PairingHello(
    val invitationId: ByteArray = ByteArray(0),
    val nodeId: ByteArray,
    val displayName: String,
    val deviceKindValue: Int,
    val transcriptHash: ByteArray,
)

/** Native Kotlin protocol helpers. Networking and UI remain host concerns. */
object AnchorProtocol {
    const val ALPN = "anchor/1"
    const val SDK_VERSION = "1.0.0"
    private const val PROTOCOL_MAJOR = 1
    private const val NODE_ID_BYTES = 32
    private const val INVITATION_ID_BYTES = 16
    private const val SHA256_BYTES = 32

    /** Creates the complete v1 nearby-pairing invitation from trusted discovery data. */
    fun createPairingInvitation(
        endpoint: String,
        inviterDeviceId: String,
        inviterCertificateDer: ByteArray,
        expiresAtUnixMs: Long,
        random: SecureRandom = SecureRandom(),
    ): PairingInvitation {
        val invitationId = ByteArray(INVITATION_ID_BYTES).also(random::nextBytes)
        val pairingNonce = ByteArray(SHA256_BYTES).also(random::nextBytes)
        return PairingInvitation(
            invitationId = invitationId,
            endpoint = endpoint,
            inviterNodeId = sha256(inviterDeviceId.toByteArray(Charsets.UTF_8)),
            inviterCertificateFingerprint = sha256(inviterCertificateDer),
            expiresAtUnixMs = expiresAtUnixMs,
            pairingNonce = pairingNonce,
            inviterCertificateDer = inviterCertificateDer.copyOf(),
            inviterDeviceId = inviterDeviceId,
        )
    }

    fun validatePairingInvitation(invitation: PairingInvitation, nowUnixMs: Long): ProtocolValidation {
        validateLength("invitation_id", invitation.invitationId.size, INVITATION_ID_BYTES)?.let { return it }
        if (invitation.endpoint.isBlank()) return ProtocolValidation.EmptyEndpoint
        if (invitation.inviterDeviceId.isBlank()) return ProtocolValidation.EmptyDeviceId
        validateLength("inviter_node_id", invitation.inviterNodeId.size, NODE_ID_BYTES)?.let { return it }
        validateLength("inviter_certificate_fingerprint", invitation.inviterCertificateFingerprint.size, SHA256_BYTES)?.let { return it }
        if (invitation.inviterCertificateDer.isEmpty() || invitation.inviterCertificateDer.size > 4096) {
            return ProtocolValidation.InvalidLength("inviter_certificate_der", 4096, invitation.inviterCertificateDer.size)
        }
        if (!sha256(invitation.inviterCertificateDer).contentEquals(invitation.inviterCertificateFingerprint)) {
            return ProtocolValidation.CertificateFingerprintMismatch
        }
        validateLength("pairing_nonce", invitation.pairingNonce.size, SHA256_BYTES)?.let { return it }
        if (invitation.expiresAtUnixMs <= nowUnixMs) return ProtocolValidation.InvitationExpired
        return ProtocolValidation.Valid
    }

    /** Binds a nearby invitation to the local client certificate pin. */
    fun pairingTranscriptHash(invitation: PairingInvitation, localCertificateFingerprint: ByteArray): ByteArray {
        require(localCertificateFingerprint.size == SHA256_BYTES) { "certificate fingerprint must be 32 bytes" }
        return sha256(invitation.toWire().toByteArray() + localCertificateFingerprint)
    }

    /** Binds a direct-address pairing request to its target and local client pin. */
    fun directPairingTranscriptHash(host: String, nonce: ByteArray, localCertificateFingerprint: ByteArray): ByteArray {
        require(localCertificateFingerprint.size == SHA256_BYTES) { "certificate fingerprint must be 32 bytes" }
        return sha256(
            "anchor/direct-pairing/v1".toByteArray(Charsets.UTF_8) +
                host.toByteArray(Charsets.UTF_8) + nonce + localCertificateFingerprint,
        )
    }

    internal fun validateControlEnvelope(envelope: ControlEnvelope): ProtocolValidation {
        if (envelope.requestId != 0L && envelope.responseTo != 0L) return ProtocolValidation.AmbiguousCorrelation
        if (envelope.bodyCase == ControlEnvelope.BodyCase.BODY_NOT_SET) return ProtocolValidation.MissingBody
        return ProtocolValidation.Valid
    }

    private fun validateLength(field: String, actual: Int, expected: Int): ProtocolValidation.InvalidLength? =
        if (actual == expected) null else ProtocolValidation.InvalidLength(field, expected, actual)

    internal fun PairingInvitation.toWire(): org.anchor.sdk.v1.PairingInvitation =
        org.anchor.sdk.v1.PairingInvitation.newBuilder()
            .setProtocolVersion(ProtocolVersion.newBuilder().setMajor(PROTOCOL_MAJOR).setMinor(0))
            .setInvitationId(ByteString.copyFrom(invitationId))
            .setEndpoint(endpoint)
            .setInviterNodeId(NodeId.newBuilder().setValue(ByteString.copyFrom(inviterNodeId)))
            .setInviterCertificateFingerprint(ByteString.copyFrom(inviterCertificateFingerprint))
            .setExpiresAtUnixMs(expiresAtUnixMs)
            .setPairingNonce(ByteString.copyFrom(pairingNonce))
            .setInviterCertificateDer(ByteString.copyFrom(inviterCertificateDer))
            .setInviterDeviceId(inviterDeviceId)
            .build()

    internal fun PairingHello.toWire(): org.anchor.sdk.v1.PairingHello =
        org.anchor.sdk.v1.PairingHello.newBuilder()
            .setInvitationId(ByteString.copyFrom(invitationId))
            .setNodeId(NodeId.newBuilder().setValue(ByteString.copyFrom(nodeId)))
            .setDisplay(PeerDisplayInfo.newBuilder().setDisplayName(displayName).setDeviceKindValue(deviceKindValue))
            .setTranscriptHash(ByteString.copyFrom(transcriptHash))
            .build()

    private fun sha256(value: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(value)
}

sealed interface ProtocolValidation {
    data object Valid : ProtocolValidation
    data class UnsupportedMajor(val major: Int) : ProtocolValidation
    data class InvalidLength(val field: String, val expected: Int, val actual: Int) : ProtocolValidation
    data object EmptyEndpoint : ProtocolValidation
    data object EmptyDeviceId : ProtocolValidation
    data object InvitationExpired : ProtocolValidation
    data object CertificateFingerprintMismatch : ProtocolValidation
    data object AmbiguousCorrelation : ProtocolValidation
    data object MissingBody : ProtocolValidation
}
