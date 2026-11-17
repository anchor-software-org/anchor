//! Discrete-time model of the screen pipeline, used to compare sender-side IDR
//! recovery policies under loss, congestion and a stalled viewer.
//!
//! Real code: `FrameBroadcaster` (per-viewer queue, gap marker, backpressure,
//! the shared `needs_keyframe` flag) and `KeyframeRecovery`. Modelled from the
//! production constants: the capture loop's IDR limiter and GOP, the drain
//! loop's coalescing and pacing, SDK datagram admission, a bandwidth-limited
//! link with XOR parity, and the receiver's assembler and re-request cadence.
//!
//! Run with: `cargo test --lib recovery_sim -- --ignored --nocapture`

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::{Admission, KeyframeRecovery};
use crate::anchorapp::device::{BroadcastFrame, FrameBroadcaster};

const FRAME_MS: f64 = 1000.0 / 60.0;
/// `wayland_plugin.rs` `MIN_IDR_INTERVAL`.
const MIN_IDR_INTERVAL_MS: u64 = 250;
/// `ScreenFrameAssembler.ASSEMBLY_TIMEOUT_NS` / `AnchorVideoFrame`.
const ASSEMBLY_TIMEOUT_MS: u64 = 250;
/// `VideoPlugin.KEYFRAME_REQUEST_MIN_INTERVAL_NS`.
const RECEIVER_REQUEST_INTERVAL_MS: u64 = 300;
/// `sdk_screen.rs` `MAX_PACING_WAIT`.
const MAX_PACING_WAIT_MS: f64 = 8.0;
/// `session.rs` staleness bound: backlog beyond this much drain time is stale.
const STALE_QUEUE_MS: f64 = 40.0;
const STALE_QUEUE_MIN_BYTES: f64 = 32.0 * 1024.0;
const STALE_QUEUE_MAX_BYTES: f64 = 256.0 * 1024.0;
const DATAGRAM_BYTES: usize = 1100;
/// One XOR parity datagram per group of this many media datagrams (~6%).
const PARITY_GROUP: usize = 16;
const ONE_WAY_DELAY_MS: u64 = 5;
/// A display gap longer than this counts as a visible freeze.
const FREEZE_MS: u64 = 200;

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Sender recovery decisions, in the shape `send_frame` and the drain loop use
/// them. Every method returns whether the caller raises (or, for
/// `on_delivered`, withdraws) the broadcaster's keyframe request.
trait Policy {
    fn on_gap(&mut self, newest_lost: u64, now: u64) -> bool;
    fn admit(&mut self, keyframe: bool, now: u64) -> Admission;
    fn on_delivered(&mut self, keyframe: bool, id: u64, now: u64) -> bool;
    fn on_send_failed(&mut self, keyframe: bool, id: u64, now: u64) -> bool;
}

/// `main` before this branch: a 500ms limiter that drops requests, recovery
/// cleared when an IDR is admitted, and no retry after a failed IDR.
#[derive(Default)]
struct OldPolicy {
    waiting: bool,
    last_request: Option<u64>,
}

impl OldPolicy {
    fn request_due(&mut self, now: u64) -> bool {
        let allowed = self.last_request.is_none_or(|previous| now - previous >= 500);
        if allowed {
            self.last_request = Some(now);
        }
        allowed
    }
}

impl Policy for OldPolicy {
    fn on_gap(&mut self, _newest_lost: u64, now: u64) -> bool {
        self.waiting = true;
        self.request_due(now)
    }

    fn admit(&mut self, keyframe: bool, _now: u64) -> Admission {
        if self.waiting && !keyframe {
            return Admission { send: false, request_keyframe: false };
        }
        if keyframe {
            self.waiting = false;
        }
        Admission { send: true, request_keyframe: false }
    }

    fn on_delivered(&mut self, _keyframe: bool, _id: u64, _now: u64) -> bool {
        false
    }

    fn on_send_failed(&mut self, keyframe: bool, id: u64, now: u64) -> bool {
        !keyframe && self.on_gap(id, now)
    }
}

