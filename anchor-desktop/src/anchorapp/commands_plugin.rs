//! Commands plugin — runs user-defined commands requested by the phone.
//!
//! Security model: the phone can only ever trigger a command by its `id`. The
//! command string itself is defined on the desktop and is never accepted from —
//! nor sent to — the phone. The phone receives `id` / `name` / `description` /
//! `detach` so it can render a grid and request runs.
//!
//! Wire protocol (`plugin_id: "commands"`):
//!
//! Phone → desktop:
//! * `{"type":"run_command","id":"<cmd id>"}`
//! * `{"type":"kill_command","exec_id":"<exec id>"}`
//! * `{"type":"list"}` — request a fresh `command_list`
//! * `{"type":"device_connected"}` — broadcast on connect; triggers a `command_list`
//!
//! Desktop → phone:
//! * `{"type":"command_list","commands":[{"id","name","description","detach"}]}`
//! * `{"type":"command_result","id","exec_id","status":"started|done|failed","exit_code"?,"error"?}`

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::anchorapp::command_models::{Command, SharedCommands, load_commands};
use crate::anchorapp::command_runner::{CommandRunner, RunStatus};
use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};

pub struct CommandsPlugin {
    plugin_rx: Option<Receiver<AnchorEvent>>,
    broker_tx: Option<Sender<AnchorEvent>>,
    /// Shared with the GUI so edits in the management grid are seen here.
    pub commands: SharedCommands,
    runner: CommandRunner,
}

impl Plugin for CommandsPlugin {
    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        CommandsPlugin {
            plugin_rx,
            broker_tx,
            commands: Arc::new(Mutex::new(load_commands())),
            runner: CommandRunner::new(),
        }
    }

    fn run(&mut self) -> Option<JoinHandle<()>> {
        let rx = self.plugin_rx.take().expect("commands plugin_rx not set");
        let broker_tx = self.broker_tx.clone().expect("commands broker_tx not set");
        let commands = self.commands.clone();
        let runner = self.runner.clone();

        let handle = std::thread::Builder::new()
            .name("commands-plugin".to_string())
            .spawn(move || {
                log::debug!("Commands plugin started");
                for event in rx.iter() {
                    let json_str = match &event.message {
                        AnchorMessage::Json(s) => s.clone(),
                        _ => continue,
                    };
                    let payload: serde_json::Value = match serde_json::from_str(&json_str) {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("Commands: invalid JSON: {}", e);
                            continue;
                        }
                    };
                    handle_message(&payload, &commands, &runner, &broker_tx);
                }
                log::info!("Commands plugin exiting");
            })
            .expect("Failed to spawn commands plugin thread");

        Some(handle)
    }
}

fn handle_message(
    payload: &serde_json::Value,
    commands: &SharedCommands,
    runner: &CommandRunner,
    broker_tx: &Sender<AnchorEvent>,
) {
    match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "device_connected" | "list" | "request_command_list" => {
            send_command_list(commands, broker_tx);
        }
        "run_command" => {
            let id = match payload.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => {
                    log::warn!("Commands: run_command missing id");
                    return;
                }
            };
            // Stamped by route_message; absent for desktop-initiated runs.
            let source =
                payload.get("_source_device").and_then(|v| v.as_str()).map(|s| s.to_string());
            run_by_id(id, source, commands, runner, broker_tx);
        }
        "kill_command" => {
            if let Some(exec_id) = payload.get("exec_id").and_then(|v| v.as_str())
                && !runner.kill(exec_id)
            {
                log::debug!("Commands: kill for unknown exec_id {}", exec_id);
            }
        }
        other => {
            log::debug!("Commands: unhandled type {}", other);
        }
    }
}

/// Look up a command by id and execute it, streaming results back to the phone
/// that asked for it (`source`) or to the desktop GUI for a local run.
fn run_by_id(
    id: &str,
    source: Option<String>,
    commands: &SharedCommands,
    runner: &CommandRunner,
    broker_tx: &Sender<AnchorEvent>,
) {
    let cmd = {
        let guard = match commands.lock() {
            Ok(g) => g,
            Err(_) => {
                log::error!("Commands: lock poisoned, cannot run");
                return;
            }
        };
        guard.iter().find(|c| c.id == id).cloned()
    };

    let cmd = match cmd {
        Some(c) => c,
        None => {
            log::warn!("Commands: phone requested unknown command id {}", id);
            send_result(
                broker_tx,
                source.as_deref(),
                id,
                "",
                "failed",
                None,
                Some("unknown command"),
            );
            return;
        }
    };

    let cmd_id = cmd.id.clone();
    let tx = broker_tx.clone();
    runner.run(&cmd.command, cmd.detach, move |exec_id, status| {
        let (wire_status, exit_code, error) = match status {
            RunStatus::Started => ("started", None, None),
            RunStatus::Finished { exit_code: 0 } => ("done", Some(0), None),
            RunStatus::Finished { exit_code } => ("failed", Some(exit_code), None),
            RunStatus::Failed { error } => ("failed", None, Some(error)),
        };
        send_result(
            &tx,
            source.as_deref(),
            &cmd_id,
            exec_id,
            wire_status,
            exit_code,
            error.as_deref(),
        );
    });
}

/// Build a phone-facing view of a command (no `command` string).
fn list_entry(c: &Command) -> serde_json::Value {
    serde_json::json!({
        "id": c.id,
        "name": c.name,
        "description": c.description,
        "detach": c.detach,
    })
}

