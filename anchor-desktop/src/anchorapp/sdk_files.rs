//! Desktop sender for the typed `org.anchor.files@1` capability.
//!
//! File metadata stays on the ordered control stream. The bytes themselves are
//! written to one capability-bound reliable QUIC stream, so a large transfer
//! cannot consume the control queue used by latency-sensitive input.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anchor_sdk::{Capability, files};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone)]
pub struct SdkFilesSender {
    capability: Capability,
}

impl SdkFilesSender {
    pub fn new(capability: Capability) -> Self {
        Self { capability }
    }

    /// Send one local file synchronously from the caller's watcher thread.
    /// The QUIC operations run on a short-lived Tokio runtime so the rest of
    /// the desktop remains entirely synchronous.
    pub fn send_file(&self, path: &Path, size: u64, chunk_size: usize) -> io::Result<()> {
        let path = path.to_path_buf();
        let capability = self.capability.clone();
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(other)?;
        runtime.block_on(async move { send_file_async(capability, path, size, chunk_size).await })
    }
}

async fn send_file_async(
    capability: Capability,
    path: PathBuf,
    size: u64,
    chunk_size: usize,
) -> io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("file").to_string();
    let hash = hash_file(&path)?;
    let transfer_id = Uuid::new_v4().into_bytes().to_vec();

    capability
        .send_record(
            files::OFFER_TYPE_URL,
            files::encode_offer(files::FileOffer {
                transfer_id: transfer_id.clone(),
                filename: name.clone(),
                mime_type: mime_type(&name),
                byte_length: size,
                sha256: hash.to_vec(),
            }),
        )
        .await
        .map_err(other)?;

    let stream = capability.open_stream(files::CONTENT_STREAM_TYPE_URL).await.map_err(other)?;
    let stream_id = stream.stream_id();
    let (mut send, _recv) = stream.into_parts();
    capability
        .send_record(
            files::CONTENT_START_TYPE_URL,
            files::encode_content_start(files::FileContentStart {
                transfer_id: transfer_id.clone(),
                quic_stream_id: stream_id,
            }),
        )
        .await
        .map_err(other)?;

    let mut file = File::open(&path)?;
    let mut buf = vec![0u8; chunk_size.clamp(4 * 1024, 4 * 1024 * 1024)];
    let mut written = 0u64;
    loop {
        let count = read_full(&mut file, &mut buf)?;
        if count == 0 {
            break;
        }
        send.write_all(&buf[..count]).await.map_err(other)?;
        written += count as u64;
    }
    send.finish().map_err(other)?;

    // Refuse to report success if the file changed between the watcher stat
    // and the stream read. The receiver will discard a mismatched completion.
    if written != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file changed while sending: expected {size} bytes, wrote {written}"),
        ));
    }
    capability
        .send_record(
            files::COMPLETE_TYPE_URL,
            files::encode_complete(files::FileComplete { transfer_id, sha256: hash.to_vec() }),
        )
        .await
        .map_err(other)?;
    Ok(())
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let count = read_full(&mut file, &mut buf)?;
        if count == 0 {
            break;
        }
        hasher.update(&buf[..count]);
    }
    Ok(hasher.finalize().into())
}

fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn mime_type(name: &str) -> String {
    match Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[derive(Clone, Default)]
pub struct SdkFilesBinding {
    senders: Arc<Mutex<HashMap<String, SdkFilesSender>>>,
}

impl SdkFilesBinding {
    pub fn attach(&self, device_id: String, sender: SdkFilesSender) {
        self.senders.lock().unwrap().insert(device_id, sender);
    }

    pub fn senders(&self) -> Vec<(String, SdkFilesSender)> {
        self.senders
            .lock()
            .unwrap()
            .iter()
            .map(|(id, sender)| (id.clone(), sender.clone()))
            .collect()
    }

    pub fn detach(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_type_is_conservative_for_unknown_extensions() {
        assert_eq!(mime_type("photo.PNG"), "image/png");
        assert_eq!(mime_type("archive.weird"), "application/octet-stream");
    }

    #[test]
    fn binding_starts_empty_and_can_be_detached() {
        let binding = SdkFilesBinding::default();
        assert!(binding.senders().is_empty());
        binding.detach("missing");
        assert!(binding.senders().is_empty());
    }
}