/// This branch: the real `KeyframeRecovery`, including its backoff. The
/// simulation clock is milliseconds since `start`.
struct CurrentPolicy {
    recovery: KeyframeRecovery,
    start: Instant,
}

impl CurrentPolicy {
    fn at(&self, now: u64) -> Instant {
        self.start + Duration::from_millis(now)
    }
}

impl Policy for CurrentPolicy {
    fn on_gap(&mut self, newest_lost: u64, now: u64) -> bool {
        let now = self.at(now);
        self.recovery.on_gap(newest_lost, now)
    }

    fn admit(&mut self, keyframe: bool, now: u64) -> Admission {
        let now = self.at(now);
        self.recovery.admit(keyframe, now)
    }

    fn on_delivered(&mut self, keyframe: bool, id: u64, _now: u64) -> bool {
        self.recovery.on_delivered(keyframe, id)
    }

    fn on_send_failed(&mut self, keyframe: bool, id: u64, now: u64) -> bool {
        let now = self.at(now);
        self.recovery.on_send_failed(keyframe, id, now)
    }
}

#[derive(Clone, Copy)]
enum PolicyKind {
    Old,
    Current,
}

impl PolicyKind {
    fn name(self) -> &'static str {
        match self {
            PolicyKind::Old => "main (old)",
            PolicyKind::Current => "this branch",
        }
    }

    fn build(self) -> Box<dyn Policy> {
        match self {
            PolicyKind::Old => Box::<OldPolicy>::default(),
            PolicyKind::Current => Box::new(CurrentPolicy {
                recovery: KeyframeRecovery::default(),
                start: Instant::now(),
            }),
        }
    }
}

struct InFlight {
    id: u64,
    seq: u64,
    keyframe: bool,
    remaining: f64,
    datagrams: usize,
    started: Option<u64>,
}

struct Arrival {
    id: u64,
    seq: u64,
    keyframe: bool,
    first_byte: u64,
    done: u64,
    lost: bool,
}

/// Bottleneck link plus Quinn's datagram queue, drained at the path rate.
struct Link {
    queue: VecDeque<InFlight>,
    queued: f64,
    loss: f64,
}

impl Link {
    /// `Session::datagram_admission_denied`, with the path rate standing in
    /// for Quinn's cwnd/RTT estimate.
    fn admits(&self, rate: f64) -> bool {
        let bound = (rate * STALE_QUEUE_MS).clamp(STALE_QUEUE_MIN_BYTES, STALE_QUEUE_MAX_BYTES);
        self.queued <= bound
    }

    fn tick(&mut self, now: u64, rate: f64, rng: &mut Rng) -> Vec<Arrival> {
        let mut budget = rate;
        let mut arrivals = Vec::new();
        while budget > 0.0 {
            let Some(front) = self.queue.front_mut() else { break };
            front.started.get_or_insert(now);
            let sent = front.remaining.min(budget);
            front.remaining -= sent;
            self.queued -= sent;
            budget -= sent;
            if front.remaining > 0.0 {
                break;
            }
            let frame = self.queue.pop_front().unwrap();
            arrivals.push(Arrival {
                id: frame.id,
                seq: frame.seq,
                keyframe: frame.keyframe,
                first_byte: frame.started.unwrap() + ONE_WAY_DELAY_MS,
                done: now + ONE_WAY_DELAY_MS,
                lost: frame_lost(frame.datagrams, self.loss, rng),
            });
        }
        arrivals
    }
}

/// XOR parity rebuilds one lost datagram per group; two losses in a group
/// lose the whole access unit.
fn frame_lost(datagrams: usize, loss: f64, rng: &mut Rng) -> bool {
    if loss == 0.0 {
        return false;
    }
    let mut remaining = datagrams;
    while remaining > 0 {
        let group = remaining.min(PARITY_GROUP);
        let lost = (0..=group).filter(|_| rng.next_f64() < loss).count();
        if lost >= 2 {
            return true;
        }
        remaining -= group;
    }
    false
}

/// Phone-side assembler and decoder, with ground truth for corruption.
#[derive(Default)]
struct Receiver_ {
    in_progress: VecDeque<Arrival>,
    awaiting_idr: bool,
    last_seq: Option<u64>,
    decoded_through: Option<u64>,
    last_request: Option<u64>,
    displays: Vec<u64>,
    corrupt: u64,
}

