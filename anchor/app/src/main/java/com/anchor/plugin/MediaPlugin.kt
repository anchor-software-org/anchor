package com.anchor.plugin

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadata
import android.media.VolumeProvider
import android.media.session.MediaController
import android.media.session.MediaSession
import android.media.session.MediaSessionManager
import android.media.session.PlaybackState
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import java.io.ByteArrayOutputStream
import android.util.Base64
import com.anchor.R
import com.anchor.core.AnchorEvent
import com.anchor.core.AnchorMessage
import com.anchor.core.AnchorTarget
import com.anchor.core.MessageBroker
import com.anchor.core.Plugin
import org.anchor.sdk.AnchorCapability
import org.anchor.sdk.AnchorSession
import org.anchor.sdk.MediaProtocol
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

private const val TAG = "anchor"
private const val CHANNEL_ID = "anchor_media"
private const val CHANNEL_NAME = "Desktop Media"
private const val NOTIFICATION_ID = 9100

/** Position fingerprint bucket (ms). Phone→desktop sends are gated to one per bucket. */
private const val POSITION_BUCKET_MS = 2_000L
private const val MAX_ARTWORK_DIMENSION = 512

/** Runtime broadcast action carrying a transport command from a notification button. */
private const val ACTION_CMD = "com.anchor.media.CMD"
private const val EXTRA_CMD = "cmd"

/**
 * How long we wait, after start(), for the desktop to send its first
 * `media_state`. If we hear nothing before this expires, we assume the
 * desktop is idle and proceed to classify our local session. Short enough
 * that a user watching the UI won't experience a long "nothing here yet"
 * state; long enough to reliably beat the 1 s desktop poll interval plus
 * network round-trip.
 */
private const val STARTUP_GRACE_MS = 3_000L

/**
 * How long we wait after receiving a desktop `stopped` / empty-title state
 * before committing it (clearing `desktopAnchor`). This absorbs the brief
 * "nothing playing" window desktop players and their MPRIS bridges can
 * momentarily report during track transitions. Without it, we'd flap out
 * of MIRROR into INDEPENDENT, forward local state, then immediately go
 * back to MIRROR when the desktop's next tick reports the new track —
 * producing the "shows up for a second and disappears" flicker.
 */
private const val DESKTOP_STOPPED_DEBOUNCE_MS = 1_500L

/** Exposed playback state for UI rendering. */
data class MediaPlaybackState(
    val source: String,
    val title: String,
    val artist: String,
    val album: String,
    val app: String,
    val state: String,
    val positionMs: Long,
    val durationMs: Long,
    val canPlay: Boolean,
    val canPause: Boolean,
    val canNext: Boolean,
    val canPrev: Boolean,
    val canSeek: Boolean,
    val artUrl: String = ""
)

/**
 * Media playback control plugin (`plugin_id: "media"`).
 *
 * Bidirectional bridge between the phone and desktop media players:
 *  - **desktop → phone**: mirrors the desktop's now-playing as a MediaStyle
 *    notification with transport controls. Tapping a control sends a
 *    `media_command` to the desktop.
 *  - **phone → desktop**: reads the phone's own active media session (Spotify,
 *    YouTube Music, browsers, …) via [MediaSessionManager] and forwards
 *    `media_state` (source: "android") to the desktop so it can show an MPRIS
 *    player / in-app panel and relay controls back via `media_command`.
 *
 * Both directions coexist; the `source` field ("desktop" / "android")
 * disambiguates which player a message describes or targets.
 */
