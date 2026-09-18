//! Ordered desktop-to-device sender for the typed notification capability.

use std::collections::HashMap;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use anchor_sdk::{Capability, notifications};

enum NotificationMessage {
    Posted { application: String, title: String, body: String, posted_at_ms: u64 },
}

#[derive(Clone)]
pub struct SdkNotificationsSender {
    tx: SyncSender<NotificationMessage>,
}

impl SdkNotificationsSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel(64);
        thread::Builder::new()
            .name("anchor-sdk-notifications-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK notifications runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    let result = runtime.block_on(async {
                        match message {
                            NotificationMessage::Posted {
                                application,
                                title,
                                body,
                                posted_at_ms,
                            } => {
                                let id = format!("desktop-{posted_at_ms}");
                                let record = notifications::NotificationPosted {
                                    notification_id: id,
                                    application_id: application.clone(),
                                    application_name: application,
                                    title,
                                    body,
                                    posted_at_unix_ms: posted_at_ms,
                                    actions: Vec::new(),
                                };
                                capability
                                    .send_record(
                                        notifications::POSTED_TYPE_URL,
                                        notifications::encode_posted(record),
                                    )
                                    .await
                            }
                        }
                    });
                    if let Err(error) = result {
                        log::warn!("SDK notification send failed: {error}");
                    }
                }
            })
            .expect("SDK notification writer thread must start");
        Self { tx }
    }

    pub fn send_posted(
        &self,
        application: String,
        title: String,
        body: String,
        posted_at_ms: u64,
    ) -> bool {
        self.tx
            .try_send(NotificationMessage::Posted { application, title, body, posted_at_ms })
            .is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkNotificationsBinding {
    senders: Arc<Mutex<HashMap<String, SdkNotificationsSender>>>,
}

impl SdkNotificationsBinding {
    pub fn attach(&self, device_id: String, sender: SdkNotificationsSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn senders(&self) -> Vec<(String, SdkNotificationsSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_starts_empty() {
        assert!(SdkNotificationsBinding::default().senders().is_empty());
    }
}