impl Receiver_ {
    /// Returns true when the receiver sends a keyframe request this tick. Like
    /// the real receive loop, it only runs when datagrams are arriving.
    fn tick(&mut self, now: u64) -> bool {
        let receiving = self.in_progress.iter().any(|frame| frame.first_byte <= now);
        while self.in_progress.front().is_some_and(|frame| frame.done <= now) {
            let frame = self.in_progress.pop_front().unwrap();
            self.complete(frame);
        }
        let due = self.last_request.is_none_or(|at| now - at >= RECEIVER_REQUEST_INTERVAL_MS);
        if receiving && self.awaiting_idr && due {
            self.last_request = Some(now);
            return true;
        }
        false
    }

    fn complete(&mut self, frame: Arrival) {
        if frame.lost || frame.done - frame.first_byte > ASSEMBLY_TIMEOUT_MS {
            self.awaiting_idr = true;
            return;
        }
        if self.last_seq.is_some_and(|last| frame.seq != last + 1) {
            self.awaiting_idr = true;
        }
        self.last_seq = Some(frame.seq);
        if frame.keyframe {
            self.awaiting_idr = false;
            self.decoded_through = Some(frame.id);
            self.displays.push(frame.done);
        } else if self.awaiting_idr {
            // Held until an IDR, exactly as the assembler does.
        } else if frame.id > 0 && self.decoded_through == Some(frame.id - 1) {
            self.decoded_through = Some(frame.id);
            self.displays.push(frame.done);
        } else {
            // The receiver saw no gap, but the reference chain is broken:
            // this P-frame decodes as garbage.
            self.corrupt += 1;
            self.decoded_through = None;
        }
    }
}

struct ViewerConfig {
    /// Path rate in bytes per millisecond at a given time.
    rate: fn(u64) -> f64,
    loss: f64,
}

struct Scenario {
    name: &'static str,
    duration_ms: u64,
    gop: u32,
    p_bytes: usize,
    idr_bytes: usize,
    /// Encoder bitrate used for pacing (`pacing_utilization_percent` = 90).
    bitrate_bps: f64,
    /// Chance per drain iteration of an OS scheduling stall, and its length.
    drain_stall: (f64, u64),
    viewers: Vec<ViewerConfig>,
}

struct Viewer {
    config: ViewerConfig,
    rx: Receiver<Arc<BroadcastFrame>>,
    gap: Arc<AtomicU64>,
    policy: Box<dyn Policy>,
    link: Link,
    receiver: Receiver_,
    busy_until: u64,
    sequence: u64,
    send_failures: u64,
}

struct ViewerResult {
    longest_freeze_ms: u64,
    frozen_ms: u64,
    good_fps: f64,
    corrupt: u64,
    send_failures: u64,
}

struct RunResult {
    idrs: u64,
    forced_idrs: u64,
    skipped_encodes: u64,
    viewers: Vec<ViewerResult>,
}

fn encode(id: u64, keyframe: bool, bytes: usize) -> Vec<u8> {
    let mut payload = id.to_le_bytes().to_vec();
    payload.push(u8::from(keyframe));
    payload.extend_from_slice(&(bytes as u64).to_le_bytes());
    payload
}

fn decode(payload: &[u8]) -> (u64, bool, usize) {
    let id = u64::from_le_bytes(payload[..8].try_into().unwrap());
    let bytes = u64::from_le_bytes(payload[9..17].try_into().unwrap()) as usize;
    (id, payload[8] == 1, bytes)
}

