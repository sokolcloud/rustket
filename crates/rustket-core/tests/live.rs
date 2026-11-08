//! Live tests against a running Rocket.Chat (see CLAUDE.md), e.g.
//! `RUSTKET_TEST_SERVER=http://localhost:3000 cargo test -p rustket-core --test live`.
//!
//! Without `RUSTKET_TEST_SERVER` every test prints a SKIPPED banner and returns
//! (libtest cannot mark a test as skipped); with `RUSTKET_REQUIRE_LIVE=1` a missing
//! server fails instead. Each test creates its own uniquely named users and rooms
//! through a [`Fixture`] that deletes them afterwards, also when the test panics.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use meteor_ddp::{DdpClient, DdpConfig, HookError};
use rocketchat::models::{Message, OutgoingMessage, UserStatus};
use rocketchat::rest::CreateRoom;
use rocketchat::{RcDate, RestClient, ServerUrl, TlsConfig};
use rustket_core::sync::{self, RoomSync};
use rustket_core::{Workspace, emoji};
use serde_json::{Value, json};

const BODY_TIMEOUT: Duration = Duration::from_secs(240);

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            if std::env::var_os("RUSTKET_REQUIRE_LIVE").is_some() {
                panic!("RUSTKET_REQUIRE_LIVE is set but RUSTKET_TEST_SERVER is not");
            }
            eprintln!(
                "\n*** SKIPPED: live test (set RUSTKET_TEST_SERVER=http://localhost:3000 to run) ***\n"
            );
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

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = client(server);
    c.login_password(user, password, None).await.unwrap();
    c
}

struct TempUser {
    id: String,
    username: String,
    rest: RestClient,
}

/// Records what a test creates and deletes it in [`Fixture::cleanup`].
#[derive(Clone)]
struct Fixture {
    admin: RestClient,
    users: Arc<Mutex<Vec<String>>>,
    rooms: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn new(server: &ServerUrl) -> Self {
        Self {
            admin: login(server, "admin", "adminpass").await,
            users: Arc::default(),
            rooms: Arc::default(),
        }
    }

    fn room(&self, rid: &str) {
        self.rooms.lock().unwrap().push(rid.to_owned());
    }

    async fn user(&self, server: &ServerUrl, prefix: &str) -> TempUser {
        let username = unique(prefix);
        let password = format!("{username}-pw");
        let r: Value = self
            .admin
            .post(
                "users.create",
                &json!({
                    "username": username, "email": format!("{username}@rustket.local"),
                    "password": password, "name": format!("Name {username}"),
                    "verified": true, "requirePasswordChange": false,
                }),
            )
            .await
            .unwrap();
        let id = r["user"]["_id"].as_str().unwrap().to_owned();
        self.users.lock().unwrap().push(id.clone());
        let rest = login(server, &username, &password).await;
        TempUser { id, username, rest }
    }

    async fn cleanup(&self) {
        let rooms = std::mem::take(&mut *self.rooms.lock().unwrap());
        for rid in rooms {
            let _ = self.admin.rooms_delete(&rid).await;
        }
        let users = std::mem::take(&mut *self.users.lock().unwrap());
        for id in users {
            let r: Result<Value, _> = self
                .admin
                .post(
                    "users.delete",
                    &json!({ "userId": id, "confirmRelinquish": true }),
                )
                .await;
            if let Err(e) = r {
                eprintln!("cleanup: deleting user {id} failed: {e}");
            }
        }
        let _ = self.admin.logout().await;
    }
}

/// Runs `body`, then cleans up even if it panicked or timed out.
async fn with_fixture<F, Fut>(body: F)
where
    F: FnOnce(ServerUrl, Fixture) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let Some(server) = server() else { return };
    let fixture = Fixture::new(&server).await;
    let task = tokio::spawn(body(server.clone(), fixture.clone()));
    let abort = task.abort_handle();
    let result = tokio::time::timeout(BODY_TIMEOUT, task).await;
    fixture.cleanup().await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e.into_panic()),
        Err(_) => {
            abort.abort();
            panic!("test body timed out after {BODY_TIMEOUT:?}");
        }
    }
}

