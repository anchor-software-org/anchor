//! AirDrop-style file transfer over the typed SDK capability.
//!
//! A single shared folder (default `~/Anchor`) is the whole desktop UI: drop a
//! file into it and it is sent to the connected phone; files the phone sends
//! land in it. Transfers use a reliable QUIC stream with typed protobuf
//!
//! A shared `known` set records the size of every file we've already sent or
//! just received so the folder watcher never echoes an inbound file back.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::anchorapp::attachment_pending;
use crate::anchorapp::clipboard_models::now_ms;
use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorapp::sdk_files::SdkFilesBinding;
use crate::anchorapp::settings::settings;
use crate::anchorapp::sms_db;

/// Newest-first ring of recent transfers, shared with the GUI.
const HISTORY_CAP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Sent,
    Received,
}

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub name: String,
    pub direction: TransferDirection,
    pub size: u64,
    pub timestamp_ms: u64,
    /// Absolute path in the shared folder — both sent and received files live
    /// there, so the GUI can open either with `xdg-open`.
    pub path: PathBuf,
}

pub type SharedTransferHistory = Arc<Mutex<VecDeque<TransferRecord>>>;

fn record_transfer(
    history: &SharedTransferHistory,
    direction: TransferDirection,
    path: &Path,
    size: u64,
) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    let mut hist = history.lock().unwrap();
    hist.push_front(TransferRecord {
        name,
        direction,
        size,
        timestamp_ms: now_ms(),
        path: path.to_path_buf(),
    });
    hist.truncate(HISTORY_CAP);
}

/// Files whose base name starts with this are our own in-progress downloads and
/// must never be picked up by the watcher.
const PART_PREFIX: &str = ".anchor-incoming-";

/// path -> size of files we must not (re)send: pre-existing at startup, already
/// sent, or just received from the phone.
type KnownFiles = Arc<Mutex<HashMap<PathBuf, u64>>>;

/// The shared folder path, held behind a lock so the GUI can repoint it at
/// runtime and the watcher/receiver pick it up on their next iteration.
pub type SharedFolder = Arc<Mutex<PathBuf>>;

pub struct FileTransferPlugin {
    plugin_rx: Option<Receiver<AnchorEvent>>,
    broker_tx: Option<std::sync::mpsc::Sender<AnchorEvent>>,
    /// The shared folder; exposed so the GUI can open it and repoint it live.
    pub folder: SharedFolder,
    chunk_size: usize,
    poll_interval: Duration,
    known: KnownFiles,
    /// Recent transfers, newest first — surfaced in the GUI's Files tab.
    pub history: SharedTransferHistory,
    /// Typed QUIC file senders, populated by the SDK server per device.
    pub sdk_files: Option<SdkFilesBinding>,
}

impl Plugin for FileTransferPlugin {
    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<std::sync::mpsc::Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        let cfg = &settings().file_transfer;
        FileTransferPlugin {
            plugin_rx,
            broker_tx,
            folder: Arc::new(Mutex::new(cfg.resolve_folder())),
            chunk_size: cfg.chunk_size_bytes.clamp(4 * 1024, 4 * 1024 * 1024),
            poll_interval: Duration::from_millis(cfg.poll_interval_ms.clamp(250, 5000)),
            known: Arc::new(Mutex::new(HashMap::new())),
            history: Arc::new(Mutex::new(VecDeque::new())),
            sdk_files: None,
        }
    }

    fn run(&mut self) -> Option<JoinHandle<()>> {
        if !settings().file_transfer.enabled {
            log::info!("File transfer plugin disabled in settings");
            return None;
        }
        let sdk_files = self.sdk_files.clone().unwrap_or_default();

        let folder0 = self.folder.lock().unwrap().clone();
        if let Err(e) = fs::create_dir_all(&folder0) {
            log::error!("File transfer: cannot create shared folder {:?}: {}", folder0, e);
            return None;
        }
        log::info!("File transfer: shared folder is {:?}", folder0);

        // Seed `known` with whatever is already in the folder so we only send
        // files dropped in *after* startup.
        seed_known(&folder0, &self.known);

        // Watcher: local folder → phone.
        let watcher_folder = self.folder.clone();
        let watcher_known = self.known.clone();
        let watcher_history = self.history.clone();
        let chunk_size = self.chunk_size;
        let poll = self.poll_interval;
        thread::Builder::new()
            .name("filetransfer-watcher".to_string())
            .spawn(move || {
                watcher_loop(
                    sdk_files,
                    watcher_folder,
                    watcher_known,
                    watcher_history,
                    chunk_size,
                    poll,
                );
            })
            .expect("Failed to spawn filetransfer watcher thread");

        // Receiver: phone → local folder.
        let rx = self.plugin_rx.take().expect("filetransfer plugin_rx not set");
        let folder = self.folder.clone();
        let known = self.known.clone();
        let history = self.history.clone();
        let broker_tx = self.broker_tx.clone();
        let handle = thread::Builder::new()
            .name("filetransfer-receiver".to_string())
            .spawn(move || {
                receiver_loop(rx, folder, known, history, broker_tx);
            })
            .expect("Failed to spawn filetransfer receiver thread");

        Some(handle)
    }
}

