//! App-level flows driven through `App::update` without network (returned
//! tasks are dropped unless stated). Adopted from the review reproductions.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use rocketchat::Credentials;
use rocketchat::models::{Room, RoomType, Subscription as Sub, User, UserRef};
use rocketchat::rest::LoginResult;
use rocketchat::session::{ChangeKind, ResyncReason};
use rocketchat::{RcDate, SessionEvent};
use rustket::app::{App, Flags, Modal, SecretsMode};
use rustket::error::Failure;
use rustket::message::{Message, ModalMessage, SecretsReady};
use rustket::screens::login;
use rustket::session::ServerEvent;
use rustket::workspace::sidebar;
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, ServerEntry, TokenKind};
use rustket_core::secrets::{MemoryStore, SecretBackend, SecretStore};
use rustket_core::sync::InitialData;

const SERVER: &str = "http://localhost:3000";

fn user(id: &str, name: &str) -> User {
    User {
        id: id.into(),
        username: Some(name.into()),
        ..Default::default()
    }
}

fn ts(ms: i64) -> RcDate {
    RcDate::from_millis(ms).unwrap()
}

fn sub(rid: &str, uid: &str, unread: u64, updated: i64) -> Sub {
    Sub {
        id: format!("s-{rid}"),
        rid: rid.into(),
        name: rid.into(),
        t: RoomType::Channel,
        open: true,
        unread,
        alert: unread > 0,
        u: UserRef {
            id: uid.into(),
            username: None,
            name: None,
        },
        updated_at: Some(ts(updated)),
        ..Default::default()
    }
}

fn room(rid: &str, updated: i64) -> Room {
    Room {
        id: rid.into(),
        t: RoomType::Channel,
        name: Some(rid.into()),
        updated_at: Some(ts(updated)),
        ..Default::default()
    }
}

const T0: i64 = 1_700_000_000_000;

fn initial(me: User, rids: &[(&str, u64)]) -> InitialData {
    InitialData {
        subscriptions: rids.iter().map(|(r, u)| sub(r, &me.id, *u, T0)).collect(),
        rooms: rids.iter().map(|(r, _)| room(r, T0)).collect(),
        next_since: Some(ts(T0)),
        me,
        ..Default::default()
    }
}

fn boot(app: &mut App, tokens: Vec<(ServerId, Option<String>)>, store: Arc<MemoryStore>) {
    let _ = app.update(Message::SecretsReady(SecretsReady {
        store,
        backend: SecretBackend::Memory,
        tokens,
        notices: vec![],
    }));
}

/// One configured server; logged in as `uid` when `uid` is set.
fn app_with(
    dir: &std::path::Path,
    uid: Option<&str>,
    urls: Vec<String>,
) -> (App, ServerId, Arc<MemoryStore>) {
    let mut cfg = AppConfig::default();
    let mut entry = ServerEntry::new(SERVER, "Local");
    entry.user_id = uid.map(str::to_owned);
    let sid = cfg.add_server(entry);
    cfg.save(dir).unwrap();
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        secrets: SecretsMode::Memory,
        urls,
        ..Flags::default()
    });
    let store = Arc::new(MemoryStore::new());
    let tokens = match uid {
        Some(uid) => {
            store
                .set(&rustket_core::secrets::secret_key(&sid, uid), "tokA")
                .unwrap();
            vec![(sid.clone(), Some("tokA".into()))]
        }
        None => vec![],
    };
    boot(&mut app, tokens, store.clone());
    (app, sid, store)
}

/// A logout or server removal: the app first asks the secret store for an
/// unsaved E2E passphrase; the answer is fed back. Returns the task of the
/// actual sign-out.
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

fn ev(app: &App, sid: &ServerId, e: SessionEvent) -> Message {
    Message::Server(
        sid.clone(),
        app.servers[sid].generation,
        ServerEvent::Event(Box::new(e)),
    )
}

fn loaded(app: &App, sid: &ServerId, data: InitialData) -> Message {
    Message::InitialLoaded(sid.clone(), app.servers[sid].generation, Ok(Box::new(data)))
}

fn token_link(uid: &str, token: &str) -> String {
    format!("rocketchat://auth?host={SERVER}&userId={uid}&token={token}")
}

fn confirm_modals(app: &App) -> usize {
    app.modals
        .iter()
        .filter(|m| matches!(m, Modal::ConfirmToken { .. }))
        .count()
}

fn signed_in(app: &App, sid: &ServerId) -> Option<String> {
    app.servers[sid]
        .credentials
        .as_ref()
        .map(|c| c.user_id.clone())
}

fn pending(app: &App, sid: &ServerId) -> bool {
    app.servers[sid]
        .login
        .as_ref()
        .is_some_and(|l| l.pending.is_some())
}

/// Delivers a token link and its `me` result.
fn token_flow(app: &mut App, sid: &ServerId, uid: &str, token: &str) {
    let _ = app.update(Message::DeepLinks(vec![token_link(uid, token)]));
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new(uid, token),
        Ok(user(uid, &uid.to_lowercase())),
    ));
}

/// Stamp of the confirmation on top, if any.
fn top_stamp(app: &App) -> Option<(ServerId, u64, bool)> {
    match app.modals.last() {
        Some(Modal::ConfirmToken {
            server_id,
            stamp,
            armed,
            ..
        }) => Some((server_id.clone(), *stamp, *armed)),
        _ => None,
    }
}

/// Arms the confirmation on top (as its timer would) and confirms it.
fn confirm(app: &mut App, _sid: &ServerId) {
    if let Some((_, stamp, _)) = top_stamp(app) {
        let _ = app.update(Message::ArmConfirm(stamp));
    }
    let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
}

// --- login CSRF (review #5, #22-#24) ---------------------------------------------

#[tokio::test]
async fn token_link_needs_matching_user_and_armed_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![token_link("U1", "T1")]));
    assert!(
        signed_in(&app, &sid).is_none(),
        "a link alone never logs in"
    );
    assert_eq!(confirm_modals(&app), 0);
    // `me` returned a different user than the link claimed: rejected.
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("U1", "T1"),
        Ok(user("OTHER", "eve")),
    ));
    assert_eq!(confirm_modals(&app), 0);
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("U1", "T1"),
        Ok(user("U1", "u1")),
    ));
    assert_eq!(confirm_modals(&app), 1);
    // Not armed yet: clicking does nothing.
    let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
    assert!(signed_in(&app, &sid).is_none());
    confirm(&mut app, &sid);
    assert_eq!(signed_in(&app, &sid).as_deref(), Some("U1"));
    assert_eq!(confirm_modals(&app), 0);
    assert!(app.servers[&sid].login.is_none());
}

#[tokio::test]
async fn second_token_link_cancels_the_login() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![token_link("LEGIT", "T1")]));
    let _ = app.update(Message::DeepLinks(vec![token_link("ATTACKER", "T2")]));
    assert!(
        !pending(&app, &sid),
        "duplicate link must cancel the pending login"
    );
    for (u, t) in [("LEGIT", "T1"), ("ATTACKER", "T2")] {
        let _ = app.update(Message::TokenUser(
            sid.clone(),
            Credentials::new(u, t),
            Ok(user(u, u)),
        ));
    }
    assert_eq!(confirm_modals(&app), 0);
    confirm(&mut app, &sid);
    assert!(signed_in(&app, &sid).is_none());
}

#[tokio::test]
async fn confirmation_after_cancel_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![token_link("ATTACKER", "T")]));
    let _ = app.update(Message::Login(sid.clone(), login::Message::CancelPending));
    assert!(!pending(&app, &sid));
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("ATTACKER", "T"),
        Ok(user("ATTACKER", "attacker")),
    ));
    assert_eq!(confirm_modals(&app), 0);
    confirm(&mut app, &sid);
    assert!(signed_in(&app, &sid).is_none());
}

#[tokio::test]
async fn cancelling_the_browser_login_removes_the_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    token_flow(&mut app, &sid, "ATTACKER", "T");
    assert_eq!(confirm_modals(&app), 1);
    let _ = app.update(Message::Login(sid.clone(), login::Message::CancelPending));
    assert_eq!(confirm_modals(&app), 0);
    // Rejecting the account in the dialog ends the pending login too.
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    token_flow(&mut app, &sid, "ATTACKER", "T2");
    let _ = app.update(Message::CloseModal);
    assert!(!pending(&app, &sid));
    // A late link after that is ignored.
    token_flow(&mut app, &sid, "ATTACKER", "T3");
    assert_eq!(confirm_modals(&app), 0);
}

#[tokio::test]
async fn unsolicited_token_link_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    token_flow(&mut app, &sid, "ATTACKER", "T");
    assert_eq!(confirm_modals(&app), 0);
    assert!(signed_in(&app, &sid).is_none());
}

/// Review #6: an unsolicited error link neither cancels the login nor shows
/// unquoted attacker text.
#[tokio::test]
async fn error_link_keeps_pending_login() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![format!(
        "rocketchat://auth?host={SERVER}&error=Session+expired.+Visit+https://evil.example+to+continue"
    )]));
    assert!(pending(&app, &sid));
    let l = app.servers[&sid].login.as_ref().unwrap();
    assert!(l.error.is_none());
    let notice = l.pending.as_ref().unwrap().error.clone().unwrap();
    assert!(notice.contains('“'), "{notice}");
}

#[tokio::test]
async fn links_before_boot_are_replayed() {
    let (mut app, _task) = App::new(Flags {
        ephemeral: true,
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    let _ = app.update(Message::DeepLinks(vec![
        "rocketchat://room?host=https://chat.example.com&rid=R".into(),
    ]));
    assert!(app.modals.is_empty());
    boot(&mut app, vec![], Arc::new(MemoryStore::new()));
    assert!(
        app.modals
            .iter()
            .any(|m| matches!(m, Modal::OfferAddServer { .. }))
    );
}

/// Review #8: a link to another scheme of a configured host is not offered.
#[tokio::test]
async fn other_origin_of_known_host_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::DeepLinks(vec![
        "rocketchat://room?host=https://localhost:3000&rid=R".into(),
        "rocketchat://room?host=http://localhost:4000&rid=R".into(),
    ]));
    assert!(app.modals.is_empty());
    assert!(!app.toasts.is_empty());
}

// --- PAT (review #2) ----------------------------------------------------------------

