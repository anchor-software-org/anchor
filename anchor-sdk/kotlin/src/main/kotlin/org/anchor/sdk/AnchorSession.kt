package org.anchor.sdk

import com.google.protobuf.ByteString
import java.util.ArrayDeque
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.anchor.sdk.v1.CapabilityOpened
import org.anchor.sdk.v1.ControlEnvelope
import org.anchor.sdk.v1.EndpointAdvertisement
import org.anchor.sdk.v1.ProtocolVersion
import org.anchor.sdk.v1.SessionHello
import org.anchor.sdk.v1.SessionReady
import org.anchor.sdk.v1.StreamOpen
import org.anchor.sdk.v1.StreamOpened

private const val PROTOCOL_MAJOR = 1
private const val PROTOCOL_MINOR = 0
private const val NODE_ID_BYTES = 32
private const val MAX_CONTROL_RECORD_BYTES = 1024 * 1024
private val CONTROL_MAGIC = byteArrayOf('A'.code.toByte(), 'N'.code.toByte(), 'C'.code.toByte(), 'R'.code.toByte())

data class SessionIdentity(
    val nodeId: ByteArray,
    val displayName: String,
    val deviceKindValue: Int,
    val endpoints: List<EndpointAdvertisement>,
) {
    init {
        require(nodeId.size == NODE_ID_BYTES) { "nodeId must be exactly 32 bytes" }
        require(displayName.isNotBlank()) { "displayName must not be blank" }
    }

    internal fun hello(): SessionHello = SessionHello.newBuilder()
        .setProtocolVersion(ProtocolVersion.newBuilder().setMajor(PROTOCOL_MAJOR).setMinor(PROTOCOL_MINOR))
        .setNodeId(org.anchor.sdk.v1.NodeId.newBuilder().setValue(ByteString.copyFrom(nodeId)))
        .setDisplay(org.anchor.sdk.v1.PeerDisplayInfo.newBuilder().setDisplayName(displayName).setDeviceKindValue(deviceKindValue))
        .addAllEndpoints(endpoints)
        .build()
}

data class SessionPeer(
    val nodeId: ByteArray,
    val displayName: String,
    val deviceKindValue: Int,
    val endpoints: List<EndpointAdvertisement>,
)

sealed interface AnchorSessionEvent {
    data class CapabilityOpenRequested(
        val requestId: Long,
        val capabilitySessionId: Long,
        val endpointId: String,
        val capabilityName: String,
        val capabilityMajor: Int,
    ) : AnchorSessionEvent
    data class CapabilityOpened(val responseTo: Long, val capabilitySessionId: Long) : AnchorSessionEvent
    data class CapabilityClosed(val capabilitySessionId: Long, val reasonValue: Int) : AnchorSessionEvent
    data class CapabilityRecord(val capabilitySessionId: Long, val typeUrl: String, val payload: ByteArray) : AnchorSessionEvent
    data class StreamOpenRequested(val requestId: Long, val quicStreamId: Long, val capabilitySessionId: Long, val payloadTypeUrl: String) : AnchorSessionEvent
    data class StreamOpened(val responseTo: Long, val quicStreamId: Long) : AnchorSessionEvent
    data class StreamClosed(val quicStreamId: Long) : AnchorSessionEvent
    data class DatagramFlowOpenRequested(
        val requestId: Long,
        val capabilitySessionId: Long,
        val flowId: Long,
        val payloadTypeUrl: String,
    ) : AnchorSessionEvent
    data class DatagramFlowOpened(val responseTo: Long, val flowId: Long) : AnchorSessionEvent
    data class DatagramFlowClosed(val flowId: Long) : AnchorSessionEvent
    data class Ping(val nonce: Long) : AnchorSessionEvent
    data class Pong(val nonce: Long) : AnchorSessionEvent
    data class ProtocolError(val responseTo: Long, val codeValue: Int, val message: String) : AnchorSessionEvent
    data class SessionClosed(val reasonValue: Int) : AnchorSessionEvent
}

class AnchorSessionException(message: String) : Exception(message)

