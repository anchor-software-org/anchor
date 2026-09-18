//! Measures Anchor's UDP frame fragmentation and reassembly protocol on a
//! loopback connection. This is intentionally separate from capture and
//! encoding: input consists of deterministic encoded-frame-sized byte buffers.

use std::collections::HashMap;
use std::env;
use std::io;
use std::net::UdpSocket;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

// Keep these in lockstep with src/anchorapp/device/video_server.rs.
const HEADER_SIZE: usize = 12;
const SEND_PACKET_SIZE: usize = 1200;
const SEND_PAYLOAD_SIZE: usize = SEND_PACKET_SIZE - HEADER_SIZE;
const DEFAULT_FRAME_SIZE: usize = 27 * 1024;
const DEFAULT_FRAMES: usize = 600;
const DEFAULT_FPS: u32 = 60;

#[derive(Debug)]
struct Config {
    frames: usize,
    frame_size: usize,
    fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self { frames: DEFAULT_FRAMES, frame_size: DEFAULT_FRAME_SIZE, fps: DEFAULT_FPS }
    }
}

fn usage() -> &'static str {
    "Usage: cargo run --release -- [--frames N] [--frame-bytes N] [--fps N]\n\
     \n\
     Runs an Anchor-format UDP sender and reassembler over 127.0.0.1. --fps 0\n\
     sends without pacing. The default models 600 frames of roughly 27 KiB at 60 FPS."
}

fn parse_usize(name: &str, value: Option<String>) -> Result<usize, String> {
    let value = value.ok_or_else(|| format!("{name} needs a value"))?;
    let parsed = value.parse().map_err(|_| format!("invalid {name} value: {value}"))?;
    if parsed == 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => config.frames = parse_usize("--frames", args.next())?,
            "--frame-bytes" => config.frame_size = parse_usize("--frame-bytes", args.next())?,
            "--fps" => {
                config.fps = args
                    .next()
                    .ok_or("--fps needs a value")?
                    .parse()
                    .map_err(|_| "--fps must be a non-negative integer")?
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

fn percentile_ns(sorted_ns: &[u128], percentile: f64) -> u128 {
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

fn build_header(
    frame_id: u32,
    fragment_index: u16,
    fragment_count: u16,
    frame_size: u32,
) -> [u8; HEADER_SIZE] {
    let mut header = [0; HEADER_SIZE];
    header[0..4].copy_from_slice(&frame_id.to_le_bytes());
    header[4..6].copy_from_slice(&fragment_index.to_le_bytes());
    header[6..8].copy_from_slice(&fragment_count.to_le_bytes());
    header[8..12].copy_from_slice(&frame_size.to_le_bytes());
    header
}

fn parse_header(packet: &[u8]) -> Option<(u32, u16, u16, usize)> {
    if packet.len() < HEADER_SIZE {
        return None;
    }
    let frame_id = u32::from_le_bytes(packet[0..4].try_into().ok()?);
    let fragment_index = u16::from_le_bytes(packet[4..6].try_into().ok()?);
    let fragment_count = u16::from_le_bytes(packet[6..8].try_into().ok()?);
    let frame_size = u32::from_le_bytes(packet[8..12].try_into().ok()?) as usize;
    (fragment_count > 0 && fragment_index < fragment_count).then_some((
        frame_id,
        fragment_index,
        fragment_count,
        frame_size,
    ))
}

struct Assembly {
    expected_size: usize,
    fragments: Vec<Option<Vec<u8>>>,
    received: usize,
}

impl Assembly {
    fn new(expected_size: usize, fragment_count: u16) -> Self {
        Self { expected_size, fragments: (0..fragment_count).map(|_| None).collect(), received: 0 }
    }

    fn insert(&mut self, index: u16, payload: &[u8]) -> bool {
        let slot = &mut self.fragments[index as usize];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            self.received += 1;
        }
        self.received == self.fragments.len()
    }

    fn validate_size(&self) -> bool {
        self.fragments.iter().flatten().map(Vec::len).sum::<usize>() == self.expected_size
    }
}

#[derive(Debug)]
struct ReceivedFrame {
    frame_id: u32,
    latency: Duration,
    valid: bool,
}

fn spawn_receiver(
    socket: UdpSocket,
    expected_frames: usize,
    sent_at: Arc<Mutex<HashMap<u32, Instant>>>,
    tx: mpsc::Sender<ReceivedFrame>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let mut packet = [0u8; SEND_PACKET_SIZE];
        let mut assemblies = HashMap::<u32, Assembly>::new();
        let mut completed = 0usize;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        while completed < expected_frames {
            let (len, _) = socket.recv_from(&mut packet)?;
            let Some((frame_id, fragment_index, fragment_count, frame_size)) =
                parse_header(&packet[..len])
            else {
                continue;
            };
            let entry = assemblies
                .entry(frame_id)
                .or_insert_with(|| Assembly::new(frame_size, fragment_count));
            if entry.fragments.len() != fragment_count as usize || entry.expected_size != frame_size
            {
                assemblies.remove(&frame_id);
                continue;
            }
            if entry.insert(fragment_index, &packet[HEADER_SIZE..len]) {
                let assembly = assemblies.remove(&frame_id).expect("assembly disappeared");
                let sent = sent_at.lock().unwrap().remove(&frame_id);
                if let Some(sent) = sent {
                    tx.send(ReceivedFrame {
                        frame_id,
                        latency: sent.elapsed(),
                        valid: assembly.validate_size(),
                    })
                    .ok();
                }
                completed += 1;
            }
        }
        Ok(())
    })
}

