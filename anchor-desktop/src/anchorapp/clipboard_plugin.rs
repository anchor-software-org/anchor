use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::anchorapp::clipboard_models::{
    ClipboardContentType, ClipboardEntry, SharedClipboardHistory, content_hash, now_ms,
};
use crate::anchorapp::event::{AnchorEvent, AnchorMessage};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorapp::sdk_clipboard::SdkClipboardSender;
use crate::anchorapp::settings::settings;
use base64::Engine;

pub struct ClipboardPlugin {
    plugin_rx: Option<Receiver<AnchorEvent>>,
    pub history: SharedClipboardHistory,
    last_applied_hash: Arc<Mutex<Option<String>>>,
    last_sent_hash: Arc<Mutex<Option<String>>>,
    sdk_binding: SdkClipboardBinding,
}

type ClipboardSdkPeer = (SdkClipboardSender, [u8; 32]);

#[derive(Clone, Default)]
pub struct SdkClipboardBinding {
    senders: Arc<Mutex<HashMap<String, ClipboardSdkPeer>>>,
    revision: Arc<std::sync::atomic::AtomicU64>,
}

impl SdkClipboardBinding {
    pub fn attach(&self, device_id: String, sender: SdkClipboardSender, origin_node_id: [u8; 32]) {
        self.senders.lock().unwrap().insert(device_id, (sender, origin_node_id));
        self.revision.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    fn senders(&self) -> Vec<ClipboardSdkPeer> {
        self.senders.lock().unwrap().values().cloned().collect()
    }
}

impl Plugin for ClipboardPlugin {
    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        _broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        ClipboardPlugin {
            plugin_rx,
            history: Arc::new(Mutex::new(VecDeque::new())),
            last_applied_hash: Arc::new(Mutex::new(None)),
            last_sent_hash: Arc::new(Mutex::new(None)),
            sdk_binding: SdkClipboardBinding::default(),
        }
    }

    fn run(&mut self) -> Option<JoinHandle<()>> {
        let clipboard_settings = &settings().clipboard;
        if !clipboard_settings.enabled {
            log::info!("Clipboard plugin disabled in settings");
            return None;
        }

        let rx = self.plugin_rx.take().expect("clipboard plugin_rx not set");
        let history = self.history.clone();
        let last_applied_hash = self.last_applied_hash.clone();
        let last_sent_hash = self.last_sent_hash.clone();
        let sdk_binding = self.sdk_binding.clone();

        let debounce_ms = clipboard_settings.debounce_ms;
        let max_size = clipboard_settings.max_size_bytes;
        let history_size = clipboard_settings.history_size;
        let auto_share = clipboard_settings.auto_share;

        // Thread 1: Watch local clipboard for changes, send to device
        let watcher_applied = last_applied_hash.clone();
        let watcher_sent = last_sent_hash.clone();
        let watcher_history = history.clone();
        thread::Builder::new()
            .name("clipboard-watcher".to_string())
            .spawn(move || {
                log::debug!("Clipboard watcher thread starting");
                clipboard_watcher_loop(
                    watcher_applied,
                    watcher_sent,
                    watcher_history,
                    debounce_ms,
                    max_size,
                    history_size,
                    auto_share,
                    sdk_binding,
                );
            })
            .expect("Failed to spawn clipboard watcher thread");

        // Thread 2: Receive remote clipboard content from device
        let receiver_applied = last_applied_hash;
        let receiver_sent = last_sent_hash;
        let handle = thread::Builder::new()
            .name("clipboard-receiver".to_string())
            .spawn(move || {
                log::debug!("Clipboard receiver thread starting");
                clipboard_receiver_loop(
                    rx,
                    receiver_applied,
                    receiver_sent,
                    history,
                    max_size,
                    history_size,
                );
            })
            .expect("Failed to spawn clipboard receiver thread");

        Some(handle)
    }
}

impl ClipboardPlugin {
    /// Attach a negotiated SDK clipboard capability for one device.
    pub fn attach_sdk_sender(
        &self,
        device_id: String,
        sender: SdkClipboardSender,
        origin_node_id: [u8; 32],
    ) {
        self.sdk_binding.attach(device_id, sender, origin_node_id);
    }

    pub fn sdk_binding(&self) -> SdkClipboardBinding {
        self.sdk_binding.clone()
    }
}

// -- Watcher: local clipboard → network --

