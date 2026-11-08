use iced::widget::text_editor::{Action as EditorAction, Edit};
use rocketchat::models::{Room, RoomType, Setting, Subscription as Sub, User, UserRef};
use rocketchat::{RcDate, ServerUrl};
use rustket_core::config::UiPrefs;
use rustket_core::sync::InitialData;
use rustket_core::{ServerId, Workspace};
use serde_json::json;

use super::*;
use crate::images::Images;
use crate::room::panel::{Kind, Panel};

const T0: i64 = 1_800_000_000_000;

struct Env {
    sid: ServerId,
    server: ServerUrl,
    ws: Workspace,
    images: Images,
    prefs: UiPrefs,
}

impl Env {
    fn new(preference: Option<&str>) -> Self {
        let mut me = User {
            id: "me".into(),
            username: Some("me".into()),
            roles: Some(vec!["user".into()]),
            ..Default::default()
        };
        if let Some(p) = preference {
            me = serde_json::from_value(json!({
                "_id": "me", "username": "me", "roles": ["user"],
                "settings": {"preferences": {"alsoSendThreadToChannel": p}}
            }))
            .unwrap();
        }
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me,
            subscriptions: vec![Sub {
                id: "s1".into(),
                rid: "R1".into(),
                name: "general".into(),
                t: RoomType::Channel,
                u: UserRef {
                    id: "me".into(),
                    username: Some("me".into()),
                    name: None,
                },
                open: true,
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "R1".into(),
                t: RoomType::Channel,
                name: Some("general".into()),
                ..Default::default()
            }],
            settings: vec![Setting {
                id: "Threads_enabled".into(),
                value: json!(true),
                ..Default::default()
            }],
            ..Default::default()
        });
        ws.open_room("R1");
        Self {
            sid: ServerId("thread-tests".into()),
            server: ServerUrl::parse("http://chat.test").unwrap(),
            ws,
            images: Images::new(None),
            prefs: UiPrefs::default(),
        }
    }

    fn ctx(&mut self) -> UpdateCtx<'_> {
        UpdateCtx {
            server_id: &self.sid,
            server: &self.server,
            rid: "R1",
            workspace: &mut self.ws,
            session: None,
            images: &self.images,
            prefs: &self.prefs,
        }
    }
}

fn message(id: &str, uid: &str, ms: i64, tmid: Option<&str>) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: "R1".into(),
        msg: format!("text {id}"),
        ts: RcDate::from_millis(ms),
        updated_at: RcDate::from_millis(ms),
        tmid: tmid.map(Into::into),
        u: UserRef {
            id: uid.into(),
            username: Some(uid.into()),
            name: None,
        },
        ..Default::default()
    }
}

fn page(items: Vec<RcMessage>, offset: u64, total: u64) -> Paginated<RcMessage> {
    Paginated {
        raw_len: items.len(),
        count: items.len() as u64,
        items,
        offset,
        total,
        dropped: 0,
    }
}

fn parent(tcount: u64) -> RcMessage {
    let mut p = message("P", "bob", T0, None);
    p.tcount = Some(tcount);
    p.replies = Some(vec!["bob".into()]);
    p
}

#[test]
fn also_send_defaults_follow_the_preference() {
    let env = Env::new(None);
    assert!(also_send_default(&env.ws, None), "first reply");
    assert!(also_send_default(&env.ws, Some(0)));
    assert!(!also_send_default(&env.ws, Some(3)));
    let env = Env::new(Some("always"));
    assert!(also_send_default(&env.ws, Some(3)));
    let env = Env::new(Some("never"));
    assert!(!also_send_default(&env.ws, Some(0)));
    // Unset: the server's default user preference.
    let mut env = Env::new(None);
    env.ws.apply_setting(
        rustket_core::workspace::Change::Inserted,
        Setting {
            id: "Accounts_Default_User_Preferences_alsoSendThreadToChannel".into(),
            value: json!("always"),
            ..Default::default()
        },
    );
    assert_eq!(also_send_preference(&env.ws), "always");
    assert!(also_send_default(&env.ws, Some(5)));
}

