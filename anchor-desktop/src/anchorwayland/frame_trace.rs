//! Per-frame timing trace ring and percentile stats.
//!
//! `TraceRing` is a lock-protected 256-entry ring buffer indexed by frame_id.
//! The capture thread records desktop-side stage markers as a frame flows
//! through capture, encode, and handoff to the broadcaster. Transport events
//! are emitted separately by the Sideboat sender with the same source ID.
//!
//! The ring is indexed by `(frame_id % RING_SIZE)`. If a stale entry is
//! overwritten before the phone echoes its timings, its phone markers are
//! simply lost — acceptable for a stats collector.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

static TRACE_ENABLED: OnceLock<bool> = OnceLock::new();
static RUN_ID: OnceLock<String> = OnceLock::new();

/// Structured Sideboat tracing is opt-in because it emits one small JSON event
/// per pipeline transition. Enable it before starting the desktop process with
/// `ANCHOR_SIDEBOAT_TRACE=1`.
pub fn enabled() -> bool {
    *TRACE_ENABLED.get_or_init(|| {
        std::env::var("ANCHOR_SIDEBOAT_TRACE")
            .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    })
}

/// Identifier shared by every Sideboat event from this process. `anchor.log`
/// is append-only, so this lets operators select one run without trusting file
/// position or timestamps from an earlier process.
pub fn run_id() -> &'static str {
    RUN_ID.get_or_init(|| format!("{}-{:x}", std::process::id(), mono_ns()))
}

/// Emit the one event that identifies a Sideboat run and its source build.
pub fn announce_run() {
    log::info!(
        target: "anchor::sideboat_trace",
        "[SIDEBOAT_RUN_START] run_id={} build={} trace_enabled={} pid={}",
        run_id(),
        env!("GIT_HASH"),
        enabled(),
        std::process::id(),
    );
}

/// Emit an opt-in JSON event without paying for it when tracing is off.
/// The field object is only evaluated when `ANCHOR_SIDEBOAT_TRACE` is set, so
/// a disabled run costs one branch instead of building a `serde_json::Value`
/// (with its map and string allocations) on every pipeline transition.
macro_rules! event {
    ($kind:expr, $($fields:tt)*) => {
        if $crate::anchorwayland::frame_trace::enabled() {
            $crate::anchorwayland::frame_trace::emit($kind, serde_json::json!($($fields)*));
        }
    };
}
pub(crate) use event;

/// Emit an opt-in JSON event. Callers provide an object containing stage data;
/// the common run/build/time fields are added here so every event is joinable.
/// Prefer the `event!` macro so the value is not built when tracing is off.
pub fn emit(kind: &str, mut fields: serde_json::Value) {
    if !enabled() {
        return;
    }
    let Some(object) = fields.as_object_mut() else {
        return;
    };
    object.insert("event".into(), serde_json::json!(kind));
    object.insert("run_id".into(), serde_json::json!(run_id()));
    object.insert("build".into(), serde_json::json!(env!("GIT_HASH")));
    object.insert("mono_ns".into(), serde_json::json!(mono_ns()));
    log::info!(target: "anchor::sideboat_trace", "[SIDEBOAT_TRACE] {}", fields);
}

/// Ring size. At 60fps this holds ~4s of history.
pub const RING_SIZE: usize = 256;

