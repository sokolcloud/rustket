//! TLS trust: self-signed servers, host-bound pins, rejected chains.

#![allow(clippy::unwrap_used)]

mod common;

use common::{http_response, json_ok, self_signed, serve, serve_tls};
use rocketchat::tls::{fingerprint, probe_certificate};
use rocketchat::{Error, RestClient, ServerUrl, TlsConfig};

const INFO: &str = r#"{"version":"8.9","success":true}"#;

#[tokio::test]
async fn untrusted_then_pinned() {
    let (cert, key) = self_signed(&["localhost"]);
    let fp = fingerprint(&cert);
    let (port, _) = serve_tls(cert.clone(), key, |_| Some(json_ok(INFO))).await;
    let server = ServerUrl::parse(&format!("https://localhost:{port}")).unwrap();

    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(server.clone(), &tls).unwrap();
    let e = c.info().await.unwrap_err();
    match &e {
        Error::Certificate { host, .. } => assert_eq!(host.as_deref(), Some("localhost")),
        other => panic!("expected Certificate, got {other:?}"),
    }
    assert!(!e.is_transient());
    let chain = tls.take_rejected_chain("localhost").unwrap();
    assert_eq!(chain[0].sha256, fp);
    assert!(tls.take_rejected_chain("localhost").is_none(), "taken once");

    let probed = probe_certificate(&server).await.unwrap();
    assert_eq!(probed[0].sha256, fp);

    let pinned = TlsConfig::new().trust("localhost", fp).build().unwrap();
    let c = RestClient::new(server, &pinned).unwrap();
    assert_eq!(c.info().await.unwrap().version.as_deref(), Some("8.9"));
}

#[tokio::test]
async fn pin_is_bound_to_host() {
    let (cert, key) = self_signed(&["localhost", "127.0.0.1"]);
    let fp = fingerprint(&cert);
    let (port, _) = serve_tls(cert, key, |_| Some(json_ok(INFO))).await;
    // Pinned for "localhost" only; the same certificate presented as 127.0.0.1 is rejected.
    let tls = TlsConfig::new().trust("localhost", fp).build().unwrap();
    let ok = RestClient::new(
        ServerUrl::parse(&format!("https://localhost:{port}")).unwrap(),
        &tls,
    )
    .unwrap();
    ok.info().await.unwrap();
    let other = RestClient::new(
        ServerUrl::parse(&format!("https://127.0.0.1:{port}")).unwrap(),
        &tls,
    )
    .unwrap();
    let e = other.info().await.unwrap_err();
    assert!(e.is_certificate_error(), "{e:?}");
    assert!(tls.take_rejected_chain("127.0.0.1").is_some());
    assert!(tls.take_rejected_chain("localhost").is_none());
}

#[tokio::test]
async fn rejected_chains_are_per_host() {
    let (cert_a, key_a) = self_signed(&["localhost"]);
    let (cert_b, key_b) = self_signed(&["127.0.0.1"]);
    let (pa, _) = serve_tls(cert_a.clone(), key_a, |_| Some(json_ok(INFO))).await;
    let (pb, _) = serve_tls(cert_b.clone(), key_b, |_| Some(json_ok(INFO))).await;
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(
        ServerUrl::parse(&format!("https://localhost:{pa}")).unwrap(),
        &tls,
    )
    .unwrap();
    assert!(c.info().await.unwrap_err().is_certificate_error());
    // A foreign download (e.g. a link preview) fails afterwards...
    assert!(
        c.download(&format!("https://127.0.0.1:{pb}/img.png"))
            .await
            .is_err()
    );
    // ...but the server's chain is still the one shown for the server host.
    assert_eq!(
        tls.take_rejected_chain("localhost").unwrap()[0].sha256,
        fingerprint(&cert_a)
    );
    assert_eq!(
        tls.take_rejected_chain("127.0.0.1").unwrap()[0].sha256,
        fingerprint(&cert_b)
    );
}

#[tokio::test]
async fn download_redirect_https_to_http_is_refused() {
    let (pb, log_b) = serve(|_| Some(http_response("200 OK", &[], "x"))).await;
    let (cert, key) = self_signed(&["localhost"]);
    let fp = fingerprint(&cert);
    let location = format!("http://127.0.0.1:{pb}/x");
    let (pa, _) = serve_tls(cert, key, move |_| {
        Some(http_response("302 Found", &[("Location", &location)], ""))
    })
    .await;
    let tls = TlsConfig::new().trust("localhost", fp).build().unwrap();
    let c = RestClient::new(
        ServerUrl::parse(&format!("https://localhost:{pa}")).unwrap(),
        &tls,
    )
    .unwrap();
    let e = c.download("/file-upload/x").await.unwrap_err();
    assert!(matches!(e, Error::Redirect { .. }), "{e:?}");
    assert!(log_b.lock().unwrap().is_empty());
}
