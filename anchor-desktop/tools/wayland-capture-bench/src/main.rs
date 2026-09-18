//! Measures only Anchor's Wayland screencopy capture path for output index 0.
//!
//! The benchmark intentionally performs no encoding, packetization, preview,
//! logging, or network I/O. Each frame is dropped as soon as its metadata has
//! been sampled, so there is exactly one capture buffer in flight.

// The imported production modules contain capabilities that this deliberately
// narrow benchmark does not use (for example CPU frames and output refresh).
#![allow(dead_code)]

mod anchorwayland;

use anchorwayland::capture_backend::CaptureBackend;
use anchorwayland::screencopy::{ScreencopyMode, WaylandScreencopyBackend};
use std::env;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const DEFAULT_FRAMES: usize = 600;
const DEFAULT_WARMUP_FRAMES: usize = 60;

#[derive(Debug)]
struct Config {
    frames: Option<usize>,
    seconds: Option<Duration>,
    warmup_frames: usize,
    mode: ScreencopyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasurementKind {
    Frames,
    Seconds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            frames: Some(DEFAULT_FRAMES),
            seconds: None,
            warmup_frames: DEFAULT_WARMUP_FRAMES,
            mode: ScreencopyMode::EveryFrame,
        }
    }
}

fn usage() -> &'static str {
    "Usage: cargo run --release -- [--frames N | --seconds S] [--warmup N] [--damage-only]\n\
     \n\
     Measures Anchor's current wlr-screencopy + DMA-BUF capture path on the\n\
     first Wayland output. The default is 60 warmup frames and 600 measured\n\
     frames. `--damage-only` uses wlr-screencopy's copy_with_damage request;\n\
     it blocks until the output changes. No frames are encoded or transmitted."
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
    let mut measurement_kind = None;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => {
                if measurement_kind.is_some() {
                    return Err("--frames and --seconds cannot be used together".into());
                }
                config.frames = Some(parse_positive("--frames", args.next())?);
                measurement_kind = Some(MeasurementKind::Frames);
            }
            "--seconds" => {
                if measurement_kind.is_some() {
                    return Err("--frames and --seconds cannot be used together".into());
                }
                let seconds: f64 = parse_positive("--seconds", args.next())?;
                config.frames = None;
                config.seconds = Some(Duration::from_secs_f64(seconds));
                measurement_kind = Some(MeasurementKind::Seconds);
            }
            "--warmup" => config.warmup_frames = parse_positive("--warmup", args.next())?,
            "--damage-only" => config.mode = ScreencopyMode::OnlyOnDamage,
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

fn percentile_ns(sorted_ns: &[u128], percentile: f64) -> u128 {
    debug_assert!(!sorted_ns.is_empty());
    let index = ((sorted_ns.len() - 1) as f64 * percentile).round() as usize;
    sorted_ns[index]
}

fn summarize(samples: &[Duration]) -> Summary {
    assert!(!samples.is_empty());

    let mut sorted_ns = samples.iter().map(Duration::as_nanos).collect::<Vec<_>>();
    sorted_ns.sort_unstable();
    let total_ns: u128 = sorted_ns.iter().sum();

    Summary {
        count: sorted_ns.len(),
        mean: Duration::from_nanos((total_ns / sorted_ns.len() as u128) as u64),
        min: Duration::from_nanos(sorted_ns[0] as u64),
        p50: Duration::from_nanos(percentile_ns(&sorted_ns, 0.50) as u64),
        p95: Duration::from_nanos(percentile_ns(&sorted_ns, 0.95) as u64),
        p99: Duration::from_nanos(percentile_ns(&sorted_ns, 0.99) as u64),
        max: Duration::from_nanos(*sorted_ns.last().unwrap() as u64),
    }
}

fn format_ms(value: Duration) -> String {
    format!("{:.3} ms", value.as_secs_f64() * 1_000.0)
}

fn print_summary(name: &str, summary: &Summary) {
    println!("{name} ({} samples)", summary.count);
    println!(
        "  mean {}  min {}  p50 {}  p95 {}  p99 {}  max {}",
        format_ms(summary.mean),
        format_ms(summary.min),
        format_ms(summary.p50),
        format_ms(summary.p95),
        format_ms(summary.p99),
        format_ms(summary.max),
    );
}