#[allow(clippy::too_many_arguments)]
fn clipboard_watcher_loop(
    last_applied_hash: Arc<Mutex<Option<String>>>,
    last_sent_hash: Arc<Mutex<Option<String>>>,
    history: SharedClipboardHistory,
    debounce_ms: u64,
    max_size: usize,
    history_size: usize,
    auto_share: bool,
    sdk_binding: SdkClipboardBinding,
) {
    let watcher = match anchor_wl_clipboard::ClipboardWatcherBuilder::new().build() {
        Ok(w) => w,
        Err(e) => {
            log::error!("Clipboard: failed to start watcher: {}", e);
            return;
        }
    };

    log::debug!("Clipboard watcher connected to Wayland");

    let debounce = Duration::from_millis(debounce_ms);
    let mut pending: Option<(Vec<u8>, anchor_wl_clipboard::MimeType, Vec<String>)> = None;
    let mut deadline: Option<Instant> = None;

    loop {
        let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));

        let event = match timeout {
            Some(dur) if dur.is_zero() => {
                // Debounce expired — flush pending content
                if let Some((data, mime, _available)) = pending.take()
                    && auto_share
                {
                    send_clipboard_to_device(
                        &data,
                        &mime,
                        &last_applied_hash,
                        &last_sent_hash,
                        &history,
                        max_size,
                        history_size,
                        sdk_binding.senders(),
                        &sdk_binding.revision,
                    );
                }
                deadline = None;
                continue;
            }
            Some(dur) => match watcher.rx.recv_timeout(dur) {
                Ok(ev) => ev,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    log::info!("Clipboard watcher channel closed");
                    break;
                }
            },
            None => match watcher.rx.recv() {
                Ok(ev) => ev,
                Err(_) => {
                    log::info!("Clipboard watcher channel closed");
                    break;
                }
            },
        };

        match event {
            anchor_wl_clipboard::ClipboardEvent::Changed(content) => {
                if content.data.len() > max_size {
                    log::debug!(
                        "Clipboard: content too large ({} bytes), skipping",
                        content.data.len()
                    );
                    continue;
                }

                // Check echo guard
                let hash = content_hash(&content.data);
                {
                    let applied = last_applied_hash.lock().unwrap();
                    if applied.as_deref() == Some(&hash) {
                        log::debug!("Clipboard: skipping echo (matches last_applied_hash)");
                        continue;
                    }
                }

                pending = Some((content.data, content.mime_type, content.available_types));
                deadline = Some(Instant::now() + debounce);
            }
            anchor_wl_clipboard::ClipboardEvent::Cleared => {
                pending = None;
                deadline = None;
            }
            anchor_wl_clipboard::ClipboardEvent::Error(e) => {
                log::error!("Clipboard watcher error: {}", e);
                break;
            }
        }
    }

    log::info!("Clipboard watcher thread exiting");
}

