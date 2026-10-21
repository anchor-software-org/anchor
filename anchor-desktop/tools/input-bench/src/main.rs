//! Measures the per-input-event message cost in Anchor's broker path.
//!
//! `json` replicates the current pipeline exactly: prost decode (done,
//! outside this bench) -> `serde_json::json!{...}` -> `.to_string()` ->
//! `AnchorMessage::Json(String)` -> mpsc channel -> `serde_json::from_str::<Value>`
//! -> `payload.get(...)` field extraction -> backend call.
//!
//! `typed` is the candidate pipeline: construct `AnchorMessage::Input(InputPluginMsg)`
//! with decoded fields -> same channel -> match -> backend call.
//!
//! Reported numbers are per-event wall time for: produce (message construction),
//! consume (parse/dispatch on the plugin side), and roundtrip
//! (produce + channel send/recv + consume).

use std::hint::black_box;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

// ---- replicated production types ------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)] // Generic mirrors the real enum but has no sender here.
enum AnchorMessage {
    Generic(String),
    Json(String),
    Input(InputPluginMsg),
}

#[derive(Debug, Clone)]
enum InputPluginMsg {
    MotionAbsolute { x: f64, y: f64, output_name: String, time: u32 },
    MotionRelative { dx: f64, dy: f64, time: u32 },
    Button { button: u32, pressed: bool, time: u32 },
    Axis { axis: u32, value: f64, time: u32 },
    KeyHid { hid_usage: u32, pressed: bool, time: u32 },
    Text { text: String, time: u32 },
}

#[derive(Debug, Clone)]
struct AnchorEvent {
    message: AnchorMessage,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

// ---- realistic decoded inputs (what prost decode yields) -------------------

#[allow(dead_code)] // button_enum documents the wire value; mapping is inlined.
struct DecodedInputs {
    abs_x: u32,
    abs_y: u32,
    abs_output_name: String,
    rel_dx_1000ths: i32,
    rel_dy_1000ths: i32,
    key_hid_usage: u32,
    key_pressed: bool,
    button_enum: u32,
    button_pressed: bool,
    scroll_vertical_120ths: i32,
    text: String,
}

impl Default for DecodedInputs {
    fn default() -> Self {
        Self {
            abs_x: 32_000,
            abs_y: 48_000,
            abs_output_name: "HEADLESS-1".to_string(),
            rel_dx_1000ths: 1_250,
            rel_dy_1000ths: -875,
            key_hid_usage: 0x2C,
            key_pressed: true,
            button_enum: 1,
            button_pressed: true,
            scroll_vertical_120ths: -120,
            text: "Hello from Android".to_string(),
        }
    }
}

// ---- producer side: one fn per event kind per pipeline ---------------------
// `#[inline(never)]` keeps codegen comparable between the two pipelines.

#[inline(never)]
fn produce_json_motion_absolute(i: &DecodedInputs) -> AnchorEvent {
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.motion_absolute", "x": i.abs_x as f64 / 65535.0, "y": i.abs_y as f64 / 65535.0, "output_name": i.abs_output_name, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_motion_absolute(i: &DecodedInputs) -> AnchorEvent {
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::MotionAbsolute {
            x: i.abs_x as f64 / 65535.0,
            y: i.abs_y as f64 / 65535.0,
            output_name: i.abs_output_name.clone(),
            time: now_ms() as u32,
        }),
    }
}

#[inline(never)]
fn produce_json_motion_relative(i: &DecodedInputs) -> AnchorEvent {
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.motion", "dx": i.rel_dx_1000ths as f64 / 1000.0, "dy": i.rel_dy_1000ths as f64 / 1000.0, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_motion_relative(i: &DecodedInputs) -> AnchorEvent {
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::MotionRelative {
            dx: i.rel_dx_1000ths as f64 / 1000.0,
            dy: i.rel_dy_1000ths as f64 / 1000.0,
            time: now_ms() as u32,
        }),
    }
}

