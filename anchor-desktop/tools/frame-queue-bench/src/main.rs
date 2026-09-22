//! Measures frame admission and dequeue age under a slow sender.
//!
//! The `anchor` policy imports Anchor's production `FrameBroadcaster` and
//! reproduces the UDP drain's "take one, then coalesce newer queued frames"
//! behavior. `latest` is a deliberately separate, latest-wins single-slot
//! experiment: it never lets a stale pending frame prevent a newer one from
//! entering the queue.

#![allow(dead_code)]

// `FrameBroadcaster` emits runtime trace events. The benchmark measures its
// queue behavior, so preserve the production type while making tracing a
// no-op instead of pulling the full desktop capture stack into this crate.
mod anchorwayland {
    pub mod frame_trace {
        use std::sync::OnceLock;
        use std::time::Instant;

        pub fn mono_ns() -> u64 {
            static START: OnceLock<Instant> = OnceLock::new();
            START
                .get_or_init(Instant::now)
                .elapsed()
                .as_nanos()
                .min(u64::MAX as u128) as u64
        }

        // Production callers use `frame_trace::event!`, a macro that skips
        // building the JSON payload when tracing is off. The stub matches the
        // macro shape (no-op) rather than a function so `event!` resolves.
        macro_rules! event {
            ($kind:expr, $($fields:tt)*) => {{}};
        }
        pub(crate) use event;
    }
}

#[path = "../../../src/anchorapp/device/broadcaster.rs"]
mod broadcaster;

use broadcaster::FrameBroadcaster;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Condvar, Mutex, mpsc::Receiver};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_FRAMES: usize = 600;
const DEFAULT_FPS: u32 = 60;
const DEFAULT_QUEUE_DEPTH: usize = 2;
const DEFAULT_FRAME_BYTES: usize = 27 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Anchor,
    Latest,
    Both,
}

#[derive(Debug)]
struct Config {
    frames: usize,
    fps: u32,
    queue_depth: usize,
    frame_bytes: usize,
    drain_delay: Duration,
    policy: Policy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            frames: DEFAULT_FRAMES,
            fps: DEFAULT_FPS,
            queue_depth: DEFAULT_QUEUE_DEPTH,
            frame_bytes: DEFAULT_FRAME_BYTES,
            drain_delay: Duration::ZERO,
            policy: Policy::Both,
        }
    }
}

fn usage() -> &'static str {
    "Usage: cargo run --release -- [--frames N] [--fps N] [--queue-depth N] [--frame-bytes N] [--drain-ms N] [--policy anchor|latest|both]\n\
     \n\
     Produces encoded-frame-sized buffers at a fixed FPS. --drain-ms models time\n\
     spent by a device drain on one frame (for example a blocked socket write).\n\
     The default runs both policies with Anchor's UDP queue depth of two."
}

fn parse_positive<T>(name: &str, value: Option<String>) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + From<u8>,
{
    let value = value.ok_or_else(|| format!("{name} needs a value"))?;
    let parsed = value.parse::<T>().map_err(|_| format!("invalid {name} value: {value}"))?;
    if parsed <= T::from(0) {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => config.frames = parse_positive("--frames", args.next())?,
            "--fps" => config.fps = parse_positive("--fps", args.next())?,
            "--queue-depth" => config.queue_depth = parse_positive("--queue-depth", args.next())?,
            "--frame-bytes" => config.frame_bytes = parse_positive("--frame-bytes", args.next())?,
            "--drain-ms" => {
                let milliseconds = args
                    .next()
                    .ok_or("--drain-ms needs a value")?
                    .parse::<u64>()
                    .map_err(|_| "--drain-ms must be a non-negative integer")?;
                config.drain_delay = Duration::from_millis(milliseconds);
            }
            "--policy" => {
                config.policy = match args.next().as_deref() {
                    Some("anchor") => Policy::Anchor,
                    Some("latest") => Policy::Latest,
                    Some("both") => Policy::Both,
                    _ => return Err("--policy must be anchor, latest, or both".into()),
                }
            }
            "-h" | "--help" => return Err(usage().into()),
            _ => return Err(format!("unknown argument: {arg}\n\n{}", usage())),
        }
    }
    Ok(config)
}

#[derive(Debug)]
struct Summary {
    count: usize,
    mean: Duration,
    min: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
}

fn summarize(samples: &[Duration]) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.iter().map(Duration::as_nanos).collect::<Vec<_>>();
    sorted.sort_unstable();
    let percentile =
        |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction).round() as usize];
    let total: u128 = sorted.iter().sum();
    Some(Summary {
        count: sorted.len(),
        mean: Duration::from_nanos((total / sorted.len() as u128) as u64),
        min: Duration::from_nanos(sorted[0] as u64),
        p50: Duration::from_nanos(percentile(0.50) as u64),
        p95: Duration::from_nanos(percentile(0.95) as u64),
        p99: Duration::from_nanos(percentile(0.99) as u64),
        max: Duration::from_nanos(*sorted.last().expect("non-empty") as u64),
    })
}

