//! Secrets in redacted methods never reach the logs.
#![allow(clippy::unwrap_used)]

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{MockServer, within};
use meteor_ddp::CallError;
use meteor_ddp::DdpClient;
use serde_json::json;
use std::time::Duration;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    (capture, tracing::subscriber::set_default(subscriber))
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// A login result arriving after the caller stopped waiting (timeout or dropped
/// future) is still redacted.
#[tokio::test]
async fn late_results_of_abandoned_redacted_calls_are_not_logged() {
    let (capture, _guard) = capture_logs();
    let mut server = MockServer::start().await;
    let config = server.config();
    let (client, mut conn) =
        tokio::join!(DdpClient::connect(config), server.accept_handshake("s1"));
    let client = client.unwrap();

    // Timed out.
    let c = client.clone();
    let timed_out = tokio::spawn(async move {
        c.call_with_timeout(
            "login",
            vec![json!({"resume": "x"})],
            Duration::from_millis(100),
        )
        .await
    });
    let first = conn.recv_msg("method").await;
    assert!(matches!(
        within(timed_out).await.unwrap(),
        Err(CallError::Timeout)
    ));

    // Cancelled by dropping the future.
    let c = client.clone();
    let dropped = tokio::spawn(async move { c.call("login", vec![json!({"resume": "y"})]).await });
    let second = conn.recv_msg("method").await;
    dropped.abort();
    let _ = dropped.await;

    // Make sure both cancellations reached the driver before the results arrive.
    let c = client.clone();
    let sync = tokio::spawn(async move { c.call("sync", vec![]).await });
    conn.answer_method("sync", json!(0)).await;
    within(sync).await.unwrap().unwrap();

    for (req, token) in [(first, "LATE_TOKEN_1"), (second, "LATE_TOKEN_2")] {
        conn.send(json!({"msg": "result", "id": req["id"], "result": {"token": token}}))
            .await;
    }
    let c = client.clone();
    let echo = tokio::spawn(async move { c.call("echo", vec![]).await });
    conn.answer_method("echo", json!(1)).await;
    within(echo).await.unwrap().unwrap();
    client.close().await;

    let logs = capture.text();
    assert!(logs.contains("redacted"), "{logs}");
    for secret in ["LATE_TOKEN_1", "LATE_TOKEN_2"] {
        assert!(!logs.contains(secret), "{secret} leaked:\n{logs}");
    }
}

#[tokio::test]
async fn redacted_methods_are_not_logged() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut server = MockServer::start().await;
    let config = server.config().on_connected(|ddp| async move {
        ddp.call("login", vec![json!({"password": "SECRET_DIGEST"})])
            .await?;
        Ok(())
    });
    let (client, mut conn) = tokio::join!(DdpClient::connect(config), async {
        let mut conn = server.accept_handshake("s1").await;
        conn.answer_method("login", json!({"token": "SECRET_TOKEN"}))
            .await;
        conn
    });
    let client = client.unwrap();

    let c = client.clone();
    let save = tokio::spawn(async move {
        c.call("saveUserProfile", vec![json!({"newPassword": "SECRET_PW"})])
            .await
    });
    conn.answer_method("saveUserProfile", json!("SECRET_RESULT"))
        .await;
    within(save).await.unwrap().unwrap();

    let c = client.clone();
    let echo = tokio::spawn(async move { c.call("echo", vec![json!("VISIBLE_PARAM")]).await });
    conn.answer_method("echo", json!("VISIBLE_RESULT")).await;
    within(echo).await.unwrap().unwrap();
    client.close().await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("VISIBLE_PARAM") && logs.contains("VISIBLE_RESULT"),
        "{logs}"
    );
    assert!(logs.contains("redacted"), "{logs}");
    for secret in [
        "SECRET_DIGEST",
        "SECRET_TOKEN",
        "SECRET_PW",
        "SECRET_RESULT",
    ] {
        assert!(!logs.contains(secret), "{secret} leaked:\n{logs}");
    }
}
