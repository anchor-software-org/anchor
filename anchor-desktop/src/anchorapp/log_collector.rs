use log::{Level, LevelFilter, Metadata, Record};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Unix timestamp in milliseconds
    pub timestamp_ms: u64,
    /// "desktop" or "android"
    pub source: &'static str,
    pub level: String,
    pub message: String,
}

pub type LogBuffer = Arc<Mutex<Vec<LogEntry>>>;

/// A log::Log implementation that writes to a shared buffer, stderr, AND a file.
struct SharedLogger {
    buffer: LogBuffer,
    file: Mutex<File>,
}

const ALLOWED_PREFIXES: &[&str] = &["anchor"];

impl log::Log for SharedLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
            && ALLOWED_PREFIXES.iter().any(|p| metadata.target().starts_with(p))
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let msg = format!("{}", record.args());
        // Always print to stderr as well
        eprintln!("[{}] {}: {}", record.level(), record.target(), msg);

        // Write to log file
        if let Ok(mut f) = self.file.lock() {
            let ts = now_ms();
            let _ = writeln!(f, "{} [{}] {}: {}", ts, record.level(), record.target(), msg);
            let _ = f.flush();
        }

        let entry = LogEntry {
            timestamp_ms: now_ms(),
            source: "desktop",
            level: record.level().to_string(),
            message: format!("[{}] {}", record.target(), msg),
        };
        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(entry);
            // Keep at most 2000 entries to avoid unbounded growth
            if buf.len() > 2000 {
                buf.drain(0..500);
            }
        }
    }

    fn flush(&self) {}
}

/// Initialise the shared logger. Call once at startup before any log:: calls.
/// Returns the shared buffer to pass to the GUI.
/// Logs are also written to /tmp/anchor.log (truncated on each launch).
pub fn init_logger(max_level: LevelFilter) -> LogBuffer {
    let buffer: LogBuffer = Arc::new(Mutex::new(Vec::new()));
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("/tmp/anchor.log")
        .expect("Failed to open /tmp/anchor.log for writing");
    let logger = Box::new(SharedLogger { buffer: buffer.clone(), file: Mutex::new(file) });
    log::set_boxed_logger(logger).expect("logger already set");
    log::set_max_level(max_level);
    buffer
}

/// Spawn a background thread that tails `adb logcat` filtered to anchor tags
/// and pushes entries into the shared buffer.
pub fn start_adb_logcat_collector(buffer: LogBuffer) {
    thread::spawn(move || {
        loop {
            // -s = silent (only show selected tags), -v time = include timestamp
            // Tags: anchor (our custom tag), anchor.timing, AndroidRuntime for crashes
            let child = Command::new("adb")
                .args([
                    "logcat",
                    "-v",
                    "threadtime",
                    "-s",
                    "anchor:V",
                    "anchor.timing:V",
                    "VideoPlugin:V",
                    "NetworkPlugin:V",
                    "AndroidRuntime:E",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();

            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    log::debug!("adb logcat spawn failed (no device?): {}", e);
                    thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };

            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };

            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };

                let entry = parse_logcat_line(&line);
                if let Ok(mut buf) = buffer.lock() {
                    buf.push(entry);
                    if buf.len() > 2000 {
                        buf.drain(0..500);
                    }
                }
            }

            // logcat exited (device disconnected etc.) — wait and retry
            let _ = child.wait();
            log::debug!("adb logcat exited, retrying in 3s");
            thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}

fn parse_logcat_line(line: &str) -> LogEntry {
    // logcat threadtime format: "MM-DD HH:MM:SS.mmm PID TID LEVEL TAG: message"
    // We do a best-effort parse; fall back to raw line if it doesn't match.
    let level = if line.contains(" E ") {
        "ERROR"
    } else if line.contains(" W ") {
        "WARN"
    } else if line.contains(" I ") {
        "INFO"
    } else if line.contains(" D ") {
        "DEBUG"
    } else {
        "TRACE"
    };

    LogEntry {
        timestamp_ms: now_ms(),
        source: "android",
        level: level.to_string(),
        message: line.to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
