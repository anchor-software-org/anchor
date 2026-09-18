use serde::{Deserialize, Serialize};

/// A phone number or email address participating in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmsAddress {
    pub address: String,
    /// Display name from the phone's contacts database, if resolved.
    #[serde(default)]
    pub name: Option<String>,
    /// Base64-encoded JPEG thumbnail (~64×64) of the contact photo, if available.
    #[serde(default)]
    pub photo: Option<String>,
}

/// MMS attachment metadata. The actual file lives on disk; `local_path` is
/// None until the file transfer from Android completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsAttachment {
    pub part_id: i64,
    pub mime_type: String,
    /// Base64-encoded thumbnail preview, if provided by Android.
    pub encoded_thumbnail: Option<String>,
    /// Unique filename used as the cache key.
    pub unique_identifier: String,
    /// Populated once the file has been downloaded to the local cache dir.
    pub local_path: Option<String>,
}

/// A single SMS or MMS message, mirroring the anchor.sms.messages schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsMessage {
    /// Android's message UID — used for deduplication.
    pub uid: i64,
    pub thread_id: i64,
    /// Bitfield of event flags (see Android Telephony.TextBasedSmsColumns).
    pub event: i32,
    pub body: Option<String>,
    pub addresses: Vec<SmsAddress>,
    /// Millisecond epoch timestamp.
    pub date: i64,
    /// Android MESSAGE_TYPE_* (1 = inbox, 2 = sent, etc.)
    pub message_type: i32,
    pub read: bool,
    /// Android subscriber ID — identifies which SIM card the message belongs to.
    pub sub_id: Option<i64>,
    pub attachments: Vec<SmsAttachment>,
}

/// A conversation thread — one row per thread_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsThread {
    pub thread_id: i64,
    /// Epoch ms of the most recent message, for sorting.
    pub last_updated: i64,
    /// Addresses of the participants (excluding the user's own number).
    pub addresses: Vec<SmsAddress>,
    /// Preview of the most recent message body.
    pub snippet: Option<String>,
}

/// Outbound send request — becomes a anchor.sms.request packet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsSendRequest {
    pub addresses: Vec<SmsAddress>,
    pub message_body: Option<String>,
    #[serde(default)]
    pub attachments: Vec<SmsSendAttachment>,
    pub sub_id: Option<i64>,
    pub thread_id: Option<i64>,
}

/// Attachment payload for outbound MMS — file bytes are base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmsSendAttachment {
    pub file_name: String,
    pub base64_encoded_file: String,
    pub mime_type: String,
}

// -- JSON parsing helpers --

impl SmsMessage {
    /// Parse a message object from the `messages` array inside a
    /// anchor.sms.messages packet body.
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let uid = v.get("event").and_then(|e| e.as_i64()).unwrap_or(0);

        // Android uses "_id" as the stable UID in some versions; fall back to
        // a hash of (thread_id, date) if absent.
        let uid = v.get("_id").and_then(|id| id.as_i64()).unwrap_or(uid);

        let thread_id = v.get("thread_id")?.as_i64()?;
        let date = v.get("date")?.as_i64()?;
        let event = v.get("event").and_then(|e| e.as_i64()).unwrap_or(0) as i32;
        let body = v.get("body").and_then(|b| b.as_str()).map(str::to_string);
        let message_type = v.get("type").and_then(|t| t.as_i64()).unwrap_or(0) as i32;
        let read = v.get("read").and_then(|r| r.as_bool()).unwrap_or(false);
        let sub_id = v.get("sub_id").and_then(|s| s.as_i64());

        let addresses = v
            .get("addresses")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let address = entry.get("address").and_then(|a| a.as_str())?.to_string();
                        let name = entry.get("name").and_then(|n| n.as_str()).map(str::to_string);
                        let photo = entry.get("photo").and_then(|p| p.as_str()).map(str::to_string);
                        Some(SmsAddress { address, name, photo })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let attachments = v
            .get("attachments")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(SmsAttachment::from_json).collect())
            .unwrap_or_default();

        Some(SmsMessage {
            uid,
            thread_id,
            event,
            body,
            addresses,
            date,
            message_type,
            read,
            sub_id,
            attachments,
        })
    }
}

impl SmsAttachment {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        let part_id = v.get("part_id")?.as_i64()?;
        let mime_type = v.get("mime_type")?.as_str()?.to_string();
        let unique_identifier = v.get("unique_identifier")?.as_str()?.to_string();
        let encoded_thumbnail =
            v.get("encoded_thumbnail").and_then(|t| t.as_str()).map(str::to_string);

        Some(SmsAttachment {
            part_id,
            mime_type,
            encoded_thumbnail,
            unique_identifier,
            local_path: None,
        })
    }
}