async fn ddp_login(server: &ServerUrl, token: String) -> DdpClient {
    let url = url::Url::parse(&server.websocket()).unwrap();
    let config = DdpConfig::new(url).on_connected(move |ddp| {
        let token = token.clone();
        async move {
            ddp.call("login", vec![json!({ "resume": token })]).await?;
            Ok::<(), HookError>(())
        }
    });
    DdpClient::connect(config).await.unwrap()
}

/// The first argument of the next event on `sub`.
async fn next_event(sub: &mut meteor_ddp::Subscription, secs: u64) -> Option<Value> {
    let r = tokio::time::timeout(Duration::from_secs(secs), sub.recv()).await;
    match r {
        Ok(Ok(m)) => m.args().and_then(|a| a.first()).cloned(),
        _ => None,
    }
}

/// Loads every remaining history page of a room.
async fn load_all(ws: &mut Workspace, rest: &RestClient, rid: &str) {
    while let Some(mut c) = ws.begin_load_older(rid) {
        let page = c.next_page(rest).await.map_err(|e| e.to_string());
        ws.finish_load_older(rid, c, page);
    }
}

fn texts(ws: &Workspace, rid: &str) -> Vec<String> {
    ws.timeline(rid)
        .unwrap()
        .messages()
        .filter(|m| !m.is_system())
        .map(|m| m.msg.clone())
        .collect()
}

/// Rocket.Chat decides server-side which messages notify, following the user's
/// `desktopNotifications` preference. With `mentions`, a DM and a mention produce
/// a `{uid}/notification` event, a plain channel message does not.
#[tokio::test(flavor = "multi_thread")]
async fn server_sends_desktop_notifications() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-nota").await;
        let b = fx.user(&s, "core-notb").await;
        let _: Value = a
            .rest
            .post(
                "users.setPreferences",
                &json!({ "userId": a.id, "data": { "desktopNotifications": "mentions" } }),
            )
            .await
            .unwrap();
        let token = a.rest.credentials().unwrap().auth_token;
        let ddp = ddp_login(&s, token).await;
        let mut sub = ddp
            .subscribe(
                "stream-notify-user",
                vec![json!(format!("{}/notification", a.id)), json!(false)],
            )
            .await
            .unwrap();

        // DM from B → notification for A.
        let dm = b.rest.im_create(&a.username).await.unwrap();
        fx.room(&dm.id);
        b.rest
            .send_message(&OutgoingMessage::new(&dm.id, "hello there"))
            .await
            .unwrap();
        let n = next_event(&mut sub, 10).await.expect("DM notification");
        assert_eq!(n["payload"]["rid"], json!(dm.id));
        assert_eq!(n["payload"]["sender"]["username"], json!(b.username));
        assert_eq!(n["payload"]["type"], json!("d"));
        assert!(n["payload"]["_id"].is_string());
        assert!(n["text"].is_string());
        assert!(n["title"].is_string());

        // Channel: plain message → nothing, mention → notification.
        let mut req = CreateRoom::new(unique("core-not"));
        req.members = vec![a.username.clone()];
        let room = b.rest.channels_create(&req).await.unwrap();
        fx.room(&room.id);
        b.rest
            .send_message(&OutgoingMessage::new(&room.id, "no mention here"))
            .await
            .unwrap();
        b.rest
            .send_message(&OutgoingMessage::new(
                &room.id,
                format!("hey @{} look", a.username),
            ))
            .await
            .unwrap();
        // Whatever arrives must be the mention, never the plain message.
        let n = next_event(&mut sub, 10)
            .await
            .expect("mention notification");
        assert_eq!(n["payload"]["rid"], json!(room.id));
        assert_eq!(n["payload"]["type"], json!("c"));
        assert!(
            n["text"].as_str().unwrap_or_default().contains("look"),
            "unexpected notification: {n:#}"
        );
        let parsed: rustket_core::notify::IncomingNotification =
            serde_json::from_value(n.clone()).unwrap();
        assert_eq!(parsed.rid(), room.id);
        ddp.close().await;
    })
    .await;
}

