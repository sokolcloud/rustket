//! In-process HTTP(S) test servers.
#![allow(clippy::unwrap_used, dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

pub type Log = Arc<Mutex<Vec<String>>>;
type Responder = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A self-signed certificate for `names`: `(cert DER, PKCS#8 key DER)`.
pub fn self_signed(names: &[&str]) -> (Vec<u8>, Vec<u8>) {
    let ck = rcgen::generate_simple_self_signed(
        names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
    )
    .unwrap();
    (ck.cert.der().to_vec(), ck.signing_key.serialize_der())
}

pub fn http_response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let mut s = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    s.push_str("\r\n");
    s.push_str(body);
    s
}

pub fn json_ok(body: &str) -> String {
    http_response("200 OK", &[("Content-Type", "application/json")], body)
}

/// Reads one request: headers plus a `Content-Length` body (at most 1 MiB is kept).
async fn read_request<S: AsyncRead + Unpin>(s: &mut S) -> String {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = s.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len.min(1 << 20) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// The JSON body of a request.
pub fn body_json(req: &str) -> serde_json::Value {
    let body = req
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_default();
    serde_json::from_str(body).unwrap_or_default()
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, log: Log, resp: Responder) {
    let req = read_request(&mut s).await;
    log.lock().unwrap().push(req.clone());
    match resp(&req) {
        Some(r) => {
            let _ = s.write_all(r.as_bytes()).await;
            let _ = s.shutdown().await;
        }
        None => tokio::time::sleep(Duration::from_secs(3600)).await,
    }
}

/// Plain HTTP/1.1 server answering every request with `resp(request_text)`
/// (`None` = never answer). Returns the port and a log of raw requests.
pub async fn serve(resp: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> (u16, Log) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let log2 = log.clone();
    let resp: Responder = Arc::new(resp);
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            tokio::spawn(handle(s, log2.clone(), resp.clone()));
        }
    });
    (port, log)
}

/// HTTPS server with the given certificate (HTTP/1.1 only).
pub async fn serve_tls(
    cert: Vec<u8>,
    key: Vec<u8>,
    resp: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
) -> (u16, Log) {
    use tokio_rustls::rustls::{self, pki_types};
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![pki_types::CertificateDer::from(cert)],
            pki_types::PrivateKeyDer::Pkcs8(pki_types::PrivatePkcs8KeyDer::from(key)),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let log2 = log.clone();
    let resp: Responder = Arc::new(resp);
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            let acceptor = acceptor.clone();
            let (log, resp) = (log2.clone(), resp.clone());
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    handle(tls, log, resp).await;
                }
            });
        }
    });
    (port, log)
}

pub fn joined(log: &Log) -> String {
    log.lock().unwrap().join("\n---\n").to_ascii_lowercase()
}

/// Accepts connections but never reads or answers (stalls uploads).
pub async fn serve_noread() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            held.push(s);
        }
    });
    port
}