#[inline(never)]
fn produce_json_key_hid(i: &DecodedInputs) -> AnchorEvent {
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.key_hid", "hid_usage": i.key_hid_usage, "pressed": i.key_pressed, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_key_hid(i: &DecodedInputs) -> AnchorEvent {
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::KeyHid {
            hid_usage: i.key_hid_usage,
            pressed: i.key_pressed,
            time: now_ms() as u32,
        }),
    }
}

#[inline(never)]
fn produce_json_button(i: &DecodedInputs) -> AnchorEvent {
    // sdk_server maps the wire enum to evdev codes first; button_enum == 1 -> 272.
    let button: u32 = 272;
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.button", "button": button, "state": if i.button_pressed { 1 } else { 0 }, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_button(i: &DecodedInputs) -> AnchorEvent {
    let button: u32 = 272;
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::Button {
            button,
            pressed: i.button_pressed,
            time: now_ms() as u32,
        }),
    }
}

#[inline(never)]
fn produce_json_axis(i: &DecodedInputs) -> AnchorEvent {
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.axis", "axis": 0, "value": i.scroll_vertical_120ths, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_axis(i: &DecodedInputs) -> AnchorEvent {
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::Axis {
            axis: 0,
            value: i.scroll_vertical_120ths as f64,
            time: now_ms() as u32,
        }),
    }
}

#[inline(never)]
fn produce_json_text(i: &DecodedInputs) -> AnchorEvent {
    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.text", "text": i.text, "time": now_ms()});
    AnchorEvent { message: AnchorMessage::Json(packet.to_string()) }
}

#[inline(never)]
fn produce_typed_text(i: &DecodedInputs) -> AnchorEvent {
    AnchorEvent {
        message: AnchorMessage::Input(InputPluginMsg::Text {
            text: i.text.clone(),
            time: now_ms() as u32,
        }),
    }
}

// ---- consumer side: sink standing in for `WaylandInput` --------------------
// Each method does the minimum real work (touch every extracted field) so the
// compiler cannot elide the field reads.

#[derive(Default)]
struct Sink {
    acc_f64: f64,
    acc_u64: u64,
    events: u64,
}

impl Sink {
    fn motion(&mut self, time: u32, dx: f64, dy: f64) {
        self.acc_f64 += dx + dy + f64::from(time);
        self.events += 1;
    }
    fn motion_absolute(&mut self, time: u32, x: f64, y: f64, output_name: Option<&str>) {
        self.acc_f64 += x + y + f64::from(time);
        self.acc_u64 += output_name.map_or(0, |n| n.len() as u64);
        self.events += 1;
    }
    fn button(&mut self, time: u32, button: u32, pressed: bool) {
        self.acc_u64 += u64::from(button) + u64::from(time) + u64::from(pressed);
        self.events += 1;
    }
    fn axis(&mut self, time: u32, horizontal: bool, value: f64) {
        self.acc_f64 += value + f64::from(time) + f64::from(u8::from(horizontal));
        self.events += 1;
    }
    fn key_hid(&mut self, time: u32, hid_usage: u32, pressed: bool) {
        self.acc_u64 += u64::from(hid_usage) + u64::from(time) + u64::from(pressed);
        self.events += 1;
    }
    fn type_text(&mut self, time: u32, text: &str) {
        self.acc_u64 += text.len() as u64 + u64::from(time);
        self.events += 1;
    }
}

