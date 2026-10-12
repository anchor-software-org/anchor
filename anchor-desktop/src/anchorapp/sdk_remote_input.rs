//! Ordered desktop-to-device sender for the `org.anchor.input` capability.
//!
//! The same capability session carries phone-as-touchpad records inbound and
//! remote-control records outbound; this binding is the outbound half. The
//! Android side normalizes pointer coordinates against its default display,
//! so callers send [0,1] fractions which this module scales to the wire's
//! [0, 65535] range.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Instant;

use anchor_sdk::{Capability, DatagramFlow, input, video_frame};

use super::mirror_decoder::MirrorDecoder;

const POINTER_BUTTON_LEFT: i32 = 1;

static MONOTONIC_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Shared monotonic clock for ping/pong offsets and frame ageing.
fn monotonic_us() -> i64 {
    MONOTONIC_EPOCH.elapsed().as_micros() as i64
}

struct InputRecord {
    type_url: &'static str,
    payload: Vec<u8>,
}

#[derive(Clone)]
pub struct SdkRemoteInputSender {
    tx: SyncSender<InputRecord>,
}

impl SdkRemoteInputSender {
    pub fn new(capability: Capability) -> Self {
        // High-rate motion must not build a stale queue: a bounded channel
        // makes `send` drop the oldest-pending behavior cheap at the caller.
        let (tx, rx) = mpsc::sync_channel::<InputRecord>(256);
        thread::Builder::new()
            .name("anchor-sdk-remote-input-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK remote-input runtime failed: {error}");
                            return;
                        }
                    };
                for first in rx.iter() {
                    // Drain whatever queued while the last record was in
                    // flight, then collapse each run of absolute moves to the
                    // newest — a drag should never replay stale positions.
                    let mut batch = vec![first];
                    while let Ok(next) = rx.try_recv() {
                        batch.push(next);
                    }
                    let mut skip = vec![false; batch.len()];
                    let mut last_move: Option<usize> = None;
                    for (i, record) in batch.iter().enumerate() {
                        if record.type_url == input::POINTER_ABSOLUTE_TYPE_URL
                            && let Some(prev) = last_move.replace(i)
                        {
                            skip[prev] = true;
                        }
                    }
                    for (record, drop) in batch.into_iter().zip(skip) {
                        if drop {
                            continue;
                        }
                        let result = runtime.block_on(async {
                            capability.send_record(record.type_url, record.payload).await
                        });
                        if let Err(error) = result {
                            log::warn!("SDK remote-input send failed: {error}");
                        }
                    }
                }
            })
            .expect("SDK remote-input writer thread must start");
        Self { tx }
    }

    fn send(&self, type_url: &'static str, payload: Vec<u8>) -> bool {
        self.tx.try_send(InputRecord { type_url, payload }).is_ok()
    }

    fn move_absolute(&self, x_norm: f64, y_norm: f64) -> bool {
        let x = (x_norm.clamp(0.0, 1.0) * 65535.0).round() as u32;
        let y = (y_norm.clamp(0.0, 1.0) * 65535.0).round() as u32;
        self.send(input::POINTER_ABSOLUTE_TYPE_URL, input::encode_pointer_absolute(x, y, ""))
    }

    fn pointer_button(&self, button: i32, pressed: bool) -> bool {
        self.send(input::POINTER_BUTTON_TYPE_URL, input::encode_pointer_button(button, pressed))
    }

    fn tap(&self, x_norm: f64, y_norm: f64) -> bool {
        self.move_absolute(x_norm, y_norm)
            && self.pointer_button(POINTER_BUTTON_LEFT, true)
            && self.pointer_button(POINTER_BUTTON_LEFT, false)
    }

    fn remote_control(&self, kind: i32) -> bool {
        self.send(input::REMOTE_CONTROL_TYPE_URL, input::encode_remote_control(kind))
    }

    /// Latency probe — the device echoes this with its own monotonic
    /// timestamp, letting the binding derive a clock offset for frame ageing.
    pub fn ping(&self) -> bool {
        self.send(input::PING_TYPE_URL, input::encode_ping(monotonic_us()))
    }
}