/** An authenticated, protocol-ready session over the byte-oriented QUIC API. */
class AnchorSession private constructor(
    internal val transport: QuicTransport,
    internal val controlStreamId: Long,
    val peer: SessionPeer,
    private val framer: ControlFramer,
) : AutoCloseable {
    private val queuedEvents = ArrayDeque<AnchorSessionEvent>()
    private val queuedStreamData = mutableMapOf<Long, ArrayDeque<AnchorStreamData>>()
    private val queuedDatagrams = ArrayDeque<ByteArray>()
    /* Queue containers and their counters share one lock. This makes a
     * snapshot describe a real state rather than a mixture of two moments. */
    private val queueLock = Any()
    private val eventQueueAccounting = QueueAccounting()
    private val streamQueueAccounting = QueueAccounting()
    private val datagramQueueAccounting = QueueAccounting()
    private val pollMutex = Mutex()
    // A session has one control stream, but capability operations and the
    // SDK event router may wait on it concurrently. Serialize reads and queue
    // unrelated events so a response (especially StreamOpened) cannot be
    // consumed by the wrong coroutine.
    private val eventMutex = Mutex()
    private val sendMutex = Mutex()
    private var nextRequestId = 1L
    private var nextCapabilityId = 1L
    private var nextFlowId = 1L
    private var closedReason: Int? = null

    suspend fun nextEvent(): AnchorSessionEvent {
        return readEvent()
    }

    suspend fun openCapability(endpointId: String, capabilityName: String, capabilityMajor: Int): AnchorCapability {
        val advertisement = peer.endpoints.firstOrNull { it.endpointId == endpointId }
            ?.capabilitiesList?.firstOrNull { it.name == capabilityName && it.major == capabilityMajor }
            ?: throw AnchorSessionException("peer does not advertise $capabilityName@$capabilityMajor")
        val sessionId = allocateCapabilityId()
        val requestId = allocateRequestId()
        send(requestId, ControlEnvelope.newBuilder().setCapabilityOpen(
            org.anchor.sdk.v1.CapabilityOpen.newBuilder()
                .setCapabilitySessionId(sessionId).setEndpointId(endpointId)
                .setCapabilityName(capabilityName).setCapabilityMajor(capabilityMajor),
        ).build())
        val deferred = ArrayDeque<AnchorSessionEvent>()
        while (true) {
            when (val event = readEvent()) {
                is AnchorSessionEvent.CapabilityOpened -> if (event.responseTo == requestId && event.capabilitySessionId == sessionId) {
                    restoreEvents(deferred)
                    return AnchorCapability(this, sessionId, advertisement.recordTypeUrlsList, advertisement.supportsDatagrams)
                } else deferred.addLast(event)
                else -> deferred.addLast(event)
            }
        }
    }

    suspend fun sendCapabilityOpened(requestId: Long, capabilitySessionId: Long) {
        sendResponse(requestId, ControlEnvelope.newBuilder().setCapabilityOpened(
            CapabilityOpened.newBuilder().setCapabilitySessionId(capabilitySessionId),
        ).build())
    }

    suspend fun sendStreamOpened(requestId: Long, quicStreamId: Long) {
        sendResponse(requestId, ControlEnvelope.newBuilder().setStreamOpened(
            StreamOpened.newBuilder().setQuicStreamId(quicStreamId),
        ).build())
    }

    /** Accept a peer-opened reliable stream after its capability policy check. */
    suspend fun acceptStream(requestId: Long, quicStreamId: Long): AnchorStream {
        sendStreamOpened(requestId, quicStreamId)
        return AnchorStream(this, quicStreamId)
    }

    suspend fun sendCapabilityClosed(capabilitySessionId: Long, reasonValue: Int) {
        send(0, ControlEnvelope.newBuilder().setCapabilityClose(
            org.anchor.sdk.v1.CapabilityClose.newBuilder()
                .setCapabilitySessionId(capabilitySessionId).setReasonValue(reasonValue),
        ).build())
    }

    suspend fun sendStreamClosed(quicStreamId: Long) {
        send(0, ControlEnvelope.newBuilder().setStreamClose(
            org.anchor.sdk.v1.StreamClose.newBuilder().setQuicStreamId(quicStreamId),
        ).build())
    }

    suspend fun sendDatagramFlowOpened(requestId: Long, flowId: Long) {
        sendResponse(requestId, ControlEnvelope.newBuilder().setDatagramFlowOpened(
            org.anchor.sdk.v1.DatagramFlowOpened.newBuilder().setFlowId(flowId),
        ).build())
    }

    suspend fun sendDatagramFlowClosed(flowId: Long) {
        send(0, ControlEnvelope.newBuilder().setDatagramFlowClose(
            org.anchor.sdk.v1.DatagramFlowClose.newBuilder().setFlowId(flowId),
        ).build())
    }

    suspend fun sendPing(nonce: Long) {
        send(0, ControlEnvelope.newBuilder().setPing(org.anchor.sdk.v1.Ping.newBuilder().setNonce(nonce)).build())
    }

    suspend fun sendPong(nonce: Long) {
        send(0, ControlEnvelope.newBuilder().setPong(org.anchor.sdk.v1.Pong.newBuilder().setNonce(nonce)).build())
    }

    internal suspend fun sendRecord(sessionId: Long, typeUrl: String, payload: ByteArray) {
        send(0, ControlEnvelope.newBuilder().setCapabilityRecord(
            org.anchor.sdk.v1.CapabilityRecord.newBuilder().setCapabilitySessionId(sessionId)
                .setTypeUrl(typeUrl).setPayload(ByteString.copyFrom(payload)),
        ).build())
    }

    internal suspend fun openStream(capabilitySessionId: Long, payloadTypeUrl: String): AnchorStream {
        val streamId = transport.openBidirectionalStream()
        val requestId = allocateRequestId()
        send(requestId, ControlEnvelope.newBuilder().setStreamOpen(
            StreamOpen.newBuilder().setQuicStreamId(streamId)
                .setCapabilitySessionId(capabilitySessionId)
                .setPayloadTypeUrl(payloadTypeUrl),
        ).build())
        val deferred = ArrayDeque<AnchorSessionEvent>()
        while (true) {
        when (val event = readEvent()) {
                is AnchorSessionEvent.StreamOpened -> if (event.responseTo == requestId && event.quicStreamId == streamId) {
                    restoreEvents(deferred)
                    return AnchorStream(this, streamId)
                } else deferred.addLast(event)
                else -> deferred.addLast(event)
            }
        }
    }

    fun sendDatagram(bytes: ByteArray) {
        require(bytes.isNotEmpty()) { "datagrams must not be empty" }
        transport.sendDatagram(bytes)
    }

    override fun close() = transport.close()

    internal suspend fun readEvent(): AnchorSessionEvent = eventMutex.withLock {
        while (true) {
            framer.next()?.let { return@withLock decodeEvent(it) }
            takeQueuedEvent()?.let { return@withLock it }
            closedReason?.let { return@withLock AnchorSessionEvent.SessionClosed(it) }
            pollMutex.withLock { dispatch(transport.poll()) }
            framer.next()?.let { return@withLock decodeEvent(it) }
            takeQueuedEvent()?.let { return@withLock it }
            closedReason?.let { return@withLock AnchorSessionEvent.SessionClosed(it) }
            delay(2)
        }
        error("unreachable control reader")
    }

    internal suspend fun readStream(streamId: Long): AnchorStreamData {
        var closeObserved = false
        while (true) {
            takeQueuedStream(streamId)?.let { return it }
            pollMutex.withLock { dispatch(transport.poll()) }
            takeQueuedStream(streamId)?.let { return it }
            if (closedReason != null) {
                // MsQuic can report connection shutdown before its final
                // stream-receive callback. Give one more poll cycle for that
                // callback to enqueue the peer's last payload.
                if (closeObserved) throw AnchorSessionException("QUIC transport closed")
                closeObserved = true
            }
            delay(2)
        }
    }

    internal suspend fun readDatagram(): ByteArray {
        while (true) {
            takeQueuedDatagram()?.let { return it }
            if (closedReason != null) throw AnchorSessionException("QUIC transport closed")
            pollMutex.withLock { dispatch(transport.poll()) }
            takeQueuedDatagram()?.let { return it }
            if (closedReason != null) throw AnchorSessionException("QUIC transport closed")
            delay(2)
        }
    }

    private fun dispatch(events: List<QuicEvent>) {
        for (event in events) {
            when (event) {
                is QuicEvent.StreamData -> if (event.streamId == controlStreamId) {
                    framer.feed(event.bytes)
                } else {
                    synchronized(queueLock) {
                        queuedStreamData.getOrPut(event.streamId) { ArrayDeque() }
                            .addLast(AnchorStreamData(event.bytes, event.finished))
                        streamQueueAccounting.enqueue(event.bytes.size)
                    }
                }
                is QuicEvent.Datagram -> synchronized(queueLock) {
                    queuedDatagrams.addLast(event.bytes)
                    datagramQueueAccounting.enqueue(event.bytes.size)
                }
                is QuicEvent.Failed -> throw AnchorSessionException(event.reason)
                is QuicEvent.Closed -> if (closedReason == null) closedReason = event.code.toInt()
                QuicEvent.Connected -> Unit
            }
        }
    }

    internal suspend fun send(requestId: Long, envelope: ControlEnvelope) {
        sendMutex.withLock {
            val bytes = ControlFraming.encode(
                envelope.toBuilder().setRequestId(requestId).setResponseTo(0).build(),
                first = false,
            )
            transport.sendStream(controlStreamId, bytes)
        }
    }

    private suspend fun sendResponse(responseTo: Long, envelope: ControlEnvelope) {
        require(responseTo != 0L) { "responseTo must be non-zero" }
        sendMutex.withLock {
            val builder = envelope.toBuilder().setResponseTo(responseTo).setRequestId(0)
            transport.sendStream(controlStreamId, ControlFraming.encode(builder.build(), first = false))
        }
    }

    internal fun allocateRequestId(): Long = nextRequestId++.also { if (it == 0L) nextRequestId = 1L }
    private fun allocateCapabilityId(): Long = nextCapabilityId++.also { if (it == 0L) nextCapabilityId = 1L }
    internal fun allocateFlowId(): Long = nextFlowId++.also { if (it == 0L) nextFlowId = 1L }
    /**
     * Return an event to the session queue when an application-level reader
     * observes an acknowledgement owned by another pending operation.
     *
     * Anchor itself uses this for the optional event router; applications
     * normally only call [nextEvent].
     */
    suspend fun queueEvent(event: AnchorSessionEvent) = eventMutex.withLock {
        synchronized(queueLock) {
            queuedEvents.addLast(event)
            eventQueueAccounting.enqueue()
        }
    }

    /** Put events deferred while correlating a response back in wire order. */
    internal suspend fun restoreEvents(events: ArrayDeque<AnchorSessionEvent>) {
        if (events.isEmpty()) return
        eventMutex.withLock {
            synchronized(queueLock) {
                for (event in events.toList().asReversed()) {
                    queuedEvents.addFirst(event)
                    eventQueueAccounting.enqueue()
                }
            }
        }
    }

    /**
     * Snapshot SDK-side queues without polling or changing delivery order.
     * Stream bytes are counted here after the native event has crossed JNI;
     * they remain queued until the corresponding stream reader consumes them.
     */
    fun queueMetrics(): AnchorSessionQueueMetrics = synchronized(queueLock) {
        AnchorSessionQueueMetrics(
            eventQueueItems = eventQueueAccounting.currentItems,
            eventQueueHighWaterItems = eventQueueAccounting.highWaterItems,
            streamQueueItems = streamQueueAccounting.currentItems,
            streamQueueBytes = streamQueueAccounting.currentBytes,
            streamQueueHighWaterItems = streamQueueAccounting.highWaterItems,
            streamQueueHighWaterBytes = streamQueueAccounting.highWaterBytes,
            datagramQueueItems = datagramQueueAccounting.currentItems,
            datagramQueueBytes = datagramQueueAccounting.currentBytes,
            datagramQueueHighWaterItems = datagramQueueAccounting.highWaterItems,
            datagramQueueHighWaterBytes = datagramQueueAccounting.highWaterBytes,
            enqueuedItems = streamQueueAccounting.enqueuedItems + datagramQueueAccounting.enqueuedItems + eventQueueAccounting.enqueuedItems,
            enqueuedBytes = streamQueueAccounting.enqueuedBytes + datagramQueueAccounting.enqueuedBytes,
            dequeuedItems = streamQueueAccounting.dequeuedItems + datagramQueueAccounting.dequeuedItems + eventQueueAccounting.dequeuedItems,
            dequeuedBytes = streamQueueAccounting.dequeuedBytes + datagramQueueAccounting.dequeuedBytes,
        )
    }

    /** Native and SDK queue counters for diagnostics; does not poll or consume data. */
    fun transportMetrics(): QuicTransportMetrics = transport.metrics()

    private fun takeQueuedEvent(): AnchorSessionEvent? = synchronized(queueLock) {
        queuedEvents.takeFirstOrNull()?.also { eventQueueAccounting.dequeue() }
    }

    private fun takeQueuedStream(streamId: Long): AnchorStreamData? = synchronized(queueLock) {
        queuedStreamData[streamId]?.takeFirstOrNull()?.also {
            streamQueueAccounting.dequeue(it.bytes.size)
        }
    }

    private fun takeQueuedDatagram(): ByteArray? = synchronized(queueLock) {
        queuedDatagrams.takeFirstOrNull()?.also { datagramQueueAccounting.dequeue(it.size) }
    }

    companion object {
        suspend fun connect(transport: QuicTransport, request: QuicConnectRequest, local: SessionIdentity, timeoutMs: Long = 5_000): AnchorSession = withContext(Dispatchers.IO) {
            transport.connect(request)
            awaitConnected(transport, timeoutMs)
            val controlStream = transport.openBidirectionalStream()
            val framer = ControlFramer()
            val helloFrame = ControlFraming.encode(ControlEnvelope.newBuilder().setSessionHello(local.hello()).build(), first = true)
            val readyFrame = ControlFraming.encode(ControlEnvelope.newBuilder().setSessionReady(
                SessionReady.newBuilder().setProtocolVersion(ProtocolVersion.newBuilder().setMajor(PROTOCOL_MAJOR).setMinor(PROTOCOL_MINOR)),
            ).build(), first = false)
            // Keep the two handshake records in one QUIC write so every
            // implementation observes the complete initial control batch.
            transport.sendStream(controlStream, helloFrame + readyFrame)
            val peerHello = awaitEnvelope(transport, controlStream, framer, timeoutMs)
            val peer = peerFromHello(peerHello)
            val ready = awaitEnvelope(transport, controlStream, framer, timeoutMs)
            if (ready.bodyCase != ControlEnvelope.BodyCase.SESSION_READY) throw AnchorSessionException("expected SessionReady")
            AnchorSession(transport, controlStream, peer, framer)
        }

        private suspend fun awaitConnected(transport: QuicTransport, timeoutMs: Long) {
            val deadline = System.nanoTime() + timeoutMs * 1_000_000
            while (System.nanoTime() < deadline) {
                if (transport.poll().any { it == QuicEvent.Connected }) return
                transport.poll().filterIsInstance<QuicEvent.Failed>().firstOrNull()?.let { throw AnchorSessionException(it.reason) }
                delay(2)
            }
            throw AnchorSessionException("QUIC connection timed out")
        }

        private suspend fun awaitEnvelope(transport: QuicTransport, streamId: Long, framer: ControlFramer, timeoutMs: Long): ControlEnvelope {
            val deadline = System.nanoTime() + timeoutMs * 1_000_000
            while (System.nanoTime() < deadline) {
                framer.next()?.let { return it }
                transport.poll().forEach {
                    if (it is QuicEvent.StreamData && it.streamId == streamId) framer.feed(it.bytes)
                }
                framer.next()?.let { return it }
                delay(2)
            }
            throw AnchorSessionException("control handshake timed out")
        }

        private fun peerFromHello(envelope: ControlEnvelope): SessionPeer {
            if (envelope.bodyCase != ControlEnvelope.BodyCase.SESSION_HELLO) throw AnchorSessionException("expected SessionHello")
            val hello = envelope.sessionHello
            if (!hello.hasProtocolVersion() || hello.protocolVersion.major != PROTOCOL_MAJOR || hello.protocolVersion.minor > PROTOCOL_MINOR) throw AnchorSessionException("unsupported protocol version")
            if (!hello.hasNodeId() || hello.nodeId.value.size() != NODE_ID_BYTES) throw AnchorSessionException("SessionHello has invalid node ID")
            if (!hello.hasDisplay()) throw AnchorSessionException("SessionHello has no display info")
            return SessionPeer(hello.nodeId.value.toByteArray(), hello.display.displayName, hello.display.deviceKindValue, hello.endpointsList)
        }
    }
}

