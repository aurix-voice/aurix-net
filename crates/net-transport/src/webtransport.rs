//! WebTransport (HTTP/3 over QUIC) endpoint for browsers. Same certificate, same tickets and
//! the same datagram-only data path as the raw QUIC endpoint; only the handshake differs
//! (browsers cannot open raw QUIC). Browsers pin the self-signed dev certificate through
//! `serverCertificateHashes`, production nodes use their real certificate.

use crate::quic::ServerIdentity;
use net_common::{NetError, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use wtransport::endpoint::endpoint_side::{Client, Server};
use wtransport::{ClientConfig, Endpoint, ServerConfig};

pub fn server_endpoint(
    addr: SocketAddr,
    id: &ServerIdentity,
    idle_timeout: Duration,
    keep_alive: Duration,
) -> Result<Endpoint<Server>> {
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(id.cert_chain.clone(), id.key.clone_key())
        .map_err(|e| NetError::Config(format!("webtransport tls: {e}")))?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let cfg = ServerConfig::builder()
        .with_bind_address(addr)
        .with_custom_tls(tls)
        .max_idle_timeout(Some(idle_timeout))
        .map_err(|e| NetError::Config(format!("webtransport idle: {e}")))?
        .keep_alive_interval(Some(keep_alive))
        .build();
    Endpoint::server(cfg).map_err(|e| NetError::Transport(format!("webtransport bind {addr}: {e}")))
}

/// Client that accepts exactly the server certificate with the given SHA-256 fingerprint
/// (hex), as a browser does with `serverCertificateHashes`. Used by tests and the load bot.
pub fn client_endpoint(cert_sha256_hex: &str) -> Result<Endpoint<Client>> {
    let bytes =
        hex::decode(cert_sha256_hex).map_err(|e| NetError::Config(format!("fingerprint: {e}")))?;
    let digest: [u8; 32] = bytes
        .try_into()
        .map_err(|_| NetError::Config("fingerprint must be 32 bytes".into()))?;
    let cfg = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(digest)])
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .build();
    Endpoint::client(cfg).map_err(|e| NetError::Transport(format!("webtransport client: {e}")))
}

pub type SharedConnection = Arc<wtransport::Connection>;
