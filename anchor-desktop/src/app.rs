//! Application composition root — builds and wires every plugin together.

use crate::anchorapp::clipboard_models::SharedClipboardHistory;
use crate::anchorapp::clipboard_plugin::ClipboardPlugin;
use crate::anchorapp::command_models::SharedCommands;
use crate::anchorapp::commands_plugin::CommandsPlugin;
use crate::anchorapp::device::{CameraSetupHandle, DeviceManager, DeviceRegistry};
use crate::anchorapp::event::{AnchorEvent, SharedPairingRequest};
use crate::anchorapp::filetransfer_plugin::{
    FileTransferPlugin, SharedFolder, SharedTransferHistory,
};
use crate::anchorapp::input_plugin::AnchorPluginInput;
use crate::anchorapp::log_collector::LogBuffer;
use crate::anchorapp::media_plugin::{MediaPlugin, SharedMediaState};
use crate::anchorapp::message_handler::MessageHandler;
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorapp::sdk_files::SdkFilesBinding;
use crate::anchorapp::sms_plugin::AnchorPluginSMS;
use crate::anchorapp::sms_plugin::SharedSmsThreads;
use crate::anchorapp::tls::AnchorIdentity;
use crate::anchorwayland::wayland_plugin::AnchorPluginWayland;
use crate::foghorn::FoghornPlugin;
use crate::foghorn::SharedNotifications;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// The framework-neutral handles needed by a desktop frontend.
///
/// The Tauri frontend consumes this event stream and drives existing plugins
/// through `broker_tx`, without knowing capture or device transport details.
pub struct DesktopRuntime {
    pub broker_tx: Sender<AnchorEvent>,
    pub gui_events: Receiver<AnchorEvent>,
    pub device_registry: DeviceRegistry,
    pub pairing_request: SharedPairingRequest,
    pub log_buffer: LogBuffer,
    pub foghorn_notifications: SharedNotifications,
    pub sms_threads: SharedSmsThreads,
    pub clipboard_history: SharedClipboardHistory,
    pub filetransfer_history: SharedTransferHistory,
    pub filetransfer_folder: SharedFolder,
    pub remote_media_state: SharedMediaState,
    pub desktop_media_state: SharedMediaState,
    pub commands: SharedCommands,
    pub camera_setup: CameraSetupHandle,
    pub frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
}

pub struct AnchorAppInstance {
    wayland_plugin: AnchorPluginWayland,
    log_buffer: LogBuffer,
    device_manager: DeviceManager,
    sms_plugin: AnchorPluginSMS,
    foghorn_plugin: FoghornPlugin,
    input_plugin: AnchorPluginInput,
    clipboard_plugin: ClipboardPlugin,
    media_plugin: MediaPlugin,
    commands_plugin: CommandsPlugin,
    filetransfer_plugin: FileTransferPlugin,
    broker_tx: Sender<AnchorEvent>,
    gui_event_rx: Option<Receiver<AnchorEvent>>,
}

impl AnchorAppInstance {
    pub fn new_with_logs(log_buffer: LogBuffer, identity: AnchorIdentity) -> AnchorAppInstance {
        Self::build(log_buffer, identity)
    }

