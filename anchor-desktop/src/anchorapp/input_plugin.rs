//! InputPlugin — receives pointer/touch/keyboard events from the phone and
//! injects them into the host via a swappable [`InputBackend`].

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorwayland::input::{InputBackend, WaylandInput};

pub struct AnchorPluginInput {
    pub plugin_rx: Option<Receiver<AnchorEvent>>,
    pub broker_tx: Option<Sender<AnchorEvent>>,
}

const INPUT_STATS_INTERVAL: Duration = Duration::from_secs(1);
const INPUT_SLOW_EVENT_WARN_US: u128 = 4_000;

#[derive(Default)]
struct InputTimingStats {
    window_started: Option<Instant>,
    motion_absolute_events: u64,
    motion_relative_events: u64,
    button_events: u64,
    axis_events: u64,
    max_motion_gap_ms: f64,
    max_event_us: u128,
    sum_event_us: u128,
    slow_events: u64,
    last_motion_absolute_at: Option<Instant>,
}

impl InputTimingStats {
    fn begin_window_if_needed(&mut self, now: Instant) {
        if self.window_started.is_none() {
            self.window_started = Some(now);
        }
    }

    fn record_motion_absolute(&mut self, now: Instant) {
        self.begin_window_if_needed(now);
        self.motion_absolute_events += 1;
        if let Some(prev) = self.last_motion_absolute_at.replace(now) {
            let gap_ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            if gap_ms > self.max_motion_gap_ms {
                self.max_motion_gap_ms = gap_ms;
            }
        }
    }

    fn record_motion_relative(&mut self, now: Instant) {
        self.begin_window_if_needed(now);
        self.motion_relative_events += 1;
    }

    fn record_button(&mut self, now: Instant) {
        self.begin_window_if_needed(now);
        self.button_events += 1;
    }

    fn record_axis(&mut self, now: Instant) {
        self.begin_window_if_needed(now);
        self.axis_events += 1;
    }

    fn record_cost(&mut self, cost_us: u128) {
        self.sum_event_us += cost_us;
        if cost_us > self.max_event_us {
            self.max_event_us = cost_us;
        }
        if cost_us >= INPUT_SLOW_EVENT_WARN_US {
            self.slow_events += 1;
        }
    }

    fn maybe_log_and_reset(&mut self, now: Instant) {
        let Some(started) = self.window_started else {
            return;
        };
        if now.duration_since(started) < INPUT_STATS_INTERVAL {
            return;
        }
        let total_events = self.motion_absolute_events
            + self.motion_relative_events
            + self.button_events
            + self.axis_events;
        if total_events > 0 {
            let avg_event_us = self.sum_event_us as f64 / total_events as f64;
            log::info!(
                "[input.perf] abs={} rel={} button={} axis={} avg_event_us={:.1} max_event_us={} slow_events={} max_abs_gap_ms={:.1}",
                self.motion_absolute_events,
                self.motion_relative_events,
                self.button_events,
                self.axis_events,
                avg_event_us,
                self.max_event_us,
                self.slow_events,
                self.max_motion_gap_ms
            );
            crate::metrics::emit(serde_json::json!({
                "t": "input",
                "abs": self.motion_absolute_events,
                "rel": self.motion_relative_events,
                "button": self.button_events,
                "axis": self.axis_events,
                "avg_event_us": (avg_event_us * 10.0).round() / 10.0,
                "max_event_us": self.max_event_us,
                "slow_events": self.slow_events,
                "max_abs_gap_ms": (self.max_motion_gap_ms * 10.0).round() / 10.0,
            }));
        }
        *self = Self::default();
        self.window_started = Some(now);
    }
}

impl Plugin for AnchorPluginInput {
    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        AnchorPluginInput { plugin_rx, broker_tx }
    }

    fn run(&mut self) -> Option<JoinHandle<()>> {
        let rx = self.plugin_rx.take().expect("input plugin_rx not set");
        let broker_tx = self.broker_tx.take().expect("input broker_tx not set");

        let handle = thread::Builder::new()
            .name("input-plugin".to_string())
            .spawn(move || {
                log::debug!("Input plugin thread started");
                run_input_loop(rx, broker_tx);
                log::info!("Input plugin thread exiting");
            })
            .expect("failed to spawn input plugin thread");

        Some(handle)
    }
}

