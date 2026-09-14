package com.anchor.plugin

/**
 * Phone-side classification of "what is our local media session actually
 * doing, relative to what the desktop is doing?" Drives whether we forward
 * state to the desktop or stay silent.
 *
 * The problem this solves: when the desktop is playing music via a
 * cast-style protocol (Spotify Connect, Google Cast), the phone's own
 * Spotify (or equivalent) app publishes a `MediaSession` that mirrors the
 * desktop's playback. A naive read of "the phone's active session" would
 * forward that mirror back to the desktop as if the phone owned the
 * playback — the desktop UI would show two identical PHONE / DESKTOP
 * cards, media keys would route to the wrong side, and the phone would
 * briefly appear as an MPRIS mirror on the desktop bus.
 *
 * The classifier is expressed as a pure function of two snapshots + a
 * `startupComplete` flag so it can be unit-tested exhaustively without
 * pulling in Android framework types or coroutines.
 */
internal enum class PhonePlaybackRole {
    /**
     * We just started. We haven't received any state from the desktop yet
     * and haven't waited long enough to conclude "desktop is idle." Stay
     * silent — sending a forward now risks appearing as a phone-owned track
     * that will vanish moments later when we learn the desktop was already
     * playing it (the "shows up for a second then disappears" flap).
     */
    STARTUP,

    /** No local MediaSession, or it's stopped. Nothing to report. */
    IDLE,

    /**
     * Local session is playing something *distinct* from what the desktop
     * has reported. Forward local state to the desktop; this is a genuine
     * phone-owned playback.
     */
    INDEPENDENT,

    /**
     * Local session is a reflection of the desktop's own playback (matched
     * on title + artist + duration within tolerance). Stay silent — the
     * desktop already knows what it's playing, and we'd only echo it back.
     */
    MIRROR,
}

/** Metadata triple used for track identity. */
internal data class TrackId(
    val title: String,
    val artist: String,
    val durationMs: Long,
)

/**
 * Duration tolerance for treating two tracks as "the same." Spotify Connect
 * and similar cast protocols round durations slightly differently on each
 * client — observed drift is 100–300 ms for a track both sides agree on.
 * 1000 ms comfortably absorbs that jitter while staying well below the
 * duration gap between different masters of the same song (typically 2–10
 * seconds), which we still want to treat as distinct.
 */
internal const val TRACK_MATCH_DURATION_TOLERANCE_MS: Long = 1_000L

internal fun tracksMatch(a: TrackId, b: TrackId): Boolean {
    if (a.title.isEmpty() || b.title.isEmpty()) return false
    return a.title == b.title &&
        a.artist == b.artist &&
        kotlin.math.abs(a.durationMs - b.durationMs) <= TRACK_MATCH_DURATION_TOLERANCE_MS
}

/**
 * Pure classifier. Given the phone's current local session state, the last
 * known desktop state, and whether the startup grace period has elapsed,
 * return which role the phone is in.
 *
 * @param localTrack the phone's local MediaSession as a track, or `null` if
 *   no session is active / the session is stopped
 * @param desktopSnapshot the desktop's last reported playback, or `null` if
 *   the desktop reported stopped or hasn't reported yet
 * @param startupComplete true once we've either (a) received at least one
 *   desktop state or (b) waited past the startup grace period
 */
internal fun classifyPhoneRole(
    localTrack: TrackId?,
    desktopSnapshot: MediaPlaybackState?,
    startupComplete: Boolean,
): PhonePlaybackRole {
    if (!startupComplete) return PhonePlaybackRole.STARTUP
    if (localTrack == null) return PhonePlaybackRole.IDLE

    val desktopActive =
        desktopSnapshot != null &&
            desktopSnapshot.title.isNotEmpty() &&
            (desktopSnapshot.state == "playing" || desktopSnapshot.state == "paused")

    if (!desktopActive) return PhonePlaybackRole.INDEPENDENT

    val desktopTrack = TrackId(
        title = desktopSnapshot!!.title,
        artist = desktopSnapshot.artist,
        durationMs = desktopSnapshot.durationMs,
    )
    return if (tracksMatch(localTrack, desktopTrack)) {
        PhonePlaybackRole.MIRROR
    } else {
        PhonePlaybackRole.INDEPENDENT
    }
}
