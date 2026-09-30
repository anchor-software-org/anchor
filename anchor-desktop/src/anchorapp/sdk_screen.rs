//! Ordered sender for screen state records on the typed SDK capability.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anchor_sdk::{Capability, DatagramFlow, ReliableSendStream, screen, video_frame};
use bytes::Bytes;

use crate::anchorapp::device::{BroadcastFrame, FrameBroadcaster};

pub use screen::{OUTPUT_LIST_TYPE_URL, STATUS_TYPE_URL};

type ScreenOutput = screen::ScreenOutput;

/// Frames between periodic telemetry samples of ordinary predictive frames.
/// Keyframes are reported separately and unconditionally; see `frame_sampled`.
const PERIODIC_SAMPLE_FRAMES: u64 = 60;

/// Decide whether one access unit is reported in the screen telemetry.
///
/// Keyframes are always reported. Sampling them periodically does not work:
/// the sample period is a whole multiple of the GOP, so a fixed phase offset
/// between the encoder's IDR cadence and the sender's sequence counter decides
/// whether a keyframe is *ever* sampled — not chance. A 60-frame sample with a
/// 30-frame GOP offset by 15 reports zero keyframes indefinitely, which makes
/// the log show only small predictive frames while multi-megabyte IDRs go
/// unrecorded.
fn frame_sampled(sequence: u64, keyframe: bool) -> bool {
    keyframe || sequence.is_multiple_of(PERIODIC_SAMPLE_FRAMES)
}

/// Return the media-packet limit negotiated for this QUIC path. The sender's
/// fixed 1,100-byte packet is only safe when the peer accepts that full size.
fn negotiated_screen_datagram_limit(limit: Option<usize>) -> Option<usize> {
    limit
        .map(|value| value.min(video_frame::FRAME_DATAGRAM_BYTES))
        .filter(|value| *value > video_frame::FRAME_HEADER_BYTES)
}

/// XOR parity has a fixed 1,100-byte payload. Do not emit it on a smaller
/// negotiated path; the media fragments still fit, and avoiding parity is
/// better than rejecting the whole access unit.
fn can_use_fixed_size_parity(fec_enabled: bool, datagram_limit: usize) -> bool {
    fec_enabled && datagram_limit >= video_frame::FRAME_DATAGRAM_BYTES
}

#[derive(Clone)]
enum ScreenMessage {
    Outputs(Vec<ScreenOutput>),
    Status { state: i32, output_id: String },
}

#[derive(Clone)]
enum FrameTarget {
    Datagram(DatagramFlow),
    Stream(ReliableFrameStream),
}

impl FrameTarget {
    fn flow_id(&self) -> u64 {
        match self {
            FrameTarget::Datagram(flow) => flow.flow_id(),
            FrameTarget::Stream(stream) => stream.stream_id,
        }
    }
}

#[derive(Clone)]
pub struct SdkScreenSender {
    tx: SyncSender<ScreenMessage>,
    /// The two transports are mutually exclusive — one lock instead of two
    /// keeps the per-frame `send_frame` entry cheap.
    frame_target: Arc<Mutex<Option<FrameTarget>>>,
    keyframe_request: Arc<Mutex<Option<Arc<FrameBroadcaster>>>>,
    recovery: Arc<Mutex<KeyframeRecovery>>,
    capability_session_id: u64,
    frame_sequence: Arc<AtomicU64>,
    /// Monotonic admission schedule for screen access units.  Quinn paces
    /// packets once queued, but an application can still enqueue frames faster
    /// than the path drains them.  Keeping the schedule here prevents that
    /// queue from becoming a hidden latency buffer.
    next_frame_at: Arc<Mutex<Instant>>,
    pacing_percent: usize,
    pacing_bps: usize,
    fec_enabled: bool,
}

#[derive(Clone)]
struct ReliableFrameStream {
    stream_id: u64,
    handle: tokio::runtime::Handle,
    send: Arc<tokio::sync::Mutex<ReliableSendStream>>,
}

