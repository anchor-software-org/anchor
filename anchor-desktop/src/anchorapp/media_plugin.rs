//! Media playback control plugin (`plugin_id: "media"`).
//!
//! Bidirectional bridge between desktop and phone media players:
//!
//!   * **desktop → phone**: poll the active local MPRIS player and forward its
//!     now-playing state as a `media_state` message (`source: "desktop"`). The
//!     phone renders this as a MediaStyle notification with transport controls.
//!   * **phone → desktop**: receive `media_state` (`source: "android"`), expose
//!     it as a standard MPRIS player via `souvlaki` so KDE/GNOME's media widget,
//!     `playerctl`, and hardware media keys can control phone playback. Also
//!     store it in `remote_state` for the desktop frontend.
//!
//! ## Wire protocol (shared with the Android `MediaPlugin`)
//!
//! `media_state` — a side reporting *its own* now-playing:
//! ```json
//! {
//!   "plugin_id": "media", "type": "media_state", "source": "desktop"|"android",
//!   "session_id": "...", "state": "playing"|"paused"|"stopped",
//!   "title": "...", "artist": "...", "album": "...", "app": "...",
//!   "position_ms": 12345, "duration_ms": 200000,
//!   "can_play": true, "can_pause": true, "can_next": true,
//!   "can_prev": true, "can_seek": true, "art_url": "...", "ts": 1690000000
//! }
//! ```
//!
//! `media_command` — a side controlling the *other's* session:
//! ```json
//! {
//!   "plugin_id": "media", "type": "media_command",
//!   "target_session": "...", "command": "playpause"|"play"|"pause"|"next"|"previous"|"seek"|"stop",
//!   "position_ms": 45000
//! }
//! ```

use std::fs;
use std::io::Read;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::codecs::jpeg::JpegEncoder;
use mpris::{PlaybackStatus, Player, PlayerFinder};
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::json;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorapp::sdk_media::SdkMediaBinding;

/// How often we poll the active local player for state changes.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);
const MAX_ARTWORK_INPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ARTWORK_JPEG_BYTES: usize = 256 * 1024;