class AnchorCapability internal constructor(
    private val session: AnchorSession,
    val sessionId: Long,
    private val allowedTypeUrls: List<String>,
    private val supportsDatagrams: Boolean,
) {
    suspend fun sendRecord(typeUrl: String, payload: ByteArray) {
        require(allowedTypeUrls.contains(typeUrl)) { "record type URL was not advertised" }
        session.sendRecord(sessionId, typeUrl, payload)
    }

    suspend fun openStream(payloadTypeUrl: String): AnchorStream {
        require(allowedTypeUrls.contains(payloadTypeUrl)) { "stream type URL was not advertised" }
        return session.openStream(sessionId, payloadTypeUrl)
    }

    suspend fun openDatagramFlow(payloadTypeUrl: String): AnchorDatagramFlow {
        require(supportsDatagrams) { "capability does not advertise datagrams" }
        require(allowedTypeUrls.contains(payloadTypeUrl)) { "datagram type URL was not advertised" }
        val flowId = session.allocateFlowId()
        val requestId = session.allocateRequestId()
        session.send(requestId, ControlEnvelope.newBuilder().setDatagramFlowOpen(
            org.anchor.sdk.v1.DatagramFlowOpen.newBuilder()
                .setCapabilitySessionId(sessionId).setFlowId(flowId).setPayloadTypeUrl(payloadTypeUrl),
        ).build())
        val deferred = ArrayDeque<AnchorSessionEvent>()
        while (true) {
            when (val event = session.readEvent()) {
                is AnchorSessionEvent.DatagramFlowOpened -> if (event.responseTo == requestId && event.flowId == flowId) {
                    session.restoreEvents(deferred)
                    return AnchorDatagramFlow(session, flowId)
                } else deferred.addLast(event)
                else -> deferred.addLast(event)
            }
        }
    }
}

