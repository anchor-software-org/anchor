//! Desktop bridge for typed camera controls on `org.anchor.camera@1`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anchor_sdk::{Capability, DatagramFlow, camera, video_frame};

use crate::anchorapp::device::camera_receiver::{CameraSink, CameraTxCell};

#[derive(Clone)]
pub struct SdkCameraSender {
    tx: mpsc::SyncSender<CameraControlMessage>,
}

struct CameraControlMessage {
    control: camera::CameraControl,
}

impl SdkCameraSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel::<CameraControlMessage>(16);
        thread::Builder::new()
            .name("anchor-sdk-camera-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK camera runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    if let Err(error) = runtime.block_on(capability.send_record(
                        camera::CONTROL_TYPE_URL,
                        camera::encode_control(message.control),
                    )) {
                        log::warn!("SDK camera control send failed: {error}");
                    } else {
                        log::debug!("Camera: sent typed SDK control over QUIC");
                    }
                }
            })
            .expect("SDK camera writer thread must start");
        Self { tx }
    }

    fn send(&self, control: camera::CameraControl) -> bool {
        self.tx.try_send(CameraControlMessage { control }).is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkCameraBinding {
    senders: Arc<Mutex<HashMap<String, SdkCameraSender>>>,
    camera_tx_cell: Arc<Mutex<Option<CameraTxCell>>>,
}

impl SdkCameraBinding {
    pub fn attach(&self, device_id: String, sender: SdkCameraSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    /// Install the V4L2 sink cell used by the camera receiver. Keeping this
    /// late-bound allows the SDK session to connect before the user creates
    /// the loopback device in Settings.
    pub fn set_camera_tx_cell(&self, cell: CameraTxCell) {
        *self.camera_tx_cell.lock().unwrap() = Some(cell);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn sender(&self, device_id: &str) -> Option<SdkCameraSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn senders(&self) -> Vec<(String, SdkCameraSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }

    /// Accept a camera frame flow and drain it into the selected V4L2 sink.
    pub fn attach_frame_flow(
        &self,
        device_id: String,
        capability_session_id: u64,
        flow: DatagramFlow,
    ) {
        let expected_flow_id = flow.flow_id();
        let sink = self
            .camera_tx_cell
            .lock()
            .unwrap()
            .clone()
            .map(|cell| CameraSink { cell, device_id: device_id.clone() });
        if sink.is_none() {
            log::warn!(
                "SDK camera frame flow has no V4L2 sink configured; draining for validation"
            );
        }
        thread::Builder::new()
            .name(format!("anchor-sdk-camera-frames-{device_id}"))
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK camera frame runtime failed: {error}");
                            return;
                        }
                    };
                let mut assembler = CameraFrameAssembler::default();
                let mut frames = 0_u64;
                runtime.block_on(async move {
                    loop {
                        let datagram = match flow.recv().await {
                            Ok(datagram) => datagram,
                            Err(error) => {
                                log::info!(
                                    "SDK camera datagram receiver ended for {device_id}: {error}"
                                );
                                break;
                            }
                        };
                        let Some(frame) =
                            assembler.add(&datagram, capability_session_id, expected_flow_id)
                        else {
                            continue;
                        };
                        frames += 1;
                        if let Some(tx) = sink.as_ref().and_then(CameraSink::current)
                            && tx.try_send(Arc::new(frame)).is_err()
                        {
                            log::debug!("SDK camera frame dropped for {device_id}: sink busy");
                        }
                        if frames.is_multiple_of(60) {
                            log::info!("SDK camera frames received: {frames}");
                        }
                    }
                });
            })
            .expect("SDK camera frame thread must start");
    }
}

#[derive(Default)]
struct CameraFrameAssembler {
    partial: HashMap<u64, PartialCameraFrame>,
}

struct PartialCameraFrame {
    created: Instant,
    fragment_count: u16,
    fragments: Vec<Option<Vec<u8>>>,
    received: usize,
    total_bytes: usize,
}

