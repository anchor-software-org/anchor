use rcgen::{CertificateParams, KeyPair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use crate::anchorapp::config::IDENTITY_DIRECTORY;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TrustedDeviceEntry {
    pub device_id: String,
    pub device_name: String,
    pub certificate_pem: String,
    /// Additional certificate used by the SDK transport.  It is delivered
    /// over the already authenticated control channel and is optional so
    /// existing pairing files remain compatible.
    #[serde(default)]
    pub sdk_certificate_pem: Option<String>,
    pub paired_at: u64,
    pub last_seen: u64,
}

pub struct TrustedStore {
    config_dir: PathBuf,
    pub devices: HashMap<String, TrustedDeviceEntry>,
}

impl TrustedStore {
    pub fn load(config_dir: &Path) -> Self {
        let devices_dir = config_dir.join("devices");
        fs::create_dir_all(&devices_dir).ok();
        let mut devices = HashMap::new();
        if let Ok(entries) = fs::read_dir(&devices_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(contents) = fs::read_to_string(entry.path())
                    && let Ok(device) = serde_json::from_str::<TrustedDeviceEntry>(&contents)
                {
                    devices.insert(device.device_id.clone(), device);
                }
            }
        }
        log::info!("Loaded {} trusted devices", devices.len());
        TrustedStore { config_dir: config_dir.to_path_buf(), devices }
    }

    pub fn add_device(&mut self, device_id: &str, device_name: &str, certificate_pem: &str) {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let entry = TrustedDeviceEntry {
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            certificate_pem: certificate_pem.to_string(),
            sdk_certificate_pem: None,
            paired_at: now,
            last_seen: now,
        };
        let devices_dir = self.config_dir.join("devices");
        fs::create_dir_all(&devices_dir).ok();
        let path = devices_dir.join(format!("{}.json", device_id));
        let json = serde_json::to_string_pretty(&entry).unwrap();

        fs::write(&path, &json).expect("Failed to write trusted device");
        log::info!("Paired device: {} ({})", device_name, device_id);
        self.devices.insert(device_id.to_string(), entry);
    }

    pub fn is_trusted(&self, cert_der: &[u8]) -> Option<String> {
        let incoming_fp = Self::fingerprint(cert_der);
        for (device_id, entry) in &self.devices {
            let mut pem_certificates = vec![entry.certificate_pem.as_str()];
            if let Some(sdk_certificate) = entry.sdk_certificate_pem.as_deref() {
                pem_certificates.push(sdk_certificate);
            }
            for pem in pem_certificates {
                let stored_certs =
                    rustls_pemfile::certs(&mut pem.as_bytes()).collect::<Result<Vec<_>, _>>();
                if let Ok(certs) = stored_certs
                    && let Some(stored_cert) = certs.first()
                    && Self::fingerprint(stored_cert.as_ref()) == incoming_fp
                {
                    return Some(device_id.clone());
                }
            }
        }
        None
    }

    /// Bind a second (SDK/QUIC) certificate to an existing paired device.
    /// The certificate is parsed before persistence so malformed identity
    /// metadata can never turn into a trusted credential.
    pub fn add_sdk_certificate(&mut self, device_id: &str, certificate_pem: &str) -> bool {
        if certificate_pem.len() > 16 * 1024 {
            log::warn!("Rejecting oversized SDK certificate for {}", device_id);
            return false;
        }
        let mut reader = certificate_pem.as_bytes();
        let parsed = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .and_then(|certs| certs.into_iter().next());
        if parsed.is_none() || !certificate_pem.contains("BEGIN CERTIFICATE") {
            log::warn!("Rejecting malformed SDK certificate for {}", device_id);
            return false;
        }
        let Some(entry) = self.devices.get_mut(device_id) else {
            return false;
        };
        if entry.sdk_certificate_pem.as_deref() == Some(certificate_pem) {
            return true;
        }
        entry.sdk_certificate_pem = Some(certificate_pem.to_string());
        let path = self.config_dir.join("devices").join(format!("{}.json", device_id));
        match serde_json::to_string_pretty(entry).ok().and_then(|json| fs::write(path, json).ok()) {
            Some(()) => {
                log::info!("Bound SDK certificate to paired device {}", device_id);
                true
            }
            None => false,
        }
    }

    pub fn remove_device(&mut self, device_id: &str) -> bool {
        if self.devices.remove(device_id).is_some() {
            let path = self.config_dir.join("devices").join(format!("{}.json", device_id));
            let _ = std::fs::remove_file(&path);
            log::info!("Unpaired device: {}", device_id);
            true
        } else {
            false
        }
    }
    pub fn update_last_seen(&mut self, device_id: &str) {
        if let Some(entry) = self.devices.get_mut(device_id) {
            entry.last_seen = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let path = self.config_dir.join("devices").join(format!("{}.json", device_id));
            if let Ok(json) = serde_json::to_string_pretty(entry) {
                let _ = std::fs::write(&path, &json);
            }
        }
    }
    pub fn fingerprint(cert_der: &[u8]) -> String {
        let hash = Sha256::digest(cert_der);
        hash.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(":")
    }
}

pub struct AnchorIdentity {
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub device_id: String,
}

