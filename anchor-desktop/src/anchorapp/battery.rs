//! Battery monitoring — polls local battery via sysfs and receives
//! mobile battery updates from connected devices.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8},
        mpsc::Sender,
    },
    thread,
    time::Duration,
};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};

/// Shared battery state: level (0-100) + charging flag + status text.
pub struct BatteryState {
    pub level: AtomicU8,
    pub charging: AtomicBool,
    pub status: Mutex<String>,
}

impl BatteryState {
    #[cfg(test)]
    fn new() -> Self {
        Self {
            level: AtomicU8::new(0),
            charging: AtomicBool::new(false),
            status: Mutex::new(String::new()),
        }
    }
}

/// Per-device mobile battery info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileBattery {
    pub level: u8,
    pub charging: bool,
}

/// Shared mutable map from device_id → MobileBattery.
pub type MobileBatteryMap = Arc<Mutex<HashMap<String, MobileBattery>>>;

pub fn new_mobile_battery_map() -> MobileBatteryMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Start polling the local desktop battery. Broadcasts to connected devices
/// via `broker_tx` if provided.
pub fn start_battery_monitor(broker_tx: Option<Sender<AnchorEvent>>) {
    thread::spawn(move || {
        if read_battery_sysfs().is_none() {
            log::info!("[battery] no battery hardware detected — monitor exiting");
            return;
        }

        loop {
            if let Some((level, charging, status)) = read_battery_sysfs()
                && let Some(ref tx) = broker_tx
            {
                let payload = serde_json::json!({
                    "type": "battery_update",
                    "device_id": "desktop",
                    "level": level,
                    "charging": charging,
                    "status_text": status,
                })
                .to_string();
                let _ = tx.send(AnchorEvent {
                    target: AnchorTarget::Device,
                    message: AnchorMessage::Json(payload),
                });
            }
            thread::sleep(Duration::from_secs(30));
        }
    });
}

fn read_battery_sysfs() -> Option<(u8, bool, String)> {
    for bat in &["BAT0", "BAT1"] {
        let base = format!("/sys/class/power_supply/{}/", bat);
        if let (Ok(cap), Ok(status)) = (
            std::fs::read_to_string(format!("{}capacity", base)),
            std::fs::read_to_string(format!("{}status", base)),
        ) {
            let level = cap.trim().parse::<u8>().unwrap_or(0);
            let charging = status.trim() == "Charging";
            return Some((level, charging, status.trim().to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn battery_state_new_has_defaults() {
        let state = BatteryState::new();
        assert_eq!(state.level.load(Ordering::Relaxed), 0);
        assert!(!state.charging.load(Ordering::Relaxed));
        assert_eq!(*state.status.lock().unwrap(), "");
    }

    #[test]
    fn mobile_battery_map_insert_and_get() {
        let map = new_mobile_battery_map();
        map.lock()
            .unwrap()
            .insert("phone-1".to_string(), MobileBattery { level: 75, charging: true });
        let entry = map.lock().unwrap().get("phone-1").cloned();
        assert_eq!(entry, Some(MobileBattery { level: 75, charging: true }));
    }

    #[test]
    fn mobile_battery_map_missing_returns_none() {
        let map = new_mobile_battery_map();
        assert!(map.lock().unwrap().get("nonexistent").is_none());
    }

    #[test]
    fn mobile_battery_map_remove() {
        let map = new_mobile_battery_map();
        map.lock()
            .unwrap()
            .insert("phone-1".to_string(), MobileBattery { level: 50, charging: false });
        map.lock().unwrap().remove("phone-1");
        assert!(map.lock().unwrap().get("phone-1").is_none());
    }
}
