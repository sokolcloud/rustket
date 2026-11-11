//! Live tests of the realtime session against the dev server (see CLAUDE.md).
//! Skipped unless `RUSTKET_TEST_SERVER` is set. Every test creates its own users
//! and rooms and deletes them at the end.

#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rocketchat::models::{OutgoingMessage, UserStatus};
use rocketchat::rest::CreateRoom;
use rocketchat::session::{
    ChangeKind, ConnectionState, ResyncReason, RoomWatch, Session, SessionEvent, SessionEvents,
    SessionOptions,
};
use rocketchat::{Credentials, RestClient, ServerUrl, TlsConfig, TlsContext};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
const BODY_TIMEOUT: Duration = Duration::from_secs(120);

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

fn tls() -> TlsContext {
    TlsConfig::new().build().unwrap()
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = RestClient::new(server.clone(), &tls()).unwrap();
    c.login_password(user, password, None)
        .await
        .unwrap_or_else(|e| panic!("login {user}: {e}"));
    c
}

struct TempUser {
    id: String,
    username: String,
    rest: RestClient,
}

impl TempUser {
    fn credentials(&self) -> Credentials {
        self.rest.credentials().unwrap()
    }
}

/// Admin-side fixture: throwaway users and rooms, removed by [`Fixture::cleanup`]
/// (also when the test panics).
#[derive(Clone)]
struct Fixture {
    server: ServerUrl,
    admin: RestClient,
    users: Arc<Mutex<Vec<String>>>,
    rooms: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn new(server: &ServerUrl) -> Self {
        Self {
            server: server.clone(),
            admin: login(server, "admin", "adminpass").await,
            users: Arc::default(),
            rooms: Arc::default(),
        }
    }

    fn add_room(&self, rid: &str) {
        self.rooms.lock().unwrap().push(rid.to_owned());
    }

    async fn user(&self, prefix: &str) -> TempUser {
        let username = unique(prefix);
        let password = format!("{username}-pw");
        let r: Value = self
            .admin
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
        self.users.lock().unwrap().push(id.clone());
        let rest = login(&self.server, &username, &password).await;
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

/// Runs `body`, then cleans up even if it panicked.
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

async fn start(server: &ServerUrl, user: &TempUser) -> (Session, SessionEvents) {
    let (session, mut events) = tokio::time::timeout(
        WAIT,
        Session::start(
            server.clone(),
            &tls(),
            user.credentials(),
            SessionOptions::default(),
        ),
    )
    .await
    .expect("session start timed out")
    .expect("session start");
    let ready = expect_event(
        &mut events,
        |e| matches!(e, SessionEvent::Ready { .. }),
        WAIT,
    )
    .await;
    let SessionEvent::Ready { me } = ready else {
        unreachable!()
    };
    assert_eq!(me.id, user.id);
    assert_eq!(me.username.as_deref(), Some(user.username.as_str()));
    (session, events)
}

/// Waits for the first event matching `pred`, skipping others.
async fn expect_event(
    events: &mut SessionEvents,
    mut pred: impl FnMut(&SessionEvent) -> bool,
    timeout: Duration,
) -> SessionEvent {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(e)) if pred(&e) => return e,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream ended"),
            Err(_) => panic!("timed out waiting for event"),
        }
    }
}

/// Collects events matching `pred` for `window`.
async fn collect_for(
    events: &mut SessionEvents,
    mut pred: impl FnMut(&SessionEvent) -> bool,
    window: Duration,
) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    while let Ok(Some(e)) = tokio::time::timeout_at(deadline, events.recv()).await {
        if pred(&e) {
            out.push(e);
        }
    }
    out
}

async fn shared_channel(fx: &Fixture, owner: &TempUser, member: &TempUser) -> String {
    let room = owner
        .rest
        .channels_create(&CreateRoom {
            members: vec![member.username.clone()],
            ..CreateRoom::new(unique("sess-room"))
        })
        .await
        .unwrap();
    fx.add_room(&room.id);
    room.id
}

