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
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
    ));
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
    let _ = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
    ));
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
    let local = app.update(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
    ));
    let dir2 = tempfile::tempdir().unwrap();
    let (mut app2, sid2, _) = app_with(dir2.path(), Some("A"), vec![]);
    let everywhere = app2.update(Message::Sidebar(
        sid2.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::LogoutEverywhere),
    ));
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