#[tokio::test]
async fn pat_login_is_remembered_as_pat() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::ShowToken(true)));
    let _ = app.update(Message::Login(
        sid.clone(),
        login::Message::PatChecked(Credentials::new("U", "PAT"), Ok(user("U", "u"))),
    ));
    assert_eq!(signed_in(&app, &sid).as_deref(), Some("U"));
    assert_eq!(
        app.config.server(&sid).and_then(|e| e.token_kind),
        Some(TokenKind::Pat)
    );
    let _ = app.update(Message::Login(
        sid.clone(),
        login::Message::LoginDone(Ok(LoginResult {
            user_id: "U".into(),
            auth_token: "login-token".into(),
            me: Some(user("U", "u")),
        })),
    ));
    // Logged in already: the login screen is gone, nothing changes.
    assert_eq!(
        app.config.server(&sid).and_then(|e| e.token_kind),
        Some(TokenKind::Pat)
    );
    app.flush_config();
    let disk = AppConfig::load(dir.path()).unwrap();
    assert_eq!(
        disk.server(&sid).and_then(|e| e.token_kind),
        Some(TokenKind::Pat)
    );
}

// --- sync and session epochs (review #3, #4, #13, #15, #16) -------------------------

/// Realtime changes received while `initial_load` is in flight survive the
/// (older) snapshot.
#[tokio::test]
async fn initial_load_keeps_newer_realtime_updates() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    for e in [
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Inserted,
            sub: Box::new(sub("new-room", "A", 1, T0 + 5_000)),
        },
        SessionEvent::RoomChanged {
            kind: ChangeKind::Inserted,
            room: Box::new(room("new-room", T0 + 5_000)),
        },
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Updated,
            sub: Box::new(sub("r1", "A", 3, T0 + 6_000)),
        },
    ] {
        let m = ev(&app, &sid, e);
        let _ = app.update(m);
    }
    let m = loaded(&app, &sid, initial(me, &[("r1", 0)]));
    let _ = app.update(m);
    let ws = &app.servers[&sid].workspace;
    assert!(ws.room("new-room").is_some(), "new room lost");
    assert_eq!(ws.subscription("r1").unwrap().unread, 3, "unread lost");
    assert!(ws.last_sync().is_some());
}

/// Review #4: a room link on the command line waits for the initial load.
#[tokio::test]
async fn cold_start_room_link_opens_after_load() {
    let dir = tempfile::tempdir().unwrap();
    let link = format!("rocketchat://room?host={SERVER}&rid=R1&path=channel/r1");
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![link]);
    assert!(app.toasts.is_empty(), "no premature 'room not found'");
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(&app, &sid, initial(me, &[("R1", 0)]));
    let _ = app.update(m);
    assert_eq!(app.servers[&sid].active_room.as_deref(), Some("R1"));
    // An unknown room after the load: a clear message.
    let _ = app.update(Message::DeepLinks(vec![format!(
        "rocketchat://room?host={SERVER}&rid=NOPE"
    )]));
    assert_eq!(app.toasts.len(), 1);
    // Expiry of a queued link.
    let link = format!("rocketchat://room?host={SERVER}&rid=R2");
    let dir2 = tempfile::tempdir().unwrap();
    let (mut app2, sid2, _) = app_with(dir2.path(), Some("A"), vec![link]);
    let stamp = app2.servers[&sid2].pending_room.as_ref().unwrap().2;
    let _ = app2.update(Message::RoomLinkExpired(sid2.clone(), stamp));
    assert!(app2.servers[&sid2].pending_room.is_none());
    assert_eq!(app2.toasts.len(), 1);
}

/// Review #13: results of an earlier session never reach the current one.
#[tokio::test]
async fn stale_results_of_an_old_session_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let a = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(a.clone()),
        },
    );
    let _ = app.update(m);
    let old_epoch = app.servers[&sid].generation;
    let _ = sign_out(
        &mut app,
        Message::Sidebar(
            sid.clone(),
            sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
        ),
    )
    .await;
    assert!(app.servers[&sid].login.is_some());
    // Late successful load while logged out: ignored.
    let _ = app.update(Message::InitialLoaded(
        sid.clone(),
        old_epoch,
        Ok(Box::new(initial(a.clone(), &[("r1", 0)]))),
    ));
    assert!(app.servers[&sid].rooms.is_empty());
    assert!(app.servers[&sid].workspace.rooms().is_empty());
    // Log in as B, then A's late 401 and data arrive.
    let _ = app.update(Message::Login(
        sid.clone(),
        login::Message::LoginDone(Ok(LoginResult {
            user_id: "B".into(),
            auth_token: "tokB".into(),
            me: Some(user("B", "bob")),
        })),
    ));
    assert_eq!(signed_in(&app, &sid).as_deref(), Some("B"));
    let unauthorized = Failure::from(rocketchat::Error::Unauthorized {
        message: None,
        code: None,
    });
    let _ = app.update(Message::InitialLoaded(
        sid.clone(),
        old_epoch,
        Err(unauthorized),
    ));
    assert_eq!(
        signed_in(&app, &sid).as_deref(),
        Some("B"),
        "new session survived"
    );
    let _ = app.update(Message::InitialLoaded(
        sid.clone(),
        old_epoch,
        Ok(Box::new(initial(a, &[("alice-private", 2)]))),
    ));
    assert!(app.servers[&sid].workspace.room("alice-private").is_none());
    assert_eq!(app.servers[&sid].workspace.username(), Some("bob"));
    // Events of the old session subscription are dropped too.
    let _ = app.update(Message::Server(
        sid.clone(),
        old_epoch,
        ServerEvent::Event(Box::new(SessionEvent::LoggedOut {
            reason: "old".into(),
        })),
    ));
    assert_eq!(signed_in(&app, &sid).as_deref(), Some("B"));
}

/// Review #15/#16: failures are retried; resyncs during a sync are coalesced.
#[tokio::test]
async fn sync_failure_retries_and_resync_coalesces() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(user("A", "alice")),
        },
    );
    let _ = app.update(m);
    let epoch = app.servers[&sid].generation;
    // Pretend the initial load is running (no session in this test).
    app.servers.get_mut(&sid).unwrap().syncing = true;
    let m = ev(
        &app,
        &sid,
        SessionEvent::Resync {
            reason: ResyncReason::Overflow,
        },
    );
    let _ = app.update(m);
    assert!(app.servers[&sid].resync_pending, "resync remembered");
    let t = app.update(Message::InitialLoaded(
        sid.clone(),
        epoch,
        Err(Failure::from(rocketchat::Error::Timeout(
            "subscriptions.get",
        ))),
    ));
    let s = &app.servers[&sid];
    assert!(!s.syncing);
    assert_eq!(s.sync_attempt, 1);
    assert!(s.sync_error.is_some(), "banner shown");
    assert!(t.units() >= 1, "retry scheduled");
    // A successful sync clears the error.
    let _ = app.update(loaded(&app, &sid, initial(user("A", "alice"), &[])));
    let s = &app.servers[&sid];
    assert_eq!(s.sync_attempt, 0);
    assert!(s.sync_error.is_none());
}

// --- rooms (review #19) -----------------------------------------------------------------

#[tokio::test]
async fn removed_room_closes_its_view_and_can_be_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(&app, &sid, initial(me, &[("r1", 0), ("r2", 0)]));
    let _ = app.update(m);
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    assert!(app.servers[&sid].rooms.contains_key("r1"));
    let m = ev(
        &app,
        &sid,
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Removed,
            sub: Box::new(sub("r1", "A", 0, T0 + 1)),
        },
    );
    let _ = app.update(m);
    let s = &app.servers[&sid];
    assert!(!s.rooms.contains_key("r1"));
    assert_eq!(s.active_room, None);
    // Re-added and clicked: a fresh view with a timeline.
    for e in [
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Inserted,
            sub: Box::new(sub("r1", "A", 0, T0 + 2)),
        },
        SessionEvent::RoomChanged {
            kind: ChangeKind::Inserted,
            room: Box::new(room("r1", T0 + 2)),
        },
    ] {
        let m = ev(&app, &sid, e);
        let _ = app.update(m);
    }
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    assert!(app.servers[&sid].workspace.timeline("r1").is_some());
    assert!(app.servers[&sid].rooms.contains_key("r1"));
}

/// A livechat chat `L1` served by `A`, open in the app; `prefs` are the
/// user's preferences.
fn livechat_app(dir: &std::path::Path, prefs: serde_json::Value) -> (App, ServerId, Sub) {
    let (mut app, sid, _) = app_with(dir, Some("A"), vec![]);
    let me: User = serde_json::from_value(serde_json::json!({
        "_id": "A", "username": "alice", "settings": {"preferences": prefs}
    }))
    .unwrap();
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let mut data = initial(me, &[("r1", 0)]);
    let mut s = sub("L1", "A", 1, T0);
    s.t = RoomType::Livechat;
    data.subscriptions.push(s.clone());
    let mut r = room("L1", T0);
    r.t = RoomType::Livechat;
    r.name = None;
    r.fname = Some("Visitor".into());
    r.open = Some(true);
    r.served_by = Some(serde_json::json!({"_id": "A", "username": "alice"}));
    data.rooms.push(r);
    let m = loaded(&app, &sid, data);
    let _ = app.update(m);
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("L1".into()),
    ));
    assert!(app.servers[&sid].rooms.contains_key("L1"));
    (app, sid, s)
}

fn remove_sub(app: &mut App, sid: &ServerId, s: &Sub) {
    let m = ev(
        app,
        sid,
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Removed,
            sub: Box::new(s.clone()),
        },
    );
    let _ = app.update(m);
}

/// Omnichannel: with "Hide conversation after closing" (default) a chat
/// closed or forwarded elsewhere closes its view; results of an old session
/// are dropped.
#[tokio::test]
async fn closed_livechat_chat_closes_its_view_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, s) = livechat_app(dir.path(), serde_json::json!({}));
    // A result of the previous session (generation bumped by a re-login).
    let old = app.servers[&sid].generation;
    app.servers.get_mut(&sid).unwrap().generation += 1;
    let _ = app.update(Message::Omni(
        sid.clone(),
        old,
        rustket::omnichannel::Message::Routing(Ok(Default::default())),
    ));
    assert!(
        app.servers[&sid].omni.state.routing.is_none(),
        "old session's result dropped"
    );
    // An app panel open in the chat goes away with its view.
    {
        use rocketchat::models::ButtonContext;
        use rocketchat::uikit::{InteractionResponse, ServerInteraction};
        use rustket_core::uikit::{InteractionResult, Origin, OriginKind};
        let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
        let now = std::time::Instant::now();
        let origin =
            Origin::new(OriginKind::ActionButton(ButtonContext::RoomAction)).rid(Some("L1"));
        let t = ws.uikit_mut().begin(Some("app1"), origin, now);
        let mut si: serde_json::Value = serde_json::from_str(include_str!(
            "../../rocketchat/tests/fixtures/uikit/synthetic_contextual_bar_open.json"
        ))
        .unwrap();
        si["triggerId"] = serde_json::Value::String(t.clone());
        let result = InteractionResult::Response(InteractionResponse::Interaction(
            ServerInteraction::from_value(si),
        ));
        ws.uikit_mut().finish(&t, result, now);
        assert!(ws.uikit().bar("bar-1").is_some());
    }
    remove_sub(&mut app, &sid, &s);
    let st = &app.servers[&sid];
    assert!(!st.rooms.contains_key("L1"));
    assert_eq!(st.active_room, None);
    assert!(st.workspace.uikit().bar("bar-1").is_none(), "bar closed");
}

