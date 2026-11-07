//! Shared rustls configuration: platform verifier plus user-approved pinned certificates.
//!
//! One [`TlsContext`] per server is shared by the REST client and the DDP WebSocket.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::server_url::ServerUrl;

/// SHA-256 of a certificate's DER encoding.
pub type Fingerprint = [u8; 32];

pub fn fingerprint(der: &[u8]) -> Fingerprint {
    Sha256::digest(der).into()
}

/// `AB:CD:…` (uppercase, colon separated).
pub fn fingerprint_hex(fp: &Fingerprint) -> String {
    fp.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parses a fingerprint written as hex, with or without `:`/space separators.
pub fn parse_fingerprint(s: &str) -> Result<Fingerprint> {
    let clean: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    let bytes = hex::decode(&clean).map_err(|e| Error::Tls(format!("bad fingerprint: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Tls("fingerprint must be 32 bytes".into()))
}

/// A certificate presented by a server.
#[derive(Clone, PartialEq, Eq)]
pub struct CertificateInfo {
    pub der: Vec<u8>,
    pub sha256: Fingerprint,
}

impl CertificateInfo {
    pub fn from_der(der: &[u8]) -> Self {
        Self {
            der: der.to_vec(),
            sha256: fingerprint(der),
        }
    }

    pub fn fingerprint_hex(&self) -> String {
        fingerprint_hex(&self.sha256)
    }
}

impl fmt::Debug for CertificateInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertificateInfo(sha256={})", self.fingerprint_hex())
    }
}

/// Normalises a host for pin / rejected-chain lookup: lowercase, no trailing dot,
/// no brackets around IPv6 literals.
pub fn normalize_tls_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// TLS trust settings.
///
/// Pins are bound to a host name: a pinned leaf is accepted only when the
/// connection's server name (SNI host or IP) equals that host. For a self-signed
/// certificate the user approved, the exact leaf is accepted for that host
/// regardless of its SAN/validity (common "trust this certificate" UX); the port is
/// not part of the pin.
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pinned: Vec<(String, Fingerprint)>,
}

impl TlsConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust the leaf certificate with fingerprint `fp` for `host` even if the
    /// platform verifier rejects it.
    pub fn trust(mut self, host: &str, fp: Fingerprint) -> Self {
        let entry = (normalize_tls_host(host), fp);
        if !self.pinned.contains(&entry) {
            self.pinned.push(entry);
        }
        self
    }

    pub fn pinned(&self) -> &[(String, Fingerprint)] {
        &self.pinned
    }

    /// Builds the shared client config (explicit aws-lc-rs provider, never the
    /// process default).
    pub fn build(&self) -> Result<TlsContext> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let rejected: RejectedChains = Arc::new(Mutex::new(HashMap::new()));
        let verifier = PinningVerifier {
            inner: rustls_platform_verifier::Verifier::new(provider.clone())
                .map_err(|e| Error::Tls(e.to_string()))?,
            pinned: self.pinned.clone(),
            rejected: rejected.clone(),
        };
        let config = client_config_with(provider, Arc::new(verifier))?;
        Ok(TlsContext {
            config: Arc::new(config),
            rejected,
        })
    }
}

fn client_config_with(
    provider: Arc<CryptoProvider>,
    verifier: Arc<dyn ServerCertVerifier>,
) -> Result<ClientConfig> {
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// Rejected chains keyed by normalised server name.
type RejectedChains = Arc<Mutex<HashMap<String, Vec<CertificateInfo>>>>;

/// Built TLS configuration plus the certificate chains the verifier rejected.
#[derive(Clone)]
pub struct TlsContext {
    config: Arc<ClientConfig>,
    rejected: RejectedChains,
}

impl fmt::Debug for TlsContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsContext").finish_non_exhaustive()
    }
}

impl TlsContext {
    /// Config without ALPN, for the WebSocket (DDP) connection.
    pub fn client_config(&self) -> Arc<ClientConfig> {
        self.config.clone()
    }

    /// Copy with ALPN `h2`/`http/1.1` for HTTP clients.
    pub fn http_client_config(&self) -> ClientConfig {
        let mut c = (*self.config).clone();
        c.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        c
    }