/// Bus suffix of the souvlaki MPRIS player we register to mirror the phone.
/// Must match `PlatformConfig::dbus_name` in [`run_souvlaki`]. We filter it
/// out of the local-player poll so we don't echo the phone's own state back
/// to it as if it were a desktop player.
const MIRROR_DBUS_SUFFIX: &str = "anchor_phone_media";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Desktop,
    Android,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Command {
    Play,
    Pause,
    Playpause,
    Next,
    Previous,
    Seek,
    Stop,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaState {
    pub source: Source,
    pub session_id: String,
    pub state: PlaybackState,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_seek: bool,
    #[serde(default)]
    pub art_url: String,
    /// Playback rate at the time of the snapshot. The receiver multiplies by
    /// this when extrapolating position locally, so a podcast played at 1.5×
    /// ticks at the right speed between anchor messages.
    #[serde(default = "default_playback_speed")]
    pub playback_speed: f32,
    #[serde(default)]
    pub ts: u64,
}

fn default_playback_speed() -> f32 {
    1.0
}

impl MediaState {
    fn stopped(source: Source) -> Self {
        MediaState {
            source,
            session_id: String::new(),
            state: PlaybackState::Stopped,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            app: String::new(),
            position_ms: 0,
            duration_ms: 0,
            can_play: false,
            can_pause: false,
            can_next: false,
            can_prev: false,
            can_seek: false,
            art_url: String::new(),
            playback_speed: 1.0,
            ts: now_secs(),
        }
    }

    /// Stable identity of the *track + transport*, ignoring position. Used both
    /// by the poller (to detect when an anchor resend is needed) and souvlaki
    /// (to avoid re-publishing metadata that didn't change).
    fn track_fingerprint(&self) -> String {
        format!(
            "{}|{:?}|{}|{}|{}|{}",
            self.session_id,
            self.state,
            self.title,
            self.artist,
            self.duration_ms,
            // Speed bucket: changing playback rate must trigger a resend so
            // the phone re-anchors its 1Hz extrapolation tick.
            (self.playback_speed * 100.0) as i32,
        )
    }

    fn is_effectively_stopped(&self) -> bool {
        self.state == PlaybackState::Stopped || self.title.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaCommand {
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaMessage {
    MediaState(MediaState),
    MediaCommand(MediaCommand),
}

/// Serialize a `MediaMessage` to wire JSON with the `plugin_id: "media"` envelope.
fn to_wire(msg: &MediaMessage) -> String {
    let mut v = serde_json::to_value(msg).expect("MediaMessage serializes");
    if let Some(obj) = v.as_object_mut() {
        obj.insert("plugin_id".to_string(), json!("media"));
    }
    v.to_string()
}

pub(crate) fn send_media_message(
    broker_tx: &Sender<AnchorEvent>,
    target: AnchorTarget,
    msg: &MediaMessage,
) {
    broker_tx.send(AnchorEvent { target, message: AnchorMessage::Json(to_wire(msg)) }).ok();
}

/// Send a media message to every peer that negotiated the typed SDK capability.
/// There is no control-channel fallback: all peers use the same QUIC protocol.
fn send_media_to_device(sdk_binding: &SdkMediaBinding, msg: &MediaMessage) {
    for (device_id, sender) in sdk_binding.senders() {
        let accepted = match msg {
            MediaMessage::MediaState(state) => sender.send_state(state.clone()),
            MediaMessage::MediaCommand(command) => sender.send_command(command.clone()),
        };
        if !accepted {
            log::debug!("media: SDK writer queue full for {device_id}; dropping update");
        }
    }
}

/// Now-playing snapshot shared with the GUI and souvlaki.
pub type SharedMediaState = Arc<Mutex<Option<MediaState>>>;

pub struct MediaPlugin {
    pub plugin_rx: Option<Receiver<AnchorEvent>>,
    pub broker_tx: Option<Sender<AnchorEvent>>,
    /// The phone's current media state (for the GUI and souvlaki).
    pub remote_state: SharedMediaState,
    /// The desktop's own current MPRIS state (for the GUI).
    pub desktop_state: SharedMediaState,
    /// Last anchor sent to the phone. Shared so the receiver thread can clear
    /// it on `device_disconnected`, guaranteeing a fresh anchor on reconnect.
    pub(crate) last_sent: SharedLastSent,
    /// Typed QUIC media capability binding.
    sdk_binding: SdkMediaBinding,
}

impl MediaPlugin {
    pub fn sdk_binding(&self) -> SdkMediaBinding {
        self.sdk_binding.clone()
    }
}

impl Plugin for MediaPlugin {
    fn run(&mut self) -> Option<JoinHandle<()>> {
        let rx = self.plugin_rx.take().expect("media plugin_rx not set");
        let remote_state_recv = Arc::clone(&self.remote_state);
        let remote_state_sou = Arc::clone(&self.remote_state);
        let desktop_state = Arc::clone(&self.desktop_state);
        let last_sent_recv = Arc::clone(&self.last_sent);
        let last_sent_poll = Arc::clone(&self.last_sent);
        let broker_tx_recv = self.broker_tx.clone();
        let broker_tx_poll = self.broker_tx.clone();
        let sdk_binding_recv = self.sdk_binding.clone();
        let sdk_binding_poll = self.sdk_binding.clone();
        let sdk_binding_sou = self.sdk_binding.clone();

        let handle = thread::Builder::new()
            .name("media-receiver".to_string())
            .spawn(move || {
                log::debug!("Media receiver started");
                for event in rx.iter() {
                    if let AnchorMessage::Json(payload) = &event.message {
                        handle_incoming(
                            payload,
                            &remote_state_recv,
                            &last_sent_recv,
                            &sdk_binding_recv,
                            broker_tx_recv.as_ref(),
                        );
                    }
                }
                log::info!("Media receiver exited");
            })
            .expect("Failed to spawn media receiver thread");

        thread::Builder::new()
            .name("media-mpris-poller".to_string())
            .spawn(move || {
                log::debug!("Media MPRIS poller starting");
                poll_local_players(
                    desktop_state,
                    last_sent_poll,
                    sdk_binding_poll,
                    broker_tx_poll.as_ref(),
                );
            })
            .expect("Failed to spawn media MPRIS poller thread");

        thread::Builder::new()
            .name("media-souvlaki".to_string())
            .spawn(move || {
                log::debug!("Media souvlaki (phone MPRIS exposure) starting");
                run_souvlaki(remote_state_sou, sdk_binding_sou);
            })
            .expect("Failed to spawn media souvlaki thread");

        Some(handle)
    }

    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        MediaPlugin {
            plugin_rx,
            broker_tx,
            remote_state: Arc::new(Mutex::new(None)),
            desktop_state: Arc::new(Mutex::new(None)),
            last_sent: Arc::new(Mutex::new(None)),
            sdk_binding: SdkMediaBinding::default(),
        }
    }
}

/// Last anchor we sent to the phone — the phone extrapolates position locally
/// using this base + elapsed time × speed, so we only need to resend on
/// track/transport changes, seek (drift), or a periodic heartbeat.
///
/// Wrapped in `SharedLastSent` so the receiver thread can reset it when a
/// device disconnects: on the next reconnect the poller sees `None` and
/// re-anchors from scratch instead of silently skipping the resend because
/// the (now-disconnected) phone "already had" the current track.
#[derive(Clone)]
pub(crate) struct LastSent {
    track_fp: String,
    position_ms: u64,
    sent_at: Instant,
    speed: f32,
}

pub(crate) type SharedLastSent = Arc<Mutex<Option<LastSent>>>;

/// Max drift (ms) between expected and actual position before we treat it as a
/// seek and re-anchor. ~1.5s covers normal jitter without missing real seeks.
const SEEK_DRIFT_MS: i64 = 1500;
/// Heartbeat interval so a phone that connected after the last change still
/// gets the current state. Kept long because the extrapolation tick keeps the
/// UI live in between.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// Poll loop: every tick, read the active local MPRIS player. Always refresh
/// the shared `desktop_state` (GUI reads from it) but only send to the phone
/// when the track/transport changed, the user seeked, or a heartbeat is due.
fn poll_local_players(
    desktop_state: SharedMediaState,
    last_sent: SharedLastSent,
    sdk_binding: SdkMediaBinding,
    broker_tx: Option<&Sender<AnchorEvent>>,
) {
    let mut artwork_cache = ArtworkCache::new();
    loop {
        let finder = match PlayerFinder::new() {
            Ok(f) => f,
            Err(e) => {
                log::warn!("media: cannot connect to D-Bus, retrying: {e}");
                thread::sleep(POLL_INTERVAL);
                continue;
            }
        };

        // Previous snapshot used to repair transient MPRIS metadata.
        let mut previous_snapshot: Option<MediaState> = None;

        loop {
            let raw = match pick_active_player(&finder) {
                Ok(Some(p)) => {
                    player_to_state(&p).unwrap_or_else(|| MediaState::stopped(Source::Desktop))
                }
                Ok(None) => MediaState::stopped(Source::Desktop),
                Err(e) => {
                    // Transient D-Bus error — commonly happens right after we
                    // tear down our own souvlaki mirror in response to a phone
                    // `stopped` state: the enumeration races the teardown and
                    // `find_all` briefly fails. If we translated that to
                    // `MediaState::stopped(Desktop)` we'd send it to the phone,
                    // the phone would clear its `desktopAnchor`, and its echo
                    // guard would then fail to fire → the mirror flaps back
                    // up on the next tick and we ping-pong forever.
                    // Skip the tick instead — the next iteration reads fresh.
                    log::debug!("media: transient DBus enumerate error, holding last state: {e}");
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
            };

            let mut state = patch_transient_zero_duration(raw, previous_snapshot.as_ref());
            artwork_cache.resolve(&mut state);
            if !state.is_effectively_stopped() {
                previous_snapshot = Some(state.clone());
            }

            if let Ok(mut guard) = desktop_state.lock() {
                *guard = if state.is_effectively_stopped() { None } else { Some(state.clone()) };
            }

            let prev = last_sent.lock().ok().and_then(|g| g.clone());
            if should_send(&state, prev.as_ref()) {
                if let Ok(mut guard) = last_sent.lock() {
                    *guard = Some(LastSent {
                        track_fp: state.track_fingerprint(),
                        position_ms: state.position_ms,
                        sent_at: Instant::now(),
                        speed: state.playback_speed,
                    });
                }
                send_media_to_device(&sdk_binding, &MediaMessage::MediaState(state));
                notify_media_ui(broker_tx);
            }

            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn notify_media_ui(broker_tx: Option<&Sender<AnchorEvent>>) {
    let Some(broker_tx) = broker_tx else {
        return;
    };
    let _ = broker_tx.send(AnchorEvent {
        target: AnchorTarget::Gui,
        message: AnchorMessage::Json(r#"{"plugin_id":"media","type":"media_state"}"#.into()),
    });
}

struct ArtworkCache {
    source: String,
    data_url: String,
    client: Option<Client>,
}

impl ArtworkCache {
    fn new() -> Self {
        Self {
            source: String::new(),
            data_url: String::new(),
            client: Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(5))
                .redirect(Policy::none())
                .build()
                .ok(),
        }
    }

    fn resolve(&mut self, state: &mut MediaState) {
        if state.art_url == self.source {
            state.art_url.clone_from(&self.data_url);
            return;
        }
        self.source.clone_from(&state.art_url);
        self.data_url = load_artwork_jpeg(&state.art_url, self.client.as_ref())
            .map(|jpeg| format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg)))
            .unwrap_or_default();
        state.art_url.clone_from(&self.data_url);
    }
}

fn load_artwork_jpeg(source: &str, client: Option<&Client>) -> Option<Vec<u8>> {
    let url = url::Url::parse(source).ok()?;
    let bytes = match url.scheme() {
        "file" => {
            let path = url.to_file_path().ok()?;
            let metadata = fs::metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > MAX_ARTWORK_INPUT_BYTES {
                return None;
            }
            let bytes = fs::read(path).ok()?;
            if bytes.len() > MAX_ARTWORK_INPUT_BYTES as usize {
                return None;
            }
            bytes
        }
        "https" => {
            let response = client?.get(source).send().ok()?;
            if !response.status().is_success()
                || response.content_length().is_some_and(|size| size > MAX_ARTWORK_INPUT_BYTES)
            {
                return None;
            }
            let mut bytes = Vec::new();
            response.take(MAX_ARTWORK_INPUT_BYTES + 1).read_to_end(&mut bytes).ok()?;
            if bytes.len() > MAX_ARTWORK_INPUT_BYTES as usize {
                return None;
            }
            bytes
        }
        _ => return None,
    };
    let image = image::load_from_memory(&bytes).ok()?.thumbnail(512, 512).to_rgb8();
    for quality in [85, 70, 55, 40] {
        let mut jpeg = Vec::new();
        if JpegEncoder::new_with_quality(&mut jpeg, quality).encode_image(&image).is_ok()
            && jpeg.len() <= MAX_ARTWORK_JPEG_BYTES
        {
            return Some(jpeg);
        }
    }
    None
}

/// Preserve the previous duration when the source briefly reports zero for
/// the same track.
fn patch_transient_zero_duration(state: MediaState, previous: Option<&MediaState>) -> MediaState {
    if state.duration_ms > 0 {
        return state;
    }
    match previous {
        Some(prev)
            if prev.session_id == state.session_id
                && prev.title == state.title
                && !state.title.is_empty()
                && prev.duration_ms > 0 =>
        {
            MediaState { duration_ms: prev.duration_ms, ..state }
        }
        _ => state,
    }
}

/// Decide whether the current snapshot warrants a fresh anchor message.
fn should_send(state: &MediaState, last: Option<&LastSent>) -> bool {
    let Some(last) = last else {
        return true;
    };
    if last.track_fp != state.track_fingerprint() {
        return true;
    }
    if last.sent_at.elapsed() >= HEARTBEAT {
        return true;
    }
    // Drift check: only meaningful while playing. When paused, position is
    // fixed and any change is reflected in the (state-bearing) track_fp above.
    if state.state == PlaybackState::Playing {
        let elapsed_ms = last.sent_at.elapsed().as_millis() as i64;
        let expected = last.position_ms as i64 + (elapsed_ms as f32 * last.speed) as i64;
        if (state.position_ms as i64 - expected).abs() > SEEK_DRIFT_MS {
            return true;
        }
    }
    false
}

/// Pick the most relevant local MPRIS player, excluding our own phone mirror.
///
/// `mpris::PlayerFinder::find_active()` can pick the souvlaki mirror we
/// register for the phone — which would loop the phone's own state back to it
/// as if it were a desktop player (no art_url, frozen position). Walk the full
/// list, drop the mirror, prefer a playing player, then a paused one.
///
/// Returns:
///   * `Ok(Some(p))` — an eligible player was found (playing preferred).
///   * `Ok(None)` — the D-Bus enumeration succeeded but no non-mirror player
///     is currently playing or paused. Real "nothing is playing" state.
///   * `Err(_)` — the enumeration itself failed. The caller should treat this
///     as "unknown" and hold its previous state; downgrading to `stopped`
///     here creates a flap when souvlaki is mid-teardown (see poll loop).
fn pick_active_player(finder: &PlayerFinder) -> Result<Option<Player>, String> {
    let players = finder.find_all().map_err(|e| e.to_string())?;
    let mut paused_fallback: Option<Player> = None;
    for p in players {
        if p.bus_name().contains(MIRROR_DBUS_SUFFIX) {
            continue;
        }
        match p.get_playback_status() {
            Ok(PlaybackStatus::Playing) => return Ok(Some(p)),
            Ok(PlaybackStatus::Paused) if paused_fallback.is_none() => {
                paused_fallback = Some(p);
            }
            _ => {}
        }
    }
    Ok(paused_fallback)
}

fn player_to_state(player: &Player) -> Option<MediaState> {
    let status = player.get_playback_status().ok()?;
    let state = match status {
        PlaybackStatus::Playing => PlaybackState::Playing,
        PlaybackStatus::Paused => PlaybackState::Paused,
        PlaybackStatus::Stopped => PlaybackState::Stopped,
    };

    let metadata = player.get_metadata().ok()?;
    let raw_length = metadata.length();
    let duration_ms = raw_length.map(|d| d.as_millis() as u64).unwrap_or(0);
    let title = metadata.title().unwrap_or("").to_string();
    log::debug!(
        "duration-probe: raw MPRIS length={:?} (parsed_ms={}) title='{}' app='{}'",
        raw_length,
        duration_ms,
        title,
        player.identity(),
    );
    Some(MediaState {
        source: Source::Desktop,
        session_id: player.bus_name().to_string(),
        state,
        title,
        artist: metadata.artists().map(|a| a.join(", ")).unwrap_or_default(),
        album: metadata.album_name().unwrap_or("").to_string(),
        app: player.identity().to_string(),
        position_ms: player.get_position().map(|d| d.as_millis() as u64).unwrap_or(0),
        duration_ms,
        can_play: player.can_play().unwrap_or(false),
        can_pause: player.can_pause().unwrap_or(false),
        can_next: player.can_go_next().unwrap_or(false),
        can_prev: player.can_go_previous().unwrap_or(false),
        can_seek: player.can_seek().unwrap_or(false),
        art_url: metadata.art_url().map(|u| u.to_string()).unwrap_or_default(),
        playback_speed: player.get_playback_rate().map(|r| r as f32).unwrap_or(1.0),
        ts: now_secs(),
    })
}

/// MPRIS controls plus the owned strings the borrowed `MediaMetadata` points
/// into. Holding the strings here means each track change reuses the same
/// allocations instead of leaking the previous track's metadata.
struct SouvlakiPlayer {
    controls: MediaControls,
    title: String,
    artist: String,
    album: String,
}

/// Run a souvlaki MPRIS player that mirrors the phone's playback state.
///
/// The mirror is only registered on D-Bus when we want the OS's media-key
/// dispatcher (GNOME/KDE Plasma/waybar/playerctl/…) to route to the phone.
/// See [`should_expose_phone_mirror`] for the exact policy — the short
/// version is: when the phone is paused *and* the desktop has a locally
/// playing MPRIS player, we drop the mirror so the media keys hit the
/// desktop player. When the phone is paused and nothing local is playing,
/// the mirror stays so `play` resumes the phone.
fn run_souvlaki(remote_state: SharedMediaState, sdk_binding: SdkMediaBinding) {
    let mut player: Option<SouvlakiPlayer> = None;
    let mut last_fingerprint = String::new();
    let finder = PlayerFinder::new().ok();

    loop {
        thread::sleep(Duration::from_millis(500));

        let state = match remote_state.lock() {
            Ok(g) => g.clone(),
            Err(_) => break,
        };

        let desktop_playing =
            finder.as_ref().map(desktop_has_playing_local_player).unwrap_or(false);
        let expose = should_expose_phone_mirror(state.as_ref(), desktop_playing);

        if !expose {
            if player.take().is_some() {
                last_fingerprint.clear();
                log::info!(
                    "media: souvlaki MPRIS mirror dropped (phone idle or yielding to desktop)"
                );
            }
            continue;
        }

        // `expose = true` requires `state = Some(_)` — see should_expose_phone_mirror.
        let Some(s) = state.as_ref() else { continue };
        let fingerprint = s.track_fingerprint();
        if fingerprint == last_fingerprint && player.is_some() {
            continue;
        }
        last_fingerprint = fingerprint;

        if player.is_none() {
            player = create_souvlaki_player(&sdk_binding);
        }

        if let Some(ref mut p) = player {
            p.title.clone_from(&s.title);
            p.artist.clone_from(&s.artist);
            p.album.clone_from(&s.album);
            let metadata = MediaMetadata {
                title: (!p.title.is_empty()).then_some(p.title.as_str()),
                artist: (!p.artist.is_empty()).then_some(p.artist.as_str()),
                album: (!p.album.is_empty()).then_some(p.album.as_str()),
                duration: Some(Duration::from_millis(s.duration_ms)),
                ..Default::default()
            };
            if let Err(e) = p.controls.set_metadata(metadata) {
                log::warn!("media: souvlaki set_metadata failed: {e}");
            }
            if let Err(e) = p.controls.set_playback(state_to_souvlaki_playback(s)) {
                log::warn!("media: souvlaki set_playback failed: {e}");
            }
        }
    }
}

/// Decide whether the "Phone (Anchor)" MPRIS mirror should be registered on
/// D-Bus this tick.
///
/// The OS session manager (mutter/plasmashell/waybar mediaplayer applet/…)
/// picks *which* MPRIS player receives a media-key event — usually
/// "whichever player was most recently active." When both the phone mirror
/// and a real desktop player are registered, that pick can send `Next` to
/// the phone when the user meant the desktop. Dropping the mirror at the
/// right time is our only lever over the routing:
///
/// * **Phone playing** → expose. The phone is the active track; media keys
///   should hit it.
/// * **Phone paused, desktop has a playing local player** → *don't* expose.
///   The user is actively playing something local; keep media keys on that
///   player.
/// * **Phone paused, nothing playing locally** → expose. Media keys should
///   still be able to resume the phone.
/// * **Phone stopped / no state** → don't expose. Nothing to control.
fn should_expose_phone_mirror(
    phone_state: Option<&MediaState>,
    desktop_has_playing_local_player: bool,
) -> bool {
    match phone_state {
        None => false,
        Some(s) => match s.state {
            PlaybackState::Playing => true,
            PlaybackState::Paused => !desktop_has_playing_local_player,
            PlaybackState::Stopped => false,
        },
    }
}

/// True when any non-mirror MPRIS player on the local D-Bus is currently
/// in `Playing` state. Used by [`should_expose_phone_mirror`] to yield the
/// mirror when the user has audio going locally. `find_all` failures are
/// treated as "no local playing player" — worst case the mirror stays
/// registered a beat longer than it should.
fn desktop_has_playing_local_player(finder: &PlayerFinder) -> bool {
    let players = match finder.find_all() {
        Ok(p) => p,
        Err(_) => return false,
    };
    for p in players {
        if p.bus_name().contains(MIRROR_DBUS_SUFFIX) {
            continue;
        }
        if matches!(p.get_playback_status(), Ok(PlaybackStatus::Playing)) {
            return true;
        }
    }
    false
}

fn create_souvlaki_player(sdk_binding: &SdkMediaBinding) -> Option<SouvlakiPlayer> {
    let config = PlatformConfig {
        dbus_name: MIRROR_DBUS_SUFFIX,
        display_name: "Phone (Anchor)",
        hwnd: None,
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("media: souvlaki init failed: {e}");
            return None;
        }
    };
    let sdk = sdk_binding.clone();
    if let Err(e) = controls.attach(move |event: MediaControlEvent| {
        if let Some(cmd) = souvlaki_event_to_command(event) {
            send_media_to_device(
                &sdk,
                &MediaMessage::MediaCommand(MediaCommand {
                    command: cmd,
                    target_session: None,
                    position_ms: None,
                }),
            );
        }
    }) {
        log::warn!("media: souvlaki attach failed: {e}");
        return None;
    }
    log::info!("media: souvlaki MPRIS player for phone created");
    Some(SouvlakiPlayer {
        controls,
        title: String::new(),
        artist: String::new(),
        album: String::new(),
    })
}

fn souvlaki_event_to_command(event: MediaControlEvent) -> Option<Command> {
    match event {
        MediaControlEvent::Play => Some(Command::Play),
        MediaControlEvent::Pause => Some(Command::Pause),
        MediaControlEvent::Toggle => Some(Command::Playpause),
        MediaControlEvent::Next => Some(Command::Next),
        MediaControlEvent::Previous => Some(Command::Previous),
        MediaControlEvent::Stop => Some(Command::Stop),
        _ => None,
    }
}

fn state_to_souvlaki_playback(state: &MediaState) -> MediaPlayback {
    let position = Some(MediaPosition(Duration::from_millis(state.position_ms)));
    match state.state {
        PlaybackState::Playing => MediaPlayback::Playing { progress: position },
        PlaybackState::Paused => MediaPlayback::Paused { progress: position },
        PlaybackState::Stopped => MediaPlayback::Stopped,
    }
}

/// Tear down per-device media state when the control link drops.
///
/// * clears `remote_state` — the souvlaki loop sees `None` on its next tick
///   and drops the phone-mirror MPRIS player, so system media keys stop
///   routing to a device that isn't there
/// * clears `last_sent` — on reconnect the poller has no cached anchor to
///   diff against, so it re-sends the current desktop `MediaState` even if
///   the track fingerprint hasn't changed (the reconnected phone missed
///   whatever we sent before the drop)
fn handle_device_disconnected(remote_state: &SharedMediaState, last_sent: &SharedLastSent) {
    if let Ok(mut g) = remote_state.lock() {
        *g = None;
    }
    if let Ok(mut g) = last_sent.lock() {
        *g = None;
    }
    log::info!("media: cleared per-device state on disconnect");
}

fn handle_incoming(
    payload: &str,
    remote_state: &SharedMediaState,
    last_sent: &SharedLastSent,
    sdk_binding: &SdkMediaBinding,
    broker_tx: Option<&Sender<AnchorEvent>>,
) {
    // Non-media broadcasts (device_connected/disconnected) share the same
    // channel because the broker fans Broadcast events out to every service.
    // Peek at `type` first so we act on disconnect and don't warn on either.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        match v.get("type").and_then(|t| t.as_str()) {
            Some("device_disconnected") => {
                handle_device_disconnected(remote_state, last_sent);
                return;
            }
            Some("device_connected") => return,
            // The broker fans typed capability control records (for example
            // the screen stream's `stream_info`) to every service. They are
            // valid JSON, but not media messages; ignore them before the
            // MediaMessage enum can report a misleading parse warning.
            Some("media_state" | "media_command") => {}
            Some(_) => return,
            _ => {}
        }
    }

    let msg: MediaMessage = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("media: bad incoming JSON: {e}");
            return;
        }
    };

    match msg {
        MediaMessage::MediaState(state) => {
            // Desktop-source states are produced by the poller thread on this
            // side; we only consume phone state here.
            if state.source == Source::Android
                && let Ok(mut guard) = remote_state.lock()
            {
                *guard = if state.is_effectively_stopped() { None } else { Some(state) };
                notify_media_ui(broker_tx);
            }
        }
        MediaMessage::MediaCommand(cmd) => {
            // Android session IDs are phone-generated identifiers; MPRIS bus
            // names contain "org.mpris.MediaPlayer2". If the target doesn't
            // look like an MPRIS bus name, forward to the phone.
            let forward_to_phone = cmd
                .target_session
                .as_deref()
                .is_some_and(|t| !t.contains("org.mpris.MediaPlayer2"));
            if forward_to_phone {
                send_media_to_device(sdk_binding, &MediaMessage::MediaCommand(cmd));
            } else {
                apply_local_command(&cmd);
            }
        }
    }
}