/// Initial load, delta sync (new, updated and deleted rooms), room message sync
/// and history pagination through the workspace.
#[tokio::test(flavor = "multi_thread")]
async fn initial_delta_and_room_sync() {
    use rustket_core::room_list::{self, RoomListOptions};
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-sync").await;
        let ra = a.rest.clone();
        let room1 = ra
            .channels_create(&CreateRoom::new(unique("core-sync1")))
            .await
            .unwrap();
        fx.room(&room1.id);
        for i in 0..5 {
            ra.send_message(&OutgoingMessage::new(&room1.id, format!("msg {i}")))
                .await
                .unwrap();
        }

        let data = sync::initial_load(&ra).await.unwrap();
        assert!(data.warnings.is_empty(), "{:?}", data.warnings);
        assert_eq!(data.me.id, a.id);
        assert!(data.settings.len() > 100);
        assert!(!data.permissions.is_empty());
        assert!(!data.roles.is_empty());
        assert!(data.subscriptions.iter().any(|s| s.rid == room1.id));
        let since = data.next_since.unwrap();
        assert!(since <= RcDate::now());
        let mut ws = Workspace::new();
        ws.apply_initial(data);
        let sections = room_list::compute(&ws, &RoomListOptions::default(), "core-sync1");
        let item = &sections[0].items[0];
        assert_eq!(item.rid, room1.id);
        assert_eq!(
            item.preview.as_ref().unwrap().text.as_deref(),
            Some("msg 4")
        );
        assert!(item.preview.as_ref().unwrap().own);
        assert!(rustket_core::permissions::can_post(&ws, &room1.id));

        ws.open_room(&room1.id);
        let mut cursor = ws.begin_load_older(&room1.id).unwrap();
        let page = cursor.next_page(&ra).await.map_err(|e| e.to_string());
        ws.finish_load_older(&room1.id, cursor, page);
        assert_eq!(
            texts(&ws, &room1.id),
            ["msg 0", "msg 1", "msg 2", "msg 3", "msg 4"]
        );
        let t = ws.timeline(&room1.id).unwrap();
        assert!(!t.pagination.has_more_before);
        let last_update = t.last_updated_at().unwrap();
        let ids: Vec<String> = t
            .messages()
            .filter(|m| !m.is_system())
            .map(|m| m.id.clone())
            .collect();

        // Room sync: one edit, one delete.
        tokio::time::sleep(Duration::from_millis(20)).await;
        ra.update_message(&room1.id, &ids[0], "edited")
            .await
            .unwrap();
        ra.delete_message(&room1.id, &ids[1]).await.unwrap();
        let rs = sync::sync_room(&ra, &room1.id, last_update).await.unwrap();
        assert!(
            rs.updated
                .iter()
                .any(|m| m.id == ids[0] && m.msg == "edited")
        );
        assert!(rs.deleted.contains(&ids[1]), "{:?}", rs.deleted);
        ws.apply_room_sync(rs);
        let t = ws.timeline(&room1.id).unwrap();
        assert_eq!(t.get(&ids[0]).unwrap().message.msg, "edited");
        assert!(!t.contains(&ids[1]));

        // Delta: a new room appears; room1's topic and the user's name change.
        let room2 = ra
            .channels_create(&CreateRoom::new(unique("core-sync2")))
            .await
            .unwrap();
        fx.room(&room2.id);
        let _: Value = ra
            .post(
                "channels.setTopic",
                &json!({ "roomId": room1.id, "topic": "delta topic" }),
            )
            .await
            .unwrap();
        let _: Value = ra
            .post(
                "users.updateOwnBasicInfo",
                &json!({ "data": { "name": "Delta Name" } }),
            )
            .await
            .unwrap();
        let delta = sync::delta_sync(&ra, since).await.unwrap();
        assert!(delta.warnings.is_empty(), "{:?}", delta.warnings);
        assert!(delta.subscriptions.update.iter().any(|s| s.rid == room2.id));
        let since2 = delta.next_since.unwrap();
        assert!(since2 >= since);
        ws.apply_delta(delta);
        assert!(ws.room(&room2.id).unwrap().sub.is_some());
        let r1 = ws.room(&room1.id).unwrap().room.as_ref().unwrap();
        assert_eq!(r1.topic.as_deref(), Some("delta topic"));
        assert_eq!(ws.me().unwrap().name.as_deref(), Some("Delta Name"));
        assert!(ws.settings().len() > 100);

        ra.rooms_delete(&room2.id).await.unwrap();
        let delta = sync::delta_sync(&ra, since2).await.unwrap();
        assert!(
            delta.rooms.remove.iter().any(|r| r.id == room2.id)
                || delta
                    .subscriptions
                    .remove
                    .iter()
                    .any(|r| r.rid.as_deref() == Some(room2.id.as_str()))
        );
        ws.apply_delta(delta);
        assert!(ws.room(&room2.id).is_none());
        assert!(ws.room(&room1.id).is_some());
    })
    .await;
}

