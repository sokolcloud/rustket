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
use rustket_core::config::{AppConfig, IndicatorScheme, ServerEntry, TokenKind};
use rustket_core::secrets::{MemoryStore, SecretBackend, SecretStore, SwitchableStore};
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
        store: Arc::new(SwitchableStore::new(
            Box::new(store),
            SecretBackend::Memory,
            None,
        )),
        tokens,
        notices: vec![],
    }));
}

/// A default config under the Rustket indicator scheme: the process-wide
/// indicator look never changes in this binary.
fn rustket_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::Rustket;
    cfg
}

/// One configured server; logged in as `uid` when `uid` is set.
fn app_with(
    dir: &std::path::Path,
    uid: Option<&str>,
    urls: Vec<String>,
) -> (App, ServerId, Arc<MemoryStore>) {
    let mut cfg = rustket_cfg();
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
        indicator_scheme: Some(IndicatorScheme::Rustket),
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
    let mut cfg = rustket_cfg();
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
    let mut cfg = rustket_cfg();
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
    // (The cheat sheet opens over dialogs; Ctrl+K no longer does.)
    let _ = app.update(Message::Shortcut(rustket::keymap::Shortcut::CheatSheet));
    assert!(top_stamp(&app).is_none(), "covered by the cheat sheet");
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
    let _ = app.update(Message::Shortcut(rustket::keymap::Shortcut::QuickSwitcher));
    let _ = app.update(Message::Shortcut(rustket::keymap::Shortcut::QuickSwitcher));
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

/// Item 15: a plain @mention in a room above `Notifications_Max_Room_Members`
/// (100 by default) gets the client's own notification after the delay,
/// unless a server notification of the same message came, or the room's
/// last message is not the mention.
#[tokio::test]
async fn large_room_mentions_notify_once() {
    use rustket::desktop::notifications::{MockNotifier, Notifications};
    use rustket_core::notify::large_room;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let mock = MockNotifier::default();
    app.desktop.notifications = Notifications::with_backend(Arc::new(mock.clone()));
    let now = T0 + 600_000;
    let mention = |id: &str, rid: &str| rocketchat::models::Message {
        mentions: Some(vec![rocketchat::models::Mention {
            id: "A".into(),
            username: Some("alice".into()),
            ..Default::default()
        }]),
        ..rc_msg(id, rid, now, "B")
    };
    // r7: the last message is not the mention; r8: the server notifies
    // too; r9: only the fallback.
    for (rid, last) in [
        ("r7", rc_msg("m7", "r7", now, "B")),
        ("r8", mention("m8", "r8")),
        ("r9", mention("m9", "r9")),
    ] {
        let e = ev(
            &app,
            &sid,
            SessionEvent::RoomChanged {
                kind: ChangeKind::Inserted,
                room: Box::new(Room {
                    users_count: Some(150),
                    last_message: Some(Box::new(last)),
                    ..room(rid, now)
                }),
            },
        );
        let _ = app.update(e);
        let e = ev(
            &app,
            &sid,
            SessionEvent::SubscriptionChanged {
                kind: ChangeKind::Inserted,
                sub: Box::new(sub(rid, "A", 0, T0)),
            },
        );
        let _ = app.update(e);
        // The update carries the `_updatedAt` read before counting.
        let counted = Sub {
            user_mentions: 1,
            ..sub(rid, "A", 1, T0)
        };
        let e = ev(
            &app,
            &sid,
            SessionEvent::SubscriptionChanged {
                kind: ChangeKind::Updated,
                sub: Box::new(counted),
            },
        );
        let _ = app.update(e);
    }
    let server: rocketchat::session::DesktopNotification =
        serde_json::from_value(serde_json::json!({
            "title": "#r8", "text": "B: text m8",
            "payload": {"_id": "m8", "rid": "r8", "type": "c",
                "sender": {"_id": "B", "username": "b"}, "message": {"msg": "text m8"}}
        }))
        .unwrap();
    let e = ev(&app, &sid, SessionEvent::Notification(Box::new(server)));
    let _ = app.update(e);
    assert_eq!(mock.shown.lock().unwrap().len(), 1);
    // Nothing before the delay.
    let _ = app.update(Message::LargeRoomDue);
    assert_eq!(mock.shown.lock().unwrap().len(), 1);
    tokio::time::sleep(large_room::DELAY + std::time::Duration::from_millis(100)).await;
    let _ = app.update(Message::LargeRoomDue);
    let shown = mock.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert_eq!(shown[1].title, "#r9");
    assert_eq!(shown[1].body, "b: text m9");
    // Checked once.
    let _ = app.update(Message::LargeRoomDue);
    assert_eq!(mock.shown.lock().unwrap().len(), 2);
    // A server notification of m9 arriving after the fallback: not again.
    let late: rocketchat::session::DesktopNotification =
        serde_json::from_value(serde_json::json!({
            "title": "#r9", "text": "B: text m9",
            "payload": {"_id": "m9", "rid": "r9", "type": "c",
                "sender": {"_id": "B", "username": "b"}, "message": {"msg": "text m9"}}
        }))
        .unwrap();
    let e = ev(&app, &sid, SessionEvent::Notification(Box::new(late)));
    let _ = app.update(e);
    assert_eq!(mock.shown.lock().unwrap().len(), 2);
}

/// Q4: the server sends a thread reply's notification before the reply.
/// For the open room it is decided once the reply arrived: a reply also
/// sent to the channel (`tshow`) is on screen, a thread-only one notifies.
#[tokio::test]
async fn thread_reply_notification_waits_for_the_reply() {
    use rustket::desktop::notifications::{MockNotifier, Notifications};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let mock = MockNotifier::default();
    app.desktop.notifications = Notifications::with_backend(Arc::new(mock.clone()));
    app.focused = true;
    let now = T0 + 600_000;
    let notification = |mid: &str| -> rocketchat::session::DesktopNotification {
        serde_json::from_value(serde_json::json!({
            "title": "#r1", "text": "b: reply",
            "payload": {"_id": mid, "rid": "r1", "tmid": "t1", "type": "c",
                "sender": {"_id": "B", "username": "b"}, "message": {"msg": "reply"}}
        }))
        .unwrap()
    };
    let reply = |mid: &str, tshow: bool| rocketchat::models::Message {
        tmid: Some("t1".into()),
        tshow: tshow.then_some(true),
        ..rc_msg(mid, "r1", now, "B")
    };
    for (mid, tshow) in [("m1", true), ("m2", false)] {
        // Notification first, as the server sends it: held.
        let n = notification(mid);
        let e = ev(&app, &sid, SessionEvent::Notification(Box::new(n.clone())));
        let _ = app.update(e);
        assert_eq!(mock.shown.lock().unwrap().len(), 0, "{mid}: held");
        // Then the room's lastMessage and the message itself.
        let e = ev(
            &app,
            &sid,
            SessionEvent::RoomChanged {
                kind: ChangeKind::Updated,
                room: Box::new(Room {
                    last_message: Some(Box::new(reply(mid, tshow))),
                    ..room("r1", now)
                }),
            },
        );
        let _ = app.update(e);
        deliver(&mut app, &sid, reply(mid, tshow));
        let _ = app.update(Message::NotificationRecheck(sid.clone(), Box::new(n)));
    }
    let shown = mock.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1, "only the thread-only reply: {shown:?}");
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
    let mut cfg = rustket_cfg();
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
        rustket_ui::theme::font_regular(),
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

/// "Sidebar settings…" opens the settings on the Sidebar tab with its first
/// card in view.
#[tokio::test]
async fn header_menu_settings_opens_the_sidebar_tab() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket::screens::settings::Tab;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let task = app.update(header(&sid, sidebar::HeaderAct::OpenSettings));
    let rustket::app::Screen::Settings(s) = &app.screen else {
        panic!("settings not open");
    };
    assert_eq!(s.tab, Tab::Sidebar);
    let mut renderer = test_renderer();
    let size = iced::Size::new(1280.0, 800.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    // Start away from the card so the reveal task has to scroll back.
    run_ops(
        &mut ui,
        &renderer,
        iced::widget::operation::snap_to_end(rustket::screens::page::SCROLL_ID),
    );
    assert!(scrollables(&mut ui, &renderer)[0].1.y > 0.0);
    run_ops(&mut ui, &renderer, task);
    let (bounds, t) = scrollables(&mut ui, &renderer)[0];
    let card = container_bounds(
        &mut ui,
        &renderer,
        iced::advanced::widget::Id::new(rustket::screens::settings::SIDEBAR_CARD),
    )
    .expect("the Sidebar card");
    let top = card.y - t.y;
    // The scroll offset is snapped to whole pixels; the card's top may not be.
    assert!(
        top >= bounds.y - 0.5 && top < bounds.y + bounds.height,
        "card {card:?} at {top} in {bounds:?}"
    );
}

/// In a small window (the minimum one at 150% zoom) the wrapped tabs scroll
/// with the content instead of leaving it no height.
#[tokio::test]
async fn settings_tabs_leave_room_for_content_in_a_small_window() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(header(&sid, sidebar::HeaderAct::OpenSettings));
    let mut renderer = test_renderer();
    let size = iced::Size::new(320.0, 267.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    let (bounds, _) = scrollables(&mut ui, &renderer)[0];
    assert!(bounds.height > 100.0, "page scroll {bounds:?}");
}

/// The settings reopen on the tab shown last; sidebar deep links open the
/// Sidebar tab.
#[tokio::test]
async fn settings_remember_the_last_tab() {
    use rustket::app::Screen;
    use rustket::screens::settings::{Message as Sm, Tab};
    use rustket::workspace::rail;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let tab = |app: &App| match &app.screen {
        Screen::Settings(s) => Some(s.tab),
        _ => None,
    };
    let _ = app.update(Message::Rail(rail::Message::Settings));
    assert_eq!(tab(&app), Some(Tab::General));
    let _ = app.update(Message::Settings(Sm::Tab(Tab::Appearance)));
    let _ = app.update(Message::Settings(Sm::Close));
    assert_eq!(tab(&app), None);
    let _ = app.update(Message::Rail(rail::Message::Settings));
    assert_eq!(tab(&app), Some(Tab::Appearance));
    let _ = app.update(Message::Settings(Sm::Close));
    for act in [
        sidebar::HeaderAct::OpenSettings,
        sidebar::HeaderAct::RenameFlag(rustket_core::flags::FlagColor::Red),
    ] {
        let _ = app.update(Message::Rail(rail::Message::Settings));
        let _ = app.update(Message::Settings(Sm::Tab(Tab::About)));
        let _ = app.update(Message::Settings(Sm::Close));
        let _ = app.update(header(&sid, act));
        assert_eq!(tab(&app), Some(Tab::Sidebar));
        let _ = app.update(Message::Settings(Sm::Close));
    }
    let _ = app.update(Message::Rail(rail::Message::Settings));
    assert_eq!(
        tab(&app),
        Some(Tab::Sidebar),
        "the deep link's tab is remembered"
    );
    // The status menu's Help item opens the Help tab, built for the account.
    let _ = app.update(Message::Settings(Sm::Close));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Help),
    ));
    assert_eq!(tab(&app), Some(Tab::Help));
    let texts = view_texts(&app);
    assert!(
        texts.iter().any(|t| t.starts_with("Computed for ")),
        "{texts:?}"
    );
    assert!(!texts.iter().any(|t| t.contains("No account is signed in")));
}