impl CameraFrameAssembler {
    fn add(
        &mut self,
        datagram: &[u8],
        capability_session_id: u64,
        flow_id: u64,
    ) -> Option<Vec<u8>> {
        let (header, payload) = match video_frame::FrameHeader::decode(datagram) {
            Ok(header) => header,
            Err(_) => return None,
        };
        if header.kind != video_frame::FRAME_KIND_CAMERA
            || header.capability_session_id != capability_session_id
            || header.flow_id != flow_id
            || header.fragment_count == 0
            || header.fragment_index >= header.fragment_count
        {
            return None;
        }
        // Keep this map bounded and discard half-assembled frames quickly;
        // datagrams are intentionally lossy and must never grow memory.
        let now = Instant::now();
        self.partial
            .retain(|_, frame| now.duration_since(frame.created) <= Duration::from_millis(500));
        if self.partial.len() >= 32
            && !self.partial.contains_key(&header.sequence)
            && let Some(oldest) = self
                .partial
                .iter()
                .min_by_key(|(_, frame)| frame.created)
                .map(|(sequence, _)| *sequence)
        {
            self.partial.remove(&oldest);
        }
        let entry = self.partial.entry(header.sequence).or_insert_with(|| PartialCameraFrame {
            created: now,
            fragment_count: header.fragment_count,
            fragments: vec![None; usize::from(header.fragment_count)],
            received: 0,
            total_bytes: 0,
        });
        if entry.fragment_count != header.fragment_count {
            self.partial.remove(&header.sequence);
            return None;
        }
        let payload = payload.to_vec();
        let index = usize::from(header.fragment_index);
        if entry.fragments[index].is_none() {
            entry.total_bytes += payload.len();
            if entry.total_bytes > video_frame::MAX_FRAME_BYTES {
                self.partial.remove(&header.sequence);
                return None;
            }
            entry.fragments[index] = Some(payload);
            entry.received += 1;
        }
        if entry.received != usize::from(entry.fragment_count) {
            return None;
        }
        let entry = self.partial.remove(&header.sequence)?;
        let mut frame = Vec::with_capacity(entry.total_bytes);
        for fragment in entry.fragments {
            frame.extend(fragment?);
        }
        Some(frame)
    }
}

/// Translate a desktop camera JSON command into a compact typed control.
pub fn send_json(sender: &SdkCameraSender, value: &serde_json::Value) -> bool {
    if value.get("plugin_id").and_then(serde_json::Value::as_str) != Some("cameraplugin") {
        return false;
    }
    let Some(command) = value.get("command").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let mut control = camera::CameraControl {
        kind: match command {
            "start" => 1,
            "stop" => 2,
            "switch_camera" => 3,
            "set_stream_params" => 4,
            "set_zoom_ratio" => 5,
            "set_linear_zoom" => 6,
            "set_torch" => 7,
            "set_exposure" => 8,
            _ => return false,
        },
        ..Default::default()
    };
    if command == "set_stream_params" {
        control.fps =
            value.get("fps").and_then(serde_json::Value::as_u64).unwrap_or_default() as u32;
        control.bitrate_kbps =
            value.get("bitrate_kbps").and_then(serde_json::Value::as_u64).unwrap_or_default()
                as u32;
    } else if command == "set_zoom_ratio" {
        control.zoom_ratio =
            value.get("value").and_then(serde_json::Value::as_f64).unwrap_or(1.0) as f32;
    } else if command == "set_linear_zoom" {
        control.linear_zoom =
            value.get("value").and_then(serde_json::Value::as_f64).unwrap_or_default() as f32;
    } else if command == "set_exposure" {
        control.exposure =
            value.get("value").and_then(serde_json::Value::as_i64).unwrap_or_default() as i32;
    } else if command == "set_torch" {
        control.torch_enabled =
            value.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false);
    }
    sender.send(control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_camera_command_is_not_claimed() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let sender = SdkCameraSender { tx };
        assert!(!send_json(
            &sender,
            &serde_json::json!({"plugin_id":"cameraplugin", "command":"future"})
        ));
    }

    #[test]
    fn camera_frame_assembler_reassembles_out_of_order_and_filters_flow() {
        let payload = (0..(video_frame::FRAME_PAYLOAD_BYTES * 2 + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let packets =
            video_frame::fragment_frame(video_frame::FRAME_KIND_CAMERA, 9, 4, 77, 123, 1, &payload)
                .unwrap();
        let mut assembler = CameraFrameAssembler::default();
        assert!(assembler.add(&packets[0], 9, 999).is_none());
        assert!(assembler.add(&packets[2], 9, 4).is_none());
        assert!(assembler.add(&packets[2], 9, 4).is_none());
        assert!(assembler.add(&packets[1], 9, 4).is_none());
        let rebuilt = assembler.add(&packets[0], 9, 4).unwrap();
        assert_eq!(rebuilt, payload);
    }

    #[test]
    fn camera_frame_assembler_rejects_wrong_kind_and_oversized_partial() {
        let packet = video_frame::FrameHeader {
            kind: video_frame::FRAME_KIND_SCREEN,
            flags: 0,
            capability_session_id: 1,
            flow_id: 2,
            sequence: 3,
            fragment_index: 0,
            fragment_count: 1,
            presentation_time_us: 0,
            codec_config_id: 0,
        }
        .encode(&[1, 2, 3])
        .unwrap();
        let mut assembler = CameraFrameAssembler::default();
        assert!(assembler.add(&packet, 1, 2).is_none());
    }
}
