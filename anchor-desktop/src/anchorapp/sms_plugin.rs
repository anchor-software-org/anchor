use std::{
    sync::mpsc::{Receiver, Sender},
    sync::{Arc, Mutex},
    thread,
    thread::JoinHandle,
};

use rusqlite::Connection;

use crate::anchorapp::{
    event::{AnchorEvent, AnchorMessage, AnchorTarget},
    plugin::{Plugin, SharedFrameBuffer},
    sms_db,
    sms_models::{SmsMessage, SmsSendRequest, SmsThread},
};

pub type SharedSmsThreads = Arc<Mutex<Vec<SmsThread>>>;

pub struct AnchorPluginSMS {
    plugin_rx: Option<Receiver<AnchorEvent>>,
    broker_tx: Option<Sender<AnchorEvent>>,
    conn: Option<Connection>,
    pub sms_threads: SharedSmsThreads,
}

impl Plugin for AnchorPluginSMS {
    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        _frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        let db_path =
            dirs::data_local_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("anchor");
        let _ = std::fs::create_dir_all(&db_path);
        let db_path = db_path.join("sms.db");
        log::info!("SMS: opening database at {}", db_path.display());
        let conn = match Connection::open(&db_path) {
            Ok(c) => {
                if let Err(e) = sms_db::init_schema(&c) {
                    log::error!("SMS: failed to init schema: {}", e);
                    None
                } else {
                    log::info!("SMS: database ready");
                    Some(c)
                }
            }
            Err(e) => {
                log::error!("SMS: failed to open database: {}", e);
                None
            }
        };

        AnchorPluginSMS {
            plugin_rx,
            broker_tx,
            conn,
            sms_threads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn run(&mut self) -> Option<JoinHandle<()>> {
        let rx = self.plugin_rx.take().expect("sms plugin_rx not set");
        let broker_tx = self.broker_tx.take().expect("sms broker_tx not set");
        let conn = self.conn.take();
        let sms_threads = self.sms_threads.clone();

        let handle = thread::Builder::new()
            .name("sms-plugin".to_string())
            .spawn(move || {
                log::debug!("SMS plugin thread started");

                let conn = match conn {
                    Some(c) => c,
                    None => {
                        log::error!("SMS plugin: no database connection, exiting");
                        return;
                    }
                };

                // Seed the shared thread list from whatever is already cached locally.
                refresh_threads(&conn, &sms_threads);

                for event in rx.iter() {
                    let json_str = match &event.message {
                        AnchorMessage::Json(s) => s.clone(),
                        _ => continue,
                    };

                    let payload: serde_json::Value = match serde_json::from_str(&json_str) {
                        Ok(v) => v,
                        Err(e) => {
                            log::warn!("SMS: invalid JSON: {}", e);
                            continue;
                        }
                    };

                    match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        // A device just connected — now it's safe to request messages.
                        "device_connected" => {
                            log::info!("SMS: device connected, requesting all conversations");
                            request_all_conversations(&broker_tx);
                        }
                        "anchor.sms.messages" => {
                            handle_incoming_messages(&payload, &conn, &broker_tx, &sms_threads);
                        }
                        "sms.send" => {
                            handle_send_request(&payload, &broker_tx);
                        }
                        "sms.request_conversation" => {
                            handle_request_conversation(&payload, &conn, &broker_tx);
                        }
                        "sms.request_more_history" => {
                            handle_request_more_history(&payload, &broker_tx);
                        }
                        "sms.request_attachment" => {
                            handle_request_attachment(&payload, &broker_tx);
                        }
                        "anchor.sms.clear_local" => {
                            let attachment_paths =
                                sms_db::attachment_local_paths(&conn).unwrap_or_default();
                            match sms_db::clear_all(&conn) {
                                Ok(()) => {
                                    for path in attachment_paths {
                                        let _ = std::fs::remove_file(path);
                                    }
                                    sms_threads.lock().unwrap().clear();
                                    broker_tx
                                        .send(AnchorEvent {
                                            target: AnchorTarget::Gui,
                                            message: AnchorMessage::Json(
                                                serde_json::json!({
                                                    "type": "sms.local_cache_cleared"
                                                })
                                                .to_string(),
                                            ),
                                        })
                                        .ok();
                                    log::info!("SMS: cleared local message cache");
                                }
                                Err(error) => {
                                    log::error!("SMS: could not clear local cache: {error}");
                                }
                            }
                        }
                        other => {
                            log::debug!("SMS: unhandled packet type: {}", other);
                        }
                    }
                }

                log::info!("SMS plugin thread exiting");
            })
            .expect("failed to spawn sms plugin thread");

        Some(handle)
    }
}

