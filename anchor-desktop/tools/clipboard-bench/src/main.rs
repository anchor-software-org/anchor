use std::hint::black_box;
use std::time::{Duration, Instant};

use base64::Engine;
use flate2::Compression;
use flate2::write::DeflateEncoder;
use sha2::{Digest, Sha256};
use std::io::Write;

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Image,
}

struct Case {
    name: &'static str,
    kind: Kind,
    data: Vec<u8>,
}

fn prepare_packet(data: &[u8], kind: Kind) -> String {
    let hash = Sha256::digest(data);
    let hash = format!("{hash:x}");
    let (content_type, content) = match kind {
        Kind::Text => ("text", String::from_utf8_lossy(data).into_owned()),
        Kind::Image => (
            "image_png",
            base64::engine::general_purpose::STANDARD.encode(data),
        ),
    };

    let (content, compressed) = if matches!(kind, Kind::Text) && content.len() > 1024 {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        (
            base64::engine::general_purpose::STANDARD.encode(compressed),
            true,
        )
    } else {
        (content, false)
    };

    let mut packet = serde_json::json!({
        "plugin_id": "clipboard",
        "type": "clipboard_content",
        "content_type": content_type,
        "content": content,
        "timestamp": 1_700_000_000_000_i64,
        "hash": hash,
    });
    if compressed {
        packet["compressed"] = serde_json::json!("zlib");
    }
    packet.to_string()
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn parse_iterations() -> usize {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--iterations" {
            return args.next().and_then(|value| value.parse().ok()).unwrap_or(1000);
        }
    }
    1000
}

fn main() {
    let iterations = parse_iterations().max(1);
    let cases = [
        Case { name: "text-128B", kind: Kind::Text, data: vec![b'a'; 128] },
        Case { name: "text-16KiB", kind: Kind::Text, data: vec![b'a'; 16 * 1024] },
        Case { name: "text-1MiB", kind: Kind::Text, data: vec![b'a'; 1024 * 1024] },
        Case { name: "image-256KiB", kind: Kind::Image, data: (0..256 * 1024).map(|i| i as u8).collect() },
    ];

    println!("Anchor clipboard benchmark");
    println!("  iterations per case: {iterations}");
    println!();

    for case in cases {
        let mut samples = Vec::with_capacity(iterations);
        let mut output_bytes = 0usize;
        for _ in 0..iterations {
            let start = Instant::now();
            let packet = prepare_packet(black_box(&case.data), case.kind);
            output_bytes = black_box(packet.len());
            samples.push(start.elapsed());
        }
        samples.sort_unstable();
        let total: Duration = samples.iter().copied().sum();
        let mean = total / iterations as u32;
        println!(
            "{}: input={} B output={} B | mean {:.3} ms p50 {:.3} ms p95 {:.3} ms p99 {:.3} ms",
            case.name,
            case.data.len(),
            output_bytes,
            mean.as_secs_f64() * 1000.0,
            percentile(&samples, 0.50).as_secs_f64() * 1000.0,
            percentile(&samples, 0.95).as_secs_f64() * 1000.0,
            percentile(&samples, 0.99).as_secs_f64() * 1000.0,
        );
    }
}