impl SdkScreenSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        let capability_session_id = capability.session_id();
        let configured_bitrate = crate::anchorapp::settings::settings().encoding.bitrate_bps;
        // Leave room for QUIC/Anchor datagram overhead and short Wi-Fi rate
        // fluctuations.  This is an admission rate, not an encoder bitrate;
        // frame quality remains controlled by the encoder setting.
        let pacing_percent = crate::anchorapp::settings::settings()
            .encoding
            .pacing_utilization_percent
            .clamp(50, 100);
        let pacing_bps =
            configured_bitrate.saturating_mul(pacing_percent).saturating_div(100).max(1);
        // XOR-parity FEC on the lossy datagram path; ~6% extra datagrams let a
        // receiver rebuild a singly-lost fragment instead of corrupting the
        // whole access unit. Old receivers drop parity by kind, so this is
        // wire-safe — `ANCHOR_FEC=0` disables it for A/B measurement.
        let fec_enabled = std::env::var("ANCHOR_FEC").map_or(true, |value| value != "0");
        thread::Builder::new()
            .name("anchor-sdk-screen-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK screen runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    let result = runtime.block_on(async {
                        match message {
                            ScreenMessage::Outputs(outputs) => {
                                capability
                                    .send_record(
                                        OUTPUT_LIST_TYPE_URL,
                                        screen::encode_output_list(outputs),
                                    )
                                    .await
                            }
                            ScreenMessage::Status { state, output_id } => {
                                capability
                                    .send_record(
                                        STATUS_TYPE_URL,
                                        screen::encode_status(screen::ScreenStatus {
                                            state,
                                            output_id,
                                            width: 0,
                                            height: 0,
                                            fps: 0,
                                            bitrate_kbps: 0,
                                        }),
                                    )
                                    .await
                            }
                        }
                    });
                    if let Err(error) = result {
                        log::warn!("SDK screen send failed: {error}");
                    } else {
                        log::info!("SDK screen record sent over QUIC");
                    }
                }
            })
            .expect("SDK screen writer thread must start");
        Self {
            tx,
            frame_target: Arc::new(Mutex::new(None)),
            keyframe_request: Arc::new(Mutex::new(None)),
            recovery: Arc::new(Mutex::new(KeyframeRecovery::default())),
            capability_session_id,
            frame_sequence: Arc::new(AtomicU64::new(0)),
            next_frame_at: Arc::new(Mutex::new(Instant::now())),
            pacing_percent,
            pacing_bps,
            fec_enabled,
        }
    }

    pub fn send_outputs(&self, outputs: Vec<ScreenOutput>) -> bool {
        self.tx.try_send(ScreenMessage::Outputs(outputs)).is_ok()
    }

    pub fn send_status(&self, state: i32, output_id: String) -> bool {
        self.tx.try_send(ScreenMessage::Status { state, output_id }).is_ok()
    }

    pub fn attach_frame_flow(&self, flow: DatagramFlow) {
        crate::anchorwayland::frame_trace::announce_run();
        log::info!("Sideboat screen transport active: datagram flow={}", flow.flow_id());
        *self.frame_target.lock().unwrap() = Some(FrameTarget::Datagram(flow));
    }

    /// Attach the reliable ordered stream used by screen video. The stream is
    /// written from the dedicated frame subscription thread, while Quinn's
    /// async send stream remains protected by its Tokio mutex.
    pub fn attach_frame_stream(
        &self,
        stream_id: u64,
        send: ReliableSendStream,
        handle: tokio::runtime::Handle,
    ) {
        crate::anchorwayland::frame_trace::announce_run();
        log::info!("Sideboat screen transport active: reliable QUIC stream={stream_id}");
        *self.frame_target.lock().unwrap() = Some(FrameTarget::Stream(ReliableFrameStream {
            stream_id,
            handle,
            send: Arc::new(tokio::sync::Mutex::new(send)),
        }));
    }

    /// Connect transport-level frame drops to the encoder's existing IDR
    /// recovery path. A lost access unit makes following H.264 predictive
    /// frames unusable until the next keyframe.
    pub fn set_keyframe_request_source(&self, broadcaster: Arc<FrameBroadcaster>) {
        *self.keyframe_request.lock().unwrap() = Some(broadcaster);
    }

    /// Ask the capture loop to force an IDR on its next encode.
    fn raise_keyframe_request(&self) {
        if let Some(broadcaster) = self.keyframe_request.lock().unwrap().as_ref() {
            broadcaster.needs_keyframe.store(true, Ordering::Relaxed);
        }
    }

    /// Withdraw a pending IDR request once an IDR has been delivered. With
    /// several viewers on one broadcaster this can clear another viewer's
    /// request; that viewer's next held predictive frame raises it again.
    fn withdraw_keyframe_request(&self) {
        if let Some(broadcaster) = self.keyframe_request.lock().unwrap().as_ref() {
            broadcaster.needs_keyframe.store(false, Ordering::Relaxed);
        }
    }

    /// Mark a producer-side gap on either transport: capture frame
    /// `newest_lost` (and possibly older ones) never reached the decoder. Only
    /// an IDR encoded after it makes predictive units useful again.
    pub fn mark_frame_gap(&self, newest_lost: u64) {
        if self.frame_target.lock().unwrap().is_some()
            && self.recovery.lock().unwrap().on_gap(newest_lost, Instant::now())
        {
            self.raise_keyframe_request();
        }
    }

    /// Fragment and send one encoded H.264 access unit.
    pub fn send_frame(&self, frame: &BroadcastFrame) -> bool {
        let target = self.frame_target.lock().unwrap().clone();
        let Some(target) = target else {
            return false;
        };
        let flow_id = target.flow_id();
        let sequence = self.frame_sequence.fetch_add(1, Ordering::Relaxed);
        let source_frame_id = frame.source_frame_id;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_micros() as u64)
            .unwrap_or_default();
        crate::anchorwayland::frame_trace::event!(
            "sender_start",
            {
                "source_frame_id": source_frame_id,
                "sender_sequence": sequence,
                "capability_session_id": self.capability_session_id,
                "flow_id": flow_id,
                "presentation_time_us": timestamp,
                "encoded_bytes": frame.len(),
                "transport": match target {
                    FrameTarget::Stream(_) => "reliable_stream",
                    FrameTarget::Datagram(_) => "datagram",
                },
                "broadcaster_age_us": crate::anchorwayland::frame_trace::mono_ns()
                    .saturating_sub(frame.published_ns) / 1_000,
            }
        );
        let flags = if contains_h264_idr(frame) { video_frame::FLAG_KEYFRAME } else { 0 };
        let keyframe = flags & video_frame::FLAG_KEYFRAME != 0;
        let admission = self.recovery.lock().unwrap().admit(keyframe, Instant::now());
        if admission.request_keyframe {
            self.raise_keyframe_request();
        }
        if !admission.send {
            // Do not put a P-frame after a coalescing gap on either
            // transport. It references an access unit that was never
            // delivered; on datagrams it would also waste the loss budget.
            crate::anchorwayland::frame_trace::event!(
                "sender_outcome",
                {
                    "source_frame_id": source_frame_id,
                    "sender_sequence": sequence,
                    "transport": match target {
                        FrameTarget::Stream(_) => "reliable_stream",
                        FrameTarget::Datagram(_) => "datagram",
                    },
                    "outcome": "dropped",
                    "reason": "awaiting_keyframe_after_coalescing",
                    "encoded_bytes": frame.len(),
                }
            );
            return false;
        }
        let fragment_result = if let FrameTarget::Datagram(flow) = &target {
            let Some(datagram_limit) = negotiated_screen_datagram_limit(flow.max_datagram_size())
            else {
                log::warn!(
                    "SDK screen datagram path has no usable peer limit; dropping frame before send"
                );
                return false;
            };
            // Datagrams are lossy — append XOR parity so receivers can rebuild
            // a singly-lost fragment instead of corrupting the whole access
            // unit and stalling until the next keyframe.
            if can_use_fixed_size_parity(self.fec_enabled, datagram_limit) {
                video_frame::fragment_frame_with_parity(
                    video_frame::FRAME_KIND_SCREEN,
                    flags,
                    self.capability_session_id,
                    flow_id,
                    sequence,
                    timestamp,
                    0,
                    frame,
                )
            } else {
                video_frame::fragment_frame_with_flags_and_limit(
                    video_frame::FRAME_KIND_SCREEN,
                    flags,
                    self.capability_session_id,
                    flow_id,
                    sequence,
                    timestamp,
                    0,
                    frame,
                    datagram_limit,
                )
            }
        } else {
            video_frame::fragment_frame_with_flags(
                video_frame::FRAME_KIND_SCREEN,
                flags,
                self.capability_session_id,
                flow_id,
                sequence,
                timestamp,
                0,
                frame,
            )
        };
        let packets = match fragment_result {
            Ok(packets) => packets,
            Err(error) => {
                log::warn!("SDK screen frame rejected: {error}");
                crate::anchorwayland::frame_trace::event!(
                    "sender_outcome",
                    {
                        "source_frame_id": source_frame_id,
                        "sender_sequence": sequence,
                        "outcome": "rejected",
                        "reason": error.to_string(),
                        "encoded_bytes": frame.len(),
                    }
                );
                return false;
            }
        };
        let packet_count = packets.len();
        let sampled = frame_sampled(sequence, keyframe);
        if sampled {
            log::info!(
                "SDK screen access unit: sequence={sequence} bytes={} keyframe={} nal_types={:?}",
                frame.len(),
                keyframe,
                h264_nal_types(frame),
            );
        }
        let pacing_wait_us = if matches!(target, FrameTarget::Datagram(_)) {
            let pacing_started = Instant::now();
            let wire_bytes =
                packets.iter().map(|packet| packet.len().saturating_add(32)).sum::<usize>();
            let pacing_duration =
                Duration::from_secs_f64(wire_bytes as f64 * 8.0 / self.pacing_bps as f64);
            let mut next = self.next_frame_at.lock().unwrap();
            let now = Instant::now();
            if *next < now {
                *next = now;
            }
            // The subscription queue behind this thread holds ~2 frames
            // (~33ms at 60fps). A wait beyond that — e.g. the ~80ms a
            // 350KB recovery IDR accrues at a bitrate-matched pacing rate —
            // fills the queue mid-sleep: publisher drops a fresh access
            // unit, which forces another IDR, which accrues more debt.
            // Bound the wait so smoothing can never starve the drain;
            // sustained overflow is guarded by the send-buffer admission
            // check below, not by sleeping here.
            const MAX_PACING_WAIT: Duration = Duration::from_millis(8);
            let wait = next.saturating_duration_since(now).min(MAX_PACING_WAIT);
            if !wait.is_zero() {
                // nanosleep overshoots by tens of microseconds on Linux;
                // sleep the bulk, then spin the tail so datagrams leave the
                // queue on schedule instead of a scheduler tick late.
                const SPIN_TAIL: Duration = Duration::from_micros(250);
                if wait > SPIN_TAIL {
                    thread::sleep(wait - SPIN_TAIL);
                }
                let deadline = now + wait;
                while Instant::now() < deadline {
                    std::hint::spin_loop();
                }
            }
            // Debt beyond the cap is discarded — the schedule stays within
            // MAX_PACING_WAIT of now instead of stalling the drain while a
            // large frame's budget amortizes.
            *next = next
                .checked_add(pacing_duration)
                .map(|t| t.min(Instant::now() + MAX_PACING_WAIT))
                .unwrap_or_else(Instant::now);
            pacing_started.elapsed().as_micros()
        } else {
            0
        };
        // Datagram mode queues the whole access unit atomically. If its budget
        // is exhausted, drop this frame before sending any fragments; partial
        // frames only consume queue space and can never be decoded. Stream
        // mode writes the same packets sequentially and relies on QUIC flow
        // control for backpressure.
        let admission_started = Instant::now();
        let delivered = if let FrameTarget::Stream(stream) = &target {
            let mut chunks = match video_frame::stream_chunks(&packets) {
                Ok(chunks) => chunks,
                Err(error) => {
                    log::warn!("SDK screen stream frame rejected: {error}");
                    return false;
                }
            };
            let wire_bytes = chunks.iter().map(Bytes::len).sum::<usize>();
            let stream_started = Instant::now();
            let result = stream.handle.block_on(async {
                let mut send = stream.send.lock().await;
                send.write_all_chunks(&mut chunks).await
            });
            let stream_send_us = stream_started.elapsed().as_micros();
            if let Err(error) = result {
                if self.recovery.lock().unwrap().on_send_failed(
                    keyframe,
                    source_frame_id,
                    Instant::now(),
                ) {
                    self.raise_keyframe_request();
                }
                log::warn!(
                    "SDK screen reliable stream send failed (stream={} source_frame_id={} sequence={} bytes={} fragments={} keyframe={}): {error}",
                    stream.stream_id,
                    source_frame_id,
                    sequence,
                    frame.len(),
                    packet_count,
                    keyframe
                );
                false
            } else {
                if sampled {
                    log::info!(
                        "SDK screen frame sent on reliable stream: source_frame_id={} sequence={sequence} bytes={} fragments={} keyframe={} write_us={}",
                        source_frame_id,
                        frame.len(),
                        packet_count,
                        keyframe,
                        stream_send_us
                    );
                }
                if stream_send_us >= 100_000 {
                    log::warn!(
                        "SDK screen reliable stream backpressure: stream={} write_us={} bytes={}",
                        stream.stream_id,
                        stream_send_us,
                        wire_bytes
                    );
                }
                crate::anchorwayland::frame_trace::event!(
                    "sender_outcome",
                    {
                        "source_frame_id": source_frame_id,
                        "sender_sequence": sequence,
                        "transport": "reliable_stream",
                        "outcome": "accepted",
                        "encoded_bytes": frame.len(),
                        "wire_bytes": wire_bytes,
                        "fragments": packet_count,
                        "keyframe": keyframe,
                        "send_us": stream_send_us,
                    }
                );
                true
            }
        } else {
            let FrameTarget::Datagram(flow) = &target else {
                unreachable!("target is Datagram or Stream")
            };
            match flow.send_many(packets) {
                Ok(()) => true,
                Err(error) => {
                    let admission_us = admission_started.elapsed().as_micros();
                    let stats = flow.transport_stats();
                    let (available, required) = match &error {
                        anchor_sdk::DatagramSendError::BufferFull { available, required } => {
                            (*available, *required)
                        }
                        anchor_sdk::DatagramSendError::Transport(_) => {
                            (flow.send_buffer_space(), 0)
                        }
                    };
                    // A failed predictive frame requests an IDR as soon as
                    // backoff allows. A failed IDR holds predictive frames and
                    // is retried by the next one.
                    if self.recovery.lock().unwrap().on_send_failed(
                        keyframe,
                        source_frame_id,
                        Instant::now(),
                    ) {
                        self.raise_keyframe_request();
                    }
                    log::warn!(
                        "SDK screen datagram send failed (flow={} source_frame_id={} sequence={} bytes={} fragments={} keyframe={} pacing_wait_us={} pacing_percent={} admission_us={} rtt_ms={} cwnd={} lost_packets={} congestion_events={} available={} required={}): {error}",
                        flow.flow_id(),
                        source_frame_id,
                        sequence,
                        frame.len(),
                        packet_count,
                        keyframe,
                        pacing_wait_us,
                        self.pacing_percent,
                        admission_us,
                        stats.rtt.as_millis(),
                        stats.congestion_window_bytes,
                        stats.lost_packets,
                        stats.congestion_events,
                        available,
                        required,
                    );
                    crate::anchorwayland::frame_trace::event!(
                        "sender_outcome",
                        {
                            "source_frame_id": source_frame_id,
                            "sender_sequence": sequence,
                            "transport": "datagram",
                            "outcome": "rejected",
                            "reason": error.to_string(),
                            "encoded_bytes": frame.len(),
                            "fragments": packet_count,
                            "keyframe": keyframe,
                        }
                    );
                    false
                }
            }
        };
        if delivered && self.recovery.lock().unwrap().on_delivered(keyframe, source_frame_id) {
            self.withdraw_keyframe_request();
        }
        let admission_us = admission_started.elapsed().as_micros();
        if delivered
            && sampled
            && let FrameTarget::Datagram(flow) = &target
        {
            let stats = flow.transport_stats();
            log::info!(
                "SDK screen frame datagrams sent: source_frame_id={} sequence={sequence} bytes={} fragments={} keyframe={} pacing_wait_us={} admission_us={} pacing_percent={} pacing_bps={} available={} rtt_ms={} cwnd={} lost_packets={} congestion_events={}",
                source_frame_id,
                frame.len(),
                packet_count,
                keyframe,
                pacing_wait_us,
                admission_us,
                self.pacing_percent,
                self.pacing_bps,
                flow.send_buffer_space(),
                stats.rtt.as_millis(),
                stats.congestion_window_bytes,
                stats.lost_packets,
                stats.congestion_events,
            );
            crate::anchorwayland::frame_trace::event!(
                "sender_outcome",
                {
                    "source_frame_id": source_frame_id,
                    "sender_sequence": sequence,
                    "transport": "datagram",
                    "outcome": "accepted",
                    "encoded_bytes": frame.len(),
                    "fragments": packet_count,
                    "keyframe": keyframe,
                    "pacing_wait_us": pacing_wait_us,
                    "admission_us": admission_us,
                }
            );
        }
        delivered
    }
}

