//! Desktop-side bridge for typed media records on an Anchor SDK session.
//!
//! The media plugin is synchronous and may be driven by D-Bus callbacks, so
//! this bridge owns a small ordered writer thread just like the clipboard
//! bridge. Each authenticated SDK peer owns an independent writer.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use anchor_sdk::{Capability, media};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::codecs::jpeg::JpegEncoder;
use url::Url;

use crate::anchorapp::media_plugin::{Command, MediaCommand, MediaState, PlaybackState};

enum MediaCommandMessage {
    State(MediaState),
    Command(MediaCommand),
}

#[derive(Clone)]
pub struct SdkMediaSender {
    tx: SyncSender<MediaCommandMessage>,
}

impl SdkMediaSender {
    pub fn new(capability: Capability) -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        thread::Builder::new()
            .name("anchor-sdk-media-writer".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            log::error!("SDK media runtime failed: {error}");
                            return;
                        }
                    };
                for message in rx {
                    let result = runtime.block_on(async {
                        match message {
                            MediaCommandMessage::State(state) => {
                                capability
                                    .send_record(media::STATE_TYPE_URL, encode_state(state))
                                    .await
                            }
                            MediaCommandMessage::Command(command) => {
                                capability
                                    .send_record(media::COMMAND_TYPE_URL, encode_command(command))
                                    .await
                            }
                        }
                    });
                    if let Err(error) = result {
                        log::warn!("SDK media send failed: {error}");
                    }
                }
            })
            .expect("SDK media writer thread must start");
        Self { tx }
    }

    pub fn send_state(&self, state: MediaState) -> bool {
        self.tx.try_send(MediaCommandMessage::State(state)).is_ok()
    }

    pub fn send_command(&self, command: MediaCommand) -> bool {
        self.tx.try_send(MediaCommandMessage::Command(command)).is_ok()
    }
}

#[derive(Clone, Default)]
pub struct SdkMediaBinding {
    senders: Arc<Mutex<HashMap<String, SdkMediaSender>>>,
}

impl SdkMediaBinding {
    pub fn attach(&self, device_id: String, sender: SdkMediaSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }

    pub fn senders(&self) -> Vec<(String, SdkMediaSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(device_id, sender)| (device_id.clone(), sender.clone()))
            .collect()
    }
}

fn encode_state(state: MediaState) -> Vec<u8> {
    media::encode_state(media::MediaState {
        title: state.title,
        artist: state.artist,
        album: state.album,
        playing: state.state == PlaybackState::Playing,
        position_ms: state.position_ms,
        duration_ms: state.duration_ms,
        artwork_jpeg: artwork_jpeg(&state.art_url),
    })
}

fn artwork_jpeg(art_url: &str) -> Vec<u8> {
    if let Some(encoded) = art_url.strip_prefix("data:image/jpeg;base64,") {
        return STANDARD
            .decode(encoded)
            .ok()
            .filter(|bytes| bytes.len() <= media::MAX_ARTWORK_JPEG_BYTES)
            .unwrap_or_default();
    }
    let Ok(url) = Url::parse(art_url) else {
        return Vec::new();
    };
    if url.scheme() != "file" {
        return Vec::new();
    }
    let Ok(path) = url.to_file_path() else {
        return Vec::new();
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return Vec::new();
    };
    if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let Ok(image) = image::open(Path::new(&path)) else {
        return Vec::new();
    };
    let image = image.thumbnail(512, 512).to_rgb8();
    for quality in [85, 70, 55, 40] {
        let mut bytes = Vec::new();
        if JpegEncoder::new_with_quality(&mut bytes, quality).encode_image(&image).is_ok()
            && bytes.len() <= media::MAX_ARTWORK_JPEG_BYTES
        {
            return bytes;
        }
    }
    Vec::new()
}

fn encode_command(command: MediaCommand) -> Vec<u8> {
    let kind = match command.command {
        Command::Play => 1,
        Command::Pause => 2,
        Command::Next => 3,
        Command::Previous => 4,
        Command::Seek => 5,
        Command::Playpause | Command::Stop | Command::Unknown => 0,
    };
    media::encode_command(media::MediaCommand {
        kind,
        position_ms: command.position_ms.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_encoding_keeps_the_wire_fields_platform_neutral() {
        let bytes = encode_state(MediaState {
            source: crate::anchorapp::media_plugin::Source::Desktop,
            session_id: "mpris".into(),
            state: PlaybackState::Playing,
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            app: "Player".into(),
            position_ms: 123,
            duration_ms: 456,
            can_play: true,
            can_pause: true,
            can_next: true,
            can_prev: true,
            can_seek: true,
            art_url: String::new(),
            playback_speed: 1.0,
            ts: 0,
        });
        let state = anchor_sdk::media::decode_state(&bytes).expect("valid MediaState");
        assert_eq!(state.title, "Song");
        assert!(state.playing);
        assert_eq!(state.position_ms, 123);
        assert_eq!(state.duration_ms, 456);
    }

    #[test]
    fn unsupported_transport_commands_are_not_reinterpreted() {
        let bytes = encode_command(MediaCommand {
            command: Command::Playpause,
            target_session: None,
            position_ms: None,
        });
        let command = anchor_sdk::media::decode_command(&bytes).expect("valid MediaCommand");
        assert_eq!(command.kind, 0);
    }

    #[test]
    fn artwork_transfer_accepts_only_local_files() {
        assert!(artwork_jpeg("https://example.com/art.jpg").is_empty());
        assert!(artwork_jpeg("not a URL").is_empty());
    }

    #[test]
    fn artwork_transfer_decodes_resolved_data_url() {
        let encoded = STANDARD.encode([0xff, 0xd8, 0xff, 0xd9]);
        assert_eq!(
            artwork_jpeg(&format!("data:image/jpeg;base64,{encoded}")),
            vec![0xff, 0xd8, 0xff, 0xd9],
        );
    }

    #[test]
    fn artwork_transfer_reencodes_a_local_image_as_bounded_jpeg() {
        let path =
            std::env::temp_dir().join(format!("anchor-artwork-{}.png", uuid::Uuid::new_v4()));
        image::RgbImage::from_pixel(640, 480, image::Rgb([12, 34, 56]))
            .save(&path)
            .expect("write test image");
        let url = Url::from_file_path(&path).expect("file URL");
        let artwork = artwork_jpeg(url.as_str());
        std::fs::remove_file(path).expect("remove test image");

        assert!(!artwork.is_empty());
        assert!(artwork.len() <= media::MAX_ARTWORK_JPEG_BYTES);
        assert_eq!(&artwork[..2], &[0xff, 0xd8]);
    }
}
