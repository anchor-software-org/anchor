use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::anchorapp::{
    event::{AnchorEvent, AnchorMessage, AnchorTarget, SharedPairingRequest},
    tls::{AnchorIdentity, TrustedStore},
};

use super::{
    DeviceCheckResult, DeviceRegistry, FrameBroadcaster,
    adb::start_adb_scanner,
    camera_receiver::{CameraTxCell, new_camera_tx_cell, spawn_camera_receiver},
    loopback_setup::{
        LoopbackError, LoopbackStatus, anchor_device_path, ensure_loopback_nopass,
        ensure_loopback_pkexec, loopback_status,
    },
    sdk_server::SdkServer,
};

pub struct DeviceManager {
    pub registry: DeviceRegistry,
    pub broker_tx: Sender<AnchorEvent>,
    pub plugin_rx: Option<Receiver<AnchorEvent>>,
    pub identity: AnchorIdentity,
    pub broadcaster: Arc<FrameBroadcaster>,
    pub pairing_request: SharedPairingRequest,
    /// Late-bindable camera frame sender; populated by the Settings UI when
    /// the user runs the v4l2loopback setup, or at boot if `camera.auto_load`.
    pub camera_tx_cell: CameraTxCell,
    pub sdk_clipboard: crate::anchorapp::clipboard_plugin::SdkClipboardBinding,
    pub sdk_media: crate::anchorapp::sdk_media::SdkMediaBinding,
    pub sdk_notifications: crate::anchorapp::sdk_notifications::SdkNotificationsBinding,
    pub sdk_screen: crate::anchorapp::sdk_screen::SdkScreenBinding,
    pub sdk_files: crate::anchorapp::sdk_files::SdkFilesBinding,
    pub sdk_sms: crate::anchorapp::sdk_sms::SdkSmsBinding,
    pub sdk_commands: crate::anchorapp::sdk_commands::SdkCommandsBinding,
    pub sdk_camera: crate::anchorapp::sdk_camera::SdkCameraBinding,
    pub sdk_device: crate::anchorapp::sdk_device::SdkDeviceBinding,
}

/// Bag of the bits the GUI needs to display loopback status and trigger setup.
#[derive(Clone)]
pub struct CameraSetupHandle {
    cell: CameraTxCell,
}

#[derive(Debug, Clone)]
pub enum CameraGuiStatus {
    Ready {
        device: PathBuf,
    },
    NotLoaded,
    ModuleLoaded,
    /// Module is loaded and device exists but `spawn_camera_receiver` failed earlier.
    DeviceOpenFailed,
}

impl CameraSetupHandle {
    pub fn current_status(&self) -> CameraGuiStatus {
        match loopback_status() {
            LoopbackStatus::Ready { device } => {
                if self.cell.lock().unwrap().tx.is_some() {
                    CameraGuiStatus::Ready { device }
                } else {
                    CameraGuiStatus::DeviceOpenFailed
                }
            }
            LoopbackStatus::ModuleLoaded => CameraGuiStatus::ModuleLoaded,
            LoopbackStatus::NotLoaded => CameraGuiStatus::NotLoaded,
        }
    }

    /// Load the module via pkexec and start the camera receiver.
    /// Idempotent — if the receiver is already running, returns the current device path.
    pub fn create_camera(&self) -> Result<PathBuf, LoopbackError> {
        // If the receiver thread is running (module loaded but device had wrong caps),
        // drop the sender so the thread exits and releases its O_WRONLY fd before
        // pkexec modprobe runs. Without this, a lingering fd can prevent the module
        // from being reloaded with the correct parameters.
        if matches!(loopback_status(), LoopbackStatus::ModuleLoaded) {
            self.cell.lock().unwrap().tx = None;
            thread::sleep(Duration::from_millis(250));
        } else if self.cell.lock().unwrap().tx.is_some() {
            return Ok(anchor_device_path());
        }

        let device = ensure_loopback_pkexec()?;

        match spawn_camera_receiver(&device.to_string_lossy()) {
            Some(tx) => {
                self.cell.lock().unwrap().tx = Some(tx);
                log::info!("[camera] receiver started on {}", device.display());
                Ok(device)
            }
            None => Err(LoopbackError::StillNotPresent),
        }
    }

