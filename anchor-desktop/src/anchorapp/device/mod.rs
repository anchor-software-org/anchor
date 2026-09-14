mod adb;
mod broadcaster;
pub(crate) mod camera_receiver;
pub mod loopback_setup;
mod manager;
mod mdns;
mod registry;
mod sdk_server;

use serde::{Deserialize, Serialize};

pub use broadcaster::{BroadcastFrame, FrameBroadcaster};
pub use manager::{CameraGuiStatus, CameraSetupHandle, DeviceManager};
pub use registry::{DeviceRegistry, DeviceSnapshot, PairedDeviceSnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Desktop,
    Mobile,
    Ipad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Discovered,
    Pairing,
    Paired,
    Connected,
}

#[derive(Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub device_type: DeviceType,
    pub capabilities: Vec<String>,
    pub certificate_pem: String,
    pub paired_at: u64,
    pub last_seen: u64,
}

pub(crate) struct Device {
    pub info: DeviceInfo,
    pub state: DeviceState,
    pub display_size: Option<(u32, u32)>,
}

pub enum DeviceCheckResult {
    Trusted(String),
    Unknown(String),
}