fn run_input_loop(rx: Receiver<AnchorEvent>, broker_tx: Sender<AnchorEvent>) {
    let mut backend = match WaylandInput::new() {
        Ok(b) => b,
        Err(e) => {
            log::error!("Input: {}", e);
            broker_tx
                .send(AnchorEvent {
                    target: AnchorTarget::Gui,
                    message: AnchorMessage::Json(
                        serde_json::json!({
                            "type": "input.unavailable",
                            "reason": e,
                        })
                        .to_string(),
                    ),
                })
                .ok();
            return;
        }
    };

    let (total_w, total_h) = backend.total_extent();
    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Gui,
            message: AnchorMessage::Json(
                serde_json::json!({
                    "type": "input.ready",
                    "total_width": total_w,
                    "total_height": total_h,
                    "pointer": true,
                    "keyboard": backend.has_keyboard(),
                })
                .to_string(),
            ),
        })
        .ok();

    // Main event loop.
    let mut perf = InputTimingStats::default();
    for event in rx.iter() {
        let json_str = match &event.message {
            AnchorMessage::Json(s) => s.as_str(),
            _ => continue,
        };

        let payload: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Input: bad JSON: {}", e);
                continue;
            }
        };

        let msg_type = match payload.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };

        let time = payload.get("time").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
        let event_started = Instant::now();

        match msg_type {
            "stream_info" => {
                if let (Some(w), Some(h)) = (
                    payload.get("width").and_then(|v| v.as_u64()),
                    payload.get("height").and_then(|v| v.as_u64()),
                ) {
                    let output_name =
                        payload.get("output_name").and_then(|v| v.as_str()).unwrap_or("");
                    let output_x =
                        payload.get("output_x").and_then(|v| v.as_i64()).map(|v| v as i32);
                    let output_y =
                        payload.get("output_y").and_then(|v| v.as_i64()).map(|v| v as i32);
                    backend.set_stream_info(output_name, w as u32, h as u32, output_x, output_y);
                }
            }
            "anchor.input.motion" => {
                perf.record_motion_relative(event_started);
                let dx = payload.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let dy = payload.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
                backend.pointer_motion(time, dx, dy);
            }
            "anchor.input.motion_absolute" => {
                perf.record_motion_absolute(event_started);
                let x = payload.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = payload.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                backend.pointer_motion_absolute(time, x, y);
            }
            "anchor.input.button" => {
                perf.record_button(event_started);
                let button = payload.get("button").and_then(|v| v.as_u64()).unwrap_or(272) as u32;
                let pressed = payload.get("state").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
                backend.pointer_button(time, button, pressed);
            }
            "anchor.input.axis" => {
                perf.record_axis(event_started);
                let axis_val = payload.get("axis").and_then(|v| v.as_u64()).unwrap_or(0);
                let value = payload.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                backend.pointer_axis(time, axis_val == 1, value);
            }
            "anchor.input.frame" => {
                backend.pointer_frame();
            }
            "anchor.input.text" => {
                if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
                    backend.type_text(time, text);
                }
            }
            "anchor.input.key" => {
                if let Some(key) = payload.get("key").and_then(|v| v.as_str()) {
                    backend.key_special(time, key);
                }
            }
            "anchor.input.key_hid" => {
                let hid_usage =
                    payload.get("hid_usage").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let pressed = payload.get("pressed").and_then(|v| v.as_bool()).unwrap_or(false);
                if !backend.key_hid(time, hid_usage, pressed) {
                    log::debug!("Input: unhandled HID usage {}", hid_usage);
                }
            }
            "anchor.input.key_combo" => {
                let mods = payload
                    .get("modifiers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
                backend.key_combo(time, &mods, key);
            }
            _ => {
                log::debug!("Input: unhandled message type: {}", msg_type);
            }
        }

        backend.flush();
        let event_cost_us = event_started.elapsed().as_micros();
        perf.record_cost(event_cost_us);
        if msg_type == "anchor.input.motion_absolute" && event_cost_us >= INPUT_SLOW_EVENT_WARN_US {
            let (total_w, total_h) = backend.total_extent();
            let (sw, sh, sx, sy) = backend.stream_mapping();
            log::warn!(
                "[input.perf] slow_abs_event_us={} extent={}x{} mapped_output={}x{}+{},{}",
                event_cost_us,
                total_w,
                total_h,
                sw,
                sh,
                sx,
                sy
            );
        }
        perf.maybe_log_and_reset(Instant::now());
    }
}