/// The first history page fails; a thread reply then bumps the oldest message
/// over `__my_messages__`. The retry must still load everything.
#[tokio::test(flavor = "multi_thread")]
async fn failed_first_page_then_realtime_parent_bump() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-gapa").await;
        let b = fx.user(&s, "core-gapb").await;
        let mut req = CreateRoom::new(unique("core-gap"));
        req.members = vec![b.username.clone()];
        let room = a.rest.channels_create(&req).await.unwrap();
        fx.room(&room.id);
        let mut ids = Vec::new();
        for i in 0..8 {
            let m = b
                .rest
                .send_message(&OutgoingMessage::new(&room.id, format!("m{i}")))
                .await
                .unwrap();
            ids.push(m.id);
        }
        let mut ws = Workspace::new();
        ws.apply_initial(sync::initial_load(&a.rest).await.unwrap());
        let ddp = ddp_login(&s, a.rest.credentials().unwrap().auth_token).await;
        let mut sub = ddp
            .subscribe(
                "stream-room-messages",
                vec![json!("__my_messages__"), json!(false)],
            )
            .await
            .unwrap();
        ws.open_room(&room.id);
        let c = ws.begin_load_older(&room.id).unwrap();
        ws.finish_load_older(&room.id, c, Err("timeout".into()));
        b.rest
            .send_message(&OutgoingMessage::new(&room.id, "reply").in_thread(&ids[0], false))
            .await
            .unwrap();
        let mut bumped = false;
        while let Some(v) = next_event(&mut sub, 3).await {
            let m: Message = serde_json::from_value(v).unwrap();
            bumped |= m.id == ids[0];
            ws.apply_message(m);
        }
        assert!(bumped, "the parent update arrived");
        load_all(&mut ws, &a.rest, &room.id).await;
        let want: Vec<String> = (0..8).map(|i| format!("m{i}")).collect();
        assert_eq!(texts(&ws, &room.id), want);
        assert!(!ws.timeline(&room.id).unwrap().pagination.has_more_before);
        ddp.close().await;
    })
    .await;
}

