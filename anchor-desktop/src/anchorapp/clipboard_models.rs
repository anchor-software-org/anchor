use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// MIME content types we support for clipboard sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardContentType {
    #[serde(rename = "text/plain")]
    Text,
    #[serde(rename = "image/png")]
    ImagePng,
}

/// A single clipboard entry — stored in history and transmitted over the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub content_type: ClipboardContentType,
    /// For text: the UTF-8 string. For images: base64-encoded PNG.
    pub content: String,
    /// Milliseconds since UNIX epoch on the source device.
    pub timestamp: u64,
    /// SHA-256 hex digest of the raw content bytes (before base64 for images).
    pub hash: String,
    /// Device ID of the source.
    pub device_id: String,
}

/// Ring buffer of recent clipboard entries, shared between plugin and GUI.
pub type SharedClipboardHistory = Arc<Mutex<VecDeque<ClipboardEntry>>>;

/// Compute SHA-256 hex digest of the given bytes.
pub fn content_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let result = Sha256::digest(data);
    hex_encode(&result)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Current time in milliseconds since UNIX epoch.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash(b"hello"), content_hash(b"hello"));
    }

    #[test]
    fn content_hash_differs_for_different_input() {
        assert_ne!(content_hash(b"hello"), content_hash(b"world"));
    }

    #[test]
    fn content_hash_is_hex_encoded() {
        let hash = content_hash(b"test");
        // Must be 64 hex chars (SHA-256)
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hex_encode_roundtrip_via_known_value() {
        // SHA-256 of empty string is well-known
        let hash = content_hash(b"");
        assert_eq!(hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn now_ms_returns_reasonable_value() {
        let ms = now_ms();
        assert!(ms > 1_704_000_000_000);
        // Should be within a few seconds of now
        let again = now_ms();
        assert!(again >= ms);
        assert!(again - ms < 5000);
    }
}
