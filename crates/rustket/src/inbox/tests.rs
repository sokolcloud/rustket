#![allow(clippy::unwrap_used)]
use std::time::{Duration, Instant};

use rocketchat::RcDate;
use rocketchat::models::{
    History, Mention, Message as RcMessage, Room, RoomType, Subscription, User, UserRef,
};
use rustket_core::reads::ReadReq;
use rustket_core::{Change, Workspace};

use super::*;

fn ts(ms: i64) -> RcDate {
    RcDate::from_millis(ms).unwrap()
}

fn sub(rid: &str, t: RoomType) -> Subscription {
    Subscription {
        id: format!("s-{rid}"),
        rid: rid.into(),
        name: rid.into(),
        t,
        u: UserRef {
            id: "me".into(),
            username: Some("me".into()),
            name: None,
        },
        open: true,
        alert: true,
        ls: Some(ts(100)),
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
            id: "bob".into(),
            username: Some("bob".into()),
            name: None,
        },
        ..Default::default()
    }
}

fn ws_with(subs: Vec<Subscription>) -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(User {
        id: "me".into(),
        username: Some("me".into()),
        ..Default::default()
    });
    for s in subs {
        let r = Room {
            id: s.rid.clone(),
            t: s.t,
            name: Some(s.rid.clone()),
            lm: Some(ts(500)),
            ..Default::default()
        };
        ws.apply_subscription(Change::Inserted, s);
        ws.apply_room(Change::Inserted, r);
    }
    ws
}