#[test]
fn loading_pages_and_realtime_replies() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(3));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    // The parent comes from the room timeline; the first page is requested.
    assert!(t.parent.is_some());
    assert!(t.tshow_set && !t.tshow, "the thread has replies already");
    let first = t.ticket.expect("first page requested");
    assert!(env.ws.thread("P").unwrap().pagination.loading_before);
    // The first (newest) page.
    let _ = t.update(
        Msg::Page(
            first,
            Ok(page(vec![message("r2", "bob", T0 + 3, Some("P"))], 0, 3)),
        ),
        &mut env.ctx(),
    );
    let tl = env.ws.thread("P").unwrap();
    assert!(tl.pagination.loaded && tl.pagination.has_more_before);
    assert_eq!(tl.len(), 1);
    // Older replies when scrolled to the top: offset = loaded replies.
    let _ = t.update(Msg::ReachedTop, &mut env.ctx());
    let second = t.ticket.expect("second page requested");
    assert_ne!(first, second);
    let _ = t.update(Msg::Page(first, Ok(page(vec![], 0, 3))), &mut env.ctx());
    assert!(
        env.ws.thread("P").unwrap().pagination.loading_before,
        "an old ticket does not end the request"
    );
    let _ = t.update(
        Msg::Page(
            second,
            Ok(page(
                vec![
                    message("r1", "alice", T0 + 2, Some("P")),
                    message("r0", "alice", T0 + 1, Some("P")),
                ],
                1,
                3,
            )),
        ),
        &mut env.ctx(),
    );
    let tl = env.ws.thread("P").unwrap();
    assert!(!tl.pagination.has_more_before);
    let ids: Vec<&str> = tl.messages().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["r0", "r1", "r2"]);
    // A realtime reply reaches the open thread, and the parent's counters.
    env.ws.take_changes();
    env.ws
        .apply_message(message("r3", "bob", T0 + 10, Some("P")));
    let mut p = parent(4);
    p.updated_at = RcDate::from_millis(T0 + 10);
    env.ws.apply_message(p);
    let changes = env.ws.take_changes();
    assert!(changes.threads.contains("P"));
    let _ = t.on_changes(&changes, &mut env.ctx());
    assert!(env.ws.thread("P").unwrap().contains("r3"));
    assert_eq!(t.parent.as_ref().unwrap().message.tcount, Some(4));
    // A reply to another thread does not.
    env.ws
        .apply_message(message("x", "bob", T0 + 11, Some("Q")));
    assert!(!env.ws.thread("P").unwrap().contains("x"));
}

#[test]
fn parent_is_fetched_when_unknown_and_deletion_closes() {
    let mut env = Env::new(None);
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    assert!(t.parent.is_none() && t.parent_loading);
    let _ = t.update(Msg::Parent(Ok(Box::new(parent(0)))), &mut env.ctx());
    assert!(t.parent.is_some());
    assert!(t.tshow, "first reply of a thread: also send to channel");
    env.ws.take_changes();
    env.ws.apply_message_deleted("R1", "P");
    let changes = env.ws.take_changes();
    let a = t.on_changes(&changes, &mut env.ctx());
    assert!(a.effects.iter().any(|e| matches!(e, Effect::ClosePanel)));
    // A reply id opens its thread instead.
    let mut t = Thread::new("R1", "r1");
    let _ = t.activate(&mut env.ctx());
    let a = t.update(
        Msg::Parent(Ok(Box::new(message("r1", "bob", T0, Some("P"))))),
        &mut env.ctx(),
    );
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::OpenPanel(Kind::Thread(p))] if p == "P"
    ));
}