fn run(scenario: &Scenario, kind: PolicyKind, seed: u64) -> RunResult {
    let mut rng = Rng(seed);
    let broadcaster = FrameBroadcaster::new();
    let mut viewers: Vec<Viewer> = scenario
        .viewers
        .iter()
        .enumerate()
        .map(|(index, config)| {
            let (rx, gap) = broadcaster.subscribe_traced(format!("sim-{index}"), 2);
            Viewer {
                config: ViewerConfig { rate: config.rate, loss: config.loss },
                rx,
                gap,
                policy: kind.build(),
                link: Link { queue: VecDeque::new(), queued: 0.0, loss: config.loss },
                receiver: Receiver_::default(),
                busy_until: 0,
                sequence: 0,
                send_failures: 0,
            }
        })
        .collect();

    let pacing_bytes_per_ms = scenario.bitrate_bps * 0.9 / 8.0 / 1000.0;
    let mut next_frame_at = 0.0f64;
    let mut next_id = 0u64;
    let mut since_idr = u32::MAX;
    let mut force_idr = false;
    let mut last_forced: Option<u64> = None;
    let mut phone_requests: VecDeque<u64> = VecDeque::new();
    let (mut idrs, mut forced_idrs, mut skipped_encodes) = (0, 0, 0);
    let idr_due =
        |last: Option<u64>, now: u64| last.is_none_or(|at| now - at >= MIN_IDR_INTERVAL_MS);

    for now in 0..scenario.duration_ms {
        // Receiver keyframe requests reach the capture loop one way later and
        // are dropped when the IDR limiter is not due (wayland_plugin.rs).
        while phone_requests.front().is_some_and(|at| *at <= now) {
            phone_requests.pop_front();
            if idr_due(last_forced, now) {
                last_forced = Some(now);
                force_idr = true;
            }
        }

        // Capture loop.
        if now as f64 >= next_frame_at {
            next_frame_at += FRAME_MS;
            if broadcaster.has_backpressure() {
                skipped_encodes += 1;
            } else {
                if broadcaster.needs_keyframe.load(Ordering::Relaxed) && idr_due(last_forced, now) {
                    broadcaster.take_keyframe_request();
                    last_forced = Some(now);
                    force_idr = true;
                }
                let keyframe = force_idr || since_idr >= scenario.gop;
                if keyframe {
                    idrs += 1;
                    forced_idrs += u64::from(force_idr);
                    force_idr = false;
                    since_idr = 1;
                } else {
                    since_idr += 1;
                }
                let bytes = if keyframe { scenario.idr_bytes } else { scenario.p_bytes };
                broadcaster.publish_frame(next_id, encode(next_id, keyframe, bytes));
                next_id += 1;
            }
        }

        for viewer in &mut viewers {
            let rate = (viewer.config.rate)(now);

            // Drain thread: gap marker, coalescing, then send_frame.
            if now >= viewer.busy_until
                && let Ok(mut frame) = viewer.rx.try_recv()
            {
                let mut raise = false;
                let dropped_through = viewer.gap.swap(0, Ordering::AcqRel);
                if dropped_through != 0 {
                    raise |= viewer.policy.on_gap(dropped_through - 1, now);
                }
                let mut discarded = None;
                while let Ok(newer) = viewer.rx.try_recv() {
                    discarded = Some(frame.source_frame_id);
                    frame = newer;
                }
                broadcaster.clear_backpressure();
                if let Some(newest_discarded) = discarded {
                    raise |= viewer.policy.on_gap(newest_discarded, now);
                }

                let (id, keyframe, bytes) = decode(&frame);
                let sequence = viewer.sequence;
                viewer.sequence += 1;
                let admission = viewer.policy.admit(keyframe, now);
                raise |= admission.request_keyframe;
                let mut withdraw = false;
                let mut busy_ms = 0.0;
                if admission.send {
                    let datagrams = bytes.div_ceil(DATAGRAM_BYTES);
                    let wire = (datagrams + datagrams.div_ceil(PARITY_GROUP)) * DATAGRAM_BYTES;
                    if viewer.link.admits(rate) {
                        viewer.link.queue.push_back(InFlight {
                            id,
                            seq: sequence,
                            keyframe,
                            remaining: wire as f64,
                            datagrams,
                            started: None,
                        });
                        viewer.link.queued += wire as f64;
                        withdraw = viewer.policy.on_delivered(keyframe, id, now);
                    } else {
                        viewer.send_failures += 1;
                        raise |= viewer.policy.on_send_failed(keyframe, id, now);
                    }
                    busy_ms = (wire as f64 / pacing_bytes_per_ms).min(MAX_PACING_WAIT_MS);
                }
                if rng.next_f64() < scenario.drain_stall.0 {
                    busy_ms += scenario.drain_stall.1 as f64;
                }
                viewer.busy_until = now + busy_ms.ceil() as u64;
                if withdraw {
                    broadcaster.needs_keyframe.store(false, Ordering::Relaxed);
                }
                if raise {
                    broadcaster.needs_keyframe.store(true, Ordering::Relaxed);
                }
            }

            for arrival in viewer.link.tick(now, rate, &mut rng) {
                viewer.receiver.in_progress.push_back(arrival);
            }
            if viewer.receiver.tick(now) {
                phone_requests.push_back(now + ONE_WAY_DELAY_MS);
            }
        }
    }

    let duration = scenario.duration_ms;
    RunResult {
        idrs,
        forced_idrs,
        skipped_encodes,
        viewers: viewers
            .iter()
            .map(|viewer| {
                let displays = &viewer.receiver.displays;
                let mut edges = vec![0];
                edges.extend(displays.iter().copied());
                edges.push(duration);
                let gaps: Vec<u64> = edges.windows(2).map(|pair| pair[1] - pair[0]).collect();
                ViewerResult {
                    longest_freeze_ms: gaps.iter().copied().max().unwrap_or(duration),
                    frozen_ms: gaps.iter().filter(|gap| **gap > FREEZE_MS).sum(),
                    good_fps: displays.len() as f64 * 1000.0 / duration as f64,
                    corrupt: viewer.receiver.corrupt,
                    send_failures: viewer.send_failures,
                }
            })
            .collect(),
    }
}

