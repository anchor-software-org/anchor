use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::anchorapp::tls::TrustedStore;

use super::{Device, DeviceCheckResult, DeviceInfo, DeviceState, DeviceType};

/// Pairing store plus UI-visible state of live SDK sessions. It deliberately
/// owns no sockets, writers, or JSON queues: `anchor_sdk` owns transport.
#[derive(Clone)]
pub struct DeviceRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

struct RegistryState {
    devices: HashMap<String, Device>,
    connections: HashMap<String, usize>,
    trusted_store: TrustedStore,
}

#[derive(Debug, Clone)]
pub struct DeviceSnapshot {
    pub id: String,
    pub name: String,
    pub device_type: DeviceType,
    pub capabilities: Vec<String>,
    pub state: DeviceState,
    pub has_control: bool,
    pub has_video: bool,
    pub display_size: Option<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub struct PairedDeviceSnapshot {
    pub device_id: String,
    pub device_name: String,
    pub certificate_pem: String,
    pub is_online: bool,
}

/// Dropping a live, authenticated SDK session makes the device offline after
/// its last session closes, including error and early-return paths.
pub(crate) struct SdkConnection {
    registry: DeviceRegistry,
    device_id: String,
}

impl Drop for SdkConnection {
    fn drop(&mut self) {
        self.registry.disconnect_sdk(&self.device_id);
    }
}

impl DeviceRegistry {
    pub fn new(trusted_store: TrustedStore) -> Self {
        let mut devices = HashMap::new();
        for (id, entry) in &trusted_store.devices {
            devices.insert(id.clone(), seeded_device(entry));
        }
        log::info!("DeviceRegistry: loaded {} paired devices", devices.len());
        Self {
            inner: Arc::new(Mutex::new(RegistryState {
                devices,
                connections: HashMap::new(),
                trusted_store,
            })),
        }
    }

    pub(crate) fn check_device(&self, cert_der: &[u8]) -> DeviceCheckResult {
        let state = self.inner.lock().unwrap();
        match state.trusted_store.is_trusted(cert_der) {
            Some(device_id) => DeviceCheckResult::Trusted(device_id),
            None => DeviceCheckResult::Unknown(TrustedStore::fingerprint(cert_der)),
        }
    }

    pub(crate) fn mark_last_seen(&self, device_id: &str) {
        let mut state = self.inner.lock().unwrap();
        state.trusted_store.update_last_seen(device_id);
        let last_seen = state.trusted_store.devices.get(device_id).map(|entry| entry.last_seen);
        if let (Some(last_seen), Some(device)) = (last_seen, state.devices.get_mut(device_id)) {
            device.info.last_seen = last_seen;
        }
    }

    pub(crate) fn add_paired_device(&self, device_id: &str, device_name: &str, cert_pem: &str) {
        let mut state = self.inner.lock().unwrap();
        state.trusted_store.add_device(device_id, device_name, cert_pem);
        if let Some(entry) = state.trusted_store.devices.get(device_id) {
            let device = seeded_device(entry);
            state.devices.entry(device_id.to_owned()).or_insert(device);
        }
    }

    pub(crate) fn connect_sdk(&self, device_id: &str) -> SdkConnection {
        let mut state = self.inner.lock().unwrap();
        *state.connections.entry(device_id.to_owned()).or_insert(0) += 1;
        if let Some(device) = state.devices.get_mut(device_id) {
            device.state = DeviceState::Connected;
        }
        log::info!("SDK device connected: {device_id}");
        SdkConnection { registry: self.clone(), device_id: device_id.to_owned() }
    }

    pub(crate) fn register_sdk_capability(&self, device_id: &str, capability: &str) {
        let mut state = self.inner.lock().unwrap();
        let Some(device) = state.devices.get_mut(device_id) else { return };
        if !device.info.capabilities.iter().any(|known| known == capability) {
            device.info.capabilities.push(capability.to_owned());
        }
    }

    /// Record the connected peer's physical display bounds for sizing a
    /// matching virtual output. Zero dimensions are intentionally ignored:
    /// protobuf's default means the peer did not provide this optional data.
    pub(crate) fn set_display_size(&self, device_id: &str, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let mut state = self.inner.lock().unwrap();
        if let Some(device) = state.devices.get_mut(device_id) {
            device.display_size = Some((width, height));
        }
    }

