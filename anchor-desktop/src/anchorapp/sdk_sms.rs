//! Desktop-side bridge for typed SMS sends on `org.anchor.sms@1`.
//!
//! The UI and SMS plugin still speak the desktop broker's local JSON
//! vocabulary. This adapter is the narrow boundary that translates outbound
//! operations into protobuf records; no JSON device transport exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anchor_sdk::{Capability, sms};
use uuid::Uuid;

#[derive(Clone)]
pub struct SdkSmsSender {
    tx: mpsc::SyncSender<SmsSendMessage>,
}

enum SmsSendMessage {
    Send { client_message_id: String, address: String, body: String },
    ConversationList,
    Conversation { conversation_id: i64, before_timestamp_ms: i64, limit: u32 },
    Attachment { part_id: i64, transfer_id: String },
}

impl SdkSmsSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel::<SmsSendMessage>(16);
        thread::Builder::new()
            .name("anchor-sdk-sms-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK SMS runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    let (type_url, payload) = match message {
                        SmsSendMessage::Send { client_message_id, address, body } => (
                            sms::SEND_TYPE_URL,
                            sms::encode_send(sms::SmsSend { client_message_id, address, body }),
                        ),
                        SmsSendMessage::ConversationList => (
                            sms::CONVERSATION_LIST_REQUEST_TYPE_URL,
                            sms::encode_conversation_list_request(sms::ConversationListRequest {}),
                        ),
                        SmsSendMessage::Conversation {
                            conversation_id,
                            before_timestamp_ms,
                            limit,
                        } => (
                            sms::CONVERSATION_REQUEST_TYPE_URL,
                            sms::encode_conversation_request(sms::ConversationRequest {
                                conversation_id,
                                before_timestamp_unix_ms: before_timestamp_ms,
                                limit,
                            }),
                        ),
                        SmsSendMessage::Attachment { part_id, transfer_id } => (
                            sms::ATTACHMENT_REQUEST_TYPE_URL,
                            sms::encode_attachment_request(sms::AttachmentRequest {
                                part_id,
                                transfer_id,
                            }),
                        ),
                    };
                    if let Err(error) = runtime.block_on(capability.send_record(type_url, payload))
                    {
                        log::warn!("SDK SMS send failed: {error}");
                    } else {
                        log::info!("SMS: sent typed SDK request over QUIC");
                    }
                }
            })
            .expect("SDK SMS writer thread must start");
        Self { tx }
    }

    pub fn send(&self, address: String, body: String) -> bool {
        self.tx
            .try_send(SmsSendMessage::Send {
                client_message_id: Uuid::new_v4().to_string(),
                address,
                body,
            })
            .is_ok()
    }

    pub fn request_conversation_list(&self) -> bool {
        self.tx.try_send(SmsSendMessage::ConversationList).is_ok()
    }

    pub fn request_conversation(
        &self,
        conversation_id: i64,
        before_timestamp_ms: i64,
        limit: u32,
    ) -> bool {
        self.tx
            .try_send(SmsSendMessage::Conversation {
                conversation_id,
                before_timestamp_ms,
                limit: limit.clamp(1, 250),
            })
            .is_ok()
    }

    pub fn request_attachment(&self, part_id: i64, transfer_id: String) -> bool {
        self.tx.try_send(SmsSendMessage::Attachment { part_id, transfer_id }).is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkSmsBinding {
    senders: Arc<Mutex<HashMap<String, SdkSmsSender>>>,
}

impl SdkSmsBinding {
    pub fn attach(&self, device_id: String, sender: SdkSmsSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn sender(&self, device_id: &str) -> Option<SdkSmsSender> {
        self.senders.lock().unwrap().get(device_id).cloned()
    }

    pub fn senders(&self) -> Vec<(String, SdkSmsSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }

    pub fn has_sender(&self, device_id: &str) -> bool {
        self.senders.lock().unwrap().contains_key(device_id)
    }
}

/// Translate local UI messages into the typed SMS record family.
pub fn send_json(sender: &SdkSmsSender, value: &serde_json::Value) -> bool {
    if value.get("plugin_id").and_then(serde_json::Value::as_str) != Some("smsplugin") {
        return false;
    }
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("anchor.sms.request_conversations") => return sender.request_conversation_list(),
        Some("anchor.sms.request_conversation") => {
            let Some(conversation_id) = value.get("threadID").and_then(serde_json::Value::as_i64)
            else {
                return false;
            };
            let before =
                value.get("rangeStartTimestamp").and_then(serde_json::Value::as_i64).unwrap_or(-1);
            let limit =
                value.get("numberToRequest").and_then(serde_json::Value::as_u64).unwrap_or(25)
                    as u32;
            return sender.request_conversation(conversation_id, before, limit);
        }
        Some("anchor.sms.request") => {}
        Some("anchor.sms.request_attachment") => {
            let Some(part_id) = value.get("part_id").and_then(serde_json::Value::as_i64) else {
                return false;
            };
            let Some(transfer_id) =
                value.get("unique_identifier").and_then(serde_json::Value::as_str)
            else {
                return false;
            };
            return sender.request_attachment(part_id, transfer_id.to_owned());
        }
        _ => return false,
    }
    let body =
        value.get("messageBody").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
    let Some(addresses) = value.get("addresses").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let mut sent = false;
    for address in addresses.iter().filter_map(|entry| {
        entry
            .get("address")
            .and_then(serde_json::Value::as_str)
            .filter(|address| !address.is_empty())
    }) {
        sent |= sender.send(address.to_owned(), body.clone());
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_payload_is_not_claimed() {
        let value = serde_json::json!({"plugin_id":"commands", "type":"list"});
        // No sender is needed because validation rejects it first.
        let (tx, _rx) = mpsc::sync_channel(1);
        let sender = SdkSmsSender { tx };
        assert!(!send_json(&sender, &value));
    }

    #[test]
    fn binding_is_empty_and_device_scoped() {
        let binding = SdkSmsBinding::default();
        assert!(!binding.has_sender("phone"));
        binding.detach("phone");
        assert!(binding.sender("phone").is_none());
    }

    #[test]
    fn send_payload_enqueues_one_record_per_recipient() {
        let (tx, rx) = mpsc::sync_channel::<SmsSendMessage>(4);
        let sender = SdkSmsSender { tx };
        let value = serde_json::json!({
            "plugin_id":"smsplugin", "type":"anchor.sms.request",
            "messageBody":"hello", "addresses":[{"address":"+1"},{"address":"+2"}]
        });
        assert!(send_json(&sender, &value));
        assert_eq!(rx.try_iter().count(), 2);
    }
}
