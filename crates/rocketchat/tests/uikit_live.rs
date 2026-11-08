//! Live UIKit / apps-engine tests against the dev server (see CLAUDE.md).
//! Skipped unless `RUSTKET_TEST_SERVER` is set. Read-only for global state: no
//! app is installed and no setting is changed; throwaway users and rooms are
//! deleted at the end.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use rocketchat::models::OutgoingMessage;
use rocketchat::rest::CreateRoom;
use rocketchat::session::{Session, SessionEvent, SessionEvents, SessionOptions};
use rocketchat::uikit::{
    BlockActionPayload, Container, Element, InteractionResponse, LayoutBlock, UserInteraction,
};
use rocketchat::{Error, RestClient, ServerUrl, TlsConfig};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

fn client(server: &ServerUrl) -> RestClient {
    RestClient::new(server.clone(), &TlsConfig::new().build().unwrap()).unwrap()
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = client(server);
    c.login_password(user, password, None)
        .await
        .unwrap_or_else(|e| panic!("login {user}: {e}"));
    c
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

/// Creates a throwaway user; returns `(id, username, logged-in client)`.
async fn temp_user(
    admin: &RestClient,
    server: &ServerUrl,
    prefix: &str,
) -> (String, String, RestClient) {
    let username = unique(prefix);
    let password = format!("{username}-pw");
    let r: Value = admin
        .post(
            "users.create",
            &json!({
                "username": username, "email": format!("{username}@rustket.local"),
                "password": password, "name": format!("Real {username}"), "verified": true,
                "requirePasswordChange": false,
            }),
        )
        .await
        .unwrap();
    let id = r["user"]["_id"].as_str().unwrap().to_owned();
    let rest = login(server, &username, &password).await;
    (id, username, rest)
}

async fn delete_user(admin: &RestClient, id: &str) {
    let r: Result<Value, _> = admin
        .post(
            "users.delete",
            &json!({ "userId": id, "confirmRelinquish": true }),
        )
        .await;
    if let Err(e) = r {
        eprintln!("cleanup: deleting user {id} failed: {e}");
    }
}

async fn expect_event(
    events: &mut SessionEvents,
    mut pred: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(e)) if pred(&e) => return e,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream ended"),
            Err(_) => panic!("timed out waiting for event"),
        }
    }
}

#[tokio::test]
async fn apps_read_only_endpoints() {
    let Some(server) = server() else { return };
    // Public endpoints work without login.
    let anon = client(&server);
    assert!(anon.action_buttons().await.is_ok());
    let count = anon.apps_count().await.unwrap();
    assert!(count.max_marketplace_apps > 0 || count.total_marketplace_enabled == 0);
    anon.apps_languages().await.unwrap();

    let admin = login(&server, "admin", "adminpass").await;
    let buttons = admin.action_buttons().await.unwrap();
    // No app is installed on the dev server; whatever is there must decode.
    eprintln!("{} action buttons", buttons.len());
    admin.installed_apps().await.unwrap();
    admin.apps_languages().await.unwrap();
    let logs = admin
        .apps_logs(
            None,
            &rocketchat::models::AppLogsQuery {
                count: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(logs.logs.len() <= 2);
    admin.banners().await.unwrap();
    // Unknown app ids: 404 with `No App found`.
    let e = admin.app_settings("rustket-no-such-app").await.unwrap_err();
    assert!(matches!(e, Error::Api { status: 404, .. }), "{e:?}");
    let e = admin.app_status("rustket-no-such-app").await.unwrap_err();
    assert!(matches!(e, Error::Api { status: 404, .. }), "{e:?}");
    admin.logout().await.unwrap();
}

#[tokio::test]
async fn ui_interaction_error_shapes() {
    let Some(server) = server() else { return };
    let i = UserInteraction::BlockAction {
        action_id: "a".into(),
        payload: BlockActionPayload {
            block_id: "b".into(),
            value: Some(json!("v")),
        },
        container: Container::Message("m".into()),
        mid: Some("m".into()),
        tmid: None,
        rid: Some("r".into()),
        trigger_id: rocketchat::ids::random_id(),
    };
    // Not logged in: 401.
    let e = client(&server)
        .ui_interaction("rustket-no-such-app", &i)
        .await
        .unwrap_err();
    assert!(e.is_unauthorized(), "{e:?}");

    let alice = login(&server, "alice", "alicepass").await;
    let e = alice
        .ui_interaction("rustket-no-such-app", &i)
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Api { status: 500, error: Some(m), .. } if m.contains("Cannot read properties")),
        "{e:?}"
    );
    // A core app without an actionButton handler.
    let button = UserInteraction::ActionButton {
        action_id: "x".into(),
        payload: rocketchat::uikit::ActionButtonPayload {
            context: rocketchat::models::ButtonContext::RoomAction,
            message: None,
        },
        mid: None,
        tmid: None,
        rid: Some("r".into()),
        trigger_id: rocketchat::ids::random_id(),
    };
    let e = alice
        .ui_interaction("videoconf-core", &button)
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Api { status: 500, error: Some(m), .. } if m == "Type not supported"),
        "{e:?}"
    );
    alice.logout().await.unwrap();
}