fn should_schedule_keyframe_recovery(keyframe: bool) -> bool {
    !keyframe
}

fn should_drop_predictive_frame(waiting_for_keyframe: bool, keyframe: bool) -> bool {
    waiting_for_keyframe && !keyframe
}

/// What the sender does with one access unit, and whether the encoder must be
/// asked for a recovery IDR as a side effect.
#[derive(Debug, PartialEq, Eq)]
struct Admission {
    send: bool,
    request_keyframe: bool,
}

/// Delays between one viewer's consecutive keyframe requests while recovery
/// keeps failing. The first request after a healthy stream is immediate.
const KEYFRAME_RETRY_BACKOFF: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
];

/// Sender-side H.264 recovery decisions, kept free of transports, threads and
/// the wall clock so a whole loss/recovery sequence can be driven by a test.
///
/// While the decoder's reference chain is broken, held predictive frames keep
/// asking for an IDR, and only a delivered IDR encoded after the newest lost
/// frame ends recovery. A lost recovery IDR is therefore always requested
/// again, rather than leaving the stream frozen until the periodic keyframe.
///
/// Requests are postponed, never dropped: each unsuccessful one pushes the
/// next back along `KEYFRAME_RETRY_BACKOFF`, and a healing IDR resets it. The
/// encoder is shared by every viewer, so without this a viewer whose
/// connection has stalled would force IDRs on all of them at the capture
/// loop's `MIN_IDR_INTERVAL` until its session timed out.
#[derive(Debug, Default)]
struct KeyframeRecovery {
    /// Newest capture frame (`source_frame_id`) known not to have reached the
    /// decoder, while the reference chain is broken. H.264 cannot safely skip
    /// a predictive access unit on either transport: every following P-frame
    /// can reference it, so P-frames are held while this is set. This keeps
    /// the receiver from displaying a stale/broken reference chain.
    lost_through: Option<u64>,
    /// Keyframe requests made since the chain last healed.
    attempts: usize,
    /// Earliest time the next keyframe request may be raised.
    next_request_at: Option<Instant>,
}

