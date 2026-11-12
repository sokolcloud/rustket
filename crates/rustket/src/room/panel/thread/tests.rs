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

/// Review: a parent copy from the sidebar's thread index (possibly restored
/// from the offline cache and out of date) is shown but fetched again; one
/// from the room timeline is current.
#[test]
fn index_parent_is_shown_but_fetched_again() {
    let mut env = Env::new(None);
    env.ws
        .set_thread_config(rustket_core::config::ThreadsMode::UnreadKept, [("R1", "P")]);
    env.ws.apply_thread_parents(vec![parent(1)]);
    assert!(env.ws.thread_index().parent("P").is_some());
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    assert!(t.parent.is_some(), "the index copy is shown meanwhile");
    assert!(t.parent_loading, "and fetched again");
    let mut edited = parent(1);
    edited.msg = "edited".into();
    edited.updated_at = RcDate::from_millis(T0 + 10);
    let _ = t.update(Msg::Parent(Ok(Box::new(edited))), &mut env.ctx());
    assert_eq!(t.parent.as_ref().unwrap().message.msg, "edited");
    t.on_close(&mut env.ws);
    // The room timeline holds it: not fetched.
    env.ws.apply_message(parent(1));
    assert!(env.ws.timeline("R1").unwrap().get("P").is_some());
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    assert!(t.parent.is_some() && !t.parent_loading);
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
        Msg::Composer(composer::Message::Sent(id.clone(), Ok(Box::new(echo)))),
        &mut env.ctx(),
    );
    assert_eq!(state(&env.ws), Some(DeliveryState::Sent));
    assert!(env.ws.pending_messages("R1").is_empty());
}

#[test]
fn failed_reply_is_handled_by_the_thread_composer_only() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let mut m = env.ws.new_local_message("R1", "oops", Some("P"), false);
    m.ts = RcDate::from_millis(T0 + 5);
    let id = m.id.clone();
    env.ws.add_pending(m);
    env.ws.mark_pending_failed("R1", Some("P"), &id, "boom");
    // Another thread's composer leaves it alone.
    let mut other = Composer::for_thread("R1", "Q");
    let _ = other.intent(ComposeIntent::Discard(id.clone()), &mut env.ctx());
    let _ = other.intent(ComposeIntent::Retry(id.clone()), &mut env.ctx());
    assert_eq!(env.ws.pending_messages("R1").len(), 1);
    // The row action reaches this thread's composer, never the room's.
    let a = t.update(Msg::Item(ItemMsg::Discard(id.clone())), &mut env.ctx());
    assert!(a.effects.iter().all(|e| !matches!(e, Effect::Compose(_))));
    assert!(env.ws.pending_messages("R1").is_empty());
    assert!(env.ws.thread("P").unwrap().get(&id).is_none());
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
    assert!(!acts.contains(&Act::StartDiscussion));
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
    let _ = p.retire(&mut env.ws, None);
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
    let _ = p.retire(&mut env.ws, None);
    assert_eq!(p.kind(), Some(&Kind::Threads));
    assert!(env.ws.thread("P").is_none());
}

#[test]
fn mark_read_needs_unread_visible_and_bottom() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    t.set_visible(true);
    let _ = t.mark_read(&env.ctx());
    assert!(!t.reading, "nothing unread");
    let mut sub = env.ws.subscription("R1").unwrap().clone();
    sub.tunread = Some(vec!["P".into()]);
    env.ws
        .apply_subscription(rustket_core::workspace::Change::Updated, sub);
    t.set_visible(false);
    let _ = t.mark_read(&env.ctx());
    assert!(!t.reading, "not on screen");
    t.set_visible(true);
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
    t.set_visible(true);
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
    let _ = p.retire(&mut env.ws, None);
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