/// The texts of the app's view laid out at 1280x800.
fn view_texts(app: &App) -> Vec<String> {
    use iced::advanced::widget::{Id, Operation};
    use iced_runtime::user_interface::{Cache, UserInterface};
    struct Texts(Vec<String>);
    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }
    let mut renderer = test_renderer();
    let mut ui = UserInterface::build(
        app.view(),
        iced::Size::new(1280.0, 800.0),
        Cache::default(),
        &mut renderer,
    );
    let mut texts = Texts(Vec::new());
    ui.operate(&renderer, &mut texts);
    texts.0
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

/// "Add thread to Favorites" stars the parent through the server: nothing
/// happens without a session, and a failed call takes the optimistic star
/// back.
#[tokio::test]
async fn failed_thread_star_is_reverted() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let idx = |app: &App| app.servers[&sid].workspace.thread_index().is_starred("t1");
    assert!(app.servers[&sid].workspace.thread_index().pins_enabled());
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::ThreadAct {
            rid: "r1".into(),
            tmid: "t1".into(),
            act: sidebar::ThreadAct::Star,
        },
    ));
    assert!(!idx(&app), "no session: not applied");
    // As applied when the call was made.
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .set_thread_starred("r1", "t1", true);
    let _ = app.update(Message::ThreadStarred {
        sid: sid.clone(),
        epoch: app.servers[&sid].generation,
        rid: "r1".into(),
        tmid: "t1".into(),
        on: true,
        queued: false,
        result: Err(Failure::from(rocketchat::Error::Timeout(
            "chat.starMessage",
        ))),
    });
    assert!(!idx(&app), "reverted");
    // Off in the settings: nothing is tracked.
    app.config.ui.sidebar_starred_threads = false;
    let _ = app.update(Message::Noop);
    assert!(!app.servers[&sid].workspace.thread_index().pins_enabled());
}

/// Failures of "Move kept threads to Favorites" are reverted silently and
/// reported once the queue is done; rate-limited ones are tried again.
#[tokio::test]
async fn queued_star_failures_are_reported_together() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    app.config.ui.sidebar_starred_threads = true;
    let _ = app.update(Message::Noop);
    let failed = |app: &mut App, e: rocketchat::Error| {
        app.servers
            .get_mut(&sid)
            .unwrap()
            .workspace
            .set_thread_starred("r1", "t1", true);
        let _ = app.update(Message::ThreadStarred {
            sid: sid.clone(),
            epoch: app.servers[&sid].generation,
            rid: "r1".into(),
            tmid: "t1".into(),
            on: true,
            queued: true,
            result: Err(Failure::from(e)),
        });
        assert!(!app.servers[&sid].workspace.thread_index().is_starred("t1"));
    };
    failed(
        &mut app,
        rocketchat::Error::RateLimited {
            retry_after: Some(std::time::Duration::ZERO),
            message: None,
        },
    );
    failed(&mut app, rocketchat::Error::Timeout("chat.starMessage"));
    assert!(app.toasts.is_empty());
    // The retried one has no session to go out on: skipped. Then one
    // message for the failure.
    let _ = app.update(Message::StarKeptNext);
    assert_eq!(app.toasts.len(), 1, "{:?}", app.toasts);
    assert!(app.toasts[0].title.contains('1'), "{:?}", app.toasts[0]);
}

#[tokio::test]
async fn flag_edits_persist_until_the_service_room_shows_them() {
    use rocketchat::models::{FLAGS_KEY, ServiceFields};
    use rustket_core::flags::{FlagColor, FlagEdit};
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let edit = FlagEdit::Room {
        rid: "r1".into(),
        color: Some(FlagColor::Red),
    };
    let _ = app.update(Message::SetFlag(sid.clone(), edit));
    assert_eq!(
        app.servers[&sid].workspace.room_flag("r1"),
        Some(FlagColor::Red),
        "shown at once"
    );
    assert!(app.config.device_id.is_some());
    let pending = app.config.server(&sid).unwrap().pending_flags("A");
    assert_eq!(pending.view().room("r1"), Some(FlagColor::Red));
    // A restart (same config) shows the pending edit before any sync.
    app.flush_config();
    let saved = AppConfig::load(dir.path()).unwrap();
    assert_eq!(
        saved.servers[0].pending_flags("A").view().room("r1"),
        Some(FlagColor::Red)
    );
    // The service room's subscription echo stores it: the overlay goes.
    let mut fields = ServiceFields::new("A");
    fields.set(FLAGS_KEY, pending.to_value());
    let svc = Sub {
        t: RoomType::Private,
        open: false,
        roles: Some(vec!["owner".into()]),
        custom_fields: Some(fields.to_custom_fields()),
        ts: Some(ts(T0)),
        ..sub("svc", "A", 0, T0)
    };
    let m = ev(
        &app,
        &sid,
        SessionEvent::SubscriptionChanged {
            kind: ChangeKind::Inserted,
            sub: Box::new(svc),
        },
    );
    let _ = app.update(m);
    let svc_room = Room {
        t: RoomType::Private,
        u: Some(UserRef {
            id: "A".into(),
            username: Some("alice".into()),
            name: None,
        }),
        users_count: Some(1),
        custom_fields: Some(fields.to_custom_fields()),
        ts: Some(ts(T0)),
        ..room("svc", T0)
    };
    let m = ev(
        &app,
        &sid,
        SessionEvent::RoomChanged {
            kind: ChangeKind::Inserted,
            room: Box::new(svc_room),
        },
    );
    let _ = app.update(m);
    let s = &app.servers[&sid];
    assert!(s.flags.overlay().is_empty());
    assert_eq!(s.workspace.room_flag("r1"), Some(FlagColor::Red));
    assert!(
        app.config
            .server(&sid)
            .unwrap()
            .pending_flags("A")
            .is_empty()
    );
    assert_eq!(s.workspace.service_rooms(), ["svc".to_owned()]);
}