// Consumer replicas. `consume_json` mirrors input_plugin::run_input_loop's
// parse + get + as_* field extraction. `consume_typed` is the candidate match.
#[inline(never)]
fn consume_json(event: &AnchorEvent, sink: &mut Sink) {
    let json_str = match &event.message {
        AnchorMessage::Json(s) => s.as_str(),
        _ => return,
    };
    let payload: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return,
    };
    let msg_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };
    let time = payload.get("time").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
    match msg_type {
        "anchor.input.motion" => {
            let dx = payload.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let dy = payload.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
            sink.motion(time, dx, dy);
        }
        "anchor.input.motion_absolute" => {
            let x = payload.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = payload.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let output_name =
                payload.get("output_name").and_then(|v| v.as_str()).filter(|name| !name.is_empty());
            sink.motion_absolute(time, x, y, output_name);
        }
        "anchor.input.button" => {
            let button = payload.get("button").and_then(|v| v.as_u64()).unwrap_or(272) as u32;
            let pressed = payload.get("state").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
            sink.button(time, button, pressed);
        }
        "anchor.input.axis" => {
            let axis_val = payload.get("axis").and_then(|v| v.as_u64()).unwrap_or(0);
            let value = payload.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            sink.axis(time, axis_val == 1, value);
        }
        "anchor.input.text" => {
            if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
                sink.type_text(time, text);
            }
        }
        "anchor.input.key_hid" => {
            let hid_usage = payload.get("hid_usage").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let pressed = payload.get("pressed").and_then(|v| v.as_bool()).unwrap_or(false);
            sink.key_hid(time, hid_usage, pressed);
        }
        _ => {}
    }
}

#[inline(never)]
fn consume_typed(event: &AnchorEvent, sink: &mut Sink) {
    let msg = match &event.message {
        AnchorMessage::Input(m) => m,
        _ => return,
    };
    match msg {
        InputPluginMsg::MotionAbsolute { x, y, output_name, time } => {
            sink.motion_absolute(
                *time,
                *x,
                *y,
                (!output_name.is_empty()).then_some(output_name.as_str()),
            );
        }
        InputPluginMsg::MotionRelative { dx, dy, time } => sink.motion(*time, *dx, *dy),
        InputPluginMsg::Button { button, pressed, time } => sink.button(*time, *button, *pressed),
        InputPluginMsg::Axis { axis, value, time } => sink.axis(*time, *axis == 1, *value),
        InputPluginMsg::KeyHid { hid_usage, pressed, time } => {
            sink.key_hid(*time, *hid_usage, *pressed)
        }
        InputPluginMsg::Text { text, time } => sink.type_text(*time, text),
    }
}

// ---- measurement harness ----------------------------------------------------

const DEFAULT_ITERS: usize = 200_000;

struct Summary {
    count: usize,
    mean_ns: f64,
    p50_ns: u128,
    p95_ns: u128,
    p99_ns: u128,
    max_ns: u128,
}

fn summarize(samples: &[u128]) -> Summary {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let pct = |f: f64| sorted[((sorted.len() - 1) as f64 * f).round() as usize];
    let total: u128 = sorted.iter().sum();
    Summary {
        count: sorted.len(),
        mean_ns: total as f64 / sorted.len() as f64,
        p50_ns: pct(0.50),
        p95_ns: pct(0.95),
        p99_ns: pct(0.99),
        max_ns: *sorted.last().unwrap(),
    }
}

fn fmt_ns(ns: f64) -> String {
    if ns >= 1000.0 { format!("{:.2} us", ns / 1000.0) } else { format!("{ns:.0} ns") }
}

fn print_summary(label: &str, s: &Summary) {
    println!(
        "  {label:<14} mean {:>9}  p50 {:>9}  p95 {:>9}  p99 {:>9}  max {:>9}  (n={})",
        fmt_ns(s.mean_ns),
        fmt_ns(s.p50_ns as f64),
        fmt_ns(s.p95_ns as f64),
        fmt_ns(s.p99_ns as f64),
        fmt_ns(s.max_ns as f64),
        s.count,
    );
}

type ProduceFn = fn(&DecodedInputs) -> AnchorEvent;