class MediaPlugin(
    private val broker: MessageBroker,
    private val context: Context
) : Plugin {

    override val pluginId = "media"

    /** Desktop now-playing, rendered in the notification and UI. */
    private val _desktopPlayback = MutableStateFlow<MediaPlaybackState?>(null)
    val desktopPlayback: StateFlow<MediaPlaybackState?> = _desktopPlayback.asStateFlow()

    /** Phone's own now-playing, forwarded to the desktop and shown in the UI. */
    private val _phonePlayback = MutableStateFlow<MediaPlaybackState?>(null)
    val phonePlayback: StateFlow<MediaPlaybackState?> = _phonePlayback.asStateFlow()

    /** Decoded album art for the current desktop track. Exposed so the UI can
     *  consume the already-fetched bitmap instead of re-downloading it. */
    private val _desktopArt = MutableStateFlow<Bitmap?>(null)
    val desktopArt: StateFlow<Bitmap?> = _desktopArt.asStateFlow()

    private val _phoneArt = MutableStateFlow<Bitmap?>(null)
    val phoneArt: StateFlow<Bitmap?> = _phoneArt.asStateFlow()
    private var phoneArtworkMetadata: MediaMetadata? = null

    /** Whether we have NotificationListener access to read the phone's media
     *  sessions. UI uses this to surface a "grant access" CTA. */
    private val _permissionGranted = MutableStateFlow(false)
    val permissionGranted: StateFlow<Boolean> = _permissionGranted.asStateFlow()

    /** Open Android's NotificationListener settings so the user can grant access. */
    fun openNotificationListenerSettings() {
        val intent = Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK
        }
        context.startActivity(intent)
    }

    /** Send a transport command from the UI. Dispatches to desktop or applies
     *  locally based on the source. */
    fun sendTransportCommand(command: String, source: String = "desktop") {
        if (source == "desktop") {
            sendCommand(command)
        } else {
            val ctrl = mediaController ?: return
            val tc = ctrl.transportControls
            when (command) {
                "play" -> tc.play()
                "pause" -> tc.pause()
                "playpause" -> if (ctrl.playbackState?.state == PlaybackState.STATE_PLAYING) tc.pause() else tc.play()
                "next" -> tc.skipToNext()
                "previous" -> tc.skipToPrevious()
            }
        }
    }

    private var listenJob: Job? = null
    private var session: MediaSession? = null
    private var permissionRetryJob: Job? = null
    private var localTickJob: Job? = null
    private var desktopTickJob: Job? = null
    private var pluginScope: CoroutineScope? = null
    @Volatile private var sdkCapability: AnchorCapability? = null
    private val mainHandler = Handler(Looper.getMainLooper())

    /** Anchor for desktop playback: the last state we received from the desktop,
     *  the playback speed at that moment, and the local monotonic time when it
     *  landed. The 1Hz tick extrapolates the live position from this base so the
     *  progress bar moves smoothly between (rare) anchor messages. */
    private data class DesktopAnchor(
        val state: MediaPlaybackState,
        val speed: Float,
        val anchorElapsedRealtime: Long
    )

    @Volatile private var desktopAnchor: DesktopAnchor? = null

    /** Bus name of the desktop session we're currently mirroring, echoed back in
     *  commands so the desktop can target the right player. */
    private var currentSessionId: String = ""

    /** Last phone→desktop fingerprint we transmitted. Gates network sends to
     *  one per [POSITION_BUCKET_MS] window — see [forwardIndependentState]. */
    private var lastSentFingerprint: String = ""

    // Everything under `stateLock` because inputs arrive from three threads:
    //   * MediaController.Callback (main handler) — local session changes
    //   * broker.events collector coroutine       — desktop wire updates
    //   * localTickJob coroutine                  — 1 Hz local position tick
    // Any read-modify-send sequence has to be atomic or roles flap.
    private val stateLock = Any()

    /** True once we've either received a desktop `media_state` or the
     *  startup grace window has elapsed. Until this flips, `currentRole`
     *  stays at STARTUP and we send nothing on the wire. */
    private var startupComplete: Boolean = false

    /** Current classification of our local session vs. desktop's. Guarded
     *  by [stateLock]. */
    private var currentRole: PhonePlaybackRole = PhonePlaybackRole.STARTUP

    /** One-shot coroutine that flips [startupComplete] after
     *  [STARTUP_GRACE_MS]. Cancelled when we receive a real desktop state
     *  before then. */
    private var startupGraceJob: Job? = null

    /** Pending "commit desktop stopped" job. Set when we receive a stopped/
     *  empty desktop state and cleared if a non-stopped desktop state
     *  arrives inside [DESKTOP_STOPPED_DEBOUNCE_MS]. */
    private var desktopStoppedDebounceJob: Job? = null

    private val cmdReceiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context?, intent: Intent?) {
            val cmd = intent?.getStringExtra(EXTRA_CMD) ?: return
            sendCommand(cmd)
        }
    }


    private val mediaSessionManager: MediaSessionManager by lazy {
        context.getSystemService(Context.MEDIA_SESSION_SERVICE) as MediaSessionManager
    }

    private var mediaController: MediaController? = null
    private var sessionsListener: MediaSessionManager.OnActiveSessionsChangedListener? = null

    private val notificationListenerComponent by lazy {
        ComponentName(context, AnchorNotificationListener::class.java)
    }

    private val localCallback = object : MediaController.Callback() {
        override fun onPlaybackStateChanged(state: PlaybackState?) {
            onLocalUpdate()
        }

        override fun onMetadataChanged(metadata: MediaMetadata?) {
            onLocalUpdate()
        }

        override fun onSessionDestroyed() {
            Log.i(TAG, "MediaPlugin: local media session destroyed")
            mediaController?.unregisterCallback(this)
            mediaController = null
            // No local session anymore — the reclassifier will transition us
            // to IDLE and fire a `stopped` if we weren't already silent.
            onLocalUpdate()
        }
    }


    override fun start(scope: CoroutineScope) {
        pluginScope = scope
        createChannel()
        registerCmdReceiver()
        // MediaSession is created lazily on first inbound desktop state — see
        // ensureSession(). Avoids registering a phantom remote-playback session
        // for users who never connect a desktop.

        listenJob = scope.launch {
            broker.events.collect { event ->
                val target = event.target
                if (target is AnchorTarget.Service && target.id == pluginId) {
                    handleIncoming(event)
                }
            }
        }

        // Try to register the sessions-changed listener immediately. If the user
        // hasn't granted notification-listener access, retry on a low-rate poll
        // until they do — then exit. The listener pushes session changes (no
        // polling needed), so this job stops after a successful register.
        permissionRetryJob = scope.launch {
            while (true) {
                if (tryRegisterSessionsListener()) break
                delay(5_000L)
            }
        }

        // Tick the local position while playing so the UI's progress bar moves
        // every second. MediaController.Callback only fires on metadata / state
        // changes, so without this the timer would freeze between events. The
        // per-tick reclassify is cheap (a few field reads); the wire send is
        // still gated by the position-bucket fingerprint.
        localTickJob = scope.launch {
            while (true) {
                delay(1_000L)
                val ctrl = mediaController ?: continue
                if (ctrl.playbackState?.state == PlaybackState.STATE_PLAYING) {
                    onLocalUpdate()
                }
            }
        }

        // Startup grace: if we don't hear from the desktop within
        // STARTUP_GRACE_MS, treat it as idle and proceed. Prevents an
        // indefinitely silent STARTUP state if a phone connects but the
        // desktop's poll heartbeat hasn't fired yet.
        startupGraceJob = scope.launch {
            delay(STARTUP_GRACE_MS)
            synchronized(stateLock) {
                if (!startupComplete) {
                    Log.i(TAG, "MediaPlugin: startup grace elapsed without desktop state, assuming idle")
                    startupComplete = true
                    reclassifyLocked()
                }
            }
        }

        Log.i(TAG, "MediaPlugin started (bidirectional)")
    }

    fun attachSdkSession(session: AnchorSession, capability: AnchorCapability) {
        sdkCapability = capability
        Log.i(TAG, "SDK media capability attached (session=${capability.sessionId})")
    }

    /** Lazily create the MediaSession used to render the desktop's playback on
     *  the lockscreen. Called the first time we receive a desktop media_state. */
    private fun ensureSession(): MediaSession {
        session?.let { return it }
        return MediaSession(context, "com.anchor.AnchorMedia").apply {
            setCallback(sessionCallback)
            isActive = true
            // Mark this as a *remote* playback session — the audio is on the
            // desktop, not the phone. Without this, Android shows "Phone
            // speaker" as the output route, which is misleading. We don't
            // expose volume control (the desktop owns it), so the
            // VolumeProvider is fixed and accepts no changes.
            setPlaybackToRemote(
                object : VolumeProvider(VOLUME_CONTROL_FIXED, 100, 100) {
                    override fun onSetVolumeTo(volume: Int) {}
                    override fun onAdjustVolume(direction: Int) {}
                }
            )
            session = this
        }
    }

    /** Subscribe to active-session changes if we have notification-listener
     *  access. Returns true when the listener is live (now or already). */
    private fun tryRegisterSessionsListener(): Boolean {
        if (sessionsListener != null) return true
        val listener = MediaSessionManager.OnActiveSessionsChangedListener { ctrls ->
            attachBestSession(ctrls.orEmpty())
        }
        return try {
            mediaSessionManager.addOnActiveSessionsChangedListener(
                listener, notificationListenerComponent, mainHandler
            )
            sessionsListener = listener
            _permissionGranted.value = true
            attachBestSession(mediaSessionManager.getActiveSessions(notificationListenerComponent))
            true
        } catch (e: SecurityException) {
            _permissionGranted.value = false
            false
        }
    }

    /** Pick the best candidate from `sessions` and (re)attach if it differs
     *  from the current controller. Prefers a playing session over a paused
     *  one, mirroring the desktop's `pick_active_player`. */
    private fun attachBestSession(sessions: List<MediaController>) {
        val best = pickBestSession(sessions)
        if (best == null) {
            mediaController?.let {
                it.unregisterCallback(localCallback)
                mediaController = null
            }
            onLocalUpdate()
            return
        }
        if (mediaController?.sessionToken == best.sessionToken) {
            onLocalUpdate()
            return
        }
        mediaController?.unregisterCallback(localCallback)
        mediaController = best
        best.registerCallback(localCallback, mainHandler)
        Log.i(TAG, "MediaPlugin: attached to local session ${best.packageName}")
        onLocalUpdate()
    }

    private fun pickBestSession(sessions: List<MediaController>): MediaController? {
        val ours = context.packageName
        val candidates = sessions.filter {
            it.packageName != ours && it.playbackState != null
        }
        return candidates.firstOrNull { it.playbackState?.state == PlaybackState.STATE_PLAYING }
            ?: candidates.firstOrNull { it.playbackState?.state == PlaybackState.STATE_PAUSED }
            ?: candidates.firstOrNull()
    }


    /**
     * Read the current local MediaController into a [TrackId] for the
     * classifier — or `null` if there's no session or it's effectively
     * stopped. Framework calls only; safe from any thread since
     * `MediaController` reads are synchronous.
     */
    private fun currentLocalTrack(): TrackId? {
        val ctrl = mediaController ?: return null
        val ps = ctrl.playbackState
        // No PlaybackState, or a genuinely-stopped/none state, means we have
        // nothing to report. (Paused counts as "we have a session" — user
        // can still resume and it's a valid phone-owned playback.)
        if (ps == null ||
            ps.state == PlaybackState.STATE_STOPPED ||
            ps.state == PlaybackState.STATE_NONE
        ) {
            return null
        }
        val meta = ctrl.metadata ?: return null
        val title = meta.getString(MediaMetadata.METADATA_KEY_TITLE).orEmpty()
        if (title.isEmpty()) return null
        val artist = meta.getString(MediaMetadata.METADATA_KEY_ARTIST).orEmpty()
        val durationMs = meta.getLong(MediaMetadata.METADATA_KEY_DURATION).coerceAtLeast(0)
        return TrackId(title = title, artist = artist, durationMs = durationMs)
    }

    /**
     * Push a fresh snapshot into the phone-side UI flow (`_phonePlayback`)
     * so the panel and progress bar always reflect the *actual* local
     * session, independent of whether we're sending it on the wire. When
     * classified as MIRROR / IDLE, we set the flow to `null` — the UI
     * shouldn't advertise phone-owned playback that we're deliberately
     * suppressing.
     */
    private fun refreshPhonePlaybackFlowLocked(role: PhonePlaybackRole) {
        if (role != PhonePlaybackRole.INDEPENDENT) {
            _phonePlayback.value = null
            _phoneArt.value = null
            phoneArtworkMetadata = null
            return
        }
        val ctrl = mediaController ?: run { _phonePlayback.value = null; return }
        val ps = ctrl.playbackState ?: run { _phonePlayback.value = null; return }
        val meta = ctrl.metadata
        refreshPhoneArtwork(meta)
        val title = meta?.getString(MediaMetadata.METADATA_KEY_TITLE).orEmpty()
        val artist = meta?.getString(MediaMetadata.METADATA_KEY_ARTIST).orEmpty()
        val album = meta?.getString(MediaMetadata.METADATA_KEY_ALBUM).orEmpty()
        val durationMs = meta?.getLong(MediaMetadata.METADATA_KEY_DURATION)?.coerceAtLeast(0) ?: 0L

        val stateStr = when (ps.state) {
            PlaybackState.STATE_PLAYING -> "playing"
            PlaybackState.STATE_PAUSED -> "paused"
            else -> "stopped"
        }

        val basePos = ps.position.coerceAtLeast(0)
        val positionMs = if (ps.state == PlaybackState.STATE_PLAYING) {
            val elapsed = SystemClock.elapsedRealtime() - ps.lastPositionUpdateTime
            (basePos + (elapsed * ps.playbackSpeed).toLong()).coerceAtLeast(0)
        } else {
            basePos
        }

        val actions = ps.actions
        _phonePlayback.value = MediaPlaybackState(
            source = "android",
            title = title, artist = artist, album = album,
            app = ctrl.packageName,
            state = stateStr,
            positionMs = positionMs, durationMs = durationMs,
            canPlay = (actions and PlaybackState.ACTION_PLAY) != 0L,
            canPause = (actions and PlaybackState.ACTION_PAUSE) != 0L,
            canNext = (actions and PlaybackState.ACTION_SKIP_TO_NEXT) != 0L,
            canPrev = (actions and PlaybackState.ACTION_SKIP_TO_PREVIOUS) != 0L,
            canSeek = (actions and PlaybackState.ACTION_SEEK_TO) != 0L,
        )
    }

    /**
     * Input: something about the local session changed (attach, detach,
     * metadata / state change, 1 Hz position tick). Recompute the role and
     * fire any transition action.
     */
    private fun onLocalUpdate() {
        synchronized(stateLock) { reclassifyLocked() }
    }

    /**
     * Recompute the current role from local + desktop snapshots, run any
     * transition action, refresh the UI flow, and (if INDEPENDENT) send the
     * latest local state on the wire.
     *
     * Must be called under [stateLock].
     */
    private fun reclassifyLocked() {
        val localTrack = currentLocalTrack()
        val desktopSnap = desktopAnchor?.state
        val newRole = classifyPhoneRole(localTrack, desktopSnap, startupComplete)

        if (newRole != currentRole) {
            Log.i(TAG, "MediaPlugin: role transition $currentRole -> $newRole")
            handleRoleEntryLocked(newRole)
            currentRole = newRole
        }

        refreshPhonePlaybackFlowLocked(newRole)

        // Continuous behavior: INDEPENDENT forwards on every reclassify
        // (throttled by fingerprint). Every other role stays silent.
        if (newRole == PhonePlaybackRole.INDEPENDENT) {
            sendCurrentLocalStateOverWire()
        }
    }

    /**
     * Perform the one-shot action associated with *entering* [role]. Called
     * under [stateLock] before we commit the transition.
     */
    private fun handleRoleEntryLocked(role: PhonePlaybackRole) {
        when (role) {
            PhonePlaybackRole.STARTUP -> {
                // Only ever transitioned *out of*, never into.
            }
            PhonePlaybackRole.INDEPENDENT -> {
                // Next `sendCurrentLocalStateOverWire()` inside `reclassifyLocked`
                // publishes the fresh state. Reset the fingerprint so that
                // send isn't accidentally throttled by a stale one from a
                // prior INDEPENDENT run.
                lastSentFingerprint = ""
            }
            PhonePlaybackRole.IDLE, PhonePlaybackRole.MIRROR -> {
                // Tell the desktop to drop any phone-owned MPRIS state it's
                // currently holding, and clear our own fingerprint so the
                // next INDEPENDENT run can send a fresh anchor.
                sendStoppedOverWire()
                lastSentFingerprint = ""
            }
        }
    }

    /**
     * Wire-side send used in INDEPENDENT: publish current local state to
     * the desktop. Throttled by a position-bucketed fingerprint so we send
     * ~once per [POSITION_BUCKET_MS] window while a track plays instead of
     * on every 1 Hz tick.
     */
    private fun sendCurrentLocalStateOverWire() {
        val ctrl = mediaController ?: return
        val ps = ctrl.playbackState ?: return
        val meta = ctrl.metadata
        val title = meta?.getString(MediaMetadata.METADATA_KEY_TITLE).orEmpty()
        // Defensive guard.
        if (title.isEmpty() && ps.state == PlaybackState.STATE_NONE) return

        val artist = meta?.getString(MediaMetadata.METADATA_KEY_ARTIST).orEmpty()
        val album = meta?.getString(MediaMetadata.METADATA_KEY_ALBUM).orEmpty()
        val durationMs = meta?.getLong(MediaMetadata.METADATA_KEY_DURATION)?.coerceAtLeast(0) ?: 0L

        val stateStr = when (ps.state) {
            PlaybackState.STATE_PLAYING -> "playing"
            PlaybackState.STATE_PAUSED -> "paused"
            PlaybackState.STATE_STOPPED -> "stopped"
            else -> "stopped"
        }

        val actions = ps.actions
        val caps = Caps(
            play = (actions and PlaybackState.ACTION_PLAY) != 0L,
            pause = (actions and PlaybackState.ACTION_PAUSE) != 0L,
            next = (actions and PlaybackState.ACTION_SKIP_TO_NEXT) != 0L,
            prev = (actions and PlaybackState.ACTION_SKIP_TO_PREVIOUS) != 0L,
            seek = (actions and PlaybackState.ACTION_SEEK_TO) != 0L,
        )

        val basePos = ps.position.coerceAtLeast(0)
        val positionMs = if (ps.state == PlaybackState.STATE_PLAYING) {
            val elapsed = SystemClock.elapsedRealtime() - ps.lastPositionUpdateTime
            (basePos + (elapsed * ps.playbackSpeed).toLong()).coerceAtLeast(0)
        } else {
            basePos
        }

        val sessionId = "${ctrl.packageName}:${ctrl.sessionToken.hashCode()}"
        val artworkJpeg = artworkJpeg(meta)
        val fingerprint =
            "$sessionId|$stateStr|$title|$artist|$durationMs|${positionMs / POSITION_BUCKET_MS}|${artworkJpeg.contentHashCode()}"
        if (fingerprint == lastSentFingerprint) return
        lastSentFingerprint = fingerprint

        val payload = buildJsonObject {
            put("plugin_id", "media")
            put("type", "media_state")
            put("source", "android")
            put("app", ctrl.packageName)
            put("session_id", sessionId)
            put("state", stateStr)
            put("title", title)
            if (artist.isNotEmpty()) put("artist", artist)
            if (album.isNotEmpty()) put("album", album)
            put("position_ms", positionMs)
            put("duration_ms", durationMs)
            put("can_play", caps.play)
            put("can_pause", caps.pause)
            put("can_next", caps.next)
            put("can_prev", caps.prev)
            put("can_seek", caps.seek)
        }
        sdkCapability?.let { sdk ->
            val typed = MediaProtocol.encodeState(
                title = title,
                artist = artist,
                album = album,
                playing = stateStr == "playing",
                positionMs = positionMs,
                durationMs = durationMs,
                artworkJpeg = artworkJpeg,
            )
            pluginScope?.launch(Dispatchers.IO) {
                runCatching { sdk.sendRecord(MediaProtocol.STATE_TYPE_URL, typed) }
                    .onFailure { error -> Log.w(TAG, "SDK media state send failed: ${error.message}") }
            }
            return
        }
        broker.send(AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(payload.toString())))
    }

    /**
     * Tell the desktop we have no phone-owned playback to report. Sent when
     * the local session goes away (IDLE) or is being suppressed as a mirror
     * of the desktop's own playback (MIRROR).
     *
     * The payload has to include *every* field the desktop's `MediaState`
     * struct declares as non-optional (`session_id`, `artist`, `album`, `app`,
     * `position_ms`, `duration_ms`), otherwise serde on the desktop side
     * rejects the message with `missing field …` and never clears its
     * `remote_state` — the phone MPRIS mirror then stays stuck showing
     *
     * UI-side flow (`_phonePlayback`) and `lastSentFingerprint` are managed
     * by the state machine (`refreshPhonePlaybackFlowLocked` /
     * `handleRoleEntryLocked`), not here.
     */
    private fun sendStoppedOverWire() {
        val payload = buildJsonObject {
            put("plugin_id", "media")
            put("type", "media_state")
            put("source", "android")
            put("session_id", "")
            put("state", "stopped")
            put("title", "")
            put("artist", "")
            put("album", "")
            put("app", "")
            put("position_ms", 0L)
            put("duration_ms", 0L)
            put("can_play", false)
            put("can_pause", false)
            put("can_next", false)
            put("can_prev", false)
            put("can_seek", false)
        }
        sdkCapability?.let { sdk ->
            val typed = MediaProtocol.encodeState("", "", "", false, 0, 0)
            pluginScope?.launch(Dispatchers.IO) {
                runCatching { sdk.sendRecord(MediaProtocol.STATE_TYPE_URL, typed) }
                    .onFailure { error -> Log.w(TAG, "SDK stopped-state send failed: ${error.message}") }
            }
            return
        }
        broker.send(AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(payload.toString())))
    }

    /** Inbound events: desktop's media_state for phone rendering, or media_command to apply locally. */
    private fun handleIncoming(event: AnchorEvent) {
        val msg = event.message
        if (msg !is AnchorMessage.Json) return
        try {
            val json = Json.parseToJsonElement(msg.payload).jsonObject
            val type = json["type"]?.jsonPrimitive?.content ?: return

            when (type) {
                "media_state" -> handleState(json)
                "media_command" -> handleCommand(json)
            }
        } catch (e: Exception) {
            Log.e(TAG, "MediaPlugin: failed to parse event: ${e.message}")
        }
    }

    /** Desktop playback update → render notification and drive the phone-side
     *  state machine. */
    private fun handleState(json: kotlinx.serialization.json.JsonObject) {
        val source = json["source"]?.jsonPrimitive?.content
        if (source != "desktop") return

        val state = json["state"]?.jsonPrimitive?.content ?: "stopped"
        val title = json["title"]?.jsonPrimitive?.content.orEmpty()

        if (state == "stopped" || title.isEmpty()) {
            scheduleDesktopStoppedCommit()
            return
        }

        currentSessionId = json["session_id"]?.jsonPrimitive?.content.orEmpty()
        val artist = json["artist"]?.jsonPrimitive?.content.orEmpty()
        val album = json["album"]?.jsonPrimitive?.content.orEmpty()
        val app = json["app"]?.jsonPrimitive?.content.orEmpty()
        val positionMs = json["position_ms"]?.jsonPrimitive?.content?.toLongOrNull() ?: 0L
        val durationMs = json["duration_ms"]?.jsonPrimitive?.content?.toLongOrNull() ?: 0L
        val caps = Caps(
            play = json["can_play"]?.jsonPrimitive?.content?.toBoolean() ?: false,
            pause = json["can_pause"]?.jsonPrimitive?.content?.toBoolean() ?: false,
            next = json["can_next"]?.jsonPrimitive?.content?.toBoolean() ?: false,
            prev = json["can_prev"]?.jsonPrimitive?.content?.toBoolean() ?: false,
            seek = json["can_seek"]?.jsonPrimitive?.content?.toBoolean() ?: false
        )

        val artUrl = json["art_url"]?.jsonPrimitive?.content.orEmpty()
        val speed = json["playback_speed"]?.jsonPrimitive?.content?.toFloatOrNull() ?: 1.0f

        _desktopArt.value = json["artwork_jpeg_base64"]?.jsonPrimitive?.contentOrNull
            ?.let(::decodeArtwork)

        val snapshot = MediaPlaybackState(
            source = "desktop",
            title = title, artist = artist, album = album, app = app,
            state = state, positionMs = positionMs, durationMs = durationMs,
            canPlay = caps.play, canPause = caps.pause,
            canNext = caps.next, canPrev = caps.prev, canSeek = caps.seek,
            artUrl = artUrl
        )
        val anchor = DesktopAnchor(snapshot, speed, SystemClock.elapsedRealtime())

        // Any real (non-stopped) desktop update cancels a pending
        // stopped-debounce (this *is* the "actually there was another track"
        // signal the debounce is waiting for), completes startup, and
        // becomes the new anchor. Reclassify locally against it.
        synchronized(stateLock) {
            startupGraceJob?.cancel()
            startupGraceJob = null
            desktopStoppedDebounceJob?.cancel()
            desktopStoppedDebounceJob = null
            startupComplete = true
            desktopAnchor = anchor
            _desktopPlayback.value = snapshot
            reclassifyLocked()
        }

        ensureSession()
        updateSession(snapshot, _desktopArt.value, anchor.anchorElapsedRealtime, speed)
        showNotification(snapshot, _desktopArt.value)
        ensureDesktopTick()
    }

    private fun artworkJpeg(metadata: MediaMetadata?): ByteArray {
        val source = metadata?.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
            ?: metadata?.getBitmap(MediaMetadata.METADATA_KEY_ART)
            ?: return ByteArray(0)
        val largestSide = maxOf(source.width, source.height)
        val image = if (largestSide > MAX_ARTWORK_DIMENSION) {
            val scale = MAX_ARTWORK_DIMENSION.toFloat() / largestSide
            Bitmap.createScaledBitmap(
                source,
                (source.width * scale).toInt().coerceAtLeast(1),
                (source.height * scale).toInt().coerceAtLeast(1),
                true,
            )
        } else {
            source
        }
        for (quality in intArrayOf(85, 70, 55, 40)) {
            val output = ByteArrayOutputStream()
            if (image.compress(Bitmap.CompressFormat.JPEG, quality, output) && output.size() <= MediaProtocol.MAX_ARTWORK_JPEG_BYTES) {
                return output.toByteArray()
            }
        }
        return ByteArray(0)
    }

    private fun refreshPhoneArtwork(metadata: MediaMetadata?) {
        if (metadata === phoneArtworkMetadata) return
        phoneArtworkMetadata = metadata
        val source = metadata?.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART)
            ?: metadata?.getBitmap(MediaMetadata.METADATA_KEY_ART)
        if (source == null) {
            _phoneArt.value = null
            return
        }
        val largestSide = maxOf(source.width, source.height)
        _phoneArt.value = if (largestSide > MAX_ARTWORK_DIMENSION) {
            val scale = MAX_ARTWORK_DIMENSION.toFloat() / largestSide
            Bitmap.createScaledBitmap(
                source,
                (source.width * scale).toInt().coerceAtLeast(1),
                (source.height * scale).toInt().coerceAtLeast(1),
                true,
            )
        } else {
            source
        }
    }

    private fun decodeArtwork(encoded: String): Bitmap? = runCatching {
        val bytes = Base64.decode(encoded, Base64.NO_WRAP)
        if (bytes.size > MediaProtocol.MAX_ARTWORK_JPEG_BYTES) null else BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
    }.getOrNull()

    /**
     * Handle a desktop `stopped` / empty-title state. Instead of committing
     * immediately, wait [DESKTOP_STOPPED_DEBOUNCE_MS]. If a real update
     * arrives before the timer fires, the debounce is cancelled by
     * [handleState] and the anchor is preserved — no MIRROR→INDEPENDENT
     * flap during the momentary null MPRIS bridges emit at track
     * boundaries. If no update arrives, commit: clear the anchor + UI and
     * reclassify (typically MIRROR → INDEPENDENT if we now own the sole
     * playback, or IDLE if there's nothing local).
     *
     * If we have no anchor yet (first message on connect, or already
     * cleared), the "stopped" *is* the actual state — commit immediately
     * so the classifier can move us out of STARTUP.
     */
    private fun scheduleDesktopStoppedCommit() {
        val scope = pluginScope ?: return
        synchronized(stateLock) {
            // Any inbound desktop state — even stopped — is proof the
            // desktop is talking to us; startup can complete.
            startupGraceJob?.cancel()
            startupGraceJob = null
            val startupJustCompleted = !startupComplete
            startupComplete = true

            if (desktopAnchor == null) {
                // Nothing to protect — commit stopped immediately so the
                // classifier can transition out of STARTUP.
                if (startupJustCompleted) reclassifyLocked()
                return
            }

            desktopStoppedDebounceJob?.cancel()
            // Capture our own Job so the callback can tell "am I still the
            // active debounce" — if cancel()+relaunch happened between our
            // delay resuming and the sync lock, the field will point at a
            // newer job and we must bail without touching state.
            val jobHolder = arrayOfNulls<Job>(1)
            val job = scope.launch {
                delay(DESKTOP_STOPPED_DEBOUNCE_MS)
                synchronized(stateLock) {
                    if (desktopStoppedDebounceJob !== jobHolder[0]) return@synchronized
                    Log.i(TAG, "MediaPlugin: desktop stopped commit (post-debounce)")
                    desktopStoppedDebounceJob = null
                    clearNotification()
                    reclassifyLocked()
                }
            }
            jobHolder[0] = job
            desktopStoppedDebounceJob = job
        }
    }

    /** Start (or keep) the 1Hz extrapolation tick that advances the desktop
     *  playback's position locally between anchor messages. The job is idle
     *  while paused/stopped (cheap — one `continue` per second). */
    private fun ensureDesktopTick() {
        if (desktopTickJob?.isActive == true) return
        val scope = pluginScope ?: return
        desktopTickJob = scope.launch {
            while (true) {
                delay(1_000L)
                val a = desktopAnchor ?: continue
                if (a.state.state != "playing") continue
                val elapsed = SystemClock.elapsedRealtime() - a.anchorElapsedRealtime
                val livePos = (a.state.positionMs + (elapsed * a.speed).toLong()).coerceAtLeast(0)
                _desktopPlayback.value = a.state.copy(positionMs = livePos)
            }
        }
    }

    /** Desktop command → apply to the phone's local media session. */
    private fun handleCommand(json: kotlinx.serialization.json.JsonObject) {
        val ctrl = mediaController ?: return
        val tc = ctrl.transportControls
        val cmd = json["command"]?.jsonPrimitive?.content ?: return

        when (cmd) {
            "play" -> tc.play()
            "pause" -> tc.pause()
            "playpause" -> {
                if (ctrl.playbackState?.state == PlaybackState.STATE_PLAYING) tc.pause() else tc.play()
            }
            "next" -> tc.skipToNext()
            "previous" -> tc.skipToPrevious()
            "stop" -> tc.stop()
            "seek" -> {
                val pos = json["position_ms"]?.jsonPrimitive?.content?.toLongOrNull() ?: return
                tc.seekTo(pos)
            }
        }
        Log.i(TAG, "MediaPlugin: applied desktop command '$cmd' to local player")
    }

    /** Transient holder for the five capability bits we extract from PlaybackState.actions
     *  and embed into MediaPlaybackState / wire messages. */
    private data class Caps(
        val play: Boolean, val pause: Boolean, val next: Boolean,
        val prev: Boolean, val seek: Boolean
    )

    private fun updateSession(
        s: MediaPlaybackState,
        art: Bitmap? = null,
        updateTimeElapsed: Long = SystemClock.elapsedRealtime(),
        speed: Float = 1.0f
    ) {
        val session = this.session ?: return

        val metaBuilder = MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, s.title)
            .putString(MediaMetadata.METADATA_KEY_ARTIST, s.artist)
            .putString(MediaMetadata.METADATA_KEY_ALBUM, s.album)
            .putLong(MediaMetadata.METADATA_KEY_DURATION, s.durationMs)
        if (art != null) {
            // Both keys: ALBUM_ART feeds the lockscreen/system card, ART is the
            // larger generic key some surfaces prefer.
            metaBuilder.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, art)
            metaBuilder.putBitmap(MediaMetadata.METADATA_KEY_ART, art)
        }
        session.setMetadata(metaBuilder.build())

        var actions = 0L
        if (s.canPlay) actions = actions or PlaybackState.ACTION_PLAY
        if (s.canPause) actions = actions or PlaybackState.ACTION_PAUSE
        actions = actions or PlaybackState.ACTION_PLAY_PAUSE
        if (s.canNext) actions = actions or PlaybackState.ACTION_SKIP_TO_NEXT
        if (s.canPrev) actions = actions or PlaybackState.ACTION_SKIP_TO_PREVIOUS
        if (s.canSeek) actions = actions or PlaybackState.ACTION_SEEK_TO

        val playbackState = when (s.state) {
            "playing" -> PlaybackState.STATE_PLAYING
            "paused" -> PlaybackState.STATE_PAUSED
            else -> PlaybackState.STATE_STOPPED
        }

        // For PLAYING, pass the real speed and the anchor time so the lockscreen
        // extrapolates position itself; for PAUSED, speed=0 freezes it.
        val effectiveSpeed = if (playbackState == PlaybackState.STATE_PLAYING) speed else 0f
        session.setPlaybackState(
            PlaybackState.Builder()
                .setActions(actions)
                .setState(playbackState, s.positionMs, effectiveSpeed, updateTimeElapsed)
                .build()
        )
    }

    private fun showNotification(s: MediaPlaybackState, art: Bitmap? = null) {
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val token = session?.sessionToken ?: return

        val subtitle = listOf(s.artist, s.album).filter { it.isNotEmpty() }.joinToString(" — ")
        val playing = s.state == "playing"

        // Tapping the notification opens the Anchor app's Media screen.
        val openIntent = Intent().apply {
            setClassName(context.packageName, "com.anchor.MainActivity")
            putExtra("open_screen", "media")
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val openPi = PendingIntent.getActivity(
            context,
            0,
            openIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val builder = Notification.Builder(context, CHANNEL_ID)
            // Pure-alpha vector — Android renders status/route icons as a
            // silhouette, so a multi-color raster like ic_launcher comes out
            // as a featureless white square.
            .setSmallIcon(R.drawable.ic_anchor)
            .setContentTitle(s.title)
            .setContentText(subtitle)
            .setSubText(s.app.ifEmpty { "Desktop" })
            .setContentIntent(openPi)
            .setOngoing(playing)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
        if (art != null) {
            builder.setLargeIcon(art)
        }

        // Transport buttons, gated by what the desktop player supports. Track
        // which compact-view slots map to the added actions.
        val compactIdx = mutableListOf<Int>()
        var idx = 0
        if (s.canPrev) {
            builder.addAction(action(android.R.drawable.ic_media_previous, "Previous", "previous"))
            compactIdx.add(idx); idx++
        }
        // Play/pause toggle always present.
        if (playing) {
            builder.addAction(action(android.R.drawable.ic_media_pause, "Pause", "pause"))
        } else {
            builder.addAction(action(android.R.drawable.ic_media_play, "Play", "play"))
        }
        compactIdx.add(idx); idx++
        if (s.canNext) {
            builder.addAction(action(android.R.drawable.ic_media_next, "Next", "next"))
            compactIdx.add(idx); idx++
        }

        val mediaStyle = Notification.MediaStyle()
            .setMediaSession(token)
            .setShowActionsInCompactView(*compactIdx.toIntArray())

        builder.style = mediaStyle

        nm.notify(NOTIFICATION_ID, builder.build())
    }

    private fun action(icon: Int, label: String, cmd: String): Notification.Action {
        val intent = Intent(ACTION_CMD).apply {
            setPackage(context.packageName)
            putExtra(EXTRA_CMD, cmd)
        }
        val pi = PendingIntent.getBroadcast(
            context,
            cmd.hashCode(),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Action.Builder(
            android.graphics.drawable.Icon.createWithResource(context, icon), label, pi
        ).build()
    }

    private val sessionCallback = object : MediaSession.Callback() {
        override fun onPlay() = sendCommand("play")
        override fun onPause() = sendCommand("pause")
        override fun onSkipToNext() = sendCommand("next")
        override fun onSkipToPrevious() = sendCommand("previous")
        override fun onSeekTo(pos: Long) = sendCommand("seek", pos)
    }

    private fun sendCommand(command: String, positionMs: Long? = null) {
        val payload = buildJsonObject {
            put("plugin_id", "media")
            put("type", "media_command")
            put("command", command)
            if (currentSessionId.isNotEmpty()) put("target_session", currentSessionId)
            if (positionMs != null) put("position_ms", positionMs)
        }
        sdkCapability?.let { sdk ->
            val kind = when (command) {
                "play" -> 1
                "pause" -> 2
                "next" -> 3
                "previous" -> 4
                "seek" -> 5
                else -> 0
            }
            if (kind != 0) {
                val typed = MediaProtocol.encodeCommand(kind, positionMs ?: 0)
                pluginScope?.launch(Dispatchers.IO) {
                    runCatching { sdk.sendRecord(MediaProtocol.COMMAND_TYPE_URL, typed) }
                        .onFailure { error -> Log.w(TAG, "SDK media command send failed: ${error.message}") }
                }
                Log.i(TAG, "MediaPlugin: sent typed SDK command '$command'")
                return
            }
        }
        broker.send(AnchorEvent(AnchorTarget.Device, AnchorMessage.Json(payload.toString())))
        Log.i(TAG, "MediaPlugin: sent command '$command' to desktop")
    }

    private fun clearNotification() {
        currentSessionId = ""
        _desktopArt.value = null
        _desktopPlayback.value = null
        desktopAnchor = null
        session?.isActive = false
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.cancel(NOTIFICATION_ID)
    }

    /** Drop cached desktop-side playback state. */
    fun clearRemoteState() {
        clearNotification()
    }

    private fun registerCmdReceiver() {
        val filter = IntentFilter(ACTION_CMD)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(cmdReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            context.registerReceiver(cmdReceiver, filter)
        }
    }

    private fun createChannel() {
        val channel = NotificationChannel(
            CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_LOW
        ).apply { description = "Media playing on your desktop" }
        val nm = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(channel)
    }

    override fun stop() {
        listenJob?.cancel()
        listenJob = null
        permissionRetryJob?.cancel()
        permissionRetryJob = null
        localTickJob?.cancel()
        localTickJob = null
        desktopTickJob?.cancel()
        desktopTickJob = null
        synchronized(stateLock) {
            startupGraceJob?.cancel()
            startupGraceJob = null
            desktopStoppedDebounceJob?.cancel()
            desktopStoppedDebounceJob = null
        }
        sessionsListener?.let {
            try {
                mediaSessionManager.removeOnActiveSessionsChangedListener(it)
            } catch (e: Exception) {
                Log.w(TAG, "MediaPlugin: removeOnActiveSessionsChangedListener failed: ${e.message}")
            }
        }
        sessionsListener = null
        try {
            context.unregisterReceiver(cmdReceiver)
        } catch (_: IllegalArgumentException) {
            // Receiver wasn't registered (start() never ran); ignore.
        }
        mediaController?.unregisterCallback(localCallback)
        mediaController = null
        clearNotification()
        session?.release()
        session = null
        Log.i(TAG, "MediaPlugin stopped")
    }

}