/// Sidebar flag items go through the flag sync (shown at once); "Rename
/// flag…" opens the settings at that flag's name field, focused, and the
/// name typed there renames the section; "Add to favorites" needs a session.
#[tokio::test]
async fn sidebar_flags_and_rename_through_settings() {
    use iced_runtime::user_interface::{Cache, UserInterface};
    use rustket_core::flags::FlagColor;
    use rustket_core::sidebar_layout::SectionId;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::RoomAct {
            rid: "r1".into(),
            act: sidebar::RoomAct::Flag(Some(FlagColor::Purple)),
        },
    ));
    assert_eq!(
        app.servers[&sid].workspace.room_flag("r1"),
        Some(FlagColor::Purple)
    );
    assert!(
        app.servers[&sid]
            .sidebar
            .sections
            .iter()
            .any(|s| s.id == SectionId::Flag(FlagColor::Purple))
    );
    // No session in this app: nothing is called, nothing breaks.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::RoomAct {
            rid: "r1".into(),
            act: sidebar::RoomAct::Favorite(true),
        },
    ));

    let task = app.update(header(
        &sid,
        sidebar::HeaderAct::RenameFlag(FlagColor::Purple),
    ));
    assert!(matches!(app.screen, rustket::app::Screen::Settings(_)));
    let mut renderer = test_renderer();
    let size = iced::Size::new(1280.0, 800.0);
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    run_ops(&mut ui, &renderer, task);
    let (bounds, t) = scrollables(&mut ui, &renderer)[0];
    let row = container_bounds(
        &mut ui,
        &renderer,
        iced::advanced::widget::Id::from(rustket::screens::settings::flag_row_id(
            &sid,
            FlagColor::Purple,
        )),
    )
    .expect("the flag's row");
    let top = row.y - t.y;
    assert!(
        top >= bounds.y && top + row.height <= bounds.y + bounds.height,
        "row {row:?} at {top} in {bounds:?}"
    );
    let focus = focused(&mut ui, &renderer);
    let name_id = iced::advanced::widget::Id::from(rustket::screens::settings::flag_name_id(
        &sid,
        FlagColor::Purple,
    ));
    assert!(
        focus.iter().any(|(id, _)| id.as_ref() == Some(&name_id)),
        "{focus:?}"
    );
    drop(ui);
    let settings = |m: rustket::screens::settings::SidebarMessage| {
        Message::Settings(rustket::screens::settings::Message::Sidebar(m))
    };
    let _ = app.update(settings(
        rustket::screens::settings::SidebarMessage::FlagName(
            sid.clone(),
            FlagColor::Purple,
            "Waiting".into(),
        ),
    ));
    let _ = app.update(settings(
        rustket::screens::settings::SidebarMessage::FlagNameSave(sid.clone(), FlagColor::Purple),
    ));
    let ws = &app.servers[&sid].workspace;
    assert_eq!(ws.flags().name(FlagColor::Purple), Some("Waiting"));
    let pending = app.config.server(&sid).unwrap().pending_flags("A");
    assert_eq!(pending.view().name(FlagColor::Purple), Some("Waiting"));

    // A draft that reads like the stored name is dropped.
    use rustket::screens::settings::SidebarMessage;
    let drafts = |app: &App| match &app.screen {
        rustket::app::Screen::Settings(s) => s.flag_drafts.len(),
        _ => usize::MAX,
    };
    let _ = app.update(settings(SidebarMessage::FlagName(
        sid.clone(),
        FlagColor::Purple,
        "Waiting ".into(),
    )));
    assert_eq!(drafts(&app), 1);
    let _ = app.update(settings(SidebarMessage::FlagName(
        sid.clone(),
        FlagColor::Purple,
        "Waiting".into(),
    )));
    assert_eq!(drafts(&app), 0);
    // Leaving the settings saves a name typed without Enter.
    let _ = app.update(settings(SidebarMessage::FlagName(
        sid.clone(),
        FlagColor::Blue,
        "Later".into(),
    )));
    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));
    assert!(!matches!(app.screen, rustket::app::Screen::Settings(_)));
    let ws = &app.servers[&sid].workspace;
    assert_eq!(ws.flags().name(FlagColor::Blue), Some("Later"));
}

/// Runs `task` and feeds back the outputs `keep` accepts.
async fn feed(app: &mut App, task: iced::Task<Message>, keep: fn(&Message) -> bool) {
    use iced::futures::StreamExt;
    let Some(stream) = iced_runtime::task::into_stream(task) else {
        return;
    };
    let outputs: Vec<Message> = stream
        .filter_map(|a| async move {
            match a {
                iced_runtime::Action::Output(m) if keep(&m) => Some(m),
                _ => None,
            }
        })
        .collect()
        .await;
    for m in outputs {
        let _ = app.update(m);
    }
}

/// One signed-in server whose secrets are in a plain file in `dir/data`.
fn app_on_file(dir: &std::path::Path) -> (App, ServerId) {
    use rustket_core::secrets::FileStore;
    let mut cfg = rustket_cfg();
    let mut entry = ServerEntry::new(SERVER, "Local");
    entry.user_id = Some("A".into());
    let sid = cfg.add_server(entry);
    cfg.save(dir).unwrap();
    let (mut app, _t) = App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        secrets: SecretsMode::Memory,
        ..Flags::default()
    });
    let data = dir.join("data");
    let file = FileStore::new(&data);
    file.set(&rustket_core::secrets::secret_key(&sid, "A"), "tokA")
        .unwrap();
    file.set(
        &rustket_core::e2e::pending_passphrase_secret(&sid, "A"),
        "generated",
    )
    .unwrap();
    let _ = app.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(file),
            SecretBackend::File,
            Some(data),
        )),
        tokens: vec![(sid.clone(), Some("tokA".into()))],
        notices: vec![],
    }));
    (app, sid)
}

/// Turning "never store secrets in a file" on asks first (warning about the
/// unsaved E2E password), then moves the secrets into memory and deletes the
/// file; turning it off writes them back without asking.
#[tokio::test]
async fn memory_only_switch_confirms_then_moves_the_secrets() {
    use rustket::screens::settings::Message as Sm;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = app_on_file(dir.path());
    let file = dir.path().join("data").join("secrets.json");
    assert!(file.exists());
    let _ = app.update(Message::Rail(rustket::workspace::rail::Message::Settings));
    let task = app.update(Message::Settings(Sm::SetSecretsMemoryOnly(true)));
    assert!(matches!(
        app.modals.last(),
        Some(Modal::SecretsMemoryOnly { .. })
    ));
    assert!(!app.config.ui.secrets_memory_only, "not before confirming");
    // The stored, unconfirmed E2E password is found.
    feed(&mut app, task, |m| {
        matches!(m, Message::SecretsPendingFound(_))
    })
    .await;
    let Some(Modal::SecretsMemoryOnly { pending }) = app.modals.last() else {
        panic!("modal closed");
    };
    assert_eq!(pending, std::slice::from_ref(&sid));
    // Cancel keeps everything.
    let _ = app.update(Message::CloseModal);
    assert!(app.modals.is_empty());
    assert_eq!(app.secret_backend, Some(SecretBackend::File));
    // Confirm.
    let _ = app.update(Message::Settings(Sm::SetSecretsMemoryOnly(true)));
    let task = app.update(Message::Modal(ModalMessage::ConfirmSecretsMemoryOnly));
    assert!(app.modals.is_empty());
    assert!(app.config.ui.secrets_memory_only);
    feed(&mut app, task, |m| {
        matches!(m, Message::SecretsPolicyApplied(..))
    })
    .await;
    assert_eq!(app.secret_backend, Some(SecretBackend::MemoryByPolicy));
    assert!(!file.exists(), "the plaintext file is gone");
    let store = app.secrets.clone().unwrap();
    let key = rustket_core::secrets::secret_key(&sid, "A");
    assert_eq!(store.get(&key).unwrap().as_deref(), Some("tokA"));
    // Off: no question, back into the file.
    let task = app.update(Message::Settings(Sm::SetSecretsMemoryOnly(false)));
    assert!(app.modals.is_empty());
    assert!(!app.config.ui.secrets_memory_only);
    // While that runs, turning it on again opens no modal (its confirmation
    // would be dropped).
    let _ = app.update(Message::Settings(Sm::SetSecretsMemoryOnly(true)));
    assert!(app.modals.is_empty());
    feed(&mut app, task, |m| {
        matches!(m, Message::SecretsPolicyApplied(..))
    })
    .await;
    assert_eq!(app.secret_backend, Some(SecretBackend::File));
    assert!(std::fs::read_to_string(&file).unwrap().contains("tokA"));
}

/// The startup warning about the plain file links to the secret store card.
#[tokio::test]
async fn file_fallback_toast_opens_the_privacy_tab() {
    use rustket::screens::settings::Tab;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sid) = app_on_file(dir.path());
    let toast = app
        .toasts
        .iter()
        .find(|t| t.action.is_some())
        .expect("a toast with an action")
        .clone();
    assert!(toast.body.contains("E2E"), "{}", toast.body);
    let (_, action) = toast.action.unwrap();
    let _ = app.update(action);
    let rustket::app::Screen::Settings(s) = &app.screen else {
        panic!("settings not open");
    };
    assert_eq!(s.tab, Tab::Privacy);
    assert!(app.toasts.iter().all(|t| t.id != toast.id), "toast closed");
}

// --- keyboard shortcuts --------------------------------------------------------------