    /// Retry opening the V4L2 device without needing a password.
    /// Used when the module is loaded but `spawn_camera_receiver` previously failed.
    pub fn retry_camera(&self) -> Result<PathBuf, LoopbackError> {
        let device = match loopback_status() {
            LoopbackStatus::Ready { device } => device,
            _ => return Err(LoopbackError::StillNotPresent),
        };
        if self.cell.lock().unwrap().tx.is_some() {
            return Ok(device);
        }
        match spawn_camera_receiver(&device.to_string_lossy()) {
            Some(tx) => {
                self.cell.lock().unwrap().tx = Some(tx);
                log::info!("[camera] receiver restarted on {}", device.display());
                Ok(device)
            }
            None => Err(LoopbackError::StillNotPresent),
        }
    }

    /// Set which device is allowed to stream. Takes effect on the device's next connection.
    pub fn set_selected_device(&self, device_id: Option<String>) {
        self.cell.lock().unwrap().selected_device_id = device_id;
    }

    pub fn get_selected_device(&self) -> Option<String> {
        self.cell.lock().unwrap().selected_device_id.clone()
    }
}

impl DeviceManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config_dir: PathBuf,
        broker_tx: Sender<AnchorEvent>,
        plugin_rx: Receiver<AnchorEvent>,
        identity: AnchorIdentity,
        sdk_clipboard: crate::anchorapp::clipboard_plugin::SdkClipboardBinding,
        sdk_media: crate::anchorapp::sdk_media::SdkMediaBinding,
        sdk_notifications: crate::anchorapp::sdk_notifications::SdkNotificationsBinding,
        sdk_screen: crate::anchorapp::sdk_screen::SdkScreenBinding,
        sdk_files: crate::anchorapp::sdk_files::SdkFilesBinding,
        sdk_sms: crate::anchorapp::sdk_sms::SdkSmsBinding,
        sdk_commands: crate::anchorapp::sdk_commands::SdkCommandsBinding,
        sdk_camera: crate::anchorapp::sdk_camera::SdkCameraBinding,
        sdk_device: crate::anchorapp::sdk_device::SdkDeviceBinding,
    ) -> Self {
        let trusted_store = TrustedStore::load(&config_dir);
        let registry = DeviceRegistry::new(trusted_store);

        DeviceManager {
            registry,
            broker_tx,
            plugin_rx: Some(plugin_rx),
            identity,
            broadcaster: Arc::new(FrameBroadcaster::new()),
            pairing_request: Arc::new(Mutex::new(None)),
            camera_tx_cell: new_camera_tx_cell(),
            sdk_clipboard,
            sdk_media,
            sdk_notifications,
            sdk_screen,
            sdk_files,
            sdk_sms,
            sdk_commands,
            sdk_camera,
            sdk_device,
        }
    }

    /// Handle the GUI/settings UI uses to query camera status and trigger
    /// `pkexec modprobe v4l2loopback …` from a button.
    pub fn camera_setup_handle(&self) -> CameraSetupHandle {
        CameraSetupHandle { cell: self.camera_tx_cell.clone() }
    }

    pub fn check_device(&self, cert_der: &[u8]) -> DeviceCheckResult {
        self.registry.check_device(cert_der)
    }

    pub fn run(&mut self) -> Option<JoinHandle<()>> {
        log::debug!("Starting DeviceManager");

        let broker_tx = self.broker_tx.clone();
        let plugin_rx = self.plugin_rx.take().expect("plugin_rx not set");
        let registry = self.registry.clone();
        let broadcaster = self.broadcaster.clone();
        let identity_device_id = self.identity.device_id.clone();
        let identity_certificate_pem = self.identity.certificate_pem.clone();
        let camera_tx_cell = self.camera_tx_cell.clone();
        let sdk_clipboard = self.sdk_clipboard.clone();
        let sdk_media = self.sdk_media.clone();
        let sdk_notifications = self.sdk_notifications.clone();
        let sdk_screen = self.sdk_screen.clone();
        let sdk_files = self.sdk_files.clone();
        let sdk_sms = self.sdk_sms.clone();
        let sdk_commands = self.sdk_commands.clone();
        let sdk_camera = self.sdk_camera.clone();
        let sdk_device = self.sdk_device.clone();
        let pairing_request = self.pairing_request.clone();
        let sdk_identity = crate::anchorapp::tls::AnchorIdentity {
            certificate_pem: self.identity.certificate_pem.clone(),
            private_key_pem: self.identity.private_key_pem.clone(),
            device_id: self.identity.device_id.clone(),
        };

        start_adb_scanner(broker_tx.clone());
        sdk_screen.set_broadcaster(broadcaster.clone());
        sdk_camera.set_camera_tx_cell(camera_tx_cell.clone());
        spawn_forwarding_thread(
            plugin_rx,
            sdk_screen.clone(),
            sdk_sms.clone(),
            sdk_commands.clone(),
            sdk_camera.clone(),
            sdk_device.clone(),
        );

        let handle = thread::spawn(move || {
            // Advertise over mDNS so LAN clients can discover us without an IP.
            // Kept alive for the thread's lifetime; dropping it unregisters.
            let _mdns_daemon =
                super::mdns::start_advertising(&identity_device_id, &identity_certificate_pem);

            // Answers wired-subnet probes mDNS cannot reach.
            // Kept alive for the thread's lifetime.
            let _probe_socket =
                super::probe::start_responder(identity_device_id, identity_certificate_pem);

            SdkServer::new(
                sdk_identity,
                registry.clone(),
                sdk_clipboard,
                sdk_media,
                sdk_notifications,
                sdk_files,
                sdk_screen,
                sdk_sms,
                sdk_commands,
                sdk_camera,
                sdk_device,
                broker_tx.clone(),
                pairing_request,
            )
            .spawn();

            // Camera loopback setup. The pinned device is /dev/video42; the
            // module load is privileged and gated by `camera.auto_load`.
            let auto_load = crate::anchorapp::settings::settings().camera.auto_load;
            initialize_camera(&camera_tx_cell, auto_load);

            loop {
                thread::sleep(Duration::from_secs(60));
            }
        });

        Some(handle)
    }
}