#[test]
fn sending_uses_tshow_and_the_default_unchecks() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(0));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    assert!(t.tshow);
    for ch in "hi".chars() {
        let _ = t.update(
            Msg::Composer(composer::Message::Editor(EditorAction::Edit(Edit::Insert(
                ch,
            )))),
            &mut env.ctx(),
        );
    }
    let _ = t.update(Msg::Composer(composer::Message::Send), &mut env.ctx());
    let pending = env.ws.pending_messages("R1");
    assert_eq!(pending.len(), 1);
    let m = &pending[0].message;
    assert_eq!(m.tmid.as_deref(), Some("P"));
    assert_eq!(m.tshow, Some(true));
    assert!(env.ws.thread("P").unwrap().contains(&m.id));
    assert!(
        env.ws.timeline("R1").unwrap().contains(&m.id),
        "shown in the channel"
    );
    assert!(!t.tshow, "the default preference unchecks after a send");
    // "always" keeps it.
    let mut env = Env::new(Some("always"));
    env.ws.apply_message(parent(2));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let _ = t.update(
        Msg::Composer(composer::Message::Editor(EditorAction::Edit(Edit::Insert(
            'x',
        )))),
        &mut env.ctx(),
    );
    let _ = t.update(Msg::Composer(composer::Message::Send), &mut env.ctx());
    assert!(t.tshow);
    // Unchecked by the user: a plain reply.
    let _ = t.update(Msg::AlsoSend(false), &mut env.ctx());
    let _ = t.update(
        Msg::Composer(composer::Message::Editor(EditorAction::Edit(Edit::Insert(
            'y',
        )))),
        &mut env.ctx(),
    );
    let _ = t.update(Msg::Composer(composer::Message::Send), &mut env.ctx());
    let last = env
        .ws
        .pending_messages("R1")
        .into_iter()
        .find(|i| i.message.msg == "y")
        .map(|i| i.message.clone())
        .unwrap();
    assert_eq!(last.tshow, None);
    assert!(!env.ws.timeline("R1").unwrap().contains(&last.id));
}

#[test]
fn failed_reply_retry_in_the_thread() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let mut m = env.ws.new_local_message("R1", "oops", Some("P"), false);
    m.ts = RcDate::from_millis(T0 + 5);
    let id = m.id.clone();
    env.ws.add_pending(m);
    env.ws.mark_pending_failed("R1", Some("P"), &id, "boom");
    let _ = t.update(Msg::Item(ItemMsg::Retry(id.clone())), &mut env.ctx());
    let state = |ws: &Workspace| ws.thread("P").unwrap().get(&id).map(|i| i.state.clone());
    assert_eq!(state(&env.ws), Some(DeliveryState::Sending));
    let mut echo = env
        .ws
        .thread("P")
        .unwrap()
        .get(&id)
        .unwrap()
        .message
        .clone();
    echo.temp = None;
    echo.updated_at = echo.ts;
    let _ = t.update(
        Msg::Item(ItemMsg::Resent(
            id.clone(),
            Some("P".into()),
            Ok(Box::new(echo)),
        )),
        &mut env.ctx(),
    );
    assert_eq!(state(&env.ws), Some(DeliveryState::Sent));
    assert!(env.ws.pending_messages("R1").is_empty());
}

#[test]
fn row_actions_route_to_the_thread_composer() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let mut own = message(
        "r1",
        "me",
        chrono::Utc::now().timestamp_millis() - 1000,
        Some("P"),
    );
    own.updated_at = own.ts;
    env.ws.apply_message(own);
    // Quote goes to the thread composer, never up to the room composer.
    let a = t.update(
        Msg::Item(ItemMsg::Act("r1".into(), Act::Quote)),
        &mut env.ctx(),
    );
    assert!(a.effects.iter().all(|e| !matches!(e, Effect::Compose(_))));
    assert_eq!(t.composer.quoted(), vec!["r1"]);
    // No "reply in thread" inside the thread.
    let acts = interact::available(
        &env.ws,
        &Source::Thread("P".into()),
        &env.ws.thread("P").unwrap().get("r1").unwrap().message,
        chrono::Utc::now(),
    );
    assert!(!acts.contains(&Act::ReplyInThread));
    assert!(acts.contains(&Act::Quote));
}

