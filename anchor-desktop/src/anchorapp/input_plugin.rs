//! InputPlugin — receives pointer/touch/keyboard events from the phone and
//! injects them into the host via a swappable [`InputBackend`].

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorwayland::input::{InputBackend, OutputGeometry, WaylandInput};

/// Decoded input events delivered without a JSON serialize/parse round trip.
/// Fields carry the same values the `anchor.input.*` JSON packets carried;
/// `time` is the producer's `now_ms()` truncated to u32 on receipt.
#[derive(Debug, Clone)]
pub enum InputPluginMsg {
    /// Normalized stream coordinates in `[0, 1]`. An empty `output_name`
    /// selects the default output mapping.
    MotionAbsolute {
        x: f64,
        y: f64,
        output_name: String,
        time: u32,
    },
    /// Pointer deltas in logical pixels.
    MotionRelative {
        dx: f64,
        dy: f64,
        time: u32,
    },
    /// `button` is a Linux evdev code (e.g. 272 = left).
    Button {
        button: u32,
        pressed: bool,
        time: u32,
    },
    /// `axis`: 0 = vertical, 1 = horizontal.
    Axis {
        axis: u32,
        value: f64,
        time: u32,
    },
    KeyHid {
        hid_usage: u32,
        pressed: bool,
        time: u32,
    },
    Text {
        text: String,
        time: u32,
    },
}

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

    // Main event loop. Typed `AnchorMessage::Input` events skip JSON entirely;
    // the `Json` path stays for lower-rate control messages (output_layout,
    // stream_info) and any other producers.
    let mut perf = InputTimingStats::default();
    for event in rx.iter() {
        match &event.message {
            AnchorMessage::Input(msg) => {
                let event_started = Instant::now();
                let was_motion_absolute =
                    dispatch_typed(&mut backend, msg, event_started, &mut perf);
                finish_input_event(&mut backend, &mut perf, event_started, was_motion_absolute);
            }
            AnchorMessage::Json(json_str) => {
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
                dispatch_json(&mut backend, &payload, msg_type, time, event_started, &mut perf);
                finish_input_event(
                    &mut backend,
                    &mut perf,
                    event_started,
                    msg_type == "anchor.input.motion_absolute",
                );
            }
            _ => continue,
        }
    }
}

/// Handles one typed input event. Returns true for motion_absolute so the
/// caller's epilogue can apply the slow-event warning to the same event kind
/// the JSON path watches.
fn dispatch_typed(
    backend: &mut impl InputBackend,
    msg: &InputPluginMsg,
    event_started: Instant,
    perf: &mut InputTimingStats,
) -> bool {
    match msg {
        InputPluginMsg::MotionAbsolute { x, y, output_name, time } => {
            perf.record_motion_absolute(event_started);
            backend.pointer_motion_absolute_targeted(
                *time,
                *x,
                *y,
                (!output_name.is_empty()).then_some(output_name.as_str()),
            );
            true
        }
        InputPluginMsg::MotionRelative { dx, dy, time } => {
            perf.record_motion_relative(event_started);
            backend.pointer_motion(*time, *dx, *dy);
            false
        }
        InputPluginMsg::Button { button, pressed, time } => {
            perf.record_button(event_started);
            backend.pointer_button(*time, *button, *pressed);
            false
        }
        InputPluginMsg::Axis { axis, value, time } => {
            perf.record_axis(event_started);
            backend.pointer_axis(*time, *axis == 1, *value);
            false
        }
        InputPluginMsg::KeyHid { hid_usage, pressed, time } => {
            if !backend.key_hid(*time, *hid_usage, *pressed) {
                log::debug!("Input: unhandled HID usage {}", hid_usage);
            }
            false
        }
        InputPluginMsg::Text { text, time } => {
            backend.type_text(*time, text);
            false
        }
    }
}

