//! Desktop publisher for `org.anchor.device@1` state records.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anchor_sdk::{Capability, device};

#[derive(Clone)]
pub struct SdkDeviceSender {
    tx: mpsc::SyncSender<(u8, bool, String)>,
}

impl SdkDeviceSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel::<(u8, bool, String)>(8);
        thread::Builder::new()
            .name("anchor-sdk-device-writer".into())
            .spawn(move || {
                let Ok(runtime) =
                    tokio::runtime::Builder::new_current_thread().enable_all().build()
                else {
                    log::error!("SDK device writer runtime failed");
                    return;
                };
                for (battery_percent, charging, name) in rx {
                    let payload = device::encode_state(device::DeviceState {
                        battery_percent: battery_percent.into(),
                        charging,
                        on_wifi: false,
                        device_name: name,
                        display_width: 0,
                        display_height: 0,
                    });
                    if let Err(error) =
                        runtime.block_on(capability.send_record(device::STATE_TYPE_URL, payload))
                    {
                        log::debug!("SDK desktop device-state send failed: {error}");
                    }
                }
            })
            .expect("SDK device writer thread must start");
        Self { tx }
    }

    pub fn send(&self, battery_percent: u8, charging: bool, name: String) -> bool {
        self.tx.try_send((battery_percent.min(100), charging, name)).is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkDeviceBinding {
    senders: Arc<Mutex<HashMap<String, SdkDeviceSender>>>,
}

impl SdkDeviceBinding {
    pub fn attach(&self, device_id: String, sender: SdkDeviceSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn senders(&self) -> Vec<SdkDeviceSender> {
        self.senders.lock().unwrap().values().cloned().collect()
    }
}

/// Translate the desktop battery monitor's local event into typed records.
pub fn send_json(binding: &SdkDeviceBinding, value: &serde_json::Value) -> bool {
    if value.get("type").and_then(serde_json::Value::as_str) != Some("battery_update")
        || value.get("device_id").and_then(serde_json::Value::as_str) != Some("desktop")
    {
        return false;
    }
    let Some(level) = value.get("level").and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let charging = value.get("charging").and_then(serde_json::Value::as_bool).unwrap_or(false);
    binding.senders().into_iter().fold(false, |sent, sender| {
        sent | sender.send(level.min(100) as u8, charging, "Anchor Desktop".into())
    })
}