/// Esc on the server view: the right panel closes first; then an unread
/// room is marked read with its threads (unless the setting is off); a view
/// in place of the room area (the contact center) closes instead, and the
/// room it covers is not marked read.
#[test]
fn escape_chain_on_the_server_view() {
    use rustket::room::panel::{Kind, Message as Pm};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let esc = |app: &mut App| outputs_within(app.update(Message::Escape), 300);
    let marks_all = |outs: &[Message]| {
        outs.iter().any(|m| {
            matches!(m, Message::Room(_, _, r, rustket::room::Message::MarkAllRead) if r == "r1")
        })
    };
    assert!(!marks_all(&esc(&mut app)), "nothing unread");
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 2, T0 + 11);
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("p1".into()))),
    ));
    let outs = esc(&mut app);
    assert!(!marks_all(&outs));
    assert!(outs.iter().any(|m| matches!(
        m,
        Message::Room(_, _, _, rustket::room::Message::Panel(Pm::Close))
    )));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Close),
    ));
    assert!(marks_all(&esc(&mut app)));
    app.config.ui.escape_marks_read = false;
    assert!(!marks_all(&esc(&mut app)), "switched off in Settings");
    app.config.ui.escape_marks_read = true;
    app.servers
        .get_mut(&sid)
        .unwrap()
        .omni
        .open_contact_center();
    assert!(app.servers[&sid].room_area_taken());
    let outs = esc(&mut app);
    assert!(!marks_all(&outs), "the covered room was marked read");
    assert!(outs.iter().any(|m| matches!(
        m,
        Message::Omni(
            _,
            _,
            rustket::omnichannel::Message::ContactCenter(
                rustket::omnichannel::contact_center::Msg::Close
            )
        )
    )));
    // Ctrl+Shift+\ does not reach the covered room either.
    let outs = outputs_within(
        app.update(Message::Shortcut(rustket::keymap::Shortcut::ReactLast)),
        300,
    );
    assert!(!outs.iter().any(|m| matches!(m, Message::Room(..))));
}

/// Esc with a message selection and a panel open: the selection is
/// cleared first (it is what the user just did), then the panel closes.
#[test]
fn escape_clears_the_selection_before_the_panel() {
    use rustket::room::Message as Rm;
    use rustket::room::panel::{Kind, Message as Pm};
    use rustket::room::timeline::{Act, ItemMsg, Message as Tm};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Timeline(Tm::Item(ItemMsg::Act("p1".into(), Act::Select))),
    ));
    assert!(app.servers[&sid].rooms["r1"].picking());
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Panel(Pm::Open(Kind::Thread("p1".into()))),
    ));
    let outs = outputs_within(app.update(Message::Escape), 300);
    assert!(
        outs.iter()
            .any(|m| matches!(m, Message::Room(_, _, _, Rm::Timeline(Tm::ClearPicks))))
    );
    assert!(
        !outs
            .iter()
            .any(|m| matches!(m, Message::Room(_, _, _, Rm::Panel(Pm::Close))))
    );
    let _ = app.update(room_msg(&app, &sid, "r1", Rm::Timeline(Tm::ClearPicks)));
    assert!(!app.servers[&sid].rooms["r1"].picking());
    let outs = outputs_within(app.update(Message::Escape), 300);
    assert!(
        outs.iter()
            .any(|m| matches!(m, Message::Room(_, _, _, Rm::Panel(Pm::Close))))
    );
}

/// Forward from a room: the quick switcher opens in forward mode; picking a
/// room opens it with the message quoted as forwarded in its composer.
#[test]
fn forward_opens_the_target_with_the_quote() {
    use rustket::room::Message as Rm;
    use rustket::room::timeline::{Act, ItemMsg, Message as Tm};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 10, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Timeline(Tm::Item(ItemMsg::Act("p1".into(), Act::Forward))),
    ));
    assert!(matches!(
        app.modals.last(),
        Some(rustket::app::Modal::QuickSwitcher(..))
    ));
    let target = "r2".to_owned();
    let _ = app.update(Message::QuickSwitcher(
        sid.clone(),
        rustket::dialogs::quick_switcher::Message::Query(String::new()),
    ));
    let items = rustket::dialogs::quick_switcher::results_for(
        &app.servers[&sid].workspace,
        &rustket_core::room_list::RoomListOptions::default(),
        "",
        None,
        true,
    );
    let i = items
        .iter()
        .position(|it| matches!(it, rustket::dialogs::quick_switcher::Item::Local { rid, .. } if *rid == target))
        .expect("the target is offered");
    let _ = app.update(Message::QuickSwitcher(
        sid.clone(),
        rustket::dialogs::quick_switcher::Message::Select(i),
    ));
    let _ = app.update(Message::QuickSwitcher(
        sid.clone(),
        rustket::dialogs::quick_switcher::Message::Submit,
    ));
    assert!(app.modals.is_empty(), "the switcher closed");
    let s = &app.servers[&sid];
    assert_eq!(s.active_room.as_deref(), Some(target.as_str()));
    assert_eq!(s.rooms[&target].composer.quoted(), ["p1"]);
}

/// Ctrl+/ toggles the cheat sheet (also from the status menu and over the
/// preferences), Ctrl+, opens the preferences, Ctrl+Shift+\ goes to the open
/// room's composer, Shift+Esc needs a connection.
#[tokio::test]
async fn app_wide_shortcuts() {
    use rustket::app::Screen;
    use rustket::keymap::Shortcut;
    use rustket::workspace::status_menu;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(Message::Shortcut(Shortcut::CheatSheet));
    assert!(matches!(app.modals.last(), Some(Modal::Shortcuts)));
    let _ = app.view();
    let _ = app.update(Message::Shortcut(Shortcut::CheatSheet));
    assert!(app.modals.is_empty());
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(status_menu::Message::Shortcuts),
    ));
    assert!(matches!(app.modals.last(), Some(Modal::Shortcuts)));
    let _ = app.update(Message::CloseModal);
    assert!(app.modals.is_empty());

    let _ = app.update(Message::Shortcut(Shortcut::Settings));
    assert!(matches!(app.screen, Screen::Settings(_)));
    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::ShowShortcuts,
    ));
    assert!(matches!(app.modals.last(), Some(Modal::Shortcuts)));
    let _ = app.view();
    let _ = app.update(Message::CloseModal);
    let _ = app.update(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));
    assert!(matches!(app.screen, Screen::Server));

    set_unread(&mut app, &sid, "r2", 1, T0 + 5);
    let toasts = app.toasts.len();
    let _ = app.update(Message::Shortcut(Shortcut::MarkAllRead));
    assert!(app.modals.is_empty(), "no dialog without a connection");
    assert_eq!(app.toasts.len(), toasts + 1);
}

/// Chords the listener hears while a dialog has the keyboard leave it
/// alone: Ctrl+K and Ctrl+, do nothing over it. Shift+Esc in the focused
/// composer (the listener skips captured Shift+Esc) still asks.
#[tokio::test]
async fn shortcuts_behind_a_dialog() {
    use rustket::app::Screen;
    use rustket::keymap::Shortcut;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Create));
    assert!(matches!(app.modals.last(), Some(Modal::CreateRoom(..))));
    let _ = app.update(Message::Shortcut(Shortcut::QuickSwitcher));
    assert_eq!(app.modals.len(), 1, "no quick switcher over the dialog");
    let _ = app.update(Message::Shortcut(Shortcut::Settings));
    assert!(matches!(app.screen, Screen::Server));
    let toasts = app.toasts.len();
    let _ = app.update(Message::Shortcut(Shortcut::MarkAllRead));
    assert_eq!(app.modals.len(), 1);
    assert_eq!(app.toasts.len(), toasts);
    let _ = app.update(Message::CloseModal);
    assert!(app.modals.is_empty());

    // From the composer: the same path as the listener (offline here).
    set_unread(&mut app, &sid, "r2", 1, T0 + 5);
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Composer(rustket::room::composer::Message::AskReadAll),
    ));
    assert_eq!(app.toasts.len(), toasts + 1, "not connected");
}

/// "Mark all as read": a running dialog cannot be dismissed, a failed run
/// closes it with an error, and it closes when its server's queue is gone.
#[tokio::test]
async fn mark_all_read_dialog() {
    use rustket::reads::Origin;
    use rustket_core::reads::ReadReq;
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let dialog = |stamp, busy| Modal::ReadAll {
        server_id: sid.clone(),
        rids: vec!["r1".into(), "r2".into()],
        busy,
        done: 0,
        failed: 0,
        stamp,
    };
    // Confirmed without a session: both rooms fail at once.
    app.modals.push(dialog(7, false));
    let _ = app.view();
    let toasts = app.toasts.len();
    let _ = app.update(Message::Modal(ModalMessage::ConfirmReadAll));
    assert!(app.modals.is_empty());
    assert_eq!(app.toasts.len(), toasts + 1);
    assert!(matches!(
        app.toasts.last().unwrap().kind,
        rustket_ui::widget::BannerKind::Error
    ));

    // Running: Esc / Cancel do nothing.
    app.servers.get_mut(&sid).unwrap().reads.push(
        ReadReq::Room {
            rid: "r1".into(),
            threads: true,
        },
        Origin::ReadAll(8),
    );
    app.modals.push(dialog(8, true));
    let _ = app.view();
    let _ = app.update(Message::CloseModal);
    assert!(matches!(app.modals.last(), Some(Modal::ReadAll { .. })));
    // Signed out meanwhile (queue cleared): the dialog goes.
    app.servers.get_mut(&sid).unwrap().reads.clear();
    let _ = app.update(Message::Noop);
    assert!(app.modals.is_empty());
}