/// User report: opening another thread while one was shown made the keyed
/// column's search (only one changed run, first key changed) diff the new
/// parent's `lazy` row against the top sensor's tree: "Downcast widget state".
#[test]
fn switching_threads_and_selection_rebuild_the_view_without_panicking() {
    use iced::advanced::widget::Tree;

    fn loaded(env: &mut Env, tmid: &str, replies: &[&str]) -> Thread {
        let mut p = message(tmid, "bob", T0, None);
        p.tcount = Some(replies.len() as u64);
        env.ws.apply_message(p);
        let mut t = Thread::new("R1", tmid);
        let _ = t.activate(&mut env.ctx());
        let ticket = t.ticket.unwrap();
        let items = (0i64..)
            .zip(replies)
            .map(|(i, id)| message(id, "alice", T0 + 10 - i, Some(tmid)))
            .collect();
        let total = replies.len() as u64;
        let _ = t.update(Msg::Page(ticket, Ok(page(items, 0, total))), &mut env.ctx());
        t
    }

    let mut env = Env::new(None);
    let mut t = loaded(&mut env, "P", &["r3", "r2", "r1"]);
    let other = loaded(&mut env, "Q", &["q2", "q1"]);
    let ctx = ViewCtx {
        server_id: &env.sid,
        server: &env.server,
        rid: "R1",
        workspace: &env.ws,
        images: &env.images,
        prefs: &env.prefs,
        connected: true,
        panel: None,
        now: std::time::Instant::now(),
    };
    let mut tree = Tree::new(t.view(&ctx).as_widget());
    for sel in [Some("r2"), None, Some("P"), Some("r3"), Some("r1"), None] {
        t.selected = sel.map(|m| (m.to_string(), 1));
        let view = t.view(&ctx);
        tree.diff(view.as_widget());
        let again = t.view(&ctx);
        tree.diff(again.as_widget());
    }
    // Another thread in the same panel (first key changes, fewer rows).
    tree.diff(other.view(&ctx).as_widget());
    tree.diff(t.view(&ctx).as_widget());
}

fn view_ctx(env: &Env) -> ViewCtx<'_> {
    ViewCtx {
        server_id: &env.sid,
        server: &env.server,
        rid: "R1",
        workspace: &env.ws,
        images: &env.images,
        prefs: &env.prefs,
        connected: true,
        panel: None,
        now: std::time::Instant::now(),
    }
}

/// The trees of the keyed message list somewhere in `tree`, by row key (the
/// address of each row's widget state).
fn row_states(tree: &iced::advanced::widget::Tree) -> Option<Vec<(u64, usize)>> {
    use iced::advanced::widget::tree::State;
    if let Some(keys) = rustket_ui::widget::keyed_list_keys::<u64>(tree) {
        let rows = keys.iter().zip(&tree.children).map(|(k, t)| {
            let addr = match &t.state {
                State::Some(s) => (&raw const **s).cast::<()>() as usize,
                State::None => 0,
            };
            (*k, addr)
        });
        return Some(rows.collect());
    }
    tree.children.iter().find_map(row_states)
}

fn tags_match(tree: &iced::advanced::widget::Tree, fresh: &iced::advanced::widget::Tree) -> bool {
    tree.tag == fresh.tag
        && tree.children.len() == fresh.children.len()
        && tree
            .children
            .iter()
            .zip(&fresh.children)
            .all(|(a, b)| tags_match(a, b))
}

/// Diffs `view` over `tree` and checks the result is what a fresh tree of
/// `view` would be (no row diffed against another widget's state) and that
/// every row that was already there kept its own tree.
fn diff_keeps_rows<M>(tree: &mut iced::advanced::widget::Tree, view: &Element<'_, M>) {
    use iced::advanced::widget::Tree;
    let before = row_states(tree).unwrap();
    tree.diff(view.as_widget());
    assert!(tags_match(tree, &Tree::new(view.as_widget())));
    let after = row_states(tree).unwrap();
    assert!(after.len() > before.len(), "{before:?} -> {after:?}");
    for (key, addr) in &before {
        let kept = after.iter().find(|(k, _)| k == key).map(|(_, a)| a);
        assert_eq!(kept, Some(addr), "row {key:x}: {before:?} -> {after:?}");
    }
}

/// A new message used to make iced's keyed column diff the new rows against
/// their neighbours' trees: the panic of a `lazy` row meeting the sensor's
/// state, and (without the panic) widget state such as an open context menu
/// moving to another message.
#[test]
fn new_room_message_keeps_every_row_tree() {
    use iced::advanced::widget::Tree;
    let mut env = Env::new(None);
    env.ws.apply_message(message("a", "bob", T0, None));
    env.ws.apply_message(message("b", "bob", T0 + 1, None));
    let tl = crate::room::timeline::Timeline::new("R1");
    let mut tree = Tree::new(tl.view(&view_ctx(&env)).as_widget());
    env.ws.apply_message(message("c", "alice", T0 + 2, None));
    diff_keeps_rows(&mut tree, &tl.view(&view_ctx(&env)));
    env.ws.apply_message(message("d", "carol", T0 + 3, None));
    diff_keeps_rows(&mut tree, &tl.view(&view_ctx(&env)));
}