class AnchorDatagramFlow internal constructor(
    private val session: AnchorSession,
    val flowId: Long,
) {
    fun send(bytes: ByteArray) = session.sendDatagram(bytes)

    suspend fun receive(): ByteArray = session.readDatagram()

    suspend fun close() = session.sendDatagramFlowClosed(flowId)
}

class AnchorStream internal constructor(private val session: AnchorSession, val streamId: Long) {
    fun send(bytes: ByteArray, finish: Boolean = false) = session.transport.sendStream(streamId, bytes, finish)

    suspend fun receive(): AnchorStreamData = session.readStream(streamId)
}

data class AnchorStreamData(val bytes: ByteArray, val finished: Boolean)

internal class ControlFramer {
    private val bytes = ArrayDeque<Byte>()
    private val records = ArrayDeque<ControlEnvelope>()
    private var first = true

    fun feed(chunk: ByteArray) {
        chunk.forEach(bytes::addLast)
        while (true) {
            if (first && bytes.size < CONTROL_MAGIC.size + 2) return
            if (first) {
                repeat(CONTROL_MAGIC.size) { if (bytes.removeFirst() != CONTROL_MAGIC[it]) throw AnchorSessionException("invalid ANCR control preface") }
                val major = bytes.removeFirst().toInt() and 0xff
                val minor = bytes.removeFirst().toInt() and 0xff
                // Major must match exactly. A peer's minor may be behind or
                // equal to ours (minor changes are additive by policy); a
                // peer ahead of us may rely on control-layer behavior we
                // don't understand yet.
                if (major != PROTOCOL_MAJOR || minor > PROTOCOL_MINOR) throw AnchorSessionException("unsupported control version")
                first = false
            }
            val (length, lengthBytes) = readVarint() ?: return
            if (length > MAX_CONTROL_RECORD_BYTES) throw AnchorSessionException("control record exceeds 1 MiB")
            // Do not consume the length prefix until the complete payload is
            // buffered. A QUIC receive callback may split the varint and body
            // across separate events.
            if (bytes.size < lengthBytes + length) return
            repeat(lengthBytes) { bytes.removeFirst() }
            val payload = ByteArray(length) { bytes.removeFirst() }
            val envelope = ControlEnvelope.parseFrom(payload)
            if (AnchorProtocol.validateControlEnvelope(envelope) !is ProtocolValidation.Valid) {
                throw AnchorSessionException("invalid control envelope")
            }
            records.addLast(envelope)
        }
    }