// ── Watcher: folder → phone ──────────────────────────────────────────────

fn seed_known(folder: &Path, known: &KnownFiles) {
    let mut map = known.lock().unwrap();
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                map.insert(entry.path(), meta.len());
            }
        }
    }
}

fn watcher_loop(
    sdk_files: SdkFilesBinding,
    folder: SharedFolder,
    known: KnownFiles,
    history: SharedTransferHistory,
    chunk_size: usize,
    poll: Duration,
) {
    // path -> (last-seen size, times seen at that size). A file must hold the
    // same size across two polls before we send it, so we never grab a file
    // mid-copy.
    let mut pending: HashMap<PathBuf, (u64, u32)> = HashMap::new();
    let mut current = folder.lock().unwrap().clone();

    loop {
        // Pick up a live folder change: reset the known/pending state and
        // re-seed from the new folder so we don't blast its existing contents.
        let active = folder.lock().unwrap().clone();
        if active != current {
            log::info!("File transfer: shared folder changed to {:?}", active);
            let _ = fs::create_dir_all(&active);
            known.lock().unwrap().clear();
            seed_known(&active, &known);
            pending.clear();
            current = active;
        }
        let dir = &current;

        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("File transfer: cannot read folder {:?}: {}", dir, e);
                thread::sleep(poll);
                continue;
            }
        };

        let mut seen_now: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if name.starts_with('.') || name.starts_with(PART_PREFIX) {
                continue; // hidden / our own in-progress downloads
            }
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let size = meta.len();
            if size == 0 {
                continue;
            }
            seen_now.push(path.clone());

            if known.lock().unwrap().get(&path) == Some(&size) {
                continue; // already handled at this size
            }

            let stable = match pending.get(&path) {
                Some((psize, count)) if *psize == size => count + 1,
                _ => 0,
            };
            if stable < 1 {
                pending.insert(path.clone(), (size, stable));
                continue;
            }

            // Size held steady across two polls — try to send it.
            pending.remove(&path);
            match send_file(&sdk_files, &path, size, chunk_size) {
                Ok(true) => {
                    known.lock().unwrap().insert(path.clone(), size);
                    record_transfer(&history, TransferDirection::Sent, &path, size);
                }
                Ok(false) => {
                    // No device connected (or all dropped mid-send). Leave it
                    // pending so it goes out when a phone connects.
                }
                Err(e) => {
                    log::warn!("File transfer: failed to send {:?}: {}", path, e);
                    known.lock().unwrap().insert(path.clone(), size); // don't spin on a bad file
                }
            }
        }

        // Forget pending/known entries for files that no longer exist.
        pending.retain(|p, _| seen_now.contains(p));
        known.lock().unwrap().retain(|p, _| p.exists());

        thread::sleep(poll);
    }
}

/// Stream a file to every connected device via its bounded control queue,
/// blocking (backpressure) when a queue is full. Returns Ok(true) if it was
/// delivered to at least one device, Ok(false) if none were connected / all
/// dropped mid-transfer.
fn send_file(
    sdk_files: &SdkFilesBinding,
    path: &Path,
    size: u64,
    chunk_size: usize,
) -> std::io::Result<bool> {
    // Typed QUIC is the only data plane. Sender failures are terminal for
    // that peer, preventing a partial transfer from being duplicated.
    let typed = sdk_files.senders();
    let mut delivered = false;
    for (device_id, sender) in typed {
        match sender.send_file(path, size, chunk_size) {
            Ok(()) => {
                delivered = true;
                log::info!("File transfer: sent {:?} over SDK QUIC to {}", path, device_id);
            }
            Err(error) => {
                log::warn!(
                    "File transfer: SDK QUIC send to {} failed for {:?}: {}",
                    device_id,
                    path,
                    error
                );
            }
        }
    }

    Ok(delivered)
}

// ── Receiver: phone → folder ─────────────────────────────────────────────