#[test]
fn new_thread_reply_keeps_every_row_tree() {
    use iced::advanced::widget::Tree;
    let mut env = Env::new(None);
    let mut p = message("P", "bob", T0, None);
    p.tcount = Some(0);
    env.ws.apply_message(p);
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let ticket = t.ticket.unwrap();
    let _ = t.update(Msg::Page(ticket, Ok(page(vec![], 0, 0))), &mut env.ctx());
    let mut tree = Tree::new(t.view(&view_ctx(&env)).as_widget());
    // The first reply, then another one.
    for (i, id) in [(1, "r1"), (2, "r2")] {
        env.ws
            .apply_message(message(id, "alice", T0 + i, Some("P")));
        diff_keeps_rows(&mut tree, &t.view(&view_ctx(&env)));
    }
}

/// The "jump to latest" button used to wrap the timeline's scrollable in a
/// stack only while it was shown: scrolling up, a message arriving below and
/// jumping back down each rebuilt the scrollable (offset back to the bottom)
/// and every row.
#[test]
fn jump_button_keeps_the_timeline_scrollable() {
    use crate::room::timeline::{Message as TlMsg, Timeline};
    use crate::tree_probe::{assert_kept, scrollable_tag};
    use iced::advanced::widget::Tree;
    let mut env = Env::new(None);
    env.ws.apply_message(message("a", "bob", T0, None));
    env.ws.apply_message(message("b", "bob", T0 + 1, None));
    let mut tl = Timeline::new("R1");
    // `jump_to` starts timers.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _rt = rt.enter();
    let s = scrollable_tag();
    let mut tree = Tree::new(tl.view(&view_ctx(&env)).as_widget());
    // Scrolled away from the bottom (held at a message, like a scroll up).
    let _ = tl.jump_to("a", None, &mut env.ctx());
    assert_kept(&mut tree, &tl.view(&view_ctx(&env)), s, "scrolled up");
    // A message arrives below while scrolled up.
    env.ws.apply_message(message("c", "alice", T0 + 2, None));
    let mut changes = rustket_core::workspace::Changes::default();
    changes.timelines.insert("R1".into());
    let _ = tl.on_changes(&changes, &mut env.ctx());
    assert_kept(&mut tree, &tl.view(&view_ctx(&env)), s, "new message below");
    let _ = tl.update(TlMsg::JumpToLatest, &mut env.ctx());
    assert_kept(
        &mut tree,
        &tl.view(&view_ctx(&env)),
        s,
        "back at the bottom",
    );
}

/// The same for the thread panel's list (and its composer).
#[test]
fn jump_button_keeps_the_thread_scrollable_and_editor() {
    use crate::tree_probe::{assert_kept, scrollable_tag, text_editor_tag};
    use iced::advanced::widget::Tree;
    let mut env = Env::new(None);
    env.ws.apply_message(parent(2));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let ticket = t.ticket.unwrap();
    let replies = vec![
        message("r2", "alice", T0 + 2, Some("P")),
        message("r1", "alice", T0 + 1, Some("P")),
    ];
    let _ = t.update(Msg::Page(ticket, Ok(page(replies, 0, 2))), &mut env.ctx());
    let (s, ed) = (scrollable_tag(), text_editor_tag());
    let mut tree = Tree::new(t.view(&view_ctx(&env)).as_widget());
    type Change = dyn Fn(&mut Thread, &mut Env);
    let steps: [(&str, &Change); 3] = [
        ("scrolled up", &|t, env| {
            assert!(t.follow.hold_at(env.ws.thread("P").unwrap(), "r1"));
        }),
        ("new reply below", &|_, env| {
            env.ws
                .apply_message(message("r3", "bob", T0 + 3, Some("P")));
        }),
        ("back at the bottom", &|t, env| {
            let _ = t.update(Msg::JumpToLatest, &mut env.ctx());
        }),
    ];
    for (step, change) in steps {
        let before = crate::tree_probe::states(&tree, ed);
        change(&mut t, &mut env);
        let view = t.view(&view_ctx(&env));
        assert_kept(&mut tree, &view, s, step);
        assert_eq!(crate::tree_probe::states(&tree, ed), before, "{step}");
    }
}