/// Omnichannel: without "Hide conversation after closing" the closed chat
/// stays open (a preview) with the "closed" callout instead of the composer.
#[tokio::test]
async fn closed_livechat_chat_stays_when_not_hidden() {
    use rustket_core::omnichannel::room::ComposerState;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, s) = livechat_app(
        dir.path(),
        serde_json::json!({"omnichannelHideConversationAfterClosing": false}),
    );
    remove_sub(&mut app, &sid, &s);
    let st = &app.servers[&sid];
    assert!(st.rooms.contains_key("L1"), "view kept");
    assert_eq!(st.active_room.as_deref(), Some("L1"));
    assert!(st.workspace.room("L1").is_some_and(|e| e.sub.is_none()));
    assert!(st.workspace.timeline("L1").is_some(), "history reloaded");
    // The server's copy (re-read, or streamed) says the chat is closed.
    let mut r = room("L1", T0 + 5);
    r.t = RoomType::Livechat;
    r.fname = Some("Visitor".into());
    let m = ev(
        &app,
        &sid,
        SessionEvent::RoomChanged {
            kind: ChangeKind::Updated,
            room: Box::new(r),
        },
    );
    let _ = app.update(m);
    let st = &app.servers[&sid];
    let omni = rustket_core::omnichannel::OmnichannelState::new();
    assert_eq!(
        rustket::omnichannel::room::state_of(&st.workspace, &omni, "L1"),
        Some(ComposerState::Closed)
    );
    assert!(st.rooms.contains_key("L1"));
}

/// Omnichannel: the user's own "return to queue" removes the subscription
/// before the HTTP result arrives; the view waits for it, so its success
/// toast is shown, then closes.
#[tokio::test]
async fn own_return_to_queue_keeps_its_toast() {
    use rustket::omnichannel::dialogs::{self, confirm};
    use rustket::omnichannel::room::{Msg as OmniMsg, Request};
    use rustket_core::omnichannel::room::QuickAction;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, s) = livechat_app(dir.path(), serde_json::json!({}));
    let room_msg = |app: &App, m: OmniMsg| {
        Message::Room(
            sid.clone(),
            app.servers[&sid].generation,
            "L1".into(),
            rustket::room::Message::Omni(m),
        )
    };
    let m = room_msg(
        &app,
        OmniMsg::Request(Request::Quick(QuickAction::MoveToQueue)),
    );
    let _ = app.update(m);
    let m = room_msg(
        &app,
        OmniMsg::Dialog(dialogs::Msg::Confirm(confirm::Msg::Confirm)),
    );
    let _ = app.update(m);
    assert!(app.servers[&sid].rooms["L1"].omni.request_in_flight());
    remove_sub(&mut app, &sid, &s);
    assert!(
        app.servers[&sid].rooms.contains_key("L1"),
        "waits for the result"
    );
    let toasts = app.toasts.len();
    let m = room_msg(
        &app,
        OmniMsg::Dialog(dialogs::Msg::Confirm(confirm::Msg::Done(Ok(true)))),
    );
    let _ = app.update(m);
    assert_eq!(app.toasts.len(), toasts + 1, "success toast");
    assert!(!app.servers[&sid].rooms.contains_key("L1"), "then closed");
}

// --- config writes (review #17) ---------------------------------------------------------

#[tokio::test]
async fn config_saves_keep_the_newest_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    let mut e1 = ServerEntry::new(SERVER, "One");
    e1.user_id = Some("A".into());
    let s1 = cfg.add_server(e1);
    let s2 = cfg.add_server(ServerEntry::new("http://127.0.0.1:3000", "Two"));
    cfg.active_server = Some(s1.clone());
    cfg.save(dir.path()).unwrap();
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(dir.path().to_owned()),
        cache_dir: Some(dir.path().join("cache")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    boot(
        &mut app,
        vec![(s1.clone(), Some("tokA".into()))],
        Arc::new(MemoryStore::new()),
    );
    let _ = app.update(Message::Login(s2.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![
        "rocketchat://auth?host=http://127.0.0.1:3000&userId=B&token=tokB".into(),
    ]));
    let _ = app.update(Message::TokenUser(
        s2.clone(),
        Credentials::new("B", "tokB"),
        Ok(user("B", "bob")),
    ));
    confirm(&mut app, &s2);
    app.flush_config();
    let disk = AppConfig::load(dir.path()).unwrap();
    assert_eq!(
        disk.server(&s2).and_then(|e| e.user_id.clone()).as_deref(),
        Some("B")
    );
    assert_eq!(disk.active_server.as_ref(), Some(&s2));
}

// --- images (review #20, #21) -------------------------------------------------------------

#[tokio::test]
async fn avatar_update_refetches_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(&app, &sid, initial(me, &[("r1", 0)]));
    let _ = app.update(m);
    let url = app.servers[&sid].url.clone();
    let r1 = rustket::images::Images::avatar_url(
        &url,
        &rustket_core::room_list::Avatar::Room {
            rid: "r1".into(),
            etag: None,
        },
    );
    let _ = app.update(Message::ImageLoaded(
        sid.clone(),
        app.images.generation(&sid),
        r1.clone(),
        fetched_png(),
    ));
    assert!(app.images.contains(&r1));
    let m = ev(
        &app,
        &sid,
        SessionEvent::AvatarUpdated {
            username: None,
            rid: Some("r1".into()),
            etag: None,
        },
    );
    let t = app.update(m);
    assert!(!app.images.contains(&r1));
    assert!(t.units() >= 1, "refetch scheduled by the same update");
}

#[tokio::test]
async fn many_rooms_do_not_thrash_the_avatar_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let n = rustket::images::MEMORY_CAPACITY + 300;
    let names: Vec<String> = (0..n).map(|i| format!("room{i:05}")).collect();
    let rids: Vec<(&str, u64)> = names.iter().map(|r| (r.as_str(), 0)).collect();
    let m = loaded(&app, &sid, initial(me, &rids));
    let _ = app.update(m);
    assert!(app.images.capacity() >= n);
}

fn fetched_png() -> rustket::images::FetchResult {
    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, 2, 2);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&[255, 0, 0, 255].repeat(4)).unwrap();
    drop(w);
    rustket::images::FetchResult {
        fetched: rustket::images::decode(&png, 128).map(rustket::images::Fetched::Image),
        write: None,
    }
}

// --- re-review -------------------------------------------------------------------

/// Re-review #5: one confirmation at a time; the next one starts disarmed.
#[tokio::test]
async fn confirmations_of_two_servers_do_not_stack() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    let s1 = cfg.add_server(ServerEntry::new(SERVER, "One"));
    let s2 = cfg.add_server(ServerEntry::new("http://127.0.0.1:3000", "Two"));
    cfg.save(dir.path()).unwrap();
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(dir.path().to_owned()),
        cache_dir: Some(dir.path().join("cache")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    boot(&mut app, vec![], Arc::new(MemoryStore::new()));
    for (sid, host, uid) in [(&s1, SERVER, "A"), (&s2, "http://127.0.0.1:3000", "B")] {
        let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
        let _ = app.update(Message::DeepLinks(vec![format!(
            "rocketchat://auth?host={host}&userId={uid}&token=T{uid}"
        )]));
        let _ = app.update(Message::TokenUser(
            sid.clone(),
            Credentials::new(uid, format!("T{uid}")),
            Ok(user(uid, uid)),
        ));
    }
    assert_eq!(confirm_modals(&app), 1, "single confirmation slot");
    let (first, stamp, armed) = top_stamp(&app).unwrap();
    assert_eq!(first, s1);
    assert!(!armed);
    // A stale arm (wrong stamp) does nothing.
    let _ = app.update(Message::ArmConfirm(stamp + 1000));
    assert!(!top_stamp(&app).unwrap().2);
    confirm(&mut app, &s1);
    assert_eq!(signed_in(&app, &s1).as_deref(), Some("A"));
    // The second one is shown now, disarmed with a fresh stamp.
    let (second, stamp2, armed2) = top_stamp(&app).unwrap();
    assert_eq!(second, s2);
    assert!(!armed2);
    assert_ne!(stamp2, stamp);
    let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
    assert!(signed_in(&app, &s2).is_none(), "not armed yet");
    // Covered by another dialog: disarmed, re-armed when uncovered.
    let _ = app.update(Message::ArmConfirm(stamp2));
    let _ = app.update(Message::OpenQuickSwitcher);
    assert!(top_stamp(&app).is_none(), "covered by the quick switcher");
    let _ = app.update(Message::CloseModal);
    let (_, stamp3, armed3) = top_stamp(&app).unwrap();
    assert!(!armed3);
    assert_ne!(stamp3, stamp2);
    confirm(&mut app, &s2);
    assert_eq!(signed_in(&app, &s2).as_deref(), Some("B"));
}

/// Re-review #10: the same link twice, or after a transient lookup failure,
/// does not cancel the user's sign-in.
#[tokio::test]
async fn repeated_token_link_is_harmless() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    let _ = app.update(Message::DeepLinks(vec![token_link("U", "T")]));
    let _ = app.update(Message::DeepLinks(vec![token_link("U", "T")]));
    assert!(pending(&app, &sid));
    // Transient failure of the lookup: the claim is released...
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("U", "T"),
        Err(Failure::from(rocketchat::Error::Timeout("me"))),
    ));
    assert!(pending(&app, &sid));
    // ... and the same link works when delivered again.
    let t = app.update(Message::DeepLinks(vec![token_link("U", "T")]));
    assert!(t.units() >= 1, "lookup started again");
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("U", "T"),
        Ok(user("U", "u")),
    ));
    confirm(&mut app, &sid);
    assert_eq!(signed_in(&app, &sid).as_deref(), Some("U"));
}