fn print_summary(name: &str, samples: &[Duration]) {
    let Some(summary) = summarize(samples) else {
        println!("{name}: no frames delivered");
        return;
    };
    let milliseconds = |value: Duration| value.as_secs_f64() * 1_000.0;
    println!("{name} ({} samples)", summary.count);
    println!(
        "  mean {:.3} ms  min {:.3} ms  p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms  max {:.3} ms",
        milliseconds(summary.mean),
        milliseconds(summary.min),
        milliseconds(summary.p50),
        milliseconds(summary.p95),
        milliseconds(summary.p99),
        milliseconds(summary.max),
    );
}

#[derive(Debug)]
struct Delivered {
    frame_id: u32,
    queue_age: Duration,
    send_complete_age: Duration,
}

#[derive(Debug)]
struct PolicyResult {
    policy_name: &'static str,
    produced: usize,
    admission_rejections: usize,
    coalesced_or_replaced: usize,
    delivered: Vec<Delivered>,
    publish_times: Vec<Duration>,
    elapsed: Duration,
}

impl PolicyResult {
    fn print(&self, config: &Config) {
        println!("{}", self.policy_name);
        println!(
            "  produced: {}; admitted: {}; rejected at admission: {}; replaced/coalesced before send: {}; delivered: {}",
            self.produced,
            self.produced.saturating_sub(self.admission_rejections),
            self.admission_rejections,
            self.coalesced_or_replaced,
            self.delivered.len(),
        );
        let gaps = self
            .delivered
            .iter()
            .scan(0u32, |previous, delivered| {
                let gap = delivered.frame_id.saturating_sub(*previous).saturating_sub(1);
                *previous = delivered.frame_id;
                Some(gap as usize)
            })
            .sum::<usize>();
        println!(
            "  output frame-ID gaps: {gaps}; effective send rate: {:.2} fps",
            self.delivered.len() as f64 / self.elapsed.as_secs_f64()
        );
        print_summary("  publish / enter-queue", &self.publish_times);
        let queue_ages = self.delivered.iter().map(|frame| frame.queue_age).collect::<Vec<_>>();
        let complete_ages =
            self.delivered.iter().map(|frame| frame.send_complete_age).collect::<Vec<_>>();
        print_summary("  production to dequeue", &queue_ages);
        print_summary("  production to simulated send completion", &complete_ages);
        println!(
            "  drain time modeled per sent frame: {:.3} ms; source rate: {} fps; frame payload: {} bytes\n",
            config.drain_delay.as_secs_f64() * 1_000.0,
            config.fps,
            config.frame_bytes,
        );
    }
}

fn make_frame(frame_id: u32, frame_bytes: usize) -> Vec<u8> {
    let mut frame = vec![0u8; frame_bytes.max(4)];
    frame[..4].copy_from_slice(&frame_id.to_le_bytes());
    frame
}

fn frame_id(frame: &[u8]) -> u32 {
    u32::from_le_bytes(frame[..4].try_into().expect("benchmark frame id"))
}

fn pace(started: Instant, frame_number: usize, fps: u32) {
    let deadline = started + Duration::from_secs_f64(frame_number as f64 / f64::from(fps));
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

fn drain_anchor(
    rx: Receiver<Arc<Vec<u8>>>,
    timestamps: Arc<Mutex<HashMap<u32, Instant>>>,
    drain_delay: Duration,
) -> (Vec<Delivered>, usize) {
    let mut delivered = Vec::new();
    let mut coalesced = 0usize;

    // This is the same dequeue rule as `udp_drain_thread`: receive one frame,
    // then discard everything already queued in favour of the newest frame.
    while let Ok(initial) = rx.recv() {
        let mut newest = initial;
        while let Ok(newer) = rx.try_recv() {
            newest = newer;
            coalesced += 1;
        }
        let id = frame_id(&newest);
        let produced = timestamps.lock().unwrap()[&id];
        let dequeued = Instant::now();
        if !drain_delay.is_zero() {
            thread::sleep(drain_delay);
        }
        delivered.push(Delivered {
            frame_id: id,
            queue_age: dequeued.duration_since(produced),
            send_complete_age: produced.elapsed(),
        });
    }
    (delivered, coalesced)
}

fn run_anchor(config: &Config) -> PolicyResult {
    let broadcaster = Arc::new(FrameBroadcaster::new());
    let rx = broadcaster.subscribe("benchmark".to_string(), config.queue_depth);
    let timestamps = Arc::new(Mutex::new(HashMap::with_capacity(config.frames)));
    let drain_timestamps = Arc::clone(&timestamps);
    let drain_delay = config.drain_delay;
    let drain = thread::spawn(move || drain_anchor(rx, drain_timestamps, drain_delay));

    let mut admission_rejections = 0usize;
    let mut publish_times = Vec::with_capacity(config.frames);
    let started = Instant::now();
    for index in 0..config.frames {
        let id = (index + 1) as u32;
        let frame = make_frame(id, config.frame_bytes);
        timestamps.lock().unwrap().insert(id, Instant::now());
        let publish_started = Instant::now();
        if !broadcaster.publish(frame) {
            admission_rejections += 1;
        }
        publish_times.push(publish_started.elapsed());
        pace(started, index + 1, config.fps);
    }
    broadcaster.unsubscribe("benchmark");
    let (delivered, coalesced) = drain.join().expect("anchor drain thread panicked");

    PolicyResult {
        policy_name: "Anchor current policy: bounded FIFO admission + UDP drain coalescing",
        produced: config.frames,
        admission_rejections,
        coalesced_or_replaced: coalesced,
        delivered,
        publish_times,
        elapsed: started.elapsed(),
    }
}

#[derive(Debug)]
struct LatestFrame {
    frame_id: u32,
    produced: Instant,
    // Retain the encoded payload in the pending slot just as Anchor retains an
    // Arc<Vec<u8>> in its current per-device channel.
    payload: Arc<Vec<u8>>,
}

#[derive(Debug, Default)]
struct LatestState {
    frame: Option<LatestFrame>,
    closed: bool,
}

#[derive(Debug, Default)]
struct LatestQueue {
    state: Mutex<LatestState>,
    available: Condvar,
}

impl LatestQueue {
    /// Returns true if an unsent frame was replaced. This is the candidate
    /// latest-wins admission rule, not Anchor's current implementation.
    fn publish(&self, frame: LatestFrame) -> bool {
        let mut state = self.state.lock().unwrap();
        let replaced = state.frame.replace(frame).is_some();
        self.available.notify_one();
        replaced
    }

    fn recv(&self) -> Option<LatestFrame> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(frame) = state.frame.take() {
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            state = self.available.wait(state).unwrap();
        }
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        self.available.notify_all();
    }
}