/// Sink-generic, side-effect-free chunk assembler: decode base64 chunks into
/// `sink`, tracking the running SHA-256 and chunk count so completeness and
/// integrity can be checked independently of any file/IO (and unit-tested).
struct Assembler<W: Write> {
    sink: W,
    hasher: Sha256,
    chunks_expected: u64,
    chunks_received: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum FinishOutcome {
    Complete,
    Incomplete { received: u64, expected: u64 },
    ChecksumMismatch,
}

impl<W: Write> Assembler<W> {
    fn new(sink: W, chunks_expected: u64) -> Self {
        Assembler { sink, hasher: Sha256::new(), chunks_expected, chunks_received: 0 }
    }

    /// Decode and append one base64 chunk. Err on bad base64 or write failure.
    fn push_chunk_b64(&mut self, data_b64: &str) -> std::io::Result<()> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.sink.write_all(&bytes)?;
        self.hasher.update(&bytes);
        self.chunks_received += 1;
        Ok(())
    }

    /// Flush and verify. An `expected_chunks` of 0 (unknown size) skips the
    /// count check and relies on the checksum alone; an empty `expected_sha`
    /// skips the checksum. Returns the sink so callers can close/rename it.
    fn finish(mut self, expected_sha: &str) -> std::io::Result<(W, FinishOutcome)> {
        self.sink.flush()?;
        if self.chunks_expected != 0 && self.chunks_received != self.chunks_expected {
            return Ok((
                self.sink,
                FinishOutcome::Incomplete {
                    received: self.chunks_received,
                    expected: self.chunks_expected,
                },
            ));
        }
        let actual = hex(&self.hasher.finalize());
        let outcome = if !expected_sha.is_empty() && actual != expected_sha {
            FinishOutcome::ChecksumMismatch
        } else {
            FinishOutcome::Complete
        };
        Ok((self.sink, outcome))
    }
}

struct Incoming {
    name: String,
    temp_path: PathBuf,
    asm: Assembler<BufWriter<File>>,
    last_activity: Instant,
    sms_unique_identifier: Option<String>,
}

/// A transfer with no traffic for this long is dead (peer vanished mid-send);
/// its partial file is discarded so `.part` files and fds don't accumulate.
const STALE_AFTER: Duration = Duration::from_secs(60);

fn reap_stale(transfers: &mut HashMap<String, Incoming>) {
    let stale: Vec<String> = transfers
        .iter()
        .filter(|(_, inc)| inc.last_activity.elapsed() > STALE_AFTER)
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(inc) = transfers.remove(&id) {
            log::warn!("File transfer: {:?} stalled — discarding partial", inc.name);
            drop(inc.asm);
            let _ = fs::remove_file(&inc.temp_path);
        }
    }
}

fn receiver_loop(
    rx: Receiver<AnchorEvent>,
    folder: SharedFolder,
    known: KnownFiles,
    history: SharedTransferHistory,
    broker_tx: Option<std::sync::mpsc::Sender<AnchorEvent>>,
) {
    let mut transfers: HashMap<String, Incoming> = HashMap::new();
    let mut last_reap = Instant::now();

    loop {
        let event = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => {
                reap_stale(&mut transfers);
                last_reap = Instant::now();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // Steady traffic on *other* transfers keeps recv_timeout from firing,
        // so also reap inline every so often.
        if last_reap.elapsed() > Duration::from_secs(10) {
            reap_stale(&mut transfers);
            last_reap = Instant::now();
        }

        let payload = match event.message {
            AnchorMessage::Json(s) => s,
            _ => continue,
        };
        let msg: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("File transfer: bad JSON: {}", e);
                continue;
            }
        };
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let transfer_id = msg.get("transfer_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if transfer_id.is_empty() {
            continue;
        }

        match msg_type {
            "file_offer" => {
                let dir = folder.lock().unwrap().clone();
                if let Err(e) = begin_incoming(&mut transfers, &dir, &transfer_id, &msg) {
                    log::warn!("File transfer: cannot begin {}: {}", transfer_id, e);
                }
            }
            "file_chunk" => {
                if let Some(inc) = transfers.get_mut(&transfer_id) {
                    inc.last_activity = Instant::now();
                    if let Some(data) = msg.get("data").and_then(|v| v.as_str())
                        && let Err(e) = inc.asm.push_chunk_b64(data)
                    {
                        log::warn!("File transfer: bad chunk for {}: {}", transfer_id, e);
                    }
                }
            }
            "file_complete" => {
                if let Some(inc) = transfers.remove(&transfer_id) {
                    let expected_sha = msg.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
                    finish_incoming(
                        inc,
                        expected_sha,
                        &known,
                        &history,
                        transfer_id.clone(),
                        &broker_tx,
                    );
                }
            }
            _ => {}
        }
        // Reap stale SMS attachment correlation entries periodically.
        crate::anchorapp::attachment_pending::reap_stale();
    }
}

