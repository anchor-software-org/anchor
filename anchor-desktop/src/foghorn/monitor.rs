use super::{NotificationEntry, SharedNotifications, cached_icon_path, push_notification};
use crate::anchorapp::sdk_notifications::SdkNotificationsBinding;

/// Parse incoming notification JSON from a remote device and:
/// 1. Store in the session log (for GUI tab)
/// 2. Show a D-Bus desktop notification
pub fn handle_incoming_notification(payload: &str, notifications: &SharedNotifications) {
    let json: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Foghorn: invalid JSON: {}", e);
            return;
        }
    };

    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if msg_type != "notification" {
        return;
    }

    let source = json.get("source").and_then(|v| v.as_str()).unwrap_or("unknown");
    let app_name = json.get("app_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
    let title = json.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let body = json.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let timestamp = json.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
    let app_package = json.get("app_package").and_then(|v| v.as_str()).unwrap_or("");
    let icon_base64 = json.get("icon_base64").and_then(|v| v.as_str());

    log::info!("Foghorn: notification from {} — {} : {} — {}", source, app_name, title, body);

    // Store in session log
    let entry = NotificationEntry {
        source: source.to_string(),
        app_name: app_name.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        timestamp,
        app_package: app_package.to_string(),
        icon_name: String::new(),
    };
    push_notification(notifications, entry);

    // Resolve icon: use cached app icon if available, fall back to anchor-desktop.
    let icon = resolve_icon(app_package, icon_base64);

    // Show D-Bus desktop notification
    if let Err(e) = notify_rust::Notification::new()
        .summary(&format!("{} — {}", app_name, title))
        .body(body)
        .appname("Anchor")
        .icon(&icon)
        .timeout(5000)
        .show()
    {
        log::warn!("Foghorn: D-Bus notification failed: {}", e);
    }
}

/// Monitor the D-Bus session bus for desktop notifications (from other apps).
/// When a notification is posted via `org.freedesktop.Notifications.Notify`,
/// we intercept it and forward to the connected phone.
pub fn monitor_desktop_notifications(
    notifications: SharedNotifications,
    sdk_binding: SdkNotificationsBinding,
) {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let child = Command::new("dbus-monitor")
        .arg("--session")
        .arg("interface='org.freedesktop.Notifications',member='Notify'")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "Foghorn: failed to start dbus-monitor: {}. Desktop → phone notifications disabled.",
                e
            );
            return;
        }
    };

    log::debug!("Foghorn: D-Bus monitor running (dbus-monitor)");

    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::new(stdout);

    let mut in_notify = false;
    let mut string_index = 0;
    let mut app_name = String::new();
    let mut app_icon = String::new();
    let mut summary = String::new();
    let mut body = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.contains("member=Notify") {
            in_notify = true;
            string_index = 0;
            app_name.clear();
            app_icon.clear();
            summary.clear();
            body.clear();
            continue;
        }

        if in_notify && let Some(value) = extract_dbus_string(&line) {
            match string_index {
                0 => app_name = value,
                1 => app_icon = value,
                2 => summary = value,
                3 => {
                    body = value;

                    if app_name == "Anchor" {
                        in_notify = false;
                        continue;
                    }

                    log::info!(
                        "Foghorn: desktop notification — {} : {} — {}",
                        app_name,
                        summary,
                        body
                    );

                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();

                    push_notification(
                        &notifications,
                        NotificationEntry {
                            source: "desktop".to_string(),
                            app_name: app_name.clone(),
                            title: summary.clone(),
                            body: body.clone(),
                            timestamp: ts,
                            app_package: String::new(),
                            icon_name: if app_icon.is_empty()
                                || super::system_icon_path(&app_icon).is_none()
                            {
                                app_name.to_ascii_lowercase()
                            } else {
                                app_icon.clone()
                            },
                        },
                    );

                    let mut delivered = false;
                    for (_, sender) in sdk_binding.senders() {
                        delivered |= sender.send_posted(
                            app_name.clone(),
                            summary.clone(),
                            body.clone(),
                            ts.saturating_mul(1000),
                        );
                    }
                    if delivered {
                        in_notify = false;
                        continue;
                    }

                    in_notify = false;
                }
                _ => {}
            }
            string_index += 1;
        }
    }

    log::info!("Foghorn: D-Bus monitor exited");
}

/// Returns the icon path to use for a notification.
/// If icon_base64 is provided, decodes and caches it under
/// ~/.cache/anchor/icons/<app_package>.png, then returns that path.
/// Falls back to "anchor-desktop" (the installed pixmap) on any failure.
fn resolve_icon(app_package: &str, icon_base64: Option<&str>) -> String {
    use base64::Engine;

    let fallback = "anchor-desktop".to_string();

    let Some(icon_path) = cached_icon_path(app_package) else {
        return fallback;
    };

    if icon_path.exists() {
        return icon_path.to_string_lossy().into_owned();
    }

    let b64 = match icon_base64 {
        Some(b) => b,
        None => return fallback,
    };

    let bytes = match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("Foghorn: failed to decode icon for {}: {}", app_package, e);
            return fallback;
        }
    };

    let Some(cache_dir) = icon_path.parent() else {
        return fallback;
    };
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        log::warn!("Foghorn: failed to create icon cache dir: {}", e);
        return fallback;
    }

    if let Err(e) = std::fs::write(&icon_path, &bytes) {
        log::warn!("Foghorn: failed to write icon cache for {}: {}", app_package, e);
        return fallback;
    }

    icon_path.to_string_lossy().into_owned()
}

/// Extract a string value from a dbus-monitor output line like:
///    string "Hello World"
fn extract_dbus_string(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("string \"") && trimmed.ends_with("\"") {
        let inner = &trimmed[8..trimmed.len() - 1];
        Some(inner.to_string())
    } else {
        None
    }
}