fn drain_latest(queue: Arc<LatestQueue>, drain_delay: Duration) -> Vec<Delivered> {
    let mut delivered = Vec::new();
    while let Some(frame) = queue.recv() {
        let dequeued = Instant::now();
        let _payload_len = frame.payload.len();
        if !drain_delay.is_zero() {
            thread::sleep(drain_delay);
        }
        delivered.push(Delivered {
            frame_id: frame.frame_id,
            queue_age: dequeued.duration_since(frame.produced),
            send_complete_age: frame.produced.elapsed(),
        });
    }
    delivered
}

fn run_latest(config: &Config) -> PolicyResult {
    let queue = Arc::new(LatestQueue::default());
    let drain_queue = Arc::clone(&queue);
    let drain_delay = config.drain_delay;
    let drain = thread::spawn(move || drain_latest(drain_queue, drain_delay));

    let mut replaced = 0usize;
    let mut publish_times = Vec::with_capacity(config.frames);
    let started = Instant::now();
    for index in 0..config.frames {
        let id = (index + 1) as u32;
        let frame = LatestFrame {
            frame_id: id,
            produced: Instant::now(),
            payload: Arc::new(make_frame(id, config.frame_bytes)),
        };
        let publish_started = Instant::now();
        replaced += usize::from(queue.publish(frame));
        publish_times.push(publish_started.elapsed());
        pace(started, index + 1, config.fps);
    }
    queue.close();
    let delivered = drain.join().expect("latest drain thread panicked");

    PolicyResult {
        policy_name: "Candidate policy: latest-wins single pending slot",
        produced: config.frames,
        admission_rejections: 0,
        coalesced_or_replaced: replaced,
        delivered,
        publish_times,
        elapsed: started.elapsed(),
    }
}

fn run(config: Config) {
    println!("Anchor frame-queue benchmark");
    println!(
        "  {} frames at {} fps; queue depth {}; simulated drain delay {:.3} ms\n",
        config.frames,
        config.fps,
        config.queue_depth,
        config.drain_delay.as_secs_f64() * 1_000.0,
    );
    if matches!(config.policy, Policy::Anchor | Policy::Both) {
        run_anchor(&config).print(&config);
    }
    if matches!(config.policy, Policy::Latest | Policy::Both) {
        run_latest(&config).print(&config);
    }
    println!(
        "Interpretation: --drain-ms is an intentional controlled stall. It does not claim that UDP send_to normally blocks for that duration; it exposes how each admission/dequeue policy behaves when a device drain cannot keep up."
    );
}

fn main() {
    match parse_args() {
        Ok(config) => run(config),
        Err(message) if message == usage() => println!("{message}"),
        Err(message) => {
            eprintln!("frame-queue-bench: {message}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_queue_replaces_the_unsent_frame() {
        let queue = LatestQueue::default();
        assert!(!queue.publish(LatestFrame {
            frame_id: 1,
            produced: Instant::now(),
            payload: Arc::new(vec![1]),
        }));
        assert!(queue.publish(LatestFrame {
            frame_id: 2,
            produced: Instant::now(),
            payload: Arc::new(vec![2]),
        }));
        assert_eq!(queue.recv().expect("latest frame").frame_id, 2);
    }
}