fn reads(out: &[Out]) -> Vec<ReadReq> {
    out.iter()
        .filter_map(|o| match o {
            Out::Read(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn room_read(rid: &str) -> ReadReq {
    ReadReq::Room {
        rid: rid.into(),
        threads: false,
    }
}

#[test]
fn catch_up_reads_skips_and_goes_back() {
    let ws = ws_with(vec![
        sub("a", RoomType::Channel),
        sub("b", RoomType::Channel),
    ]);
    let mut st = InboxState::default();
    let mut s = InboxScreen::new(InboxKind::Unreads);
    let _ = s.update(Msg::StartCatchUp, &ws, &mut st, false, true);
    assert!(s.is_catch_up() && s.shows(InboxKind::Unreads));
    let first = s.catch_up().unwrap().current().unwrap().to_owned();
    let out = s.update(Msg::Key(CatchUpKey::Enter), &ws, &mut st, false, true);
    // The room is read without its threads (they stay for Activity).
    assert_eq!(reads(&out), [room_read(&first)]);
    assert!(s.is_marking(&first));
    let out = s.update(Msg::Key(CatchUpKey::Right), &ws, &mut st, false, true);
    assert!(reads(&out).is_empty());
    assert!(s.catch_up().unwrap().is_done());
    let _ = s.update(Msg::Key(CatchUpKey::Left), &ws, &mut st, false, true);
    assert!(s.catch_up().unwrap().current().is_some());
    // Offline: nothing is sent, the room is only skipped.
    let out = s.update(Msg::ReadNext, &ws, &mut st, false, false);
    assert!(reads(&out).is_empty());
    assert_eq!(s.catch_up().unwrap().skipped_count(), 1);
}

#[test]
fn mark_all_in_all_unreads_reads_the_listed_rooms_without_threads() {
    let mut muted = sub("muted", RoomType::Channel);
    muted.disable_notifications = Some(true);
    let ws = ws_with(vec![
        sub("a", RoomType::Channel),
        sub("b", RoomType::Private),
        muted,
    ]);
    let mut st = InboxState::default();
    st.skipped.skip("b", ws.room("b").unwrap().last_activity());
    let mut s = InboxScreen::new(InboxKind::Unreads);
    let out = s.update(Msg::MarkAll, &ws, &mut st, false, true);
    // Skipped and muted rooms are not listed, so they stay.
    assert_eq!(reads(&out), [room_read("a")]);
    assert!(s.rooms(&ws, &st, false).is_empty(), "hidden while marking");
    // A failure shows the room again; a success keeps it hidden a while.
    s.read_done(&room_read("a"), false, Instant::now());
    assert_eq!(s.rooms(&ws, &st, false).len(), 1);
    let _ = s.update(Msg::MarkRead("a".into()), &ws, &mut st, false, true);
    let t0 = Instant::now();
    s.read_done(&room_read("a"), true, t0);
    s.tick(&ws, &st, false, t0);
    assert!(s.is_marking("a"));
    s.tick(&ws, &st, false, t0 + Duration::from_secs(10));
    assert!(!s.is_marking("a"));
    // Offline: nothing.
    assert!(
        s.update(Msg::MarkAll, &ws, &mut st, false, false)
            .is_empty()
    );
}

#[test]
fn mark_all_in_activity_reads_mention_rooms_and_threads() {
    let mut a = sub("a", RoomType::Channel);
    a.user_mentions = 1;
    let mut b = sub("b", RoomType::Channel);
    b.alert = false;
    b.tunread = Some(vec!["t1".into()]);
    let ws = ws_with(vec![a, b]);
    let mut st = InboxState::default();
    let mut s = InboxScreen::new(InboxKind::Activity);
    let out = s.update(Msg::MarkAll, &ws, &mut st, false, true);
    let r = reads(&out);
    assert!(r.contains(&room_read("a")));
    assert!(r.contains(&ReadReq::Thread {
        rid: "b".into(),
        tmid: "t1".into()
    }));
    assert_eq!(r.len(), 2);
    assert!(s.activity(&ws, false).is_empty(), "hidden while marking");
}

#[test]
fn reveal_uses_the_local_copy() {
    let mut a = sub("a", RoomType::Channel);
    a.user_mentions = 1;
    let mut ws = ws_with(vec![a]);
    let mut m = msg("m1", "a", 200);
    m.mentions = Some(vec![Mention {
        id: "me".into(),
        username: Some("me".into()),
        ..Default::default()
    }]);
    let req = ws.begin_feed("a", false).unwrap();
    ws.finish_feed(
        &req,
        Ok(History {
            messages: vec![m],
            raw_len: 1,
            ..Default::default()
        }),
    );
    let mut st = InboxState::default();
    let mut s = InboxScreen::new(InboxKind::Activity);
    let out = s.update(
        Msg::Reveal {
            rid: "a".into(),
            mid: "m1".into(),
        },
        &ws,
        &mut st,
        false,
        true,
    );
    assert!(matches!(&out[..], [Out::Reveal { m, .. }] if m.id == "m1"));
    let out = s.update(
        Msg::Reveal {
            rid: "a".into(),
            mid: "gone".into(),
        },
        &ws,
        &mut st,
        false,
        true,
    );
    assert!(matches!(&out[..], [Out::OpenRoom(r)] if r == "a"));
}

#[test]
fn wanted_follows_the_view() {
    let mut subs: Vec<Subscription> = (0..40)
        .map(|i| sub(&format!("r{i:02}"), RoomType::Channel))
        .collect();
    subs[0].user_mentions = 1;
    subs[1].tunread = Some(vec!["t".into()]);
    subs[1].tunread_user = Some(vec!["t".into()]);
    let ws = ws_with(subs);
    let st = InboxState::default();
    let mut s = InboxScreen::new(InboxKind::Unreads);
    let w = s.wanted(&ws, &st, false);
    assert_eq!(w.feeds.len(), ROOMS_PAGE);
    let mut st2 = InboxState::default();
    let _ = s.update(Msg::LoadMoreRooms, &ws, &mut st2, false, true);
    let _ = s.update(Msg::LoadEarlier("r05".into()), &ws, &mut st2, false, true);
    let w = s.wanted(&ws, &st, false);
    assert_eq!(w.feeds.len(), 40);
    assert_eq!(w.earlier, ["r05"]);
    s.earlier_done("r05");
    let _ = s.update(Msg::StartCatchUp, &ws, &mut st2, false, true);
    assert_eq!(s.wanted(&ws, &st, false).feeds.len(), PREFETCH);
    let _ = s.update(Msg::Open(InboxKind::Activity), &ws, &mut st2, false, true);
    let w = s.wanted(&ws, &st, false);
    assert_eq!(w.feeds, ["r00"]);
    assert_eq!(w.thread_mentions, ["r01"]);
    assert!(s.shows(InboxKind::Activity) && !s.shows(InboxKind::Unreads));
}

#[test]
fn catch_up_keys_need_no_modifiers() {
    use iced::keyboard::key::Named;
    use iced::keyboard::{Key, Modifiers};
    let none = Modifiers::empty();
    assert_eq!(
        CatchUpKey::of(&Key::Named(Named::Enter), none),
        Some(CatchUpKey::Enter)
    );
    assert_eq!(
        CatchUpKey::of(&Key::Named(Named::ArrowRight), none),
        Some(CatchUpKey::Right)
    );
    assert_eq!(
        CatchUpKey::of(&Key::Named(Named::ArrowLeft), none),
        Some(CatchUpKey::Left)
    );
    assert_eq!(
        CatchUpKey::of(&Key::Named(Named::Enter), Modifiers::SHIFT),
        None
    );
    assert_eq!(CatchUpKey::of(&Key::Character("a".into()), none), None);
}
