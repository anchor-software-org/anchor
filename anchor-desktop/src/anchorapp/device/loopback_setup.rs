//! v4l2loopback module loading + status detection.
//!
//! Pinned device path: `/dev/video42`, label="Anchor Camera".
//!
//! Setup strategy — modeled after OBS's linux-v4l2 virtual camera:
//!   1. `/dev/video42` already exists with loopback driver → done.
//!   2. Module not loaded → `pkexec modprobe v4l2loopback … && sleep 0.5`.
//!
//! IMPORTANT: do NOT pass `exclusive_caps=1`. In v4l2loopback 0.15+, it
//! HIDES VIDEO_CAPTURE from the device, which breaks ffplay/OBS/Zoom readers.
//! Without it, the device reports both VIDEO_OUTPUT + VIDEO_CAPTURE by default.
//!
//! Two entry points:
//!   `ensure_loopback_pkexec()` — GUI path, uses pkexec (polkit popup)
//!   `ensure_loopback_nopass()` — boot-time auto-load, uses sudo -n only

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

pub const ANCHOR_VIDEO_NR: u32 = 42;
pub const ANCHOR_CARD_LABEL: &str = "Anchor Camera";

pub fn anchor_device_path() -> PathBuf {
    PathBuf::from(format!("/dev/video{}", ANCHOR_VIDEO_NR))
}

#[derive(Debug, Clone)]
pub enum LoopbackStatus {
    /// `/dev/video42` exists and reports a `loopback` driver.
    Ready { device: PathBuf },
    /// `v4l2loopback` is loaded but our pinned device isn't ready.
    ModuleLoaded,
    /// Module not loaded.
    NotLoaded,
}

#[derive(Debug, Clone)]
pub enum LoopbackError {
    ModprobeFailed { stderr: String },
    StillNotPresent,
    NeedPassword,
}

impl std::fmt::Display for LoopbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModprobeFailed { stderr } => write!(f, "Setup command failed: {}", stderr.trim()),
            Self::StillNotPresent => {
                write!(f, "Setup ran but /dev/video{} did not appear.", ANCHOR_VIDEO_NR)
            }
            Self::NeedPassword => {
                write!(f, "Passwordless sudo not configured. Use 'Create camera' in Settings.")
            }
        }
    }
}

/// Check whether `/dev/video42` exists with a loopback driver.
/// OBS doesn't check for specific caps — just the driver. We do the same.
pub fn loopback_status() -> LoopbackStatus {
    let path = anchor_device_path();
    if path.exists()
        && let Some(driver) = super::camera_receiver::query_v4l2_driver(&path.to_string_lossy())
        && driver.contains("loopback")
    {
        return LoopbackStatus::Ready { device: path };
    }
    if module_loaded() { LoopbackStatus::ModuleLoaded } else { LoopbackStatus::NotLoaded }
}

fn module_loaded() -> bool {
    if PathBuf::from("/dev/v4l2loopback").exists() {
        return true;
    }
    std::fs::read_to_string("/proc/modules")
        .map(|m| m.lines().any(|l| l.starts_with("v4l2loopback ")))
        .unwrap_or(false)
}

/// Build the modprobe command. Mirrors OBS pattern: `pkexec modprobe ... && sleep 0.5`.
/// NOTE: do NOT pass `exclusive_caps=1` — it HIDES VIDEO_CAPTURE in v4l2loopback 0.15+.
/// Without it, the device reports both VIDEO_OUTPUT and VIDEO_CAPTURE (needed by ffplay/OBS/Zoom readers).
fn modprobe_cmd() -> String {
    format!(
        "modprobe v4l2loopback devices=1 video_nr={} card_label='{}' && sleep 0.5",
        ANCHOR_VIDEO_NR, ANCHOR_CARD_LABEL
    )
}

