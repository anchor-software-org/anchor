package com.anchor

import com.anchor.plugin.MediaPlaybackState
import com.anchor.plugin.PhonePlaybackRole
import com.anchor.plugin.TrackId
import com.anchor.plugin.classifyPhoneRole
import com.anchor.plugin.tracksMatch
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Regression tests for the phone-side playback classifier.
 *
 * The scenario we care about: the user plays music on the desktop with a
 * cast-style protocol (Spotify Connect, Google Cast). Those protocols publish
 * a `MediaSession` on the phone under the app's own package (e.g.
 * `com.spotify.music`) that mirrors the desktop's playback. Without a
 * classifier, anchor's phone plugin would read that session as "the phone is
 * playing X" and forward it to the desktop — where it would appear as a
 * phantom phone MPRIS mirror. The user asked us to only forward genuine
 * phone-owned playback and stay silent while the phone is just reflecting
 * the desktop.
 *
 * Every test runs the classifier as a pure function of (localTrack,
 * desktopSnapshot, startupComplete), so we can enumerate the state-machine
 * transition inputs without spinning up Android framework types.
 */
class MediaEchoSuppressionTest {

    private fun desktopPlaying(
        title: String = "Miraí no Tēma",
        artist: String = "Crimson Craftsman",
        durationMs: Long = 265_000L,
        state: String = "playing"
    ) = MediaPlaybackState(
        source = "desktop",
        title = title,
        artist = artist,
        album = "",
        app = "spotify",
        state = state,
        positionMs = 12_000L,
        durationMs = durationMs,
        canPlay = true,
        canPause = true,
        canNext = true,
        canPrev = true,
        canSeek = true
    )

    private fun phoneTrack(
        title: String = "Miraí no Tēma",
        artist: String = "Crimson Craftsman",
        durationMs: Long = 265_000L
    ) = TrackId(title = title, artist = artist, durationMs = durationMs)

    // ---- tracksMatch: identity primitive ----

    /** The exact case from the user report: desktop plays via Spotify, phone's
     *  Spotify app has the same session via Connect. Same track, artist,
     *  duration → match. */
    @Test
    fun matchingTrackArtistAndDurationMatches() {
        assertTrue(tracksMatch(phoneTrack(), phoneTrack()))
    }

    /** Two different tracks — user's stated intent is to forward this. */
    @Test
    fun differentTitleDoesNotMatch() {
        assertFalse(
            tracksMatch(
                phoneTrack(title = "A Different Phone Song"),
                phoneTrack(title = "Some Desktop Song")
            )
        )
    }

    /** Same title but different artist: probably a cover / remake. Not a match. */
    @Test
    fun differentArtistDoesNotMatch() {
        assertFalse(
            tracksMatch(
                phoneTrack(artist = "Cover Artist"),
                phoneTrack(artist = "Original Artist")
            )
        )
    }

    /** Duration is the tiebreaker: same title + artist but a different master
     *  (single vs album vs live version) → seconds apart in duration, treat
     *  as no match so the user hears about it. */
    @Test
    fun differentDurationDoesNotMatch() {
        assertFalse(
            tracksMatch(
                phoneTrack(durationMs = 265_000L),
                phoneTrack(durationMs = 270_000L)
            )
        )
    }

    /** Spotify Connect and similar protocols round durations slightly
     *  differently on each client — observed drift is 100–300 ms for a track
     *  both sides agree on. Sub-second differences must still match, otherwise
     *  the fix silently regresses whenever the two clients disagree on rounding. */
    @Test
    fun subSecondDurationDriftStillMatches() {
        assertTrue(
            "duration 209000 vs 209200 (Spotify Connect drift) should still match",
            tracksMatch(
                phoneTrack(durationMs = 209_000L),
                phoneTrack(durationMs = 209_200L)
            )
        )
    }