    fn disconnect_sdk(&self, device_id: &str) {
        let mut state = self.inner.lock().unwrap();
        let Some(count) = state.connections.get_mut(device_id) else { return };
        *count = count.saturating_sub(1);
        if *count != 0 {
            return;
        }
        state.connections.remove(device_id);
        if let Some(device) = state.devices.get_mut(device_id) {
            device.state = DeviceState::Paired;
            device.info.capabilities.clear();
            device.display_size = None;
        }
        log::info!("SDK device disconnected: {device_id}");
    }

    pub fn unpair_device(&self, device_id: &str) {
        let mut state = self.inner.lock().unwrap();
        state.trusted_store.remove_device(device_id);
        state.connections.remove(device_id);
        state.devices.remove(device_id);
        log::info!("Unpaired device: {device_id}");
    }

    pub fn paired_devices(&self) -> Vec<PairedDeviceSnapshot> {
        let state = self.inner.lock().unwrap();
        state
            .trusted_store
            .devices
            .iter()
            .map(|(device_id, entry)| PairedDeviceSnapshot {
                device_id: device_id.clone(),
                device_name: entry.device_name.clone(),
                certificate_pem: entry.certificate_pem.clone(),
                is_online: state.connections.get(device_id).copied().unwrap_or_default() > 0,
            })
            .collect()
    }

    pub fn connected_devices(&self) -> Vec<DeviceSnapshot> {
        let state = self.inner.lock().unwrap();
        state
            .devices
            .values()
            .filter(|device| device.state == DeviceState::Connected)
            .map(snapshot_from_device)
            .collect()
    }
}

fn seeded_device(entry: &crate::anchorapp::tls::TrustedDeviceEntry) -> Device {
    Device {
        info: DeviceInfo {
            id: entry.device_id.clone(),
            name: entry.device_name.clone(),
            device_type: DeviceType::Mobile,
            capabilities: Vec::new(),
            certificate_pem: entry.certificate_pem.clone(),
            paired_at: entry.paired_at,
            last_seen: entry.last_seen,
        },
        state: DeviceState::Paired,
        display_size: None,
    }
}

fn snapshot_from_device(device: &Device) -> DeviceSnapshot {
    DeviceSnapshot {
        id: device.info.id.clone(),
        name: device.info.name.clone(),
        device_type: device.info.device_type.clone(),
        capabilities: device.info.capabilities.clone(),
        state: device.state.clone(),
        has_control: true,
        // Capability names are canonical protocol names. The old `screen`
        // shorthand made a fully negotiated screen provider look unavailable
        // to the desktop UI.
        has_video: device
            .info
            .capabilities
            .iter()
            .any(|capability| capability == "org.anchor.screen"),
        display_size: device.display_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (DeviceRegistry, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "anchor-registry-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let store = TrustedStore::load(&directory);
        let registry = DeviceRegistry::new(store);
        registry.add_paired_device("phone", "Phone", "certificate");
        (registry, directory)
    }

    #[test]
    fn sdk_connection_tracks_online_state_until_last_session_closes() {
        let (registry, directory) = registry();
        let first = registry.connect_sdk("phone");
        let second = registry.connect_sdk("phone");
        registry.register_sdk_capability("phone", "sms");
        assert_eq!(registry.connected_devices()[0].capabilities, ["sms"]);
        assert!(registry.paired_devices()[0].is_online);

        drop(first);
        assert!(registry.paired_devices()[0].is_online);
        drop(second);
        assert!(registry.connected_devices().is_empty());
        assert!(!registry.paired_devices()[0].is_online);
        drop(registry);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn canonical_screen_capability_marks_device_as_video_capable() {
        let (registry, directory) = registry();
        let connection = registry.connect_sdk("phone");
        registry.register_sdk_capability("phone", "org.anchor.screen");
        assert!(registry.connected_devices()[0].has_video);
        drop(connection);
        drop(registry);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn tracks_connected_phone_display_size() {
        let (registry, directory) = registry();
        let connection = registry.connect_sdk("phone");
        registry.set_display_size("phone", 1440, 3120);
        assert_eq!(registry.connected_devices()[0].display_size, Some((1440, 3120)));
        drop(connection);
        assert!(!registry.paired_devices()[0].is_online);
        drop(registry);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
