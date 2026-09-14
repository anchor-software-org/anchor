use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualOutput {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

static COMPOSITOR: OnceLock<Compositor> = OnceLock::new();
static COMPOSITOR_NAME: OnceLock<String> = OnceLock::new();

/// Compositor-specific command providers, auto-detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Sway,
    Hyprland,
    Mutter,
    KWin,
    Cosmic,
    Generic,
}

impl Compositor {
    /// Initialize the global compositor. Call once at startup.
    pub fn init() {
        let compositor = Self::detect();
        let name = match compositor {
            Compositor::Sway => "Sway".to_string(),
            Compositor::Hyprland => "Hyprland".to_string(),
            Compositor::Mutter => "Mutter".to_string(),
            Compositor::KWin => "KWin".to_string(),
            Compositor::Cosmic => "COSMIC".to_string(),
            Compositor::Generic => detect_desktop_name(),
        };
        COMPOSITOR.set(compositor).ok();
        COMPOSITOR_NAME.set(name).ok();
    }

    /// Get the detected compositor. Panics if init() wasn't called.
    pub fn get() -> &'static Compositor {
        COMPOSITOR.get().expect("Compositor not initialized — call Compositor::init() first")
    }

    /// Detect the running compositor from environment variables.
    fn detect() -> Self {
        Self::detect_from_presence(
            std::env::var_os("SWAYSOCK").is_some(),
            std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some(),
            ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "DESKTOP_SESSION"]
                .into_iter()
                .filter_map(|key| std::env::var(key).ok())
                .find_map(|value| compositor_from_session(&value)),
        )
    }

    fn detect_from_presence(
        has_sway_socket: bool,
        has_hyprland_signature: bool,
        desktop_compositor: Option<Compositor>,
    ) -> Self {
        if has_sway_socket {
            log::info!("Detected compositor: Sway");
            Compositor::Sway
        } else if has_hyprland_signature {
            log::info!("Detected compositor: Hyprland");
            Compositor::Hyprland
        } else if let Some(compositor) = desktop_compositor {
            log::info!("Detected compositor: {}", compositor.name());
            compositor
        } else {
            log::debug!("No compositor-specific command provider detected");
            Compositor::Generic
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Compositor::Sway => "Sway",
            Compositor::Hyprland => "Hyprland",
            Compositor::Mutter => "Mutter",
            Compositor::KWin => "KWin",
            Compositor::Cosmic => "COSMIC",
            Compositor::Generic => "Unknown",
        }
    }

    /// User-facing compositor identity. Generic Wayland sessions retain their
    /// desktop name even when Anchor has no compositor-specific command provider.
    pub fn display_name() -> &'static str {
        COMPOSITOR_NAME.get().map(String::as_str).unwrap_or("Unknown")
    }

    /// Whether Anchor can create a correctly sized output through this
    /// compositor provider. Capture support is detected separately at runtime.
    pub fn supports_virtual_displays(&self) -> bool {
        matches!(self, Compositor::Sway)
    }

    const APP_TITLE: &str = "Anchor";
    const APP_ID: &str = "anchor";

    pub fn minimize_window(&self) {
        match self {
            Compositor::Sway => {
                Command::new("swaymsg")
                    .args([&format!("[title={}]", Self::APP_TITLE), "move", "scratchpad"])
                    .spawn()
                    .ok();
            }
            Compositor::Hyprland => {
                Command::new("hyprctl")
                    .args([
                        "dispatch",
                        "movetoworkspacesilent",
                        &format!("special,title:{}", Self::APP_TITLE),
                    ])
                    .spawn()
                    .ok();
            }
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                log::debug!("minimize_window: not supported on this compositor");
            }
        }
    }

    pub fn restore_window(&self) {
        match self {
            Compositor::Sway => {
                Command::new("swaymsg")
                    .args([&format!("[title={}]", Self::APP_TITLE), "scratchpad", "show"])
                    .spawn()
                    .ok();
            }
            Compositor::Hyprland => {
                Command::new("hyprctl").args(["dispatch", "togglespecialworkspace"]).spawn().ok();
            }
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                log::debug!("restore_window: not supported on this compositor");
            }
        }
    }

    pub fn focus_window(&self) {
        match self {
            Compositor::Sway => {
                Command::new("swaymsg")
                    .args([&format!("[app_id=\"{}\"]", Self::APP_ID), "focus"])
                    .output()
                    .ok();
            }
            Compositor::Hyprland => {
                Command::new("hyprctl")
                    .args(["dispatch", "focuswindow", &format!("title:{}", Self::APP_TITLE)])
                    .output()
                    .ok();
            }
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                log::debug!("focus_window: not supported on this compositor");
            }
        }
    }

    pub fn create_virtual_output(&self, name: Option<&str>) -> Result<String, String> {
        match self {
            Compositor::Sway => {
                let output = Command::new("swaymsg")
                    .arg("create_output")
                    .output()
                    .map_err(|e| format!("swaymsg create_output failed: {}", e))?;

                if !output.status.success() {
                    return Err(format!(
                        "swaymsg create_output: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                Ok("created".to_string())
            }
            Compositor::Hyprland => {
                let mut args = vec!["output", "create", "headless"];
                if let Some(n) = name {
                    args.push(n);
                }
                let output = Command::new("hyprctl")
                    .args(&args)
                    .output()
                    .map_err(|e| format!("hyprctl output create failed: {}", e))?;

                if !output.status.success() {
                    return Err(format!(
                        "hyprctl output create: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                Err("Virtual output not supported on this compositor".to_string())
            }
        }
    }

    /// Create and configure a headless display for a remote desktop session.
    /// The returned name is discovered from the compositor, never guessed.
    pub fn create_sized_virtual_output(
        &self,
        width: u32,
        height: u32,
    ) -> Result<VirtualOutput, String> {
        if width < 64 || height < 64 {
            return Err("Virtual display dimensions must be at least 64 × 64.".into());
        }
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err("Virtual display dimensions must be even for H.264 capture.".into());
        }
        match self {
            Compositor::Sway => {
                let before = sway_outputs()?;
                run_sway(["create_output"])?;
                let after = sway_outputs()?;
                let output = after
                    .iter()
                    .find(|output| !before.iter().any(|old| old.name == output.name))
                    .ok_or_else(|| "Sway created a display but did not report its name.".to_string())?;
                let right_edge = after
                    .iter()
                    .filter(|candidate| candidate.name != output.name)
                    .map(|candidate| candidate.x + candidate.width as i64)
                    .max()
                    .unwrap_or(0);
                let mode = format!("{width}x{height}@60Hz");
                run_sway([
                    "output",
                    &output.name,
                    "resolution",
                    &mode,
                    "pos",
                    &right_edge.to_string(),
                    "0",
                ])?;
                Ok(VirtualOutput { name: output.name.clone(), width, height })
            }
            Compositor::Hyprland => Err(
                "Virtual display sizing is not implemented for Hyprland yet. Use Sway for this feature."
                    .into(),
            ),
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                Err("Virtual output not supported on this compositor".into())
            }
        }
    }

    pub fn destroy_virtual_output(&self, name: &str) -> Result<(), String> {
        match self {
            Compositor::Sway => {
                let output = Command::new("swaymsg")
                    .args(["output", name, "unplug"])
                    .output()
                    .map_err(|e| format!("swaymsg output unplug failed: {}", e))?;

                if !output.status.success() {
                    return Err(format!(
                        "swaymsg output unplug: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                Ok(())
            }
            Compositor::Hyprland => {
                let output = Command::new("hyprctl")
                    .args(["output", "remove", name])
                    .output()
                    .map_err(|e| format!("hyprctl output remove failed: {}", e))?;

                if !output.status.success() {
                    return Err(format!(
                        "hyprctl output remove: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
                Ok(())
            }
            Compositor::Mutter | Compositor::KWin | Compositor::Cosmic | Compositor::Generic => {
                Err("Virtual output not supported on this compositor".to_string())
            }
        }
    }
}

fn compositor_from_session(value: &str) -> Option<Compositor> {
    value.split([':', ';']).find_map(|part| {
        let part = part.trim().to_ascii_lowercase();
        if matches!(part.as_str(), "gnome" | "ubuntu" | "mutter")
            || part.starts_with("gnome-")
            || part.starts_with("ubuntu-")
        {
            Some(Compositor::Mutter)
        } else if matches!(part.as_str(), "kde" | "plasma" | "kwin") || part.starts_with("plasma-")
        {
            Some(Compositor::KWin)
        } else if part == "cosmic" || part.starts_with("cosmic-") {
            Some(Compositor::Cosmic)
        } else {
            None
        }
    })
}

fn detect_desktop_name() -> String {
    for key in ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "DESKTOP_SESSION"] {
        let Some(value) = std::env::var_os(key) else { continue };
        let value = value.to_string_lossy();
        let value = value.split(':').find(|part| !part.trim().is_empty()).unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        return normalize_desktop_name(value);
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        "Other Wayland".to_string()
    } else {
        "Unknown".to_string()
    }
}

fn normalize_desktop_name(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "gnome" | "ubuntu" => "GNOME".to_string(),
        "kde" | "plasma" => "KDE Plasma".to_string(),
        "cosmic" => "COSMIC".to_string(),
        "hyprland" => "Hyprland".to_string(),
        "sway" => "Sway".to_string(),
        "wayfire" => "Wayfire".to_string(),
        "river" => "River".to_string(),
        "labwc" => "labwc".to_string(),
        _ => value.to_string(),
    }
}

#[derive(Debug)]
struct SwayOutput {
    name: String,
    x: i64,
    width: u32,
}

fn run_sway<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let output = Command::new("swaymsg")
        .args(args)
        .output()
        .map_err(|error| format!("swaymsg failed: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("swaymsg: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

fn sway_outputs() -> Result<Vec<SwayOutput>, String> {
    let output = Command::new("swaymsg")
        .args(["-t", "get_outputs", "-r"])
        .output()
        .map_err(|error| format!("Could not inspect Sway outputs: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "swaymsg get_outputs: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Could not parse Sway output list: {error}"))?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|output| {
            Some(SwayOutput {
                name: output.get("name")?.as_str()?.to_string(),
                x: output.get("rect")?.get("x")?.as_i64()?,
                width: output.get("rect")?.get("width")?.as_u64()? as u32,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_methods_dont_panic_and_virtual_output_returns_err() {
        let c = Compositor::Generic;
        c.minimize_window();
        c.restore_window();
        c.focus_window();
        assert!(c.create_virtual_output(None).is_err());
        assert!(c.destroy_virtual_output("test").is_err());
    }

    #[test]
    fn detect_without_compositor_env_returns_generic() {
        assert_eq!(Compositor::detect_from_presence(false, false, None), Compositor::Generic);
    }

    #[test]
    fn detect_with_swaysock_returns_sway() {
        assert_eq!(Compositor::detect_from_presence(true, false, None), Compositor::Sway);
        assert_eq!(
            Compositor::detect_from_presence(true, true, Some(Compositor::Mutter)),
            Compositor::Sway
        );
    }

    #[test]
    fn detect_with_hyprland_returns_hyprland() {
        assert_eq!(Compositor::detect_from_presence(false, true, None), Compositor::Hyprland);
    }

    #[test]
    fn detects_mutter_from_gnome_and_ubuntu_sessions() {
        assert_eq!(compositor_from_session("ubuntu:GNOME"), Some(Compositor::Mutter));
        assert_eq!(compositor_from_session("gnome-wayland"), Some(Compositor::Mutter));
        assert_eq!(compositor_from_session("Mutter"), Some(Compositor::Mutter));
        assert_eq!(
            Compositor::detect_from_presence(false, false, Some(Compositor::Mutter)),
            Compositor::Mutter
        );
    }

    #[test]
    fn detects_other_common_desktop_compositors() {
        assert_eq!(compositor_from_session("KDE"), Some(Compositor::KWin));
        assert_eq!(compositor_from_session("KDE:Plasma"), Some(Compositor::KWin));
        assert_eq!(compositor_from_session("COSMIC"), Some(Compositor::Cosmic));
        assert_eq!(compositor_from_session("river"), None);
    }

    #[test]
    fn normalizes_common_wayland_desktop_names() {
        assert_eq!(normalize_desktop_name("river"), "River");
        assert_eq!(normalize_desktop_name("wayfire"), "Wayfire");
        assert_eq!(normalize_desktop_name("plasma"), "KDE Plasma");
        assert_eq!(normalize_desktop_name("SomeCompositor"), "SomeCompositor");
    }
}
