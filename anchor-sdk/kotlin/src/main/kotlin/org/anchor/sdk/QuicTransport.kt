package org.anchor.sdk

import java.io.FileInputStream
import java.security.MessageDigest
import java.security.cert.CertificateFactory

/**
 * The byte-oriented QUIC boundary used by the generated protocol layer.
 *
 * A connection is not usable until [QuicEvent.Connected] is reported.
 * Implementations must use [AnchorProtocol.ALPN] and reject an unpinned peer
 * before emitting that state.
 */
interface QuicTransport : AutoCloseable {
    fun connect(request: QuicConnectRequest)
    fun openBidirectionalStream(): Long
    fun sendStream(streamId: Long, bytes: ByteArray, finish: Boolean = false)
    fun sendDatagram(bytes: ByteArray)
    fun poll(): List<QuicEvent>

    /**
     * Return native ingress-queue counters without consuming any events.
     * Implementations without a native queue may return [QuicTransportMetrics.ZERO].
     */
    fun metrics(): QuicTransportMetrics = QuicTransportMetrics.ZERO
}

data class QuicConnectRequest(
    val host: String,
    val port: Int,
    /** DNS name carried by the peer certificate; it need not be the endpoint's IP address. */
    val serverName: String,
    /** SHA-256 of the certificate DER agreed during pairing. */
    val expectedCertificateFingerprint: ByteArray,
    val localIdentity: QuicClientIdentity,
    /** PEM encoding of the exact self-signed peer certificate accepted at pairing. */
    val trustedPeerCertificatePemPath: String,
    /**
     * A one-shot enrollment connection to an address the user supplied. This
     * is never valid for a normal Anchor session: callers must use only the
     * pairing-only API and persist the returned pin before reconnecting.
     */
    val pairingBootstrap: Boolean = false,
) {
    init {
        require(host.isNotBlank()) { "host must not be blank" }
        require(port in 1..65535) { "port must be in 1..65535" }
        require(serverName.isNotBlank()) { "serverName must not be blank" }
        if (pairingBootstrap) {
            require(expectedCertificateFingerprint.isEmpty()) {
                "a pairing bootstrap must not carry a pre-existing peer pin"
            }
            require(trustedPeerCertificatePemPath.isEmpty()) {
                "a pairing bootstrap must not carry a peer trust-store path"
            }
        } else {
            require(expectedCertificateFingerprint.size == 32) {
                "expectedCertificateFingerprint must be a SHA-256 digest"
            }
            require(trustedPeerCertificatePemPath.isNotBlank()) {
                "trustedPeerCertificatePemPath must not be blank"
            }
        }
    }
}

/**
 * File references are app-private Android storage paths. The host application
 * creates and protects the key material; the SDK only gives MsQuic the exact
 * files needed for the mutually authenticated TLS handshake.
 */
data class QuicClientIdentity(
    val certificatePemPath: String,
    val privateKeyPemPath: String,
) {
    init {
        require(certificatePemPath.isNotBlank()) { "certificatePemPath must not be blank" }
        require(privateKeyPemPath.isNotBlank()) { "privateKeyPemPath must not be blank" }
    }
}

sealed interface QuicEvent {
    data object Connected : QuicEvent
    data class StreamData(val streamId: Long, val bytes: ByteArray, val finished: Boolean) : QuicEvent
    data class Datagram(val bytes: ByteArray) : QuicEvent
    data class Closed(val code: Long, val reason: String) : QuicEvent
    data class Failed(val reason: String) : QuicEvent
}

/**
 * Android's native MsQuic adapter. The library is loaded lazily so protocol and
 * pairing-only consumers do not require native code at class-load time.
 */
class MsQuicTransport : QuicTransport {
    private var handle = 0L
    /** MsQuic is thread-safe, but keeping JNI calls serialized gives every
     * platform adapter the same ordering guarantees for a stream's writes. */
    private val nativeLock = Any()