fn apply_local_command(cmd: &MediaCommand) {
    let finder = match PlayerFinder::new() {
        Ok(f) => f,
        Err(e) => {
            log::warn!("media: cannot connect to D-Bus to apply command: {e}");
            return;
        }
    };

    let player = match cmd.target_session.as_deref().filter(|s| !s.is_empty()) {
        Some(bus) => finder
            .find_all()
            .ok()
            .and_then(|players| players.into_iter().find(|p| p.bus_name() == bus)),
        None => pick_active_player(&finder).ok().flatten(),
    };

    let player = match player {
        Some(p) => p,
        None => {
            log::warn!("media: no local player to apply '{:?}'", cmd.command);
            return;
        }
    };

    let result = match cmd.command {
        Command::Play => player.play(),
        Command::Pause => player.pause(),
        Command::Playpause => player.play_pause(),
        Command::Next => player.next(),
        Command::Previous => player.previous(),
        Command::Stop => {
            // MPRIS has no direct Stop; pause + seek to 0.
            player.pause().ok();
            if let Some(track_id) = player.get_metadata().ok().and_then(|m| m.track_id()) {
                player.set_position(track_id, &Duration::from_millis(0)).ok();
            }
            return;
        }
        Command::Seek => match cmd.position_ms {
            Some(ms) => apply_seek(&player, ms),
            None => {
                log::warn!("media: seek without position_ms");
                return;
            }
        },
        Command::Unknown => {
            log::warn!("media: unknown command");
            return;
        }
    };

    if let Err(e) = result {
        log::warn!("media: command {:?} failed: {e}", cmd.command);
    }
}