/// Review: showing a reply of the thread that is already open did not
/// scroll to it, and the leftover focus jumped the view later.
#[tokio::test]
async fn reply_of_the_open_thread_is_revealed_right_away() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(4));
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let first = t.ticket.unwrap();
    let replies = vec![
        message("r3", "bob", T0 + 4, Some("P")),
        message("r2", "bob", T0 + 3, Some("P")),
    ];
    let _ = t.update(Msg::Page(first, Ok(page(replies, 0, 4))), &mut env.ctx());
    assert!(t.follow.at_bottom && t.selected.is_none());
    let _ = t.focus_reply("r2", &mut env.ctx());
    assert_eq!(t.selected.as_ref().map(|(m, _)| m.as_str()), Some("r2"));
    assert!(t.focus.is_none() && !t.follow.at_bottom);
    // Not loaded yet: older pages are requested until it is there.
    let _ = t.focus_reply("r0", &mut env.ctx());
    let second = t.ticket.unwrap();
    assert_ne!(first, second, "an older page is requested");
    let older = vec![message("r1", "bob", T0 + 2, Some("P"))];
    let _ = t.update(Msg::Page(second, Ok(page(older, 2, 4))), &mut env.ctx());
    let third = t.ticket.unwrap();
    assert_ne!(second, third, "still not there: the next page");
    let older = vec![message("r0", "bob", T0 + 1, Some("P"))];
    let _ = t.update(Msg::Page(third, Ok(page(older, 3, 4))), &mut env.ctx());
    assert_eq!(t.selected.as_ref().map(|(m, _)| m.as_str()), Some("r0"));
    // A reply that is not in the (fully loaded) thread is forgotten.
    let _ = t.focus_reply("gone", &mut env.ctx());
    assert!(t.focus.is_none());
}

/// Review: the first page (also reloaded after a reset) marked the thread
/// read while the room was not on screen.
#[test]
fn hidden_thread_is_not_read_by_its_first_page() {
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut sub = env.ws.subscription("R1").unwrap().clone();
    sub.tunread = Some(vec!["P".into()]);
    env.ws
        .apply_subscription(rustket_core::workspace::Change::Updated, sub);
    let mut t = Thread::new("R1", "P");
    let _ = t.activate(&mut env.ctx());
    let first = t.ticket.unwrap();
    let replies = vec![message("r1", "bob", T0 + 1, Some("P"))];
    let _ = t.update(Msg::Page(first, Ok(page(replies, 0, 1))), &mut env.ctx());
    assert!(!t.reading, "hidden: not read");
    t.set_visible(true);
    let _ = t.mark_read(&env.ctx());
    assert!(t.reading, "read once shown");
}

#[test]
fn keep_toggle_keeps_or_removes_the_thread() {
    use rustket_core::config::ThreadsMode;
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    let mut t = Thread::new("R1", "P");
    // Only offered while the sidebar lists kept threads.
    let a = t.update(Msg::ToggleKeep, &mut env.ctx());
    assert!(a.effects.is_empty());
    env.ws.set_thread_config(ThreadsMode::UnreadKept, []);
    let a = t.update(Msg::ToggleKeep, &mut env.ctx());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::KeepThread { rid, tmid, keep: true }] if rid == "R1" && tmid == "P"
    ));
    env.ws
        .set_thread_config(ThreadsMode::UnreadKept, [("R1", "P")]);
    let a = t.update(Msg::ToggleKeep, &mut env.ctx());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::KeepThread { keep: false, .. }]
    ));
}

/// Review: the header offered Keep in a previewed (unsubscribed) room, where
/// a kept thread is never listed; it now matches the message menu.
#[test]
fn keep_toggle_needs_a_subscription_unless_already_kept() {
    use rustket_core::config::ThreadsMode;
    let mut env = Env::new(None);
    env.ws.apply_message(parent(1));
    env.ws.set_thread_config(ThreadsMode::UnreadKept, []);
    let t = Thread::new("R1", "P");
    let subscribed = t.header_actions(&view_ctx(&env)).len();
    let sub = env.ws.subscription("R1").unwrap().clone();
    env.ws
        .apply_subscription(rustket_core::workspace::Change::Removed, sub);
    assert!(env.ws.subscription("R1").is_none());
    let mut t = Thread::new("R1", "P");
    assert_eq!(
        t.header_actions(&view_ctx(&env)).len(),
        subscribed - 1,
        "no keep button"
    );
    let a = t.update(Msg::ToggleKeep, &mut env.ctx());
    assert!(a.effects.is_empty(), "no keep effect");
    // A thread kept earlier can still be removed.
    env.ws
        .set_thread_config(ThreadsMode::UnreadKept, [("R1", "P")]);
    assert_eq!(t.header_actions(&view_ctx(&env)).len(), subscribed);
    let a = t.update(Msg::ToggleKeep, &mut env.ctx());
    assert!(matches!(
        a.effects.as_slice(),
        [Effect::KeepThread { keep: false, .. }]
    ));
}
