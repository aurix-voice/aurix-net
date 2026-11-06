//! QUIC endpoints (quinn). Only DATAGRAM frames are used; streams are disabled.
//! Clients pin the server certificate by SHA-256 fingerprint advertised over the control
//! channel, so self-signed certificates work everywhere native.

use net_common::{NetError, Result};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub const ALPN: &[u8] = b"aurix-net/1";

pub struct ServerIdentity {
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    pub fingerprint_sha256: String,
}

pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

/// Loads PEM material or generates a self-signed certificate for `hosts`.
pub fn server_identity(
    cert_pem: Option<&str>,
    key_pem: Option<&str>,
    hosts: &[String],
) -> Result<ServerIdentity> {
    match (cert_pem, key_pem) {
        (Some(c), Some(k)) if !c.is_empty() && !k.is_empty() => {
            let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut c.as_bytes())
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| NetError::Config(format!("cert_pem: {e}")))?;
            let key = rustls_pemfile::private_key(&mut k.as_bytes())
                .map_err(|e| NetError::Config(format!("key_pem: {e}")))?
                .ok_or_else(|| NetError::Config("key_pem has no private key".into()))?;
            let fp = chain
                .first()
                .map(|c| fingerprint(c))
                .ok_or_else(|| NetError::Config("cert_pem has no certificate".into()))?;
            Ok(ServerIdentity {
                cert_chain: chain,
                key,
                fingerprint_sha256: fp,
            })
        }
        _ => {
            let names: Vec<String> = if hosts.is_empty() {
                vec!["localhost".into()]
            } else {
                hosts.to_vec()
            };
            let cert = rcgen::generate_simple_self_signed(names)
                .map_err(|e| NetError::Config(format!("self-signed: {e}")))?;
            let der = CertificateDer::from(cert.cert);
            let fp = fingerprint(&der);
            let key = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
                .map_err(|e| NetError::Config(format!("self-signed key: {e}")))?;
            Ok(ServerIdentity {
                cert_chain: vec![der],
                key,
                fingerprint_sha256: fp,
            })
        }
    }
}

pub fn transport_config(
    idle_timeout: Duration,
    keep_alive: Duration,
) -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_concurrent_bidi_streams(0u32.into());
    t.max_concurrent_uni_streams(0u32.into());
    t.datagram_receive_buffer_size(Some(2 * 1024 * 1024));
    t.datagram_send_buffer_size(2 * 1024 * 1024);
    t.max_idle_timeout(Some(idle_timeout.try_into().unwrap_or_else(|_| {
        Duration::from_secs(15)
            .try_into()
            .expect("15s fits IdleTimeout")
    })));
    t.keep_alive_interval(Some(keep_alive));
    t.initial_mtu(1200);
    Arc::new(t)
}

pub fn server_endpoint(
    addr: SocketAddr,
    id: &ServerIdentity,
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::Endpoint> {
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(id.cert_chain.clone(), id.key.clone_key())
        .map_err(|e| NetError::Config(format!("tls: {e}")))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qsc =
        QuicServerConfig::try_from(tls).map_err(|e| NetError::Config(format!("quic tls: {e}")))?;
    let mut sc = quinn::ServerConfig::with_crypto(Arc::new(qsc));
    sc.transport_config(transport);
    quinn::Endpoint::server(sc, addr).map_err(|e| NetError::Transport(format!("bind {addr}: {e}")))
}

/// Accepts exactly the certificate with the given fingerprint, nothing else.
#[derive(Debug)]
pub struct PinnedVerifier {
    fp: String,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.fp {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub fn client_endpoint(
    bind: SocketAddr,
    pinned_fp: &str,
    transport: Arc<quinn::TransportConfig>,
) -> Result<quinn::Endpoint> {
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
            fp: pinned_fp.to_string(),
        }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|e| NetError::Config(format!("quic client tls: {e}")))?;
    let mut cc = quinn::ClientConfig::new(Arc::new(qcc));
    cc.transport_config(transport);
    let mut ep = quinn::Endpoint::client(bind).map_err(|e| NetError::Transport(e.to_string()))?;
    ep.set_default_client_config(cc);
    Ok(ep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[tokio::test]
    async fn datagram_roundtrip_with_pinned_cert() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let id = server_identity(None, None, &["localhost".into()]).unwrap();
        let t = transport_config(Duration::from_secs(5), Duration::from_secs(1));
        let server = server_endpoint("127.0.0.1:0".parse().unwrap(), &id, t.clone()).unwrap();
        let addr = server.local_addr().unwrap();
        let fp = id.fingerprint_sha256.clone();
        let srv = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let d = conn.read_datagram().await.unwrap();
            conn.send_datagram(Bytes::from(format!("echo:{}", String::from_utf8_lossy(&d))))
                .unwrap();
            conn.read_datagram().await.ok();
        });
        let client = client_endpoint("127.0.0.1:0".parse().unwrap(), &fp, t).unwrap();
        let conn = client.connect(addr, "localhost").unwrap().await.unwrap();
        conn.send_datagram(Bytes::from_static(b"hi")).unwrap();
        let r = conn.read_datagram().await.unwrap();
        assert_eq!(&r[..], b"echo:hi");
        conn.close(0u32.into(), b"done");
        srv.abort();
    }

    #[tokio::test]
    async fn wrong_fingerprint_fails_handshake() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let id = server_identity(None, None, &["localhost".into()]).unwrap();
        let t = transport_config(Duration::from_secs(5), Duration::from_secs(1));
        let server = server_endpoint("127.0.0.1:0".parse().unwrap(), &id, t.clone()).unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            while let Some(c) = server.accept().await {
                let _ = c.await;
            }
        });
        let client = client_endpoint("127.0.0.1:0".parse().unwrap(), &"00".repeat(32), t).unwrap();
        let r = client.connect(addr, "localhost").unwrap().await;
        assert!(r.is_err());
    }
}
