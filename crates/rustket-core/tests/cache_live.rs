//! Live offline-cache test (see CLAUDE.md): the client caches a workspace,
//! goes "offline", messages are edited and deleted and a room is deleted
//! meanwhile, the client restarts from the cache and reconnects: the server's
//! state wins everywhere. Also checks that a persisted outbox message sent
//! twice exists once (the server keeps the first message with an `_id`).
//!
//! `RUSTKET_TEST_SERVER=http://localhost:3000 cargo test -p rustket-core --test cache_live`

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rocketchat::models::{OutgoingMessage, RoomType};
use rocketchat::rest::CreateRoom;
use rocketchat::{RestClient, ServerUrl, TlsConfig};
use rustket_core::Workspace;
use rustket_core::cache::{Batch, CacheDb, CachedState, CachedTimeline, OutboxEntry, TimelineKey};
use rustket_core::sync;
use serde_json::{Value, json};

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("\n*** SKIPPED: live cache test (set RUSTKET_TEST_SERVER) ***\n");
            None
        }
    }
}

fn client(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    RestClient::new(server.clone(), &tls).unwrap()
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

/// The workspace's documents as the app writes them after an initial load.
fn state_of(ws: &Workspace) -> CachedState {
    CachedState {
        me: ws.me().cloned(),
        subscriptions: ws.rooms().values().filter_map(|e| e.sub.clone()).collect(),
        rooms: ws.rooms().values().filter_map(|e| e.room.clone()).collect(),
        ..Default::default()
    }
}

async fn first_page(ws: &mut Workspace, rest: &RestClient, rid: &str) {
    let (mut c, t) = ws.begin_load_older_ticket(rid).unwrap();
    let page = c.next_page(rest).await.map_err(|e| e.to_string());
    ws.finish_load_older_ticket(rid, t, c, page);
}

fn texts(ws: &Workspace, rid: &str) -> Vec<(String, String)> {
    ws.timeline(rid)
        .unwrap()
        .messages()
        .filter(|m| !m.is_system())
        .map(|m| (m.id.clone(), m.msg.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_edits_deletions_and_outbox_reconcile() {
    let Some(server) = server() else { return };
    let admin = client(&server);
    admin
        .login_password("admin", "adminpass", None)
        .await
        .unwrap();
    let users: Arc<Mutex<Vec<String>>> = Arc::default();
    let rooms: Arc<Mutex<Vec<String>>> = Arc::default();
    let body = {
        let (server, admin, users, rooms) =
            (server.clone(), admin.clone(), users.clone(), rooms.clone());
        tokio::spawn(async move {
            let name = unique("core-cache");
            let password = format!("{name}-pw");
            let r: Value = admin
                .post(
                    "users.create",
                    &json!({
                        "username": name, "email": format!("{name}@rustket.local"),
                        "password": password, "name": "Cache Test",
                        "verified": true, "requirePasswordChange": false,
                    }),
                )
                .await
                .unwrap();
            users
                .lock()
                .unwrap()
                .push(r["user"]["_id"].as_str().unwrap().to_owned());
            let rest = client(&server);
            rest.login_password(&name, &password, None).await.unwrap();
            let keep = rest
                .channels_create(&CreateRoom::new(unique("core-cache-keep")))
                .await
                .unwrap();
            rooms.lock().unwrap().push(keep.id.clone());
            let doomed = rest
                .channels_create(&CreateRoom::new(unique("core-cache-gone")))
                .await
                .unwrap();
            rooms.lock().unwrap().push(doomed.id.clone());
            let mut ids = Vec::new();
            for i in 0..4 {
                let m = rest
                    .send_message(&OutgoingMessage::new(&keep.id, format!("cached {i}")))
                    .await
                    .unwrap();
                ids.push(m.id);
            }

            // Online: initial load and the room's first page, written to the cache.
            let tmp = tempfile::tempdir().unwrap();
            let (mut db, _) = CacheDb::open(tmp.path(), &r["user"]["_id"].to_string()).unwrap();
            let mut ws = Workspace::new();
            ws.apply_initial(sync::initial_load(&rest).await.unwrap());
            ws.open_room(&keep.id);
            first_page(&mut ws, &rest, &keep.id).await;
            let messages: Vec<_> = ws.timeline(&keep.id).unwrap().messages().cloned().collect();
            db.write(Batch {
                full: Some(Box::new(state_of(&ws))),
                timelines: vec![CachedTimeline {
                    key: TimelineKey::room(keep.id.as_str()),
                    parent: None,
                    messages,
                }],
                ..Default::default()
            })
            .unwrap();
            assert_eq!(texts(&ws, &keep.id).len(), 4);

            // Offline: an edit, a deletion, a deleted room; the outbox holds a
            // message that was (unknowingly) delivered before the app quit.
            tokio::time::sleep(Duration::from_millis(20)).await;
            rest.update_message(&keep.id, &ids[1], "edited while offline")
                .await
                .unwrap();
            rest.delete_message(&keep.id, &ids[2]).await.unwrap();
            admin.rooms_delete(&doomed.id).await.unwrap();
            let mut pending = ws.new_local_message(&keep.id, "queued once", None, false);
            pending.id = rocketchat::ids::random_id();
            let mut out = OutgoingMessage::new(&keep.id, "queued once");
            out.id = Some(pending.id.clone());
            rest.send_message(&out).await.unwrap();
            db.write(Batch {
                outbox: Some(vec![OutboxEntry {
                    message: pending.clone(),
                    queued: true,
                    maybe_sent: true,
                    error: None,
                }]),
                ..Default::default()
            })
            .unwrap();
            drop(db);

            // Restart from the cache: the stale state is shown.
            let (mut db, _) = CacheDb::open(tmp.path(), &r["user"]["_id"].to_string()).unwrap();
            let mut ws = Workspace::new();
            assert!(ws.apply_cached(db.load_state().unwrap()));
            assert!(ws.from_cache());
            assert!(ws.room(&doomed.id).is_some());
            ws.open_room(&keep.id);
            let cached = db
                .load_timeline(&TimelineKey::room(keep.id.as_str()))
                .unwrap()
                .unwrap();
            ws.seed_cached_messages(&keep.id, None, None, cached.messages);
            let shown = texts(&ws, &keep.id);
            assert!(shown.iter().any(|(id, _)| *id == ids[2]), "{shown:?}");

            // Reconnect: initial load, then the first page.
            ws.apply_initial(sync::initial_load(&rest).await.unwrap());
            assert!(!ws.from_cache());
            assert!(ws.room(&doomed.id).is_none(), "deleted room stays gone");
            assert_eq!(ws.room(&keep.id).unwrap().room_type(), RoomType::Channel);
            first_page(&mut ws, &rest, &keep.id).await;
            let shown = texts(&ws, &keep.id);
            assert!(!shown.iter().any(|(id, _)| *id == ids[2]), "{shown:?}");
            assert!(
                shown
                    .iter()
                    .any(|(id, t)| *id == ids[1] && t == "edited while offline"),
                "{shown:?}"
            );

            // The queued message is sent again after the restart: the server
            // refuses the duplicate id and the message exists exactly once.
            let restored = db.load_outbox().unwrap();
            assert_eq!(restored.len(), 1);
            let mut again = OutgoingMessage::new(&keep.id, "queued once");
            again.id = Some(restored[0].message.id.clone());
            assert!(rest.send_message(&again).await.is_err());
            let found = rest.get_message(&pending.id).await.unwrap();
            assert_eq!(found.msg, "queued once");
            let history: Value = rest
                .get(
                    "channels.history",
                    &rocketchat::rest::Query::new()
                        .push("roomId", &keep.id)
                        .push("count", "50"),
                )
                .await
                .unwrap();
            let copies = history["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["msg"] == "queued once")
                .count();
            assert_eq!(copies, 1);
            let _ = rest.logout().await;
        })
    };
    let result = tokio::time::timeout(Duration::from_secs(120), body).await;
    let created_rooms = std::mem::take(&mut *rooms.lock().unwrap());
    for rid in created_rooms {
        let _ = admin.rooms_delete(&rid).await;
    }
    let created_users = std::mem::take(&mut *users.lock().unwrap());
    for id in created_users {
        let _: Result<Value, _> = admin
            .post(
                "users.delete",
                &json!({ "userId": id, "confirmRelinquish": true }),
            )
            .await;
    }
    let _ = admin.logout().await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e.into_panic()),
        Err(_) => panic!("timed out"),
    }
}