/// Boot-time loopback init. With auto_load=false, only checks current status.
/// With auto_load=true, tries passwordless sudo first (silent on machines with a
/// NOPASSWD rule) and falls back to pkexec so polkit prompts once at startup
/// rather than failing silently.
fn initialize_camera(cell: &CameraTxCell, auto_load: bool) {
    let device_path = if auto_load {
        match ensure_loopback_nopass() {
            Ok(p) => p,
            Err(LoopbackError::NeedPassword) => {
                log::info!("[camera] auto-load: no NOPASSWD sudo, falling back to pkexec");
                match ensure_loopback_pkexec() {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("[camera] auto-load pkexec failed: {}", e);
                        return;
                    }
                }
            }
            Err(e) => {
                log::warn!("[camera] auto-load failed: {}", e);
                return;
            }
        }
    } else {
        match loopback_status() {
            LoopbackStatus::Ready { device } => device,
            LoopbackStatus::ModuleLoaded => {
                log::info!(
                    "[camera] v4l2loopback loaded but /dev/video42 missing; user must press 'Create camera'"
                );
                return;
            }
            LoopbackStatus::NotLoaded => {
                log::info!(
                    "[camera] v4l2loopback not loaded; user must press 'Create camera' in Settings"
                );
                return;
            }
        }
    };

    match spawn_camera_receiver(&device_path.to_string_lossy()) {
        Some(tx) => {
            cell.lock().unwrap().tx = Some(tx);
            log::info!("[camera] receiver started on {}", device_path.display());
        }
        None => {
            log::warn!("[camera] failed to open {}", device_path.display());
        }
    }
}

fn spawn_forwarding_thread(
    plugin_rx: Receiver<AnchorEvent>,
    sdk_screen: crate::anchorapp::sdk_screen::SdkScreenBinding,
    sdk_sms: crate::anchorapp::sdk_sms::SdkSmsBinding,
    sdk_commands: crate::anchorapp::sdk_commands::SdkCommandsBinding,
    sdk_camera: crate::anchorapp::sdk_camera::SdkCameraBinding,
    sdk_device: crate::anchorapp::sdk_device::SdkDeviceBinding,
) {
    thread::spawn(move || {
        for event in plugin_rx.iter() {
            let bytes = match &event.message {
                AnchorMessage::Json(s) => s.as_bytes().to_vec(),
                AnchorMessage::Generic(s) => s.as_bytes().to_vec(),
                _ => continue,
            };
            match &event.target {
                AnchorTarget::Device => {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && crate::anchorapp::sdk_device::send_json(&sdk_device, &value)
                    {
                        continue;
                    }
                    if route_typed_screen(&sdk_screen, &bytes) {
                        continue;
                    }
                    if route_typed_sms_broadcast(&sdk_sms, &bytes) {
                        continue;
                    }
                    if route_typed_commands_broadcast(&sdk_commands, &bytes) {
                        continue;
                    }
                    if route_typed_camera_broadcast(&sdk_camera, &bytes) {
                        continue;
                    }
                    log::debug!("Dropping untyped device broadcast after SDK migration");
                }
                AnchorTarget::DeviceId(device_id) => {
                    if route_typed_screen(&sdk_screen, &bytes) {
                        continue;
                    }
                    if route_typed_sms_device(&sdk_sms, device_id, &bytes) {
                        continue;
                    }
                    if route_typed_commands_device(&sdk_commands, device_id, &bytes) {
                        continue;
                    }
                    if route_typed_camera_device(&sdk_camera, device_id, &bytes) {
                        continue;
                    }
                    // A reply for a capability that was not negotiated must not
                    // fall back into an unrelated transport.
                    log::debug!("Dropping untyped reply for SDK device {device_id}");
                }
                _ => continue,
            }
        }
        log::debug!("Forwarding thread exiting");
    });
}