#[test]
fn closing_the_panel_releases_the_thread() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut p = Panel::new("R1");
    p.open(Kind::Thread("P".into()));
    let g = p.generation();
    let _ = p.update(super::super::Message::Activate(g), &mut env.ctx());
    assert!(env.ws.thread("P").is_some());
    assert_eq!(p.thread_tmid(), Some("P"));
    assert_eq!(p.width(), WIDTH);
    p.close();
    p.retire(&mut env.ws);
    assert!(env.ws.thread("P").is_none());
    // Back from a thread to the threads list releases it too.
    p.open(Kind::Threads);
    let g = p.generation();
    let _ = p.update(super::super::Message::Activate(g), &mut env.ctx());
    p.open(Kind::Thread("P".into()));
    let g = p.generation();
    let _ = p.update(super::super::Message::Activate(g), &mut env.ctx());
    assert!(env.ws.thread("P").is_some());
    let _ = p.update(super::super::Message::Back, &mut env.ctx());
    p.retire(&mut env.ws);
    assert_eq!(p.kind(), Some(&Kind::Threads));
    assert!(env.ws.thread("P").is_none());
}

#[test]
fn mark_read_needs_unread_and_bottom() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let _ = t.mark_read(&env.ctx());
    assert!(!t.reading, "nothing unread");
    let mut sub = env.ws.subscription("R1").unwrap().clone();
    sub.tunread = Some(vec!["P".into()]);
    env.ws
        .apply_subscription(rustket_core::workspace::Change::Updated, sub);
    t.follow.at_bottom = false;
    let _ = t.mark_read(&env.ctx());
    assert!(!t.reading, "scrolled up");
    t.follow.at_bottom = true;
    let _ = t.mark_read(&env.ctx());
    assert!(t.reading);
    let _ = t.update(Msg::MarkedRead(Ok(())), &mut env.ctx());
    assert!(!t.reading);
}

#[test]
fn reply_arriving_during_read_is_read_again() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let mut sub = env.ws.subscription("R1").unwrap().clone();
    sub.tunread = Some(vec!["P".into()]);
    env.ws
        .apply_subscription(rustket_core::workspace::Change::Updated, sub);
    let _ = t.mark_read(&env.ctx());
    assert!(t.reading);
    // Unread again while the request is in flight: skipped, then retried.
    let _ = t.mark_read(&env.ctx());
    assert!(t.read_again);
    let _ = t.update(Msg::MarkedRead(Ok(())), &mut env.ctx());
    assert!(t.reading, "read again after the request in flight");
    assert!(!t.read_again);
}

/// Review: a reset of the room's timelines (resync, history import) left
/// the open thread empty for good.
#[test]
fn reset_reloads_the_open_thread() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(2));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let first = t.ticket.unwrap();
    let _ = t.update(
        Msg::Page(
            first,
            Ok(page(vec![message("r1", "bob", T0 + 3, Some("P"))], 0, 2)),
        ),
        &mut env.ctx(),
    );
    assert!(env.ws.thread("P").unwrap().pagination.loaded);
    t.follow.at_bottom = false;
    env.ws.take_changes();
    env.ws.reset_room_timelines("R1");
    let changes = env.ws.take_changes();
    let _ = t.on_changes(&changes, &mut env.ctx());
    let p = &env.ws.thread("P").unwrap().pagination;
    assert!(p.loading_before, "the newest replies are requested again");
    assert!(t.follow.at_bottom);
    let again = t.ticket.unwrap();
    assert_ne!(again, first);
    // A first page dropped because a reply was deleted in flight: again.
    env.ws.apply_message_deleted("R1", "r1");
    let _ = t.update(
        Msg::Page(
            again,
            Ok(page(vec![message("r2", "bob", T0 + 4, Some("P"))], 0, 2)),
        ),
        &mut env.ctx(),
    );
    let tl = env.ws.thread("P").unwrap();
    assert!(tl.pagination.loaded || tl.pagination.loading_before);
}