/// Process-start Instant used to convert to monotonic nanoseconds.
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Monotonic nanoseconds since process start. Cheap — just a subtract.
pub fn mono_ns() -> u64 {
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos() as u64
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameTrace {
    pub frame_id: u64,
    pub t_capture_start_ns: u64,
    pub t_capture_ready_ns: u64,
    pub t_encode_start_ns: u64,
    pub t_encode_done_ns: u64,
    /// When the frame was pushed into the broadcaster channel.
    pub t_queue_ns: u64,
    /// When transport submission completed in the sender thread. This is not a
    /// delivery acknowledgement or proof that bytes reached the peer.
    pub t_wire_ns: u64,
    pub encoded_bytes: u32,

    // Encode sub-stage durations (microseconds, from DmabufEncodeTiming)
    pub enc_dup_fd_us: u32,
    pub enc_hwframe_map_us: u32,
    pub enc_filter_in_us: u32,
    pub enc_filter_out_us: u32,
    pub enc_send_frame_us: u32,
    pub enc_recv_packets_us: u32,
}

impl FrameTrace {
    /// Capture wait: begin → capture_ready (includes compositor vsync block).
    pub fn capture_ns(&self) -> Option<u64> {
        (self.t_capture_ready_ns > self.t_capture_start_ns)
            .then(|| self.t_capture_ready_ns - self.t_capture_start_ns)
    }

    /// Encode total: encode_start → encode_done.
    pub fn encode_ns(&self) -> Option<u64> {
        (self.t_encode_done_ns > self.t_encode_start_ns)
            .then(|| self.t_encode_done_ns - self.t_encode_start_ns)
    }

    /// Queue wait: encode_done → wire (time sitting in channel + TCP write).
    /// Falls back to queue_ns if wire hasn't been recorded yet.
    pub fn network_ns(&self) -> Option<u64> {
        if self.t_wire_ns > self.t_encode_done_ns {
            Some(self.t_wire_ns - self.t_encode_done_ns)
        } else if self.t_queue_ns > self.t_encode_done_ns {
            Some(self.t_queue_ns - self.t_encode_done_ns)
        } else {
            None
        }
    }

    /// Desktop processing: capture_ready → queue (excludes vsync wait).
    pub fn desktop_total_ns(&self) -> Option<u64> {
        let end = if self.t_queue_ns > 0 {
            self.t_queue_ns
        } else {
            return None;
        };
        (end > self.t_capture_ready_ns).then(|| end - self.t_capture_ready_ns)
    }

    /// Produce a compact JSON log line with all timing stages.
    /// Zero/missing fields are omitted to keep lines short.
    pub fn to_json_log(&self) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(256);
        s.push_str(r#"{"t":"frame_trace","run_id":""#);
        s.push_str(run_id());
        s.push('"');
        write!(s, r#","fid":{}"#, self.frame_id).ok();

        // Capture: split into vsync wait vs actual copy isn't possible with
        // current markers, so we report capture_start→capture_ready as one.
        if let Some(ns) = self.capture_ns() {
            write!(s, r#","cap_us":{}"#, ns / 1000).ok();
        }

        // Gap between capture_ready and encode_start (scheduling/fps-limit overhead)
        if self.t_encode_start_ns > self.t_capture_ready_ns {
            let gap = (self.t_encode_start_ns - self.t_capture_ready_ns) / 1000;
            if gap > 0 {
                write!(s, r#","pre_enc_us":{}"#, gap).ok();
            }
        }

        // Encode sub-stages (only non-zero)
        if self.enc_dup_fd_us > 0 {
            write!(s, r#","enc_dup_us":{}"#, self.enc_dup_fd_us).ok();
        }
        if self.enc_hwframe_map_us > 0 {
            write!(s, r#","enc_map_us":{}"#, self.enc_hwframe_map_us).ok();
        }
        if self.enc_filter_in_us > 0 {
            write!(s, r#","enc_fin_us":{}"#, self.enc_filter_in_us).ok();
        }
        if self.enc_filter_out_us > 0 {
            write!(s, r#","enc_fout_us":{}"#, self.enc_filter_out_us).ok();
        }
        if self.enc_send_frame_us > 0 {
            write!(s, r#","enc_send_us":{}"#, self.enc_send_frame_us).ok();
        }
        if self.enc_recv_packets_us > 0 {
            write!(s, r#","enc_recv_us":{}"#, self.enc_recv_packets_us).ok();
        }

        // Encode total
        if let Some(ns) = self.encode_ns() {
            write!(s, r#","enc_us":{}"#, ns / 1000).ok();
        }

        // Queue + network
        if self.t_queue_ns > self.t_encode_done_ns {
            write!(s, r#","queue_us":{}"#, (self.t_queue_ns - self.t_encode_done_ns) / 1000).ok();
        }
        if self.t_wire_ns > self.t_queue_ns && self.t_queue_ns > 0 {
            write!(s, r#","wire_us":{}"#, (self.t_wire_ns - self.t_queue_ns) / 1000).ok();
        }

        // Desktop total (capture_ready → queue, excludes vsync)
        if let Some(ns) = self.desktop_total_ns() {
            write!(s, r#","desktop_us":{}"#, ns / 1000).ok();
        }

        if self.encoded_bytes > 0 {
            write!(s, r#","bytes":{}"#, self.encoded_bytes).ok();
        }

        s.push('}');
        s
    }
}

/// Encoder sub-stage durations passed to `TraceRing::record_encode_substages`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeSubstages {
    pub dup_fd_us: u32,
    pub hwframe_map_us: u32,
    pub filter_in_us: u32,
    pub filter_out_us: u32,
    pub send_frame_us: u32,
    pub recv_packets_us: u32,
}

pub struct TraceRing {
    entries: Mutex<Box<[FrameTrace]>>,
}

impl Default for TraceRing {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceRing {
    pub fn new() -> Self {
        let _ = EPOCH.get_or_init(Instant::now);
        Self { entries: Mutex::new(vec![FrameTrace::default(); RING_SIZE].into_boxed_slice()) }
    }

    fn slot(frame_id: u64) -> usize {
        (frame_id as usize) % RING_SIZE
    }

    /// Start a new trace: zeros the slot and marks capture_start.
    pub fn begin(&self, frame_id: u64) {
        let mut e = self.entries.lock().unwrap();
        e[Self::slot(frame_id)] =
            FrameTrace { frame_id, t_capture_start_ns: mono_ns(), ..Default::default() };
        event!("capture_start", { "source_frame_id": frame_id });
    }

    /// Mark capture ready (frame mapped / available).
    pub fn mark_capture_ready(&self, frame_id: u64) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.t_capture_ready_ns = mono_ns();
            event!("capture_ready", { "source_frame_id": frame_id });
        }
    }

    pub fn mark_encode_start(&self, frame_id: u64) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.t_encode_start_ns = mono_ns();
            event!("encode_start", { "source_frame_id": frame_id });
        }
    }

    pub fn mark_encode_done(&self, frame_id: u64, bytes: usize) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.t_encode_done_ns = mono_ns();
            slot.encoded_bytes = bytes as u32;
            event!(
                "encode_done",
                { "source_frame_id": frame_id, "encoded_bytes": bytes }
            );
        }
    }

    /// Mark frame as queued into the broadcaster channel.
    pub fn mark_queued(&self, frame_id: u64) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.t_queue_ns = mono_ns();
            event!("capture_loop_done", { "source_frame_id": frame_id });
        }
    }

    /// Mark frame as written to TCP (called from drain thread).
    pub fn mark_wire(&self, frame_id: u64) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.t_wire_ns = mono_ns();
        }
    }

    /// Record encoder sub-stage durations (from DmabufEncodeTiming).
    pub fn record_encode_substages(&self, frame_id: u64, s: EncodeSubstages) {
        let mut e = self.entries.lock().unwrap();
        let slot = &mut e[Self::slot(frame_id)];
        if slot.frame_id == frame_id {
            slot.enc_dup_fd_us = s.dup_fd_us;
            slot.enc_hwframe_map_us = s.hwframe_map_us;
            slot.enc_filter_in_us = s.filter_in_us;
            slot.enc_filter_out_us = s.filter_out_us;
            slot.enc_send_frame_us = s.send_frame_us;
            slot.enc_recv_packets_us = s.recv_packets_us;
        }
    }

    /// Get a snapshot of a single frame trace entry.
    pub fn get(&self, frame_id: u64) -> FrameTrace {
        let e = self.entries.lock().unwrap();
        let slot = &e[Self::slot(frame_id)];
        if slot.frame_id == frame_id { *slot } else { FrameTrace::default() }
    }

    /// Snapshot entries with a queue marker (i.e. completed frames).
    fn sent_entries(&self) -> Vec<FrameTrace> {
        let e = self.entries.lock().unwrap();
        e.iter().filter(|t| t.t_queue_ns != 0).copied().collect()
    }

    /// Compute percentile stats over all completed frames currently in the ring.
    pub fn compute_stats(&self) -> FrameStats {
        let entries = self.sent_entries();

        FrameStats {
            samples: entries.len() as u32,
            capture_us: Percentiles::from_samples(
                entries.iter().filter_map(|t| t.capture_ns().map(ns_to_us)),
            ),
            encode_us: Percentiles::from_samples(
                entries.iter().filter_map(|t| t.encode_ns().map(ns_to_us)),
            ),
            desktop_total_us: Percentiles::from_samples(
                entries.iter().filter_map(|t| t.desktop_total_ns().map(ns_to_us)),
            ),
        }
    }
}

fn ns_to_us(n: u64) -> u64 {
    n / 1000
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Percentiles {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
}

impl Percentiles {
    /// Compute p50/p95/p99 by sorting. Input is taken by iterator to avoid
    /// materializing extra Vecs on the caller side.
    pub fn from_samples(iter: impl IntoIterator<Item = u64>) -> Self {
        let mut v: Vec<u64> = iter.into_iter().collect();
        if v.is_empty() {
            return Self::default();
        }
        v.sort_unstable();
        let pick = |p: f32| -> u64 {
            let idx = ((v.len() as f32 - 1.0) * p).round() as usize;
            v[idx.min(v.len() - 1)]
        };
        Self { p50: pick(0.50), p95: pick(0.95), p99: pick(0.99) }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub samples: u32,
    pub capture_us: Percentiles,
    pub encode_us: Percentiles,
    pub desktop_total_us: Percentiles,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_records_and_computes_stats() {
        let ring = TraceRing::new();
        for id in 0..10u64 {
            ring.begin(id);
            ring.mark_capture_ready(id);
            ring.mark_encode_start(id);
            ring.mark_encode_done(id, 1234);
            ring.mark_queued(id);
        }
        let stats = ring.compute_stats();
        assert_eq!(stats.samples, 10);
    }

    #[test]
    fn percentiles_single_sample() {
        let p = Percentiles::from_samples([100u64]);
        assert_eq!(p.p50, 100);
        assert_eq!(p.p95, 100);
        assert_eq!(p.p99, 100);
    }

    #[test]
    fn percentiles_sorted() {
        let p = Percentiles::from_samples(1u64..=100);
        assert!(p.p50 <= p.p95);
        assert!(p.p95 <= p.p99);
    }

    #[test]
    fn stale_frame_id_rejected() {
        let ring = TraceRing::new();
        ring.begin(0);
        // Different frame_id mapping to same slot
        ring.mark_queued(RING_SIZE as u64);
        let stats = ring.compute_stats();
        assert_eq!(stats.samples, 0);
    }

    #[test]
    fn json_log_output() {
        let ring = TraceRing::new();
        ring.begin(42);
        ring.mark_capture_ready(42);
        ring.mark_encode_start(42);
        ring.mark_encode_done(42, 5000);
        ring.record_encode_substages(
            42,
            EncodeSubstages {
                dup_fd_us: 1,
                hwframe_map_us: 30,
                filter_in_us: 10,
                filter_out_us: 80,
                send_frame_us: 15,
                recv_packets_us: 40,
            },
        );
        ring.mark_queued(42);
        let trace = ring.get(42);
        let json = trace.to_json_log();
        assert!(json.starts_with(r#"{"t":"frame_trace","run_id":""#));
        assert!(json.contains(r#""bytes":5000"#));
        assert!(json.contains(r#""enc_map_us":30"#));
    }

    #[test]
    fn capture_ns_measures_start_to_ready() {
        let f = FrameTrace {
            t_capture_start_ns: 1_000,
            t_capture_ready_ns: 4_000,
            ..Default::default()
        };
        assert_eq!(f.capture_ns().unwrap(), 4_000 - 1_000);
    }
}
