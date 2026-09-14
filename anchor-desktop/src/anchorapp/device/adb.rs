//! ADB scanner — watches for Android devices via USB/WiFi.
//!
//! Anchor's transport is QUIC over UDP. ADB's `reverse` command only proxies
//! TCP, so scanning USB devices must not imply that USB forwarding supports
//! the production transport.
//!
//! Uses the `adb` CLI directly instead of the `adb_client` crate to avoid pulling
//! in ~300 transitive dependencies for a few trivial shell commands.

use std::{collections::HashSet, sync::mpsc::Sender, thread, time::Duration};

use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};

pub(crate) fn start_adb_scanner(broker_tx: Sender<AnchorEvent>) {
    log::debug!("Starting adb scanner");
    thread::spawn(move || {
        let mut known_devices: HashSet<String> = HashSet::new();

        loop {
            match adb_devices() {
                Ok(devices) => {
                    let current: HashSet<String> =
                        devices.iter().map(|d| d.serial.clone()).collect();

                    for device_id in current.difference(&known_devices) {
                        log::info!("New ADB device: {}", device_id);
                        notify_device_detected(device_id.clone(), broker_tx.clone());
                    }

                    for device_id in known_devices.difference(&current) {
                        log::info!("ADB device disconnected: {}", device_id);
                        broker_tx
                            .send(AnchorEvent {
                                target: AnchorTarget::Gui,
                                message: AnchorMessage::Json(
                                    serde_json::json!({
                                        "type": "device_disconnected",
                                        "device_id": device_id,
                                    })
                                    .to_string(),
                                ),
                            })
                            .ok();
                    }

                    known_devices = current;
                }
                Err(e) => {
                    // adb missing or erroring out won't fix itself on a retry
                    // loop — stop the scanner instead of spamming every tick.
                    log::info!("ADB scanner stopping (adb unavailable): {}", e);
                    return;
                }
            }

            thread::sleep(Duration::from_secs(4));
        }
    });
}

struct AdbDevice {
    serial: String,
}

/// Run `adb devices` and extract serial numbers from the output.
/// Output looks like:
///   List of devices attached
///   R5CT123ABC    device
fn adb_devices() -> Result<Vec<AdbDevice>, String> {
    let output = std::process::Command::new("adb")
        .arg("devices")
        .output()
        .map_err(|e| format!("adb command failed: {}", e))?;

    if !output.status.success() {
        return Err(format!("adb devices failed: {}", String::from_utf8_lossy(&output.stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let devices: Vec<AdbDevice> = stdout
        .lines()
        .skip(1) // skip "List of devices attached"
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "device" {
                Some(AdbDevice { serial: parts[0].to_string() })
            } else {
                None
            }
        })
        .collect();

    Ok(devices)
}

fn notify_device_detected(serial: String, broker_tx: Sender<AnchorEvent>) {
    thread::spawn(move || {
        log::info!(
            "ADB device {} detected; QUIC uses UDP 5027 and is not reverse-proxied by adb",
            serial
        );
        // Detection is informational only. Do not emit `device_connected`:
        // that event historically meant that the TCP reverse proxies were
        // ready, which is no longer true for a UDP QUIC endpoint.
        broker_tx
            .send(AnchorEvent {
                target: AnchorTarget::Gui,
                message: AnchorMessage::Json(
                    serde_json::json!({
                        "type": "device_discovered",
                        "device_id": serial,
                    })
                    .to_string(),
                ),
            })
            .ok();
    });
}