    override fun connect(request: QuicConnectRequest) {
        synchronized(nativeLock) {
            ensureLoaded()
            check(handle == 0L) { "transport is already connected or connecting" }
            if (!request.pairingBootstrap) verifyPeerCertificatePin(request)
            handle = nativeCreate(
                request.host,
                request.port,
                request.serverName,
                request.expectedCertificateFingerprint,
                request.localIdentity.certificatePemPath,
                request.localIdentity.privateKeyPemPath,
                request.trustedPeerCertificatePemPath,
                request.pairingBootstrap,
            )
            check(handle != 0L) { "MsQuic could not create a connection" }
        }
    }

    override fun openBidirectionalStream(): Long = synchronized(nativeLock) {
        requireHandle().let(::nativeOpenBidirectionalStream)
    }

    override fun sendStream(streamId: Long, bytes: ByteArray, finish: Boolean) {
        require(bytes.isNotEmpty() || finish) { "an empty non-final stream write is meaningless" }
        synchronized(nativeLock) {
            nativeSendStream(requireHandle(), streamId, bytes, finish)
        }
    }

    override fun sendDatagram(bytes: ByteArray) {
        require(bytes.isNotEmpty()) { "datagrams must not be empty" }
        synchronized(nativeLock) {
            nativeSendDatagram(requireHandle(), bytes)
        }
    }

    override fun poll(): List<QuicEvent> = synchronized(nativeLock) {
        nativePoll(requireHandle())
    }

    override fun metrics(): QuicTransportMetrics = synchronized(nativeLock) {
        // Existing installed APKs can contain a JNI library from before
        // `nativeMetrics` was added. Telemetry must not make an otherwise
        // compatible session fail; a rebuilt bridge supplies real values.
        try {
            val values = nativeMetrics(requireHandle())
            check(values.size == METRICS_FIELD_COUNT) { "invalid native metrics snapshot" }
            QuicTransportMetrics(
                queuedEvents = values[0],
                queuedBytes = values[1],
                queueHighWaterEvents = values[2],
                queueHighWaterBytes = values[3],
                enqueuedEvents = values[4],
                enqueuedBytes = values[5],
                dequeuedEvents = values[6],
                dequeuedBytes = values[7],
                pollCalls = values[8],
                polledEvents = values[9],
                polledBytes = values[10],
                available = true,
            )
        } catch (_: UnsatisfiedLinkError) {
            QuicTransportMetrics.ZERO
        }
    }

    override fun close() {
        synchronized(nativeLock) {
            if (handle != 0L) nativeClose(handle)
            handle = 0L
        }
    }

    private fun requireHandle(): Long = checkNotNull(handle.takeIf { it != 0L }) { "transport is not connected" }

    private fun verifyPeerCertificatePin(request: QuicConnectRequest) {
        val certificate = FileInputStream(request.trustedPeerCertificatePemPath).use {
            CertificateFactory.getInstance("X.509").generateCertificate(it)
        }
        val actualFingerprint = MessageDigest.getInstance("SHA-256").digest(certificate.encoded)
        require(actualFingerprint.contentEquals(request.expectedCertificateFingerprint)) {
            "trusted peer certificate does not match the pairing pin"
        }
    }

    private fun ensureLoaded() {
        if (!loaded) {
            synchronized(MsQuicTransport::class.java) {
                if (!loaded) {
                    MsQuicRuntime.libraryVersion()
                    loaded = true
                }
            }
        }
    }

    private external fun nativeCreate(
        host: String,
        port: Int,
        serverName: String,
        expectedCertificateFingerprint: ByteArray,
        certificatePemPath: String,
        privateKeyPemPath: String,
        trustedPeerCertificatePemPath: String,
        pairingBootstrap: Boolean,
    ): Long
    private external fun nativeOpenBidirectionalStream(handle: Long): Long
    private external fun nativeSendStream(handle: Long, streamId: Long, bytes: ByteArray, finish: Boolean)
    private external fun nativeSendDatagram(handle: Long, bytes: ByteArray)
    private external fun nativePoll(handle: Long): List<QuicEvent>
    private external fun nativeMetrics(handle: Long): LongArray
    private external fun nativeClose(handle: Long)

    private companion object {
        const val METRICS_FIELD_COUNT = 11
        @Volatile private var loaded = false
    }
}