// -- Handlers --

fn handle_incoming_messages(
    payload: &serde_json::Value,
    conn: &Connection,
    broker_tx: &Sender<AnchorEvent>,
    sms_threads: &SharedSmsThreads,
) {
    let messages_json = match payload.get("messages").and_then(|m| m.as_array()) {
        Some(arr) => arr,
        None => {
            log::warn!("SMS: messages packet missing 'messages' array");
            return;
        }
    };

    let messages: Vec<SmsMessage> =
        messages_json.iter().filter_map(SmsMessage::from_json).collect();

    if messages.is_empty() {
        // Send an empty update so the GUI can clear any loading flags.
        broker_tx
            .send(AnchorEvent {
                target: AnchorTarget::Gui,
                message: AnchorMessage::Json(
                    serde_json::json!({
                        "type": "sms.messages_updated",
                        "thread_ids": [],
                    })
                    .to_string(),
                ),
            })
            .ok();
        return;
    }

    let thread_ids: Vec<i64> = {
        let s: std::collections::HashSet<i64> = messages.iter().map(|m| m.thread_id).collect();
        s.into_iter().collect()
    };
    log::info!("SMS: received {} message(s) across {} thread(s)", messages.len(), thread_ids.len());

    if let Err(e) = sms_db::insert_messages(conn, &messages) {
        log::error!("SMS: failed to store messages: {}", e);
        return;
    }

    refresh_threads(conn, sms_threads);

    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Gui,
            message: AnchorMessage::Json(
                serde_json::json!({
                    "type": "sms.messages_updated",
                    "thread_ids": thread_ids,
                })
                .to_string(),
            ),
        })
        .ok();

    // Push conversation data directly so the GUI doesn't need to re-request.
    for &tid in &thread_ids {
        send_conversation_to_gui(conn, broker_tx, tid);
    }
}

fn handle_send_request(payload: &serde_json::Value, broker_tx: &Sender<AnchorEvent>) {
    let request: SmsSendRequest = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("SMS: malformed send request: {} — payload: {}", e, payload);
            return;
        }
    };

    if request.addresses.is_empty() {
        log::warn!("SMS: send request has no addresses, dropping");
        return;
    }

    let packet = request.to_packet_json();
    log::info!("SMS: dispatching send to {} recipient(s)", request.addresses.len());

    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Device,
            message: AnchorMessage::Json(packet.to_string()),
        })
        .ok();
}

/// Read cached messages for a thread from DB and push them to the GUI.
fn send_conversation_to_gui(conn: &Connection, broker_tx: &Sender<AnchorEvent>, thread_id: i64) {
    match sms_db::get_messages(conn, thread_id) {
        Ok(cached) => {
            let messages_json: Vec<serde_json::Value> = cached
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "uid": m.uid,
                        "thread_id": m.thread_id,
                        "body": m.body,
                        "date": m.date,
                        "message_type": m.message_type,
                        "read": m.read,
                        "addresses": m.addresses.iter().map(|a| &a.address).collect::<Vec<_>>(),
                        "attachments": m.attachments.iter().map(|a| serde_json::json!({
                            "part_id": a.part_id,
                            "unique_identifier": a.unique_identifier,
                            "mime_type": a.mime_type,
                            "encoded_thumbnail": a.encoded_thumbnail,
                            "local_path": a.local_path,
                        })).collect::<Vec<serde_json::Value>>(),
                    })
                })
                .collect();

            broker_tx
                .send(AnchorEvent {
                    target: AnchorTarget::Gui,
                    message: AnchorMessage::Json(
                        serde_json::json!({
                            "type": "sms.conversation_messages",
                            "thread_id": thread_id,
                            "messages": messages_json,
                        })
                        .to_string(),
                    ),
                })
                .ok();
        }
        Err(e) => {
            log::error!("SMS: failed to read conversation {}: {}", thread_id, e);
        }
    }
}