#[allow(clippy::too_many_arguments)]
fn send_clipboard_to_device(
    data: &[u8],
    mime: &anchor_wl_clipboard::MimeType,
    _last_applied_hash: &Arc<Mutex<Option<String>>>,
    last_sent_hash: &Arc<Mutex<Option<String>>>,
    history: &SharedClipboardHistory,
    _max_size: usize,
    history_size: usize,
    sdk_senders: Vec<ClipboardSdkPeer>,
    sdk_revision: &Arc<std::sync::atomic::AtomicU64>,
) {
    let hash = content_hash(data);

    // Don't re-send what we already sent
    {
        let sent = last_sent_hash.lock().unwrap();
        if sent.as_deref() == Some(&hash) {
            return;
        }
    }

    let (content_type, content_str) = match mime {
        anchor_wl_clipboard::MimeType::TextPlain | anchor_wl_clipboard::MimeType::TextPlainUtf8 => {
            (ClipboardContentType::Text, String::from_utf8_lossy(data).into_owned())
        }
        anchor_wl_clipboard::MimeType::ImagePng => {
            (ClipboardContentType::ImagePng, base64::engine::general_purpose::STANDARD.encode(data))
        }
        anchor_wl_clipboard::MimeType::Custom(_) => {
            log::debug!("Clipboard: unsupported MIME type for sync: {:?}", mime);
            return;
        }
    };

    let timestamp = now_ms();

    if !sdk_senders.is_empty() {
        let revision = sdk_revision.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let accepted = sdk_senders.iter().fold(false, |accepted, (sender, origin)| {
            let sent = match mime {
                anchor_wl_clipboard::MimeType::TextPlain
                | anchor_wl_clipboard::MimeType::TextPlainUtf8 => {
                    sender.send_text(*origin, revision, String::from_utf8_lossy(data))
                }
                anchor_wl_clipboard::MimeType::ImagePng => {
                    sender.send_png(*origin, revision, data.to_vec())
                }
                anchor_wl_clipboard::MimeType::Custom(_) => false,
            };
            accepted || sent
        });
        if !accepted {
            log::debug!("Clipboard: SDK writer queue full; dropping revision {revision}");
            return;
        }
        *last_sent_hash.lock().unwrap() = Some(hash.clone());
        let content = match mime {
            anchor_wl_clipboard::MimeType::ImagePng => {
                base64::engine::general_purpose::STANDARD.encode(data)
            }
            _ => String::from_utf8_lossy(data).into_owned(),
        };
        push_history(
            history,
            ClipboardEntry {
                content_type: if matches!(mime, anchor_wl_clipboard::MimeType::ImagePng) {
                    ClipboardContentType::ImagePng
                } else {
                    ClipboardContentType::Text
                },
                content,
                timestamp,
                hash,
                device_id: "local".to_string(),
            },
            history_size,
        );
        log::debug!("Clipboard: sent {} bytes through Anchor SDK", data.len());
        return;
    }

    // Update sent hash
    *last_sent_hash.lock().unwrap() = Some(hash.clone());

    // With no negotiated SDK peer there is intentionally no network fallback.
    // Keep the local history for the UI, but do not reintroduce JSON-over-TCP
    push_history(
        history,
        ClipboardEntry {
            content_type,
            content: content_str,
            timestamp,
            hash,
            device_id: "local".to_string(),
        },
        history_size,
    );
    log::debug!("Clipboard: no SDK peer negotiated; retained local change only");
}

// -- Receiver: network → local clipboard --

fn clipboard_receiver_loop(
    rx: Receiver<AnchorEvent>,
    last_applied_hash: Arc<Mutex<Option<String>>>,
    last_sent_hash: Arc<Mutex<Option<String>>>,
    history: SharedClipboardHistory,
    max_size: usize,
    history_size: usize,
) {
    let clipboard_writer = match anchor_wl_clipboard::ClipboardWriter::new() {
        Ok(writer) => Some(writer),
        Err(error) => {
            log::error!("Clipboard: failed to start local writer: {error}");
            None
        }
    };

    for event in rx.iter() {
        let json_str = match &event.message {
            AnchorMessage::Json(s) => s.clone(),
            _ => continue,
        };

        let payload: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Clipboard: invalid JSON: {}", e);
                continue;
            }
        };

        match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "device_connected" => {
                send_clipboard_connect(&last_sent_hash);
            }
            "clipboard_content" => {
                handle_clipboard_content(
                    &payload,
                    &last_applied_hash,
                    &last_sent_hash,
                    &history,
                    max_size,
                    history_size,
                    clipboard_writer.as_ref(),
                );
            }
            "clipboard_connect" => {
                handle_clipboard_connect(
                    &payload,
                    &last_applied_hash,
                    &last_sent_hash,
                    &history,
                    max_size,
                    history_size,
                    clipboard_writer.as_ref(),
                );
            }
            "anchor.clipboard.copy_local" => {
                copy_history_entry(
                    &payload,
                    &last_applied_hash,
                    &history,
                    clipboard_writer.as_ref(),
                );
            }
            other => {
                log::debug!("Clipboard: unhandled packet type: {}", other);
            }
        }
    }

    log::info!("Clipboard receiver thread exiting");
}

fn copy_history_entry(
    payload: &serde_json::Value,
    last_applied_hash: &Arc<Mutex<Option<String>>>,
    history: &SharedClipboardHistory,
    clipboard_writer: Option<&anchor_wl_clipboard::ClipboardWriter>,
) {
    let Some(hash) = payload.get("hash").and_then(|value| value.as_str()) else {
        log::warn!("Clipboard: local copy request did not include a hash");
        return;
    };
    let entry = history.lock().unwrap().iter().find(|entry| entry.hash == hash).cloned();
    let Some(entry) = entry else {
        log::warn!("Clipboard: local copy entry no longer exists");
        return;
    };

    // Mark this as locally applied before setting the selection so the watcher
    // does not immediately echo the historical entry back to the phone.
    *last_applied_hash.lock().unwrap() = Some(entry.hash.clone());
    let result = match entry.content_type {
        ClipboardContentType::Text => set_desktop_clipboard_text(clipboard_writer, &entry.content),
        ClipboardContentType::ImagePng => {
            let decoded = match base64::engine::general_purpose::STANDARD.decode(&entry.content) {
                Ok(decoded) => decoded,
                Err(error) => {
                    log::warn!("Clipboard: stored PNG is not valid base64: {error}");
                    return;
                }
            };
            set_desktop_clipboard_png(clipboard_writer, &decoded)
        }
    };
    match result {
        Ok(()) => log::info!("Clipboard: copied history entry {} to the local clipboard", hash),
        Err(error) => log::error!("Clipboard: failed to copy history entry locally: {error}"),
    }
}