impl KeyframeRecovery {
    /// Returns whether a wanted keyframe request may be raised now, and if so
    /// schedules the next one further out.
    fn request_due(&mut self, wanted: bool, now: Instant) -> bool {
        if !wanted || self.next_request_at.is_some_and(|at| now < at) {
            return false;
        }
        let delay = KEYFRAME_RETRY_BACKOFF[self.attempts.min(KEYFRAME_RETRY_BACKOFF.len() - 1)];
        self.next_request_at = Some(now + delay);
        self.attempts += 1;
        true
    }

    /// Capture frame `newest_lost` (and possibly older ones) was coalesced or
    /// dropped before reaching the decoder. Returns whether to request a
    /// recovery IDR now; if backoff defers it, a later held frame asks.
    fn on_gap(&mut self, newest_lost: u64, now: Instant) -> bool {
        self.lost_through = Some(self.lost_through.map_or(newest_lost, |l| l.max(newest_lost)));
        self.request_due(true, now)
    }

    /// Decide whether one encoded access unit goes on the wire. A held
    /// predictive frame means no healing IDR has landed yet, so it asks for
    /// one again once backoff allows.
    fn admit(&mut self, keyframe: bool, now: Instant) -> Admission {
        if should_drop_predictive_frame(self.lost_through.is_some(), keyframe) {
            return Admission { send: false, request_keyframe: self.request_due(true, now) };
        }
        Admission { send: true, request_keyframe: false }
    }