/// All unreads and Activity replace the room area like the contact center:
/// their shortcuts toggle them, the sidebar rows switch between them, the
/// covered room is not marked read, Esc closes them without marking the
/// room, and opening a room closes them.
#[test]
fn inbox_views_cover_the_room_area() {
    use rustket::inbox::{CatchUpKey, InboxKind};
    use rustket::keymap::Shortcut;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let kind = |app: &App| app.servers[&sid].inbox_view().map(|i| i.kind());
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::MarkedRead(Ok(())),
    ));
    let _ = app.update(Message::Shortcut(Shortcut::AllUnreads));
    assert_eq!(kind(&app), Some(InboxKind::Unreads));
    assert!(app.servers[&sid].room_area_taken());
    let _ = app.view();
    // The covered room is not read when messages arrive.
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    deliver(&mut app, &sid, rc_msg("m2", "r2", T0 + 12, "B"));
    set_unread(&mut app, &sid, "r2", 1, T0 + 13);
    {
        let s = &app.servers[&sid];
        assert!(s.rooms["r1"].wants_auto_read(&s.workspace));
    }
    // A feed with a message renders in every view.
    {
        let s = app.servers.get_mut(&sid).unwrap();
        let req = s.workspace.begin_feed("r2", false).unwrap();
        s.workspace.finish_feed(
            &req,
            Ok(rocketchat::models::History {
                messages: vec![rc_msg("m2", "r2", T0 + 12, "B")],
                raw_len: 1,
                ..Default::default()
            }),
        );
    }
    let _ = app.view();
    // The shortcut again closes it; Activity, then the sidebar's row.
    let _ = app.update(Message::Shortcut(Shortcut::AllUnreads));
    assert_eq!(kind(&app), None);
    let _ = app.update(Message::Shortcut(Shortcut::Activity));
    assert_eq!(kind(&app), Some(InboxKind::Activity));
    let _ = app.view();
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Inbox(InboxKind::Unreads),
    ));
    assert_eq!(kind(&app), Some(InboxKind::Unreads));
    // Catch up from the Unread section's menu; its keys walk the rooms.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Header(sidebar::HeaderAct::CatchUp),
    ));
    assert_eq!(kind(&app), Some(InboxKind::CatchUp));
    let _ = app.view();
    let _ = app.update(Message::CatchUpKey(CatchUpKey::Right));
    let _ = app.update(Message::CatchUpKey(CatchUpKey::Right));
    let _ = app.view();
    // Esc closes the view and leaves the covered room alone.
    let outs = outputs_within(app.update(Message::Escape), 300);
    assert_eq!(kind(&app), None);
    assert!(!outs.iter().any(|m| matches!(
        m,
        Message::Room(_, _, _, rustket::room::Message::MarkAllRead)
    )));
    // Opening a room (or the contact center) closes it.
    let _ = app.update(Message::Shortcut(Shortcut::AllUnreads));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    assert_eq!(kind(&app), None);
    let _ = app.update(Message::Shortcut(Shortcut::Activity));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    assert_eq!(kind(&app), None, "the open room closes it too");
    // Hidden rows: the shortcuts still work.
    app.config.ui.sidebar_pinned_views = false;
    let _ = app.view();
    let _ = app.update(Message::Shortcut(Shortcut::Activity));
    assert_eq!(kind(&app), Some(InboxKind::Activity));
}

/// Esc with an inbox view over a room whose right panel is open closes
/// only the view; the catch-up keys do nothing outside catch-up or under a
/// modal; a view reopened as another kind starts its loads at once.
#[test]
fn inbox_esc_order_keys_and_reopen() {
    use rustket::inbox::{CatchUpKey, InboxKind};
    use rustket::keymap::Shortcut;
    use rustket::room::panel::{Kind, Message as Pm};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let kind = |app: &App| app.servers[&sid].inbox_view().map(|i| i.kind());
    deliver(&mut app, &sid, rc_msg("m2", "r2", T0 + 12, "B"));
    set_unread(&mut app, &sid, "r2", 1, T0 + 13);
    deliver(&mut app, &sid, rc_msg("p1", "r1", T0 + 14, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        rustket::room::Message::Panel(Pm::Open(Kind::Thread("p1".into()))),
    ));
    assert!(app.servers[&sid].rooms["r1"].panel.kind().is_some());

    // Activity's loader settles (its inputs recorded); Esc closes it and
    // leaves the panel. A view opened next starts at the same revision, so
    // the loader must not take the old inputs as current. (No session
    // here: the key stands for a settled loader.)
    let settled = Some([1, 1, 1, 1, 1]);
    let _ = app.update(Message::Shortcut(Shortcut::Activity));
    assert_eq!(kind(&app), Some(InboxKind::Activity));
    app.servers.get_mut(&sid).unwrap().inbox.plan_key = settled;
    let outs = outputs_within(app.update(Message::Escape), 300);
    assert_eq!(kind(&app), None);
    assert!(app.servers[&sid].rooms["r1"].panel.kind().is_some());
    assert!(!outs.iter().any(|m| matches!(
        m,
        Message::Room(_, _, _, rustket::room::Message::Panel(Pm::Close))
    )));
    assert_eq!(app.servers[&sid].inbox.plan_key, None);
    let _ = app.update(Message::Shortcut(Shortcut::Activity));
    app.servers.get_mut(&sid).unwrap().inbox.plan_key = settled;
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Inbox(InboxKind::Unreads),
    ));
    assert_eq!(kind(&app), Some(InboxKind::Unreads));
    assert_eq!(app.servers[&sid].inbox.plan_key, None);

    // Enter in All unreads (not catch-up) reads nothing.
    let reads = |app: &App| {
        app.servers[&sid]
            .reads
            .has_origin(&rustket::reads::Origin::Inbox)
    };
    let _ = app.update(Message::CatchUpKey(CatchUpKey::Enter));
    assert!(!reads(&app));
    assert_eq!(kind(&app), Some(InboxKind::Unreads));
    // In catch-up under a modal the keys are the modal's.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Header(sidebar::HeaderAct::CatchUp),
    ));
    assert_eq!(kind(&app), Some(InboxKind::CatchUp));
    let current = |app: &App| {
        app.servers[&sid]
            .inbox_view()
            .and_then(|i| i.catch_up())
            .and_then(|c| c.current().map(str::to_owned))
    };
    let at = current(&app);
    assert!(at.is_some());
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Create));
    assert!(!app.modals.is_empty());
    let _ = app.update(Message::CatchUpKey(CatchUpKey::Right));
    let _ = app.update(Message::CatchUpKey(CatchUpKey::Enter));
    assert_eq!(current(&app), at);
    assert!(!reads(&app));
}