/// GUI path: uses `pkexec` (polkit graphical auth dialog).
/// No password handling — polkit does its own auth.
pub fn ensure_loopback_pkexec() -> Result<PathBuf, LoopbackError> {
    if matches!(loopback_status(), LoopbackStatus::Ready { .. }) {
        return Ok(anchor_device_path());
    }

    let cmd = modprobe_cmd();
    log::info!("[camera] setup (pkexec): {}", cmd);

    // Close any existing receiver fd before modprobe so rmmod can succeed
    // if the module already has devices we need to replace.
    //
    // NOTE: we deliberately drop & sleep BEFORE building modprobe_cmd()
    // (which calls loopback_status) so that the receiver thread exits
    // and releases its fd. This is handled in create_camera() in manager.rs.
    let output = Command::new("pkexec")
        .arg("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .map_err(|e| LoopbackError::ModprobeFailed { stderr: e.to_string() })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    log::info!(
        "[camera] pkexec exit={} stdout={:?} stderr={:?}",
        output.status.code().unwrap_or(-1),
        stdout.trim(),
        stderr.trim()
    );

    if !output.status.success() {
        return Err(LoopbackError::ModprobeFailed { stderr: stderr.into_owned() });
    }

    wait_for_device()
}

/// Boot-time auto-load: passwordless sudo only.
pub fn ensure_loopback_nopass() -> Result<PathBuf, LoopbackError> {
    if matches!(loopback_status(), LoopbackStatus::Ready { .. }) {
        return Ok(anchor_device_path());
    }

    let cmd = modprobe_cmd();
    log::info!("[camera] setup (sudo -n): sh -c \"{}\"", cmd);
    let out = Command::new("sudo")
        .arg("-n")
        .arg("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .map_err(|e| LoopbackError::ModprobeFailed { stderr: e.to_string() })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if stderr.contains("password") || stderr.contains("no tty") || stderr.contains("sudo:") {
            return Err(LoopbackError::NeedPassword);
        }
        return Err(LoopbackError::ModprobeFailed { stderr });
    }
    wait_for_device()
}

fn wait_for_device() -> Result<PathBuf, LoopbackError> {
    log::info!("[camera] waiting for /dev/video{} to appear...", ANCHOR_VIDEO_NR);
    for _attempt in 0..40 {
        if let LoopbackStatus::Ready { device } = loopback_status() {
            log::info!("[camera] device ready at {}", device.display());
            return Ok(device);
        }
        thread::sleep(Duration::from_millis(50));
    }
    // Log final state for diagnosis
    let path = anchor_device_path();
    log::warn!(
        "[camera] device still not ready after 2s. exists={} module={}",
        path.exists(),
        module_loaded()
    );
    if path.exists()
        && let Some(driver) = super::camera_receiver::query_v4l2_driver(&path.to_string_lossy())
    {
        log::warn!("[camera] driver={:?}", driver);
    }
    Err(LoopbackError::StillNotPresent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_device_path_is_video42() {
        assert_eq!(anchor_device_path().to_string_lossy(), "/dev/video42");
    }

    #[test]
    fn modprobe_cmd_contains_expected_params() {
        let cmd = modprobe_cmd();
        assert!(cmd.starts_with("modprobe v4l2loopback"));
        assert!(cmd.contains("devices=1"));
        assert!(cmd.contains("video_nr=42"));
        assert!(cmd.contains("card_label='Anchor Camera'"));
        assert!(cmd.contains("sleep 0.5"));
        // Must NOT contain exclusive_caps — it hides VIDEO_CAPTURE
        assert!(!cmd.contains("exclusive_caps"));
    }

    #[test]
    fn loopback_error_display() {
        let e = LoopbackError::ModprobeFailed { stderr: "modprobe: FATAL".into() };
        assert!(e.to_string().contains("FATAL"));

        let e = LoopbackError::StillNotPresent;
        assert!(e.to_string().contains("/dev/video42"));
        assert!(e.to_string().contains("did not appear"));

        let e = LoopbackError::NeedPassword;
        assert!(e.to_string().contains("Passwordless sudo"));
    }
}