    /// The transport accepted an access unit. Returns true when it was an IDR
    /// encoded after every known loss: that heals the chain and resets
    /// backoff, so any IDR request still pending is redundant and should be
    /// withdrawn to keep the capture loop from forcing a second one. An older
    /// IDR — one queued before a later frame was dropped — heals nothing and
    /// leaves the request standing.
    fn on_delivered(&mut self, keyframe: bool, source_frame_id: u64) -> bool {
        let heals = keyframe && self.lost_through.is_none_or(|lost| source_frame_id > lost);
        if heals {
            self.lost_through = None;
            self.attempts = 0;
            self.next_request_at = None;
        }
        heals
    }

    /// The transport rejected an access unit that `admit` let through. Either
    /// way the decoder is now missing a reference, so predictive frames are
    /// held. Returns whether to request an IDR right away: a rejected IDR is
    /// retried by the next held frame instead, which keeps a full datagram
    /// budget from being answered with an immediate second keyframe burst.
    fn on_send_failed(&mut self, keyframe: bool, source_frame_id: u64, now: Instant) -> bool {
        self.lost_through =
            Some(self.lost_through.map_or(source_frame_id, |l| l.max(source_frame_id)));
        self.request_due(should_schedule_keyframe_recovery(keyframe), now)
    }
}

/// Detect an H.264 IDR NAL in the Annex-B access unit emitted by FFmpeg.
/// Repeating the keyframe bit in the Anchor frame header lets receivers
/// recover after an intentional queue drop or an unreliable-datagram loss.
fn contains_h264_idr(data: &[u8]) -> bool {
    // Some FFmpeg paths expose Annex-B start codes and others expose AVC
    // length-prefixed NAL units. Parse boundaries, rather than searching for
    // a byte pattern: scanning payload bytes can mistake an ordinary P-frame
    // for an IDR when it happens to contain `00 00 01 65`.
    if contains_annex_b_idr(data) {
        return true;
    }
    contains_length_prefixed_idr(data)
}

/// Small diagnostic parser used in runtime telemetry. It makes an encoder GOP
/// regression visible without dumping screen content into logs.
fn h264_nal_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut start = find_annex_b_start(data, 0);
    while let Some(offset) = start {
        let header = offset + if data[offset..].starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
        if header >= data.len() {
            break;
        }
        types.push(data[header] & 0x1f);
        start = find_annex_b_start(data, header);
    }
    if !types.is_empty() {
        return types;
    }
    let mut offset = 0;
    while offset + 4 <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let header = offset + 4;
        let Some(end) = header.checked_add(length) else { break };
        if length == 0 || end > data.len() {
            break;
        }
        types.push(data[header] & 0x1f);
        offset = end;
    }
    types
}

fn contains_annex_b_idr(data: &[u8]) -> bool {
    let Some(mut start) = find_annex_b_start(data, 0) else {
        return false;
    };
    loop {
        let nal_start = start + if data[start..].starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
        if nal_start >= data.len() {
            return false;
        }
        let Some(next) = find_annex_b_start(data, nal_start) else {
            return data[nal_start] & 0x1f == 5;
        };
        let nal_end = data[nal_start..next]
            .iter()
            .rposition(|byte| *byte != 0)
            .map(|index| nal_start + index + 1)
            .unwrap_or(nal_start);
        if nal_end > nal_start && data[nal_start] & 0x1f == 5 {
            return true;
        }
        start = next;
    }
}

fn find_annex_b_start(data: &[u8], from: usize) -> Option<usize> {
    // Search for the 3-byte start code with a SIMD substring scan. The
    // 4-byte form `00 00 00 01` contains `00 00 01` at offset +1, so a hit
    // preceded by another zero belongs to a 4-byte code. H.264 emulation
    // prevention guarantees `00 00 01` cannot appear inside a NAL payload,
    // so every hit is a genuine start code.
    let offset = memchr::memmem::find(data.get(from..)?, &[0, 0, 1])? + from;
    if offset > from && data[offset - 1] == 0 {
        return Some(offset - 1);
    }
    Some(offset)
}

