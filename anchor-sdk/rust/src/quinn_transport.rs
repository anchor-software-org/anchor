//! Quinn transport controls for the Rust/Linux SDK.
//!
//! This module configures only Anchor's QUIC invariants. Identity creation,
//! certificate pinning, reconnect policy, and host permissions stay outside the
//! transport library so they can have matching Kotlin and Swift semantics.

use std::{sync::Arc, time::Duration};

use quinn::{ClientConfig, ServerConfig};
use thiserror::Error;

use crate::ALPN;

#[derive(Debug, Error)]
pub enum QuinnConfigError {
    #[error("TLS configuration is not QUIC/TLS-1.3 compatible: {0}")]
    Tls(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
}

/// Converts a caller-owned TLS client configuration into an Anchor QUIC client
/// configuration. The caller must configure its paired-peer certificate pin
/// verifier before invoking this function.
pub fn client_config(mut tls: rustls::ClientConfig) -> Result<ClientConfig, QuinnConfigError> {
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut config = ClientConfig::new(Arc::new(quic_tls));
    config.transport_config(transport_config());
    Ok(config)
}

/// Converts a caller-owned TLS server configuration into an Anchor QUIC server
/// configuration. A production caller must require/validate its peer identity
/// according to the currently approved pairing security model.
pub fn server_config(mut tls: rustls::ServerConfig) -> Result<ServerConfig, QuinnConfigError> {
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut config = ServerConfig::with_crypto(Arc::new(quic_tls));
    config.transport_config(transport_config());
    Ok(config)
}

/// Keep an otherwise idle SDK session alive. Clipboard and control sessions
/// can legitimately have no application data for minutes; Quinn's default
/// idle timeout is short enough to tear those sessions down unexpectedly.
/// Both sides use the same policy so the MsQuic and Quinn adapters behave the
/// same way on the wire.
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    // Keep the unreliable media queue small enough that scene-change bursts
    // cannot build a second of stale frames behind the one being displayed.
    // DatagramFlow applies its own reserve before enqueueing a complete access
    // unit, so this bounds latency without invoking Quinn's lossy eviction path.
    config.datagram_send_buffer_size(256 * 1024);
    config.keep_alive_interval(Some(Duration::from_secs(10)));
    config.max_idle_timeout(Some(
        Duration::from_secs(120)
            .try_into()
            .expect("120 seconds is a valid QUIC idle timeout"),
    ));
    Arc::new(config)
}