/// Review: the focused reply was revealed again with every older page and
/// stayed highlighted forever.
#[tokio::test]
async fn focused_reply_is_revealed_once() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(3));
    let mut t = Thread::new("R1", "P");
    t.set_focus(Some("r2".into()));
    let _ = t.activate(&mut env.ctx());
    let first = t.ticket.unwrap();
    let _ = t.update(
        Msg::Page(
            first,
            Ok(page(
                vec![
                    message("r3", "bob", T0 + 4, Some("P")),
                    message("r2", "bob", T0 + 3, Some("P")),
                ],
                0,
                4,
            )),
        ),
        &mut env.ctx(),
    );
    assert!(!t.follow.at_bottom);
    assert!(t.focus.is_none(), "taken once revealed");
    let stamp = t.selected.as_ref().map(|(m, s)| {
        assert_eq!(m, "r2");
        *s
    });
    // Back at the bottom; an older page does not move the view again.
    t.follow.at_bottom = true;
    let _ = t.update(Msg::ReachedTop, &mut env.ctx());
    let second = t.ticket.unwrap();
    let _ = t.update(
        Msg::Page(
            second,
            Ok(page(vec![message("r1", "bob", T0 + 2, Some("P"))], 2, 4)),
        ),
        &mut env.ctx(),
    );
    assert!(t.follow.at_bottom);
    // The highlight ends.
    let _ = t.update(Msg::Unselect(stamp.unwrap()), &mut env.ctx());
    assert!(t.selected.is_none());
}

/// Review: closing a thread that never activated wiped its saved draft.
#[test]
fn unactivated_thread_keeps_the_draft() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    env.ws.set_draft("R1", Some("P"), "my unsent reply");
    let mut p = Panel::new("R1");
    p.open(Kind::Thread("P".into()));
    p.close();
    p.retire(&mut env.ws);
    assert_eq!(env.ws.draft("R1", Some("P")), Some("my unsent reply"));
}

#[test]
fn scrolled_up_thread_freezes_and_jumps_back() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let first = t.ticket.unwrap();
    let _ = t.update(
        Msg::Page(
            first,
            Ok(page(vec![message("r1", "bob", T0 + 3, Some("P"))], 0, 1)),
        ),
        &mut env.ctx(),
    );
    let _ = t.on_changes(&env.ws.take_changes(), &mut env.ctx());
    // Scrolled up: a new reply waits below the view.
    t.follow.at_bottom = false;
    t.follow
        .hold_at(env.ws.thread("P").unwrap(), "r1")
        .then_some(())
        .unwrap();
    env.ws
        .apply_message(message("r2", "bob", T0 + 9, Some("P")));
    let _ = t.on_changes(&env.ws.take_changes(), &mut env.ctx());
    assert_eq!(t.follow.frozen(), Some((T0 + 3, "r1")));
    let _ = t.update(Msg::JumpToLatest, &mut env.ctx());
    assert!(t.follow.at_bottom && t.follow.frozen().is_none());
}

#[test]
fn expanded_state_carries_over_to_the_next_thread() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut p = Panel::new("R1");
    p.open(Kind::Thread("P".into()));
    let g = p.generation();
    let _ = p.update(super::super::Message::Activate(g), &mut env.ctx());
    assert!(!p.expanded());
    let _ = p.update(
        super::super::Message::Body(g, super::super::BodyMsg::Thread(Msg::ToggleExpand)),
        &mut env.ctx(),
    );
    assert!(p.expanded());
    p.open(Kind::Thread("Q".into()));
    assert!(p.expanded());
}