    /// A `reqwest::ClientBuilder` preconfigured with this TLS config, the Rustket
    /// user agent, a connect timeout, HTTP/2 keep-alive pings, no `Referer` and **no
    /// redirects** (callers choose a redirect policy; the REST client only follows
    /// same-origin redirects).
    pub fn reqwest_builder(&self) -> reqwest::ClientBuilder {
        reqwest_builder(self.http_client_config())
    }

    /// The chain most recently rejected for `host` (leaf first). Call after an
    /// [`Error::Certificate`] (its `host` field) to show the user a fingerprint;
    /// pin it with [`TlsConfig::trust`] for the same host.
    pub fn take_rejected_chain(&self, host: &str) -> Option<Vec<CertificateInfo>> {
        self.rejected
            .lock()
            .ok()
            .and_then(|mut g| g.remove(&normalize_tls_host(host)))
    }
}

fn reqwest_builder(config: ClientConfig) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tls_backend_preconfigured(config)
        .user_agent(crate::USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        .referer(false)
        .redirect(reqwest::redirect::Policy::none())
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(60))
}

fn server_name_str(name: &ServerName<'_>) -> String {
    normalize_tls_host(&name.to_str())
}

#[derive(Debug)]
struct PinningVerifier {
    inner: rustls_platform_verifier::Verifier,
    pinned: Vec<(String, Fingerprint)>,
    rejected: RejectedChains,
}

fn chain_info(
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> Vec<CertificateInfo> {
    std::iter::once(end_entity)
        .chain(intermediates)
        .map(|c| CertificateInfo::from_der(c.as_ref()))
        .collect()
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let host = server_name_str(server_name);
        let fp = fingerprint(end_entity.as_ref());
        if self.pinned.iter().any(|(h, p)| *h == host && *p == fp) {
            return Ok(ServerCertVerified::assertion());
        }
        let result = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        if result.is_err()
            && let Ok(mut map) = self.rejected.lock()
        {
            map.insert(host, chain_info(end_entity, intermediates));
        }
        result
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

type ProbeSlot = Arc<Mutex<Option<Vec<CertificateInfo>>>>;

/// Records the presented chain and aborts the handshake.
#[derive(Debug)]
struct RecordingVerifier {
    provider: Arc<CryptoProvider>,
    slot: ProbeSlot,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some(chain_info(end_entity, intermediates));
        }
        Err(rustls::Error::General("certificate probe".into()))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Connects to `server` and returns the certificate chain it presents (leaf first),
/// without trusting it. No HTTP request is sent. Returns an empty list for `http://`.
pub async fn probe_certificate(server: &ServerUrl) -> Result<Vec<CertificateInfo>> {
    if !server.is_https() {
        return Ok(Vec::new());
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let slot: ProbeSlot = Arc::new(Mutex::new(None));
    let verifier = Arc::new(RecordingVerifier {
        provider: provider.clone(),
        slot: slot.clone(),
    });
    let mut config = client_config_with(provider, verifier)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let client = reqwest_builder(config).build().map_err(Error::from)?;
    let result = client.head(server.as_str()).send().await;
    let chain = slot.lock().ok().and_then(|mut g| g.take());
    match (chain, result) {
        (Some(chain), _) => Ok(chain),
        (None, Err(e)) => Err(e.into()),
        (None, Ok(_)) => Err(Error::Tls("server presented no certificate".into())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints() {
        let fp = fingerprint(b"hello");
        let hex = fingerprint_hex(&fp);
        assert!(hex.starts_with("2C:F2:4D:BA"));
        assert_eq!(parse_fingerprint(&hex).unwrap(), fp);
        assert_eq!(
            parse_fingerprint(&hex.replace(':', "").to_lowercase()).unwrap(),
            fp
        );
        assert!(parse_fingerprint("abcd").is_err());
    }

    #[test]
    fn builds_configs() {
        let cfg = TlsConfig::new()
            .trust("Example.COM.", [7; 32])
            .trust("example.com", [7; 32]);
        assert_eq!(cfg.pinned(), &[("example.com".to_owned(), [7; 32])]);
        let ctx = cfg.build().unwrap();
        assert!(ctx.client_config().alpn_protocols.is_empty());
        assert_eq!(ctx.http_client_config().alpn_protocols.len(), 2);
        assert!(ctx.reqwest_builder().build().is_ok());
        assert!(ctx.take_rejected_chain("example.com").is_none());
    }
}