/// §10.4: a mention of a non-member yields a `mention-core` ephemeral with
/// blocks; `dismiss` answers `{}` and replaces it under the same `_id`.
#[tokio::test(flavor = "multi_thread")]
async fn mention_core_ephemeral_and_dismiss() {
    let Some(server) = server() else { return };
    let admin = login(&server, "admin", "adminpass").await;
    let (a_id, _a_name, a) = temp_user(&admin, &server, "uka").await;
    let (b_id, b_name, _b) = temp_user(&admin, &server, "ukb").await;
    let room = a
        .channels_create(&CreateRoom::new(unique("uikit-live")))
        .await
        .unwrap();
    let rid = room.id.clone();

    let (server2, rid2, a2, b_name2) = (server.clone(), rid.clone(), a.clone(), b_name.clone());
    let body = tokio::spawn(async move {
        let (server, rid, a, b_name) = (server2, rid2, a2, b_name2);
        let (session, mut events) = tokio::time::timeout(
            WAIT,
            Session::start(
                server.clone(),
                &TlsConfig::new().build().unwrap(),
                a.credentials().unwrap(),
                SessionOptions::default(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        expect_event(&mut events, |e| matches!(e, SessionEvent::Ready { .. })).await;

        a.send_message(&OutgoingMessage::new(&rid, format!("hello @{b_name}")))
            .await
            .unwrap();
        let ev = expect_event(
            &mut events,
            |e| matches!(e, SessionEvent::EphemeralMessage(m) if m.rid == rid && m.has_blocks()),
        )
        .await;
        let SessionEvent::EphemeralMessage(m) = ev else {
            unreachable!()
        };
        assert_eq!(m.u.id, "rocket.cat");
        let blocks = m.uikit_blocks();
        let actions = blocks
            .iter()
            .find_map(|b| match b {
                LayoutBlock::Actions { meta, elements } => Some((meta.clone(), elements.clone())),
                _ => None,
            })
            .expect("actions block");
        assert_eq!(actions.0.app_id.as_deref(), Some("mention-core"));
        let dismiss = actions
            .1
            .iter()
            .find(|e| e.action_id() == Some("dismiss"))
            .expect("dismiss button")
            .inherited(&actions.0);
        let Element::Button { a: act, value, .. } = &dismiss else {
            panic!("dismiss is a button")
        };

        let i = UserInteraction::BlockAction {
            action_id: "dismiss".into(),
            payload: BlockActionPayload {
                block_id: act.block_id.clone().unwrap(),
                value: value.clone().map(Value::String),
            },
            container: Container::Message(m.id.clone()),
            mid: Some(m.id.clone()),
            tmid: None,
            rid: Some(rid.clone()),
            trigger_id: rocketchat::ids::random_id(),
        };
        let resp = a
            .ui_interaction(act.app_id.as_deref().unwrap(), &i)
            .await
            .unwrap();
        assert_eq!(resp, InteractionResponse::Ack);
        let ev = expect_event(
            &mut events,
            |e| matches!(e, SessionEvent::EphemeralMessage(r) if r.id == m.id),
        )
        .await;
        let SessionEvent::EphemeralMessage(r) = ev else {
            unreachable!()
        };
        assert!(!r.has_blocks());
        assert!(r.msg.contains(&b_name));
        session.close().await;
    });
    let abort = body.abort_handle();
    let result = tokio::time::timeout(Duration::from_secs(90), body).await;
    abort.abort();
    let _ = admin.rooms_delete(&rid).await;
    delete_user(&admin, &a_id).await;
    delete_user(&admin, &b_id).await;
    let _ = admin.logout().await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e.into_panic()),
        Err(_) => panic!("live test timed out"),
    }
}