/// Re-review #4/#11: a fetch that finishes after logout neither refills the
/// memory cache nor writes the purged disk cache.
#[tokio::test]
async fn image_results_after_logout_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let generation = app.images.generation(&sid);
    let url = format!("{SERVER}/avatar/bob?size=64");
    let _ = sign_out(
        &mut app,
        Message::Sidebar(
            sid.clone(),
            sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
        ),
    )
    .await;
    let mut late = fetched_png();
    let path = dir.path().join("cache/images/x/late.png");
    late.write = Some((path.clone(), vec![1, 2, 3]));
    let t = app.update(Message::ImageLoaded(
        sid.clone(),
        generation,
        url.clone(),
        late,
    ));
    assert!(!app.images.contains(&url));
    assert_eq!(t.units(), 0, "no cache write scheduled");
    assert!(!path.exists());
}

/// Re-review #9: unknown token kind (older configs) is never revoked by a
/// plain logout.
#[tokio::test]
async fn unknown_token_kind_is_kept_local() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    assert_eq!(app.config.server(&sid).and_then(|e| e.token_kind), None);
    // No session: a revoke would need a REST task; a local logout schedules
    // only the secret deletion and the login meta load.
    let local = sign_out(
        &mut app,
        Message::Sidebar(
            sid.clone(),
            sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
        ),
    )
    .await;
    let dir2 = tempfile::tempdir().unwrap();
    let (mut app2, sid2, _) = app_with(dir2.path(), Some("A"), vec![]);
    let everywhere = sign_out(
        &mut app2,
        Message::Sidebar(
            sid2.clone(),
            sidebar::Message::Status(rustket::workspace::status_menu::Message::LogoutEverywhere),
        ),
    )
    .await;
    assert_eq!(
        everywhere.units(),
        local.units() + 1,
        "revoke only when asked"
    );
}

/// Re-review #14: the sidebar width is saved (debounced).
#[tokio::test]
async fn sidebar_width_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sid, _) = app_with(dir.path(), None, vec![]);
    let _ = app.update(Message::SidebarResized(333.0));
    let _ = app.update(Message::SidebarResized(345.0));
    // Only the last debounce timer saves.
    let _ = app.update(Message::SaveDebounced(1));
    app.flush_config();
    assert_ne!(
        rustket::ui_state::UiState::load(dir.path()).sidebar_width,
        345.0
    );
    let _ = app.update(Message::SaveDebounced(2));
    app.flush_config();
    assert_eq!(
        rustket::ui_state::UiState::load(dir.path()).sidebar_width,
        345.0
    );
}

/// Component stubs are reachable: rail gear, status menu, sidebar buttons, Ctrl+K.
#[tokio::test]
async fn component_navigation() {
    use rustket::app::Screen;
    use rustket::workspace::{rail, status_menu};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let _ = app.update(Message::Rail(rail::Message::Settings));
    assert!(matches!(app.screen, Screen::Settings(_)));
    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::SetTheme(rustket_core::config::Theme::Dark),
    ));
    assert_eq!(app.config.ui.theme, rustket_core::config::Theme::Dark);
    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));
    assert!(matches!(app.screen, Screen::Server));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(status_menu::Message::Account),
    ));
    assert!(matches!(app.screen, Screen::Account(..)));
    let _ = app.update(Message::Account(
        sid.clone(),
        rustket::screens::account::Message::Close,
    ));
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Directory));
    assert!(matches!(app.screen, Screen::Directory(..)));
    let _ = app.update(Message::Directory(
        sid.clone(),
        rustket::screens::directory::Message::Close,
    ));
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Create));
    assert!(matches!(app.modals.last(), Some(Modal::CreateRoom(..))));
    let _ = app.update(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Close,
    ));
    assert!(app.modals.is_empty());
    let _ = app.update(Message::OpenQuickSwitcher);
    let _ = app.update(Message::OpenQuickSwitcher);
    assert_eq!(app.modals.len(), 1);
    let _ = app.update(Message::QuickSwitcher(
        sid.clone(),
        rustket::dialogs::quick_switcher::Message::Close,
    ));
    assert!(app.modals.is_empty());
    // Views render without panicking.
    let _ = app.view();
}

/// Alt(+Shift)+Up/Down walk the sidebar; the desktop layer sees unread totals.
#[tokio::test]
async fn room_shortcuts_and_desktop_totals() {
    use rustket::desktop::shortcuts::RoomStep;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid, _) = app_with(dir.path(), Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(
        &app,
        &sid,
        initial(me, &[("r1", 0), ("r2", 2), ("r3", 0), ("r4", 1)]),
    );
    let _ = app.update(m);
    assert_eq!(app.desktop.tray.state().unread, 2);
    let order: Vec<String> = app.servers[&sid]
        .sidebar
        .sections
        .iter()
        .flat_map(|s| s.items.iter().map(|i| i.rid.clone()))
        .collect();
    // Same activity: sorted by name.
    assert_eq!(order, ["r1", "r2", "r3", "r4"]);
    let step = |delta, unread_only| {
        Message::Desktop(rustket::desktop::Message::RoomStep(RoomStep {
            delta,
            unread_only,
        }))
    };
    let active = |app: &App| app.servers[&sid].active_room.clone();
    let _ = app.update(step(1, false));
    assert_eq!(active(&app).as_deref(), Some("r1"));
    let _ = app.update(step(1, false));
    assert_eq!(active(&app).as_deref(), Some("r2"));
    // Next unread skips the read r3.
    let _ = app.update(step(1, true));
    assert_eq!(active(&app).as_deref(), Some("r4"));
    // Previous unread from r4 skips r3 again.
    let _ = app.update(step(-1, true));
    assert_eq!(active(&app).as_deref(), Some("r2"));
    // Previous wraps from the first room to the last.
    let _ = app.update(step(-1, false));
    let _ = app.update(step(-1, false));
    assert_eq!(active(&app).as_deref(), Some("r4"));
    // A second launch without links raises the (inert) window: no work.
    let t = app.update(Message::DeepLinks(Vec::new()));
    assert_eq!(t.units(), 0);
}

// --- integration of the room components ------------------------------------------------

/// Logged in as alice with rooms `r1`, `r2` loaded; `r1` open.
fn loaded_app(dir: &std::path::Path) -> (App, ServerId) {
    let (mut app, sid, _) = app_with(dir, Some("A"), vec![]);
    let me = user("A", "alice");
    let m = ev(
        &app,
        &sid,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(&app, &sid, initial(me, &[("r1", 0), ("r2", 0)]));
    let _ = app.update(m);
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    (app, sid)
}

fn rc_msg(id: &str, rid: &str, ms: i64, from: &str) -> rocketchat::models::Message {
    rocketchat::models::Message {
        id: id.into(),
        rid: rid.into(),
        msg: format!("text {id}"),
        ts: Some(ts(ms)),
        updated_at: Some(ts(ms)),
        u: UserRef {
            id: from.into(),
            username: Some(from.to_lowercase()),
            name: None,
        },
        ..Default::default()
    }
}

fn deliver(app: &mut App, sid: &ServerId, m: rocketchat::models::Message) {
    let e = ev(
        app,
        sid,
        SessionEvent::Message {
            message: Box::new(m),
            room_participant: true,
        },
    );
    let _ = app.update(e);
}

fn room_msg(app: &App, sid: &ServerId, rid: &str, m: rustket::room::Message) -> Message {
    Message::Room(sid.clone(), app.servers[sid].generation, rid.into(), m)
}

fn set_unread(app: &mut App, sid: &ServerId, rid: &str, unread: u64, at: i64) {
    let e = ev(
        app,
        sid,
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Updated,
            sub: Box::new(sub(rid, "A", unread, at)),
        },
    );
    let _ = app.update(e);
}

/// Applies "marked unread on another device" to the workspace only and
/// returns whether the open room would be marked read again.
fn unread_elsewhere_wants_read(app: &mut App, sid: &ServerId, at: i64) -> bool {
    let s = app.servers.get_mut(sid).unwrap();
    s.workspace
        .apply_subscription(rustket_core::Change::Updated, sub("r1", "A", 1, at));
    s.rooms["r1"].wants_auto_read(&s.workspace)
}

/// "Mark unread" leaves the room (like the web client) and nothing marks it
/// read again; the automatic read mark only follows new messages.
#[tokio::test]
async fn mark_unread_sticks() {
    use rustket::room::timeline::{Act, Message as Tm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    // `subscriptions.read` of the open finished (results are not run here).
    let read_done =
        |app: &App| room_msg(app, &sid, "r1", rustket::room::Message::MarkedRead(Ok(())));
    let _ = app.update(read_done(&app));
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    // New message + unread subscription: the open room is marked read (and
    // remembers up to which message).
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    let s = &app.servers[&sid];
    assert!(!s.rooms["r1"].wants_auto_read(&s.workspace));
    let _ = app.update(read_done(&app));
    // Marked unread elsewhere (no new message): not undone. The predicate is
    // checked before the app sees the change (afterwards a read mark it ran
    // would hide the problem).
    assert!(!unread_elsewhere_wants_read(&mut app, &sid, T0 + 12));
    set_unread(&mut app, &sid, "r1", 3, T0 + 12);
    // A newer message while the window is in the background: marked read
    // once it is focused again.
    let _ = app.update(Message::WindowFocused(false));
    deliver(&mut app, &sid, rc_msg("m2", "r1", T0 + 20, "B"));
    let s = &app.servers[&sid];
    assert!(s.rooms["r1"].wants_auto_read(&s.workspace));
    let _ = app.update(Message::WindowFocused(true));
    let s = &app.servers[&sid];
    assert!(!s.rooms["r1"].wants_auto_read(&s.workspace));

    // The message action leaves the room.
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Timeline(Tm::Item(rustket::room::timeline::ItemMsg::Act(
            "m1".into(),
            Act::MarkUnread,
        ))),
    );
    let _ = app.update(m);
    let s = &app.servers[&sid];
    assert!(s.rooms.is_empty());
    assert_eq!(s.active_room, None);
    // Late unread updates find no open room to mark.
    set_unread(&mut app, &sid, "r1", 3, T0 + 30);
    assert!(app.servers[&sid].rooms.is_empty());
    let _ = app.update(Message::WindowFocused(true));
    assert!(app.servers[&sid].rooms.is_empty());
}

