package com.anchor.plugin

import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import com.anchor.data.TrustedStore
import com.anchor.data.DeviceListEntry
import android.util.Log
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import android.os.SystemClock
import android.util.DisplayMetrics
import android.view.WindowManager
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.json.JSONObject
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorPairingSession
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.AnchorSessionEvent
import org.anchor.sdk.ClipboardProtocol
import org.anchor.sdk.MsQuicTransport
import org.anchor.sdk.DeviceProtocol
import org.anchor.sdk.NotificationsProtocol
import org.anchor.sdk.InputProtocol
import org.anchor.sdk.MediaProtocol
import org.anchor.sdk.AdditionalProtocols
import org.anchor.sdk.ScreenProtocol
import org.anchor.sdk.CameraProtocol
import org.anchor.sdk.QuicClientIdentity
import org.anchor.sdk.QuicConnectRequest
import org.anchor.sdk.SessionIdentity
import org.anchor.sdk.SdkIdentityStore
import org.anchor.sdk.PairingResolution
import org.anchor.sdk.AnchorProtocol
import org.anchor.sdk.PairingHello
import org.anchor.sdk.PairingInvitation
import java.io.File
import java.io.ByteArrayInputStream
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

private const val TAG = "anchor"
private const val LATENCY_PROBE_INTERVAL_MS = 2_000L
private const val SESSION_HEARTBEAT_TIMEOUT_MS = 8_000L

