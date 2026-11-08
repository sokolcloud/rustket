//! Offline cache flows through `App::update` (`crate::cache`): a session
//! writes its workspace, last room and outbox; a restart shows them before
//! the network sync, without read marks; the initial load replaces them;
//! logout deletes the cache.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use rocketchat::RcDate;
use rocketchat::models::{Message as RcMessage, Room, RoomType, Subscription, UserRef};
use rocketchat::rest::HistoryCursor;
use rustket::app::{App, Flags, SecretsMode};
use rustket::cache::Msg;
use rustket::message::{Message, SecretsReady};
use rustket_core::ServerId;
use rustket_core::cache::{CacheHandle, TimelineKey, server_dir};
use rustket_core::config::{AppConfig, ServerEntry};
use rustket_core::secrets::{MemoryStore, SecretBackend};
use rustket_core::sync::InitialData;
use rustket_core::timeline::DeliveryState;

const UID: &str = "U1";

fn ts(ms: i64) -> RcDate {
    RcDate::from_millis(ms).unwrap()
}

fn sub(rid: &str, unread: u64) -> Subscription {
    Subscription {
        id: format!("s-{rid}"),
        rid: rid.into(),
        name: rid.into(),
        t: RoomType::Channel,
        open: true,
        unread,
        alert: unread > 0,
        updated_at: Some(ts(1_000)),
        ..Default::default()
    }
}

fn room(rid: &str) -> Room {
    Room {
        id: rid.into(),
        t: RoomType::Channel,
        name: Some(rid.into()),
        updated_at: Some(ts(1_000)),
        ..Default::default()
    }
}

fn msg(id: &str, rid: &str, ms: i64) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: rid.into(),
        msg: format!("text {id}"),
        ts: Some(ts(ms)),
        updated_at: Some(ts(ms)),
        u: UserRef {
            id: "U2".into(),
            username: Some("bob".into()),
            name: None,
        },
        ..Default::default()
    }
}

/// The app with one server signed in as `UID` (last room `R1`).
fn start(dir: &Path) -> (App, ServerId) {
    let cfg_dir = dir.join("config");
    let sid = if cfg_dir.join("config.toml").exists() || cfg_dir.exists() {
        AppConfig::load(&cfg_dir).unwrap().servers[0].id.clone()
    } else {
        let mut cfg = AppConfig::default();
        let mut entry = ServerEntry::new("http://localhost:3000", "Local");
        entry.user_id = Some(UID.into());
        entry.last_room = Some("R1".into());
        let sid = cfg.add_server(entry);
        cfg.active_server = Some(sid.clone());
        cfg.save(&cfg_dir).unwrap();
        sid
    };
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(cfg_dir),
        cache_dir: Some(dir.join("images")),
        offline_cache: Some(dir.join("offline")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    let _ = app.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(MemoryStore::new()),
        backend: SecretBackend::Memory,
        tokens: vec![(sid.clone(), Some("tok".into()))],
        notices: vec![],
    }));
    (app, sid)
}