/// Retry/discard of failed messages (room and thread) go through the
/// composer outbox; the composer has no list of its own.
#[tokio::test]
async fn failed_messages_retry_and_discard_through_the_outbox() {
    use rustket::room::timeline::Message as Tm;
    use rustket_core::timeline::DeliveryState;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let room_m = ws.new_local_message("r1", "hello", None, false);
    let thread_m = ws.new_local_message("r1", "reply", Some("parent"), false);
    let (a, b) = (room_m.id.clone(), thread_m.id.clone());
    for m in [room_m, thread_m] {
        let (id, tmid) = (m.id.clone(), m.tmid.clone());
        ws.add_pending(m);
        ws.mark_pending_failed("r1", tmid.as_deref(), &id, "offline");
    }
    let state = |app: &App, id: &str| {
        app.servers[&sid]
            .workspace
            .pending_messages("r1")
            .into_iter()
            .find(|i| i.message.id == id)
            .map(|i| i.state.clone())
    };
    for id in [&a, &b] {
        let m = room_msg(
            &app,
            &sid,
            "r1",
            rustket::room::Message::Timeline(Tm::Item(rustket::room::timeline::ItemMsg::Retry(
                id.clone(),
            ))),
        );
        let _ = app.update(m);
        assert_eq!(state(&app, id), Some(DeliveryState::Sending), "{id}");
    }
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Timeline(Tm::Item(rustket::room::timeline::ItemMsg::Discard(
            b.clone(),
        ))),
    );
    let _ = app.update(m);
    assert_eq!(state(&app, &b), None);
    let _ = app.view();
}

/// A room opened without joining is a preview: not in the sidebar, and
/// leaving it removes it from the workspace; once joined it stays.
#[tokio::test]
async fn previewed_rooms_are_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .apply_room(rustket_core::Change::Updated, room("p1", T0));
    let _ = app.update(Message::Noop);
    let _ = app.open_room(&sid, "p1");
    let _ = app.update(Message::Noop);
    {
        let s = &app.servers[&sid];
        assert_eq!(s.active_room.as_deref(), Some("p1"));
        assert!(s.rooms["p1"].is_preview(&s.workspace));
        assert!(
            !s.sidebar
                .sections
                .iter()
                .any(|sec| sec.items.iter().any(|i| i.rid == "p1"))
        );
    }
    let _ = app.view();
    let _ = app.open_room(&sid, "r2");
    let s = &app.servers[&sid];
    assert!(s.workspace.room("p1").is_none(), "preview forgotten");
    assert!(s.workspace.timeline("p1").is_none());
    assert!(!s.rooms.contains_key("p1"));

    // Joined while previewed: it becomes a normal room and stays.
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .apply_room(rustket_core::Change::Updated, room("p2", T0));
    let _ = app.open_room(&sid, "p2");
    set_unread(&mut app, &sid, "p2", 0, T0 + 5);
    let s = &app.servers[&sid];
    assert!(!s.rooms["p2"].is_preview(&s.workspace));
    let _ = app.open_room(&sid, "r1");
    let _ = app.update(Message::Noop);
    let s = &app.servers[&sid];
    assert!(s.workspace.room("p2").is_some());
    assert!(
        s.sidebar
            .sections
            .iter()
            .any(|sec| sec.items.iter().any(|i| i.rid == "p2"))
    );
}

/// "Create discussion" from the room menu and "Start a discussion" from a
/// message open the dialog with the parent room (and message) preset.
#[tokio::test]
async fn discussions_start_from_the_room_and_messages() {
    use rustket::room::header::Message as Hm;
    use rustket::room::timeline::{Act, Message as Tm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    {
        let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
        for id in ["start-discussion", "start-discussion-other-user"] {
            ws.apply_permission(
                rustket_core::Change::Updated,
                rocketchat::models::Permission {
                    id: id.into(),
                    roles: vec!["user".into()],
                    updated_at: None,
                },
            );
        }
        ws.apply_setting(
            rustket_core::Change::Updated,
            rocketchat::models::Setting {
                id: "Discussion_enabled".into(),
                value: serde_json::json!(true),
                ..Default::default()
            },
        );
        let mut me = user("A", "alice");
        me.roles = Some(vec!["user".into()]);
        ws.apply_me(me);
    }
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Header(Hm::CreateDiscussion),
    );
    let _ = app.update(m);
    match app.modals.last() {
        Some(Modal::CreateRoom(_, c)) => {
            assert_eq!(c.parent.as_ref().map(|p| p.0.as_str()), Some("r1"));
            assert!(c.from_room && c.pmid.is_none());
        }
        _ => panic!("create dialog expected"),
    }
    let _ = app.view();
    let _ = app.update(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Close,
    ));
    assert!(app.modals.is_empty());
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Timeline(Tm::Item(rustket::room::timeline::ItemMsg::Act(
            "m1".into(),
            Act::StartDiscussion,
        ))),
    );
    let _ = app.update(m);
    match app.modals.last() {
        Some(Modal::CreateRoom(_, c)) => {
            assert_eq!(c.pmid.as_ref().map(|p| p.0.as_str()), Some("m1"));
            assert_eq!(c.pmid.as_ref().map(|p| p.1.as_str()), Some("text m1"));
        }
        _ => panic!("create dialog expected"),
    }
}

/// Custom status texts used in the status menu are kept in the config.
#[tokio::test]
async fn recent_status_texts_are_saved() {
    use rustket::workspace::status_menu::Message as Sm;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
    let _ = app.update(Message::StatusMenu(
        sid.clone(),
        Sm::TextChanged("On a call".into()),
    ));
    let _ = app.update(Message::StatusMenu(sid.clone(), Sm::SubmitText));
    assert_eq!(app.config.recent_status_texts, ["On a call"]);
    app.flush_config();
    let back = AppConfig::load(dir.path()).unwrap();
    assert_eq!(back.recent_status_texts, ["On a call"]);
}

/// The room was opened while read and its history arrived afterwards (no
/// new message): an unread mark made on another device is kept.
#[tokio::test]
async fn unread_elsewhere_after_quiet_history_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::MarkedRead(Ok(())),
    ));
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let (cursor, ticket) = ws.begin_load_older_ticket("r1").unwrap();
    let page = vec![rc_msg("h1", "r1", T0 - 50, "B")];
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::HistoryPage(cursor, ticket, Ok(page)),
    ));
    assert!(
        app.servers[&sid]
            .workspace
            .timeline("r1")
            .unwrap()
            .get("h1")
            .is_some()
    );
    assert!(!unread_elsewhere_wants_read(&mut app, &sid, T0 + 12));
}

/// Own messages never count as unread: marked unread elsewhere after an own
/// message is kept; the server's `ls` of a read room is a baseline too.
#[tokio::test]
async fn unread_elsewhere_after_own_message_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let read_done =
        |app: &App| room_msg(app, &sid, "r1", rustket::room::Message::MarkedRead(Ok(())));
    let _ = app.update(read_done(&app));
    deliver(&mut app, &sid, rc_msg("b1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11); // auto-read
    let _ = app.update(read_done(&app));
    set_unread(&mut app, &sid, "r1", 0, T0 + 12);
    deliver(&mut app, &sid, rc_msg("own1", "r1", T0 + 20, "A"));
    assert!(!unread_elsewhere_wants_read(&mut app, &sid, T0 + 30));

    // A message from someone else read on another device (`ls` after it).
    set_unread(&mut app, &sid, "r1", 0, T0 + 31);
    deliver(&mut app, &sid, rc_msg("b2", "r1", T0 + 40, "B"));
    let mut read = sub("r1", "A", 0, T0 + 41);
    read.ls = Some(ts(T0 + 41));
    let e = ev(
        &app,
        &sid,
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Updated,
            sub: Box::new(read),
        },
    );
    let _ = app.update(e);
    assert!(!unread_elsewhere_wants_read(&mut app, &sid, T0 + 50));
    // A newer message from someone else still enables the auto-read (in
    // the background: nothing marks it read yet).
    let _ = app.update(Message::WindowFocused(false));
    deliver(&mut app, &sid, rc_msg("b3", "r1", T0 + 60, "B"));
    assert!(unread_elsewhere_wants_read(&mut app, &sid, T0 + 61));
}

/// A reply in a followed thread sets `alert` and `tunread` (it is not in the
/// room timeline): the automatic read mark leaves it to the thread, like the
/// web (`subscriptions.read` without `readThreads` keeps that alert, so
/// marking it would only loop).
#[tokio::test]
async fn thread_reply_alert_is_kept_for_the_thread() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let read_done =
        |app: &App| room_msg(app, &sid, "r1", rustket::room::Message::MarkedRead(Ok(())));
    let _ = app.update(read_done(&app));
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    let _ = app.update(read_done(&app));
    set_unread(&mut app, &sid, "r1", 0, T0 + 12);
    let mut reply = rc_msg("t1", "r1", T0 + 20, "B");
    reply.tmid = Some("p1".into());
    deliver(&mut app, &sid, reply);
    let mut alerted = sub("r1", "A", 0, T0 + 21);
    alerted.alert = true;
    alerted.tunread = Some(vec!["p1".into()]);
    let s = app.servers.get_mut(&sid).unwrap();
    s.workspace
        .apply_subscription(rustket_core::Change::Updated, alerted);
    assert!(!s.rooms["r1"].wants_auto_read(&s.workspace));
}

/// An alert without unread threads is still cleared by the open room.
#[tokio::test]
async fn thread_reply_alert_is_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let read_done =
        |app: &App| room_msg(app, &sid, "r1", rustket::room::Message::MarkedRead(Ok(())));
    let _ = app.update(read_done(&app));
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    let _ = app.update(read_done(&app));
    set_unread(&mut app, &sid, "r1", 0, T0 + 12);
    let mut reply = rc_msg("t1", "r1", T0 + 20, "B");
    reply.tmid = Some("p1".into());
    deliver(&mut app, &sid, reply);
    let mut alerted = sub("r1", "A", 0, T0 + 21);
    alerted.alert = true;
    let s = app.servers.get_mut(&sid).unwrap();
    s.workspace
        .apply_subscription(rustket_core::Change::Updated, alerted);
    assert!(s.rooms["r1"].wants_auto_read(&s.workspace));
}

/// preview-c-room denied: the lock state loads nothing; after joining the
/// history is requested.
#[tokio::test]
async fn join_after_blocked_preview_loads_history() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    {
        let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
        ws.apply_permission(
            rustket_core::Change::Updated,
            rocketchat::models::Permission {
                id: "preview-c-room".into(),
                roles: vec!["admin".into()],
                updated_at: None,
            },
        );
        let mut me = user("A", "alice");
        me.roles = Some(vec!["user".into()]);
        ws.apply_me(me);
        ws.apply_room(rustket_core::Change::Updated, room("p1", T0));
    }
    let _ = app.open_room(&sid, "p1");
    {
        let s = &app.servers[&sid];
        assert!(rustket::room::preview_blocked(&s.workspace, "p1"));
        let p = &s.workspace.timeline("p1").unwrap().pagination;
        assert!(p.error.is_none() && !p.loaded && !p.loading_before);
    }
    set_unread(&mut app, &sid, "p1", 0, T0 + 5);
    let s = &app.servers[&sid];
    assert!(!rustket::room::preview_blocked(&s.workspace, "p1"));
    // Without a session the request fails at once ("not connected").
    let p = &s.workspace.timeline("p1").unwrap().pagination;
    assert!(p.error.is_some() || p.loading_before || p.loaded);
}

