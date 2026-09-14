//! Desktop-side bridge from the clipboard plugin to an Anchor SDK capability.
//!
//! The clipboard watcher is synchronous/threaded, while the Rust SDK session
//! is asynchronous. This bounded bridge keeps those concerns separate: the
//! watcher never blocks on Quinn and a full queue drops new updates.

use std::sync::mpsc::{self, SyncSender};
use std::thread;

use anchor_sdk::{Capability, clipboard};

enum ClipboardCommand {
    Text { origin: [u8; 32], revision: u64, text: String },
    Png { origin: [u8; 32], revision: u64, png: Vec<u8> },
    Clear { origin: [u8; 32], revision: u64 },
}

#[derive(Clone)]
pub struct SdkClipboardSender {
    tx: SyncSender<ClipboardCommand>,
}

impl SdkClipboardSender {
    /// Starts a bounded writer for an already-negotiated capability.
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        thread::Builder::new()
            .name("anchor-sdk-clipboard-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK clipboard runtime failed: {error}");
                            return;
                        }
                    };
                for command in rx {
                    let result = runtime.block_on(async {
                        match command {
                            ClipboardCommand::Text { origin, revision, text } => {
                                capability
                                    .send_record(
                                        clipboard::PUBLISH_TYPE_URL,
                                        clipboard::encode_text(origin, revision, text),
                                    )
                                    .await
                            }
                            ClipboardCommand::Png { origin, revision, png } => {
                                capability
                                    .send_record(
                                        clipboard::PUBLISH_TYPE_URL,
                                        clipboard::encode_png(origin, revision, png),
                                    )
                                    .await
                            }
                            ClipboardCommand::Clear { origin, revision } => {
                                capability
                                    .send_record(
                                        clipboard::CLEAR_TYPE_URL,
                                        clipboard::encode_clear(origin, revision),
                                    )
                                    .await
                            }
                        }
                    });
                    if let Err(error) = result {
                        log::warn!("SDK clipboard send failed: {error}");
                    }
                }
            })
            .expect("SDK clipboard writer thread must start");
        Self { tx }
    }

    pub fn send_text(&self, origin: [u8; 32], revision: u64, text: impl Into<String>) -> bool {
        self.tx.try_send(ClipboardCommand::Text { origin, revision, text: text.into() }).is_ok()
    }

    pub fn send_png(&self, origin: [u8; 32], revision: u64, png: Vec<u8>) -> bool {
        self.tx.try_send(ClipboardCommand::Png { origin, revision, png }).is_ok()
    }

    pub fn send_clear(&self, origin: [u8; 32], revision: u64) -> bool {
        self.tx.try_send(ClipboardCommand::Clear { origin, revision }).is_ok()
    }
}