/// Latest reassembled mirror frame for a device. Phase 1 carries JPEG stills
/// produced by `AccessibilityService.takeScreenshot`; the slot is payload-typed
/// (`jpeg` bytes) so a later H.264 producer can replace the encoding without
/// touching the flow plumbing.
#[derive(Clone)]
pub struct MirrorFrame {
    /// Monotonic receive-order sequence, unique per binding instance.
    pub sequence: u64,
    /// ANFR frame sequence echoed back for debugging/ordering.
    pub frame_sequence: u64,
    /// Glass-to-glass latency when the ping/pong clock offset is known:
    /// desktop receive time minus (device pts + offset).
    pub latency_ms: Option<f64>,
    pub jpeg: Arc<Vec<u8>>,
}

/// Per-device set of UI windows that asked for the remote mirror. The stream
/// starts when a device's first watcher appears and stops when the last one
/// leaves, so a popout and the main window can share one mirror without
/// racing each other's stop requests.
#[derive(Default)]
pub struct MirrorWatchers {
    /// device key (device_id, or "" for the broadcast default) → window labels
    inner: Mutex<HashMap<String, HashSet<String>>>,
}

impl MirrorWatchers {
    /// Returns Some(true) when the device transitions watched from zero,
    /// Some(false) when it drops to zero watchers — i.e. when a START/STOP
    /// record should go on the wire. None means no state change.
    pub fn set(&self, device_key: &str, watcher: &str, enabled: bool) -> Option<bool> {
        let mut inner = self.inner.lock().unwrap();
        if enabled {
            let set = inner.entry(device_key.to_string()).or_default();
            let start = set.is_empty();
            set.insert(watcher.to_string());
            return start.then_some(true);
        }
        let set = inner.get_mut(device_key)?;
        if !set.remove(watcher) {
            return None;
        }
        if set.is_empty() {
            inner.remove(device_key);
            return Some(false);
        }
        None
    }

    /// Drop a watcher from every device (window closed). Returns the device
    /// keys that transitioned to zero watchers and need a STOP.
    pub fn dropped(&self, watcher: &str) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap();
        let mut emptied = Vec::new();
        inner.retain(|device_key, set| {
            set.remove(watcher);
            if set.is_empty() {
                emptied.push(device_key.clone());
                false
            } else {
                true
            }
        });
        emptied
    }
}

#[derive(Clone, Default)]
pub struct SdkRemoteInputBinding {
    senders: Arc<Mutex<HashMap<String, SdkRemoteInputSender>>>,
    frames: Arc<Mutex<HashMap<String, MirrorFrame>>>,
    frame_sequence: Arc<AtomicU64>,
    /// device_id → ((desktop monotonic us) − (device monotonic us), best rtt)
    /// from ping/pong round trips. The offset estimate is only trusted as far
    /// as its sample's RTT is symmetric — keeping the minimum-RTT sample
    /// filters queued/asymmetric pongs that otherwise skew the offset.
    clock_offsets: Arc<Mutex<HashMap<String, (i64, i64)>>>,
}