/// "Create discussion" and "Start a discussion" inside a discussion use the
/// discussion's parent (the server rejects nested discussions).
#[tokio::test]
async fn create_discussion_inside_a_discussion_uses_parent() {
    use rustket::dialogs::create_room::{Kind, Request};
    use rustket::room::header::Message as Hm;
    use rustket::room::timeline::{Act, Message as Tm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    {
        let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
        for id in ["start-discussion", "start-discussion-other-user"] {
            ws.apply_permission(
                rustket_core::Change::Updated,
                rocketchat::models::Permission {
                    id: id.into(),
                    roles: vec!["user".into()],
                    updated_at: None,
                },
            );
        }
        ws.apply_setting(
            rustket_core::Change::Updated,
            rocketchat::models::Setting {
                id: "Discussion_enabled".into(),
                value: serde_json::json!(true),
                ..Default::default()
            },
        );
        let mut me = user("A", "alice");
        me.roles = Some(vec!["user".into()]);
        ws.apply_me(me);
        let mut d = room("d1", T0);
        d.prid = Some("r1".into());
        ws.apply_room(rustket_core::Change::Updated, d);
        let mut s = sub("d1", "A", 0, T0);
        s.prid = Some("r1".into());
        ws.apply_subscription(rustket_core::Change::Inserted, s);
    }
    let _ = app.open_room(&sid, "d1");
    let dialog = |app: &App| match app.modals.last() {
        Some(Modal::CreateRoom(_, c)) => {
            let (name, kind) = (
                c.parent.clone().unwrap().1,
                c.effective_kind(&app.servers[&sid].workspace),
            );
            match c.request(Kind::Discussion) {
                Request::Discussion { prid, pmid, .. } => (prid, pmid, name, kind, c.name.clone()),
                _ => unreachable!(),
            }
        }
        _ => panic!("create dialog expected"),
    };
    let m = room_msg(
        &app,
        &sid,
        "d1",
        rustket::room::Message::Header(Hm::CreateDiscussion),
    );
    let _ = app.update(m);
    let (prid, pmid, parent_name, kind, _) = dialog(&app);
    assert_eq!((prid.as_str(), pmid), ("r1", None));
    assert_eq!(parent_name, "r1");
    assert_eq!(kind, Some(Kind::Discussion));
    let _ = app.update(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Close,
    ));
    deliver(&mut app, &sid, rc_msg("m1", "d1", T0 + 10, "B"));
    let m = room_msg(
        &app,
        &sid,
        "d1",
        rustket::room::Message::Timeline(Tm::Item(rustket::room::timeline::ItemMsg::Act(
            "m1".into(),
            Act::StartDiscussion,
        ))),
    );
    let _ = app.update(m);
    let (prid, pmid, _, _, name) = dialog(&app);
    assert_eq!((prid.as_str(), pmid.as_deref()), ("r1", Some("m1")));
    // The name is prefilled from the message, as in the web client.
    assert_eq!(name, "text m1");
}

// --- read state and sending (review round) ---------------------------------------------

/// Outputs of a task within `ms` (tasks that never end are cut off). Runs
/// its own runtime: call from a plain `#[test]`.
fn outputs_within(task: iced::Task<Message>, ms: u64) -> Vec<Message> {
    use futures::StreamExt;
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut out = Vec::new();
        let deadline = tokio::time::sleep(std::time::Duration::from_millis(ms));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                next = stream.next() => match next {
                    Some(iced_runtime::Action::Output(m)) => out.push(m),
                    Some(_) => {}
                    None => break,
                },
            }
        }
        out
    })
}

fn marked_read(outs: &[Message], sid: &ServerId, rid: &str) -> bool {
    outs.iter().any(|m| {
        matches!(m, Message::Room(s, _, r, rustket::room::Message::MarkedRead(_)) if s == sid && r == rid)
    })
}

/// Review: the room hidden behind the Directory, Settings, Account or
/// Administration page was marked read when messages arrived or the window
/// was focused; it is marked once the room is shown again.
#[tokio::test]
async fn hidden_room_is_marked_read_only_when_shown_again() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let read_done =
        |app: &App| room_msg(app, &sid, "r1", rustket::room::Message::MarkedRead(Ok(())));
    let _ = app.update(read_done(&app));
    let wants = |app: &App| {
        let s = &app.servers[&sid];
        s.rooms["r1"].wants_auto_read(&s.workspace)
    };
    // Directory page: not read.
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Directory));
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    assert!(
        wants(&app),
        "room behind the Directory page was marked read"
    );
    // Settings page and a window focus: still not read.
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Settings));
    let _ = app.update(Message::WindowFocused(false));
    deliver(&mut app, &sid, rc_msg("m2", "r1", T0 + 20, "B"));
    set_unread(&mut app, &sid, "r1", 2, T0 + 21);
    let _ = app.update(Message::WindowFocused(true));
    assert!(
        wants(&app),
        "focusing on the Settings page marked the room read"
    );
    // Back to the server view: read now.
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Select(
        sid.clone(),
    )));
    assert!(!wants(&app), "read once shown");
    let _ = app.update(read_done(&app));
    // In the background: kept until the window is focused again.
    let _ = app.update(Message::WindowFocused(false));
    deliver(&mut app, &sid, rc_msg("m3", "r1", T0 + 30, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 31);
    assert!(wants(&app));
    let _ = app.update(Message::WindowFocused(true));
    assert!(!wants(&app));
}

/// Review: at startup the last room of a server in the background was
/// opened and marked read; it opens (and is read) when that server is shown.
#[test]
fn background_servers_last_room_waits_for_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    let mut e1 = ServerEntry::new(SERVER, "One");
    e1.user_id = Some("A".into());
    let s1 = cfg.add_server(e1);
    let mut e2 = ServerEntry::new("http://127.0.0.1:3000", "Two");
    e2.user_id = Some("B".into());
    e2.last_room = Some("x1".into());
    let s2 = cfg.add_server(e2);
    cfg.save(dir.path()).unwrap();
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(dir.path().to_owned()),
        cache_dir: Some(dir.path().join("cache")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    let store = Arc::new(MemoryStore::new());
    store
        .set(&rustket_core::secrets::secret_key(&s1, "A"), "tA")
        .unwrap();
    store
        .set(&rustket_core::secrets::secret_key(&s2, "B"), "tB")
        .unwrap();
    boot(
        &mut app,
        vec![
            (s1.clone(), Some("tA".into())),
            (s2.clone(), Some("tB".into())),
        ],
        store,
    );
    assert_eq!(app.active.as_ref(), Some(&s1));
    let me = user("B", "bob");
    let m = ev(
        &app,
        &s2,
        SessionEvent::Ready {
            me: Box::new(me.clone()),
        },
    );
    let _ = app.update(m);
    let m = loaded(&app, &s2, initial(me, &[("x1", 3)]));
    let task = app.update(m);
    assert!(!app.servers[&s2].rooms.contains_key("x1"));
    assert!(!marked_read(&outputs_within(task, 1000), &s2, "x1"));
    // Shown: opened and read.
    let task = app.update(Message::Rail(rustket::workspace::rail::Message::Select(
        s2.clone(),
    )));
    assert!(app.servers[&s2].rooms.contains_key("x1"));
    assert!(marked_read(&outputs_within(task, 1000), &s2, "x1"));
}

/// Review: a thread reply that failed after its panel was closed stayed
/// "sending" forever.
#[tokio::test]
async fn failed_reply_of_a_closed_thread_is_marked_failed() {
    use rustket::room::panel::{BodyMsg, Kind, Message as Pm, thread};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("p1".into()))),
    );
    let _ = app.update(m);
    let g = app.servers[&sid].rooms["r1"].panel.generation();
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Activate(g)),
    ));
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let reply = ws.new_local_message("r1", "a reply", Some("p1"), false);
    ws.add_pending(reply.clone());
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Close),
    ));
    let failed = Err(Failure::from(rocketchat::Error::Unauthorized {
        message: None,
        code: None,
    }));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Body(
            g,
            BodyMsg::Thread(thread::Msg::Composer(
                rustket::room::composer::Message::Sent(reply.id.clone(), failed),
            )),
        )),
    ));
    let pending = app.servers[&sid].workspace.pending_messages("r1");
    let item = pending.iter().find(|i| i.message.id == reply.id).unwrap();
    assert!(
        matches!(
            item.state,
            rustket_core::timeline::DeliveryState::Failed { .. }
        ),
        "{:?}",
        item.state
    );
}

/// Review: an upload whose room view was closed was lost without a word;
/// its failure is reported.
#[tokio::test]
async fn failed_upload_of_a_closed_room_is_reported() {
    use rustket::room::composer::Message as Cm;
    use rustket::room::upload::Message as Um;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let failed = Err(Failure::from(rocketchat::Error::Unauthorized {
        message: None,
        code: None,
    }));
    let before = app.toasts.len();
    // r2 is not open.
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r2",
        rustket::room::Message::Composer(Cm::Upload(Um::Done(7, "clip.mp4".into(), failed))),
    ));
    assert_eq!(app.toasts.len(), before + 1);
    assert!(app.toasts.last().unwrap().title.contains("clip.mp4"));
}

/// Review: dropped files went to the hidden room's composer on other pages
/// and to the room composer while a thread was open.
#[test]
fn dropped_files_go_to_the_shown_composer() {
    use rustket::room::panel::{BodyMsg, Kind, Message as Pm};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, b"hello").unwrap();
    let dropped = |outs: &[Message]| {
        outs.iter()
            .filter(|m| {
                matches!(
                    m,
                    Message::Room(_, _, _, rustket::room::Message::FilesDropped(_))
                )
            })
            .count()
    };
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Settings));
    let outs = outputs_within(app.update(Message::FileDropped(file.clone())), 300);
    assert_eq!(dropped(&outs), 0, "no room on screen");
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Select(
        sid.clone(),
    )));
    // A thread is open: its composer takes the files.
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("p1".into()))),
    ));
    let g = app.servers[&sid].rooms["r1"].panel.generation();
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Activate(g)),
    ));
    let outs = outputs_within(app.update(Message::FileDropped(file)), 300);
    assert_eq!(dropped(&outs), 1);
    let m = outs
        .into_iter()
        .find(|m| {
            matches!(
                m,
                Message::Room(_, _, _, rustket::room::Message::FilesDropped(_))
            )
        })
        .unwrap();
    let outs = outputs_within(app.update(m), 2000);
    assert!(
        outs.iter().any(|m| matches!(
            m,
            Message::Room(
                _,
                _,
                _,
                rustket::room::Message::Panel(Pm::Body(_, BodyMsg::Thread(_)))
            )
        )),
        "to the thread composer"
    );
    assert!(
        !app.servers[&sid].rooms["r1"]
            .composer
            .is_upload_dialog_open()
    );
}