async fn post(user: &TempUser, rid: &str, text: &str) -> String {
    user.rest
        .send_message(&OutgoingMessage {
            rid: rid.to_owned(),
            msg: text.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

fn is_deleted(id: &str) -> impl FnMut(&SessionEvent) -> bool + '_ {
    move |e| matches!(e, SessionEvent::MessageDeleted { id: i, .. } if i == id)
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_edits_deletes_and_typing() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let b = fx.user("sb").await;
        let rid = shared_channel(&fx, &b, &a).await;
        let (sa, mut ea) = start(&server, &a).await;
        let (sb, _eb) = start(&server, &b).await;

        // New message through __my_messages__.
        let id = post(&b, &rid, "live hello").await;
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::Message { message, .. } if message.id == id),
            WAIT,
        )
        .await;
        let SessionEvent::Message {
            message: m,
            room_participant,
        } = ev
        else {
            unreachable!()
        };
        assert!(room_participant);
        assert_eq!(m.rid, rid);
        assert_eq!(m.msg, "live hello");
        assert!(m.edited_at.is_none());

        // Edit.
        b.rest.update_message(&rid, &id, "live edited").await.unwrap();
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::Message { message, .. } if message.id == id && message.msg == "live edited"),
            WAIT,
        )
        .await;
        let SessionEvent::Message { message: m, .. } = ev else {
            unreachable!()
        };
        assert!(m.edited_at.is_some());

        // Watch twice: events are delivered once.
        let w1: RoomWatch = sa.watch_room(&rid).await.unwrap();
        let w2 = sa.watch_room(&rid).await.unwrap();
        assert_eq!(w2.rid(), rid);

        // Typing (bob's DDP method call reaches alice), then stop.
        sb.set_user_activity(&rid, None, &["user-typing"]).await.unwrap();
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::UserActivity { activities, .. } if !activities.is_empty()),
            WAIT,
        )
        .await;
        assert_eq!(
            ev,
            SessionEvent::UserActivity {
                rid: rid.clone(),
                username: b.username.clone(),
                activities: vec!["user-typing".into()],
                tmid: None,
            }
        );
        sb.set_user_activity(&rid, Some("thread-1"), &[]).await.unwrap();
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::UserActivity { .. }),
            WAIT,
        )
        .await;
        assert_eq!(
            ev,
            SessionEvent::UserActivity {
                rid: rid.clone(),
                username: b.username.clone(),
                activities: vec![],
                tmid: Some("thread-1".into()),
            }
        );

        // Delete: exactly one MessageDeleted although the room is watched twice.
        b.rest.delete_message(&rid, &id).await.unwrap();
        expect_event(&mut ea, is_deleted(&id), WAIT).await;
        let dups = collect_for(&mut ea, is_deleted(&id), Duration::from_secs(2)).await;
        assert!(dups.is_empty(), "duplicate delete events: {dups:?}");

        // One watch left: still delivered (once).
        drop(w2);
        let id2 = post(&b, &rid, "second").await;
        b.rest.delete_message(&rid, &id2).await.unwrap();
        let got = collect_for(&mut ea, is_deleted(&id2), Duration::from_secs(4)).await;
        assert_eq!(got.len(), 1, "{got:?}");

        // No watch: no delete events.
        drop(w1);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let id3 = post(&b, &rid, "third").await;
        b.rest.delete_message(&rid, &id3).await.unwrap();
        let got = collect_for(&mut ea, is_deleted(&id3), Duration::from_secs(3)).await;
        assert!(got.is_empty(), "{got:?}");

        sa.close().await;
        sb.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_message_and_presence() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let b = fx.user("sb").await;
        let (sa, mut ea) = start(&server, &a).await;
        let (sb, _eb) = start(&server, &b).await;

        // B creates a DM with A.
        let dm = b.rest.im_create(&a.username).await.unwrap();
        fx.add_room(&dm.id);
        let dm_id = dm.id.clone();
        let mut sub_seen = false;
        let mut room_seen = false;
        while !(sub_seen && room_seen) {
            match expect_event(
                &mut ea,
                |e| {
                    matches!(e, SessionEvent::SubscriptionChanged { sub, .. } if sub.rid == dm_id)
                        || matches!(e, SessionEvent::RoomChanged { room, .. } if room.id == dm_id)
                },
                WAIT,
            )
            .await
            {
                SessionEvent::SubscriptionChanged { kind, sub } => {
                    assert_eq!(kind, ChangeKind::Inserted);
                    assert_eq!(sub.u.id, a.id);
                    sub_seen = true;
                }
                SessionEvent::RoomChanged { kind, room } => {
                    if !room_seen {
                        assert_eq!(kind, ChangeKind::Inserted);
                    }
                    assert!(room.uids.as_ref().unwrap().contains(&b.id));
                    room_seen = true;
                }
                _ => unreachable!(),
            }
        }

        // Initial presence over REST: B is online (its session is connected).
        let presence = sa.rest().users_presence(None, &[b.id.as_str()]).await.unwrap();
        let entry = presence.users.iter().find(|u| u.id == b.id).expect("b in presence");
        assert_eq!(entry.status, Some(UserStatus::Online));

        // Live change through user-status.
        sb.rest().users_set_status(UserStatus::Busy, Some("in a test")).await.unwrap();
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::UserStatus { uid, status: UserStatus::Busy, .. } if *uid == b.id),
            WAIT,
        )
        .await;
        assert_eq!(
            ev,
            SessionEvent::UserStatus {
                uid: b.id.clone(),
                username: b.username.clone(),
                status: UserStatus::Busy,
                status_text: Some("in a test".into()),
            }
        );
        sb.rest().users_set_status(UserStatus::Online, Some("")).await.unwrap();
        expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::UserStatus { uid, status: UserStatus::Online, .. } if *uid == b.id),
            WAIT,
        )
        .await;

        sa.close().await;
        sb.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_emits_resync_and_events_keep_flowing() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let b = fx.user("sb").await;
        let rid = shared_channel(&fx, &b, &a).await;
        let (sa, mut ea) = start(&server, &a).await;
        assert_eq!(sa.connection_state(), ConnectionState::Connected);
        let _watch = sa.watch_room(&rid).await.unwrap();

        sa.reconnect_now();
        expect_event(
            &mut ea,
            |e| {
                matches!(
                    e,
                    SessionEvent::Connection(
                        ConnectionState::Reconnecting { .. } | ConnectionState::Connecting
                    )
                )
            },
            WAIT,
        )
        .await;
        expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::Connection(ConnectionState::Connected)),
            WAIT,
        )
        .await;
        expect_event(
            &mut ea,
            |e| {
                matches!(
                    e,
                    SessionEvent::Resync {
                        reason: ResyncReason::Reconnected
                    }
                )
            },
            WAIT,
        )
        .await;

        // Global and room subscriptions were renewed.
        let id = post(&b, &rid, "after reconnect").await;
        expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::Message { message, .. } if message.id == id),
            WAIT,
        )
        .await;
        b.rest.delete_message(&rid, &id).await.unwrap();
        expect_event(&mut ea, is_deleted(&id), WAIT).await;
        // DDP calls work on the new connection (resumed login).
        let r = sa
            .call("getUsersOfRoom", vec![json!(rid), json!(true)])
            .await
            .unwrap();
        assert!(r["total"].as_u64().unwrap() >= 2, "{r}");

        sa.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_token_logs_out() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let (sa, mut ea) = start(&server, &a).await;

        // Another client with the same token logs out (removes the token).
        let other = RestClient::new(server.clone(), &tls()).unwrap();
        other.set_credentials(Some(a.credentials()));
        other.logout().await.unwrap();

        // The server may keep the authenticated socket open; a reconnect has to
        // resume with the revoked token and fails.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut logged_out = None;
        while let Ok(Some(e)) = tokio::time::timeout_at(deadline, ea.recv()).await {
            if let SessionEvent::LoggedOut { reason } = e {
                logged_out = Some(reason);
                break;
            }
        }
        if logged_out.is_none() {
            eprintln!("socket stayed open after token revocation; forcing a reconnect");
            sa.reconnect_now();
            let SessionEvent::LoggedOut { reason } = expect_event(
                &mut ea,
                |e| matches!(e, SessionEvent::LoggedOut { .. }),
                WAIT,
            )
            .await
            else {
                unreachable!()
            };
            logged_out = Some(reason);
        }
        eprintln!("logged out: {logged_out:?}");
        // After LoggedOut the stream ends.
        let rest = collect_for(&mut ea, |_| true, Duration::from_secs(5)).await;
        assert!(
            rest.iter()
                .all(|e| matches!(e, SessionEvent::Connection(_))),
            "{rest:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(5), ea.recv())
                .await
                .unwrap()
                .is_none(),
            "event stream did not end"
        );
        assert_eq!(sa.connection_state(), ConnectionState::Closed);
        drop(sa);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_token_fails_to_start() {
    let Some(server) = server() else { return };
    let r = Session::start(
        server,
        &tls(),
        Credentials::new("nobody", "not-a-token"),
        SessionOptions::default(),
    )
    .await;
    match r {
        Err(e) => assert!(e.is_unauthorized(), "{e:?}"),
        Ok(_) => panic!("started with an invalid token"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_session_closes_the_connection() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;

        // Drop session and events: the socket closes.
        let (sa, ea) = start(&server, &a).await;
        let mut state = sa.watch_connection();
        drop(sa);
        drop(ea);
        tokio::time::timeout(
            Duration::from_secs(5),
            state.wait_for(|s| matches!(s, meteor_ddp::ConnectionState::Closed)),
        )
        .await
        .expect("connection not closed after drop")
        .ok();

        // Drop only the session: the event stream ends after Connection(Closed).
        let (sa, mut ea) = start(&server, &a).await;
        let room = a
            .rest
            .channels_create(&CreateRoom::new(unique("sess-drop")))
            .await
            .unwrap();
        fx.add_room(&room.id);
        let _w = sa.watch_room(&room.id).await.unwrap();
        drop(sa);
        let rest = collect_for(&mut ea, |_| true, Duration::from_secs(3)).await;
        assert!(
            matches!(
                rest.last(),
                Some(SessionEvent::Connection(ConnectionState::Closed))
            ),
            "{rest:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(5), ea.recv())
                .await
                .expect("event stream did not end")
                .is_none()
        );

        // logout(): the stream ends and the token is invalid.
        let (sa, mut ea) = start(&server, &a).await;
        sa.logout().await.unwrap();
        let rest = collect_for(&mut ea, |_| true, Duration::from_secs(3)).await;
        assert!(
            matches!(
                rest.last(),
                Some(SessionEvent::Connection(ConnectionState::Closed))
            ),
            "{rest:?}"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(5), ea.recv())
                .await
                .expect("event stream did not end")
                .is_none()
        );
        let check = RestClient::new(server.clone(), &tls()).unwrap();
        check.set_credentials(Some(a.credentials()));
        assert!(check.me().await.unwrap_err().is_unauthorized());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn public_channel_messages_without_membership_are_flagged() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let b = fx.user("sb").await;
        // A public channel A never joined.
        let room = b
            .rest
            .channels_create(&CreateRoom::new(unique("sess-pub")))
            .await
            .unwrap();
        fx.add_room(&room.id);
        let (sa, mut ea) = start(&server, &a).await;
        let id = post(&b, &room.id, "public hello").await;
        let ev = expect_event(
            &mut ea,
            |e| matches!(e, SessionEvent::Message { message, .. } if message.id == id),
            WAIT,
        )
        .await;
        assert!(
            matches!(
                ev,
                SessionEvent::Message {
                    room_participant: false,
                    ..
                }
            ),
            "{ev:?}"
        );
        // Room events of a non-joined public channel need a watch.
        let _w = sa.watch_room(&room.id).await.unwrap();
        b.rest.delete_message(&room.id, &id).await.unwrap();
        expect_event(&mut ea, is_deleted(&id), WAIT).await;
        sa.close().await;
    })
    .await;
}

