//! Ordered desktop-to-device sender for the `org.anchor.input` capability.
//!
//! The same capability session carries phone-as-touchpad records inbound and
//! remote-control records outbound; this binding is the outbound half. The
//! Android side normalizes pointer coordinates against its default display,
//! so callers send [0,1] fractions which this module scales to the wire's
//! [0, 65535] range.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use anchor_sdk::{Capability, DatagramFlow, input, video_frame};

const POINTER_BUTTON_LEFT: i32 = 1;

struct InputRecord {
    type_url: &'static str,
    payload: Vec<u8>,
}

#[derive(Clone)]
pub struct SdkRemoteInputSender {
    tx: SyncSender<InputRecord>,
}

impl SdkRemoteInputSender {
    pub fn new(capability: Capability) -> Self {
        // High-rate motion must not build a stale queue: a bounded channel
        // makes `send` drop the oldest-pending behavior cheap at the caller.
        let (tx, rx) = mpsc::sync_channel::<InputRecord>(256);
        thread::Builder::new()
            .name("anchor-sdk-remote-input-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK remote-input runtime failed: {error}");
                            return;
                        }
                    };
                for record in rx {
                    let result = runtime.block_on(async {
                        capability.send_record(record.type_url, record.payload).await
                    });
                    if let Err(error) = result {
                        log::warn!("SDK remote-input send failed: {error}");
                    }
                }
            })
            .expect("SDK remote-input writer thread must start");
        Self { tx }
    }

    fn send(&self, type_url: &'static str, payload: Vec<u8>) -> bool {
        self.tx
            .try_send(InputRecord { type_url, payload })
            .is_ok()
    }

    fn move_absolute(&self, x_norm: f64, y_norm: f64) -> bool {
        let x = (x_norm.clamp(0.0, 1.0) * 65535.0).round() as u32;
        let y = (y_norm.clamp(0.0, 1.0) * 65535.0).round() as u32;
        self.send(
            input::POINTER_ABSOLUTE_TYPE_URL,
            input::encode_pointer_absolute(x, y, ""),
        )
    }

    fn pointer_button(&self, button: i32, pressed: bool) -> bool {
        self.send(
            input::POINTER_BUTTON_TYPE_URL,
            input::encode_pointer_button(button, pressed),
        )
    }

    fn tap(&self, x_norm: f64, y_norm: f64) -> bool {
        self.move_absolute(x_norm, y_norm)
            && self.pointer_button(POINTER_BUTTON_LEFT, true)
            && self.pointer_button(POINTER_BUTTON_LEFT, false)
    }

    fn remote_control(&self, kind: i32) -> bool {
        self.send(
            input::REMOTE_CONTROL_TYPE_URL,
            input::encode_remote_control(kind),
        )
    }
}

/// Latest reassembled mirror frame for a device. Phase 1 carries JPEG stills
/// produced by `AccessibilityService.takeScreenshot`; the slot is payload-typed
/// (`jpeg` bytes) so a later H.264 producer can replace the encoding without
/// touching the flow plumbing.
#[derive(Clone)]
pub struct MirrorFrame {
    /// Monotonic receive-order sequence, unique per binding instance.
    pub sequence: u64,
    /// ANFR frame sequence echoed back for debugging/ordering.
    pub frame_sequence: u64,
    pub jpeg: Arc<Vec<u8>>,
}

#[derive(Clone, Default)]
pub struct SdkRemoteInputBinding {
    senders: Arc<Mutex<HashMap<String, SdkRemoteInputSender>>>,
    frames: Arc<Mutex<HashMap<String, MirrorFrame>>>,
    frame_sequence: Arc<AtomicU64>,
}