    fn build(log_buffer: LogBuffer, identity: AnchorIdentity) -> AnchorAppInstance {
        let (broker_tx, broker_rx) = mpsc::channel::<AnchorEvent>();
        let (gui_tx, gui_rx) = mpsc::channel::<AnchorEvent>();
        let (wayland_tx, wayland_rx) = mpsc::channel::<AnchorEvent>();
        let (network_tx, network_rx) = mpsc::channel::<AnchorEvent>();

        let frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>> = Arc::new(Mutex::new(None));

        let mut handler =
            MessageHandler::new(broker_rx, gui_tx.clone(), wayland_tx.clone(), network_tx.clone());
        let mut wplugin: AnchorPluginWayland = AnchorPluginWayland::init(
            Some(wayland_rx),
            Some(broker_tx.clone()),
            frame_buffer.clone(),
        );

        let (sms_tx, sms_rx) = mpsc::channel::<AnchorEvent>();
        let smsplugin: AnchorPluginSMS =
            AnchorPluginSMS::init(Some(sms_rx), Some(broker_tx.clone()), frame_buffer.clone());

        handler.add_service(String::from("smsplugin"), sms_tx.clone());

        let (foghorn_tx, foghorn_rx) = mpsc::channel::<AnchorEvent>();
        let foghorn_plugin: FoghornPlugin =
            FoghornPlugin::init(Some(foghorn_rx), Some(broker_tx.clone()), frame_buffer.clone());
        handler.add_service(String::from("foghorn"), foghorn_tx);

        let (input_tx, input_rx) = mpsc::channel::<AnchorEvent>();
        let input_plugin: AnchorPluginInput =
            AnchorPluginInput::init(Some(input_rx), Some(broker_tx.clone()), frame_buffer.clone());
        handler.add_service(String::from("input"), input_tx);

        let (clipboard_tx, clipboard_rx) = mpsc::channel::<AnchorEvent>();
        let clipboard_plugin: ClipboardPlugin = ClipboardPlugin::init(
            Some(clipboard_rx),
            Some(broker_tx.clone()),
            frame_buffer.clone(),
        );
        handler.add_service(String::from("clipboard"), clipboard_tx);

        let (media_tx, media_rx) = mpsc::channel::<AnchorEvent>();
        let media_plugin: MediaPlugin =
            MediaPlugin::init(Some(media_rx), Some(broker_tx.clone()), frame_buffer.clone());
        handler.add_service(String::from("media"), media_tx);

        let (commands_tx, commands_rx) = mpsc::channel::<AnchorEvent>();
        let commands_plugin: CommandsPlugin =
            CommandsPlugin::init(Some(commands_rx), Some(broker_tx.clone()), frame_buffer.clone());
        handler.add_service(String::from("commands"), commands_tx);

        let (filetransfer_tx, filetransfer_rx) = mpsc::channel::<AnchorEvent>();
        let filetransfer_plugin: FileTransferPlugin = FileTransferPlugin::init(
            Some(filetransfer_rx),
            Some(broker_tx.clone()),
            frame_buffer.clone(),
        );
        handler.add_service(String::from("filetransfer"), filetransfer_tx);

        // Register a no-op receiver for camera messages that are not handled here.
        let (camera_tx, camera_rx) = mpsc::channel::<AnchorEvent>();
        handler.add_service(String::from("cameraplugin"), camera_tx);
        std::thread::spawn(move || for _event in camera_rx.iter() {});

        handler.start();

        let config_dir = dirs::config_dir().unwrap().join("anchor");
        let sdk_files = SdkFilesBinding::default();
        let sdk_sms = crate::anchorapp::sdk_sms::SdkSmsBinding::default();
        let sdk_commands = crate::anchorapp::sdk_commands::SdkCommandsBinding::default();
        let sdk_camera = crate::anchorapp::sdk_camera::SdkCameraBinding::default();
        let sdk_device = crate::anchorapp::sdk_device::SdkDeviceBinding::default();
        let device_manager = DeviceManager::new(
            config_dir,
            broker_tx.clone(),
            network_rx,
            identity,
            clipboard_plugin.sdk_binding(),
            media_plugin.sdk_binding(),
            foghorn_plugin.sdk_binding(),
            crate::anchorapp::sdk_screen::SdkScreenBinding::default(),
            sdk_files.clone(),
            sdk_sms.clone(),
            sdk_commands.clone(),
            sdk_camera.clone(),
            sdk_device.clone(),
        );

        wplugin.broadcaster = Some(device_manager.broadcaster.clone());

        let mut filetransfer_plugin = filetransfer_plugin;
        filetransfer_plugin.sdk_files = Some(sdk_files);
        // Start background battery monitor (broadcasts to connected devices)
        crate::anchorapp::battery::start_battery_monitor(Some(broker_tx.clone()));

        AnchorAppInstance {
            wayland_plugin: wplugin,
            log_buffer,
            device_manager,
            sms_plugin: smsplugin,
            foghorn_plugin,
            input_plugin,
            clipboard_plugin,
            media_plugin,
            commands_plugin,
            filetransfer_plugin,
            broker_tx,
            gui_event_rx: Some(gui_rx),
        }
    }

    fn start_background_services(
        &mut self,
    ) -> (Option<std::thread::JoinHandle<()>>, Option<std::thread::JoinHandle<()>>) {
        let _sms_handle = self.sms_plugin.run();
        let _foghorn_handle = self.foghorn_plugin.run();
        let _input_handle = self.input_plugin.run();
        let _clipboard_handle = self.clipboard_plugin.run();
        let _media_handle = self.media_plugin.run();
        let _commands_handle = self.commands_plugin.run();
        let _filetransfer_handle = self.filetransfer_plugin.run();
        let wayland_handle = self.wayland_plugin.run();
        let device_handle = self.device_manager.run();

        (wayland_handle, device_handle)
    }

    /// Start the existing services for the desktop frontend.
    ///
    /// This is intentionally small: the capture, device, message, clipboard,
    /// media, and command plugins remain unchanged; the caller gets the UI
    /// event stream and broker sender used by the Tauri shell.
    pub fn run_for_desktop_frontend(&mut self) -> DesktopRuntime {
        let runtime = DesktopRuntime {
            broker_tx: self.broker_tx.clone(),
            gui_events: self
                .gui_event_rx
                .take()
                .expect("desktop frontend event receiver already taken"),
            device_registry: self.device_manager.registry.clone(),
            pairing_request: self.device_manager.pairing_request.clone(),
            log_buffer: self.log_buffer.clone(),
            foghorn_notifications: self.foghorn_plugin.notifications.clone(),
            sms_threads: self.sms_plugin.sms_threads.clone(),
            clipboard_history: self.clipboard_plugin.history.clone(),
            filetransfer_history: self.filetransfer_plugin.history.clone(),
            filetransfer_folder: self.filetransfer_plugin.folder.clone(),
            remote_media_state: self.media_plugin.remote_state.clone(),
            desktop_media_state: self.media_plugin.desktop_state.clone(),
            commands: self.commands_plugin.commands.clone(),
            camera_setup: self.device_manager.camera_setup_handle(),
            frame_buffer: self.wayland_plugin.frame_buffer.clone(),
        };

        // The plugin threads own their receivers after startup, so their join
        // handles can be detached for the lifetime of the Tauri process.
        let _ = self.start_background_services();
        runtime
    }
}