const MBPS: f64 = 1_000_000.0 / 8.0 / 1000.0;

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "healthy link (baseline)",
            duration_ms: 20_000,
            gop: 240,
            p_bytes: 25_000,
            idr_bytes: 150_000,
            bitrate_bps: 25e6,
            drain_stall: (0.01, 20),
            viewers: vec![ViewerConfig { rate: |_| 40.0 * MBPS, loss: 0.001 }],
        },
        Scenario {
            name: "500ms congestion burst, GOP 500 (the reported freeze)",
            duration_ms: 20_000,
            gop: 500,
            p_bytes: 25_000,
            idr_bytes: 150_000,
            bitrate_bps: 25e6,
            drain_stall: (0.02, 25),
            viewers: vec![ViewerConfig {
                rate: |t| if (3_000..3_500).contains(&t) { 2.0 * MBPS } else { 40.0 * MBPS },
                loss: 0.001,
            }],
        },
        Scenario {
            name: "15s sustained congestion (12 Mbps), IDR capped at 150KB",
            duration_ms: 20_000,
            gop: 240,
            p_bytes: 30_000,
            idr_bytes: 150_000,
            bitrate_bps: 25e6,
            drain_stall: (0.01, 20),
            viewers: vec![ViewerConfig {
                rate: |t| if (2_000..17_000).contains(&t) { 12.0 * MBPS } else { 40.0 * MBPS },
                loss: 0.005,
            }],
        },
        Scenario {
            name: "15s sustained congestion (12 Mbps), uncapped 1.3MB IDR",
            duration_ms: 20_000,
            gop: 240,
            p_bytes: 30_000,
            idr_bytes: 1_300_000,
            bitrate_bps: 25e6,
            drain_stall: (0.01, 20),
            viewers: vec![ViewerConfig {
                rate: |t| if (2_000..17_000).contains(&t) { 12.0 * MBPS } else { 40.0 * MBPS },
                loss: 0.005,
            }],
        },
        Scenario {
            name: "healthy viewer + second viewer stalls at 2s",
            duration_ms: 20_000,
            gop: 240,
            p_bytes: 25_000,
            idr_bytes: 150_000,
            bitrate_bps: 25e6,
            drain_stall: (0.01, 20),
            viewers: vec![
                ViewerConfig { rate: |_| 40.0 * MBPS, loss: 0.001 },
                ViewerConfig { rate: |t| if t < 2_000 { 40.0 * MBPS } else { 0.0 }, loss: 0.0 },
            ],
        },
    ]
}