/// Later covers the room area like the inbox views: the covered room is not
/// read, Esc closes the view and leaves the room alone, the shortcut
/// toggles it, the sidebar row opens it and opening a room closes it.
/// "Remind me…" from a message's menu sets a reminder the Reminders tab
/// lists; it waits in the config until the service room stores it.
#[test]
fn later_view_and_reminders() {
    use rustket::keymap::Shortcut;
    use rustket::later::when::Msg as Wm;
    use rustket::room::Message as Rm;
    use rustket::room::timeline::{Act, ItemMsg, Message as Tm};
    use rustket_core::reminders::{Preset, ReminderBlob};
    use rustket_core::workspace::LaterTab;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let tab = |app: &App| app.servers[&sid].later_view().map(|v| v.tab);
    let _ = app.update(room_msg(&app, &sid, "r1", Rm::MarkedRead(Ok(()))));
    let _ = app.update(Message::Shortcut(Shortcut::Later));
    assert_eq!(tab(&app), Some(LaterTab::InProgress));
    assert!(app.servers[&sid].room_area_taken());
    let _ = app.view();
    // The covered room is not read when messages arrive.
    deliver(&mut app, &sid, rc_msg("m1", "r1", T0 + 10, "B"));
    set_unread(&mut app, &sid, "r1", 1, T0 + 11);
    assert!(!app.servers[&sid].rooms["r1"].is_visible());
    // Esc closes it and does not mark the covered room read.
    let outs = outputs_within(app.update(Message::Escape), 300);
    assert_eq!(tab(&app), None);
    assert!(!outs.iter().any(|m| matches!(
        m,
        Message::Room(_, _, _, rustket::room::Message::MarkAllRead)
    )));
    // The shortcut toggles; the sidebar row opens; a room closes it.
    let _ = app.update(Message::Shortcut(Shortcut::Later));
    let _ = app.update(Message::Shortcut(Shortcut::Later));
    assert_eq!(tab(&app), None);
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Later));
    assert_eq!(tab(&app), Some(LaterTab::InProgress));
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    assert_eq!(tab(&app), None);
    // Back in r1: "Remind me…" on a new message, then "In 1 hour".
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    deliver(&mut app, &sid, rc_msg("m3", "r1", T0 + 20, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Timeline(Tm::Item(ItemMsg::Act("m3".into(), Act::Remind))),
    ));
    let _ = app.view();
    let before = chrono::Utc::now().timestamp_millis();
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Timeline(Tm::Item(ItemMsg::Remind(Wm::Preset(Preset::In1Hour)))),
    ));
    let s = &app.servers[&sid];
    let r = s.workspace.reminders().get("m3").expect("reminder set");
    assert_eq!(r.rid, "r1");
    assert!(r.at_ms >= before + 3_599_000);
    // The message's copy is kept for the notification.
    assert!(s.workspace.later_message("m3").is_some());
    // The Reminders tab lists it; the sidebar row shows no overdue count.
    let _ = app.update(Message::Shortcut(Shortcut::Later));
    let epoch = app.servers[&sid].generation;
    let _ = app.update(Message::Later(
        sid.clone(),
        epoch,
        rustket::later::Msg::Tab(LaterTab::Reminders),
    ));
    let rows = app.servers[&sid].workspace.later_rows(LaterTab::Reminders);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mid, "m3");
    let _ = app.view();
    // The edit waits in the config (`pending_service`) until stored.
    let _ = app.update(Message::Noop);
    let entry = app.config.server(&sid).unwrap();
    let pending: ReminderBlob = entry.pending_blob("A");
    assert!(pending.view().get("m3").is_some());
    // Removing it from the view clears it.
    let _ = app.update(Message::Later(
        sid.clone(),
        epoch,
        rustket::later::Msg::RemoveReminder("m3".into()),
    ));
    assert!(app.servers[&sid].workspace.reminders().get("m3").is_none());
}

/// A due reminder gets one notification (its text from the local copy);
/// clicking it opens the message and marks the reminder done; one cleared
/// elsewhere closes its notification; with reminder notifications off
/// nothing is shown.
#[test]
fn due_reminders_notify_once_and_a_click_opens_and_clears() {
    use rustket::desktop::notifications::{
        self as dn, ACTION_DEFAULT, MockNotifier, Notifications, Signal,
    };
    use rustket_core::reminders::ReminderEdit;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let mock = MockNotifier::default();
    app.desktop.notifications = Notifications::with_backend(Arc::new(mock.clone()));
    deliver(&mut app, &sid, rc_msg("m3", "r1", T0 + 20, "B"));
    let _ = app.update(Message::Noop);
    // A reminder due in a moment (the dialog refuses times this close), on
    // a reply in thread m3 known only from its local copy.
    let soon = chrono::Utc::now().timestamp_millis() + 150;
    {
        let s = app.servers.get_mut(&sid).unwrap();
        let set = ReminderEdit::Set {
            rid: "r1".into(),
            tmid: Some("m3".into()),
            mid: "t1".into(),
            at_ms: soon,
        };
        s.reminders
            .edit(&mut s.workspace, &set, "dev", std::time::Instant::now())
            .unwrap();
        let mut reply = rc_msg("t1", "r1", T0 + 30, "B");
        reply.tmid = Some("m3".into());
        s.workspace.keep_reminder_copy(&reply);
    }
    std::thread::sleep(std::time::Duration::from_millis(250));
    let task = app.update(Message::Noop);
    {
        let shown = mock.shown.lock().unwrap();
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].body.contains("text t1"), "{:?}", shown[0]);
        assert!(shown[0].actions.iter().any(|(k, _)| k == "snooze"));
    }
    // Its id comes back; later passes show nothing more.
    for m in outputs_within(task, 300) {
        if matches!(m, Message::ReminderNotify(_)) {
            let _ = app.update(m);
        }
    }
    let _ = app.update(Message::Noop);
    assert_eq!(mock.shown.lock().unwrap().len(), 1);
    // A click: the room opens with the thread at the reply (from the local
    // copy, no fetch) and the reminder is done.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    let _ = app.update(Message::Desktop(rustket::desktop::Message::Notifications(
        dn::Message::Signal {
            id: 1,
            signal: Signal::Action(ACTION_DEFAULT.into()),
        },
    )));
    let s = &app.servers[&sid];
    assert_eq!(s.active_room.as_deref(), Some("r1"));
    let panel = &s.rooms["r1"].panel;
    assert_eq!(panel.thread_tmid(), Some("m3"));
    assert_eq!(panel.thread_focus(), Some("t1"), "shown at the reply");
    assert!(s.workspace.reminders().get("t1").is_none(), "marked done");
    assert!(s.workspace.later_message("t1").is_none(), "copy dropped");

    // Another reminder, cleared elsewhere after it fired: closed.
    let soon = chrono::Utc::now().timestamp_millis() + 150;
    let set = |s: &mut rustket::workspace::ServerState, mid: &str, at: i64| {
        let e = ReminderEdit::Set {
            rid: "r1".into(),
            tmid: None,
            mid: mid.into(),
            at_ms: at,
        };
        s.reminders
            .edit(&mut s.workspace, &e, "dev", std::time::Instant::now())
            .unwrap();
    };
    set(app.servers.get_mut(&sid).unwrap(), "m4", soon);
    std::thread::sleep(std::time::Duration::from_millis(250));
    let task = app.update(Message::Noop);
    assert_eq!(mock.shown.lock().unwrap().len(), 2);
    for m in outputs_within(task, 300) {
        if matches!(m, Message::ReminderNotify(_)) {
            let _ = app.update(m);
        }
    }
    {
        let s = app.servers.get_mut(&sid).unwrap();
        s.reminders
            .edit(
                &mut s.workspace,
                &ReminderEdit::Clear { mid: "m4".into() },
                "dev",
                std::time::Instant::now(),
            )
            .unwrap();
    }
    let _ = app.update(Message::Noop);
    assert_eq!(*mock.closed.lock().unwrap(), [2]);

    // Notifications off: a due reminder shows nothing.
    app.config.ui.reminder_notifications = false;
    let soon = chrono::Utc::now().timestamp_millis() + 150;
    set(app.servers.get_mut(&sid).unwrap(), "m5", soon);
    std::thread::sleep(std::time::Duration::from_millis(250));
    let _ = app.update(Message::Noop);
    assert_eq!(mock.shown.lock().unwrap().len(), 2);
}

/// Later's first run seeds the account (older stars count as archived);
/// a kept thread starred by "Move kept threads to Favorites" is archived;
/// a star from the message menu goes to In progress, an unstar forgets it.
#[test]
fn later_seed_kept_threads_and_menu_stars() {
    use rustket::room::Message as Rm;
    use rustket::room::timeline::{Act, ItemMsg, Message as Tm};
    use rustket_core::later::LaterStatus;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let _ = app.update(Message::Noop);
    let later = |app: &App| app.servers[&sid].workspace.later().clone();
    assert!(later(&app).is_seeded());
    assert_eq!(later(&app).default_status(Some(T0)), LaterStatus::Archived);
    // A queued star of a kept thread succeeded.
    let _ = app.update(Message::ThreadStarred {
        sid: sid.clone(),
        epoch: app.servers[&sid].generation,
        rid: "r1".into(),
        tmid: "t9".into(),
        on: true,
        queued: true,
        result: Ok(()),
    });
    assert_eq!(later(&app).status_of("t9", None), LaterStatus::Archived);
    // The menu's star of an old message: In progress explicitly.
    deliver(&mut app, &sid, rc_msg("m6", "r1", T0 + 30, "B"));
    let _ = app.update(room_msg(
        &app,
        &sid,
        "r1",
        Rm::Timeline(Tm::Item(ItemMsg::Act("m6".into(), Act::Star))),
    ));
    assert_eq!(
        later(&app).status_of("m6", Some(T0 + 30)),
        LaterStatus::InProgress
    );
    // The pending edits wait in the config.
    let _ = app.update(Message::Noop);
    let pending: rustket_core::later::LaterBlob =
        app.config.server(&sid).unwrap().pending_blob("A");
    assert!(pending.is_seeded() && pending.status.contains_key("m6"));
}