/// A failed thread upload of a closed room view is reported even when the
/// reopened room view has another panel open (generations are never reused
/// across room views).
#[tokio::test]
async fn orphan_thread_upload_of_a_reopened_room_is_reported() {
    use rustket::room::composer::Message as Cm;
    use rustket::room::panel::{BodyMsg, Kind, Message as Pm, thread};
    use rustket::room::upload::Message as Um;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("p1".into()))),
    ));
    let g_old = app.servers[&sid].rooms["r1"].panel.generation();
    // Room switch and back: a new RoomView for r1.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Members)),
    ));
    let g_new = app.servers[&sid].rooms["r1"].panel.generation();
    assert_ne!(g_old, g_new, "generation reused across room views");
    let before = app.toasts.len();
    let failed = Err(Failure::from(rocketchat::Error::Unauthorized {
        message: None,
        code: None,
    }));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Body(
            g_old,
            BodyMsg::Thread(thread::Msg::Composer(Cm::Upload(Um::Done(
                9,
                "clip.mp4".into(),
                failed,
            )))),
        )),
    ));
    assert_eq!(
        app.toasts.len(),
        before + 1,
        "failed upload of the old thread not reported"
    );
}

/// Messages a task yields right away (`Task::done`).
fn ready_outputs(task: iced::Task<Message>) -> Vec<Message> {
    use futures::{FutureExt as _, StreamExt as _};
    let mut out = Vec::new();
    if let Some(mut stream) = iced_runtime::task::into_stream(task) {
        while let Some(Some(action)) = stream.next().now_or_never() {
            if let iced_runtime::Action::Output(m) = action {
                out.push(m);
            }
        }
    }
    out
}

/// Phase 13: an Escape no widget took closes the open room's right panel,
/// then nothing more; on the settings page it closes the page.
#[tokio::test]
async fn escape_closes_the_panel_and_the_settings_page() {
    use rustket::app::Screen;
    use rustket::room::panel::{Kind, Message as Pm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let m = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Info)),
    );
    let _ = app.update(m);
    assert!(app.servers[&sid].rooms["r1"].panel.kind().is_some());
    for m in ready_outputs(app.update(Message::Escape)) {
        let _ = app.update(m);
    }
    assert!(app.servers[&sid].rooms["r1"].panel.kind().is_none());
    assert!(ready_outputs(app.update(Message::Escape)).is_empty());
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Settings));
    assert!(matches!(app.screen, Screen::Settings(_)));
    for m in ready_outputs(app.update(Message::Escape)) {
        let _ = app.update(m);
    }
    assert!(matches!(app.screen, Screen::Server));
    // The directory and account pages close the same way.
    app.screen = Screen::Directory(
        sid.clone(),
        Box::new(rustket::screens::directory::Directory::new()),
    );
    for m in ready_outputs(app.update(Message::Escape)) {
        let _ = app.update(m);
    }
    assert!(matches!(app.screen, Screen::Server));
    app.screen = Screen::Account(
        sid.clone(),
        Box::new(rustket::screens::account::Account::new()),
    );
    for m in ready_outputs(app.update(Message::Escape)) {
        let _ = app.update(m);
    }
    assert!(matches!(app.screen, Screen::Server));
}

// ------------------------------------------------------ focus in the view --

struct NoClip;
impl iced::advanced::Clipboard for NoClip {
    fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
}

/// The focused widgets (id, bounds) of a view.
struct Focused(Vec<(Option<iced::advanced::widget::Id>, iced::Rectangle)>);
impl iced::advanced::widget::Operation for Focused {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
        operate(self);
    }
    fn focusable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if state.is_focused() {
            self.0.push((id.cloned(), bounds));
        }
    }
}

type Ui<'a> = iced_runtime::user_interface::UserInterface<'a, Message, iced::Theme, iced::Renderer>;

fn test_renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        iced::Pixels(rustket_ui::theme::TEXT_BODY),
    ))
}

/// Runs the widget operations of `task` (and their chains) on `ui`.
fn run_ops(ui: &mut Ui<'_>, renderer: &iced::Renderer, task: iced::Task<Message>) {
    use futures::{FutureExt as _, StreamExt as _};
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return;
    };
    while let Some(Some(action)) = stream.next().now_or_never() {
        if let iced_runtime::Action::Widget(op) = action {
            let mut current = Some(op);
            while let Some(mut op) = current.take() {
                ui.operate(renderer, op.as_mut());
                if let iced::advanced::widget::operation::Outcome::Chain(next) = op.finish() {
                    current = Some(next);
                }
            }
        }
    }
}

fn focused(
    ui: &mut Ui<'_>,
    renderer: &iced::Renderer,
) -> Vec<(Option<iced::advanced::widget::Id>, iced::Rectangle)> {
    let mut f = Focused(Vec::new());
    ui.operate(renderer, &mut f);
    f.0
}

fn press(named: Option<iced::keyboard::key::Named>, ch: Option<&str>) -> iced::Event {
    use iced::keyboard;
    let k = match (named, ch) {
        (Some(n), _) => keyboard::Key::Named(n),
        (None, Some(c)) => keyboard::Key::Character(c.into()),
        _ => keyboard::Key::Unidentified,
    };
    iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: k.clone(),
        modified_key: k,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: ch.map(Into::into),
        repeat: false,
    })
}

/// Phase 13 review: with the thread expanded over the room column, F6 /
/// Shift+F6 never focus the main composer under it, so typing goes nowhere
/// near the main room.
#[tokio::test]
async fn f6_skips_the_room_column_under_an_expanded_thread() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket::room::panel::{BodyMsg, Kind, Message as Pm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    let open = room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("m1".into()))),
    );
    let _ = app.update(open);
    let g = app.servers[&sid].rooms["r1"].panel.generation();
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Activate(g)),
    ));
    let g = app.servers[&sid].rooms["r1"].panel.generation();
    let mut parent = rc_msg("m1", "r1", T0 + 10, "B");
    parent.tcount = Some(1);
    let thread = |m| rustket::room::Message::Panel(Pm::Body(g, BodyMsg::Thread(m)));
    let m = room_msg(
        &app,
        &sid,
        "r1",
        thread(rustket::room::panel::thread::Msg::Parent(Ok(Box::new(
            parent,
        )))),
    );
    let _ = app.update(m);
    let m = room_msg(
        &app,
        &sid,
        "r1",
        thread(rustket::room::panel::thread::Msg::ToggleExpand),
    );
    let _ = app.update(m);
    assert!(app.servers[&sid].rooms["r1"].panel.expanded());
    let mut renderer = test_renderer();
    let mut ui = UserInterface::build(
        app.view(),
        iced::Size::new(1280.0, 800.0),
        Cache::default(),
        &mut renderer,
    );
    let thread_editor = rustket::focus::composer_id("r1", Some("m1"));
    let main_editor = rustket::focus::composer_id("r1", None);
    run_ops(
        &mut ui,
        &renderer,
        iced::widget::operation::focus(thread_editor.clone()),
    );
    assert_eq!(
        focused(&mut ui, &renderer)
            .first()
            .and_then(|f| f.0.clone()),
        Some(thread_editor.clone())
    );
    let regions = || rustket::focus::regions(Some(("r1", Some("m1"))));
    for forward in [false, true, true, true] {
        run_ops(
            &mut ui,
            &renderer,
            rustket_ui::widget::focus_region(regions(), forward),
        );
        let now = focused(&mut ui, &renderer);
        assert_eq!(now.len(), 1, "{now:?}");
        assert_ne!(now[0].0.as_ref(), Some(&main_editor), "forward {forward}");
    }
    // Focused programmatically anyway: the covered composer drops it.
    run_ops(
        &mut ui,
        &renderer,
        iced::widget::operation::focus(main_editor.clone()),
    );
    assert!(
        focused(&mut ui, &renderer)
            .iter()
            .all(|f| f.0.as_ref() != Some(&main_editor))
    );
    let mut msgs = Vec::new();
    let cursor = iced::mouse::Cursor::Available(iced::Point::new(-10.0, -10.0));
    let evs = [
        press(None, Some("h")),
        press(Some(iced::keyboard::key::Named::Enter), None),
    ];
    let _ = ui.update(&evs, cursor, &mut renderer, &mut NoClip, &mut msgs);
    let main_send = msgs
        .iter()
        .any(|m| format!("{m:?}").contains("\"r1\", Composer("));
    assert!(!main_send, "{msgs:?}");
}

/// Phase 13 review: Escape in the focused composer with nothing to cancel
/// only unfocuses it (captured), so the right panel stays open.
#[tokio::test]
async fn escape_in_the_composer_does_not_close_the_panel() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket::room::panel::{Kind, Message as Pm};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Info)),
    ));
    let mut renderer = test_renderer();
    let mut ui = UserInterface::build(
        app.view(),
        iced::Size::new(1280.0, 800.0),
        Cache::default(),
        &mut renderer,
    );
    run_ops(
        &mut ui,
        &renderer,
        iced::widget::operation::focus(rustket::focus::composer_id("r1", None)),
    );
    let mut msgs = Vec::new();
    let cursor = iced::mouse::Cursor::Available(iced::Point::new(-10.0, -10.0));
    let (_, statuses) = ui.update(
        &[press(Some(iced::keyboard::key::Named::Escape), None)],
        cursor,
        &mut renderer,
        &mut NoClip,
        &mut msgs,
    );
    assert_eq!(statuses, [iced::event::Status::Captured]);
    drop(ui);
    for m in msgs {
        for m in ready_outputs(app.update(m)) {
            let _ = app.update(m);
        }
    }
    assert!(app.servers[&sid].rooms["r1"].panel.kind().is_some());
}