/// A room sync that resets the timeline (dropped entries) and re-applies an old
/// thread parent: the reload brings back every message.
#[tokio::test(flavor = "multi_thread")]
async fn room_sync_reset_reloads_the_whole_window() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-rst").await;
        let ra = a.rest.clone();
        let room = ra
            .channels_create(&CreateRoom::new(unique("core-rst")))
            .await
            .unwrap();
        fx.room(&room.id);
        let mut ids = Vec::new();
        for i in 0..8 {
            let m = ra
                .send_message(&OutgoingMessage::new(&room.id, format!("m{i}")))
                .await
                .unwrap();
            ids.push(m.id);
        }
        let mut ws = Workspace::new();
        ws.apply_initial(sync::initial_load(&ra).await.unwrap());
        ws.open_room(&room.id);
        load_all(&mut ws, &ra, &room.id).await;
        let last = ws.timeline(&room.id).unwrap().last_updated_at().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        ra.send_message(&OutgoingMessage::new(&room.id, "reply").in_thread(&ids[1], false))
            .await
            .unwrap();
        let mut rs = sync::sync_room(&ra, &room.id, last).await.unwrap();
        assert!(
            rs.updated.iter().any(|m| m.id == ids[1]),
            "parent bump is in the sync"
        );
        rs.dropped = 1;
        ws.apply_room_sync(RoomSync { ..rs });
        load_all(&mut ws, &ra, &room.id).await;
        let want: Vec<String> = (0..8).map(|i| format!("m{i}")).collect();
        assert_eq!(texts(&ws, &room.id), want);
    })
    .await;
}

/// Newest-first thread pagination must not skip a reply when an already loaded
/// reply is deleted between pages.
#[tokio::test(flavor = "multi_thread")]
async fn thread_pagination_survives_deleted_replies() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-thr").await;
        let ra = a.rest.clone();
        let room = ra
            .channels_create(&CreateRoom::new(unique("core-thr")))
            .await
            .unwrap();
        fx.room(&room.id);
        let parent = ra
            .send_message(&OutgoingMessage::new(&room.id, "parent"))
            .await
            .unwrap();
        let mut replies = Vec::new();
        for i in 0..6 {
            let m = ra
                .send_message(
                    &OutgoingMessage::new(&room.id, format!("r{i}")).in_thread(&parent.id, false),
                )
                .await
                .unwrap();
            replies.push(m.id);
        }
        let mut ws = Workspace::new();
        ws.open_thread(&room.id, &parent.id);
        let off = ws.begin_load_thread(&parent.id).unwrap();
        let p = ra
            .thread_messages_latest(&parent.id, off, 3)
            .await
            .map_err(|e| e.to_string());
        ws.finish_load_thread(&parent.id, p);
        // The first page is the newest replies (the server honours `sort`).
        let first: Vec<String> = ws
            .thread(&parent.id)
            .unwrap()
            .messages()
            .map(|m| m.msg.clone())
            .collect();
        assert_eq!(first, ["r3", "r4", "r5"]);
        ra.delete_message(&room.id, &replies[4]).await.unwrap();
        ws.apply_message_deleted(&room.id, &replies[4]);
        while let Some(off) = ws.begin_load_thread(&parent.id) {
            let p = ra
                .thread_messages_latest(&parent.id, off, 3)
                .await
                .map_err(|e| e.to_string());
            ws.finish_load_thread(&parent.id, p);
        }
        let got: Vec<String> = ws
            .thread(&parent.id)
            .unwrap()
            .messages()
            .map(|m| m.msg.clone())
            .collect();
        assert_eq!(got, ["r0", "r1", "r2", "r3", "r5"]);
    })
    .await;
}

/// A DM partner who went offline while the client was disconnected is shown
/// offline after the delta sync, also with a `since` older than 10 minutes
/// (`users.presence` then returns the full list of non-offline users).
#[tokio::test(flavor = "multi_thread")]
async fn presence_goes_offline_after_reconnect() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-pra").await;
        let b = fx.user(&s, "core-prb").await;
        let _: Value = b
            .rest
            .post("users.setStatus", &json!({ "status": "online" }))
            .await
            .unwrap();
        let dm = a.rest.im_create(&b.username).await.unwrap();
        fx.room(&dm.id);
        let data = sync::initial_load(&a.rest).await.unwrap();
        let since = data.next_since.unwrap();
        let mut ws = Workspace::new();
        ws.apply_initial(data);
        assert_eq!(ws.presence().status(&b.id), UserStatus::Online);
        let _: Value = b
            .rest
            .post("users.setStatus", &json!({ "status": "offline" }))
            .await
            .unwrap();
        // Reconnect more than 10 minutes after the last REST sync.
        let old = RcDate::from_millis(RcDate::now().timestamp_millis() - 3_600_000).unwrap();
        let mut d = sync::delta_sync(&a.rest, old).await.unwrap();
        assert!(d.presence_full);
        d.next_since = Some(since);
        ws.apply_delta(d);
        assert_eq!(ws.presence().status(&b.id), UserStatus::Offline);
        // A fresh initial load on the same workspace agrees.
        let _: Value = b
            .rest
            .post("users.setStatus", &json!({ "status": "online" }))
            .await
            .unwrap();
        ws.apply_initial(sync::initial_load(&a.rest).await.unwrap());
        assert_eq!(ws.presence().status(&b.id), UserStatus::Online);
        let _: Value = b
            .rest
            .post("users.setStatus", &json!({ "status": "offline" }))
            .await
            .unwrap();
        ws.apply_initial(sync::initial_load(&a.rest).await.unwrap());
        assert_eq!(ws.presence().status(&b.id), UserStatus::Offline);
    })
    .await;
}