/// "Schedule message" keeps the text out of the timeline; the composer's
/// hint and the Drafts & scheduled view (row, Ctrl+Shift+D) list it with
/// the drafts; Send now puts it in the outbox; a message whose time passed
/// while it could not be sent opens the "not sent on time" dialog.
#[test]
fn scheduled_messages_and_drafts_view() {
    use rustket::drafts::Msg as Dm;
    use rustket::drafts::missed::MissedMsg;
    use rustket::keymap::Shortcut;
    use rustket::later::when::Msg as Wm;
    use rustket::room::Message as Rm;
    use rustket::room::composer::Message as Cm;
    use rustket_core::reminders::Preset;
    use rustket_core::timeline::DeliveryState;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    // Without an offline cache nothing can be scheduled (it would be lost).
    let _ = app.update(Message::Noop);
    assert!(!app.servers[&sid].workspace.scheduled_storage());
    app.offline = rustket::cache::Caches::new(Some(dir.path().join("offline")));
    let _ = app.update(Message::Noop);
    assert!(app.servers[&sid].workspace.scheduled_storage());
    let composer = |app: &App, m: Cm| room_msg(app, &sid, "r1", Rm::Composer(m));
    for ch in "hello later".chars() {
        let m = composer(
            &app,
            Cm::Editor(iced::widget::text_editor::Action::Edit(
                iced::widget::text_editor::Edit::Insert(ch),
            )),
        );
        let _ = app.update(m);
    }
    let _ = app.update(composer(&app, Cm::Schedule));
    let _ = app.view();
    let _ = app.update(composer(
        &app,
        Cm::SchedulePicker(Wm::Preset(Preset::In1Hour)),
    ));
    let ws = &app.servers[&sid].workspace;
    assert_eq!(ws.scheduled().len(), 1);
    assert!(ws.pending_messages("r1").is_empty());
    let id = ws.scheduled().sorted()[0].id().to_owned();
    let _ = app.view();
    // A draft in r2 shows next to it.
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r2".into()),
    ));
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .set_draft("r1", None, "unsent words");
    let _ = app.update(Message::Shortcut(Shortcut::Drafts));
    assert!(app.servers[&sid].drafts_view().is_some());
    assert!(app.servers[&sid].room_area_taken());
    let _ = app.view();
    let drafts = rustket::drafts::drafts(&app.servers[&sid].workspace);
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].2, "unsent words");
    // The shortcut toggles; the sidebar row opens it again.
    let _ = app.update(Message::Shortcut(Shortcut::Drafts));
    assert!(app.servers[&sid].drafts_view().is_none());
    let _ = app.update(Message::Sidebar(sid.clone(), sidebar::Message::Drafts));
    assert!(app.servers[&sid].drafts_view().is_some());
    // Send now: a queued outbox entry (no connection here: it waits).
    let epoch = app.servers[&sid].generation;
    let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::SendNow(id.clone())));
    let ws = &app.servers[&sid].workspace;
    assert!(ws.scheduled().is_empty());
    let pending = ws.pending_messages("r1");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.msg, "hello later");
    assert!(matches!(pending[0].state, DeliveryState::Failed { .. }));
    // Opening a draft's room closes the view.
    let _ = app.update(Message::Drafts(
        sid.clone(),
        epoch,
        Dm::Open {
            rid: "r1".into(),
            tmid: None,
        },
    ));
    assert!(app.servers[&sid].drafts_view().is_none());
    assert_eq!(app.servers[&sid].active_room.as_deref(), Some("r1"));
    // Missed while Rustket was off: not sent, the dialog asks.
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let past = chrono::Utc::now().timestamp_millis() - 3_600_000;
    for text in ["one", "two"] {
        let m = ws.new_local_message("r2", text, None, false);
        ws.schedule_message(m, past);
    }
    let _ = app.update(Message::Noop);
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    let _ = app.view();
    let missed: Vec<String> = app.servers[&sid]
        .workspace
        .scheduled()
        .missed()
        .iter()
        .filter(|e| e.message.msg == "one")
        .map(|e| e.id().to_owned())
        .collect();
    assert_eq!(app.servers[&sid].workspace.scheduled().missed().len(), 2);
    assert!(
        app.servers[&sid]
            .workspace
            .pending_messages("r2")
            .is_empty()
    );
    let _ = app.update(Message::Missed(MissedMsg::Delete(
        sid.clone(),
        missed[0].clone(),
    )));
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    let _ = app.update(Message::Missed(MissedMsg::SendAll));
    assert!(app.modals.is_empty(), "nothing left: the dialog closes");
    let pending = app.servers[&sid].workspace.pending_messages("r2");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.msg, "two");
    // Missed ones removed elsewhere: the dialog closes by itself.
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let m = ws.new_local_message("r2", "three", None, false);
    let id3 = m.id.clone();
    ws.schedule_message(m, past);
    let _ = app.update(Message::Noop);
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .remove_scheduled(&id3);
    let _ = app.update(Message::Noop);
    assert!(app.modals.is_empty());
    // An entry being edited does not go out when its time comes; the
    // saved text goes once the dialog closes.
    let soon = chrono::Utc::now().timestamp_millis() + 600_000;
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let m = ws.new_local_message("r2", "draft text", None, false);
    let id4 = m.id.clone();
    ws.schedule_message(m, soon);
    let _ = app.update(Message::Shortcut(Shortcut::Drafts));
    let epoch = app.servers[&sid].generation;
    let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::Edit(id4.clone())));
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    assert!(ws.scheduled().get(&id4).unwrap().held);
    ws.reschedule(&id4, chrono::Utc::now().timestamp_millis() - 1_000);
    let _ = app.update(Message::Noop);
    assert!(
        app.servers[&sid].workspace.scheduled().get(&id4).is_some(),
        "held while the dialog is open"
    );
    for a in [
        iced::widget::text_editor::Action::SelectAll,
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(
            std::sync::Arc::new("edited text".into()),
        )),
    ] {
        let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::EditAction(a)));
    }
    let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::SaveEdit));
    let _ = app.update(Message::Noop);
    // Released with the saved text; it goes once the server answers (not
    // connected here, so it waits within the grace minute).
    let e = app.servers[&sid].workspace.scheduled().get(&id4).unwrap();
    assert!(!e.held);
    assert_eq!(e.message.msg, "edited text");
}

/// A dialog of Drafts & scheduled holds its entry (delete too) and keeps
/// the view: shortcuts and room switches wait for it to close. An edited
/// text goes through the composer's checks and stays in the dialog when
/// refused. The "not sent on time" dialog leaves held entries to their
/// dialog and asks before "Delete all". A rejected token says how many
/// scheduled messages went with the offline data, on the login screen too.
#[test]
fn drafts_dialogs_hold_their_entry_and_check_edits() {
    use iced::widget::text_editor::{Action, Edit};
    use rustket::drafts::Msg as Dm;
    use rustket::drafts::missed::MissedMsg;
    use rustket::keymap::Shortcut;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    app.offline = rustket::cache::Caches::new(Some(dir.path().join("offline")));
    let _ = app.update(Message::Noop);
    let now = chrono::Utc::now().timestamp_millis();
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let m = ws.new_local_message("r2", "original", None, false);
    let x = m.id.clone();
    ws.schedule_message(m, now + 600_000);
    let _ = app.update(Message::Shortcut(Shortcut::Drafts));
    let epoch = app.servers[&sid].generation;
    let drafts = |m: Dm| Message::Drafts(sid.clone(), epoch, m);
    let held = |app: &App| {
        app.servers[&sid]
            .workspace
            .scheduled()
            .get(&x)
            .unwrap()
            .held
    };
    // "Delete this scheduled message?" holds it like edit and reschedule.
    let _ = app.update(drafts(Dm::Delete(x.clone())));
    assert!(held(&app));
    let _ = app.update(drafts(Dm::CloseDialog));
    assert!(!held(&app));
    // An open edit keeps the view: shortcuts and room switches wait.
    let _ = app.update(drafts(Dm::Edit(x.clone())));
    for sc in [Shortcut::Search, Shortcut::Later, Shortcut::Drafts] {
        let _ = app.update(Message::Shortcut(sc));
    }
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open("r1".into()),
    ));
    assert!(app.servers[&sid].drafts_dialog_open());
    assert!(held(&app));
    // A slash command or a text over the server's limit is refused; the
    // dialog stays with the text.
    let type_over = |app: &mut App, text: String| {
        for a in [
            Action::SelectAll,
            Action::Edit(Edit::Paste(std::sync::Arc::new(text))),
        ] {
            let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::EditAction(a)));
        }
    };
    for text in ["/kick @bob".to_owned(), "x".repeat(5_001)] {
        type_over(&mut app, text);
        let toasts = app.toasts.len();
        let _ = app.update(drafts(Dm::SaveEdit));
        assert_eq!(app.toasts.len(), toasts + 1);
        assert!(app.servers[&sid].drafts_dialog_open(), "the text stays");
    }
    let e = app.servers[&sid].workspace.scheduled().get(&x).unwrap();
    assert_eq!(e.message.msg, "original");
    type_over(&mut app, "fixed".into());
    let _ = app.update(drafts(Dm::SaveEdit));
    assert!(!app.servers[&sid].drafts_dialog_open());
    let e = app.servers[&sid].workspace.scheduled().get(&x).unwrap();
    assert_eq!(e.message.msg, "fixed");
    // Missed X is being edited when Y turns missed too: "Send all" and
    // "Delete all" leave X to its dialog; "Delete all" asks first.
    let past = chrono::Utc::now().timestamp_millis() - 3_600_000;
    app.servers
        .get_mut(&sid)
        .unwrap()
        .workspace
        .reschedule(&x, past);
    let _ = app.update(Message::Noop);
    app.modals.clear();
    let _ = app.update(drafts(Dm::Edit(x.clone())));
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    for text in ["y1", "y2"] {
        let m = ws.new_local_message("r2", text, None, false);
        ws.schedule_message(m, past);
    }
    let _ = app.update(Message::Noop);
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    let _ = app.view();
    let missed = |app: &App| app.servers[&sid].workspace.scheduled().missed().len();
    assert_eq!(missed(&app), 3);
    let _ = app.update(Message::Missed(MissedMsg::DeleteAll));
    assert_eq!(missed(&app), 3, "asks first");
    assert!(matches!(app.modals.last(), Some(Modal::Missed(m)) if m.confirm_delete_all));
    let _ = app.view();
    let _ = app.update(Message::Missed(MissedMsg::CancelDeleteAll));
    let _ = app.update(Message::Missed(MissedMsg::SendAll));
    assert_eq!(missed(&app), 1);
    assert!(app.servers[&sid].workspace.scheduled().get(&x).is_some());
    let _ = app.update(Message::Missed(MissedMsg::SendNow(sid.clone(), x.clone())));
    let _ = app.update(Message::Missed(MissedMsg::Delete(sid.clone(), x.clone())));
    assert!(app.servers[&sid].workspace.scheduled().get(&x).is_some());
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    // A rejected token: the scheduled messages go with the cache, and the
    // user hears of it.
    let toasts = app.toasts.len();
    let m = ev(
        &app,
        &sid,
        SessionEvent::LoggedOut {
            reason: "token expired".into(),
        },
    );
    let _ = app.update(m);
    assert!(
        app.toasts[toasts..]
            .iter()
            .any(|t| t.title == "Scheduled messages deleted" && t.body.contains('1')),
        "{:?}",
        app.toasts
    );
    // The toast goes by: the login screen keeps saying it until sign-in.
    let reason = app.servers[&sid]
        .login
        .as_ref()
        .and_then(|l| l.reason.clone())
        .unwrap_or_default();
    assert!(
        reason.contains("scheduled message(s) were deleted") && reason.contains('1'),
        "{reason}"
    );
}