fn print_tail_detail(samples: &[Duration]) {
    const THRESHOLDS_MS: [u64; 6] = [20, 25, 34, 50, 100, 250];

    println!("capture-tail counts");
    for threshold_ms in THRESHOLDS_MS {
        let threshold = Duration::from_millis(threshold_ms);
        let count = samples.iter().filter(|sample| **sample > threshold).count();
        println!("  > {threshold_ms:>3} ms: {count}");
    }

    let mut slowest = samples.iter().copied().enumerate().collect::<Vec<_>>();
    slowest.sort_unstable_by(|left, right| right.1.cmp(&left.1));
    println!("slowest capture calls (zero-based measured sample index)");
    for (index, duration) in slowest.into_iter().take(10) {
        println!("  {index:>5}: {}", format_ms(duration));
    }
}

fn run(config: Config) -> Result<(), String> {
    let setup_started = Instant::now();
    let mut capture = WaylandScreencopyBackend::try_new()?;
    capture.set_mode(config.mode);
    capture.init()?;
    let setup_time = setup_started.elapsed();

    let outputs = capture.get_outputs();
    let output = outputs.first().ok_or("No outputs available after capture initialization")?;
    println!("Anchor Wayland capture benchmark");
    println!("  output 0: {} ({})", output.name, output.description);
    println!(
        "  mode: {}x{} at reported {} mHz",
        output.width, output.height, output.refresh_rate_mhz
    );
    println!("  setup: {}", format_ms(setup_time));
    println!(
        "  request mode: {}",
        match capture.mode() {
            ScreencopyMode::EveryFrame => "copy (next compositor frame)",
            ScreencopyMode::OnlyOnDamage => "copy_with_damage (wait for output damage)",
        }
    );
    println!("  cursor: included; encoder/network/preview: disabled");
    println!("  capture buffers retained: 0 (one DMA-BUF is reused by the backend)");

    for _ in 0..config.warmup_frames {
        let _frame = capture.capture_frame(0)?;
    }

    let mut capture_call_times = Vec::new();
    let mut completion_intervals = Vec::new();
    let mut presentation_intervals = Vec::new();
    let measurement_started = Instant::now();
    let deadline = config.seconds.map(|duration| measurement_started + duration);
    let mut previous_completion = None;
    let mut previous_presentation = None;
    let mut first_frame_info = None;

    loop {
        if config.frames.is_some_and(|frames| capture_call_times.len() >= frames)
            || deadline.is_some_and(|at| Instant::now() >= at)
        {
            break;
        }

        let started = Instant::now();
        let frame = capture.capture_frame(0)?;
        let completed = Instant::now();
        capture_call_times.push(completed.duration_since(started));

        if let Some(previous) = previous_completion {
            completion_intervals.push(completed.duration_since(previous));
        }
        previous_completion = Some(completed);

        let presentation = capture
            .last_presentation_time_ns()
            .ok_or("Compositor sent ready without a valid presentation timestamp")?;
        if let Some(previous) = previous_presentation {
            if let Some(delta_ns) = presentation.checked_sub(previous) {
                presentation_intervals.push(Duration::from_nanos(delta_ns));
            }
        }
        previous_presentation = Some(presentation);

        if first_frame_info.is_none() {
            first_frame_info = Some((frame.width, frame.height, frame.stride, frame.format));
        }
        drop(frame);
    }

    let elapsed = measurement_started.elapsed();
    let calls = summarize(&capture_call_times);
    let effective_fps = capture_call_times.len() as f64 / elapsed.as_secs_f64();

    if let Some((width, height, stride, format)) = first_frame_info {
        println!("  captured frame: {width}x{height}, stride {stride}, format {format:?}");
    }
    println!();
    print_summary("capture call (request through compositor ready)", &calls);
    print_tail_detail(&capture_call_times);
    if !completion_intervals.is_empty() {
        print_summary("completion cadence", &summarize(&completion_intervals));
    }
    if !presentation_intervals.is_empty() {
        print_summary("compositor presentation cadence", &summarize(&presentation_intervals));
    }
    println!(
        "\neffective capture rate: {:.2} fps over {:.3} s",
        effective_fps,
        elapsed.as_secs_f64()
    );
    println!(
        "Interpretation: this is the ceiling of the current sequential capture stage; it excludes encoder, transport, decode, and presentation latency."
    );

    Ok(())
}

fn main() -> ExitCode {
    match parse_args().and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) if message == usage() => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("wayland-capture-bench: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::percentile_ns;

    #[test]
    fn percentile_uses_nearest_sample() {
        let samples = [1, 2, 3, 4, 5];
        assert_eq!(percentile_ns(&samples, 0.50), 3);
        assert_eq!(percentile_ns(&samples, 0.95), 5);
    }
}