fn follows(m: &rocketchat::models::Message, me: &str) -> bool {
    m.replies.iter().flatten().any(|u| u == me)
}

/// Live discovery behind the sidebar's Threads section: `__my_messages__`
/// re-broadcasts a thread parent on every reply, follow, unfollow and
/// deletion (as `t: "rm"`, not as a delete event) without a room watch.
#[tokio::test(flavor = "multi_thread")]
async fn thread_parent_updates_reach_my_messages() {
    with_fixture(|server, fx| async move {
        let a = fx.user("sa").await;
        let b = fx.user("sb").await;
        let rid = shared_channel(&fx, &a, &b).await;
        let parent = post(&a, &rid, "thread parent").await;
        let (sa, mut ea) = start(&server, &a).await;
        let is_parent = |pred: fn(&rocketchat::models::Message, &str) -> bool| {
            let parent = parent.clone();
            let me = a.id.clone();
            move |e: &SessionEvent| {
                matches!(e, SessionEvent::Message { message, .. }
                    if message.id == parent && pred(message, &me))
            }
        };

        let reply = b
            .rest
            .send_message(&OutgoingMessage::new(&rid, "reply").in_thread(&parent, false))
            .await
            .unwrap();
        let ev = expect_event(
            &mut ea,
            is_parent(|m, me| m.tcount == Some(1) && follows(m, me)),
            WAIT,
        )
        .await;
        let SessionEvent::Message { message: p, .. } = ev else {
            unreachable!()
        };
        assert!(p.tlm.is_some(), "{p:?}");
        assert!(p.replies.iter().flatten().any(|u| u == &b.id), "{p:?}");

        a.rest.unfollow_message(&parent).await.unwrap();
        expect_event(&mut ea, is_parent(|m, me| !follows(m, me)), WAIT).await;
        a.rest.follow_message(&parent).await.unwrap();
        expect_event(&mut ea, is_parent(follows), WAIT).await;

        // A second reply bumps `tcount` and `tlm`.
        b.rest
            .send_message(&OutgoingMessage::new(&rid, "reply 2").in_thread(&parent, false))
            .await
            .unwrap();
        let ev = expect_event(&mut ea, is_parent(|m, _| m.tcount == Some(2)), WAIT).await;
        let SessionEvent::Message { message: p2, .. } = ev else {
            unreachable!()
        };
        assert!(p2.tlm > p.tlm && p2.updated_at > p.updated_at, "{p2:?}");

        // Deleting the parent sends it again as `t: "rm"`; no delete event
        // without a room watch.
        a.rest.delete_message(&rid, &parent).await.unwrap();
        expect_event(
            &mut ea,
            is_parent(|m, _| m.t.as_deref() == Some("rm")),
            WAIT,
        )
        .await;
        let deleted = collect_for(&mut ea, is_deleted(&parent), Duration::from_secs(2)).await;
        assert!(deleted.is_empty(), "{deleted:?}");
        assert!(!reply.id.is_empty());
        sa.close().await;
    })
    .await;
}
