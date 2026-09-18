package org.anchor.sdk

import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.util.Locale
import kotlin.math.roundToLong
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Repeats identical echo workloads over UDP and QUIC DATAGRAM. */
@RunWith(AndroidJUnit4::class)
class TransportBenchmarkInstrumentedTest {
    private val udpPort = 4450
    private val quicPort = 4451
    private val packetSize = 512
    private val rttPackets = 100
    private val throughputPackets = 4096
    private val timeoutMs = 2_000

    @Test
    fun comparesUdpAndQuicDatagram() {
        val args = InstrumentationRegistry.getArguments()
        val host = args.getString("anchor.benchmark.host") ?: "100.97.102.32"
        val repeats = args.getString("anchor.benchmark.repeats")?.toIntOrNull()?.coerceIn(1, 5) ?: 3
        val fixture = Fixtures(InstrumentationRegistry.getInstrumentation().targetContext)
        val udpResults = (0 until repeats).map { runUdp(host, fixture) }
        val quicResults = (0 until repeats).map { runQuic(host, fixture) }
        udpResults.forEachIndexed { index, result -> logResult("udp", index + 1, result) }
        quicResults.forEachIndexed { index, result -> logResult("quic", index + 1, result) }
        assertTrue("UDP did not receive all RTT echoes", udpResults.all { it.rttReceived == rttPackets })
        assertTrue("QUIC did not receive all RTT echoes", quicResults.all { it.rttReceived == rttPackets })
    }

    private fun runUdp(host: String, fixture: Fixtures): Result {
        DatagramSocket().use { socket ->
            socket.soTimeout = timeoutMs
            val peer = InetSocketAddress(host, udpPort)
            val rtt = ArrayList<Long>(rttPackets)
            repeat(rttPackets) { sequence ->
                val sentAt = System.nanoTime()
                socket.send(DatagramPacket(packet(sequence, packetSize), packetSize, peer))
                val received = receive(socket)
                if (received != null && sequenceOf(received) == sequence) rtt += (System.nanoTime() - sentAt) / 1_000_000
            }
            val throughput = throughput(socket, peer)
            return Result(rtt.size, percentile(rtt, 50), percentile(rtt, 95), percentile(rtt, 99), throughput)
        }
    }

    private fun runQuic(host: String, fixture: Fixtures): Result {
        val transport = MsQuicTransport()
        val started = System.nanoTime()
        transport.connect(fixture.request(host, quicPort))
        val connected = waitFor(transport) { it == QuicEvent.Connected }
        check(connected != null) { "QUIC did not connect within ${timeoutMs}ms" }
        val setupMs = (System.nanoTime() - started) / 1_000_000
        val rtt = ArrayList<Long>(rttPackets)
        repeat(rttPackets) { sequence ->
            val sentAt = System.nanoTime()
            transport.sendDatagram(packet(sequence, packetSize))
            val received = waitFor(transport) { event -> event is QuicEvent.Datagram && sequenceOf(event.bytes) == sequence }
            if (received != null) rtt += (System.nanoTime() - sentAt) / 1_000_000
        }
        val throughputStart = System.nanoTime()
        repeat(throughputPackets) { sequence -> transport.sendDatagram(packet(sequence + rttPackets, packetSize)) }
        var received = 0
        val deadline = System.nanoTime() + timeoutMs * 1_000_000L
        while (received < throughputPackets && System.nanoTime() < deadline) {
            received += transport.poll().count { it is QuicEvent.Datagram }
            if (received < throughputPackets) Thread.sleep(1)
        }
        val throughputMs = (System.nanoTime() - throughputStart) / 1_000_000
        transport.close()
        return Result(rtt.size, percentile(rtt, 50), percentile(rtt, 95), percentile(rtt, 99),
            Goodput(received, throughputPackets * packetSize, throughputMs, setupMs))
    }

    private fun throughput(socket: DatagramSocket, peer: InetSocketAddress): Goodput {
        val started = System.nanoTime()
        repeat(throughputPackets) { sequence -> socket.send(DatagramPacket(packet(sequence + rttPackets, packetSize), packetSize, peer)) }
        var received = 0
        val deadline = System.nanoTime() + timeoutMs * 1_000_000L
        while (received < throughputPackets && System.nanoTime() < deadline) {
            if (receive(socket) != null) received++
        }
        return Goodput(received, throughputPackets * packetSize, (System.nanoTime() - started) / 1_000_000, 0)
    }

    private fun receive(socket: DatagramSocket): ByteArray? = try {
        val packet = DatagramPacket(ByteArray(packetSize), packetSize)
        socket.receive(packet)
        packet.data.copyOf(packet.length)
    } catch (_: java.net.SocketTimeoutException) { null }

    private fun waitFor(transport: MsQuicTransport, predicate: (QuicEvent) -> Boolean): QuicEvent? {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000L
        while (System.nanoTime() < deadline) {
            transport.poll().firstOrNull(predicate)?.let { return it }
            Thread.sleep(1)
        }
        return null
    }

    private fun packet(sequence: Int, size: Int): ByteArray = ByteArray(size).also {
        ByteBuffer.wrap(it).order(ByteOrder.LITTLE_ENDIAN).putInt(sequence).putLong(System.nanoTime()).putInt(size)
    }

    private fun sequenceOf(bytes: ByteArray): Int = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).int

    private fun percentile(values: List<Long>, percentile: Int): Long = values.sorted().let {
        if (it.isEmpty()) -1 else it[((it.size - 1) * percentile / 100.0).roundToLong().toInt()]
    }

    private fun logResult(protocol: String, repeat: Int, result: Result) {
        val goodput = result.goodput
        Log.i("AnchorBenchmark", String.format(Locale.US,
            "protocol=%s repeat=%d rtt_received=%d p50_ms=%d p95_ms=%d p99_ms=%d throughput_received=%d throughput_sent_bytes=%d throughput_ms=%d goodput_mbps=%.3f setup_ms=%d",
            protocol, repeat, result.rttReceived, result.p50, result.p95, result.p99,
            goodput.received, goodput.sentBytes, goodput.elapsedMs,
            if (goodput.elapsedMs == 0L) 0.0 else goodput.received * packetSize * 8.0 / goodput.elapsedMs / 1000.0,
            goodput.setupMs))
    }

    private data class Result(val rttReceived: Int, val p50: Long, val p95: Long, val p99: Long, val goodput: Goodput)
    private data class Goodput(val received: Int, val sentBytes: Int, val elapsedMs: Long, val setupMs: Long)

    private class Fixtures(private val context: android.content.Context) {
        private fun copy(name: String): File = File(context.filesDir, name).also { output ->
            context.assets.open("mtls/$name").use { input -> output.outputStream().use(input::copyTo) }
        }
        private val server = copy("server-cert.pem")
        private val client = copy("client-cert.pem")
        private val key = copy("client-key.pem")
        private val fingerprint = MessageDigest.getInstance("SHA-256").digest(
            CertificateFactory.getInstance("X.509").generateCertificate(server.inputStream()).encoded,
        )
        fun request(host: String, port: Int) = QuicConnectRequest(host, port, "anchor.test", fingerprint,
            QuicClientIdentity(client.path, key.path), server.path)
    }
}