fn begin_incoming(
    transfers: &mut HashMap<String, Incoming>,
    folder: &Path,
    transfer_id: &str,
    msg: &serde_json::Value,
) -> std::io::Result<()> {
    let name = sanitize_name(msg.get("name").and_then(|v| v.as_str()).unwrap_or("file"));
    let chunks_expected = msg.get("chunks").and_then(|v| v.as_u64()).unwrap_or(0);
    let size = msg.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let sms_unique_identifier = msg
        .get("sms_unique_identifier")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Drop any stale transfer reusing this id — and do it *before* creating
    // the new temp file, since both share a path and the old writer would
    // otherwise flush buffered bytes into the fresh file on drop.
    if let Some(old) = transfers.remove(transfer_id) {
        drop(old.asm);
        let _ = fs::remove_file(&old.temp_path);
    }

    let temp_path = folder.join(format!("{PART_PREFIX}{transfer_id}.part"));
    let file = File::create(&temp_path)?;
    log::info!("File transfer: receiving {:?} ({} bytes) from phone", name, size);
    if let Some(ref uid) = sms_unique_identifier {
        log::info!("File transfer {} correlated to SMS attachment {}", transfer_id, uid);
    }
    transfers.insert(
        transfer_id.to_string(),
        Incoming {
            name,
            temp_path,
            asm: Assembler::new(BufWriter::new(file), chunks_expected),
            last_activity: Instant::now(),
            sms_unique_identifier,
        },
    );
    Ok(())
}