fn apply_seek(player: &Player, position_ms: u64) -> Result<(), mpris::DBusError> {
    let target = Duration::from_millis(position_ms);
    if let Some(track_id) = player.get_metadata().ok().and_then(|m| m.track_id()) {
        return player.set_position(track_id, &target);
    }
    let current = player.get_position().unwrap_or_default();
    let delta_us = target.as_micros() as i64 - current.as_micros() as i64;
    player.seek(delta_us)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(session: &str, title: &str, pos: u64) -> MediaState {
        MediaState {
            source: Source::Android,
            session_id: session.to_string(),
            state: PlaybackState::Playing,
            title: title.to_string(),
            artist: "someone".to_string(),
            album: String::new(),
            app: String::new(),
            position_ms: pos,
            duration_ms: 200_000,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_prev: true,
            can_seek: true,
            art_url: String::new(),
            playback_speed: 1.0,
            ts: 0,
        }
    }

    fn last_sent_from(state: &MediaState) -> LastSent {
        LastSent {
            track_fp: state.track_fingerprint(),
            position_ms: state.position_ms,
            sent_at: Instant::now(),
            speed: state.playback_speed,
        }
    }

    fn paused(session: &str, title: &str) -> MediaState {
        MediaState { state: PlaybackState::Paused, ..playing(session, title, 0) }
    }

    fn stopped(session: &str) -> MediaState {
        MediaState { state: PlaybackState::Stopped, ..playing(session, "", 0) }
    }

    #[test]
    fn phone_playing_always_exposes_mirror() {
        let s = playing("sp", "song", 0);
        assert!(should_expose_phone_mirror(Some(&s), false));
        assert!(should_expose_phone_mirror(Some(&s), true));
    }

    #[test]
    fn phone_paused_with_desktop_playing_drops_mirror() {
        let s = paused("sp", "song");
        assert!(!should_expose_phone_mirror(Some(&s), true));
    }

    #[test]
    fn phone_paused_no_desktop_playing_keeps_mirror() {
        let s = paused("sp", "song");
        assert!(should_expose_phone_mirror(Some(&s), false));
    }

    #[test]
    fn phone_stopped_never_exposes_mirror() {
        let s = stopped("sp");
        assert!(!should_expose_phone_mirror(Some(&s), false));
        assert!(!should_expose_phone_mirror(Some(&s), true));
    }

    #[test]
    fn no_phone_state_never_exposes_mirror() {
        assert!(!should_expose_phone_mirror(None, false));
        assert!(!should_expose_phone_mirror(None, true));
    }

    fn desktop_track(session: &str, title: &str, duration_ms: u64) -> MediaState {
        MediaState {
            source: Source::Desktop,
            session_id: session.to_string(),
            state: PlaybackState::Playing,
            title: title.to_string(),
            artist: "someone".to_string(),
            album: String::new(),
            app: String::new(),
            position_ms: 0,
            duration_ms,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_prev: true,
            can_seek: true,
            art_url: String::new(),
            playback_speed: 1.0,
            ts: 0,
        }
    }

    #[test]
    fn transient_zero_duration_patched_from_previous() {
        let prev = desktop_track("firefox:1", "some video", 300_000);
        let raw = MediaState { duration_ms: 0, position_ms: 40_000, ..prev.clone() };
        let patched = patch_transient_zero_duration(raw, Some(&prev));
        assert_eq!(patched.duration_ms, 300_000);
        assert_eq!(patched.position_ms, 40_000);
    }

    #[test]
    fn zero_duration_without_previous_stays_zero() {
        let raw = desktop_track("firefox:1", "some video", 0);
        let patched = patch_transient_zero_duration(raw, None);
        assert_eq!(patched.duration_ms, 0);
    }

    #[test]
    fn different_track_does_not_carry_forward_duration() {
        let prev = desktop_track("firefox:1", "first video", 300_000);
        let raw = desktop_track("firefox:1", "second video", 0);
        let patched = patch_transient_zero_duration(raw, Some(&prev));
        assert_eq!(patched.duration_ms, 0);
    }

    #[test]
    fn different_session_does_not_carry_forward_duration() {
        let prev = desktop_track("firefox:1", "some song", 240_000);
        let raw = desktop_track("vlc:2", "some song", 0);
        let patched = patch_transient_zero_duration(raw, Some(&prev));
        assert_eq!(patched.duration_ms, 0);
    }

    #[test]
    fn nonzero_duration_passes_through() {
        let prev = desktop_track("firefox:1", "some video", 300_000);
        let raw = desktop_track("firefox:1", "some video", 305_000);
        let patched = patch_transient_zero_duration(raw, Some(&prev));
        assert_eq!(patched.duration_ms, 305_000);
    }

    #[test]
    fn remote_state_cleared_on_disconnect() {
        let remote_state: SharedMediaState =
            Arc::new(Mutex::new(Some(playing("phone-1", "Some Song", 12_000))));
        let last_sent: SharedLastSent = Arc::new(Mutex::new(None));

        handle_device_disconnected(&remote_state, &last_sent);

        assert!(remote_state.lock().unwrap().is_none());
    }

    /// A sudden disconnect must reset the poller's `LastSent` cache. Without
    /// this, on reconnect `should_send` still thinks the phone has the
    /// current anchor and skips the resend — the phone extrapolates from
    /// nothing and its UI diverges from reality.
    #[test]
    fn last_sent_reset_on_disconnect() {
        let state = playing("desktop-mpris", "Some Song", 12_000);
        let remote_state: SharedMediaState = Arc::new(Mutex::new(None));
        let last_sent: SharedLastSent = Arc::new(Mutex::new(Some(last_sent_from(&state))));

        handle_device_disconnected(&remote_state, &last_sent);

        assert!(last_sent.lock().unwrap().is_none());
    }

    /// End-to-end at the `should_send` level: with an anchor cached,
    /// re-sending the same track within the heartbeat is a no-op; after a
    /// disconnect clears the cache, the very next poll must fire a fresh
    /// anchor so the reconnected phone starts from a known base.
    #[test]
    fn should_send_returns_true_after_disconnect() {
        let state = playing("desktop-mpris", "Some Song", 12_000);
        let cached = last_sent_from(&state);

        // Pre-disconnect: same track, well inside the heartbeat, no seek —
        // the poller should stay quiet.
        assert!(
            !should_send(&state, Some(&cached)),
            "sanity: unchanged state within heartbeat shouldn't resend"
        );

        // Simulate the disconnect + reconnect: cache is cleared, next tick
        // sees the same MPRIS state but no anchor to diff against.
        let remote_state: SharedMediaState = Arc::new(Mutex::new(None));
        let last_sent: SharedLastSent = Arc::new(Mutex::new(Some(cached)));
        handle_device_disconnected(&remote_state, &last_sent);

        let after = last_sent.lock().unwrap().clone();
        assert!(should_send(&state, after.as_ref()));
    }

    /// The receiver thread shares the same channel as `media_state` /
    /// `media_command` messages because the broker fans `Broadcast` events
    /// out to every service. A `device_disconnected` payload must trigger
    /// cleanup without being mistaken for a malformed `MediaMessage`.
    #[test]
    fn handle_incoming_routes_device_disconnected_to_cleanup() {
        let remote_state: SharedMediaState =
            Arc::new(Mutex::new(Some(playing("phone-1", "Some Song", 0))));
        let last_sent: SharedLastSent =
            Arc::new(Mutex::new(Some(last_sent_from(&playing("desktop-mpris", "Local Song", 0)))));
        let payload = r#"{"type":"device_disconnected","device_id":"phone-1"}"#;
        handle_incoming(payload, &remote_state, &last_sent, &SdkMediaBinding::default(), None);

        assert!(remote_state.lock().unwrap().is_none());
        assert!(last_sent.lock().unwrap().is_none());
    }

    #[test]
    fn handle_incoming_ignores_non_media_control_messages() {
        let remote_state: SharedMediaState =
            Arc::new(Mutex::new(Some(playing("phone-1", "Some Song", 0))));
        let last_sent: SharedLastSent = Arc::new(Mutex::new(None));
        let payload = r#"{"plugin_id":"screen","type":"stream_info","width":1920,"height":1080}"#;

        handle_incoming(payload, &remote_state, &last_sent, &SdkMediaBinding::default(), None);

        let state = remote_state.lock().unwrap();
        assert_eq!(state.as_ref().map(|state| state.title.as_str()), Some("Some Song"));
    }
}