/** Anchor's single network client: a pinned QUIC SDK session on UDP 5027. */
class NetworkPlugin(
    private val broker: MessageBroker,
    private val videoPlugin: VideoPlugin,
    private val trustedStore: TrustedStore,
    private val deviceId: String,
    private val deviceName: String,
    private val hasSmsPermissions: () -> Boolean,
    private val context: android.content.Context,
    private val onSdkClipboardReady: (AnchorSession, AnchorCapability, ByteArray) -> Unit = { _, _, _ -> },
    private val onSdkNotificationsReady: (AnchorSession, AnchorCapability) -> Unit = { _, _ -> },
    private val onSdkInputReady: (AnchorSession, AnchorCapability) -> Unit = { _, _ -> },
    private val onSdkMediaReady: (AnchorSession, AnchorCapability) -> Unit = { _, _ -> },
    private val onSdkScreenReady: (AnchorSession, AnchorCapability) -> Unit = { _, _ -> },
    private val onSdkCameraReady: (AnchorSession, AnchorCapability) -> Unit = { _, _ -> },
    private val onSdkSmsReady: (AnchorCapability) -> Unit = { _ -> },
    private val onSdkCommandsReady: (AnchorCapability) -> Unit = { _ -> },
    private val onSdkFilesReady: (AnchorCapability) -> Unit = { _ -> },
    private val onSdkTransportStopped: () -> Unit = {},
    private val onSdkRecord: suspend (AnchorSessionEvent) -> Unit = { _ -> },
    private val onSdkStreamOpen: suspend (AnchorSession, AnchorSessionEvent.StreamOpenRequested) -> Unit = { session, event ->
        session.sendStreamOpened(event.requestId, event.quicStreamId)
    },
) : Plugin {

    @Volatile private var deviceCapability: AnchorCapability? = null

    override val pluginId: String = "network"

    /** Process-level scope — set by AnchorApplication, survives ViewModel death. */
    lateinit var processScope: CoroutineScope

    @Volatile var isVideoTransportReady: Boolean = false
        private set

    private var sdkSessionJob: Job? = null
    private var sdkEventJob: Job? = null
    private var sdkLatencyJob: Job? = null
    /** Prevent teardown from an old QUIC session affecting a reconnect. */
    private val sdkSessionGeneration = AtomicLong(0)
    private val pendingLatencyProbes = ConcurrentHashMap<Long, Long>()
    private val nextLatencyProbeNonce = AtomicLong(0)
    private val lastSessionPongMs = AtomicLong(0)

    val isConnected: Boolean
        get() = sdkEventJob?.isActive == true

    /** Respond to a pairing request. */
    fun respondToPairing(accepted: Boolean) {
        if (accepted) {
            Log.w(TAG, "Pairing approval ignored: this device did not initiate a nearby pairing request")
        }
    }

    override fun start(scope: CoroutineScope) = Unit

    private fun isValidIpOrHost(input: String): Boolean {
        val s = input.trim()
        if (s.isEmpty()) return false
        if (s.contains(":") || s.contains("/") || s.contains(" ")) return false
        val ipv4Regex = Regex("^((25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\\.){3}(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$")
        if (ipv4Regex.matches(s)) return true
        if (s.equals("localhost", ignoreCase = true)) return true
        if (s.matches(Regex("^[0-9]+$"))) return false
        if (s.length < 2) return false
        val hostRegex = Regex("^[A-Za-z0-9]([A-Za-z0-9\\-\\.]*[A-Za-z0-9])?$")
        if (!hostRegex.matches(s)) return false
        // Bare labels are valid on private DNS/Tailnet networks (for example
        // a user's short Tailscale machine name). Keep syntax validation here,
        // but let the asynchronous QUIC operation report unreachable hosts.
        return true
    }

    /** Connect only through the Anchor SDK QUIC endpoint (UDP 5027). */
    fun connect(ip: String, desktopId: String) {
        try {
            val clean = ip.trim()
            Log.i(TAG, "QUIC connect requested: host=$clean desktop=$desktopId")
            if (clean.isBlank()) {
                Log.w(TAG, "QUIC connect refused: blank host")
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter an IP address"}"""
                )))
                return
            }
            if (!isValidIpOrHost(clean)) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter a valid IP like 192.168.1.10"}"""
                )))
                return
            }
            if (clean.contains(":") || clean.contains("/")) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Enter just the IP — no port or scheme"}"""
                )))
                return
            }
            if (sdkSessionJob?.isActive == true || sdkEventJob?.isActive == true) {
                Log.d(TAG, "connect ignored: QUIC session already active")
                return
            }
            if (!trustedStore.isTrusted(desktopId)) {
                Log.w(TAG, "QUIC connection refused: desktop $desktopId is not paired")
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Pair this desktop before connecting over QUIC"}"""
                )))
                return
            }
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"connecting","host":"$clean","port":5027}"""
            )))
            startSdkClipboard(clean, desktopId)
        } catch (e: Exception) {
            Log.e(TAG, "Connect failed: ${e.message}", e)
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"${e.message ?: "Connection failed"}"}"""
            )))
        }
    }

    /** Opens the constrained, certificate-pinned pairing session. */
    private fun pair(invitation: PairingInvitation) {
        if (sdkSessionJob?.isActive == true || sdkEventJob?.isActive == true) {
            Log.d(TAG, "pairing ignored: a QUIC operation is already active")
            return
        }
        when (val validation = AnchorProtocol.validatePairingInvitation(invitation, System.currentTimeMillis())) {
            is org.anchor.sdk.ProtocolValidation.Valid -> Unit
            else -> {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Invalid pairing invitation: $validation"}"""
                )))
                return
            }
        }
        val (host, port) = parsePairingEndpoint(invitation.endpoint) ?: run {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Pairing invitation has an invalid endpoint"}"""
            )))
            return
        }
        broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
            """{"type":"connection_status","status":"connecting","host":"$host","port":$port}"""
        )))
        sdkSessionJob = processScope.launch(Dispatchers.IO) {
            val identity = runCatching { SdkIdentityStore(context).ensure() }.getOrElse { error ->
                Log.w(TAG, "SDK identity could not be created for pairing: ${error.message}")
                return@launch
            }
            val transcriptHash = AnchorProtocol.pairingTranscriptHash(invitation, identity.certificateFingerprint)
            val hello = PairingHello(
                invitationId = invitation.invitationId,
                nodeId = MessageDigest.getInstance("SHA-256").digest(deviceId.toByteArray(Charsets.UTF_8)),
                displayName = deviceName,
                deviceKindValue = 2,
                transcriptHash = transcriptHash,
            )
            try {
                // The SDK only permits an unpinned bootstrap for pairing, so
                // the advertised certificate cannot pin TLS — the approval is
                // checked against the advertised identity below instead.
                val pairing = AnchorPairingSession.connect(
                    MsQuicTransport(),
                    QuicConnectRequest(
                        host = host,
                        port = port,
                        serverName = "anchor.local",
                        expectedCertificateFingerprint = ByteArray(0),
                        localIdentity = QuicClientIdentity(identity.certificatePemPath, identity.privateKeyPemPath),
                        trustedPeerCertificatePemPath = "",
                        pairingBootstrap = true,
                    ),
                    hello,
                )
                // The transport has connected within AnchorPairingSession's
                // five-second deadline. Desktop approval can reasonably take
                // longer, so present it as a distinct state rather than leave
                // the UI claiming it is still connecting.
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"pairing","host":"$host","port":$port}"""
                )))
                when (val resolution = pairing.awaitResolution()) {
                    is PairingResolution.Approved -> {
                        if (!resolution.transcriptHash.contentEquals(transcriptHash)) {
                            throw IllegalStateException("desktop pairing approval did not match this invitation")
                        }
                        // Refuse to trust a responder that does not match the
                        // advertised identity.
                        require(resolution.approverDeviceId == invitation.inviterDeviceId &&
                            resolution.approverCertificateDer.contentEquals(invitation.inviterCertificateDer)) {
                            "paired desktop identity does not match the advertised device"
                        }
                        CertificateFactory.getInstance("X.509").generateCertificate(
                            ByteArrayInputStream(resolution.approverCertificateDer)
                        )
                        val pem = "-----BEGIN CERTIFICATE-----\n" +
                            android.util.Base64.encodeToString(resolution.approverCertificateDer, android.util.Base64.NO_WRAP) +
                            "\n-----END CERTIFICATE-----\n"
                        trustedStore.addDevice(resolution.approverDeviceId, "Anchor Desktop", pem)
                        Log.i(TAG, "SDK pairing approved for ${invitation.inviterDeviceId}")
                        pairing.close()
                        // This coroutine owns the pairing session. Release
                        // that slot before starting the separate normal SDK
                        // session, otherwise connect() correctly treats us as
                        // already connected and ignores the handoff.
                        sdkSessionJob = null
                        connect(host, invitation.inviterDeviceId)
                    }
                    PairingResolution.Rejected -> {
                        pairing.close()
                        broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                            """{"type":"connection_status","status":"disconnected","error":"Pairing was rejected"}"""
                        )))
                    }
                }
            } catch (error: Exception) {
                Log.w(TAG, "SDK pairing failed: ${error.message}")
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"${error.localizedMessage}"}"""
                )))
            }
        }
    }

    /**
     * Starts the normal nearby-device pairing flow. Discovery supplies public
     * certificate material that is bound into the pairing transcript and then
     * checked against the identity the desktop returns on approval; nothing is
     * saved unless the desktop approves and its identity matches the
     * advertisement.
     */
    fun pairNearby(device: DeviceListEntry) {
        val id = device.deviceId
        val certificate = device.certificateDer
        if (id.isNullOrBlank() || certificate == null) {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"This desktop is not ready for nearby pairing. Wait for discovery and try again."}"""
            )))
            return
        }
        val invitation = AnchorProtocol.createPairingInvitation(
            endpoint = "${device.ip}:${device.port}",
            inviterDeviceId = id,
            inviterCertificateDer = certificate,
            expiresAtUnixMs = System.currentTimeMillis() + 60_000,
        )
        Log.i(TAG, "Starting nearby pairing with ${device.name} ($id)")
        pair(invitation)
    }

    /**
     * Pair using an address deliberately supplied by the user. This is for
     * routed LANs, Tailscale, and networks where multicast discovery cannot
     * cross the boundary. It opens only a pairing-only QUIC stream; after the
     * desktop approval returns its certificate, every later connection is
     * pinned exactly like a nearby pairing.
     */
    fun pairDirect(host: String) {
        val clean = host.trim()
        if (!isValidIpOrHost(clean)) {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Enter a valid desktop address"}"""
            )))
            return
        }
        if (sdkSessionJob?.isActive == true || sdkEventJob?.isActive == true) return
        sdkSessionJob = processScope.launch(Dispatchers.IO) {
            val identity = runCatching { SdkIdentityStore(context).ensure() }.getOrElse { error ->
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Could not create pairing identity"}"""
                )))
                Log.w(TAG, "SDK identity could not be created for direct pairing: ${error.message}")
                return@launch
            }
            val nonce = ByteArray(32).also(SecureRandom()::nextBytes)
            val nodeId = MessageDigest.getInstance("SHA-256").digest(deviceId.toByteArray(Charsets.UTF_8))
            val transcript = AnchorProtocol.directPairingTranscriptHash(clean, nonce, identity.certificateFingerprint)
            val hello = PairingHello(
                nodeId = nodeId,
                displayName = deviceName,
                deviceKindValue = 2,
                transcriptHash = transcript,
            )
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"connecting","host":"$clean","port":5027}"""
            )))
            val pairing = try {
                AnchorPairingSession.connect(
                    MsQuicTransport(),
                    QuicConnectRequest(
                        host = clean,
                        port = 5027,
                        serverName = "anchor.local",
                        expectedCertificateFingerprint = ByteArray(0),
                        localIdentity = QuicClientIdentity(identity.certificatePemPath, identity.privateKeyPemPath),
                        trustedPeerCertificatePemPath = "",
                        pairingBootstrap = true,
                    ),
                    hello,
                )
            } catch (error: Exception) {
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"Could not reach desktop: ${error.localizedMessage ?: "pairing failed"}"}"""
                )))
                Log.w(TAG, "Direct pairing connection failed: ${error.message}")
                return@launch
            }
            try {
                // Connection establishment has its own five-second deadline;
                // this state is now only waiting for the desktop's response.
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"pairing","host":"$clean","port":5027}"""
                )))
                when (val resolution = pairing.awaitResolution()) {
                    is PairingResolution.Approved -> {
                        require(resolution.transcriptHash.contentEquals(transcript)) {
                            "desktop pairing approval did not match this request"
                        }
                        require(resolution.approverCertificateDer.isNotEmpty() &&
                            resolution.approverCertificateDer.size <= 4096 &&
                            resolution.approverDeviceId.isNotBlank()) {
                            "desktop returned an invalid pairing identity"
                        }
                        CertificateFactory.getInstance("X.509").generateCertificate(
                            ByteArrayInputStream(resolution.approverCertificateDer)
                        )
                        val certificatePem = "-----BEGIN CERTIFICATE-----\n" +
                            android.util.Base64.encodeToString(resolution.approverCertificateDer, android.util.Base64.NO_WRAP) +
                            "\n-----END CERTIFICATE-----\n"
                        trustedStore.addDevice(resolution.approverDeviceId, "Anchor Desktop", certificatePem)
                        pairing.close()
                        sdkSessionJob = null
                        connect(clean, resolution.approverDeviceId)
                    }
                    PairingResolution.Rejected -> {
                        pairing.close()
                        broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                            """{"type":"connection_status","status":"disconnected","error":"Pairing was rejected on the desktop"}"""
                        )))
                    }
                }
            } catch (error: Exception) {
                pairing.close()
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","error":"${error.localizedMessage ?: "Pairing failed"}"}"""
                )))
                Log.w(TAG, "Direct pairing failed: ${error.message}")
            }
        }
    }

    private fun parsePairingEndpoint(endpoint: String): Pair<String, Int>? {
        val value = endpoint.trim()
        val separator = value.lastIndexOf(':')
        if (separator <= 0 || separator == value.lastIndex) return null
        val host = value.substring(0, separator).removePrefix("[").removeSuffix("]")
        val port = value.substring(separator + 1).toIntOrNull()?.takeIf { it in 1..65535 } ?: return null
        return host.takeIf { it.isNotBlank() }?.let { it to port }
    }

    /** Connect to the paired desktop. */
    fun connect(ip: String, port: Int = 5027) {
        val clean = ip.trim()
        if (clean.isBlank()) {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Enter an IP address"}"""
            )))
            return
        }
        if (!isValidIpOrHost(clean)) {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Enter a valid IP like 192.168.1.10"}"""
            )))
            return
        }
        val desktopId = trustedStore.devices.keys.singleOrNull()
        if (desktopId == null) {
            Log.w(TAG, "QUIC connect needs a paired desktop ID (legacy port argument ignored)")
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Select a paired desktop"}"""
            )))
            return
        }
        connect(clean, desktopId)
    }

    /**
     * Establishes the v1 SDK session. The SDK identity is generated once in
     * app-private storage because Android Keystore keys cannot be exported to
     * the PEM files required by the current MsQuic adapter.
     */
    private fun startSdkClipboard(ip: String, desktopId: String) {
        val clean = ip.trim()
        if (clean.isBlank() || !isValidIpOrHost(clean)) {
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"Enter a valid IP like 192.168.1.10"}"""
            )))
            return
        }
        sdkSessionJob?.cancel()
        val generation = sdkSessionGeneration.incrementAndGet()
        val identity = try {
            SdkIdentityStore(context).ensure()
        } catch (error: Exception) {
            Log.w(TAG, "SDK clipboard identity could not be created: ${error.message}")
            return
        }
        val identityDir = File(context.filesDir, "sdk-identity")
        val clientCert = File(identity.certificatePemPath)
        val clientKey = File(identity.privateKeyPemPath)
        val serverCert = File(identityDir, "server-$desktopId.pem")
        if (!serverCert.isFile) {
            val entry = trustedStore.devices[desktopId]
            if (entry == null) {
                Log.i(TAG, "SDK clipboard deferred: desktop certificate is not paired")
                return
            }
            serverCert.writeText(entry.certificatePem.replace("\\n", "\n"))
        }
        val nodeId = MessageDigest.getInstance("SHA-256").digest(deviceId.toByteArray(Charsets.UTF_8))
        val pinnedCertificate = CertificateFactory.getInstance("X.509").generateCertificate(
            ByteArrayInputStream(serverCert.readBytes())
        ).encoded
        val fingerprint = MessageDigest.getInstance("SHA-256").digest(pinnedCertificate)
        val request = try {
            QuicConnectRequest(
                host = clean,
                port = 5027,
                serverName = "anchor.local",
                expectedCertificateFingerprint = fingerprint,
                localIdentity = QuicClientIdentity(clientCert.path, clientKey.path),
                trustedPeerCertificatePemPath = serverCert.path,
            )
        } catch (error: IllegalArgumentException) {
            Log.w(TAG, "SDK connect request invalid: ${error.message}")
            broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                """{"type":"connection_status","status":"disconnected","error":"${error.message}"}"""
            )))
            return
        }
        val sessionIdentity = SessionIdentity(
            nodeId = nodeId,
            displayName = deviceName,
            deviceKindValue = 2,
            endpoints = emptyList(),
        )
        sdkSessionJob = processScope.launch(Dispatchers.IO) {
            val transport = MsQuicTransport()
            try {
                val session = AnchorSession.connect(transport, request, sessionIdentity)
                isVideoTransportReady = true
                trustedStore.updateDeviceName(desktopId, session.peer.displayName)
                trustedStore.updateLastSeen(desktopId)
                Log.i(TAG, "QUIC SDK session connected to $desktopId")
                Log.i(TAG, "SDK peer endpoint catalog: ${session.peer.endpoints.joinToString { endpoint -> endpoint.capabilitiesList.joinToString { it.name } }}")
                val capability = session.openCapability(
                    ClipboardProtocol.ENDPOINT_ID,
                    ClipboardProtocol.CAPABILITY_NAME,
                    ClipboardProtocol.CAPABILITY_MAJOR,
                )
                val openedDeviceCapability = session.openCapability(
                    DeviceProtocol.ENDPOINT_ID,
                    DeviceProtocol.CAPABILITY_NAME,
                    DeviceProtocol.CAPABILITY_MAJOR,
                )
                val batteryManager = context.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
                val batteryPercent = batteryManager.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
                    .coerceIn(0, 100)
                val batteryIntent = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
                val batteryStatus = batteryIntent?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1
                val charging = batteryStatus == BatteryManager.BATTERY_STATUS_CHARGING ||
                    batteryStatus == BatteryManager.BATTERY_STATUS_FULL
                // ACCESS_NETWORK_STATE is optional for the SDK device-state
                // capability. A missing grant must not abort the whole session;
                // report an unknown/false Wi-Fi bit and keep other capabilities
                // usable.
                val wifi = runCatching {
                    (context.getSystemService(Context.CONNECTIVITY_SERVICE) as android.net.ConnectivityManager)
                        .activeNetwork?.let { network ->
                            context.getSystemService(Context.CONNECTIVITY_SERVICE)
                                .let { it as android.net.ConnectivityManager }
                                .getNetworkCapabilities(network)
                                ?.hasTransport(android.net.NetworkCapabilities.TRANSPORT_WIFI)
                        } ?: false
                }.getOrElse { error ->
                    Log.d(TAG, "SDK device Wi-Fi state unavailable: ${error.message}")
                    false
                }
                val (displayWidth, displayHeight) = nativeDisplaySize()
                openedDeviceCapability.sendRecord(
                    DeviceProtocol.STATE_TYPE_URL,
                    DeviceProtocol.encodeState(
                        batteryPercent,
                        charging,
                        wifi,
                        deviceName,
                        displayWidth,
                        displayHeight,
                    ),
                )
                deviceCapability = openedDeviceCapability
                Log.i(TAG, "SDK device state sent ($batteryPercent%, charging=$charging)")
                // Publish connected only once the live device-state capability is
                // installed, so the application's immediate battery refresh uses
                // the same typed channel as later broadcasts.
                broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
                    """{"type":"connection_status","status":"connected","host":"$ip","port":5027,"device_id":"$desktopId"}"""
                )))
                val notificationsCapability = session.openCapability(
                    NotificationsProtocol.ENDPOINT_ID,
                    NotificationsProtocol.CAPABILITY_NAME,
                    NotificationsProtocol.CAPABILITY_MAJOR,
                )
                onSdkNotificationsReady(session, notificationsCapability)
                Log.i(TAG, "SDK notifications capability attached (session=${notificationsCapability.sessionId})")
                val inputCapability = session.openCapability(
                    InputProtocol.ENDPOINT_ID,
                    InputProtocol.CAPABILITY_NAME,
                    InputProtocol.CAPABILITY_MAJOR,
                )
                onSdkInputReady(session, inputCapability)
                Log.i(TAG, "SDK input capability attached (session=${inputCapability.sessionId})")
                val mediaCapability = session.openCapability(
                    MediaProtocol.ENDPOINT_ID,
                    MediaProtocol.CAPABILITY_NAME,
                    MediaProtocol.CAPABILITY_MAJOR,
                )
                onSdkMediaReady(session, mediaCapability)
                Log.i(TAG, "SDK media capability attached (session=${mediaCapability.sessionId})")
                listOf(
                    "org.anchor.screen" to AdditionalProtocols.screen,
                    "org.anchor.camera" to AdditionalProtocols.camera,
                    "org.anchor.files" to AdditionalProtocols.files,
                    "org.anchor.sms" to AdditionalProtocols.sms,
                    "org.anchor.commands" to AdditionalProtocols.commands,
                ).forEach { (name, _) ->
                    val capability = session.openCapability("io.anchor.desktop", name, 1)
                    runCatching {
                        if (name == ScreenProtocol.CAPABILITY_NAME) onSdkScreenReady(session, capability)
                        if (name == CameraProtocol.CAPABILITY_NAME) onSdkCameraReady(session, capability)
                        if (name == "org.anchor.sms") onSdkSmsReady(capability)
                        if (name == "org.anchor.commands") onSdkCommandsReady(capability)
                        if (name == "org.anchor.files") onSdkFilesReady(capability)
                    }.onFailure { error ->
                        // A capability provider is optional. Keep the shared
                        // session alive so later capabilities can still be
                        // negotiated and report the exact failed provider.
                        Log.w(TAG, "SDK $name attachment failed: ${error.message}")
                    }
                    Log.i(TAG, "SDK $name capability attached (session=${capability.sessionId})")
                }
                // Start the clipboard consumer only after all capability-open
                // responses have been consumed by this setup coroutine.  The
                // session has a single event stream; starting a second reader
                // before opening the device capability can race it and consume
                // that response.
                onSdkClipboardReady(session, capability, nodeId)
                Log.i(TAG, "SDK clipboard session connected to $desktopId")
                startSdkEventRouter(session)
                startSdkLatencyProbe(session)
            } catch (error: Exception) {
                transport.close()
                Log.w(TAG, "QUIC SDK session unavailable: ${error.message}")
                // finishSdkSession cancels this setup job when another task
                // already observed the close. Do not emit a second terminal
                // event for that expected cancellation.
                if (isActive) finishSdkSession(generation, error.localizedMessage)
            }
        }
    }

    /**
     * Owns the terminal transition for a live session. QUIC can close without
     * cancelling the coroutine which established it, so this is also called
     * directly by the event router when it observes SessionClosed.
     */
    private fun finishSdkSession(generation: Long, error: String? = null) {
        if (sdkSessionGeneration.get() != generation) return
        sdkSessionJob?.cancel()
        sdkSessionJob = null
        isVideoTransportReady = false
        deviceCapability = null
        stopSdkLatencyProbe()
        sdkEventJob?.cancel()
        sdkEventJob = null
        onSdkTransportStopped()
        videoPlugin.onTransportStopped()
        val detail = error?.takeIf { it.isNotBlank() }?.let {
            ",\"error\":\"${it.replace("\\", "\\\\").replace("\"", "\\\"")}\""
        } ?: ""
        broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
            """{"type":"connection_status","status":"disconnected","reason":"peer_closed"$detail}"""
        )))
    }

    fun disconnect() {
        sdkSessionGeneration.incrementAndGet()
        sdkSessionJob?.cancel()
        sdkSessionJob = null
        sdkEventJob?.cancel()
        sdkEventJob = null
        stopSdkLatencyProbe()
        isVideoTransportReady = false
        deviceCapability = null
        onSdkTransportStopped()
        videoPlugin.onTransportStopped()
        broker.send(
            AnchorEvent(
                target = AnchorTarget.Gui,
                message = AnchorMessage.Json(
                    """{"type":"connection_status","status":"disconnected","reason":"user_disconnect"}"""
                )
            )
        )
    }

    /** Publish a live Android device-state change over the negotiated SDK capability. */
    fun sendDeviceState(batteryPercent: Int, charging: Boolean) {
        val capability = deviceCapability ?: return
        val safePercent = batteryPercent.coerceIn(0, 100)
        processScope.launch(Dispatchers.IO) {
            runCatching {
                val (displayWidth, displayHeight) = nativeDisplaySize()
                capability.sendRecord(
                    DeviceProtocol.STATE_TYPE_URL,
                    DeviceProtocol.encodeState(
                        safePercent,
                        charging,
                        true,
                        deviceName,
                        displayWidth,
                        displayHeight,
                    ),
                )
            }.onFailure { error ->
                Log.d(TAG, "SDK device-state update skipped: ${error.message}")
            }
        }
    }

    /**
     * There is exactly one reader for the SDK control stream. Capability
     * operations may still wait for their own acknowledgements; AnchorSession
     * serializes those reads and preserves unrelated events for this router.
     */
    private fun startSdkEventRouter(session: AnchorSession) {
        val generation = sdkSessionGeneration.get()
        sdkEventJob?.cancel()
        sdkEventJob = processScope.launch(Dispatchers.IO) {
            try {
                while (isActive) {
                    when (val event = session.nextEvent()) {
                        is AnchorSessionEvent.CapabilityRecord -> {
                            Log.i(TAG, "SDK record received: ${event.typeUrl}")
                            onSdkRecord(event)
                            routeSdkRecord(event)
                        }
                        is AnchorSessionEvent.StreamOpenRequested -> onSdkStreamOpen(session, event)
                        is AnchorSessionEvent.SessionClosed -> {
                            Log.i(TAG, "SDK peer closed the session (reason=${event.reasonValue})")
                            finishSdkSession(generation)
                            break
                        }
                        is AnchorSessionEvent.Ping -> session.sendPong(event.nonce)
                        is AnchorSessionEvent.Pong -> reportLatency(event.nonce)
                        // A capability/stream/flow operation may be waiting
                        // for its acknowledgement. Preserve events the
                        // router does not handle instead of dropping a
                        // response when it wins the single-reader race.
                        else -> session.queueEvent(event)
                    }
                }
            } catch (error: Exception) {
                Log.w(TAG, "SDK event router ended: ${error.message}")
                if (isActive) finishSdkSession(generation, error.localizedMessage)
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun nativeDisplaySize(): Pair<Int, Int> {
        val metrics = DisplayMetrics()
        val windowManager = context.getSystemService(Context.WINDOW_SERVICE) as WindowManager
        windowManager.defaultDisplay.getRealMetrics(metrics)
        return metrics.widthPixels to metrics.heightPixels
    }

    private fun startSdkLatencyProbe(session: AnchorSession) {
        stopSdkLatencyProbe()
        val generation = sdkSessionGeneration.get()
        lastSessionPongMs.set(SystemClock.elapsedRealtime())
        sdkLatencyJob = processScope.launch(Dispatchers.IO) {
            while (isActive) {
                val now = SystemClock.elapsedRealtime()
                if (now - lastSessionPongMs.get() > SESSION_HEARTBEAT_TIMEOUT_MS) {
                    Log.w(TAG, "SDK heartbeat timed out; treating desktop as disconnected")
                    session.close()
                    finishSdkSession(generation, "desktop heartbeat timed out")
                    break
                }
                val nonce = nextLatencyProbeNonce.incrementAndGet()
                pendingLatencyProbes[nonce] = SystemClock.elapsedRealtimeNanos()
                try {
                    session.sendPing(nonce)
                } catch (error: Exception) {
                    pendingLatencyProbes.remove(nonce)
                    Log.d(TAG, "SDK latency probe stopped: ${error.message}")
                    session.close()
                    finishSdkSession(generation, error.localizedMessage)
                    break
                }
                delay(LATENCY_PROBE_INTERVAL_MS)
            }
        }
    }

    private fun stopSdkLatencyProbe() {
        sdkLatencyJob?.cancel()
        sdkLatencyJob = null
        pendingLatencyProbes.clear()
    }

    private fun reportLatency(nonce: Long) {
        val sentAtNs = pendingLatencyProbes.remove(nonce) ?: return
        lastSessionPongMs.set(SystemClock.elapsedRealtime())
        val rttMs = (SystemClock.elapsedRealtimeNanos() - sentAtNs) / 1_000_000.0
        broker.send(AnchorEvent(AnchorTarget.Gui, AnchorMessage.Json(
            """{"type":"latency_update","rtt_ms":$rttMs}"""
        )))
    }

    /** Convert typed desktop records to the existing app-facing event model. */
    private fun routeSdkRecord(event: AnchorSessionEvent.CapabilityRecord) {
        val json = runCatching {
            when (event.typeUrl) {
                NotificationsProtocol.POSTED_TYPE_URL -> {
                    val notification = NotificationsProtocol.decodePosted(event.payload)
                    JSONObject().apply {
                        put("plugin_id", "foghorn").put("type", "notification")
                            .put("source", "desktop")
                            .put("notification_id", notification.notificationId)
                            .put("app_package", notification.applicationId)
                            .put("app_name", notification.applicationName)
                            .put("title", notification.title)
                            .put("body", notification.body)
                            .put("timestamp", notification.postedAtUnixMs / 1000L)
                    }
                }
                ScreenProtocol.OUTPUT_LIST_TYPE_URL -> {
                    val outputs = ScreenProtocol.decodeOutputList(event.payload)
                    JSONObject().apply {
                        put("plugin_id", "video").put("type", "output_list")
                        put("outputs", org.json.JSONArray().apply {
                            outputs.forEach { output ->
                                put(JSONObject().apply {
                                    put("id", output.outputId.toIntOrNull() ?: 0)
                                    put("name", output.displayName)
                                    put("width", output.width)
                                    put("height", output.height)
                                })
                            }
                        })
                    }
                }
                ScreenProtocol.STATUS_TYPE_URL -> {
                    val status = ScreenProtocol.decodeStatus(event.payload)
                    val state = when (status.stateValue) {
                        1 -> "idle"
                        2 -> "starting"
                        3 -> "streaming"
                        4 -> "paused"
                        5 -> "error"
                        else -> "idle"
                    }
                    JSONObject().apply {
                        put("plugin_id", "video").put("type", "stream_status")
                            .put("state", state)
                            .put("selected_index", status.outputId.toIntOrNull() ?: 0)
                    }
                }
                MediaProtocol.STATE_TYPE_URL -> {
                    val state = MediaProtocol.decodeState(event.payload)
                    JSONObject().apply {
                        put("plugin_id", "media").put("type", "media_state")
                            .put("source", "desktop")
                            .put("state", if (state.playing) "playing" else "paused")
                            .put("title", state.title).put("artist", state.artist).put("album", state.album)
                            .put("position_ms", state.positionMs).put("duration_ms", state.durationMs)
                            .put("session_id", "sdk").put("app", "desktop")
                            .put("can_play", true).put("can_pause", true).put("can_next", true)
                            .put("can_prev", true).put("can_seek", true)
                        if (
                            state.artworkJpeg.size <= MediaProtocol.MAX_ARTWORK_JPEG_BYTES &&
                            state.artworkJpeg.isNotEmpty()
                        ) {
                            put(
                                "artwork_jpeg_base64",
                                android.util.Base64.encodeToString(
                                    state.artworkJpeg,
                                    android.util.Base64.NO_WRAP,
                                ),
                            )
                        }
                    }
                }
                MediaProtocol.COMMAND_TYPE_URL -> {
                    val command = MediaProtocol.decodeCommand(event.payload)
                    val name = when (command.kindValue) {
                        1 -> "play"
                        2 -> "pause"
                        3 -> "next"
                        4 -> "previous"
                        5 -> "seek"
                        else -> return@runCatching null
                    }
                    JSONObject().apply {
                        put("plugin_id", "media").put("type", "media_command")
                            .put("command", name).put("position_ms", command.positionMs)
                    }
                }
                DeviceProtocol.STATE_TYPE_URL -> {
                    val state = DeviceProtocol.decodeState(event.payload)
                    JSONObject().apply {
                        put("plugin_id", "gui").put("type", "battery_update")
                            .put("device_id", "desktop")
                            .put("level", state.batteryPercent)
                            .put("charging", state.charging)
                            .put("status_text", if (state.charging) "Charging" else "Discharging")
                    }
                }
                else -> null
            }
        }.getOrElse { error ->
            Log.w(TAG, "SDK record decode failed (${event.typeUrl}): ${error.message}")
            null
        } ?: return
        val target = if (json.optString("plugin_id") == "gui") AnchorTarget.Gui
            else AnchorTarget.Service(json.optString("plugin_id"))
        broker.send(AnchorEvent(target, AnchorMessage.Json(json.toString())))
    }

    /**
     * Send an H.264 camera frame upstream to the desktop.
     *
     * Enqueues the frame for the background sender thread. Safe to call from the main
     * thread or MediaCodec async callbacks without triggering NetworkOnMainThreadException.
     */
    fun sendCameraFrame(data: ByteArray) {
        // Camera frames must use the negotiated SDK capability/QUIC flow. The
        Log.w(TAG, "Dropping camera frame: SDK camera flow is not available (${data.size} bytes)")
    }

    override fun stop() {
        sdkSessionGeneration.incrementAndGet()
        sdkSessionJob?.cancel()
        sdkSessionJob = null
        sdkEventJob?.cancel()
        sdkEventJob = null
        isVideoTransportReady = false
        onSdkTransportStopped()
        videoPlugin.onTransportStopped()
    }
}