fn handle_request_conversation(
    payload: &serde_json::Value,
    conn: &Connection,
    broker_tx: &Sender<AnchorEvent>,
) {
    let thread_id = match payload.get("thread_id").and_then(|t| t.as_i64()) {
        Some(id) => id,
        None => {
            log::warn!("SMS: request_conversation missing thread_id");
            return;
        }
    };

    send_conversation_to_gui(conn, broker_tx, thread_id);

    // If we have nothing cached at all, seed from the phone.
    let count = sms_db::get_messages(conn, thread_id).map(|v| v.len()).unwrap_or(0);
    if count == 0 {
        request_conversation(broker_tx, thread_id, -1, 25);
    }
}

fn handle_request_attachment(payload: &serde_json::Value, broker_tx: &Sender<AnchorEvent>) {
    let part_id = match payload.get("part_id").and_then(|p| p.as_i64()) {
        Some(id) => id,
        None => {
            log::warn!("SMS: request_attachment missing part_id");
            return;
        }
    };
    let unique_identifier = match payload.get("unique_identifier").and_then(|u| u.as_str()) {
        Some(s) => s.to_string(),
        None => {
            log::warn!("SMS: request_attachment missing unique_identifier");
            return;
        }
    };
    log::info!("SMS: requesting attachment part_id={} uid={}", part_id, unique_identifier);
    crate::anchorapp::attachment_pending::note_requested(unique_identifier.clone());
    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Device,
            message: AnchorMessage::Json(
                serde_json::json!({
                    "plugin_id": "smsplugin",
                    "type": "anchor.sms.request_attachment",
                    "part_id": part_id,
                    "unique_identifier": unique_identifier,
                })
                .to_string(),
            ),
        })
        .ok();
}

fn handle_request_more_history(payload: &serde_json::Value, broker_tx: &Sender<AnchorEvent>) {
    let thread_id = match payload.get("thread_id").and_then(|t| t.as_i64()) {
        Some(id) => id,
        None => return,
    };
    let range_start = payload.get("range_start").and_then(|r| r.as_i64()).unwrap_or(-1);
    let count = payload.get("count").and_then(|c| c.as_i64()).unwrap_or(25);

    log::info!(
        "SMS: requesting {} older messages for thread {} before {}",
        count,
        thread_id,
        range_start
    );
    request_conversation(broker_tx, thread_id, range_start, count);
}

// -- Helpers --

fn refresh_threads(conn: &Connection, sms_threads: &SharedSmsThreads) {
    match sms_db::get_threads(conn) {
        Ok(threads) => {
            *sms_threads.lock().unwrap() = threads;
        }
        Err(e) => {
            log::error!("SMS: failed to refresh thread list: {}", e);
        }
    }
}

fn request_all_conversations(broker_tx: &Sender<AnchorEvent>) {
    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Device,
            message: AnchorMessage::Json(
                serde_json::json!({
                    "plugin_id": "smsplugin",
                    "type": "anchor.sms.request_conversations",
                })
                .to_string(),
            ),
        })
        .ok();
}

fn request_conversation(
    broker_tx: &Sender<AnchorEvent>,
    thread_id: i64,
    range_start_timestamp: i64,
    number_to_request: i64,
) {
    broker_tx
        .send(AnchorEvent {
            target: AnchorTarget::Device,
            message: AnchorMessage::Json(
                serde_json::json!({
                    "plugin_id": "smsplugin",
                    "type": "anchor.sms.request_conversation",
                    "threadID": thread_id,
                    "rangeStartTimestamp": range_start_timestamp,
                    "numberToRequest": number_to_request,
                })
                .to_string(),
            ),
        })
        .ok();
}