fn contains_length_prefixed_idr(data: &[u8]) -> bool {
    let mut offset = 0;
    let mut saw_nal = false;
    let mut has_idr = false;
    while offset + 4 <= data.len() {
        let nal_len = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let payload_start = offset + 4;
        let Some(end) = payload_start.checked_add(nal_len) else {
            return false;
        };
        if nal_len == 0 || end > data.len() {
            return false;
        }
        saw_nal = true;
        has_idr |= data[payload_start] & 0x1f == 5;
        offset = end;
    }
    saw_nal && offset == data.len() && has_idr
}

#[derive(Clone, Default)]
pub struct SdkScreenBinding {
    senders: Arc<Mutex<std::collections::HashMap<String, SdkScreenSender>>>,
    pending: Arc<Mutex<Vec<ScreenMessage>>>,
    broadcaster: Arc<Mutex<Option<Arc<FrameBroadcaster>>>>,
    /// Authoritative state to replay when a device reconnects after its
    /// previous capability session was closed.
    latest_status: Arc<Mutex<Option<(i32, String)>>>,
}

impl SdkScreenBinding {
    pub fn attach(&self, sender: SdkScreenSender) {
        self.senders.lock().unwrap().insert("default".into(), sender);
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        if let Some(sender) = self.sender() {
            for message in pending {
                match message {
                    ScreenMessage::Outputs(outputs) => {
                        let _ = sender.send_outputs(outputs);
                    }
                    ScreenMessage::Status { state, output_id } => {
                        let _ = sender.send_status(state, output_id);
                    }
                }
            }
        }
    }

    pub fn sender(&self) -> Option<SdkScreenSender> {
        self.senders.lock().unwrap().values().next().cloned()
    }

    pub fn attach_device(&self, device_id: String, sender: SdkScreenSender) {
        self.senders.lock().unwrap().insert(device_id, sender.clone());
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        let had_pending_status =
            pending.iter().any(|message| matches!(message, ScreenMessage::Status { .. }));
        for message in pending {
            match message {
                ScreenMessage::Outputs(outputs) => {
                    let _ = sender.send_outputs(outputs);
                }
                ScreenMessage::Status { state, output_id } => {
                    let _ = sender.send_status(state, output_id);
                }
            }
        }
        if !had_pending_status
            && let Some((state, output_id)) = self.latest_status.lock().unwrap().clone()
        {
            let _ = sender.send_status(state, output_id);
        }
    }

    /// Record and deliver the desktop's current state. Recording before the
    /// lookup is important: a disconnect can race a status transition, and a
    /// later reconnect must still receive the newest state.
    pub fn publish_status(&self, state: i32, output_id: String) -> bool {
        *self.latest_status.lock().unwrap() = Some((state, output_id.clone()));
        let Some(sender) = self.sender() else {
            self.defer_status(state, output_id);
            return false;
        };
        sender.send_status(state, output_id)
    }

    pub fn set_broadcaster(&self, broadcaster: Arc<FrameBroadcaster>) {
        *self.broadcaster.lock().unwrap() = Some(broadcaster);
    }

    pub fn sender_for(&self, device_id: &str) -> Option<SdkScreenSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn detach_device(&self, device_id: &str) {
        if let Some(broadcaster) = self.broadcaster.lock().unwrap().clone() {
            broadcaster.unsubscribe(&frame_subscription_id(device_id));
        }
        self.senders.lock().unwrap().remove(device_id);
    }

    /// Bind a negotiated QUIC flow to the screen broadcaster. The subscription
    /// has a small independent queue, so a slow viewer cannot stall capture or
    /// another viewer.
    pub fn attach_frame_flow(&self, device_id: String, flow: DatagramFlow) {
        let Some(sender) = self.sender_for(&device_id) else {
            return;
        };
        if let Some(broadcaster) = self.broadcaster.lock().unwrap().clone() {
            sender.set_keyframe_request_source(broadcaster);
        }
        sender.attach_frame_flow(flow);
        let Some(broadcaster) = self.broadcaster.lock().unwrap().clone() else {
            log::warn!("SDK screen broadcaster is not initialized");
            return;
        };
        let subscription_id = frame_subscription_id(&device_id);
        broadcaster.unsubscribe(&subscription_id);
        let (rx, queue_gap) = broadcaster.subscribe_traced(subscription_id.clone(), 2);
        thread::Builder::new()
            .name(format!("anchor-sdk-screen-frames-{device_id}"))
            .spawn(move || {
                while let Ok(mut frame) = rx.recv() {
                    let dropped_through = queue_gap.swap(0, Ordering::AcqRel);
                    if dropped_through != 0 {
                        // The broadcaster rejected an access unit because
                        // this viewer fell behind. This gap is not visible in
                        // ANFR sequence numbers, so recover before sending
                        // another predictive H.264 frame.
                        sender.mark_frame_gap(dropped_through - 1);
                    }
                    let mut discarded = None;
                    // The screen is replaceable state, not a work queue. If
                    // capture outruns QUIC, discard every buffered older
                    // access unit before sending so a major scene change
                    // cannot be followed by stale desktop content.
                    while let Ok(newer) = rx.try_recv() {
                        discarded = Some(frame.source_frame_id);
                        frame = newer;
                    }
                    // Queue is empty now — the capture loop may resume
                    // encoding if it was holding off for backpressure.
                    broadcaster.clear_backpressure();
                    if let Some(newest_discarded) = discarded {
                        crate::anchorwayland::frame_trace::event!(
                            "broadcaster_coalesce",
                            {
                                "source_frame_id": frame.source_frame_id,
                                "transport": "datagram",
                                "reason": "replaceable_queue_newer_frame",
                            }
                        );
                        // Coalesced access units are real losses for the
                        // decoder: this frame's P-slices reference AUs that
                        // were never sent. Suppress predictive frames until
                        // one recovery IDR heals the chain.
                        sender.mark_frame_gap(newest_discarded);
                    }
                    if !sender.send_frame(&frame) {
                        log::debug!("SDK screen frame dropped for {device_id}");
                    }
                }
                log::debug!("SDK screen frame subscription ended for {device_id}");
            })
            .expect("SDK screen frame thread must start");
    }

