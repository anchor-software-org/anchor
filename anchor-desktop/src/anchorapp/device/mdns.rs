//! mDNS / DNS-SD advertising so LAN clients can discover this desktop without
//! typing an IP. Anchor's only network endpoint is the QUIC SDK listener on
//! UDP port 5027.

use std::collections::HashMap;

use base64::Engine;
use mdns_sd::{ServiceDaemon, ServiceInfo};

/// A stable, human-readable name for this desktop. The system hostname is
/// shared by both LAN discovery and the SDK session identity so paired phones
/// show the same name regardless of how they reached the desktop.
pub(crate) fn desktop_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anchor-desktop".to_string())
}

/// DNS-SD service type for Anchor desktops.
const SERVICE_TYPE: &str = "_anchor._udp.local.";
/// QUIC SDK endpoint port.
pub(super) const SDK_PORT: u16 = 5027;
const CERTIFICATE_CHUNK_BYTES: usize = 220;

/// Encodes public certificate bytes as bounded DNS-SD TXT properties. Every
/// resulting `key=value` record remains below the 255-byte TXT-string limit.
fn certificate_properties(certificate_der: &[u8]) -> Vec<(String, String)> {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(certificate_der);
    encoded
        .as_bytes()
        .chunks(CERTIFICATE_CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| {
            let chunk = std::str::from_utf8(chunk).expect("base64url output is ASCII");
            (format!("certificate{index}"), chunk.to_string())
        })
        .collect()
}

/// Begin advertising this desktop over mDNS. Returns the daemon handle, which
/// must be kept alive for the advertisement to persist — dropping it
/// unregisters the service. On failure we log and return `None` so the caller
/// can continue serving connections regardless.
pub(super) fn start_advertising(device_id: &str, certificate_pem: &str) -> Option<ServiceDaemon> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("mDNS: failed to start service daemon: {e}");
            return None;
        }
    };

    let hostname = desktop_name();
    // Instance name shown to clients; host part must end with `.local.`.
    let host_name = format!("{hostname}.local.");

    let mut txt: HashMap<String, String> = HashMap::new();
    txt.insert("device_id".to_string(), device_id.to_string());
    txt.insert("device_name".to_string(), hostname.clone());
    txt.insert("protocol_version".to_string(), "1".to_string());
    // This is public self-signed certificate material, not a credential. It
    // lets a nearby client pin the exact QUIC peer before entering the
    // strictly pairing-only connection. Neither side persists it until both
    // users approve the request.
    let certificates =
        rustls_pemfile::certs(&mut certificate_pem.as_bytes()).collect::<Result<Vec<_>, _>>().ok();
    if let Some(certificate) = certificates.and_then(|certificates| certificates.into_iter().next())
    {
        // DNS-SD limits one TXT property string to 255 bytes. Split the
        // base64 text ourselves rather than allowing the mDNS library to
        // silently truncate a certificate and make pairing fail later.
        for (key, value) in certificate_properties(certificate.as_ref()) {
            txt.insert(key, value);
        }
    } else {
        log::warn!("mDNS: desktop identity certificate could not be encoded for nearby pairing");
    }

    // Passing an empty IP slice lets mdns-sd auto-detect this host's addresses.
    let service =
        match ServiceInfo::new(SERVICE_TYPE, &hostname, &host_name, "", SDK_PORT, Some(txt)) {
            Ok(s) => s.enable_addr_auto(),
            Err(e) => {
                log::warn!("mDNS: failed to build service info: {e}");
                return None;
            }
        };

    match daemon.register(service) {
        Ok(()) => {
            log::info!("mDNS: advertising '{hostname}' as {SERVICE_TYPE} on port {SDK_PORT}");
            Some(daemon)
        }
        Err(e) => {
            log::warn!("mDNS: failed to register service: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_properties_are_lossless_and_fit_dns_sd_txt_records() {
        let certificate = (0..900).map(|index| (index % 251) as u8).collect::<Vec<_>>();
        let properties = certificate_properties(&certificate);

        assert!(properties.len() > 1);
        assert!(properties.iter().all(|(key, value)| key.len() + 1 + value.len() <= 255));

        let encoded = properties.into_iter().map(|(_, value)| value).collect::<String>();
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).unwrap(),
            certificate
        );
    }
}
