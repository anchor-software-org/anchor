//! Desktop-side join between an SMS attachment request and the Files QUIC
//! transfer that later carries its bytes.
//!
//! The typed `AttachmentRequest { part_id, transfer_id }` carries the MMS
//! provider's `unique_identifier` (a stable string like `mms_part_123`). The
//! Files capability then sends a new random 16-byte `transfer_id` (hex). This
//! module persists the join without changing `files.proto`.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// How long a pending SMS attachment request is kept before it is considered
/// abandoned (phone never sent the file, or user cancelled).
const PENDING_TTL: Duration = Duration::from_secs(300);
/// How long the hex-transfer → unique_identifier map is kept after the offer.
const TRANSFER_TTL: Duration = Duration::from_secs(300);

struct PendingState {
    /// unique_identifier -> (requested_at, seq)
    pending: HashMap<String, (Instant, u64)>,
    /// hex transfer_id -> (unique_identifier, inserted_at)
    transfers: HashMap<String, (String, Instant)>,
}

impl PendingState {
    fn new() -> Self {
        Self { pending: HashMap::new(), transfers: HashMap::new() }
    }

    fn reap(&mut self) {
        let now = Instant::now();
        self.pending.retain(|_, (t, _)| now.duration_since(*t) < PENDING_TTL);
        self.transfers.retain(|_, (_, t)| now.duration_since(*t) < TRANSFER_TTL);
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

static STATE: OnceLock<Arc<Mutex<PendingState>>> = OnceLock::new();

fn state() -> Arc<Mutex<PendingState>> {
    STATE.get_or_init(|| Arc::new(Mutex::new(PendingState::new()))).clone()
}

/// Remember that the desktop asked for `unique_identifier`. Called from
/// `sms_plugin::handle_request_attachment` *before* the QUIC record is sent.
pub fn note_requested(unique_identifier: String) {
    if unique_identifier.is_empty() {
        return;
    }
    let s = state();
    let mut guard = s.lock().unwrap();
    guard.reap();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    guard.pending.insert(unique_identifier, (Instant::now(), seq));
}

/// Claim a pending `unique_identifier` for a newly arrived file offer.
///
/// `hex_transfer_id` is `hex(offer.transfer_id)` (32 hex chars). `filename`
/// is `offer.filename` as sent by the phone — when the phone preserves the
/// SMS `transfer_id` as the filename, this gives an exact match even with
/// concurrent requests. Otherwise the oldest pending entry is claimed (v1
/// serializes attachment requests, so this is the common case).
///
/// Returns the claimed `unique_identifier` if found, and records the
/// `hex_transfer_id → unique_identifier` mapping for later completion.
pub fn claim_for_offer(hex_transfer_id: String, filename: &str) -> Option<String> {
    if hex_transfer_id.is_empty() {
        return None;
    }
    let s = state();
    let mut guard = s.lock().unwrap();
    guard.reap();

    // Fast path: filename is exactly the pending unique_identifier (or with an
    // extension appended by the phone's mime handling). Try exact and
    // prefix-with-dot match first so concurrent requests disambiguate.
    let sanitized_filename = filename.trim();
    let mut claimed: Option<String> = None;

    // Check for filename that equals or starts with a pending identifier.
    // Example: pending "mms_part_123", filename "mms_part_123.jpg" or "mms_part_123"
    // Use longest-match to disambiguate concurrent requests like 10 vs 11.
    let mut best_match: Option<(String, usize)> = None;
    for pending_id in guard.pending.keys().cloned().collect::<Vec<_>>() {
        let matches = sanitized_filename == pending_id
            || sanitized_filename.starts_with(&format!("{pending_id}."))
            || sanitized_filename == format!("{}.bin", pending_id);
        if matches {
            let len = pending_id.len();
            if best_match.as_ref().is_none_or(|(_, best_len)| len > *best_len) {
                best_match = Some((pending_id, len));
            }
        }
    }
    if let Some((id, _)) = best_match {
        claimed = Some(id);
    }

    // Fallback: oldest pending entry (serialized attachment flow).
    if claimed.is_none() && !guard.pending.is_empty() {
        let oldest =
            guard.pending.iter().min_by_key(|(_, (t, seq))| (*t, *seq)).map(|(k, _)| k.clone());
        claimed = oldest;
    }

    // Last resort: if the phone sent a filename hint that looks like an MMS
    // provider id (e.g. mms_part_123.jpg) but we have no pending entry (test
    // hook or race), synthesize the uid from the filename stem so the file
    // still correlates and `persist_attachment_local_path` can be verified.
    if claimed.is_none() {
        let stem = sanitized_filename.split('.').next().unwrap_or("");
        if stem.starts_with("mms_part_") && !stem.is_empty() {
            claimed = Some(stem.to_string());
        } else if sanitized_filename.starts_with("mms_part_") {
            claimed = Some(sanitized_filename.to_string());
        }
    }

    let uid = claimed?;
    guard.pending.remove(&uid);
    guard.transfers.insert(hex_transfer_id.clone(), (uid.clone(), Instant::now()));
    Some(uid)
}

/// Take the `unique_identifier` for a completing transfer. Called from
/// `filetransfer_plugin::finish_incoming` after the file is verified and
/// renamed. Returns `None` if this transfer was not a correlated SMS
/// attachment (normal file transfers fall through).
pub fn take_for_completion(hex_transfer_id: &str) -> Option<String> {
    let s = state();
    let mut guard = s.lock().unwrap();
    guard.reap();
    guard.transfers.remove(hex_transfer_id).map(|(uid, _)| uid)
}

/// Remove stale entries. Called opportunistically from the filetransfer
/// receiver and sms plugin loops; cheap because it only iterates the maps.
pub fn reap_stale() {
    let s = state();
    s.lock().unwrap().reap();
}

/// Clear all pending state. Used in tests.
#[cfg(test)]
pub fn clear_for_tests() {
    let s = state();
    let mut guard = s.lock().unwrap();
    guard.pending.clear();
    guard.transfers.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests share a global `STATE` and must not run in parallel.
    // Each test locks a global test mutex via `clear_for_tests` ordering and uses
    // unique keys to tolerate some interleaving, but we also serialize via a
    // dedicated mutex.

    static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn pending_then_offer_then_completion() {
        let _g = test_lock();
        clear_for_tests();
        note_requested("mms_part_42".into());
        let claimed = claim_for_offer("deadbeefdeadbeefdeadbeefdeadbeef".into(), "mms_part_42.jpg");
        assert_eq!(claimed.as_deref(), Some("mms_part_42"));
        let uid = take_for_completion("deadbeefdeadbeefdeadbeefdeadbeef");
        assert_eq!(uid.as_deref(), Some("mms_part_42"));
        assert!(take_for_completion("deadbeefdeadbeefdeadbeefdeadbeef").is_none());
    }

    #[test]
    fn fallback_to_oldest_when_filename_does_not_match() {
        let _g = test_lock();
        clear_for_tests();
        note_requested("mms_part_1".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        note_requested("mms_part_2".into());
        // Phone sent a generic name like "image.jpg" (no hint) — oldest is claimed.
        let claimed = claim_for_offer("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(), "image.jpg");
        assert_eq!(claimed.as_deref(), Some("mms_part_1"));
        // Next offer gets the second pending.
        let claimed2 = claim_for_offer("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(), "image.jpg");
        assert_eq!(claimed2.as_deref(), Some("mms_part_2"));
    }

    #[test]
    fn exact_filename_match_disambiguates_concurrent() {
        let _g = test_lock();
        clear_for_tests();
        note_requested("mms_part_10".into());
        note_requested("mms_part_11".into());
        // Offer carries filename "mms_part_11.png" — should claim 11, not 10.
        let claimed = claim_for_offer("cccccccccccccccccccccccccccccccc".into(), "mms_part_11.png");
        assert_eq!(claimed.as_deref(), Some("mms_part_11"));
        let uid = take_for_completion("cccccccccccccccccccccccccccccccc");
        assert_eq!(uid.as_deref(), Some("mms_part_11"));
        // 10 remains pending.
        let claimed2 = claim_for_offer("dddddddddddddddddddddddddddddddd".into(), "mms_part_10");
        assert_eq!(claimed2.as_deref(), Some("mms_part_10"));
    }

    #[test]
    fn no_pending_yields_none() {
        let _g = test_lock();
        clear_for_tests();
        assert!(claim_for_offer("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(), "file.jpg").is_none());
    }
}