    /// Bind a reliable ordered QUIC stream to the screen broadcaster. The
    /// stream carries length-prefixed ANFR packets and therefore has no
    /// datagram loss or reordering at the transport layer.
    pub fn attach_frame_stream(
        &self,
        device_id: String,
        stream_id: u64,
        send: ReliableSendStream,
        handle: tokio::runtime::Handle,
    ) {
        let Some(sender) = self.sender_for(&device_id) else { return };
        if let Some(broadcaster) = self.broadcaster.lock().unwrap().clone() {
            sender.set_keyframe_request_source(broadcaster);
        }
        sender.attach_frame_stream(stream_id, send, handle);
        let Some(broadcaster) = self.broadcaster.lock().unwrap().clone() else {
            log::warn!("SDK screen broadcaster is not initialized");
            return;
        };
        let subscription_id = frame_subscription_id(&device_id);
        broadcaster.unsubscribe(&subscription_id);
        let (rx, queue_gap) = broadcaster.subscribe_traced(subscription_id.clone(), 2);
        thread::Builder::new()
            .name(format!("anchor-sdk-screen-stream-{device_id}"))
            .spawn(move || {
                while let Ok(mut frame) = rx.recv() {
                    let dropped_through = queue_gap.swap(0, Ordering::AcqRel);
                    if dropped_through != 0 {
                        sender.mark_frame_gap(dropped_through - 1);
                    }
                    let mut discarded = None;
                    while let Ok(newer) = rx.try_recv() {
                        discarded = Some(frame.source_frame_id);
                        frame = newer;
                    }
                    broadcaster.clear_backpressure();
                    // This is an ordered, reliable QUIC stream. A sequence
                    // gap is intentional producer-side coalescing, not
                    // transport loss. It still breaks H.264's predictive
                    // reference chain, so hold P-frames until one requested
                    // recovery IDR is available.
                    if let Some(newest_discarded) = discarded {
                        crate::anchorwayland::frame_trace::event!(
                            "broadcaster_coalesce",
                            {
                                "source_frame_id": frame.source_frame_id,
                                "transport": "reliable_stream",
                                "reason": "replaceable_queue_newer_frame",
                            }
                        );
                        sender.mark_frame_gap(newest_discarded);
                    }
                    if !sender.send_frame(&frame) {
                        log::debug!("SDK screen reliable-stream frame dropped for {device_id}");
                    }
                }
                log::debug!("SDK screen stream subscription ended for {device_id}");
            })
            .expect("SDK screen stream thread must start");
    }

    pub fn defer_outputs(&self, outputs: Vec<ScreenOutput>) {
        self.pending.lock().unwrap().push(ScreenMessage::Outputs(outputs));
    }

    pub fn defer_status(&self, state: i32, output_id: String) {
        self.pending.lock().unwrap().push(ScreenMessage::Status { state, output_id });
    }
}

