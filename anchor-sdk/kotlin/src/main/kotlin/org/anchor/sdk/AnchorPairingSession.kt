package org.anchor.sdk

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import org.anchor.sdk.v1.ControlEnvelope

/** The only operations available during a pairing-only QUIC connection. */
sealed interface PairingResolution {
    data class Approved(
        val transcriptHash: ByteArray,
        /** The desktop pin returned after approval for direct-address enrollment. */
        val approverCertificateDer: ByteArray,
        val approverDeviceId: String,
    ) : PairingResolution
    data object Rejected : PairingResolution
}

/**
 * A constrained QUIC control stream used before a peer is trusted. It has no
 * capability, datagram, or content-stream surface by construction.
 */
class AnchorPairingSession private constructor(
    private val transport: QuicTransport,
    private val streamId: Long,
    private val framer: ControlFramer,
) : AutoCloseable {
    suspend fun awaitResolution(timeoutMs: Long = 60_000): PairingResolution = withContext(Dispatchers.IO) {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (System.nanoTime() < deadline) {
            framer.next()?.let { return@withContext decodeResolution(it) }
            for (event in transport.poll()) {
                when (event) {
                    is QuicEvent.StreamData -> if (event.streamId == streamId) framer.feed(event.bytes)
                    is QuicEvent.Failed -> throw AnchorSessionException(event.reason)
                    is QuicEvent.Closed -> throw AnchorSessionException("pairing connection closed: ${event.reason}")
                    else -> Unit
                }
            }
            framer.next()?.let { return@withContext decodeResolution(it) }
            delay(2)
        }
        throw AnchorSessionException("pairing approval timed out")
    }

    override fun close() = transport.close()

    companion object {
        suspend fun connect(
            transport: QuicTransport,
            request: QuicConnectRequest,
            hello: PairingHello,
            timeoutMs: Long = 5_000,
        ): AnchorPairingSession = withContext(Dispatchers.IO) {
            require(hello.nodeId.size == 32) { "PairingHello node_id must be 32 bytes" }
            require(hello.displayName.isNotBlank()) { "PairingHello needs display metadata" }
            require(hello.transcriptHash.size == 32) { "PairingHello transcript hash must be 32 bytes" }
            require(request.pairingBootstrap) { "pairing requires an unpinned bootstrap request" }
            transport.connect(request)
            val deadline = System.nanoTime() + timeoutMs * 1_000_000
            var connected = false
            while (System.nanoTime() < deadline && !connected) {
                for (event in transport.poll()) {
                    when (event) {
                        QuicEvent.Connected -> connected = true
                        is QuicEvent.Failed -> throw AnchorSessionException(event.reason)
                        is QuicEvent.Closed -> throw AnchorSessionException("pairing connection closed: ${event.reason}")
                        else -> Unit
                    }
                }
                if (!connected) delay(2)
            }
            if (!connected) throw AnchorSessionException("QUIC connection timed out")
            val streamId = transport.openBidirectionalStream()
            val envelope = ControlEnvelope.newBuilder().setPairingHello(AnchorProtocol.run { hello.toWire() }).build()
            transport.sendStream(streamId, ControlFraming.encode(envelope, first = true))
            AnchorPairingSession(transport, streamId, ControlFramer())
        }

        private fun decodeResolution(envelope: ControlEnvelope): PairingResolution = when (envelope.bodyCase) {
            ControlEnvelope.BodyCase.PAIRING_APPROVE -> {
                val approval = envelope.pairingApprove
                if (approval.transcriptHash.size() != 32
                    || approval.approverCertificateDer.isEmpty
                    || approval.approverDeviceId.isBlank()
                ) throw AnchorSessionException("pairing approval is missing trusted identity data")
                PairingResolution.Approved(
                    transcriptHash = approval.transcriptHash.toByteArray(),
                    approverCertificateDer = approval.approverCertificateDer.toByteArray(),
                    approverDeviceId = approval.approverDeviceId,
                )
            }
            ControlEnvelope.BodyCase.PAIRING_REJECT -> PairingResolution.Rejected
            else -> throw AnchorSessionException("pairing connection received a non-pairing record")
        }
    }
}