/// Handles one JSON `anchor.input.*` / control message (the pre-typed wire
/// format, still used by low-rate senders like output_layout and stream_info).
fn dispatch_json(
    backend: &mut impl InputBackend,
    payload: &serde_json::Value,
    msg_type: &str,
    time: u32,
    event_started: Instant,
    perf: &mut InputTimingStats,
) {
    match msg_type {
        "output_layout" => {
            if let Some(outputs) = parse_output_layout(payload) {
                backend.set_output_layout(&outputs);
            } else {
                log::warn!("Input: ignoring malformed output_layout message");
            }
        }
        "stream_info" => {
            if let (Some(w), Some(h)) = (
                payload.get("width").and_then(|v| v.as_u64()),
                payload.get("height").and_then(|v| v.as_u64()),
            ) {
                let output_name = payload.get("output_name").and_then(|v| v.as_str()).unwrap_or("");
                let output_x = payload.get("output_x").and_then(|v| v.as_i64()).map(|v| v as i32);
                let output_y = payload.get("output_y").and_then(|v| v.as_i64()).map(|v| v as i32);
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
            let output_name =
                payload.get("output_name").and_then(|v| v.as_str()).filter(|name| !name.is_empty());
            backend.pointer_motion_absolute_targeted(time, x, y, output_name);
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
            let hid_usage = payload.get("hid_usage").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
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
}

/// Shared per-event epilogue: flush the backend, record cost metrics, and warn
/// on slow absolute-motion events. Same for typed and JSON events.
fn finish_input_event(
    backend: &mut impl InputBackend,
    perf: &mut InputTimingStats,
    event_started: Instant,
    was_motion_absolute: bool,
) {
    backend.flush();
    let event_cost_us = event_started.elapsed().as_micros();
    perf.record_cost(event_cost_us);
    if was_motion_absolute && event_cost_us >= INPUT_SLOW_EVENT_WARN_US {
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

fn parse_output_layout(payload: &serde_json::Value) -> Option<Vec<OutputGeometry>> {
    let outputs = payload.get("outputs")?.as_array()?;
    outputs
        .iter()
        .map(|output| {
            let width = u32::try_from(output.get("width")?.as_u64()?).ok()?;
            let height = u32::try_from(output.get("height")?.as_u64()?).ok()?;
            if width == 0 || height == 0 {
                return None;
            }
            Some(OutputGeometry {
                name: output.get("name")?.as_str()?.to_string(),
                x: i32::try_from(output.get("x")?.as_i64()?).ok()?,
                y: i32::try_from(output.get("y")?.as_i64()?).ok()?,
                width,
                height,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_output_topology() {
        let payload = serde_json::json!({
            "outputs": [
                {"name": "eDP-1", "x": 0, "y": 0, "width": 1920, "height": 1200},
                {"name": "HEADLESS-1", "x": 1920, "y": 40, "width": 1080, "height": 2340}
            ]
        });

        assert_eq!(
            parse_output_layout(&payload),
            Some(vec![
                OutputGeometry { name: "eDP-1".into(), x: 0, y: 0, width: 1920, height: 1200 },
                OutputGeometry {
                    name: "HEADLESS-1".into(),
                    x: 1920,
                    y: 40,
                    width: 1080,
                    height: 2340,
                },
            ])
        );
    }

    #[test]
    fn malformed_topology_does_not_look_like_an_empty_layout() {
        assert_eq!(parse_output_layout(&serde_json::json!({})), None);
        assert_eq!(parse_output_layout(&serde_json::json!({"outputs": null})), None);
        assert_eq!(parse_output_layout(&serde_json::json!({"outputs": []})), Some(Vec::new()));
    }

    #[test]
    fn one_invalid_entry_rejects_the_entire_topology_snapshot() {
        let invalid_entries = [
            serde_json::json!({"name": "zero", "x": 0, "y": 0, "width": 0, "height": 600}),
            serde_json::json!({"name": "x-overflow", "x": 2147483648_i64, "y": 0, "width": 800, "height": 600}),
            serde_json::json!({"name": "width-overflow", "x": 0, "y": 0, "width": 4294967296_u64, "height": 600}),
            serde_json::json!({"name": "missing-height", "x": 0, "y": 0, "width": 800}),
        ];

        for invalid in invalid_entries {
            let payload = serde_json::json!({
                "outputs": [
                    {"name": "valid", "x": -100, "y": 25, "width": 800, "height": 600},
                    invalid
                ]
            });
            assert_eq!(parse_output_layout(&payload), None);
        }
    }

    /// Records backend calls so typed and JSON dispatch can be compared.
    #[derive(Default)]
    struct RecordingBackend {
        calls: Vec<String>,
    }

    impl InputBackend for RecordingBackend {
        fn total_extent(&self) -> (u32, u32) {
            (0, 0)
        }
        fn stream_mapping(&self) -> (u32, u32, i32, i32) {
            (0, 0, 0, 0)
        }
        fn has_keyboard(&self) -> bool {
            true
        }
        fn set_stream_info(
            &mut self,
            output_name: &str,
            width: u32,
            height: u32,
            output_x: Option<i32>,
            output_y: Option<i32>,
        ) {
            self.calls.push(format!(
                "stream_info {output_name} {width}x{height} {output_x:?} {output_y:?}"
            ));
        }
        fn set_output_layout(&mut self, outputs: &[OutputGeometry]) {
            self.calls.push(format!("output_layout {outputs:?}"));
        }
        fn pointer_motion(&mut self, time: u32, dx: f64, dy: f64) {
            self.calls.push(format!("motion {time} {dx} {dy}"));
        }
        fn pointer_motion_absolute(&mut self, time: u32, x_norm: f64, y_norm: f64) {
            self.calls.push(format!("motion_abs {time} {x_norm} {y_norm}"));
        }
        fn pointer_motion_absolute_targeted(
            &mut self,
            time: u32,
            x_norm: f64,
            y_norm: f64,
            output_name: Option<&str>,
        ) {
            self.calls.push(format!("motion_abs {time} {x_norm} {y_norm} {output_name:?}"));
        }
        fn pointer_button(&mut self, time: u32, button: u32, pressed: bool) {
            self.calls.push(format!("button {time} {button} {pressed}"));
        }
        fn pointer_axis(&mut self, time: u32, horizontal: bool, value: f64) {
            self.calls.push(format!("axis {time} {horizontal} {value}"));
        }
        fn pointer_frame(&mut self) {
            self.calls.push("frame".into());
        }
        fn type_text(&mut self, time: u32, text: &str) {
            self.calls.push(format!("text {time} {text}"));
        }
        fn key_special(&mut self, time: u32, key: &str) -> bool {
            self.calls.push(format!("key {time} {key}"));
            true
        }
        fn key_combo(&mut self, time: u32, modifiers: &[String], key: &str) {
            self.calls.push(format!("combo {time} {modifiers:?} {key}"));
        }
        fn key_hid(&mut self, time: u32, hid_usage: u32, pressed: bool) -> bool {
            self.calls.push(format!("key_hid {time} {hid_usage} {pressed}"));
            true
        }
        fn flush(&mut self) {
            self.calls.push("flush".into());
        }
    }

    /// Every input event kind must drive the identical backend call whether it
    /// arrives as a typed `InputPluginMsg` or the legacy `anchor.input.*` JSON.
    #[test]
    fn typed_and_json_dispatch_drive_identical_backend_calls() {
        let cases: Vec<(serde_json::Value, InputPluginMsg)> = vec![
            (
                serde_json::json!({"type":"anchor.input.motion_absolute","x":0.5,"y":0.25,"output_name":"HEADLESS-1","time":7}),
                InputPluginMsg::MotionAbsolute {
                    x: 0.5,
                    y: 0.25,
                    output_name: "HEADLESS-1".into(),
                    time: 7,
                },
            ),
            (
                // Empty output_name maps to the untargeted default in both paths.
                serde_json::json!({"type":"anchor.input.motion_absolute","x":0.5,"y":0.25,"output_name":"","time":7}),
                InputPluginMsg::MotionAbsolute {
                    x: 0.5,
                    y: 0.25,
                    output_name: String::new(),
                    time: 7,
                },
            ),
            (
                serde_json::json!({"type":"anchor.input.motion","dx":1.25,"dy":-0.875,"time":8}),
                InputPluginMsg::MotionRelative { dx: 1.25, dy: -0.875, time: 8 },
            ),
            (
                serde_json::json!({"type":"anchor.input.button","button":272,"state":1,"time":9}),
                InputPluginMsg::Button { button: 272, pressed: true, time: 9 },
            ),
            (
                serde_json::json!({"type":"anchor.input.button","button":273,"state":0,"time":10}),
                InputPluginMsg::Button { button: 273, pressed: false, time: 10 },
            ),
            (
                serde_json::json!({"type":"anchor.input.axis","axis":0,"value":-120,"time":11}),
                InputPluginMsg::Axis { axis: 0, value: -120.0, time: 11 },
            ),
            (
                serde_json::json!({"type":"anchor.input.key_hid","hid_usage":44,"pressed":true,"time":12}),
                InputPluginMsg::KeyHid { hid_usage: 44, pressed: true, time: 12 },
            ),
            (
                serde_json::json!({"type":"anchor.input.text","text":"Hello","time":13}),
                InputPluginMsg::Text { text: "Hello".into(), time: 13 },
            ),
        ];

        for (payload, msg) in cases {
            let mut json_backend = RecordingBackend::default();
            let mut typed_backend = RecordingBackend::default();
            let mut json_perf = InputTimingStats::default();
            let mut typed_perf = InputTimingStats::default();
            let started = Instant::now();
            let time = payload.get("time").and_then(|v| v.as_u64()).unwrap() as u32;

            dispatch_json(
                &mut json_backend,
                &payload,
                payload.get("type").and_then(|v| v.as_str()).unwrap(),
                time,
                started,
                &mut json_perf,
            );
            dispatch_typed(&mut typed_backend, &msg, started, &mut typed_perf);

            assert_eq!(
                json_backend.calls, typed_backend.calls,
                "backend calls diverge for payload {payload}"
            );
            assert_eq!(
                (
                    json_perf.motion_absolute_events,
                    json_perf.motion_relative_events,
                    json_perf.button_events,
                    json_perf.axis_events
                ),
                (
                    typed_perf.motion_absolute_events,
                    typed_perf.motion_relative_events,
                    typed_perf.button_events,
                    typed_perf.axis_events
                ),
                "perf counters diverge for payload {payload}"
            );
        }
    }
}
