//! Live tests against a real Rocket.Chat server; skipped unless
//! `RUSTKET_TEST_SERVER` is set (e.g. `http://localhost:3000`).
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use meteor_ddp::{CallError, DdpClient, DdpConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(20);

fn ws_url() -> Option<url::Url> {
    let Ok(server) = std::env::var("RUSTKET_TEST_SERVER") else {
        eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
        return None;
    };
    let mut url = url::Url::parse(&server).expect("RUSTKET_TEST_SERVER is not a URL");
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).expect("scheme");
    let path = format!("{}/websocket", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Some(url)
}

fn password_login(user: &str, password: &str) -> Vec<Value> {
    let digest = hex::encode(Sha256::digest(password.as_bytes()));
    vec![json!({
        "user": {"username": user},
        "password": {"digest": digest, "algorithm": "sha-256"}
    })]
}

fn random_id() -> String {
    const CHARS: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTWXYZabcdefghijkmnopqrstuvwxyz";
    (0..17)
        .map(|_| CHARS[rand::random_range(0..CHARS.len())] as char)
        .collect()
}

async fn connect(url: &url::Url) -> DdpClient {
    tokio::time::timeout(WAIT, DdpClient::connect(DdpConfig::new(url.clone())))
        .await
        .expect("connect timed out")
        .expect("connect failed")
}

#[tokio::test]
async fn live_login_subscribe_send_and_resume() {
    let Some(url) = ws_url() else { return };
    let client = connect(&url).await;

    let login = client
        .call("login", password_login("alice", "alicepass"))
        .await
        .expect("login");
    let token = login["token"].as_str().expect("token").to_owned();
    let user_id = login["id"].as_str().expect("id").to_owned();
    assert_eq!(login["type"], "password");

    let mut sub = client
        .subscribe(
            "stream-room-messages",
            vec![
                json!("GENERAL"),
                json!({"useCollection": false, "args": []}),
            ],
        )
        .await
        .expect("subscribe");

    let text = format!("ddp live {}", random_id());
    let sent = client
        .call(
            "sendMessage",
            vec![json!({"_id": random_id(), "rid": "GENERAL", "msg": text})],
        )
        .await
        .expect("sendMessage");
    assert_eq!(sent["msg"], text.as_str());

    let event = tokio::time::timeout(WAIT, async {
        loop {
            let event = sub.recv().await.expect("subscription ended");
            if event
                .args()
                .and_then(|a| a.first())
                .is_some_and(|m| m["msg"] == text.as_str())
            {
                return event;
            }
        }
    })
    .await
    .expect("message event did not arrive");
    assert_eq!(event.collection, "stream-room-messages");
    assert_eq!(event.event_name(), Some("GENERAL"));
    client.close().await;

    // Resume on a second connection.
    let second = connect(&url).await;
    let resumed = second
        .call("login", vec![json!({"resume": token})])
        .await
        .expect("resume login");
    assert_eq!(resumed["id"], user_id.as_str());
    assert_eq!(resumed["type"], "resume");
    second.close().await;
}

#[tokio::test]
async fn live_wrong_password_is_server_error() {
    let Some(url) = ws_url() else { return };
    let client = connect(&url).await;
    let err = client
        .call("login", password_login("alice", "wrong-password"))
        .await
        .expect_err("login with a wrong password must fail");
    let CallError::Server(e) = err else {
        panic!("expected a server error, got {err:?}");
    };
    // Rocket.Chat 8.9.1 answers `{"error":401,"reason":"User not found"}`.
    assert!(e.is("401") || e.is("403"), "{e:?}");
    assert_eq!(e.error_type.as_deref(), Some("Meteor.Error"));
    client.close().await;
}

async fn next_matching(
    sub: &mut meteor_ddp::Subscription,
    pred: impl Fn(&meteor_ddp::DataMessage) -> bool,
) -> std::sync::Arc<meteor_ddp::DataMessage> {
    tokio::time::timeout(WAIT, async {
        loop {
            let event = sub.recv().await.expect("subscription ended or lagged");
            if pred(&event) {
                return event;
            }
        }
    })
    .await
    .expect("event did not arrive")
}

fn message_text(event: &meteor_ddp::DataMessage) -> Option<String> {
    event
        .args()
        .and_then(|a| a.first())
        .and_then(|m| m["msg"].as_str())
        .map(str::to_owned)
}

#[tokio::test]
async fn live_presence_events_reach_subscription() {
    let Some(url) = ws_url() else { return };
    let alice = connect(&url).await;
    let me = alice
        .call("login", password_login("alice", "alicepass"))
        .await
        .expect("alice login");
    let bob = connect(&url).await;
    let bob_user = bob
        .call("login", password_login("bob", "bobpass"))
        .await
        .expect("bob login");
    let bob_id = bob_user["id"].as_str().expect("id").to_owned();

    let mut presence = alice
        .subscribe(
            "stream-user-presence",
            vec![json!(""), json!({"added": [me["id"], bob_id]})],
        )
        .await
        .expect("subscribe presence");
    bob.call("UserPresence:away", vec![]).await.expect("away");
    let event = next_matching(&mut presence, |e| e.event_name() == Some(bob_id.as_str())).await;
    assert_eq!(event.collection, "stream-user-presence");
    bob.call("UserPresence:online", vec![])
        .await
        .expect("online");
    alice.close().await;
    bob.close().await;
}

#[tokio::test]
async fn live_identical_subscriptions_get_one_copy_each() {
    let Some(url) = ws_url() else { return };
    let client = connect(&url).await;
    client
        .call("login", password_login("alice", "alicepass"))
        .await
        .expect("login");
    let params = || {
        vec![
            json!("GENERAL"),
            json!({"useCollection": false, "args": []}),
        ]
    };
    let mut g1 = client
        .subscribe("stream-room-messages", params())
        .await
        .expect("sub 1");
    let mut g2 = client
        .subscribe("stream-room-messages", params())
        .await
        .expect("sub 2");
    assert_eq!(g1.id(), g2.id());

    let tag = random_id();
    let first = format!("ddp dedup {tag} one");
    let second = format!("ddp dedup {tag} two");
    for text in [&first, &second] {
        client
            .call(
                "sendMessage",
                vec![json!({"_id": random_id(), "rid": "GENERAL", "msg": text})],
            )
            .await
            .expect("sendMessage");
    }
    // Each handle sees `one` then `two`; a duplicate `one` would show up in between.
    for sub in [&mut g1, &mut g2] {
        let mine = |e: &meteor_ddp::DataMessage| {
            message_text(e).is_some_and(|t| t.starts_with(&format!("ddp dedup {tag}")))
        };
        assert_eq!(
            message_text(&*next_matching(sub, mine).await),
            Some(first.clone())
        );
        assert_eq!(
            message_text(&*next_matching(sub, mine).await),
            Some(second.clone())
        );
    }
    client.close().await;
}