impl SdkRemoteInputBinding {
    pub fn attach(&self, device_id: String, sender: SdkRemoteInputSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn sender(&self, device_id: &str) -> Option<SdkRemoteInputSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn senders(&self) -> Vec<(String, SdkRemoteInputSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }

    /// Handle an inbound InputPong record: estimate the device↔desktop clock
    /// offset so frame presentation timestamps can be aged on arrival.
    pub fn handle_pong(&self, device_id: &str, payload: &[u8]) {
        let Ok(pong) = input::decode_pong(payload) else {
            log::debug!("SDK mirror: undecodable pong from {device_id}");
            return;
        };
        let now = monotonic_us();
        let rtt_us = now - pong.ping_sent_time_us;
        // Assume a symmetric path: the device clock at pong receipt reads
        // roughly pong.sent + RTT/2.
        let offset = now - (pong.sent_time_us + rtt_us / 2);
        let mut offsets = self.clock_offsets.lock().unwrap();
        let keep = offsets.get(device_id).is_none_or(|&(_, best_rtt)| rtt_us <= best_rtt);
        if keep {
            offsets.insert(device_id.to_string(), (offset, rtt_us));
        }
        log::debug!("SDK mirror clock offset {device_id}: rtt={}us", rtt_us);
    }

    /// Accept the device's mirror datagram flow and keep only the newest
    /// reassembled frame — the UI polls, so stale stills are worthless.
    pub fn attach_frame_flow(
        &self,
        device_id: String,
        capability_session_id: u64,
        flow: DatagramFlow,
    ) {
        let expected_flow_id = flow.flow_id();
        let frames = Arc::clone(&self.frames);
        let frame_sequence = Arc::clone(&self.frame_sequence);
        let clock_offsets = Arc::clone(&self.clock_offsets);
        thread::Builder::new()
            .name(format!("anchor-sdk-mirror-frames-{device_id}"))
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK mirror frame runtime failed: {error}");
                            return;
                        }
                    };
                let mut assembler = video_frame::Reassembler::default();
                let mut received = 0_u64;
                // Smoothed latency for display — per-frame measurements jitter
                // with the clock-offset error and reassembly timing, so the
                // badge would flicker nonsense (incl. negatives) without this.
                let mut latency_ema: Option<f64> = None;
                // Created lazily on the first non-JPEG payload so the stills
                // path never pays for an H.264 context it won't use.
                let mut decoder: Option<MirrorDecoder> = None;
                runtime.block_on(async move {
                    loop {
                        let datagram = match flow.recv().await {
                            Ok(datagram) => datagram,
                            Err(error) => {
                                log::info!(
                                    "SDK mirror datagram receiver ended for {device_id}: {error}"
                                );
                                break;
                            }
                        };
                        let Some(payload) = assembler.add_datagram(
                            &datagram,
                            video_frame::FRAME_KIND_SCREEN,
                            capability_session_id,
                            expected_flow_id,
                        ) else {
                            continue;
                        };
                        let (frame_sequence_field, pts_us) =
                            video_frame::FrameHeader::decode(&datagram)
                                .map(|(header, _)| {
                                    (header.sequence, header.presentation_time_us as i64)
                                })
                                .unwrap_or((0, 0));
                        let latency_ms = clock_offsets
                            .lock()
                            .unwrap()
                            .get(&device_id)
                            .map(|&(offset, _)| {
                                // Negative latency is impossible — it only
                                // means the clock-offset estimate overshot.
                                // Clamp the sample, then EMA-smooth so the
                                // UI shows a stable number, not per-frame noise.
                                ((monotonic_us() - (pts_us + offset)) as f64 / 1000.0).max(0.0)
                            });
                        if let Some(sample) = latency_ms {
                            latency_ema = Some(match latency_ema {
                                Some(ema) => ema * 0.85 + sample * 0.15,
                                None => sample,
                            });
                        }
                        let latency_ms = latency_ema;
                        // Two producers share this flow: JPEG stills arrive
                        // display-ready; anything else is an Annex-B H.264
                        // access unit (config AUs feed the decoder too — it
                        // consumes parameter sets without emitting a frame).
                        let jpegs: Vec<Vec<u8>> = if payload.starts_with(&[0xFF, 0xD8]) {
                            vec![payload]
                        } else {
                            if decoder.is_none() {
                                decoder = MirrorDecoder::new();
                                log::info!(
                                    "SDK mirror H.264 stream from {device_id}: decoder={}",
                                    decoder.is_some()
                                );
                            }
                            match decoder.as_mut() {
                                Some(decoder) => decoder.decode_to_jpeg(&payload),
                                None => continue,
                            }
                        };
                        for jpeg in jpegs {
                            let sequence = frame_sequence.fetch_add(1, Ordering::Relaxed) + 1;
                            if std::env::var("ANCHOR_DUMP_MIRROR").is_ok() {
                                let _ = std::fs::write("/tmp/anchor-mirror-latest.jpg", &jpeg);
                            }
                            let jpeg = Arc::new(jpeg);
                            frames.lock().unwrap().insert(
                                device_id.clone(),
                                MirrorFrame {
                                    sequence,
                                    frame_sequence: frame_sequence_field,
                                    latency_ms,
                                    jpeg: Arc::clone(&jpeg),
                                },
                            );
                            received += 1;
                            if received == 1 {
                                log::info!(
                                    "SDK mirror first frame from {device_id}: {} bytes, jpeg={}",
                                    jpeg.len(),
                                    jpeg.starts_with(&[0xFF, 0xD8])
                                );
                            }
                            if received.is_multiple_of(50) {
                                let rtt = flow.transport_stats().rtt.as_millis();
                                let latency = latency_ms
                                    .map(|ms| format!("{ms:.0}ms e2e"))
                                    .unwrap_or_else(|| "no clock offset".into());
                                log::info!(
                                    "SDK mirror frames received from {device_id}: {received} (rtt={rtt}ms, {latency})"
                                );
                            }
                        }
                    }
                });
            })
            .expect("SDK mirror frame thread must start");
    }

    /// Latest mirror frame after `after_sequence` (0 returns the newest).
    pub fn frame_after(&self, device_id: &str, after_sequence: u64) -> Option<MirrorFrame> {
        let frame = self.frames.lock().unwrap().get(device_id).cloned()?;
        (frame.sequence > after_sequence).then_some(frame)
    }

    /// Newest mirror frame across all devices — the UI's remote-control pane
    /// targets a single phone, so the freshest frame wins.
    pub fn latest_frame(&self, after_sequence: u64) -> Option<MirrorFrame> {
        self.frames
            .lock()
            .unwrap()
            .values()
            .filter(|frame| frame.sequence > after_sequence)
            .max_by_key(|frame| frame.sequence)
            .cloned()
    }
}