impl SmsSendRequest {
    /// Serialize to a anchor.sms.request packet body.
    pub fn to_packet_json(&self) -> serde_json::Value {
        let addresses: Vec<serde_json::Value> =
            self.addresses.iter().map(|a| serde_json::json!({ "address": a.address })).collect();

        let mut packet = serde_json::json!({
            "plugin_id": "smsplugin",
            "type": "anchor.sms.request",
            "version": 2,
            "addresses": addresses,
        });

        if let Some(ref body) = self.message_body {
            packet["messageBody"] = serde_json::Value::String(body.clone());
        }

        if let Some(sub_id) = self.sub_id {
            packet["sub_id"] = serde_json::Value::Number(sub_id.into());
        }

        if let Some(thread_id) = self.thread_id {
            packet["threadID"] = serde_json::Value::Number(thread_id.into());
        }

        if !self.attachments.is_empty() {
            let attachments: Vec<serde_json::Value> = self
                .attachments
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "fileName": a.file_name,
                        "base64EncodedFile": a.base64_encoded_file,
                        "mimeType": a.mime_type,
                    })
                })
                .collect();
            packet["attachments"] = serde_json::Value::Array(attachments);
        }

        packet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SmsMessage::from_json ──────────────────────────────────────────

    #[test]
    fn parse_minimal_message() {
        let json = serde_json::json!({
            "thread_id": 42,
            "date": 1700000000000_i64,
            "body": "hello world"
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.thread_id, 42);
        assert_eq!(msg.date, 1700000000000);
        assert_eq!(msg.body.as_deref(), Some("hello world"));
        assert!(msg.addresses.is_empty());
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn parse_message_with_type_inbox() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "type": 1
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.message_type, 1);
    }

    #[test]
    fn parse_message_with_contacts() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "addresses": [
                {"address": "+1234567890", "name": "Alice", "photo": "base64photo"},
                {"address": "+0987654321"}
            ]
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.addresses.len(), 2);
        assert_eq!(msg.addresses[0].address, "+1234567890");
        assert_eq!(msg.addresses[0].name.as_deref(), Some("Alice"));
        assert_eq!(msg.addresses[0].photo.as_deref(), Some("base64photo"));
        assert_eq!(msg.addresses[1].address, "+0987654321");
        assert_eq!(msg.addresses[1].name, None);
    }

    #[test]
    fn parse_message_with_attachments() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "attachments": [{
                "part_id": 5,
                "mime_type": "image/jpeg",
                "unique_identifier": "abc123",
                "encoded_thumbnail": "thumb"
            }]
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].part_id, 5);
        assert_eq!(msg.attachments[0].mime_type, "image/jpeg");
        assert_eq!(msg.attachments[0].unique_identifier, "abc123");
    }

    #[test]
    fn parse_message_prefers_id_over_event_as_uid() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "_id": 999,
            "event": 42
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.uid, 999);
    }

    #[test]
    fn parse_message_falls_back_to_event_as_uid() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "event": 77
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.uid, 77);
    }

    #[test]
    fn parse_message_sub_id() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64,
            "sub_id": 2
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert_eq!(msg.sub_id, Some(2));
    }

    #[test]
    fn parse_message_missing_required_fields_returns_none() {
        assert!(SmsMessage::from_json(&serde_json::json!({})).is_none());
        assert!(SmsMessage::from_json(&serde_json::json!({"date": 1000})).is_none());
        assert!(SmsMessage::from_json(&serde_json::json!({"thread_id": 1})).is_none());
    }

    #[test]
    fn parse_message_defaults_read_to_false() {
        let json = serde_json::json!({
            "thread_id": 1,
            "date": 1000_i64
        });
        let msg = SmsMessage::from_json(&json).expect("should parse");
        assert!(!msg.read);
    }

    // ── SmsSendRequest::to_packet_json ──────────────────────────────────

    #[test]
    fn send_request_minimal() {
        let req = SmsSendRequest {
            addresses: vec![SmsAddress { address: "+123".into(), name: None, photo: None }],
            message_body: None,
            attachments: vec![],
            sub_id: None,
            thread_id: None,
        };
        let packet = req.to_packet_json();
        assert_eq!(packet["plugin_id"], "smsplugin");
        assert_eq!(packet["type"], "anchor.sms.request");
        assert_eq!(packet["version"], 2);
        assert_eq!(packet["addresses"][0]["address"], "+123");
        assert!(packet.get("messageBody").is_none());
        assert!(packet.get("sub_id").is_none());
        assert!(packet.get("threadID").is_none());
        assert!(packet.get("attachments").is_none());
    }

    #[test]
    fn send_request_with_body_and_sub_id() {
        let req = SmsSendRequest {
            addresses: vec![SmsAddress { address: "+456".into(), name: None, photo: None }],
            message_body: Some("test message".into()),
            attachments: vec![],
            sub_id: Some(1),
            thread_id: Some(99),
        };
        let packet = req.to_packet_json();
        assert_eq!(packet["messageBody"], "test message");
        assert_eq!(packet["sub_id"], 1);
        assert_eq!(packet["threadID"], 99);
    }

    #[test]
    fn send_request_with_attachments() {
        let req = SmsSendRequest {
            addresses: vec![],
            message_body: None,
            attachments: vec![SmsSendAttachment {
                file_name: "photo.jpg".into(),
                base64_encoded_file: "AAAA".into(),
                mime_type: "image/jpeg".into(),
            }],
            sub_id: None,
            thread_id: None,
        };
        let packet = req.to_packet_json();
        let atts = packet["attachments"].as_array().expect("should be array");
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0]["fileName"], "photo.jpg");
        assert_eq!(atts[0]["base64EncodedFile"], "AAAA");
        assert_eq!(atts[0]["mimeType"], "image/jpeg");
    }
}