fn frame_subscription_id(device_id: &str) -> String {
    format!("sdk-screen:{device_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_starts_without_a_session() {
        assert!(SdkScreenBinding::default().sender().is_none());
    }

    #[test]
    fn detects_annex_b_idr_nal() {
        assert!(contains_h264_idr(&[0, 0, 0, 1, 0x65, 1, 2]));
        assert!(contains_h264_idr(&[0, 0, 1, 0x67, 1, 0, 0, 1, 0x65, 2]));
        assert!(!contains_h264_idr(&[0, 0, 0, 1, 0x61, 1, 2]));
    }

    #[test]
    fn detects_length_prefixed_idr_nal() {
        assert!(contains_h264_idr(&[0, 0, 0, 2, 0x65, 1]));
        assert!(!contains_h264_idr(&[0, 0, 0, 2, 0x61, 1]));
    }

    #[test]
    fn does_not_treat_malformed_length_prefix_as_idr() {
        // This is Annex-B data with an ordinary predictive NAL followed by
        // bytes that look like an AVC length prefix. The complete AVC parse
        // is invalid, so it must not be classified as a keyframe.
        assert!(!contains_h264_idr(&[0, 0, 0, 1, 0x61, 0, 0, 0, 2, 0x65, 0, 0x99,]));
    }

    #[test]
    fn samples_every_keyframe_regardless_of_phase() {
        // The regression this guards: a 30-frame GOP offset by 15 never lands
        // on a 60-frame periodic sample, so the telemetry reported only
        // predictive frames while multi-megabyte IDRs went unrecorded.
        let gop = 30;
        let offset = 15;
        let keyframes_seen = (0..600u64)
            .filter(|sequence| sequence % gop == offset)
            .filter(|sequence| frame_sampled(*sequence, true))
            .count();
        assert_eq!(keyframes_seen, 20, "every keyframe must be reported");

        // A phase that does align must not report the same frame twice or
        // silently lose the periodic predictive sample.
        assert!(frame_sampled(60, true));
        assert!(frame_sampled(60, false));
    }

    #[test]
    fn samples_predictive_frames_only_periodically() {
        assert!(frame_sampled(0, false));
        assert!(frame_sampled(PERIODIC_SAMPLE_FRAMES, false));
        assert!(!frame_sampled(1, false));
        assert!(!frame_sampled(PERIODIC_SAMPLE_FRAMES - 1, false));
        let sampled = (0..600).filter(|s| frame_sampled(*s, false)).count();
        assert_eq!(sampled, 10, "one predictive sample per 60 frames");
    }

    #[test]
    fn failed_idr_does_not_trigger_an_idr_storm() {
        assert!(!should_schedule_keyframe_recovery(true));
        assert!(should_schedule_keyframe_recovery(false));
    }

    #[test]
    fn reliable_stream_holds_predictive_frames_after_a_gap() {
        assert!(should_drop_predictive_frame(true, false));
        assert!(!should_drop_predictive_frame(true, true));
        assert!(!should_drop_predictive_frame(false, false));
    }

    #[test]
    fn recovery_idr_lost_to_coalescing_is_requested_again() {
        // The deadlock behind GOP-length screen freezes: the recovery IDR is
        // coalesced away in the drain queue, and from then on every P-frame is
        // dropped before sending. No send fails and nothing coalesces, so no
        // gap event ever fires again. The held frames themselves must keep
        // requesting an IDR.
        const FRAME: Duration = Duration::from_micros(16_667);
        let mut recovery = KeyframeRecovery::default();
        let mut now = Instant::now();

        // Predictive frame 10 is rejected by the transport.
        assert!(recovery.admit(false, now).send);
        assert!(recovery.on_send_failed(false, 10, now), "first recovery IDR is requested");

        // The encoder emits that IDR as frame 11, but P-frame 12 lands behind
        // it and the drain keeps only the newest access unit.
        now += FRAME;
        let mut requested = recovery.on_gap(11, now);

        // Capture keeps producing ordinary P-frames. Unless another IDR is
        // requested promptly, the stream stays frozen until the encoder's
        // periodic keyframe, which is seconds away at a long GOP.
        let deadline = now + Duration::from_millis(300);
        while !requested && now < deadline {
            now += FRAME;
            let admission = recovery.admit(false, now);
            assert!(!admission.send, "P-frames stay held until an IDR heals the chain");
            requested |= admission.request_keyframe;
        }
        assert!(requested, "no recovery IDR was requested within 300ms of losing the previous one");
    }

    #[test]
    fn rejected_recovery_idr_keeps_predictive_frames_held_and_retries() {
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.on_gap(10, t0));

        // Recovery IDR 11 is admitted but the full datagram budget rejects it.
        assert!(recovery.admit(true, t0).send);
        assert!(!recovery.on_send_failed(true, 11, t0), "no immediate second keyframe burst");

        // The decoder never got that IDR: P-frames referencing it stay off the
        // wire, and one asks for another IDR once backoff allows.
        assert_eq!(recovery.admit(false, t0), Admission { send: false, request_keyframe: false });
        let retry = t0 + KEYFRAME_RETRY_BACKOFF[0];
        assert_eq!(recovery.admit(false, retry), Admission { send: false, request_keyframe: true });
    }

    #[test]
    fn periodic_idr_rejected_outside_recovery_still_breaks_the_chain() {
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.admit(true, t0).send);
        assert!(!recovery.on_send_failed(true, 500, t0));
        assert_eq!(recovery.admit(false, t0), Admission { send: false, request_keyframe: true });
    }

    #[test]
    fn delivered_idr_ends_recovery_and_withdraws_the_request() {
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.on_gap(10, t0));
        assert!(recovery.admit(true, t0).send);
        assert!(recovery.on_delivered(true, 11), "pending IDR request is withdrawn");
        assert_eq!(recovery.admit(false, t0), Admission { send: true, request_keyframe: false });
        assert!(!recovery.on_delivered(false, 12), "a P-frame never withdraws a request");
    }

    #[test]
    fn idr_encoded_before_a_later_drop_does_not_heal_it() {
        // The queue holds IDR 11 when P-frame 12 is dropped at publish. IDR 11
        // is delivered, but P-frame 13 references 12, which the decoder never
        // got: recovery must continue and the request must stand.
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.on_gap(12, t0));
        assert!(recovery.admit(true, t0).send);
        assert!(!recovery.on_delivered(true, 11), "an older IDR must not withdraw the request");
        let retry = t0 + KEYFRAME_RETRY_BACKOFF[0];
        assert_eq!(recovery.admit(false, retry), Admission { send: false, request_keyframe: true });

        // A newer IDR does heal it.
        assert!(recovery.on_delivered(true, 14));
        assert!(recovery.admit(false, retry).send);
    }

    #[test]
    fn failed_recoveries_back_off_without_dropping_the_request() {
        // A viewer whose connection has stalled never heals. Its requests must
        // slow down along the backoff schedule, yet never stop entirely.
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.on_send_failed(false, 1, t0));

        let requested_at: Vec<u64> = (1..=6000)
            .filter(|ms| recovery.admit(false, t0 + Duration::from_millis(*ms)).request_keyframe)
            .collect();
        assert_eq!(requested_at, vec![250, 750, 1750, 3750, 5750]);
    }

    #[test]
    fn healing_idr_resets_backoff() {
        let t0 = Instant::now();
        let mut recovery = KeyframeRecovery::default();
        assert!(recovery.on_send_failed(false, 1, t0));
        let late = t0 + Duration::from_millis(1750);
        for ms in [250, 750, 1750] {
            assert!(recovery.admit(false, t0 + Duration::from_millis(ms)).request_keyframe);
        }

        // Recovery succeeds, then a new loss happens right away: the first
        // request after a healthy stream is immediate again.
        assert!(recovery.on_delivered(true, 5));
        assert!(recovery.on_gap(6, late), "backoff was reset by the healing IDR");
    }

    #[test]
    fn smaller_peer_limit_disables_fixed_size_parity_but_keeps_media_usable() {
        let smaller = video_frame::FRAME_DATAGRAM_BYTES - 1;
        assert_eq!(negotiated_screen_datagram_limit(Some(smaller)), Some(smaller));
        assert!(!can_use_fixed_size_parity(true, smaller));
        assert!(can_use_fixed_size_parity(true, video_frame::FRAME_DATAGRAM_BYTES));
        assert_eq!(negotiated_screen_datagram_limit(Some(video_frame::FRAME_HEADER_BYTES)), None);
        assert_eq!(negotiated_screen_datagram_limit(None), None);
    }
}

#[cfg(test)]
#[path = "sdk_screen_recovery_sim.rs"]
mod recovery_sim;