/// One event kind: interleaved measurement of json vs typed pipelines.
/// Each sample times produce+send+recv+consume as one unit, plus separate
/// produce-only and consume-only breakdowns.
fn bench_event(name: &str, iters: usize, produce_json: ProduceFn, produce_typed: ProduceFn) {
    let inputs = DecodedInputs::default();
    let mut sink_json = Sink::default();
    let mut sink_typed = Sink::default();

    // Pre-build consume-side fixtures: the JSON strings and typed events the
    // plugin thread would receive.
    let json_fixtures: Vec<AnchorEvent> = (0..1024).map(|_| produce_json(&inputs)).collect();
    let typed_fixtures: Vec<AnchorEvent> = (0..1024).map(|_| produce_typed(&inputs)).collect();

    let mut rt_json = Vec::with_capacity(iters);
    let mut rt_typed = Vec::with_capacity(iters);
    let mut prod_json = Vec::with_capacity(iters);
    let mut prod_typed = Vec::with_capacity(iters);
    let mut cons_json = Vec::with_capacity(iters);
    let mut cons_typed = Vec::with_capacity(iters);

    // Warm up both paths and both allocators.
    for fixture in json_fixtures.iter().take(1024) {
        consume_json(fixture, &mut sink_json);
    }
    for fixture in typed_fixtures.iter().take(1024) {
        consume_typed(fixture, &mut sink_typed);
    }

    // Consume-only: parse/dispatch on a fixture the plugin already received.
    for i in 0..iters {
        let fj = &json_fixtures[i % json_fixtures.len()];
        let t = Instant::now();
        consume_json(black_box(fj), &mut sink_json);
        cons_json.push(t.elapsed().as_nanos());

        let ft = &typed_fixtures[i % typed_fixtures.len()];
        let t = Instant::now();
        consume_typed(black_box(ft), &mut sink_typed);
        cons_typed.push(t.elapsed().as_nanos());
    }

    // Produce-only: message construction on the sdk_server side.
    for _ in 0..iters {
        let t = Instant::now();
        let ev = produce_json(&inputs);
        prod_json.push(t.elapsed().as_nanos());
        black_box(&ev);

        let t = Instant::now();
        let ev = produce_typed(&inputs);
        prod_typed.push(t.elapsed().as_nanos());
        black_box(&ev);
    }

    // Roundtrip: produce -> channel -> consume, single thread, one event at a
    // time (matches production: at most a few events in flight at 1kHz).
    let (tx, rx): (Sender<AnchorEvent>, Receiver<AnchorEvent>) = channel();
    for _ in 0..iters {
        let t = Instant::now();
        let _ = tx.send(produce_json(&inputs));
        let ev = rx.recv().unwrap();
        consume_json(black_box(&ev), &mut sink_json);
        rt_json.push(t.elapsed().as_nanos());

        let t = Instant::now();
        let _ = tx.send(produce_typed(&inputs));
        let ev = rx.recv().unwrap();
        consume_typed(black_box(&ev), &mut sink_typed);
        rt_typed.push(t.elapsed().as_nanos());
    }

    println!("{name}:");
    let sj = summarize(&prod_json);
    let st = summarize(&prod_typed);
    print_summary("produce json", &sj);
    print_summary("produce typed", &st);
    let sj = summarize(&cons_json);
    let st = summarize(&cons_typed);
    print_summary("consume json", &sj);
    print_summary("consume typed", &st);
    let sj = summarize(&rt_json);
    let st = summarize(&rt_typed);
    print_summary("roundtrip json", &sj);
    print_summary("roundtrip typed", &st);
    println!(
        "  roundtrip delta: mean {:.1}x faster ({} -> {})",
        sj.mean_ns / st.mean_ns.max(1.0),
        fmt_ns(sj.mean_ns),
        fmt_ns(st.mean_ns),
    );
    // Equivalence check: both pipelines must drive the sink identically.
    assert_eq!(sink_json.events, sink_typed.events, "event count mismatch");
    println!();
}

fn main() {
    let iters =
        std::env::args().nth(1).and_then(|a| a.parse::<usize>().ok()).unwrap_or(DEFAULT_ITERS);
    println!("input-bench: {iters} iterations per pipeline per event type\n");

    bench_event(
        "motion_absolute",
        iters,
        produce_json_motion_absolute,
        produce_typed_motion_absolute,
    );
    bench_event(
        "motion_relative",
        iters,
        produce_json_motion_relative,
        produce_typed_motion_relative,
    );
    bench_event("key_hid", iters, produce_json_key_hid, produce_typed_key_hid);
    bench_event("button", iters, produce_json_button, produce_typed_button);
    bench_event("axis", iters, produce_json_axis, produce_typed_axis);
    bench_event("text", iters, produce_json_text, produce_typed_text);
}