/// A DM deleted and opened again while the client was offline stays in the
/// workspace after the delta (same rid, removal and re-creation in one delta).
#[tokio::test(flavor = "multi_thread")]
async fn recreated_dm_survives_delta() {
    with_fixture(|s, fx| async move {
        let a = fx.user(&s, "core-dma").await;
        let b = fx.user(&s, "core-dmb").await;
        let dm = a.rest.im_create(&b.username).await.unwrap();
        fx.room(&dm.id);
        let data = sync::initial_load(&a.rest).await.unwrap();
        let since = data.next_since.unwrap();
        let mut ws = Workspace::new();
        ws.apply_initial(data);
        assert!(ws.room(&dm.id).is_some());
        tokio::time::sleep(Duration::from_millis(50)).await;
        fx.admin.rooms_delete(&dm.id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let dm2 = a.rest.im_create(&b.username).await.unwrap();
        fx.room(&dm2.id);
        ws.apply_delta(sync::delta_sync(&a.rest, since).await.unwrap());
        assert!(ws.room(&dm2.id).and_then(|e| e.sub.as_ref()).is_some());
    })
    .await;
}

/// Every key the emoji picker offers (all tones included) is accepted by the
/// server as a reaction.
#[tokio::test(flavor = "multi_thread")]
async fn every_picker_key_is_a_valid_reaction() {
    with_fixture(|_s, fx| async move {
        let admin = fx.admin.clone();
        let room = admin
            .groups_create(&CreateRoom::new(unique("core-emoji")))
            .await
            .unwrap();
        fx.room(&room.id);
        let mut keys = Vec::new();
        for cat in rustket_core::emoji::EmojiRegistry::new().picker_categories(&[]) {
            for e in cat.emojis {
                for tone in 1..=5 {
                    if let Some(t) = emoji::with_skin_tone(&e.key, tone) {
                        keys.push(t);
                    }
                }
                keys.push(e.key);
            }
        }
        assert!(keys.len() > 3000, "{}", keys.len());
        let mut rejected = Vec::new();
        for chunk in keys.chunks(250) {
            let m = admin
                .send_message(&OutgoingMessage::new(&room.id, "reactions"))
                .await
                .unwrap();
            let mut set = tokio::task::JoinSet::new();
            for group in chunk.chunks(chunk.len().div_ceil(8)) {
                let (admin, mid, group) = (admin.clone(), m.id.clone(), group.to_vec());
                set.spawn(async move {
                    let mut bad = Vec::new();
                    for k in group {
                        if let Err(e) = admin.react(&mid, &k, Some(true)).await {
                            bad.push(format!("{k}: {e}"));
                        }
                    }
                    bad
                });
            }
            while let Some(r) = set.join_next().await {
                rejected.extend(r.unwrap());
            }
        }
        assert!(
            rejected.is_empty(),
            "{} rejected: {:?}",
            rejected.len(),
            &rejected[..rejected.len().min(20)]
        );
    })
    .await;
}
