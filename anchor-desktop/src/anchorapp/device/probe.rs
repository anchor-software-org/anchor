//! UDP probe responder for wired (USB-tethered) discovery.
//!
//! The phone broadcasts `ANCHOR_PROBE_V1` to the tethered subnet on UDP port
//! 5028 and this answers `ANCHOR_HERE_V1\n` + a JSON object carrying the same
//! public identity the mDNS advertisement publishes (device id, name, QUIC
//! port, public certificate). mDNS cannot reach tethered downstream
//! interfaces, so the probe stands in for it.

use std::{
    collections::HashMap,
    net::{IpAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};

use base64::Engine;

/// UDP port the phone probes on wired subnets.
pub(crate) const PROBE_PORT: u16 = 5028;

/// Probe payload sent by the phone.
pub(crate) const PROBE_REQUEST: &[u8] = b"ANCHOR_PROBE_V1";

/// Prefix of the responder payload; identity JSON follows on the next line.
pub(crate) const PROBE_RESPONSE_PREFIX: &[u8] = b"ANCHOR_HERE_V1\n";

/// Minimum interval between replies to the same source. The reply is much
/// larger than the probe, so without this the responder could be abused as a
/// UDP reflection amplifier. Legitimate probing is far below this rate.
const REPLY_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Build the reply payload for a probe datagram, or `None` if the datagram is
/// not an Anchor probe. Extracted from the socket loop so it is unit-testable.
fn probe_reply(request: &[u8], device_id: &str, certificate_der: &[u8]) -> Option<Vec<u8>> {
    if request != PROBE_REQUEST {
        return None;
    }
    let certificate = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(certificate_der);
    let body = serde_json::json!({
        "device_id": device_id,
        "device_name": super::mdns::desktop_name(),
        "port": super::mdns::SDK_PORT,
        "certificate": certificate,
    });
    let mut reply = Vec::with_capacity(PROBE_RESPONSE_PREFIX.len() + 256 + certificate.len());
    reply.extend_from_slice(PROBE_RESPONSE_PREFIX);
    reply.extend_from_slice(body.to_string().as_bytes());
    Some(reply)
}

/// First certificate in the desktop identity PEM, as DER. This is public
/// self-signed material — identical to what mDNS TXT records advertise.
fn identity_certificate_der(certificate_pem: &str) -> Option<Vec<u8>> {
    rustls_pemfile::certs(&mut certificate_pem.as_bytes())
        .next()?
        .ok()
        .map(|certificate| certificate.as_ref().to_vec())
}

/// Bind the probe responder and serve requests on a detached thread.
///
/// Returns the bound socket — keeping it alive is not required (the responder
/// owns a clone) but it lets callers and tests inspect the bound address.
/// A bind or spawn failure is logged and treated as non-fatal, matching
/// `mdns.rs`: the QUIC listener keeps working without discovery.
pub(super) fn start_responder(device_id: String, certificate_pem: String) -> Option<UdpSocket> {
    start_responder_at(PROBE_PORT, device_id, certificate_pem)
}

fn start_responder_at(port: u16, device_id: String, certificate_pem: String) -> Option<UdpSocket> {
    let certificate_der = match identity_certificate_der(&certificate_pem) {
        Some(der) => der,
        None => {
            log::warn!("probe responder: identity certificate could not be parsed");
            return None;
        }
    };
    let socket = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(socket) => socket,
        Err(e) => {
            log::warn!("probe responder: failed to bind UDP {port}: {e}");
            return None;
        }
    };
    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(250))) {
        log::warn!("probe responder: failed to set read timeout: {e}");
        return None;
    }
    let thread_socket = match socket.try_clone() {
        Ok(clone) => clone,
        Err(e) => {
            log::warn!("probe responder: failed to clone socket: {e}");
            return None;
        }
    };
    log::info!("probe responder: answering wired discovery probes on UDP {port}");
    thread::Builder::new()
        .name("anchor-probe-responder".into())
        .spawn(move || {
            let mut buf = [0u8; 2048];
            let mut last_reply: HashMap<IpAddr, Instant> = HashMap::new();
            loop {
                match thread_socket.recv_from(&mut buf) {
                    Ok((len, peer)) => {
                        let Some(reply) = probe_reply(&buf[..len], &device_id, &certificate_der)
                        else {
                            continue;
                        };
                        let now = Instant::now();
                        if last_reply.get(&peer.ip()).is_some_and(|t| now - *t < REPLY_MIN_INTERVAL)
                        {
                            continue;
                        }
                        last_reply.insert(peer.ip(), now);
                        // Bound the map so a spoofed-source flood cannot grow it.
                        if last_reply.len() > 1024 {
                            last_reply.retain(|_, t| now - *t < Duration::from_secs(60));
                        }
                        if let Err(e) = thread_socket.send_to(&reply, peer) {
                            log::debug!("probe responder: reply to {peer} failed: {e}");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) => {
                        log::warn!("probe responder: recv failed ({e}); stopping");
                        return;
                    }
                }
            }
        })
        .ok()?;
    Some(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cert_pem() -> String {
        crate::anchorapp::tls::generate_test_identity().certificate_pem
    }

    #[test]
    fn reply_to_valid_probe_contains_identity() {
        let certificate = vec![1u8, 2, 3, 4];
        let reply = probe_reply(PROBE_REQUEST, "dev-123", &certificate).expect("reply");
        assert!(reply.starts_with(PROBE_RESPONSE_PREFIX));

        let json: serde_json::Value =
            serde_json::from_slice(&reply[PROBE_RESPONSE_PREFIX.len()..]).unwrap();
        assert_eq!(json["device_id"], "dev-123");
        assert_eq!(json["port"], 5027);
        let encoded = json["certificate"].as_str().unwrap();
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).unwrap(),
            certificate
        );
        assert_eq!(json["device_name"].as_str().unwrap(), super::super::mdns::desktop_name());
    }

    #[test]
    fn non_probe_datagrams_are_ignored() {
        assert!(probe_reply(b"ANCHOR_PROBE_V0", "dev", &[1]).is_none());
        assert!(probe_reply(b"anchor_probe_v1", "dev", &[1]).is_none());
        assert!(probe_reply(b"", "dev", &[1]).is_none());
        assert!(probe_reply(b"ANCHOR_PROBE_V1 ", "dev", &[1]).is_none());
    }

    #[test]
    fn responder_answers_a_real_datagram() {
        // Port 0 → OS picks a free port; the returned socket reports it.
        let socket =
            start_responder_at(0, "dev-xyz".into(), test_cert_pem()).expect("responder binds");
        let port = socket.local_addr().unwrap().port();

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        client.send_to(PROBE_REQUEST, ("127.0.0.1", port)).unwrap();

        let mut buf = [0u8; 4096];
        let (len, _) = client.recv_from(&mut buf).unwrap();
        let reply = &buf[..len];
        assert!(reply.starts_with(PROBE_RESPONSE_PREFIX));
        let json: serde_json::Value =
            serde_json::from_slice(&reply[PROBE_RESPONSE_PREFIX.len()..]).unwrap();
        assert_eq!(json["device_id"], "dev-xyz");
    }

    #[test]
    fn responder_is_silent_for_non_probe_datagrams() {
        let socket =
            start_responder_at(0, "dev-xyz".into(), test_cert_pem()).expect("responder binds");
        let port = socket.local_addr().unwrap().port();

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        client.send_to(b"hello", ("127.0.0.1", port)).unwrap();

        let mut buf = [0u8; 64];
        let err = client.recv_from(&mut buf).unwrap_err();
        assert!(
            err.kind() == std::io::ErrorKind::WouldBlock
                || err.kind() == std::io::ErrorKind::TimedOut
        );
    }
}