    /** Empty title on either side is never a match — we don't have identity. */
    @Test
    fun emptyTitleNeverMatches() {
        assertFalse(tracksMatch(phoneTrack(title = ""), phoneTrack()))
        assertFalse(tracksMatch(phoneTrack(), phoneTrack(title = "")))
    }

    // ---- classifyPhoneRole: the state-machine input function ----

    /** Before we've received any desktop state and before the grace timer
     *  fires, we must stay in STARTUP so we don't publish a phone track that
     *  the incoming desktop state will immediately reclassify to MIRROR
     *  (the "shows up for a second then disappears" flap). */
    @Test
    fun preStartupStaysSilent() {
        assertEquals(
            PhonePlaybackRole.STARTUP,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = desktopPlaying(),
                startupComplete = false
            )
        )
    }

    /** No local session — nothing to publish. */
    @Test
    fun noLocalSessionIsIdle() {
        assertEquals(
            PhonePlaybackRole.IDLE,
            classifyPhoneRole(
                localTrack = null,
                desktopSnapshot = desktopPlaying(),
                startupComplete = true
            )
        )
    }

    /** Local session with no desktop playback — this is genuine phone-owned. */
    @Test
    fun localSessionNoDesktopIsIndependent() {
        assertEquals(
            PhonePlaybackRole.INDEPENDENT,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = null,
                startupComplete = true
            )
        )
    }

    /** Desktop reported stopped: the phone playing "the same track" is the
     *  phone continuing on its own after the desktop dropped out. Forward it. */
    @Test
    fun stoppedDesktopIsIndependent() {
        assertEquals(
            PhonePlaybackRole.INDEPENDENT,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = desktopPlaying(state = "stopped"),
                startupComplete = true
            )
        )
    }

    /** Empty title on the desktop side (never received real state) — not an
     *  active desktop for classification purposes. Local is independent. */
    @Test
    fun emptyDesktopTitleIsIndependent() {
        assertEquals(
            PhonePlaybackRole.INDEPENDENT,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = desktopPlaying(title = ""),
                startupComplete = true
            )
        )
    }

    /** The core echo case. Desktop plays via Spotify Connect, phone's Spotify
     *  session mirrors it, same track → MIRROR (stay silent). */
    @Test
    fun matchingLocalAndPlayingDesktopIsMirror() {
        assertEquals(
            PhonePlaybackRole.MIRROR,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = desktopPlaying(),
                startupComplete = true
            )
        )
    }

    /** Paused desktop still owns the playback session — the phone's matching
     *  paused Connect session is a reflection. MIRROR. */
    @Test
    fun matchingLocalAndPausedDesktopIsMirror() {
        assertEquals(
            PhonePlaybackRole.MIRROR,
            classifyPhoneRole(
                localTrack = phoneTrack(),
                desktopSnapshot = desktopPlaying(state = "paused"),
                startupComplete = true
            )
        )
    }

    /** Two genuinely distinct tracks playing at the same time — the user
     *  wants both surfaces. INDEPENDENT. */
    @Test
    fun distinctTracksAreIndependent() {
        assertEquals(
            PhonePlaybackRole.INDEPENDENT,
            classifyPhoneRole(
                localTrack = phoneTrack(title = "Phone-Only Song"),
                desktopSnapshot = desktopPlaying(title = "Desktop-Only Song"),
                startupComplete = true
            )
        )
    }

    /** Spotify Connect duration drift while otherwise identical — still MIRROR,
     *  not INDEPENDENT. Regression guard: without the ±1s tolerance, the
     *  classifier would incorrectly forward this as phone-owned. */
    @Test
    fun mirrorSurvivesConnectDurationDrift() {
        assertEquals(
            PhonePlaybackRole.MIRROR,
            classifyPhoneRole(
                localTrack = phoneTrack(durationMs = 209_000L),
                desktopSnapshot = desktopPlaying(durationMs = 209_200L),
                startupComplete = true
            )
        )
    }
}
