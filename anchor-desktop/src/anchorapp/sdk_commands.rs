//! Desktop bridge for typed command execution results.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anchor_sdk::{Capability, commands};

#[derive(Clone)]
pub struct SdkCommandsSender {
    tx: mpsc::SyncSender<CommandMessage>,
}

struct CommandResultMessage {
    command_id: String,
    execution_id: String,
    status: String,
    exit_code: i32,
    error: String,
}

enum CommandMessage {
    Result(CommandResultMessage),
    List(Vec<commands::CommandDefinition>),
}

impl SdkCommandsSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel::<CommandMessage>(32);
        thread::Builder::new()
            .name("anchor-sdk-commands-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK commands runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    let (type_url, payload) = match message {
                        CommandMessage::Result(result) => (
                            commands::RESULT_TYPE_URL,
                            commands::encode_result(commands::CommandResult {
                                command_id: result.command_id,
                                execution_id: result.execution_id,
                                status: result.status,
                                exit_code: result.exit_code,
                                error: result.error,
                                stdout: String::new(),
                                stderr: String::new(),
                            }),
                        ),
                        CommandMessage::List(list) => (
                            commands::LIST_TYPE_URL,
                            commands::encode_list(commands::CommandList { commands: list }),
                        ),
                    };
                    if let Err(error) = runtime.block_on(capability.send_record(type_url, payload))
                    {
                        log::warn!("SDK command result send failed: {error}");
                    } else {
                        log::info!("Commands: sent typed SDK record over QUIC");
                    }
                }
            })
            .expect("SDK commands writer thread must start");
        Self { tx }
    }

    pub fn send_result(
        &self,
        command_id: String,
        execution_id: String,
        status: String,
        exit_code: i32,
        error: String,
    ) -> bool {
        self.tx
            .try_send(CommandMessage::Result(CommandResultMessage {
                command_id,
                execution_id,
                status,
                exit_code,
                error,
            }))
            .is_ok()
    }

    pub fn send_list(&self, commands: Vec<commands::CommandDefinition>) -> bool {
        self.tx.try_send(CommandMessage::List(commands)).is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkCommandsBinding {
    senders: Arc<Mutex<HashMap<String, SdkCommandsSender>>>,
}

impl SdkCommandsBinding {
    pub fn attach(&self, device_id: String, sender: SdkCommandsSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn sender(&self, device_id: &str) -> Option<SdkCommandsSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn senders(&self) -> Vec<(String, SdkCommandsSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }
}

/// Translate a desktop-local command envelope into a typed record.
pub fn send_json(sender: &SdkCommandsSender, value: &serde_json::Value) -> bool {
    if value.get("plugin_id").and_then(serde_json::Value::as_str) != Some("commands") {
        return false;
    }
    if value.get("type").and_then(serde_json::Value::as_str) == Some("command_list") {
        let Some(entries) = value.get("commands").and_then(serde_json::Value::as_array) else {
            return false;
        };
        let commands = entries
            .iter()
            .filter_map(|entry| {
                Some(commands::CommandDefinition {
                    id: entry.get("id")?.as_str()?.to_owned(),
                    name: entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: entry
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    detach: entry
                        .get("detach")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect();
        return sender.send_list(commands);
    }
    if value.get("type").and_then(serde_json::Value::as_str) != Some("command_result") {
        return false;
    }
    let Some(command_id) = value.get("id").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let status = value.get("status").and_then(serde_json::Value::as_str).unwrap_or("failed");
    let execution_id = value.get("exec_id").and_then(serde_json::Value::as_str).unwrap_or_default();
    let exit_code =
        value.get("exit_code").and_then(serde_json::Value::as_i64).unwrap_or_default() as i32;
    let error = value.get("error").and_then(serde_json::Value::as_str).unwrap_or_default();
    sender.send_result(
        command_id.to_owned(),
        execution_id.to_owned(),
        status.to_owned(),
        exit_code,
        error.to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_list_is_not_claimed() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let sender = SdkCommandsSender { tx };
        assert!(!send_json(
            &sender,
            &serde_json::json!({"plugin_id":"commands", "type":"command_list"})
        ));
    }
}
