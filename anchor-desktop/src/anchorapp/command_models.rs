//! User-defined commands — persisted to `~/.config/anchor/commands.json`.
//!
//! Unlike [`crate::anchorapp::settings`], commands are mutated at runtime (added,
//! edited, removed from the GUI) so they live in an `Arc<Mutex<…>>` store that is
//! re-saved on every change rather than a boot-time `OnceLock` snapshot.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A single user-defined command.
///
/// The phone only ever learns `id` / `name` / `description` / `detach` — the
/// `command` string (what actually runs in a shell) never leaves the desktop.
/// The phone triggers execution by `id` only; it can never supply a command
/// string itself.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Stable UUID. Generated on creation, used by the phone to trigger runs.
    pub id: String,
    /// Human-readable label shown in both UIs.
    pub name: String,
    /// The shell command line executed via `sh -c`.
    pub command: String,
    /// Optional longer description shown under the name.
    #[serde(default)]
    pub description: String,
    /// When true, the process is detached (fire-and-forget, survives Anchor
    /// exit) and only a `started` status is reported. When false, Anchor waits
    /// for completion and reports the exit code.
    #[serde(default)]
    pub detach: bool,
}

impl Command {
    /// Create a new command with a freshly generated UUID.
    pub fn new(name: String, command: String, description: String, detach: bool) -> Self {
        Command { id: uuid::Uuid::new_v4().to_string(), name, command, description, detach }
    }
}

/// Runtime-mutable, thread-shared command list.
pub type SharedCommands = Arc<Mutex<Vec<Command>>>;

fn commands_path() -> Result<PathBuf, String> {
    let dir = dirs::config_dir().ok_or("Could not determine config directory")?.join("anchor");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    }
    Ok(dir.join("commands.json"))
}

/// Load the command list from disk. A missing or unreadable/corrupt file yields
/// an empty list rather than an error — the feature degrades to "no commands".
pub fn load_commands() -> Vec<Command> {
    let path = match commands_path() {
        Ok(p) => p,
        Err(e) => {
            log::error!("Commands: cannot resolve path: {} — using empty list", e);
            return Vec::new();
        }
    };
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<Vec<Command>>(&content) {
            Ok(cmds) => {
                log::info!("Commands: loaded {} from {:?}", cmds.len(), path);
                cmds
            }
            Err(e) => {
                log::warn!("Commands: failed to parse {:?}: {} — using empty list", path, e);
                Vec::new()
            }
        },
        Err(e) => {
            log::warn!("Commands: failed to read {:?}: {} — using empty list", path, e);
            Vec::new()
        }
    }
}

/// Persist the command list to disk (pretty-printed JSON).
pub fn save_commands(commands: &[Command]) -> Result<(), String> {
    let path = commands_path()?;
    let content =
        serde_json::to_string_pretty(commands).map_err(|e| format!("Serialize: {}", e))?;
    std::fs::write(&path, &content).map_err(|e| format!("Write: {}", e))?;
    log::info!("Commands: saved {} to {:?}", commands.len(), path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_generates_unique_ids() {
        let a = Command::new("a".into(), "true".into(), String::new(), false);
        let b = Command::new("b".into(), "true".into(), String::new(), false);
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 36); // UUID v4 hyphenated
    }

    #[test]
    fn json_roundtrip_preserves_fields() {
        let cmd = Command::new(
            "Lock".into(),
            "loginctl lock-session".into(),
            "Lock the screen".into(),
            true,
        );
        let json = serde_json::to_string(&cmd).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }

    #[test]
    fn missing_optional_fields_default() {
        // description + detach omitted — should default to "" / false.
        let json = r#"{"id":"x","name":"n","command":"true"}"#;
        let cmd: Command = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.description, "");
        assert!(!cmd.detach);
    }

    #[test]
    fn list_roundtrip() {
        let list = vec![
            Command::new("a".into(), "true".into(), String::new(), false),
            Command::new("b".into(), "false".into(), "desc".into(), true),
        ];
        let json = serde_json::to_string_pretty(&list).unwrap();
        let back: Vec<Command> = serde_json::from_str(&json).unwrap();
        assert_eq!(list, back);
    }

    #[test]
    fn parse_garbage_is_error() {
        assert!(serde_json::from_str::<Vec<Command>>("not json").is_err());
    }
}