/// Phase 13 review: F6 into a timeline longer than the window focuses a
/// widget on screen (near the newest message), not the oldest built row.
#[tokio::test]
async fn f6_into_a_long_timeline_lands_on_screen() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    for i in 0..60 {
        let user = if i % 2 == 0 { "B" } else { "C" };
        deliver(
            &mut app,
            &sid,
            rc_msg(&format!("m{i:02}"), "r1", T0 + 10 + i * 1000, user),
        );
    }
    let mut renderer = test_renderer();
    let size = iced::Size::new(1280.0, 800.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    let cursor = iced::mouse::Cursor::Available(iced::Point::new(-10.0, -10.0));
    let mut msgs = Vec::new();
    let redraw = iced::Event::Window(iced::window::Event::RedrawRequested(
        std::time::Instant::now(),
    ));
    let _ = ui.update(
        std::slice::from_ref(&redraw),
        cursor,
        &mut renderer,
        &mut NoClip,
        &mut msgs,
    );
    run_ops(
        &mut ui,
        &renderer,
        iced::widget::operation::focus(rustket::workspace::sidebar::SEARCH_INPUT_ID),
    );
    let before = scrollables(&mut ui, &renderer);
    run_ops(
        &mut ui,
        &renderer,
        rustket_ui::widget::focus_region(rustket::focus::regions(Some(("r1", None))), true),
    );
    let now = focused(&mut ui, &renderer);
    assert_eq!(now.len(), 1, "{now:?}");
    let after = scrollables(&mut ui, &renderer);
    assert!(!after.is_empty());
    assert_eq!(before, after, "the timeline stays at the newest messages");
    let target = now[0].1;
    let on_screen = after.iter().any(|(b, t)| {
        let y = target.y - t.y;
        b.contains(iced::Point::new(target.center_x(), y + target.height / 2.0))
    });
    assert!(on_screen, "focused {target:?}, scrollables {after:?}");
}

/// Long scrollables of a view (bounds, translation).
fn scrollables(ui: &mut Ui<'_>, renderer: &iced::Renderer) -> Vec<(iced::Rectangle, iced::Vector)> {
    struct Sc(Vec<(iced::Rectangle, iced::Vector)>);
    impl iced::advanced::widget::Operation for Sc {
        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation),
        ) {
            operate(self);
        }
        fn scrollable(
            &mut self,
            _id: Option<&iced::advanced::widget::Id>,
            b: iced::Rectangle,
            c: iced::Rectangle,
            t: iced::Vector,
            _s: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            if c.height > b.height * 2.0 {
                self.0.push((b, t));
            }
        }
    }
    let mut sc = Sc(Vec::new());
    ui.operate(renderer, &mut sc);
    sc.0
}

// --- sidebar header menu ---------------------------------------------------------------

fn header(sid: &ServerId, act: sidebar::HeaderAct) -> Message {
    Message::Sidebar(sid.clone(), sidebar::Message::Header(act))
}

/// A header menu edit changes the layout and is saved; Ungroup… opens the
/// settings with the deletion to confirm, which drops the group and its
/// collapse state.
#[test]
fn header_menu_edits_the_layout_and_ungroups_through_settings() {
    use rustket_core::room_list::SectionKind;
    use rustket_core::sidebar_layout::{GroupType, LayoutEdit, SectionId};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let channels = SectionId::from(SectionKind::Channels);
    let favorites = SectionId::from(SectionKind::Favorites);
    let _ = app.update(header(
        &sid,
        sidebar::HeaderAct::Layout(LayoutEdit::MoveTo {
            id: channels.clone(),
            anchor: favorites.clone(),
            after: false,
        }),
    ));
    let pos = |order: &[SectionId], id: &SectionId| order.iter().position(|x| x == id).unwrap();
    app.flush_config();
    let disk = AppConfig::load(dir.path()).unwrap();
    assert!(pos(&disk.ui.sidebar_order, &channels) < pos(&disk.ui.sidebar_order, &favorites));

    let _ = app.update(header(
        &sid,
        sidebar::HeaderAct::Layout(LayoutEdit::AddGroup {
            name: "Work".into(),
            types: vec![GroupType::Channels],
        }),
    ));
    let gid = app.config.ui.sidebar_groups[0].id.clone();
    let key = format!("sidebar.group.{gid}");
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Toggle(SectionId::group(&gid)),
    ));
    assert!(app.ui_state.collapsed_sections.contains(&key));

    let _ = app.update(header(&sid, sidebar::HeaderAct::Ungroup(gid.clone())));
    let rustket::app::Screen::Settings(settings) = &app.screen else {
        panic!("settings not open");
    };
    assert_eq!(settings.confirm_delete.as_deref(), Some(gid.as_str()));
    assert_eq!(app.config.ui.sidebar_groups.len(), 1, "not deleted yet");

    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::Sidebar(
            rustket::screens::settings::SidebarMessage::GroupDeleteConfirm,
        ),
    ));
    assert!(app.config.ui.sidebar_groups.is_empty());
    assert!(!app.ui_state.collapsed_sections.contains(&key));
    app.flush_config();
    let disk = AppConfig::load(dir.path()).unwrap();
    assert!(disk.ui.sidebar_groups.is_empty());
    assert!(
        !rustket::ui_state::UiState::load(dir.path())
            .collapsed_sections
            .contains(&key)
    );
}

/// Bounds of the container with `id`.
fn container_bounds(
    ui: &mut Ui<'_>,
    renderer: &iced::Renderer,
    id: iced::advanced::widget::Id,
) -> Option<iced::Rectangle> {
    struct Find(iced::advanced::widget::Id, Option<iced::Rectangle>);
    impl iced::advanced::widget::Operation for Find {
        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation),
        ) {
            operate(self);
        }
        fn container(&mut self, id: Option<&iced::advanced::widget::Id>, b: iced::Rectangle) {
            if id == Some(&self.0) {
                self.1 = Some(b);
            }
        }
    }
    let mut f = Find(id, None);
    ui.operate(renderer, &mut f);
    f.1
}

/// "Sidebar settings…" opens the settings scrolled to the Sidebar card.
#[tokio::test]
async fn header_menu_settings_scrolls_to_the_sidebar_card() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let task = app.update(header(&sid, sidebar::HeaderAct::OpenSettings));
    assert!(matches!(app.screen, rustket::app::Screen::Settings(_)));
    let mut renderer = test_renderer();
    let size = iced::Size::new(1280.0, 800.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    let before = scrollables(&mut ui, &renderer);
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].1.y, 0.0);
    run_ops(&mut ui, &renderer, task);
    let after = scrollables(&mut ui, &renderer);
    let (bounds, t) = after[0];
    assert!(t.y > 0.0, "scrolled: {after:?}");
    let card = container_bounds(
        &mut ui,
        &renderer,
        iced::advanced::widget::Id::new(rustket::screens::settings::SIDEBAR_CARD),
    )
    .expect("the Sidebar card");
    let top = card.y - t.y;
    assert!(
        top >= bounds.y && top < bounds.y + bounds.height,
        "card {card:?} at {top} in {bounds:?}"
    );
}

/// "Ungroup…" scrolls to the group's row, where its deletion is confirmed
/// (a group of direct messages sits far below the card's top).
#[tokio::test]
async fn header_menu_ungroup_scrolls_to_the_group_row() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket_core::sidebar_layout::{GroupType, LayoutEdit};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(header(
        &sid,
        sidebar::HeaderAct::Layout(LayoutEdit::AddGroup {
            name: "People".into(),
            types: vec![GroupType::DirectMessages],
        }),
    ));
    let gid = app.config.ui.sidebar_groups[0].id.clone();
    let task = app.update(header(&sid, sidebar::HeaderAct::Ungroup(gid.clone())));
    let mut renderer = test_renderer();
    let size = iced::Size::new(1280.0, 800.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    run_ops(&mut ui, &renderer, task);
    let (bounds, t) = scrollables(&mut ui, &renderer)[0];
    let row = container_bounds(
        &mut ui,
        &renderer,
        iced::advanced::widget::Id::from(rustket::screens::settings::sidebar_group_row_id(&gid)),
    )
    .expect("the group's row");
    let top = row.y - t.y;
    assert!(
        top >= bounds.y && top + row.height <= bounds.y + bounds.height,
        "row {row:?} at {top} in {bounds:?}"
    );
}

/// "Keep in sidebar" from the message menu and the sidebar edit the
/// account's kept threads in the config (deduped, capped, saved) and the
/// workspace mirrors them.
#[tokio::test]
async fn kept_threads_are_deduped_capped_and_saved() {
    use rustket::room::timeline::{Act, ItemMsg, Message as Tm};
    use rustket_core::config::ThreadsMode;
    use rustket_core::sidebar_layout::MAX_KEPT_THREADS;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    app.config.ui.sidebar_threads = ThreadsMode::UnreadKept;
    let mut parent = rc_msg("p1", "r1", T0 + 10, "B");
    parent.tcount = Some(1);
    deliver(&mut app, &sid, parent);
    let act = |app: &App, a: Act| {
        room_msg(
            app,
            &sid,
            "r1",
            rustket::room::Message::Timeline(Tm::Item(ItemMsg::Act("p1".into(), a))),
        )
    };
    let kept = |app: &App| app.config.server(&sid).unwrap().kept_threads().count();
    let _ = app.update(act(&app, Act::KeepInSidebar));
    assert_eq!(kept(&app), 1);
    assert!(app.servers[&sid].workspace.thread_index().is_kept("p1"));
    // The same thread from the sidebar: no duplicate.
    let keep = |tmid: &str, act| {
        Message::Sidebar(
            sid.clone(),
            sidebar::Message::ThreadAct {
                rid: "r1".into(),
                tmid: tmid.into(),
                act,
            },
        )
    };
    let _ = app.update(keep("p1", sidebar::ThreadAct::Keep));
    assert_eq!(kept(&app), 1);
    app.flush_config();
    let disk = AppConfig::load(dir.path()).unwrap();
    assert_eq!(disk.servers[0].kept_threads().count(), 1);

    let _ = app.update(act(&app, Act::RemoveFromSidebar));
    assert_eq!(kept(&app), 0);
    assert!(!app.servers[&sid].workspace.thread_index().is_kept("p1"));

    for i in 0..MAX_KEPT_THREADS + 3 {
        let _ = app.update(keep(&format!("t{i}"), sidebar::ThreadAct::Keep));
    }
    assert_eq!(kept(&app), MAX_KEPT_THREADS);
    // The oldest are dropped first.
    let first = app.config.server(&sid).unwrap().kept_threads().next();
    assert_eq!(first.map(|k| k.tmid.as_str()), Some("t3"));
}