/// A due scheduled message the server refuses for good goes back to
/// Drafts & scheduled as a missed entry with the error (kept on disk, the
/// "not sent" dialog lists it), not a failed row in a room that a user who
/// was away never sees.
#[test]
fn scheduled_message_failing_for_good_returns_to_drafts() {
    use rustket::cache::{Msg as CacheMsg, Outcome, Sent};
    use rustket::drafts::Msg as Dm;
    use rustket::keymap::Shortcut;
    use rustket_core::workspace::ConnectionState;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    app.offline = rustket::cache::Caches::new(Some(dir.path().join("offline")));
    let _ = app.update(Message::Noop);
    let ws = &mut app.servers.get_mut(&sid).unwrap().workspace;
    let m = ws.new_local_message("r1", "read-only later", None, false);
    let id = m.id.clone();
    ws.schedule_message(m, chrono::Utc::now().timestamp_millis() + 3_600_000);
    let epoch = app.servers[&sid].generation;
    let _ = app.update(Message::Shortcut(Shortcut::Drafts));
    assert!(app.servers[&sid].drafts_view().is_some());
    let _ = app.update(Message::Drafts(sid.clone(), epoch, Dm::SendNow(id.clone())));
    let s = app.servers.get_mut(&sid).unwrap();
    assert!(s.cache.is_queued(&id));
    s.workspace.apply_connection(ConnectionState::Connected);
    let refused = rocketchat::Error::Api {
        status: 400,
        error: Some("error-not-allowed".into()),
        error_type: None,
        message: Some("read only".into()),
        details: None,
    };
    let toasts = app.toasts.len();
    let _ = app.update(Message::Cache(CacheMsg::Sent(
        sid.clone(),
        epoch,
        Box::new(Sent {
            id: id.clone(),
            rid: "r1".into(),
            tmid: None,
            outcome: Outcome::Failed(Failure::from(refused)),
        }),
    )));
    let s = &app.servers[&sid];
    assert!(s.workspace.pending_messages("r1").is_empty());
    assert!(!s.cache.is_queued(&id));
    let e = s.workspace.scheduled().get(&id).expect("back in the queue");
    assert!(e.missed && e.error.is_some());
    assert_eq!(e.message.msg, "read-only later");
    assert!(matches!(app.modals.last(), Some(Modal::Missed(_))));
    assert!(
        app.toasts[toasts..]
            .iter()
            .any(|t| t.title == "Scheduled message not sent"),
        "{:?}",
        app.toasts
    );
    let _ = app.view();
}

/// Pausing notifications from the menus is kept in the config (it survives
/// a restart) until it ends or the user resumes; the custom dialog sets
/// any later time.
#[test]
fn notification_pause_is_persisted() {
    use rustket::later::when::Msg as Wm;
    use rustket::quiet::{Msg as Qm, Request};
    use rustket_core::quiet::PausePreset;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sid) = loaded_app(dir.path());
    assert!(!app.quiet_now().paused);
    let _ = app.update(Message::Quiet(Qm::Preset(PausePreset::Hour1)));
    let q = app.quiet_now();
    assert!(q.paused);
    let until = app.config.pause.paused_until.unwrap();
    assert!(until > chrono::Utc::now().timestamp_millis() + 3_500_000);
    assert_eq!(q.until, Some(until));
    let _ = app.view();
    let _ = app.update(Message::Quiet(Qm::Request(Request::Resume)));
    assert!(!app.quiet_now().paused);
    assert_eq!(app.config.pause.paused_until, None);
    // Custom: a dialog with the picker.
    let _ = app.update(Message::Quiet(Qm::Request(Request::PauseCustom)));
    assert!(matches!(app.modals.last(), Some(Modal::PauseCustom(_))));
    let _ = app.view();
    let _ = app.update(Message::Quiet(Qm::Picker(Wm::Pause(PausePreset::Min30))));
    assert!(app.modals.is_empty());
    assert!(app.quiet_now().paused);
    // A pause in the past is ignored.
    let _ = app.update(Message::Quiet(Qm::Request(Request::Resume)));
    let _ = app.update(Message::Quiet(Qm::Request(Request::Pause(1))));
    assert!(!app.quiet_now().paused);
}

/// A status the user picks releases a pause's busy hold (nothing is
/// restored over it); the remembered statuses outlive a sign-out (a
/// rejected token, memory-only secrets: the next login of the account
/// restores them).
#[test]
fn explicit_status_releases_the_hold_and_sign_out_keeps_it() {
    use rocketchat::models::UserStatus;
    use rustket::workspace::status_menu::Message as Sm;
    use rustket_core::quiet::{StatusExpiry, StatusHold};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sid) = loaded_app(dir.path());
    let entry = app.config.server_mut(&sid).unwrap();
    entry.status_hold = Some(StatusHold {
        user_id: "A".into(),
        status: UserStatus::Online,
        text: String::new(),
        released: false,
    });
    // A text change keeps the hold.
    let _ = app.update(Message::StatusMenu(
        sid.clone(),
        Sm::StatusSet(None, Some("Lunch".into()), Ok(())),
    ));
    assert!(
        !app.config
            .server(&sid)
            .unwrap()
            .status_hold
            .as_ref()
            .unwrap()
            .released
    );
    // A chosen status (also busy) releases it.
    let _ = app.update(Message::StatusMenu(
        sid.clone(),
        Sm::StatusSet(Some(UserStatus::Busy), Some(String::new()), Ok(())),
    ));
    assert!(
        app.config
            .server(&sid)
            .unwrap()
            .status_hold
            .as_ref()
            .unwrap()
            .released
    );
    assert_eq!(
        app.servers[&sid].workspace.my_chosen_status(),
        Some(UserStatus::Busy)
    );
    // Signed out (the login screen, `restoring` over): kept.
    app.config.server_mut(&sid).unwrap().status_expiry = Some(StatusExpiry {
        user_id: "A".into(),
        at: 1,
        status: UserStatus::Busy,
        text: String::new(),
        restore: None,
    });
    let s = app.servers.get_mut(&sid).unwrap();
    s.credentials = None;
    s.restoring = false;
    let _ = app.update(Message::Noop);
    let e = app.config.server(&sid).unwrap();
    assert!(e.status_hold.is_some() && e.status_expiry.is_some());
}