fn handle_clipboard_content(
    payload: &serde_json::Value,
    last_applied_hash: &Arc<Mutex<Option<String>>>,
    last_sent_hash: &Arc<Mutex<Option<String>>>,
    history: &SharedClipboardHistory,
    max_size: usize,
    history_size: usize,
    clipboard_writer: Option<&anchor_wl_clipboard::ClipboardWriter>,
) {
    let hash = match payload.get("hash").and_then(|h| h.as_str()) {
        Some(h) => h.to_string(),
        None => return,
    };

    // Echo guard: don't apply content we just sent
    {
        let sent = last_sent_hash.lock().unwrap();
        if sent.as_deref() == Some(&hash) {
            log::debug!("Clipboard: skipping remote content (matches last_sent_hash)");
            return;
        }
    }

    let raw_content = match payload.get("content").and_then(|c| c.as_str()) {
        Some(c) => c.to_string(),
        None => return,
    };

    // Decompress if the sender used compression
    let compressed_algo = payload.get("compressed").and_then(|c| c.as_str());
    let content = match compressed_algo {
        Some("zstd") => match base64::engine::general_purpose::STANDARD.decode(&raw_content) {
            Ok(compressed_bytes) => match zstd::decode_all(compressed_bytes.as_slice()) {
                Ok(decompressed) => {
                    log::debug!(
                        "Clipboard: zstd decompressed {} -> {} bytes",
                        compressed_bytes.len(),
                        decompressed.len()
                    );
                    String::from_utf8_lossy(&decompressed).into_owned()
                }
                Err(e) => {
                    log::warn!("Clipboard: zstd decompress failed: {}", e);
                    return;
                }
            },
            Err(e) => {
                log::warn!("Clipboard: bad base64 for compressed content: {}", e);
                return;
            }
        },
        Some("zlib") => match base64::engine::general_purpose::STANDARD.decode(&raw_content) {
            Ok(compressed_bytes) => {
                use std::io::Read;
                let mut decoder = flate2::read::DeflateDecoder::new(compressed_bytes.as_slice());
                let mut decompressed = String::new();
                match decoder.read_to_string(&mut decompressed) {
                    Ok(_) => {
                        log::debug!(
                            "Clipboard: zlib decompressed {} -> {} bytes",
                            compressed_bytes.len(),
                            decompressed.len()
                        );
                        decompressed
                    }
                    Err(e) => {
                        log::warn!("Clipboard: zlib decompress failed: {}", e);
                        return;
                    }
                }
            }
            Err(e) => {
                log::warn!("Clipboard: bad base64 for compressed content: {}", e);
                return;
            }
        },
        Some(other) => {
            log::warn!("Clipboard: unknown compression: {}", other);
            return;
        }
        None => raw_content,
    };

    let content_type_str =
        payload.get("content_type").and_then(|t| t.as_str()).unwrap_or("text/plain");

    let device_id =
        payload.get("device_id").and_then(|d| d.as_str()).unwrap_or("remote").to_string();

    let timestamp = payload.get("timestamp").and_then(|t| t.as_u64()).unwrap_or_else(now_ms);

    // Set echo guard BEFORE writing to clipboard
    *last_applied_hash.lock().unwrap() = Some(hash.clone());

    match content_type_str {
        "text/plain" => {
            if content.len() > max_size {
                log::warn!("Clipboard: remote text too large ({} bytes)", content.len());
                return;
            }
            if let Err(e) = set_desktop_clipboard_text(clipboard_writer, &content) {
                log::error!("Clipboard: failed to set local clipboard: {}", e);
                return;
            }
            log::debug!("Clipboard: applied {} bytes of text from {}", content.len(), device_id);
        }
        "image/png" => {
            let decoded = match base64::engine::general_purpose::STANDARD.decode(&content) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("Clipboard: bad base64 image: {}", e);
                    return;
                }
            };
            if decoded.len() > max_size {
                log::warn!("Clipboard: remote image too large ({} bytes)", decoded.len());
                return;
            }
            if let Err(e) = set_desktop_clipboard_png(clipboard_writer, &decoded) {
                log::error!("Clipboard: failed to set local clipboard image: {}", e);
                return;
            }
            log::debug!("Clipboard: applied {} bytes of PNG from {}", decoded.len(), device_id);
        }
        other => {
            log::debug!("Clipboard: unsupported content_type from remote: {}", other);
            return;
        }
    }

    let content_type = if content_type_str == "image/png" {
        ClipboardContentType::ImagePng
    } else {
        ClipboardContentType::Text
    };

    push_history(
        history,
        ClipboardEntry { content_type, content, timestamp, hash, device_id },
        history_size,
    );
}