fn run(config: Config) -> Result<(), String> {
    let receiver_socket = UdpSocket::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let receiver_addr = receiver_socket.local_addr().map_err(|error| error.to_string())?;
    let sender = UdpSocket::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    sender.connect(receiver_addr).map_err(|error| error.to_string())?;

    let sent_at = Arc::new(Mutex::new(HashMap::new()));
    let (received_tx, received_rx) = mpsc::channel();
    let receiver =
        spawn_receiver(receiver_socket, config.frames, Arc::clone(&sent_at), received_tx);
    let mut frame = vec![0u8; config.frame_size];
    let mut packet = [0u8; SEND_PACKET_SIZE];
    let mut send_times = Vec::with_capacity(config.frames);
    let started = Instant::now();
    let period = (config.fps > 0).then(|| Duration::from_secs_f64(1.0 / config.fps as f64));

    for sequence in 1..=config.frames as u32 {
        // Avoid a perfectly uniform all-zero input, while retaining deterministic
        // payload bytes so the reassembler can be checked without a codec.
        frame.fill(sequence as u8);
        let frame_started = Instant::now();
        sent_at.lock().unwrap().insert(sequence, frame_started);
        let fragment_count = frame.len().div_ceil(SEND_PAYLOAD_SIZE).max(1) as u16;

        for fragment_index in 0..fragment_count {
            let offset = fragment_index as usize * SEND_PAYLOAD_SIZE;
            let end = (offset + SEND_PAYLOAD_SIZE).min(frame.len());
            let header = build_header(sequence, fragment_index, fragment_count, frame.len() as u32);
            packet[..HEADER_SIZE].copy_from_slice(&header);
            packet[HEADER_SIZE..HEADER_SIZE + end - offset].copy_from_slice(&frame[offset..end]);
            sender
                .send(&packet[..HEADER_SIZE + end - offset])
                .map_err(|error| format!("UDP send failed: {error}"))?;
        }
        send_times.push(frame_started.elapsed());

        if let Some(period) = period {
            let remaining = period.saturating_sub(frame_started.elapsed());
            if !remaining.is_zero() {
                thread::sleep(remaining);
            }
        }
    }

    let mut receive_times = Vec::with_capacity(config.frames);
    let mut valid_frames = 0usize;
    let mut last_frame_id = 0u32;
    for _ in 0..config.frames {
        let received = received_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| format!("did not receive all frames: {error}"))?;
        valid_frames += usize::from(received.valid);
        last_frame_id = last_frame_id.max(received.frame_id);
        receive_times.push(received.latency);
    }
    receiver
        .join()
        .map_err(|_| "receiver thread panicked".to_string())?
        .map_err(|error| format!("receiver error: {error}"))?;

    let elapsed = started.elapsed();
    let fragments_per_frame = config.frame_size.div_ceil(SEND_PAYLOAD_SIZE).max(1);
    let total_payload = config.frames * config.frame_size;

    println!("Anchor UDP transport benchmark (loopback)");
    println!(
        "  frame size: {} bytes; fragments/frame: {}; UDP payload/fragment: {} bytes",
        config.frame_size, fragments_per_frame, SEND_PAYLOAD_SIZE
    );
    println!(
        "  frames: {}; pacing: {}",
        config.frames,
        if config.fps == 0 { "unpaced".into() } else { format!("{} fps", config.fps) }
    );
    println!();
    print_summary("sender fragment-and-send", &summarize(&send_times));
    print_summary("last-fragment arrival and reassembly", &summarize(&receive_times));
    println!(
        "\nreceived frames: {valid_frames}/{} valid; highest frame id: {last_frame_id}",
        config.frames
    );
    println!(
        "effective frame rate: {:.2} fps; payload rate: {:.2} Mbit/s; datagram rate: {:.0}/s",
        config.frames as f64 / elapsed.as_secs_f64(),
        total_payload as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0,
        config.frames as f64 * fragments_per_frame as f64 / elapsed.as_secs_f64()
    );
    println!(
        "Interpretation: loopback measures Anchor-format fragmentation and kernel delivery without Wi-Fi, VPN, phone radio, or Android decode latency."
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
            eprintln!("video-transport-bench: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let header = build_header(42, 3, 9, 12_345);
        assert_eq!(parse_header(&header), Some((42, 3, 9, 12_345)));
    }

    #[test]
    fn reassembly_rejects_wrong_total_size() {
        let mut assembly = Assembly::new(4, 2);
        assert!(!assembly.insert(0, &[1, 2]));
        assert!(assembly.insert(1, &[3]));
        assert!(!assembly.validate_size());
    }
}
