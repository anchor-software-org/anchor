pub mod monitor;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorapp::sdk_notifications::SdkNotificationsBinding;
use monitor::{handle_incoming_notification, monitor_desktop_notifications};

/// A notification received from a remote device or the local desktop.
#[derive(Debug, Clone)]
pub struct NotificationEntry {
    pub source: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub timestamp: u64,
    /// Android package name, used to look up cached icon.
    pub app_package: String,
    /// Freedesktop icon name supplied by a local desktop application.
    pub icon_name: String,
}

/// Shared notification log — FoghornPlugin writes, GUI reads.
pub type SharedNotifications = Arc<Mutex<Vec<NotificationEntry>>>;

/// Upper bound on retained notifications. Spammy sources (e.g. the System UI
/// "Charging" notification) would otherwise grow the log without limit.
pub const MAX_NOTIFICATIONS: usize = 200;

/// Resolve the cache location for a remote application's icon without allowing
/// a device-provided package name to escape Anchor's cache directory.
pub fn cached_icon_path(app_package: &str) -> Option<PathBuf> {
    let valid = !app_package.is_empty()
        && app_package.len() <= 255
        && !app_package.contains("..")
        && app_package.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        });
    if !valid {
        return None;
    }
    Some(dirs::cache_dir()?.join("anchor").join("icons").join(format!("{app_package}.png")))
}

/// Resolve a freedesktop application icon from fixed system icon locations.
/// Icon hints are treated as names, never as arbitrary paths.
pub fn system_icon_path(icon_name: &str) -> Option<PathBuf> {
    let icon_name = std::path::Path::new(icon_name).file_stem()?.to_str()?.to_ascii_lowercase();
    let valid = !icon_name.is_empty()
        && icon_name.len() <= 128
        && !icon_name.contains("..")
        && icon_name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        });
    if !valid {
        return None;
    }

    let roots = [PathBuf::from("/usr/share/icons"), PathBuf::from("/usr/share/pixmaps")];
    let relative_paths = [
        format!("hicolor/64x64/apps/{icon_name}.png"),
        format!("hicolor/48x48/apps/{icon_name}.png"),
        format!("hicolor/128x128/apps/{icon_name}.png"),
        format!("hicolor/scalable/apps/{icon_name}.svg"),
        format!("{icon_name}.png"),
        format!("{icon_name}.svg"),
    ];
    roots
        .iter()
        .flat_map(|root| relative_paths.iter().map(move |relative| root.join(relative)))
        .find(|path| path.is_file())
}

/// Append a notification, evicting the oldest entries past `MAX_NOTIFICATIONS`.
pub fn push_notification(notifications: &SharedNotifications, entry: NotificationEntry) {
    let mut log = notifications.lock().unwrap();
    log.push(entry);
    if log.len() > MAX_NOTIFICATIONS {
        let overflow = log.len() - MAX_NOTIFICATIONS;
        log.drain(0..overflow);
    }
}

pub struct FoghornPlugin {
    pub plugin_rx: Option<Receiver<AnchorEvent>>,
    pub broker_tx: Option<Sender<AnchorEvent>>,
    pub notifications: SharedNotifications,
    sdk_binding: SdkNotificationsBinding,
}

impl FoghornPlugin {
    pub fn sdk_binding(&self) -> SdkNotificationsBinding {
        self.sdk_binding.clone()
    }
}

impl Plugin for FoghornPlugin {
    fn run(&mut self) -> Option<JoinHandle<()>> {
        let rx = self.plugin_rx.take().expect("foghorn plugin_rx not set");
        let notifications = self.notifications.clone();
        let notifications_for_monitor = self.notifications.clone();
        let sdk_binding = self.sdk_binding.clone();

        // Thread 1: Handle incoming notifications from phone
        let handle = thread::Builder::new()
            .name("foghorn-receiver".to_string())
            .spawn(move || {
                log::debug!("Foghorn receiver started");

                for event in rx.iter() {
                    if let AnchorMessage::Json(payload) = &event.message {
                        handle_incoming_notification(payload, &notifications);
                    }
                }

                log::info!("Foghorn receiver exited");
            })
            .expect("Failed to spawn foghorn receiver thread");

        // Thread 2: Monitor D-Bus for desktop notifications, forward to phone
        thread::Builder::new()
            .name("foghorn-dbus-monitor".to_string())
            .spawn(move || {
                log::debug!("Foghorn D-Bus monitor starting");
                monitor_desktop_notifications(notifications_for_monitor, sdk_binding);
            })
            .expect("Failed to spawn foghorn D-Bus monitor thread");

        Some(handle)
    }

    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        FoghornPlugin {
            plugin_rx,
            broker_tx,
            notifications: Arc::new(Mutex::new(Vec::new())),
            sdk_binding: SdkNotificationsBinding::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cached_icon_path, system_icon_path};

    #[test]
    fn cached_icon_path_rejects_parent_traversal() {
        assert!(cached_icon_path("../../outside").is_none());
    }

    #[test]
    fn system_icon_path_rejects_parent_traversal() {
        assert!(system_icon_path("../../outside").is_none());
    }
}