pub(crate) fn send_command_list(commands: &SharedCommands, broker_tx: &Sender<AnchorEvent>) {
    let entries: Vec<serde_json::Value> = match commands.lock() {
        Ok(g) => g.iter().map(list_entry).collect(),
        Err(_) => {
            log::error!("Commands: lock poisoned, cannot send list");
            return;
        }
    };
    let packet = serde_json::json!({
        "plugin_id": "commands",
        "type": "command_list",
        "commands": entries,
    });
    let _ = broker_tx.send(AnchorEvent {
        target: AnchorTarget::Device,
        message: AnchorMessage::Json(packet.to_string()),
    });
}

/// Send a command result back to the requesting phone, or to the desktop GUI
/// when the run originated locally.
fn send_result(
    broker_tx: &Sender<AnchorEvent>,
    source: Option<&str>,
    cmd_id: &str,
    exec_id: &str,
    status: &str,
    exit_code: Option<i32>,
    error: Option<&str>,
) {
    let mut packet = serde_json::json!({
        "plugin_id": "commands",
        "type": "command_result",
        "id": cmd_id,
        "exec_id": exec_id,
        "status": status,
    });
    if let Some(code) = exit_code {
        packet["exit_code"] = serde_json::json!(code);
    }
    if let Some(err) = error {
        packet["error"] = serde_json::json!(err);
    }
    let target = source
        .map(|source| AnchorTarget::DeviceId(source.to_string()))
        .unwrap_or(AnchorTarget::Gui);
    let _ =
        broker_tx.send(AnchorEvent { target, message: AnchorMessage::Json(packet.to_string()) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn shared(cmds: Vec<Command>) -> SharedCommands {
        Arc::new(Mutex::new(cmds))
    }

    fn recv_json(rx: &mpsc::Receiver<AnchorEvent>) -> serde_json::Value {
        recv_event(rx).1
    }

    fn recv_event(rx: &mpsc::Receiver<AnchorEvent>) -> (AnchorTarget, serde_json::Value) {
        let ev = rx.recv_timeout(Duration::from_secs(5)).expect("expected an event");
        match ev.message {
            AnchorMessage::Json(s) => (ev.target, serde_json::from_str(&s).unwrap()),
            _ => panic!("expected JSON message"),
        }
    }

    #[test]
    fn list_omits_command_string() {
        let cmds = shared(vec![Command::new(
            "Lock".into(),
            "loginctl lock-session".into(),
            "Lock screen".into(),
            false,
        )]);
        let (tx, rx) = mpsc::channel();
        send_command_list(&cmds, &tx);

        let v = recv_json(&rx);
        assert_eq!(v["type"], "command_list");
        let entry = &v["commands"][0];
        assert_eq!(entry["name"], "Lock");
        assert_eq!(entry["description"], "Lock screen");
        // The actual command string must never be sent to the phone.
        assert!(entry.get("command").is_none());
    }

    #[test]
    fn run_unknown_id_reports_failed() {
        let cmds = shared(vec![]);
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();

        let msg = serde_json::json!({
            "type":"run_command", "id":"nope", "_source_device":"phone-1"
        });
        handle_message(&msg, &cmds, &runner, &tx);

        let (target, v) = recv_event(&rx);
        assert_eq!(target, AnchorTarget::DeviceId("phone-1".to_string()));
        assert_eq!(v["type"], "command_result");
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"], "unknown command");
    }

    #[test]
    fn desktop_initiated_run_reports_to_gui() {
        let cmd = Command::new("Ok".into(), "true".into(), String::new(), false);
        let id = cmd.id.clone();
        let cmds = shared(vec![cmd]);
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();

        handle_message(&serde_json::json!({"type":"run_command","id": id}), &cmds, &runner, &tx);

        let (target, started) = recv_event(&rx);
        assert_eq!(target, AnchorTarget::Gui);
        assert_eq!(started["status"], "started");
        assert_eq!(started["id"], id);
    }

    #[test]
    fn run_known_id_reports_started_then_done() {
        let cmd = Command::new("Ok".into(), "true".into(), String::new(), false);
        let id = cmd.id.clone();
        let cmds = shared(vec![cmd]);
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();

        let msg = serde_json::json!({
            "type":"run_command", "id": id, "_source_device":"phone-1"
        });
        handle_message(&msg, &cmds, &runner, &tx);

        let (target, started) = recv_event(&rx);
        // Results are addressed to the requesting phone, not broadcast.
        assert_eq!(target, AnchorTarget::DeviceId("phone-1".to_string()));
        assert_eq!(started["status"], "started");
        assert_eq!(started["id"], id);

        let done = recv_json(&rx);
        assert_eq!(done["status"], "done");
        assert_eq!(done["exit_code"], 0);
    }

    #[test]
    fn run_failing_command_reports_failed_with_code() {
        let cmd = Command::new("Bad".into(), "exit 7".into(), String::new(), false);
        let id = cmd.id.clone();
        let cmds = shared(vec![cmd]);
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();

        handle_message(
            &serde_json::json!({"type":"run_command","id": id, "_source_device":"phone-1"}),
            &cmds,
            &runner,
            &tx,
        );

        let _started = recv_json(&rx);
        let done = recv_json(&rx);
        assert_eq!(done["status"], "failed");
        assert_eq!(done["exit_code"], 7);
    }

    #[test]
    fn device_connected_triggers_list() {
        let cmds = shared(vec![Command::new("A".into(), "true".into(), String::new(), false)]);
        let runner = CommandRunner::new();
        let (tx, rx) = mpsc::channel();

        handle_message(&serde_json::json!({"type":"device_connected"}), &cmds, &runner, &tx);
        let v = recv_json(&rx);
        assert_eq!(v["type"], "command_list");
        assert_eq!(v["commands"][0]["name"], "A");
    }
}