pub fn load_or_create_identity() -> AnchorIdentity {
    log::debug!("Running identity load / create");
    let cert_path = IDENTITY_DIRECTORY.get().unwrap().join("certificate.pem");
    let key_path = IDENTITY_DIRECTORY.get().unwrap().join("private_key.pem");
    let id_path = IDENTITY_DIRECTORY.get().unwrap().join("device_id");

    // Try loading existing identity; regenerate on any failure.
    if let (Ok(cert), Ok(key), Ok(id)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
        std::fs::read_to_string(&id_path),
    ) {
        log::debug!("Found device id, private key and certificate");
        return AnchorIdentity { certificate_pem: cert, private_key_pem: key, device_id: id };
    }

    log::info!("No valid identity found, generating new one");
    let key_pair = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec!["anchor.local".to_string()]).unwrap();
    let cert = params.self_signed(&key_pair).unwrap();

    let identity = AnchorIdentity {
        certificate_pem: cert.pem(),
        private_key_pem: key_pair.serialize_pem(),
        device_id: uuid::Uuid::new_v4().to_string(),
    };

    fs::write(&cert_path, &identity.certificate_pem).expect("Failed to write certificate");
    fs::write(&key_path, &identity.private_key_pem).expect("Failed to write private key");
    fs::write(&id_path, &identity.device_id).expect("Failed to write device id");

    identity
}

/// Generate a test identity (cert + key + device_id) without touching disk.
#[cfg(test)]
pub fn generate_test_identity() -> AnchorIdentity {
    let key_pair = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec!["anchor.test".to_string()]).unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    AnchorIdentity {
        certificate_pem: cert.pem(),
        private_key_pem: key_pair.serialize_pem(),
        device_id: uuid::Uuid::new_v4().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("anchor_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cert_der_from_pem(pem: &str) -> Vec<u8> {
        rustls_pemfile::certs(&mut pem.as_bytes()).next().unwrap().unwrap().as_ref().to_vec()
    }

    #[test]
    fn trusted_store_add_and_find() {
        let dir = make_temp_dir();
        let mut store = TrustedStore::load(&dir);

        let id = generate_test_identity();
        let cert_der = cert_der_from_pem(&id.certificate_pem);

        store.add_device("dev-1", "Test Phone", &id.certificate_pem);

        assert!(store.is_trusted(&cert_der).is_some());
        assert_eq!(store.is_trusted(&cert_der).unwrap(), "dev-1");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trusted_store_unknown_cert_returns_none() {
        let dir = make_temp_dir();
        let store = TrustedStore::load(&dir);

        let id = generate_test_identity();
        let cert_der = cert_der_from_pem(&id.certificate_pem);

        assert!(store.is_trusted(&cert_der).is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trusted_store_remove_device() {
        let dir = make_temp_dir();
        let mut store = TrustedStore::load(&dir);

        let id = generate_test_identity();
        let cert_der = cert_der_from_pem(&id.certificate_pem);

        store.add_device("dev-1", "Phone", &id.certificate_pem);
        assert!(store.is_trusted(&cert_der).is_some());

        store.remove_device("dev-1");
        assert!(store.is_trusted(&cert_der).is_none());

        // File should be gone too
        let file = dir.join("devices").join("dev-1.json");
        assert!(!file.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trusted_store_persists_to_disk() {
        let dir = make_temp_dir();

        let id = generate_test_identity();
        let cert_der = cert_der_from_pem(&id.certificate_pem);

        {
            let mut store = TrustedStore::load(&dir);
            store.add_device("dev-1", "Phone", &id.certificate_pem);
        }

        // Reload from disk
        let store2 = TrustedStore::load(&dir);
        assert!(store2.is_trusted(&cert_der).is_some());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trusted_store_accepts_bound_sdk_certificate_and_persists_it() {
        let dir = make_temp_dir();
        let mut store = TrustedStore::load(&dir);
        let control = generate_test_identity();
        let sdk = generate_test_identity();
        let sdk_der = cert_der_from_pem(&sdk.certificate_pem);

        store.add_device("dev-1", "Phone", &control.certificate_pem);
        assert!(store.add_sdk_certificate("dev-1", &sdk.certificate_pem));
        assert_eq!(store.is_trusted(&sdk_der).as_deref(), Some("dev-1"));

        let reloaded = TrustedStore::load(&dir);
        assert_eq!(reloaded.is_trusted(&sdk_der).as_deref(), Some("dev-1"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trusted_store_update_last_seen() {
        let dir = make_temp_dir();
        let mut store = TrustedStore::load(&dir);

        store.add_device("dev-1", "Phone", "cert");
        let t1 = store.devices["dev-1"].last_seen;

        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.update_last_seen("dev-1");
        let t2 = store.devices["dev-1"].last_seen;

        assert!(t2 > t1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let data = b"some cert bytes";
        let fp1 = TrustedStore::fingerprint(data);
        let fp2 = TrustedStore::fingerprint(data);
        assert_eq!(fp1, fp2);
        // SHA-256 = 64 hex chars + 31 colons
        assert_eq!(fp1.len(), 64 + 31);
    }

    #[test]
    fn fingerprint_different_certs_differ() {
        let fp1 = TrustedStore::fingerprint(b"cert A");
        let fp2 = TrustedStore::fingerprint(b"cert B");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn generate_test_identity_produces_valid_cert() {
        let id = generate_test_identity();
        assert!(!id.certificate_pem.is_empty());
        assert!(!id.private_key_pem.is_empty());
        assert!(!id.device_id.is_empty());
        let listener_identity =
            anchor_sdk::ListenerIdentity::from_pem(id.certificate_pem, id.private_key_pem);
        assert!(listener_identity.certificate_der().is_ok());
    }
}