    fun next(): ControlEnvelope? = if (records.isEmpty()) null else records.removeFirst()

    private fun readVarint(): Pair<Int, Int>? {
        var value = 0L
        var consumed = 0
        val iterator = bytes.iterator()
        for (index in 0 until 10) {
            if (!iterator.hasNext()) return null
            val byte = iterator.next().toInt() and 0xff
            consumed++
            if (index == 9 && byte > 1) throw AnchorSessionException("invalid control length")
            value = value or ((byte and 0x7f).toLong() shl (index * 7))
            if (byte and 0x80 == 0) {
                return value.toInt() to consumed
            }
        }
        throw AnchorSessionException("invalid control length")
    }
}

internal object ControlFraming {
    fun encode(envelope: ControlEnvelope, first: Boolean): ByteArray {
        check(AnchorProtocol.validateControlEnvelope(envelope) is ProtocolValidation.Valid) { "invalid control envelope" }
        val payload = envelope.toByteArray()
        require(payload.size <= MAX_CONTROL_RECORD_BYTES) { "control record exceeds 1 MiB" }
        val prefix = if (first) CONTROL_MAGIC + byteArrayOf(PROTOCOL_MAJOR.toByte(), PROTOCOL_MINOR.toByte()) else byteArrayOf()
        return prefix + varint(payload.size) + payload
    }