fn finish_incoming(
    inc: Incoming,
    expected_sha: &str,
    known: &KnownFiles,
    history: &SharedTransferHistory,
    transfer_id: String,
    broker_tx: &Option<std::sync::mpsc::Sender<AnchorEvent>>,
) {
    let Incoming { name, temp_path, asm, sms_unique_identifier, .. } = inc;

    let (writer, outcome) = match asm.finish(expected_sha) {
        Ok(pair) => pair,
        Err(e) => {
            log::warn!("File transfer: finalize failed for {:?}: {}", name, e);
            let _ = fs::remove_file(&temp_path);
            return;
        }
    };
    drop(writer); // close the file before renaming

    match outcome {
        FinishOutcome::Incomplete { received, expected } => {
            log::warn!(
                "File transfer: {:?} incomplete ({}/{} chunks) — discarding",
                name,
                received,
                expected
            );
            let _ = fs::remove_file(&temp_path);
        }
        FinishOutcome::ChecksumMismatch => {
            // Log expected vs actual for debugging; recompute actual from temp file if possible.
            let actual = fs::read(&temp_path)
                .map(|bytes| hex(&Sha256::digest(&bytes)))
                .unwrap_or_else(|_| "read_failed".into());
            log::warn!(
                "File transfer: {:?} checksum mismatch — expected {}, actual {} — discarding",
                name,
                expected_sha,
                actual
            );
            let _ = fs::remove_file(&temp_path);
        }
        FinishOutcome::Complete => {
            let folder = temp_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
            let dest = unique_dest(&folder, &name);
            // Mark known *before* the file appears so the watcher never echoes
            // it back to the phone.
            if let Ok(meta) = fs::metadata(&temp_path) {
                known.lock().unwrap().insert(dest.clone(), meta.len());
            }
            if let Err(e) = fs::rename(&temp_path, &dest) {
                log::warn!("File transfer: cannot move into folder: {}", e);
                let _ = fs::remove_file(&temp_path);
                return;
            }
            let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            record_transfer(history, TransferDirection::Received, &dest, size);
            log::info!("File transfer: received {:?}", dest);
            notify_received(&dest);

            // If this transfer was correlated to an SMS attachment, persist the
            // verified local path so the SMS DB can serve it without re-fetch.
            let correlated_uid = sms_unique_identifier
                .or_else(|| attachment_pending::take_for_completion(&transfer_id));
            if let Some(uid) = correlated_uid {
                if let Err(e) = persist_attachment_local_path(&uid, &dest) {
                    log::warn!("SMS attachment {} local_path persist failed: {}", uid, e);
                } else {
                    log::info!("SMS attachment {} → {}", uid, dest.display());
                    if let Some(tx) = broker_tx {
                        let _ = tx.send(AnchorEvent {
                            target: AnchorTarget::Gui,
                            message: AnchorMessage::Json(
                                serde_json::json!({
                                    "type": "sms.attachment_ready",
                                    "unique_identifier": uid,
                                    "local_path": dest.to_string_lossy(),
                                    "transfer_id": transfer_id,
                                })
                                .to_string(),
                            ),
                        });
                    }
                }
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Strip any directory components and reject empties / traversal so a remote
/// name can only ever land as a plain file inside the shared folder.
fn sanitize_name(raw: &str) -> String {
    let base = Path::new(raw).file_name().and_then(|n| n.to_str()).unwrap_or("file").trim();
    if base.is_empty() || base == "." || base == ".." {
        "file".to_string()
    } else {
        base.to_string()
    }
}

/// Avoid clobbering an existing file by inserting " (n)" before the extension.
fn unique_dest(folder: &Path, name: &str) -> PathBuf {
    let candidate = folder.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|s| s.to_str());
    for n in 1..10_000 {
        let candidate_name = match ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = folder.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    folder.join(name)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn persist_attachment_local_path(unique_identifier: &str, dest: &Path) -> rusqlite::Result<()> {
    let db_path = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("anchor/sms.db");
    let conn = rusqlite::Connection::open(db_path)?;
    sms_db::set_attachment_local_path(&conn, unique_identifier, &dest.to_string_lossy())
}

fn notify_received(path: &Path) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    notify_rust::Notification::new()
        .summary("Anchor — file received")
        .body(&format!("{name} landed in your Anchor folder"))
        .show()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn feed(data: &[u8], chunk_size: usize, expected: u64) -> Assembler<Vec<u8>> {
        let mut asm = Assembler::new(Vec::new(), expected);
        for c in data.chunks(chunk_size) {
            asm.push_chunk_b64(&b64(c)).unwrap();
        }
        asm
    }

    #[test]
    fn assembler_round_trip_reconstructs_bytes() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let chunks = data.chunks(256).count() as u64;
        let asm = feed(&data, 256, chunks);
        let sha = hex(&Sha256::digest(&data));
        let (out, outcome) = asm.finish(&sha).unwrap();
        assert_eq!(out, data);
        assert_eq!(outcome, FinishOutcome::Complete);
    }

    #[test]
    fn assembler_missing_chunk_is_incomplete() {
        let data = vec![9u8; 1000];
        let all = data.chunks(256).count() as u64;
        // Feed one fewer chunk than promised.
        let mut asm = Assembler::new(Vec::new(), all);
        for c in data.chunks(256).take((all - 1) as usize) {
            asm.push_chunk_b64(&b64(c)).unwrap();
        }
        let sha = hex(&Sha256::digest(&data));
        let (_out, outcome) = asm.finish(&sha).unwrap();
        assert_eq!(outcome, FinishOutcome::Incomplete { received: all - 1, expected: all });
    }

    #[test]
    fn assembler_wrong_checksum_is_mismatch() {
        let data = vec![7u8; 500];
        let chunks = data.chunks(256).count() as u64;
        let asm = feed(&data, 256, chunks);
        let (_out, outcome) = asm.finish("00ff00ff").unwrap();
        assert_eq!(outcome, FinishOutcome::ChecksumMismatch);
    }

    #[test]
    fn assembler_zero_expected_relies_on_checksum() {
        // size unknown on the sender → chunks=0 → count check skipped.
        let data = vec![3u8; 300];
        let asm = feed(&data, 128, 0);
        let sha = hex(&Sha256::digest(&data));
        let (out, outcome) = asm.finish(&sha).unwrap();
        assert_eq!(out, data);
        assert_eq!(outcome, FinishOutcome::Complete);
    }

    #[test]
    fn assembler_rejects_bad_base64() {
        let mut asm = Assembler::new(Vec::new(), 1);
        assert!(asm.push_chunk_b64("!!! not base64 !!!").is_err());
    }

    #[test]
    fn sanitize_strips_path_traversal() {
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("/abs/path/photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_name(".."), "file");
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name("plain.png"), "plain.png");
    }

    #[test]
    fn unique_dest_dedupes() {
        let dir = std::env::temp_dir().join(format!("anchor-ft-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = unique_dest(&dir, "a.txt");
        assert_eq!(first, dir.join("a.txt"));
        File::create(&first).unwrap();
        let second = unique_dest(&dir, "a.txt");
        assert_eq!(second, dir.join("a (1).txt"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xab]), "000fffab");
    }
}