/// Opens the cache the way `cache::after_update` asked for it.
async fn open_cache(app: &mut App, dir: &Path, sid: &ServerId) {
    let opened = CacheHandle::open(
        server_dir(&dir.join("offline"), sid),
        UID.into(),
        Some(TimelineKey::room("R1")),
    )
    .await
    .map_err(|e| e.to_string());
    let _ = app.update(Message::Cache(Msg::Opened(
        sid.clone(),
        UID.into(),
        Box::new(opened),
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_shows_the_cache_until_the_initial_load() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // First run: initial load, the last room's first page, a pending message
    // written while the connection is down.
    let (mut app, sid) = start(dir);
    open_cache(&mut app, dir, &sid).await;
    assert!(app.servers[&sid].cache.is_open());
    let epoch = app.servers[&sid].generation;
    let _ = app.update(Message::InitialLoaded(
        sid.clone(),
        epoch,
        Ok(Box::new(InitialData {
            me: rocketchat::models::User {
                id: UID.into(),
                username: Some("alice".into()),
                ..Default::default()
            },
            // NEWENC: its room object has not arrived, its subscription
            // holds a room key.
            subscriptions: vec![
                sub("R1", 0),
                sub("R2", 2),
                sub("ENC", 0),
                Subscription {
                    e2e_key: Some("ROOMKEY".into()),
                    ..sub("NEWENC", 0)
                },
            ],
            rooms: vec![
                room("R1"),
                room("R2"),
                Room {
                    encrypted: Some(true),
                    ..room("ENC")
                },
            ],
            next_since: Some(ts(1)),
            ..Default::default()
        })),
    ));
    assert_eq!(app.servers[&sid].active_room.as_deref(), Some("R1"));
    {
        let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
        // The room view asked already (no session: failed); ask again.
        let (_, t) = ws.begin_load_older_ticket("R1").unwrap();
        ws.finish_load_older_ticket(
            "R1",
            t,
            HistoryCursor::new("R1".to_owned(), 50),
            Ok(vec![msg("m1", "R1", 10_000), msg("m2", "R1", 20_000)]),
        );
        let pending = ws.new_local_message("R1", "written offline", None, false);
        ws.add_pending(pending);
        let secret = ws.new_local_message("ENC", "encrypted room draft", None, false);
        ws.add_pending(secret);
        let secret = ws.new_local_message("NEWENC", "NEWENCPLAINTEXT", None, false);
        ws.add_pending(secret);
    }
    let _ = app.update(Message::Noop);
    // Settings → Storage → Clear: what is shown is written again in full,
    // unsent messages stay.
    run(rustket::cache::clear_all(&mut app)).await;
    let _ = app.update(Message::Noop);
    rustket::cache::on_quit(&app);
    let offline_dir = server_dir(&dir.join("offline"), &sid);
    for entry in std::fs::read_dir(&offline_dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for needle in ["NEWENCPLAINTEXT", "encrypted room draft"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle} on disk"
            );
        }
    }
    app.flush_config();
    drop(app);

    // Second run: offline. The cache is shown at once, the last room with
    // its messages and the pending message (queued, shown as waiting).
    let (mut app, sid) = start(dir);
    open_cache(&mut app, dir, &sid).await;
    let s = &app.servers[&sid];
    assert!(s.workspace.from_cache());
    assert_eq!(s.workspace.subscription("R2").unwrap().unread, 2);
    assert_eq!(s.active_room.as_deref(), Some("R1"));
    let t = s.workspace.timeline("R1").unwrap();
    assert!(t.has_seeded());
    let ids: Vec<&str> = t.messages().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"m1") && ids.contains(&"m2"), "{ids:?}");
    let pending: Vec<_> = s.workspace.outbox_items().collect();
    assert_eq!(pending.len(), 1, "encrypted rooms are not persisted");
    assert_eq!(pending[0].message.msg, "written offline");
    assert!(matches!(pending[0].state, DeliveryState::Failed { .. }));
    assert!(s.cache.is_queued(&pending[0].message.id));
    // No read mark while the cache is shown.
    assert!(!s.rooms["R1"].readable(&s.workspace));

    // The initial load replaces the cache: R2 is gone, R1 read.
    let epoch = s.generation;
    let _ = app.update(Message::InitialLoaded(
        sid.clone(),
        epoch,
        Ok(Box::new(InitialData {
            me: rocketchat::models::User {
                id: UID.into(),
                ..Default::default()
            },
            subscriptions: vec![sub("R1", 0)],
            rooms: vec![room("R1")],
            next_since: Some(ts(2)),
            ..Default::default()
        })),
    ));
    let s = &app.servers[&sid];
    assert!(!s.workspace.from_cache());
    assert!(s.workspace.room("R2").is_none());
    assert_eq!(
        s.workspace.outbox_items().count(),
        1,
        "still waiting to be sent"
    );

    // Logout deletes the cache.
    let cache_dir = server_dir(&dir.join("offline"), &sid);
    assert!(cache_dir.exists());
    let task = sign_out(
        &mut app,
        Message::Go(rustket::message::Go::Logout {
            sid: sid.clone(),
            everywhere: false,
        }),
    )
    .await;
    run(task).await;
    for _ in 0..50 {
        if !cache_dir.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!cache_dir.exists(), "logout deletes the offline cache");
}

/// A logout: the app first asks the secret store for an unsaved E2E
/// passphrase; the answer is fed back. Returns the task of the sign-out.
async fn sign_out(app: &mut App, m: Message) -> iced::Task<Message> {
    use iced::futures::StreamExt;
    let check = app.update(m);
    let Some(stream) = iced_runtime::task::into_stream(check) else {
        return iced::Task::none();
    };
    let outputs: Vec<Message> = stream
        .filter_map(|a| async move {
            match a {
                iced_runtime::Action::Output(m @ Message::E2e(..)) => Some(m),
                _ => None,
            }
        })
        .collect()
        .await;
    let mut last = iced::Task::none();
    for m in outputs {
        last = app.update(m);
    }
    last
}

/// Runs a task's futures (results are dropped; follow-up messages that
/// matter are fed by the test).
async fn run(task: iced::Task<Message>) {
    use iced::futures::StreamExt;
    if let Some(s) = iced_runtime::task::into_stream(task) {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), s.collect::<Vec<_>>()).await;
    }
}