#[test]
#[ignore = "simulation report; run explicitly with --ignored --nocapture"]
fn compare_recovery_policies() {
    const SEEDS: [u64; 5] =
        [0x9e37_79b9_7f4a_7c15, 0xdead_beef, 0x1234_5678, 0xabcd_ef01, 0x5555_aaaa];
    for scenario in scenarios() {
        println!("\n## {}", scenario.name);
        println!(
            "{:<14} {:>8} {:>9} {:>12} {:>10} {:>9} {:>8} {:>9}",
            "policy",
            "IDRs/s",
            "forced/s",
            "max freeze",
            "frozen",
            "good fps",
            "corrupt",
            "send fail"
        );
        for kind in [PolicyKind::Old, PolicyKind::Current] {
            let runs: Vec<RunResult> =
                SEEDS.iter().map(|seed| run(&scenario, kind, *seed)).collect();
            let seconds = scenario.duration_ms as f64 / 1000.0;
            let n = runs.len() as f64;
            let idrs = runs.iter().map(|r| r.idrs as f64).sum::<f64>() / n / seconds;
            let forced = runs.iter().map(|r| r.forced_idrs as f64).sum::<f64>() / n / seconds;
            // Viewer 0 is the one we care about; it is the only viewer, or the
            // healthy one next to a stalled one.
            let worst = runs.iter().map(|r| r.viewers[0].longest_freeze_ms).max().unwrap();
            let frozen = runs.iter().map(|r| r.viewers[0].frozen_ms as f64).sum::<f64>() / n;
            let fps = runs.iter().map(|r| r.viewers[0].good_fps).sum::<f64>() / n;
            let corrupt = runs.iter().map(|r| r.viewers[0].corrupt as f64).sum::<f64>() / n;
            let failures = runs.iter().map(|r| r.viewers[0].send_failures as f64).sum::<f64>() / n;
            let _skipped = runs.iter().map(|r| r.skipped_encodes).sum::<u64>();
            println!(
                "{:<14} {:>8.2} {:>9.2} {:>10}ms {:>8.0}ms {:>9.1} {:>8.1} {:>9.0}",
                kind.name(),
                idrs,
                forced,
                worst,
                frozen,
                fps,
                corrupt,
                failures
            );
        }
    }
}

const REGRESSION_SEEDS: [u64; 3] = [0x9e37_79b9_7f4a_7c15, 0xdead_beef, 0x1234_5678];

fn scenario(name_prefix: &str) -> Scenario {
    scenarios()
        .into_iter()
        .find(|scenario| scenario.name.starts_with(name_prefix))
        .expect("scenario exists")
}

#[test]
fn congestion_burst_recovers_well_before_the_periodic_keyframe() {
    let scenario = scenario("500ms congestion burst");
    for seed in REGRESSION_SEEDS {
        // The model must reproduce the reported freeze on the old policy,
        // otherwise the assertion below proves nothing.
        let old = run(&scenario, PolicyKind::Old, seed);
        assert!(old.viewers[0].longest_freeze_ms > 4_000, "seed {seed:#x}: model lost the bug");

        let current = run(&scenario, PolicyKind::Current, seed);
        let freeze = current.viewers[0].longest_freeze_ms;
        assert!(freeze < 1_000, "seed {seed:#x}: froze for {freeze}ms");
    }
}

#[test]
fn stalled_viewer_does_not_flood_a_healthy_viewer_with_keyframes() {
    let scenario = scenario("healthy viewer + second viewer stalls");
    let seconds = scenario.duration_ms as f64 / 1000.0;
    for seed in REGRESSION_SEEDS {
        let result = run(&scenario, PolicyKind::Current, seed);
        let idrs_per_second = result.idrs as f64 / seconds;
        assert!(idrs_per_second < 1.0, "seed {seed:#x}: {idrs_per_second:.2} IDRs/s");
        let freeze = result.viewers[0].longest_freeze_ms;
        assert!(freeze < 150, "seed {seed:#x}: healthy viewer froze for {freeze}ms");
    }
}