fn handle_clipboard_connect(
    payload: &serde_json::Value,
    last_applied_hash: &Arc<Mutex<Option<String>>>,
    last_sent_hash: &Arc<Mutex<Option<String>>>,
    history: &SharedClipboardHistory,
    max_size: usize,
    history_size: usize,
    clipboard_writer: Option<&anchor_wl_clipboard::ClipboardWriter>,
) {
    let remote_timestamp = payload.get("timestamp").and_then(|t| t.as_u64()).unwrap_or(0);
    let local_timestamp = now_ms();

    // Only apply if remote clipboard is newer than ours
    if remote_timestamp > local_timestamp {
        log::debug!(
            "Clipboard: remote clipboard is newer (remote={}, local={}), applying",
            remote_timestamp,
            local_timestamp
        );
        handle_clipboard_content(
            payload,
            last_applied_hash,
            last_sent_hash,
            history,
            max_size,
            history_size,
            clipboard_writer,
        );
    } else {
        log::debug!(
            "Clipboard: local clipboard is newer (remote={}, local={}), keeping",
            remote_timestamp,
            local_timestamp
        );
    }
}

fn set_desktop_clipboard_text(
    writer: Option<&anchor_wl_clipboard::ClipboardWriter>,
    content: &str,
) -> Result<(), String> {
    writer
        .ok_or_else(|| "no local clipboard writer is available".to_string())?
        .set_text(content)
        .map_err(|error| format!("could not publish clipboard text: {error}"))
}

fn set_desktop_clipboard_png(
    writer: Option<&anchor_wl_clipboard::ClipboardWriter>,
    data: &[u8],
) -> Result<(), String> {
    writer
        .ok_or_else(|| "no local clipboard writer is available".to_string())?
        .set_image_png(data)
        .map_err(|error| format!("could not publish clipboard image: {error}"))
}

fn send_clipboard_connect(last_sent_hash: &Arc<Mutex<Option<String>>>) {
    // Try to read current clipboard to send as connect packet.
    // If we've sent something recently, re-send that.
    let sent = last_sent_hash.lock().unwrap();
    if sent.is_none() {
        // No clipboard content to share on connect
        return;
    }
    // The actual content isn't stored here — on connect we just let the
    // watcher pick up the current clipboard and send it naturally.
    // The connect packet from the remote side will be handled by handle_clipboard_connect.
    drop(sent);
}

// -- Helpers --

fn push_history(history: &SharedClipboardHistory, entry: ClipboardEntry, max_size: usize) {
    let mut h = history.lock().unwrap();
    h.push_front(entry);
    while h.len() > max_size {
        h.pop_back();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchorapp::clipboard_models::content_hash;

    #[test]
    fn push_history_respects_max_size() {
        let history: SharedClipboardHistory = Arc::new(Mutex::new(VecDeque::new()));
        for i in 0..10 {
            push_history(
                &history,
                ClipboardEntry {
                    content_type: ClipboardContentType::Text,
                    content: format!("entry {}", i),
                    timestamp: i as u64,
                    hash: format!("hash{}", i),
                    device_id: "test".to_string(),
                },
                5,
            );
        }
        let h = history.lock().unwrap();
        assert_eq!(h.len(), 5);
        // Most recent should be first
        assert_eq!(h[0].content, "entry 9");
        assert_eq!(h[4].content, "entry 5");
    }

    #[test]
    fn content_hash_deterministic() {
        let h1 = content_hash(b"test data");
        let h2 = content_hash(b"test data");
        assert_eq!(h1, h2);

        let h3 = content_hash(b"different data");
        assert_ne!(h1, h3);
    }
}