    private fun varint(input: Int): ByteArray {
        var value = input
        val output = ArrayList<Byte>()
        while (value >= 0x80) { output += ((value and 0x7f) or 0x80).toByte(); value = value ushr 7 }
        output += value.toByte()
        return output.toByteArray()
    }
}

private fun <T> ArrayDeque<T>.takeFirstOrNull(): T? = if (isEmpty()) null else removeFirst()

private fun decodeEvent(envelope: ControlEnvelope): AnchorSessionEvent = when (envelope.bodyCase) {
    ControlEnvelope.BodyCase.CAPABILITY_OPEN -> envelope.capabilityOpen.let { AnchorSessionEvent.CapabilityOpenRequested(envelope.requestId, it.capabilitySessionId, it.endpointId, it.capabilityName, it.capabilityMajor) }
    ControlEnvelope.BodyCase.CAPABILITY_OPENED -> AnchorSessionEvent.CapabilityOpened(envelope.responseTo, envelope.capabilityOpened.capabilitySessionId)
    ControlEnvelope.BodyCase.CAPABILITY_CLOSE -> AnchorSessionEvent.CapabilityClosed(envelope.capabilityClose.capabilitySessionId, envelope.capabilityClose.reasonValue)
    ControlEnvelope.BodyCase.CAPABILITY_RECORD -> envelope.capabilityRecord.let { AnchorSessionEvent.CapabilityRecord(it.capabilitySessionId, it.typeUrl, it.payload.toByteArray()) }
        ControlEnvelope.BodyCase.STREAM_OPEN -> envelope.streamOpen.let { AnchorSessionEvent.StreamOpenRequested(envelope.requestId, it.quicStreamId, it.capabilitySessionId, it.payloadTypeUrl) }
    ControlEnvelope.BodyCase.STREAM_OPENED -> AnchorSessionEvent.StreamOpened(envelope.responseTo, envelope.streamOpened.quicStreamId)
    ControlEnvelope.BodyCase.STREAM_CLOSE -> AnchorSessionEvent.StreamClosed(envelope.streamClose.quicStreamId)
    ControlEnvelope.BodyCase.DATAGRAM_FLOW_OPEN -> envelope.datagramFlowOpen.let {
        AnchorSessionEvent.DatagramFlowOpenRequested(
            envelope.requestId,
            it.capabilitySessionId,
            it.flowId,
            it.payloadTypeUrl,
        )
    }
    ControlEnvelope.BodyCase.DATAGRAM_FLOW_OPENED -> AnchorSessionEvent.DatagramFlowOpened(
        envelope.responseTo,
        envelope.datagramFlowOpened.flowId,
    )
    ControlEnvelope.BodyCase.DATAGRAM_FLOW_CLOSE -> AnchorSessionEvent.DatagramFlowClosed(
        envelope.datagramFlowClose.flowId,
    )
    ControlEnvelope.BodyCase.PING -> AnchorSessionEvent.Ping(envelope.ping.nonce)
    ControlEnvelope.BodyCase.PONG -> AnchorSessionEvent.Pong(envelope.pong.nonce)
    ControlEnvelope.BodyCase.PROTOCOL_ERROR -> envelope.protocolError.let { AnchorSessionEvent.ProtocolError(envelope.responseTo, it.codeValue, it.message) }
    ControlEnvelope.BodyCase.SESSION_CLOSE -> AnchorSessionEvent.SessionClosed(envelope.sessionClose.reasonValue)
    else -> throw AnchorSessionException("unexpected control record ${envelope.bodyCase}")
}
