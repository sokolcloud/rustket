#![allow(clippy::unwrap_used)]

use rocketchat::RcDate;
use rocketchat::models::{
    IdRef, Message as RcMessage, Room, RoomType, Subscription, User, UserRef,
};
use rustket_core::Workspace;
use rustket_core::later::{LaterEdit, LaterStatus};
use rustket_core::reminders::ReminderEdit;
use rustket_core::sync::InitialData;
use rustket_core::workspace::LaterTab;

use super::*;

fn ws() -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_initial(InitialData {
        me: User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        },
        subscriptions: vec![Subscription {
            id: "s1".into(),
            rid: "r1".into(),
            name: "general".into(),
            t: RoomType::Channel,
            open: true,
            ..Default::default()
        }],
        rooms: vec![Room {
            id: "r1".into(),
            t: RoomType::Channel,
            name: Some("general".into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    ws.set_later_tracking(true);
    ws
}

fn starred(id: &str, ts: i64) -> RcMessage {
    RcMessage {
        id: id.into(),
        rid: "r1".into(),
        msg: format!("text {id}"),
        ts: RcDate::from_millis(ts),
        u: UserRef {
            id: "u".into(),
            username: Some("u".into()),
            name: None,
        },
        starred: Some(vec![IdRef { id: "me".into() }]),
        ..Default::default()
    }
}

fn ctx(ws: &Workspace, online: bool) -> UpdateCtx<'_> {
    UpdateCtx {
        workspace: ws,
        now: chrono::Local::now(),
        morning: 9,
        online,
    }
}

#[test]
fn keys_move_open_and_toggle_done() {
    let mut ws = ws();
    assert!(ws.later_tracking(), "starring is allowed by default");
    ws.apply_message(starred("a", 1_800_000_000_000));
    ws.apply_message(starred("b", 1_800_000_100_000));
    let mut v = LaterView::new(LaterTab::InProgress);
    // Newest first: b, a; the list scrolls to the selection.
    assert!(matches!(
        &v.update(Msg::Key(LaterKey::Down), &ctx(&ws, true))[..],
        [Out::Scroll(mid)] if mid == "b"
    ));
    assert_eq!(v.selected.as_deref(), Some("b"));
    v.update(Msg::Key(LaterKey::Down), &ctx(&ws, true));
    v.update(Msg::Key(LaterKey::Down), &ctx(&ws, true));
    assert_eq!(v.selected.as_deref(), Some("a"));
    let outs = v.update(Msg::Key(LaterKey::Enter), &ctx(&ws, true));
    assert!(matches!(&outs[..], [Out::Open { mid, rid, .. }] if mid == "a" && rid == "r1"));
    let outs = v.update(Msg::Key(LaterKey::E), &ctx(&ws, true));
    assert!(matches!(
        &outs[..],
        [Out::Later(LaterEdit::Status {
            status: LaterStatus::Done,
            ..
        })]
    ));
    v.update(Msg::Key(LaterKey::Up), &ctx(&ws, true));
    assert_eq!(v.selected.as_deref(), Some("b"));
    // Close is the app's.
    assert!(matches!(
        &v.update(Msg::Close, &ctx(&ws, true))[..],
        [Out::Close]
    ));
}

#[test]
fn unstarring_needs_a_connection_and_done_items_clear_after_confirming() {
    let mut ws = ws();
    ws.apply_message(starred("a", 1_800_000_000_000));
    let mut v = LaterView::new(LaterTab::InProgress);
    assert!(
        v.update(Msg::Unstar("a".into()), &ctx(&ws, false))
            .is_empty()
    );
    assert!(matches!(
        &v.update(Msg::Unstar("a".into()), &ctx(&ws, true))[..],
        [Out::Unstar(items)] if items == &[("r1".to_owned(), "a".to_owned())]
    ));
    // Done items: confirm first.
    let mut overlay = ws.later_overlay().clone();
    overlay
        .edit(
            ws.later_blob(),
            &LaterEdit::Status {
                rid: "r1".into(),
                mid: "a".into(),
                ts_ms: None,
                status: LaterStatus::Done,
            },
            1,
            "dev",
        )
        .unwrap();
    ws.set_later_overlay(overlay);
    assert_eq!(ws.later_counts(), [0, 0, 1, 0]);
    v.update(Msg::Tab(LaterTab::Done), &ctx(&ws, true));
    assert!(v.update(Msg::ClearDone, &ctx(&ws, true)).is_empty());
    assert!(matches!(v.dialog, Some(Dialog::ClearDone(_))));
    assert!(v.has_dialog());
    let outs = v.update(Msg::ConfirmClearDone, &ctx(&ws, true));
    assert!(matches!(&outs[..], [Out::Unstar(items)] if items.len() == 1));
    assert!(v.dialog.is_none());
    // Keys do nothing behind a dialog.
    v.update(Msg::ClearDone, &ctx(&ws, true));
    assert!(
        v.update(Msg::Key(LaterKey::Down), &ctx(&ws, true))
            .is_empty()
    );
    assert!(v.selected.is_none());
}

#[test]
fn reminder_and_due_dialogs_emit_edits() {
    let mut ws = ws();
    ws.apply_message(starred("a", 1_800_000_000_000));
    let mut v = LaterView::new(LaterTab::InProgress);
    v.update(Msg::Remind("a".into()), &ctx(&ws, true));
    assert!(matches!(v.dialog, Some(Dialog::Remind { .. })));
    let outs = v.update(
        Msg::Picker(when::Msg::Preset(rustket_core::reminders::Preset::In20Min)),
        &ctx(&ws, true),
    );
    assert!(matches!(
        &outs[..],
        [Out::Reminder(ReminderEdit::Set { mid, rid, .. })] if mid == "a" && rid == "r1"
    ));
    assert!(v.dialog.is_none());
    // A due date with "also remind me".
    v.update(Msg::Due("a".into()), &ctx(&ws, true));
    v.update(Msg::AlsoRemind(true), &ctx(&ws, true));
    let outs = v.update(
        Msg::Picker(when::Msg::Due(when::DuePreset::Tomorrow)),
        &ctx(&ws, true),
    );
    assert!(matches!(
        &outs[..],
        [
            Out::Later(LaterEdit::Due { at_ms: Some(_), .. }),
            Out::Reminder(ReminderEdit::Set { .. })
        ]
    ));
    // "No due date".
    v.update(Msg::Due("a".into()), &ctx(&ws, true));
    let outs = v.update(Msg::Picker(when::Msg::Clear), &ctx(&ws, true));
    assert!(matches!(
        &outs[..],
        [Out::Later(LaterEdit::Due { at_ms: None, .. })]
    ));
    // A message gone (unstarred, no reminder) closes its dialog.
    v.update(Msg::Remind("a".into()), &ctx(&ws, true));
    let mut gone = starred("a", 1_800_000_000_000);
    gone.starred = Some(Vec::new());
    ws.apply_message(gone);
    v.prune(&ws);
    assert!(v.dialog.is_none());
}
