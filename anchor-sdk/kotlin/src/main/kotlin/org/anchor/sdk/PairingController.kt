package org.anchor.sdk


data class PairedPeer(
    val nodeId: ByteArray,
    val certificateFingerprint: ByteArray,
    val displayName: String,
    val deviceKindValue: Int,
)

/** Platform adapter for encrypted preference/keystore-backed paired-peer data.
 * Implementations must run storage work away from the Android main thread. */
fun interface PairedPeerStore {
    fun save(peer: PairedPeer)
}

data class PairingRequest(
    val nodeId: ByteArray,
    val certificateFingerprint: ByteArray,
    val displayName: String,
    val deviceKindValue: Int,
    val transcriptHash: ByteArray,
)

sealed interface PairingState {
    data object Idle : PairingState
    data class AwaitingUser(val request: PairingRequest) : PairingState
    data class Paired(val peer: PairedPeer) : PairingState
    data object Rejected : PairingState
}

sealed interface PairingAction {
    data class Approve(val transcriptHash: ByteArray) : PairingAction
    data object Reject : PairingAction
    data object InvalidState : PairingAction
    data object StoreFailed : PairingAction
}

/** Pairing UI state without a socket or view dependency. The transport calls
 * receiveHello only after validating the pairing TLS certificate and passes its
 * derived fingerprint; protobuf data never supplies the trust pin. */
class PairingController(private val store: PairedPeerStore) {
    var state: PairingState = PairingState.Idle
        private set

    fun receiveHello(hello: PairingHello, certificateFingerprint: ByteArray): ProtocolValidation {
        if (hello.nodeId.size != 32) return ProtocolValidation.InvalidLength("node_id", 32, hello.nodeId.size)
        if (hello.displayName.isBlank()) return ProtocolValidation.InvalidLength("display", 1, 0)
        if (certificateFingerprint.size != 32) return ProtocolValidation.InvalidLength("certificate_fingerprint", 32, certificateFingerprint.size)
        if (hello.transcriptHash.size != 32) return ProtocolValidation.InvalidLength("transcript_hash", 32, hello.transcriptHash.size)
        state = PairingState.AwaitingUser(
            PairingRequest(
                nodeId = hello.nodeId.copyOf(),
                certificateFingerprint = certificateFingerprint.copyOf(),
                displayName = hello.displayName,
                deviceKindValue = hello.deviceKindValue,
                transcriptHash = hello.transcriptHash.copyOf(),
            ),
        )
        return ProtocolValidation.Valid
    }

    fun approve(): PairingAction {
        val request = (state as? PairingState.AwaitingUser)?.request ?: return PairingAction.InvalidState
        val peer = PairedPeer(request.nodeId, request.certificateFingerprint, request.displayName, request.deviceKindValue)
        try {
            store.save(peer)
        } catch (_: Exception) {
            return PairingAction.StoreFailed
        }
        state = PairingState.Paired(peer)
        return PairingAction.Approve(request.transcriptHash.copyOf())
    }

    fun reject(): PairingAction {
        if (state !is PairingState.AwaitingUser) return PairingAction.InvalidState
        state = PairingState.Rejected
        return PairingAction.Reject
    }
}