impl SdkRemoteInputBinding {
    pub fn attach(&self, device_id: String, sender: SdkRemoteInputSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn sender(&self, device_id: &str) -> Option<SdkRemoteInputSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn senders(&self) -> Vec<(String, SdkRemoteInputSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }

    /// Accept the device's mirror datagram flow and keep only the newest
    /// reassembled frame — the UI polls, so stale stills are worthless.
    pub fn attach_frame_flow(
        &self,
        device_id: String,
        capability_session_id: u64,
        flow: DatagramFlow,
    ) {
        let expected_flow_id = flow.flow_id();
        let frames = Arc::clone(&self.frames);
        let frame_sequence = Arc::clone(&self.frame_sequence);
        thread::Builder::new()
            .name(format!("anchor-sdk-mirror-frames-{device_id}"))
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK mirror frame runtime failed: {error}");
                            return;
                        }
                    };
                let mut assembler = video_frame::Reassembler::default();
                let mut received = 0_u64;
                runtime.block_on(async move {
                    loop {
                        let datagram = match flow.recv().await {
                            Ok(datagram) => datagram,
                            Err(error) => {
                                log::info!(
                                    "SDK mirror datagram receiver ended for {device_id}: {error}"
                                );
                                break;
                            }
                        };
                        let Some(payload) = assembler.add_datagram(
                            &datagram,
                            video_frame::FRAME_KIND_SCREEN,
                            capability_session_id,
                            expected_flow_id,
                        ) else {
                            continue;
                        };
                        let sequence = frame_sequence.fetch_add(1, Ordering::Relaxed) + 1;
                        let frame_sequence_field = video_frame::FrameHeader::decode(&datagram)
                            .map(|(header, _)| header.sequence)
                            .unwrap_or(0);
                        frames.lock().unwrap().insert(
                            device_id.clone(),
                            MirrorFrame {
                                sequence,
                                frame_sequence: frame_sequence_field,
                                jpeg: Arc::new(payload),
                            },
                        );
                        received += 1;
                        if received == 1 || received.is_multiple_of(50) {
                            log::info!("SDK mirror frames received from {device_id}: {received}");
                        }
                    }
                });
            })
            .expect("SDK mirror frame thread must start");
    }

    /// Latest mirror frame after `after_sequence` (0 returns the newest).
    pub fn frame_after(&self, device_id: &str, after_sequence: u64) -> Option<MirrorFrame> {
        let frame = self.frames.lock().unwrap().get(device_id).cloned()?;
        (frame.sequence > after_sequence).then_some(frame)
    }

    /// Newest mirror frame across all devices — the UI's remote-control pane
    /// targets a single phone, so the freshest frame wins.
    pub fn latest_frame(&self, after_sequence: u64) -> Option<MirrorFrame> {
        self.frames
            .lock()
            .unwrap()
            .values()
            .filter(|frame| frame.sequence > after_sequence)
            .max_by_key(|frame| frame.sequence)
            .cloned()
    }
}

/// Route a `plugin_id: "remoteinput"` broker message onto the capability.
/// Returns true when the message was recognized (even if delivery failed).
pub fn send_json(sender: &SdkRemoteInputSender, value: &serde_json::Value) -> bool {
    let Some(kind) = value.get("type").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let num = |key: &str| value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    match kind {
        "remote_tap" => sender.tap(num("x"), num("y")),
        "remote_pointer_down" => {
            sender.move_absolute(num("x"), num("y"))
                && sender.pointer_button(POINTER_BUTTON_LEFT, true)
        }
        "remote_pointer_move" => sender.move_absolute(num("x"), num("y")),
        "remote_pointer_up" => sender.pointer_button(POINTER_BUTTON_LEFT, false),
        "remote_scroll" => sender.send(
            input::SCROLL_TYPE_URL,
            input::encode_scroll(num("dx") as i32, num("dy") as i32),
        ),
        "remote_text" => sender.send(
            input::TEXT_TYPE_URL,
            input::encode_text(
                value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            ),
        ),
        // Navigation actions have no HID usage — they ride RemoteControl.
        "remote_back" => sender.remote_control(input::REMOTE_CONTROL_BACK),
        "remote_home" => sender.remote_control(input::REMOTE_CONTROL_HOME),
        "remote_recents" => sender.remote_control(input::REMOTE_CONTROL_RECENTS),
        "remote_mirror" => sender.remote_control(
            if value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
            {
                input::REMOTE_CONTROL_MIRROR_START
            } else {
                input::REMOTE_CONTROL_MIRROR_STOP
            },
        ),
        _ => return false,
    };
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_starts_empty() {
        assert!(SdkRemoteInputBinding::default().senders().is_empty());
    }
}