/// Route a `plugin_id: "remoteinput"` broker message onto the capability.
/// Returns true when the message was recognized (even if delivery failed).
pub fn send_json(sender: &SdkRemoteInputSender, value: &serde_json::Value) -> bool {
    let Some(kind) = value.get("type").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let num = |key: &str| value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    match kind {
        "remote_tap" => sender.tap(num("x"), num("y")),
        "remote_pointer_down" => {
            sender.move_absolute(num("x"), num("y"))
                && sender.pointer_button(POINTER_BUTTON_LEFT, true)
        }
        "remote_pointer_move" => sender.move_absolute(num("x"), num("y")),
        "remote_pointer_up" => sender.pointer_button(POINTER_BUTTON_LEFT, false),
        "remote_scroll" => sender
            .send(input::SCROLL_TYPE_URL, input::encode_scroll(num("dx") as i32, num("dy") as i32)),
        "remote_text" => sender.send(
            input::TEXT_TYPE_URL,
            input::encode_text(
                value.get("text").and_then(serde_json::Value::as_str).unwrap_or_default(),
            ),
        ),
        "remote_key" => sender.send(
            input::KEY_TYPE_URL,
            input::encode_key(
                num("usage") as u32,
                value.get("pressed").and_then(serde_json::Value::as_bool).unwrap_or(true),
            ),
        ),
        // Navigation actions have no HID usage — they ride RemoteControl.
        "remote_back" => sender.remote_control(input::REMOTE_CONTROL_BACK),
        "remote_home" => sender.remote_control(input::REMOTE_CONTROL_HOME),
        "remote_recents" => sender.remote_control(input::REMOTE_CONTROL_RECENTS),
        "remote_mirror" => sender.remote_control(
            if value.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true) {
                input::REMOTE_CONTROL_MIRROR_START
            } else {
                input::REMOTE_CONTROL_MIRROR_STOP
            },
        ),
        _ => return false,
    };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_starts_empty() {
        assert!(SdkRemoteInputBinding::default().senders().is_empty());
    }

    #[test]
    fn watcher_first_enable_starts() {
        let watchers = MirrorWatchers::default();
        assert_eq!(watchers.set("dev1", "main", true), Some(true));
    }

    #[test]
    fn watcher_second_window_does_not_restart() {
        let watchers = MirrorWatchers::default();
        watchers.set("dev1", "main", true);
        assert_eq!(watchers.set("dev1", "remote-popout", true), None);
    }

    #[test]
    fn watcher_stop_only_after_last_leaves() {
        let watchers = MirrorWatchers::default();
        watchers.set("dev1", "main", true);
        watchers.set("dev1", "remote-popout", true);
        assert_eq!(watchers.set("dev1", "main", false), None);
        assert_eq!(watchers.set("dev1", "remote-popout", false), Some(false));
    }

    #[test]
    fn watcher_disable_without_watch_is_noop() {
        let watchers = MirrorWatchers::default();
        assert_eq!(watchers.set("dev1", "main", false), None);
        watchers.set("dev1", "main", true);
        assert_eq!(watchers.set("dev1", "nobody", false), None);
    }

    #[test]
    fn watcher_dropped_empties_everywhere() {
        let watchers = MirrorWatchers::default();
        watchers.set("dev1", "remote-popout", true);
        watchers.set("dev2", "remote-popout", true);
        watchers.set("dev2", "main", true);
        let mut emptied = watchers.dropped("remote-popout");
        emptied.sort();
        assert_eq!(emptied, vec!["dev1".to_string()]);
        // dev2 keeps running for "main" — no second start needed.
        assert_eq!(watchers.set("dev2", "other", true), None);
    }

    #[test]
    fn watcher_devices_are_independent() {
        let watchers = MirrorWatchers::default();
        watchers.set("dev1", "main", true);
        assert_eq!(watchers.set("dev2", "main", true), Some(true));
        assert_eq!(watchers.set("dev1", "main", false), Some(false));
    }
}
