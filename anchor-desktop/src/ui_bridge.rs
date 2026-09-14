//! Framework-neutral state and actions exposed by desktop frontends.
//!
//! Video frames deliberately stay on Anchor's native capture/encode path. A
//! frontend receives control-plane state only; it never copies raw frames
//! through webview IPC.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{
    Mutex,
    atomic::{AtomicI64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::anchorapp::clipboard_models::ClipboardContentType;
use crate::anchorapp::command_models::{Command as SavedCommand, save_commands};
use crate::anchorapp::compositor::Compositor;
use crate::anchorapp::config::init_config;
use crate::anchorapp::device::{CameraGuiStatus, CameraSetupHandle, DeviceRegistry};
use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget, SharedPairingRequest};
use crate::anchorapp::fd_probe;
use crate::anchorapp::filetransfer_plugin::{
    SharedFolder, SharedTransferHistory, TransferDirection,
};
use crate::anchorapp::log_collector::LogBuffer;
use crate::anchorapp::media_plugin::{
    Command as MediaCommandKind, MediaCommand, MediaMessage, SharedMediaState, send_media_message,
};
use crate::anchorapp::plugin::SharedFrameBuffer;
use crate::anchorapp::settings::{
    init_settings, save_camera_auto_load, save_camera_stream_params, save_settings, settings,
};
use crate::anchorapp::sms_models::SmsThread;
use crate::anchorapp::sms_plugin::SharedSmsThreads;
use crate::anchorapp::tls::load_or_create_identity;
use crate::anchorwayland::wayland_plugin::WaylandPluginMsg;
use crate::app::{AnchorAppInstance, DesktopRuntime};
use crate::foghorn::SharedNotifications;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopState {
    pub app_name: String,
    pub version: String,
    pub build: String,
    pub compositor: String,
    pub environment: EnvironmentSummary,
    pub capture: CapturePanelState,
    pub virtual_displays: Vec<VirtualDisplaySummary>,
    pub devices: Vec<PairedDeviceSummary>,
    pub pairing_request: Option<PairingRequestSummary>,
    pub notifications: Vec<NotificationSummary>,
    pub sms_threads: Vec<SmsThreadSummary>,
    pub selected_sms_thread: Option<i64>,
    pub sms_messages: Vec<SmsMessageSummary>,
    pub sms_available: bool,
    pub clipboard: Vec<ClipboardSummary>,
    pub transfers: Vec<TransferSummary>,
    pub shared_folder: String,
    pub commands: Vec<CommandSummary>,
    pub phone_media: Option<MediaSummary>,
    pub desktop_media: Option<MediaSummary>,
    pub logs: Vec<LogSummary>,
    pub camera: CameraSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePanelState {
    /// `initializing`, `idle`, `starting`, `streaming`, `switching`,
    /// `stopping`, `error`, or `unavailable`.
    pub status: String,
    pub unavailable_reason: Option<String>,
    pub outputs: Vec<OutputSummary>,
    pub selected_output_index: usize,
    pub metrics: Option<CaptureMetrics>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSummary {
    pub index: usize,
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    pub refresh_rate_mhz: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualDisplaySummary {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMetrics {
    pub fps: u32,
    pub avg_frame_ms: u32,
    pub frame_size_bytes: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenPreview {
    pub sequence: u64,
    pub data_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceSummary {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub battery_level: Option<u8>,
    pub battery_charging: bool,
    pub fingerprint: String,
    pub camera_capable: bool,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub screen_sharing: Option<bool>,
    pub pointer_input: Option<bool>,
    pub keyboard_input: Option<bool>,
    pub virtual_displays: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingRequestSummary {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub fingerprint: String,
    pub safety_number: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSummary {
    pub source: String,
    pub app_name: String,
    pub app_package: String,
    pub icon_key: String,
    pub title: String,
    pub body: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmsThreadSummary {
    pub thread_id: i64,
    pub name: String,
    pub snippet: String,
    pub last_updated: i64,
    pub has_photo: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmsMessageSummary {
    pub uid: i64,
    pub body: String,
    pub date: i64,
    pub sent: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardSummary {
    pub hash: String,
    pub content_type: String,
    pub preview: String,
    pub timestamp: u64,
    pub size_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSummary {
    pub name: String,
    pub direction: String,
    pub size_bytes: u64,
    pub timestamp: u64,
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFileImport {
    pub imported: usize,
    pub rejected: usize,
    pub names: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSummary {
    pub id: String,
    pub name: String,
    pub command: String,
    pub description: String,
    pub detach: bool,
    pub run_status: Option<String>,
    pub run_detail: Option<String>,
}

#[derive(Debug, Clone)]
struct CommandRunState {
    status: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSummary {
    pub session_id: String,
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub art_url: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_prev: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSummary {
    pub timestamp: u64,
    pub source: String,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraSummary {
    pub status: String,
    pub device: Option<String>,
    pub selected_device_id: Option<String>,
    pub auto_load: bool,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub zoom_ratio: f32,
    pub exposure: i32,
    pub torch_on: bool,
}

#[derive(Debug, Clone)]
struct CameraPreferences {
    auto_load: bool,
    fps: u32,
    bitrate_kbps: u32,
    zoom_ratio: f32,
    exposure: i32,
    torch_on: bool,
}

#[derive(Debug, Clone, Default)]
struct InputCapabilities {
    pointer: Option<bool>,
    keyboard: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConversationMessage {
    uid: i64,
    body: Option<String>,
    date: i64,
    #[serde(default, rename = "message_type", alias = "messageType")]
    message_type: i32,
}

/// Owns only the control-plane boundary between a frontend and Anchor's
/// existing desktop services.
pub struct DesktopController {
    broker_tx: std::sync::mpsc::Sender<AnchorEvent>,
    gui_events: Mutex<std::sync::mpsc::Receiver<AnchorEvent>>,
    device_registry: DeviceRegistry,
    pairing_request: SharedPairingRequest,
    capture: Mutex<CapturePanelState>,
    virtual_displays: Mutex<Vec<VirtualDisplaySummary>>,
    log_buffer: LogBuffer,
    foghorn_notifications: SharedNotifications,
    sms_threads: SharedSmsThreads,
    selected_sms_thread: Mutex<Option<i64>>,
    sms_messages: Mutex<HashMap<i64, Vec<SmsMessageSummary>>>,
    next_pending_sms_uid: AtomicI64,
    clipboard_history: crate::anchorapp::clipboard_models::SharedClipboardHistory,
    filetransfer_history: SharedTransferHistory,
    filetransfer_folder: SharedFolder,
    remote_media_state: SharedMediaState,
    desktop_media_state: SharedMediaState,
    commands: crate::anchorapp::command_models::SharedCommands,
    command_runs: Mutex<HashMap<String, CommandRunState>>,
    camera_setup: CameraSetupHandle,
    camera_preferences: Mutex<CameraPreferences>,
    mobile_batteries: crate::anchorapp::battery::MobileBatteryMap,
    input_capabilities: Mutex<InputCapabilities>,
    frame_buffer: std::sync::Arc<Mutex<Option<SharedFrameBuffer>>>,
}

impl DesktopController {
    pub fn from_runtime(runtime: DesktopRuntime) -> Self {
        let camera_settings = settings().camera.clone();
        let DesktopRuntime {
            broker_tx,
            gui_events,
            device_registry,
            pairing_request,
            log_buffer,
            foghorn_notifications,
            sms_threads,
            clipboard_history,
            filetransfer_history,
            filetransfer_folder,
            remote_media_state,
            desktop_media_state,
            commands,
            camera_setup,
            frame_buffer,
        } = runtime;
        Self {
            broker_tx,
            gui_events: Mutex::new(gui_events),
            device_registry,
            pairing_request,
            capture: Mutex::new(CapturePanelState {
                status: "initializing".into(),
                unavailable_reason: None,
                outputs: vec![],
                selected_output_index: 0,
                metrics: None,
            }),
            virtual_displays: Mutex::new(Vec::new()),
            log_buffer,
            foghorn_notifications,
            sms_threads,
            selected_sms_thread: Mutex::new(None),
            sms_messages: Mutex::new(HashMap::new()),
            // Negative IDs cannot collide with Android's database IDs. They
            // identify messages displayed immediately while the phone's
            // authoritative conversation snapshot is still in flight.
            // Start far negative and *increment* so later pending messages
            // sort later when dates are equal (otherwise -2 < -1 would put
            // the second sent message before the first).
            next_pending_sms_uid: AtomicI64::new(-1_000_000),
            clipboard_history,
            filetransfer_history,
            filetransfer_folder,
            remote_media_state,
            desktop_media_state,
            commands,
            command_runs: Mutex::new(HashMap::new()),
            camera_setup,
            frame_buffer,
            mobile_batteries: crate::anchorapp::battery::new_mobile_battery_map(),
            input_capabilities: Mutex::new(InputCapabilities::default()),
            camera_preferences: Mutex::new(CameraPreferences {
                auto_load: camera_settings.auto_load,
                fps: camera_settings.fps,
                bitrate_kbps: camera_settings.bitrate_kbps,
                zoom_ratio: 1.0,
                exposure: 0,
                torch_on: false,
            }),
        }
    }

    /// Return the current state. GUI events are applied by the desktop
    /// notifier thread, so this stays a read-only snapshot operation.
    pub fn state(&self, section: Option<&str>) -> DesktopState {
        let section = section.unwrap_or("sideboat");
        if section == "sideboat" && self.capture.lock().unwrap().outputs.is_empty() {
            // The backend can announce startup outputs before a webview has
            // subscribed. Request a one-shot re-send only while this cache is
            // empty; its reply follows the normal event-driven path.
            let _ = self.send_wayland(WaylandPluginMsg::RequestOutputs);
        }
        let connected_devices = self.device_registry.connected_devices();
        let sms_available = connected_devices.iter().any(|device| {
            device.has_control
                && device.capabilities.iter().any(|capability| capability == "org.anchor.sms")
        });
        let mobile_batteries = self.mobile_batteries.lock().unwrap().clone();
        let display_sizes: HashMap<_, _> = connected_devices
            .iter()
            .filter_map(|device| device.display_size.map(|size| (device.id.clone(), size)))
            .collect();
        let camera_devices: HashSet<String> = if section == "settings" {
            connected_devices
                .iter()
                .filter(|device| {
                    device.capabilities.iter().any(|capability| capability == "org.anchor.camera")
                })
                .map(|device| device.id.clone())
                .collect()
        } else {
            HashSet::new()
        };
        let selected_sms_thread =
            if section == "messages" { *self.selected_sms_thread.lock().unwrap() } else { None };
        let sms_messages = if section == "messages" {
            selected_sms_thread
                .and_then(|thread_id| self.sms_messages.lock().unwrap().get(&thread_id).cloned())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let camera = if section == "settings" {
            self.camera_summary()
        } else {
            CameraSummary {
                status: String::new(),
                device: None,
                selected_device_id: None,
                auto_load: false,
                fps: 0,
                bitrate_kbps: 0,
                zoom_ratio: 1.0,
                exposure: 0,
                torch_on: false,
            }
        };

        let capture = self.capture.lock().unwrap().clone();
        let screen_sharing = match capture.status.as_str() {
            "initializing" => None,
            "unavailable" => Some(false),
            _ => Some(true),
        };
        let input_capabilities = self.input_capabilities.lock().unwrap().clone();

        DesktopState {
            app_name: "Anchor".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            build: env!("GIT_HASH").into(),
            compositor: Compositor::display_name().into(),
            environment: EnvironmentSummary {
                screen_sharing,
                pointer_input: input_capabilities.pointer,
                keyboard_input: input_capabilities.keyboard,
                virtual_displays: Compositor::get().supports_virtual_displays(),
            },
            capture,
            virtual_displays: if section == "sideboat" {
                self.virtual_displays.lock().unwrap().clone()
            } else {
                Vec::new()
            },
            devices: self
                .device_registry
                .paired_devices()
                .into_iter()
                .map(|device| {
                    let camera_capable = camera_devices.contains(&device.device_id);
                    let display_size = display_sizes.get(&device.device_id).copied();
                    let battery = mobile_batteries.get(&device.device_id);
                    PairedDeviceSummary {
                        id: device.device_id,
                        name: device.device_name,
                        online: device.is_online,
                        battery_level: battery.map(|battery| battery.level),
                        battery_charging: battery.is_some_and(|battery| battery.charging),
                        fingerprint: certificate_fingerprint(&device.certificate_pem),
                        camera_capable,
                        display_width: display_size.map(|(width, _)| width),
                        display_height: display_size.map(|(_, height)| height),
                    }
                })
                .collect(),
            pairing_request: self.pairing_request.lock().unwrap().as_ref().map(|request| {
                PairingRequestSummary {
                    device_id: request.device_id.clone(),
                    device_name: request.device_name.clone(),
                    device_type: request.device_type.clone(),
                    fingerprint: request.fingerprint.clone(),
                    safety_number: request.safety_number.clone(),
                }
            }),
            notifications: if section == "foghorn" {
                self.foghorn_notifications
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .map(|entry| NotificationSummary {
                        source: entry.source.clone(),
                        app_name: entry.app_name.clone(),
                        app_package: entry.app_package.clone(),
                        icon_key: if entry.app_package.is_empty() {
                            format!("desktop:{}", entry.icon_name)
                        } else {
                            format!("android:{}", entry.app_package)
                        },
                        title: entry.title.clone(),
                        body: entry.body.clone(),
                        timestamp: entry.timestamp,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            sms_threads: if section == "messages" {
                self.sms_thread_summaries()
            } else {
                Vec::new()
            },
            selected_sms_thread,
            sms_messages,
            sms_available,
            clipboard: if section == "clipboard" {
                self.clipboard_history
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|entry| ClipboardSummary {
                        hash: entry.hash.clone(),
                        content_type: match entry.content_type {
                            ClipboardContentType::Text => "text".into(),
                            ClipboardContentType::ImagePng => "image".into(),
                        },
                        preview: clipboard_preview(&entry.content, &entry.content_type),
                        timestamp: entry.timestamp,
                        size_bytes: entry.content.len(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            transfers: if section == "files" {
                self.filetransfer_history
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|entry| TransferSummary {
                        name: entry.name.clone(),
                        direction: match entry.direction {
                            TransferDirection::Sent => "sent".into(),
                            TransferDirection::Received => "received".into(),
                        },
                        size_bytes: entry.size,
                        timestamp: entry.timestamp_ms,
                        path: entry.path.to_string_lossy().into_owned(),
                        exists: entry.path.exists(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            shared_folder: if section == "files" {
                self.filetransfer_folder.lock().unwrap().to_string_lossy().into_owned()
            } else {
                String::new()
            },
            commands: if section == "commands" {
                let runs = self.command_runs.lock().unwrap().clone();
                self.commands
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|command| {
                        let run = runs.get(&command.id);
                        CommandSummary {
                            id: command.id.clone(),
                            name: command.name.clone(),
                            command: command.command.clone(),
                            description: command.description.clone(),
                            detach: command.detach,
                            run_status: run.map(|run| run.status.clone()),
                            run_detail: run.and_then(|run| run.detail.clone()),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            },
            phone_media: if section == "media" {
                self.media_summary(&self.remote_media_state)
            } else {
                None
            },
            desktop_media: if section == "media" {
                self.media_summary(&self.desktop_media_state)
            } else {
                None
            },
            logs: if section == "logs" {
                self.log_buffer
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .take(500)
                    .rev()
                    .map(|entry| LogSummary {
                        timestamp: entry.timestamp_ms,
                        source: entry.source.into(),
                        level: entry.level.clone(),
                        message: entry.message.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            camera,
        }
    }

    pub fn set_streaming(&self, enabled: bool) -> Result<(), String> {
        self.send_wayland(if enabled { WaylandPluginMsg::Start } else { WaylandPluginMsg::Stop })
    }

    pub fn select_output(&self, index: usize) -> Result<(), String> {
        {
            let capture = self.capture.lock().unwrap();
            if index >= capture.outputs.len() {
                return Err("The selected output is no longer available.".into());
            }
        }
        self.send_wayland(WaylandPluginMsg::SelectOutput(index))?;

        // Reflect the queued change immediately, as the legacy UI did. The
        // Wayland plugin emits an authoritative stream-status update after it
        // applies the switch, and `apply_capture_json` replaces this value.
        self.capture.lock().unwrap().selected_output_index = index;
        Ok(())
    }

    pub fn create_virtual_display(&self, width: u32, height: u32) -> Result<(), String> {
        let output = Compositor::get().create_sized_virtual_output(width, height)?;
        self.virtual_displays.lock().unwrap().push(VirtualDisplaySummary {
            name: output.name,
            width: output.width,
            height: output.height,
        });
        self.send_wayland(WaylandPluginMsg::RequestOutputs)
    }

    pub fn destroy_virtual_display(&self, name: &str) -> Result<(), String> {
        if !self.virtual_displays.lock().unwrap().iter().any(|display| display.name == name) {
            return Err("Anchor can only remove virtual displays created in this session.".into());
        }

        // The capture worker must finish any in-flight frame and move to a
        // valid fallback before Sway removes the output. Otherwise its selected
        // index points past the refreshed output list and the worker is lost.
        let (response_tx, response_rx) = std::sync::mpsc::sync_channel(1);
        self.send_wayland(WaylandPluginMsg::PrepareOutputRemoval {
            name: name.to_string(),
            response_tx,
        })?;
        response_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "Timed out while stopping capture for display removal.".to_string())??;

        Compositor::get().destroy_virtual_output(name)?;
        self.virtual_displays.lock().unwrap().retain(|display| display.name != name);
        self.send_wayland(WaylandPluginMsg::RequestOutputs)
    }

    pub fn set_sideboat_visible(&self, visible: bool) -> Result<(), String> {
        self.send_wayland(WaylandPluginMsg::TabVisible(visible))
    }

    pub fn screen_preview_frame(&self, after_sequence: u64) -> Option<SharedFrameBuffer> {
        self.frame_buffer
            .lock()
            .unwrap()
            .as_ref()
            .filter(|frame| frame.sequence > after_sequence)
            .cloned()
    }

    pub fn unpair_device(&self, device_id: &str) {
        self.device_registry.unpair_device(device_id);
    }

    pub fn respond_to_pairing(&self, accepted: bool) -> Result<(), String> {
        let request = self
            .pairing_request
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "There is no pending pairing request.".to_string())?;
        request
            .response_tx
            .send(accepted)
            .map_err(|_| "The device stopped waiting for the pairing response.".to_string())
    }

    pub fn dismiss_notification(&self, index: usize) -> Result<(), String> {
        let mut notifications = self.foghorn_notifications.lock().unwrap();
        let actual_index = notifications
            .len()
            .checked_sub(index + 1)
            .ok_or_else(|| "Notification no longer exists.".to_string())?;
        notifications.remove(actual_index);
        Ok(())
    }

    pub fn clear_notifications(&self) {
        self.foghorn_notifications.lock().unwrap().clear();
    }

    pub fn notification_icon(&self, icon_key: &str) -> Result<Option<String>, String> {
        let path = if let Some(app_package) = icon_key.strip_prefix("android:") {
            crate::foghorn::cached_icon_path(app_package)
        } else if let Some(icon_name) = icon_key.strip_prefix("desktop:") {
            crate::foghorn::system_icon_path(icon_name)
        } else {
            None
        };
        let Some(path) = path else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("Could not read notification icon: {error}"))?;
        if bytes.len() > 1_048_576 {
            return Err("Notification icon exceeds the 1 MB limit.".into());
        }
        let mime = match path.extension().and_then(|extension| extension.to_str()) {
            Some("svg") => "image/svg+xml",
            _ => "image/png",
        };
        Ok(Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )))
    }

    pub fn sms_contact_photo(&self, thread_id: i64) -> Option<String> {
        let threads = self.sms_threads.lock().unwrap();
        let photo = threads
            .iter()
            .find(|thread| thread.thread_id == thread_id)?
            .addresses
            .iter()
            .find_map(|address| address.photo.as_deref())?
            .trim();
        if photo.is_empty() || photo.len() > 1_048_576 {
            return None;
        }
        Some(if photo.starts_with("data:image/") {
            photo.to_string()
        } else {
            format!("data:image/jpeg;base64,{photo}")
        })
    }

    pub fn select_sms_thread(&self, thread_id: i64) -> Result<(), String> {
        *self.selected_sms_thread.lock().unwrap() = Some(thread_id);
        self.send_service_json(
            "smsplugin",
            serde_json::json!({ "type": "sms.request_conversation", "thread_id": thread_id }),
        )
    }

    pub fn send_sms(&self, thread_id: i64, body: String) -> Result<(), String> {
        let body = body.trim().to_string();
        if body.is_empty() {
            return Err("A message cannot be empty.".into());
        }
        let sms_available = self.device_registry.connected_devices().iter().any(|device| {
            device.has_control
                && device.capabilities.iter().any(|capability| capability == "org.anchor.sms")
        });
        if !sms_available {
            return Err("Connect an SMS-capable phone before sending a message.".into());
        }
        let addresses: Vec<_> = self
            .sms_threads
            .lock()
            .unwrap()
            .iter()
            .find(|thread| thread.thread_id == thread_id)
            .map(|thread| {
                thread
                    .addresses
                    .iter()
                    .map(|address| serde_json::json!({ "address": address.address }))
                    .collect()
            })
            .unwrap_or_default();
        if addresses.is_empty() {
            return Err("This conversation has no reachable address.".into());
        }
        self.send_service_json(
            "smsplugin",
            serde_json::json!({
                "type": "sms.send",
                "addresses": addresses,
                "message_body": body,
                "thread_id": thread_id,
                "attachments": [],
            }),
        )?;
        self.sms_messages.lock().unwrap().entry(thread_id).or_default().push(SmsMessageSummary {
            uid: self.next_pending_sms_uid.fetch_add(1, Ordering::Relaxed),
            body,
            date: now_ms() as i64,
            sent: true,
        });
        Ok(())
    }

    pub fn clear_sms_cache(&self) -> Result<(), String> {
        self.sms_threads.lock().unwrap().clear();
        self.sms_messages.lock().unwrap().clear();
        *self.selected_sms_thread.lock().unwrap() = None;
        self.send_service_json("smsplugin", serde_json::json!({ "type": "anchor.sms.clear_local" }))
    }

    pub fn clear_clipboard(&self) {
        self.clipboard_history.lock().unwrap().clear();
    }

    pub fn send_clipboard(&self, hash: &str) -> Result<(), String> {
        let entry = self
            .clipboard_history
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.hash == hash)
            .cloned()
            .ok_or_else(|| "Clipboard entry no longer exists.".to_string())?;
        let mime_type = match entry.content_type {
            ClipboardContentType::Text => "text/plain",
            ClipboardContentType::ImagePng => "image/png",
        };
        self.send_json(
            AnchorTarget::Device,
            serde_json::json!({
                "plugin_id": "clipboard",
                "type": "anchor.clipboard.send",
                "content": entry.content,
                "mime_type": mime_type,
            }),
        )
    }

    pub fn copy_clipboard_local(&self, hash: &str) -> Result<(), String> {
        let exists =
            self.clipboard_history.lock().unwrap().iter().any(|entry| {
                entry.hash == hash && entry.content_type == ClipboardContentType::Text
            });
        if !exists {
            return Err("Text clipboard entry no longer exists.".into());
        }

        // Use the clipboard plugin's persistent writer so the selected backend
        // keeps ownership of the local clipboard after this command returns.
        self.send_json(
            AnchorTarget::Service("clipboard".into()),
            serde_json::json!({
                "type": "anchor.clipboard.copy_local",
                "hash": hash,
            }),
        )
    }

    pub fn clear_transfers(&self) {
        self.filetransfer_history.lock().unwrap().clear();
    }

    pub fn shared_folder_path(&self) -> PathBuf {
        self.filetransfer_folder.lock().unwrap().clone()
    }

    pub fn open_shared_folder(&self) -> Result<(), String> {
        self.open_path(&self.filetransfer_folder.lock().unwrap().to_string_lossy())
    }

    pub fn choose_shared_folder(&self) -> Result<(), String> {
        let current = self.filetransfer_folder.lock().unwrap().clone();
        let Some(path) = rfd::FileDialog::new().set_directory(current).pick_folder() else {
            return Ok(());
        };
        let folder = path.to_string_lossy().into_owned();
        *self.filetransfer_folder.lock().unwrap() = path;
        let mut updated = settings().clone();
        updated.file_transfer.folder = folder;
        save_settings(&updated).map_err(|error| format!("Could not save shared folder: {error}"))
    }

    pub fn open_transfer(&self, path: &str) -> Result<(), String> {
        self.open_path(path)
    }

    pub fn create_command(
        &self,
        name: String,
        command: String,
        description: String,
        detach: bool,
    ) -> Result<(), String> {
        if name.trim().is_empty() || command.trim().is_empty() {
            return Err("A command needs both a name and command line.".into());
        }
        let mut commands = self.commands.lock().unwrap();
        commands.push(SavedCommand::new(
            name.trim().into(),
            command.trim().into(),
            description.trim().into(),
            detach,
        ));
        save_commands(&commands)?;
        drop(commands);
        self.send_service_json(
            "commands",
            serde_json::json!({ "plugin_id": "commands", "type": "list" }),
        )
    }

    pub fn delete_command(&self, id: &str) -> Result<(), String> {
        let mut commands = self.commands.lock().unwrap();
        let old_len = commands.len();
        commands.retain(|command| command.id != id);
        if commands.len() == old_len {
            return Err("Command no longer exists.".into());
        }
        save_commands(&commands)?;
        drop(commands);
        self.send_service_json(
            "commands",
            serde_json::json!({ "plugin_id": "commands", "type": "list" }),
        )
    }

    pub fn update_command(
        &self,
        id: &str,
        name: String,
        command: String,
        description: String,
        detach: bool,
    ) -> Result<(), String> {
        if name.trim().is_empty() || command.trim().is_empty() {
            return Err("A command needs both a name and command line.".into());
        }
        let mut commands = self.commands.lock().unwrap();
        let saved = commands
            .iter_mut()
            .find(|saved| saved.id == id)
            .ok_or_else(|| "Command no longer exists.".to_string())?;
        saved.name = name.trim().into();
        saved.command = command.trim().into();
        saved.description = description.trim().into();
        saved.detach = detach;
        save_commands(&commands)?;
        drop(commands);
        self.send_service_json(
            "commands",
            serde_json::json!({ "plugin_id": "commands", "type": "list" }),
        )
    }

    pub fn run_command(&self, id: &str) -> Result<(), String> {
        self.send_service_json(
            "commands",
            serde_json::json!({ "plugin_id": "commands", "type": "run_command", "id": id }),
        )
    }

    pub fn media_command(&self, source: &str, command: &str) -> Result<(), String> {
        let command = match command {
            "play" => MediaCommandKind::Play,
            "pause" => MediaCommandKind::Pause,
            "playpause" => MediaCommandKind::Playpause,
            "next" => MediaCommandKind::Next,
            "previous" => MediaCommandKind::Previous,
            "stop" => MediaCommandKind::Stop,
            _ => return Err("Unsupported media command.".into()),
        };
        let (target, state) = match source {
            "phone" => (AnchorTarget::Service("media".into()), &self.remote_media_state),
            "desktop" => (AnchorTarget::Service("media".into()), &self.desktop_media_state),
            _ => return Err("Unsupported media source.".into()),
        };
        let session_id = state.lock().unwrap().as_ref().map(|media| media.session_id.clone());
        let message = MediaMessage::MediaCommand(MediaCommand {
            command,
            target_session: session_id.filter(|id| !id.is_empty()),
            position_ms: None,
        });
        send_media_message(&self.broker_tx, target, &message);
        Ok(())
    }

    pub fn clear_logs(&self) {
        self.log_buffer.lock().unwrap().clear();
    }

    pub fn setup_camera(&self, retry: bool) -> Result<(), String> {
        let result = if retry {
            self.camera_setup.retry_camera()
        } else {
            self.camera_setup.create_camera()
        };
        result.map(|_| ()).map_err(|error| error.to_string())
    }

    pub fn set_camera_device(&self, device_id: Option<String>) {
        self.camera_setup.set_selected_device(device_id);
    }

    pub fn set_camera_auto_load(&self, enabled: bool) -> Result<(), String> {
        if enabled && !matches!(self.camera_setup.current_status(), CameraGuiStatus::Ready { .. }) {
            self.camera_setup.create_camera().map_err(|error| error.to_string())?;
        }
        save_camera_auto_load(enabled)?;
        self.camera_preferences.lock().unwrap().auto_load = enabled;
        Ok(())
    }

    pub fn set_camera_stream_params(&self, fps: u32, bitrate_kbps: u32) -> Result<(), String> {
        let (fps, bitrate_kbps) = save_camera_stream_params(fps, bitrate_kbps)?;
        {
            let mut preferences = self.camera_preferences.lock().unwrap();
            preferences.fps = fps;
            preferences.bitrate_kbps = bitrate_kbps;
        }
        self.send_json(
            AnchorTarget::Device,
            serde_json::json!({
                "plugin_id": "cameraplugin",
                "command": "set_stream_params",
                "fps": fps,
                "bitrate_kbps": bitrate_kbps,
            }),
        )
    }

    pub fn set_camera_zoom(&self, value: f32) -> Result<(), String> {
        let value = value.clamp(1.0, 10.0);
        self.send_camera_control("set_zoom_ratio", serde_json::json!(value))?;
        self.camera_preferences.lock().unwrap().zoom_ratio = value;
        Ok(())
    }

    pub fn set_camera_exposure(&self, value: i32) -> Result<(), String> {
        let value = value.clamp(-6, 6);
        self.send_camera_control("set_exposure", serde_json::json!(value))?;
        self.camera_preferences.lock().unwrap().exposure = value;
        Ok(())
    }

    pub fn set_camera_torch(&self, enabled: bool) -> Result<(), String> {
        self.send_json(
            AnchorTarget::Device,
            serde_json::json!({
                "plugin_id": "cameraplugin",
                "command": "set_torch",
                "enabled": enabled,
            }),
        )?;
        self.camera_preferences.lock().unwrap().torch_on = enabled;
        Ok(())
    }

    pub fn switch_camera(&self) -> Result<(), String> {
        self.send_json(
            AnchorTarget::Device,
            serde_json::json!({ "plugin_id": "cameraplugin", "command": "switch_camera" }),
        )
    }

    fn send_camera_control(&self, command: &str, value: serde_json::Value) -> Result<(), String> {
        self.send_json(
            AnchorTarget::Device,
            serde_json::json!({
                "plugin_id": "cameraplugin",
                "command": command,
                "value": value,
            }),
        )
    }

    fn send_wayland(&self, message: WaylandPluginMsg) -> Result<(), String> {
        self.broker_tx
            .send(AnchorEvent {
                target: AnchorTarget::Wayland,
                message: AnchorMessage::Wayland(message),
            })
            .map_err(|_| "The Anchor capture service is no longer running.".to_string())
    }

    fn send_service_json(&self, service: &str, message: serde_json::Value) -> Result<(), String> {
        self.send_json(AnchorTarget::Service(service.into()), message)
    }

    fn send_json(&self, target: AnchorTarget, message: serde_json::Value) -> Result<(), String> {
        self.broker_tx
            .send(AnchorEvent { target, message: AnchorMessage::Json(message.to_string()) })
            .map_err(|_| "The Anchor service is no longer running.".to_string())
    }

    fn open_path(&self, path: &str) -> Result<(), String> {
        ProcessCommand::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("Could not open {path}: {error}"))
    }

    fn sms_thread_summaries(&self) -> Vec<SmsThreadSummary> {
        let mut threads: Vec<SmsThread> = self.sms_threads.lock().unwrap().clone();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_updated));
        threads
            .into_iter()
            .map(|thread| SmsThreadSummary {
                thread_id: thread.thread_id,
                name: thread_name(&thread),
                snippet: thread.snippet.unwrap_or_default(),
                last_updated: thread.last_updated,
                has_photo: thread.addresses.iter().any(|address| {
                    address.photo.as_ref().is_some_and(|photo| !photo.trim().is_empty())
                }),
            })
            .collect()
    }

    fn media_summary(&self, state: &SharedMediaState) -> Option<MediaSummary> {
        state.lock().unwrap().as_ref().map(|media| MediaSummary {
            session_id: media.session_id.clone(),
            state: format!("{:?}", media.state).to_lowercase(),
            title: media.title.clone(),
            artist: media.artist.clone(),
            album: media.album.clone(),
            app: media.app.clone(),
            art_url: media.art_url.clone(),
            position_ms: media.position_ms,
            duration_ms: media.duration_ms,
            can_play: media.can_play,
            can_pause: media.can_pause,
            can_next: media.can_next,
            can_prev: media.can_prev,
        })
    }

    fn camera_summary(&self) -> CameraSummary {
        let (status, device) = match self.camera_setup.current_status() {
            CameraGuiStatus::Ready { device } => {
                ("ready".into(), Some(device.to_string_lossy().into_owned()))
            }
            CameraGuiStatus::NotLoaded => ("not loaded".into(), None),
            CameraGuiStatus::ModuleLoaded => ("module loaded".into(), None),
            CameraGuiStatus::DeviceOpenFailed => ("device unavailable".into(), None),
        };
        let preferences = self.camera_preferences.lock().unwrap().clone();
        CameraSummary {
            status,
            device,
            selected_device_id: self.camera_setup.get_selected_device(),
            auto_load: preferences.auto_load,
            fps: preferences.fps,
            bitrate_kbps: preferences.bitrate_kbps,
            zoom_ratio: preferences.zoom_ratio,
            exposure: preferences.exposure,
            torch_on: preferences.torch_on,
        }
    }

    /// Make this controller's GUI receiver event-driven. The receiver has one
    /// owner for the lifetime of the frontend; it applies a small burst before
    /// asking the webview to fetch its active, section-scoped snapshot.
    pub fn spawn_event_notifier(
        self: std::sync::Arc<Self>,
        notify: impl Fn() + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("desktop-ui-events".into())
            .spawn(move || {
                loop {
                    let first = {
                        let receiver = self.gui_events.lock().unwrap();
                        match receiver.recv() {
                            Ok(event) => event,
                            Err(_) => return,
                        }
                    };
                    self.apply_event(first);

                    // Coalesce related updates (for example a database update and
                    // its conversation snapshot) into one webview invalidation.
                    let deadline = std::time::Instant::now() + Duration::from_millis(60);
                    loop {
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        let event = {
                            let receiver = self.gui_events.lock().unwrap();
                            receiver.recv_timeout(remaining)
                        };
                        match event {
                            Ok(event) => self.apply_event(event),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    notify();
                }
            })
            .expect("failed to start desktop UI event notifier")
    }

    fn apply_event(&self, event: AnchorEvent) {
        match event.message {
            AnchorMessage::Wayland(WaylandPluginMsg::Ready) => {
                let mut capture = self.capture.lock().unwrap();
                if capture.status == "initializing" {
                    capture.status = "idle".into();
                }
            }
            AnchorMessage::Wayland(WaylandPluginMsg::OutputList(outputs)) => {
                let mut capture = self.capture.lock().unwrap();
                capture.outputs = outputs
                    .into_iter()
                    .enumerate()
                    .map(|(index, output)| OutputSummary {
                        index,
                        name: output.name,
                        description: output.description,
                        width: output.width,
                        height: output.height,
                        refresh_rate_mhz: output.refresh_rate_mhz,
                    })
                    .collect();
                if !capture.outputs.is_empty() {
                    capture.selected_output_index =
                        capture.selected_output_index.min(capture.outputs.len() - 1);
                }
                log::debug!(
                    "UI received capture outputs: selected={} outputs=[{}]",
                    capture.selected_output_index,
                    capture
                        .outputs
                        .iter()
                        .map(|output| format!("{}:{}", output.index, output.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            AnchorMessage::Wayland(WaylandPluginMsg::Unavailable(reason)) => {
                let mut capture = self.capture.lock().unwrap();
                capture.status = "unavailable".into();
                capture.unavailable_reason = Some(reason);
                capture.outputs.clear();
            }
            AnchorMessage::Json(json) => self.apply_json_event(&json),
            _ => {}
        }
    }

    fn apply_json_event(&self, json: &str) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(json) else { return };
        if message.get("type").and_then(|value| value.as_str()) == Some("input.ready") {
            let mut capabilities = self.input_capabilities.lock().unwrap();
            capabilities.pointer =
                Some(message.get("pointer").and_then(|value| value.as_bool()).unwrap_or(false));
            capabilities.keyboard =
                Some(message.get("keyboard").and_then(|value| value.as_bool()).unwrap_or(false));
            return;
        }
        if message.get("type").and_then(|value| value.as_str()) == Some("input.unavailable") {
            let mut capabilities = self.input_capabilities.lock().unwrap();
            capabilities.pointer = Some(false);
            capabilities.keyboard = Some(false);
            return;
        }
        if message.get("type").and_then(|value| value.as_str()) == Some("battery_update") {
            self.apply_battery_update(&message);
            return;
        }
        if message.get("type").and_then(|value| value.as_str()) == Some("device_disconnected")
            && let Some(device_id) = message.get("device_id").and_then(|value| value.as_str())
        {
            self.mobile_batteries.lock().unwrap().remove(device_id);
        }
        if message.get("type").and_then(|value| value.as_str()) == Some("sms.conversation_messages")
        {
            self.apply_sms_messages(&message);
            return;
        }
        if message.get("type").and_then(|value| value.as_str()) == Some("command_result") {
            self.apply_command_result(&message);
            return;
        }
        Self::apply_capture_json(&mut self.capture.lock().unwrap(), &message);
    }

    fn apply_battery_update(&self, message: &serde_json::Value) {
        let (Some(device_id), Some(level), Some(charging)) = (
            message.get("device_id").and_then(|value| value.as_str()),
            message.get("level").and_then(|value| value.as_u64()),
            message.get("charging").and_then(|value| value.as_bool()),
        ) else {
            return;
        };
        let battery =
            crate::anchorapp::battery::MobileBattery { level: level.min(100) as u8, charging };
        let connected = self.device_registry.connected_devices();
        let mut batteries = self.mobile_batteries.lock().unwrap();
        batteries.insert(device_id.to_string(), battery.clone());

        // Older phone builds can report an application-scoped ID instead of
        // the paired TLS identity. Associate that update only when there is a
        // single unambiguous connected device.
        if !connected.iter().any(|device| device.id == device_id)
            && let [device] = connected.as_slice()
        {
            batteries.insert(device.id.clone(), battery);
        }
    }

    fn apply_command_result(&self, message: &serde_json::Value) {
        let Some(id) = message.get("id").and_then(|value| value.as_str()) else { return };
        let Some(status) = message.get("status").and_then(|value| value.as_str()) else { return };
        let detail =
            message.get("error").and_then(|value| value.as_str()).map(str::to_string).or_else(
                || {
                    message
                        .get("exit_code")
                        .and_then(|value| value.as_i64())
                        .map(|code| format!("Exit code {code}"))
                },
            );
        self.command_runs
            .lock()
            .unwrap()
            .insert(id.to_string(), CommandRunState { status: status.to_string(), detail });
    }

    fn apply_sms_messages(&self, message: &serde_json::Value) {
        let Some(thread_id) = message.get("thread_id").and_then(|value| value.as_i64()) else {
            return;
        };
        let Some(messages) = message.get("messages").and_then(|value| value.as_array()) else {
            return;
        };
        let mut messages: Vec<SmsMessageSummary> = messages
            .iter()
            .filter_map(|value| serde_json::from_value::<ConversationMessage>(value.clone()).ok())
            .map(|message| SmsMessageSummary {
                uid: message.uid,
                body: message.body.unwrap_or_default(),
                date: message.date,
                sent: message.message_type == 2,
            })
            .collect();
        messages.sort_by_key(|message| (message.date, message.uid));

        // `sms.conversation_messages` is a full, asynchronous database
        // snapshot. It may have been requested before an outgoing message was
        // sent, so replacing the local list would make a just-sent message
        // disappear until the next snapshot. Preserve unmatched optimistic
        // messages and discard each one only when the phone echoes its real
        // sent record back.
        let mut displayed = self.sms_messages.lock().unwrap();
        let pending = displayed
            .get(&thread_id)
            .into_iter()
            .flatten()
            .filter(|message| message.uid < 0 && message.sent)
            .cloned()
            .collect::<Vec<_>>();
        displayed.insert(thread_id, merge_sms_snapshot(messages, pending));
    }

    fn apply_capture_json(capture: &mut CapturePanelState, message: &serde_json::Value) {
        if let Some(status) = message.get("state").and_then(|value| value.as_str()) {
            capture.status = status.into();
            capture.unavailable_reason = None;
        }
        if let Some(index) = message.get("selected_index").and_then(|value| value.as_u64()) {
            capture.selected_output_index = index as usize;
        }
        let fps = message.get("fps").and_then(|value| value.as_u64()).unwrap_or(0);
        let frame_size =
            message.get("frame_size_bytes").and_then(|value| value.as_u64()).unwrap_or(0);
        let avg_frame_ms =
            message.get("avg_frame_ms").and_then(|value| value.as_u64()).unwrap_or(0);
        if fps > 0 || frame_size > 0 {
            capture.metrics = Some(CaptureMetrics {
                fps: fps as u32,
                avg_frame_ms: avg_frame_ms as u32,
                frame_size_bytes: frame_size as u32,
            });
        }
    }
}

fn merge_sms_snapshot(
    mut messages: Vec<SmsMessageSummary>,
    pending: Vec<SmsMessageSummary>,
) -> Vec<SmsMessageSummary> {
    // Only real messages from the phone can satisfy an optimistic echo. Keep
    // the marker slice fixed to that snapshot so pending messages appended
    // below can never be indexed through it.
    let mut echoed = vec![false; messages.len()];
    for pending_message in pending {
        let echoed_index = messages.iter().zip(echoed.iter()).enumerate().find_map(
            |(index, (message, was_echoed))| {
                (!*was_echoed
                    && message.sent
                    && message.body == pending_message.body
                    && (message.date - pending_message.date).abs() <= 120_000)
                    .then_some(index)
            },
        );
        if let Some(index) = echoed_index {
            echoed[index] = true;
        } else {
            messages.push(pending_message);
        }
    }
    messages.sort_by_key(|message| (message.date, message.uid));
    messages
}

pub fn encode_screen_preview(frame: SharedFrameBuffer) -> Result<ScreenPreview, String> {
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixel_data)
        .ok_or_else(|| "Preview frame has invalid dimensions.".to_string())?;
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 72)
        .encode_image(&image)
        .map_err(|error| format!("Could not encode screen preview: {error}"))?;
    Ok(ScreenPreview {
        sequence: frame.sequence,
        data_url: format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(jpeg)
        ),
    })
}

/// Copy files received from a native window drop into the watched shared
/// folder. The watcher ignores the hidden temporary file and only sees the
/// complete file after the final atomic rename.
pub fn import_dropped_files(folder: PathBuf, paths: Vec<String>) -> DroppedFileImport {
    let mut result =
        DroppedFileImport { imported: 0, rejected: 0, names: Vec::new(), errors: Vec::new() };

    if let Err(error) = fs::create_dir_all(&folder) {
        result.rejected = paths.len();
        result.errors.push(format!("Could not create the shared folder: {error}"));
        return result;
    }

    let canonical_folder = folder.canonicalize().ok();
    for (index, raw_path) in paths.into_iter().enumerate() {
        let source = PathBuf::from(&raw_path);
        let display_name =
            source.file_name().and_then(|name| name.to_str()).unwrap_or(&raw_path).to_string();

        let import = (|| -> Result<String, String> {
            let metadata = fs::symlink_metadata(&source)
                .map_err(|error| format!("{display_name}: cannot read file ({error})"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!("{display_name}: only regular files can be sent"));
            }
            if metadata.len() == 0 {
                return Err(format!("{display_name}: empty files are not supported"));
            }
            if canonical_folder.as_ref().is_some_and(|shared| {
                source
                    .canonicalize()
                    .ok()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
                    .as_ref()
                    == Some(shared)
            }) {
                return Err(format!("{display_name}: already in the shared folder"));
            }

            let temporary = create_drop_temporary(&folder, index)?;
            let copy_result =
                (|| -> Result<(), String> {
                    let mut input = File::open(&source)
                        .map_err(|error| format!("{display_name}: cannot open file ({error})"))?;
                    let mut output =
                        OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(
                            |error| format!("{display_name}: cannot stage file ({error})"),
                        )?;
                    let mut buffer = vec![0_u8; 128 * 1024];
                    loop {
                        let read = input
                            .read(&mut buffer)
                            .map_err(|error| format!("{display_name}: copy failed ({error})"))?;
                        if read == 0 {
                            break;
                        }
                        output
                            .write_all(&buffer[..read])
                            .map_err(|error| format!("{display_name}: copy failed ({error})"))?;
                    }
                    output
                        .sync_all()
                        .map_err(|error| format!("{display_name}: could not finish copy ({error})"))
                })();

            if let Err(error) = copy_result {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }

            let destination = unique_destination(&folder, &display_name);
            if let Err(error) = fs::rename(&temporary, &destination) {
                let _ = fs::remove_file(&temporary);
                return Err(format!("{display_name}: could not queue file ({error})"));
            }
            Ok(destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&display_name)
                .to_string())
        })();

        match import {
            Ok(name) => {
                result.imported += 1;
                result.names.push(name);
            }
            Err(error) => {
                result.rejected += 1;
                result.errors.push(error);
            }
        }
    }
    result
}

fn create_drop_temporary(folder: &Path, index: usize) -> Result<PathBuf, String> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    for attempt in 0..100_u8 {
        let path =
            folder.join(format!(".anchor-drop-{}-{stamp}-{index}-{attempt}", std::process::id()));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("Could not allocate a temporary file in the shared folder".into())
}

fn unique_destination(folder: &Path, file_name: &str) -> PathBuf {
    let direct = folder.join(file_name);
    if !direct.exists() {
        return direct;
    }

    let path = Path::new(file_name);
    let stem = path.file_stem().and_then(|value| value.to_str()).unwrap_or("file");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let candidate = match extension {
            Some(extension) => folder.join(format!("{stem} ({index}).{extension}")),
            None => folder.join(format!("{stem} ({index})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    folder.join(format!("{stem}-{}", now_ms()))
}

fn certificate_fingerprint(certificate_pem: &str) -> String {
    rustls_pemfile::certs(&mut certificate_pem.as_bytes())
        .next()
        .and_then(Result::ok)
        .map(|certificate| crate::anchorapp::tls::TrustedStore::fingerprint(certificate.as_ref()))
        .unwrap_or_else(|| "Unavailable".into())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn thread_name(thread: &SmsThread) -> String {
    let names: Vec<&str> = thread
        .addresses
        .iter()
        .map(|address| address.name.as_deref().unwrap_or(&address.address))
        .collect();
    if names.is_empty() { "Unknown sender".into() } else { names.join(", ") }
}

fn clipboard_preview(content: &str, content_type: &ClipboardContentType) -> String {
    match content_type {
        ClipboardContentType::ImagePng => format!("PNG image · {} bytes", content.len()),
        ClipboardContentType::Text => {
            let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut preview = compact.chars().take(180).collect::<String>();
            if compact.chars().count() > 180 {
                preview.push('…');
            }
            preview
        }
    }
}

/// Initialize Anchor's desktop services and expose their UI channel to the
/// Tauri controller.
pub fn start_desktop_controller() -> DesktopController {
    let log_buffer = crate::anchorapp::log_collector::init_logger(log::LevelFilter::Info);
    crate::anchorapp::log_collector::start_adb_logcat_collector(log_buffer.clone());
    fd_probe::start();

    let config_dir = dirs::config_dir().expect("Could not find config directory").join("anchor");
    init_config(&config_dir);
    init_settings();
    Compositor::init();

    let identity = load_or_create_identity();
    let mut app = AnchorAppInstance::new_with_logs(log_buffer, identity);
    let runtime = app.run_for_desktop_frontend();
    DesktopController::from_runtime(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_state_has_an_explicit_startup_status() {
        let state = CapturePanelState {
            status: "initializing".into(),
            unavailable_reason: None,
            outputs: vec![],
            selected_output_index: 0,
            metrics: None,
        };
        assert_eq!(state.status, "initializing");
    }

    #[test]
    fn screen_preview_encodes_as_a_jpeg_data_url() {
        let preview = encode_screen_preview(SharedFrameBuffer {
            sequence: 7,
            width: 2,
            height: 1,
            stride: 8,
            pixel_data: vec![255, 0, 0, 255, 0, 0, 255, 255],
        })
        .expect("valid RGBA preview should encode");

        assert_eq!(preview.sequence, 7);
        assert!(preview.data_url.starts_with("data:image/jpeg;base64,/9j/"));
    }

    #[test]
    fn clipboard_preview_keeps_text_compact() {
        assert_eq!(clipboard_preview("a  b\n c", &ClipboardContentType::Text), "a b c");
    }

    #[test]
    fn conversation_message_accepts_plugin_direction_field() {
        let message: ConversationMessage = serde_json::from_value(serde_json::json!({
            "uid": 1,
            "body": "sent",
            "date": 1_000,
            "message_type": 2,
        }))
        .unwrap();
        assert_eq!(message.message_type, 2);
    }

    #[test]
    fn sms_snapshot_preserves_multiple_unmatched_optimistic_messages() {
        let remote = (0..85)
            .map(|uid| SmsMessageSummary {
                uid,
                body: format!("remote {uid}"),
                date: uid * 1_000,
                sent: uid % 2 == 0,
            })
            .collect();
        let pending = vec![
            SmsMessageSummary { uid: -1, body: "first pending".into(), date: 100_000, sent: true },
            SmsMessageSummary { uid: -2, body: "second pending".into(), date: 101_000, sent: true },
        ];

        let merged = merge_sms_snapshot(remote, pending);

        assert_eq!(merged.len(), 87);
        assert!(merged.iter().any(|message| message.uid == -1));
        assert!(merged.iter().any(|message| message.uid == -2));
    }

    #[test]
    fn sms_snapshot_replaces_each_matching_optimistic_echo_once() {
        let remote =
            vec![SmsMessageSummary { uid: 41, body: "hello".into(), date: 10_000, sent: true }];
        let pending = vec![
            SmsMessageSummary { uid: -1, body: "hello".into(), date: 9_900, sent: true },
            SmsMessageSummary { uid: -2, body: "hello".into(), date: 9_950, sent: true },
        ];

        let merged = merge_sms_snapshot(remote, pending);

        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|message| message.uid == 41));
        assert_eq!(merged.iter().filter(|message| message.uid < 0).count(), 1);
    }

    #[test]
    fn dropped_files_are_staged_and_never_overwrite() {
        let root = std::env::temp_dir().join(format!("anchor-drop-test-{}", now_ms()));
        let source_folder = root.join("source");
        let shared_folder = root.join("shared");
        fs::create_dir_all(&source_folder).unwrap();
        fs::create_dir_all(&shared_folder).unwrap();
        let source = source_folder.join("report.txt");
        fs::write(&source, "new").unwrap();
        fs::write(shared_folder.join("report.txt"), "old").unwrap();

        let result = import_dropped_files(
            shared_folder.clone(),
            vec![source.to_string_lossy().into_owned()],
        );

        assert_eq!(result.imported, 1);
        assert_eq!(result.rejected, 0);
        assert_eq!(fs::read_to_string(shared_folder.join("report.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(shared_folder.join("report (1).txt")).unwrap(), "new");
        assert!(
            fs::read_dir(&shared_folder)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".anchor-drop-"))
        );
        let _ = fs::remove_dir_all(root);
    }
}
