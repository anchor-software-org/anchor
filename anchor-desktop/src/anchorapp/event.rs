//! Core message types passed between plugins through the broker.

use crate::anchorwayland::wayland_plugin::WaylandPluginMsg;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum AnchorTarget {
    Gui,
    Wayland,
    Network,
    /// Every connected device.
    Device,
    /// A single device, addressed by its device id. Used to reply to whichever
    /// phone originated a request instead of broadcasting to all of them.
    DeviceId(String),
    Service(String),
    Broadcast,
}

#[derive(Debug, Clone)]
pub enum AnchorMessage {
    Wayland(WaylandPluginMsg),
    Generic(String),
    Json(String),
}

#[derive(Debug, Clone)]
pub struct AnchorEvent {
    pub target: AnchorTarget,
    pub message: AnchorMessage,
}

/// Pairing request — shared between DeviceManager and GUI via Arc<Mutex<Option<_>>>.
pub struct PairingRequest {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub fingerprint: String,
    pub safety_number: String,
    pub certificate_pem: String,
    pub response_tx: SyncSender<bool>,
}

pub type SharedPairingRequest = Arc<Mutex<Option<PairingRequest>>>;