fn route_typed_sms_broadcast(
    binding: &crate::anchorapp::sdk_sms::SdkSmsBinding,
    bytes: &[u8],
) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    if value.get("plugin_id").and_then(serde_json::Value::as_str) != Some("smsplugin") {
        return false;
    }
    let mut typed = false;
    for (_, sender) in binding.senders() {
        typed |= crate::anchorapp::sdk_sms::send_json(&sender, &value);
    }
    typed
}

fn route_typed_sms_device(
    binding: &crate::anchorapp::sdk_sms::SdkSmsBinding,
    device_id: &str,
    bytes: &[u8],
) -> bool {
    let Some(sender) = binding.sender(device_id) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    crate::anchorapp::sdk_sms::send_json(&sender, &value)
}

fn route_typed_commands_broadcast(
    binding: &crate::anchorapp::sdk_commands::SdkCommandsBinding,
    bytes: &[u8],
) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let mut typed = false;
    for (_, sender) in binding.senders() {
        typed |= crate::anchorapp::sdk_commands::send_json(&sender, &value);
    }
    typed
}

fn route_typed_commands_device(
    binding: &crate::anchorapp::sdk_commands::SdkCommandsBinding,
    device_id: &str,
    bytes: &[u8],
) -> bool {
    let Some(sender) = binding.sender(device_id) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    crate::anchorapp::sdk_commands::send_json(&sender, &value)
}

fn route_typed_camera_broadcast(
    binding: &crate::anchorapp::sdk_camera::SdkCameraBinding,
    bytes: &[u8],
) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let mut typed = false;
    for (_, sender) in binding.senders() {
        typed |= crate::anchorapp::sdk_camera::send_json(&sender, &value);
    }
    typed
}

fn route_typed_camera_device(
    binding: &crate::anchorapp::sdk_camera::SdkCameraBinding,
    device_id: &str,
    bytes: &[u8],
) -> bool {
    let Some(sender) = binding.sender(device_id) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    crate::anchorapp::sdk_camera::send_json(&sender, &value)
}

fn route_typed_screen(
    binding: &crate::anchorapp::sdk_screen::SdkScreenBinding,
    bytes: &[u8],
) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    if value.get("plugin_id").and_then(serde_json::Value::as_str) != Some("video") {
        return false;
    }
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("output_list") => {
            let outputs = value
                .get("outputs")
                .and_then(serde_json::Value::as_array)
                .map(|outputs| {
                    outputs
                        .iter()
                        .map(|output| anchor_sdk::screen::ScreenOutput {
                            output_id: output.get("id").map_or_else(String::new, |id| {
                                id.to_string().trim_matches('"').to_string()
                            }),
                            display_name: output
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            width: output
                                .get("width")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or_default() as u32,
                            height: output
                                .get("height")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or_default() as u32,
                            refresh_millihz: output
                                .get("refresh_rate_mhz")
                                .or_else(|| output.get("refresh_millihz"))
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or_default()
                                as u32,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let Some(sender) = binding.sender() else {
                binding.defer_outputs(outputs);
                return false;
            };
            let accepted = sender.send_outputs(outputs);
            if accepted {
                log::debug!("SDK screen output list sent over QUIC");
            }
            accepted
        }
        Some("stream_status") => {
            let state = match value.get("state").and_then(serde_json::Value::as_str) {
                Some("starting") => 2,
                Some("streaming") => 3,
                Some("stopping") | Some("idle") => 1,
                Some("error") => 5,
                _ => 0,
            };
            let output_id = value
                .get("selected_index")
                .map_or_else(|| String::from("0"), |index| index.to_string());
            let accepted = binding.publish_status(state, output_id);
            if accepted {
                log::debug!("SDK screen status sent over QUIC");
            }
            accepted
        }
        _ => false,
    }
}
