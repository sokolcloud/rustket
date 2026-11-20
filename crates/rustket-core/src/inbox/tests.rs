#![allow(clippy::unwrap_used)]
use std::time::{Duration, Instant};

use rocketchat::models::{History, Mention, Message, Room, RoomType, Subscription};

use super::catch_up::{Advance, CatchUp};
use super::feeds::{FEED_PAGE, FeedState};
use super::load::{self, Fetch, LoadFailure, LoadState, Next, Wanted};
use super::*;
use crate::test_util::{me, msg, room, sub, ts};
use crate::workspace::{Change, ConnectionState, Dirty, Workspace};

/// A workspace of user `me` with these rooms (room `lm` set from `lm`).
fn ws_with(entries: Vec<(Subscription, Option<i64>)>) -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    for (s, lm) in entries {
        let mut r: Room = room(&s.rid, &s.name, s.t);
        r.lm = lm.map(ts);
        ws.apply_subscription(Change::Inserted, s);
        ws.apply_room(Change::Inserted, r);
    }
    ws
}

fn unread_sub(rid: &str, t: RoomType, ls: i64) -> Subscription {
    let mut s = sub(rid, rid, t);
    s.alert = true;
    s.ls = Some(ts(ls));
    s
}

fn rids(v: &[UnreadRoom]) -> Vec<&str> {
    v.iter().map(|r| r.rid.as_str()).collect()
}

fn mention(username: &str) -> Mention {
    Mention {
        id: format!("id-{username}"),
        username: Some(username.into()),
        ..Default::default()
    }
}

fn history(msgs: Vec<Message>, raw: usize, not_loaded: Option<u64>) -> History {
    // Newest first, as the server sends them.
    let mut messages = msgs;
    messages.sort_by_key(|a| std::cmp::Reverse(a.ts));
    History {
        messages,
        unread_not_loaded: not_loaded,
        raw_len: raw,
        ..Default::default()
    }
}

#[test]
fn unread_rooms_use_alert_not_unread() {
    // Channels count only mentions in `unread`: plain unread sets `alert`.
    let mut quiet = unread_sub("quiet", RoomType::Private, 100);
    quiet.alert = false;
    let ws = ws_with(vec![
        (unread_sub("plain", RoomType::Private, 100), Some(200)),
        (quiet, Some(200)),
    ]);
    let v = unread_rooms(&ws, &InboxPrefs::default(), &Skipped::default());
    assert_eq!(rids(&v), ["plain"]);
    assert_eq!(v[0].tier, Tier::Unread);
}

#[test]
fn thread_only_rooms_are_not_listed() {
    let mut threads = unread_sub("threads", RoomType::Channel, 300);
    threads.tunread = Some(vec!["t1".into()]);
    // No `alert`, only unread threads.
    let mut tunread_only = sub("tonly", "tonly", RoomType::Channel);
    tunread_only.tunread = Some(vec!["t2".into()]);
    let ws = ws_with(vec![(threads, Some(200)), (tunread_only, Some(200))]);
    assert!(unread_rooms(&ws, &InboxPrefs::default(), &Skipped::default()).is_empty());
    assert_eq!(counts(&ws).unread_rooms, 0);
    assert_eq!(counts(&ws).threads, 2);
}

/// M2: DMs are in the mention tier, unless mention status is hidden (R3:
/// plain unread) or notifications are off (R1: muted).
#[test]
fn dm_tier_follows_r1_and_r3() {
    let dm = unread_sub("dm", RoomType::Direct, 1);
    let mut r3 = unread_sub("r3", RoomType::Direct, 1);
    r3.hide_mention_status = Some(true);
    let mut r1 = unread_sub("r1", RoomType::Direct, 1);
    r1.disable_notifications = Some(true);
    let ws = ws_with(vec![(dm, Some(5)), (r3, Some(6)), (r1, Some(7))]);
    let all = InboxPrefs {
        include_muted: true,
    };
    let v = unread_rooms(&ws, &all, &Skipped::default());
    let tiers: Vec<(&str, Tier)> = v.iter().map(|r| (r.rid.as_str(), r.tier)).collect();
    assert_eq!(
        tiers,
        [
            ("dm", Tier::Mention),
            ("r3", Tier::Unread),
            ("r1", Tier::Muted)
        ]
    );
}

#[test]
fn tiers_order_mentions_dm_group_plain_muted() {
    let mut mention_s = unread_sub("mention", RoomType::Channel, 1);
    mention_s.unread = 1;
    mention_s.user_mentions = 1;
    let mut dm = unread_sub("dm", RoomType::Direct, 1);
    dm.unread = 2;
    let mut group = unread_sub("group", RoomType::Channel, 1);
    group.unread = 1;
    group.group_mentions = 1;
    let mut group_muted = unread_sub("gmuted", RoomType::Channel, 1);
    group_muted.group_mentions = 1;
    group_muted.mute_group_mentions = Some(true);
    let mut muted = unread_sub("muted", RoomType::Channel, 1);
    muted.disable_notifications = Some(true);
    let mut hidden_status = unread_sub("hidden", RoomType::Channel, 1);
    hidden_status.hide_unread_status = Some(true);
    let mut closed = unread_sub("closed", RoomType::Channel, 1);
    closed.open = false;
    let mut archived = unread_sub("archived", RoomType::Channel, 1);
    archived.archived = Some(true);
    let livechat = unread_sub("livechat", RoomType::Livechat, 1);
    let ws = ws_with(vec![
        (unread_sub("plain_old", RoomType::Channel, 1), Some(10)),
        (unread_sub("plain_new", RoomType::Channel, 1), Some(20)),
        (mention_s, Some(5)),
        (dm, Some(6)),
        (group, Some(7)),
        (group_muted, Some(8)),
        (muted, Some(9)),
        (hidden_status, Some(9)),
        (closed, Some(9)),
        (archived, Some(9)),
        (livechat, Some(9)),
    ]);
    let v = unread_rooms(&ws, &InboxPrefs::default(), &Skipped::default());
    assert_eq!(
        rids(&v),
        ["dm", "mention", "group", "plain_new", "plain_old", "gmuted"]
    );
    let with_muted = unread_rooms(
        &ws,
        &InboxPrefs {
            include_muted: true,
        },
        &Skipped::default(),
    );
    assert_eq!(with_muted.last().unwrap().rid, "muted");
    assert_eq!(with_muted.last().unwrap().tier, Tier::Muted);
}

#[test]
fn skipped_room_returns_after_new_message() {
    let mut ws = ws_with(vec![(unread_sub("a", RoomType::Channel, 1), Some(10))]);
    let mut skipped = Skipped::default();
    let lm = ws.room("a").unwrap().last_activity();
    skipped.skip("a", lm);
    assert!(unread_rooms(&ws, &InboxPrefs::default(), &skipped).is_empty());
    let mut r = room("a", "a", RoomType::Channel);
    r.lm = Some(ts(20));
    r.updated_at = Some(ts(20));
    ws.apply_room(Change::Updated, r);
    assert_eq!(
        rids(&unread_rooms(&ws, &InboxPrefs::default(), &skipped)),
        ["a"]
    );
    assert!(!skipped.is_skipped("a", Some(ts(20))));
}

#[test]
fn counts_follow_the_mention_rules() {
    let mut a = unread_sub("a", RoomType::Channel, 1);
    a.user_mentions = 2;
    a.group_mentions = 1;
    a.tunread = Some(vec!["t1".into()]);
    a.tunread_user = Some(vec!["t1".into()]);
    let mut hidden = unread_sub("h", RoomType::Channel, 1);
    hidden.user_mentions = 5;
    hidden.hide_mention_status = Some(true);
    let mut dm = unread_sub("d", RoomType::Direct, 1);
    dm.unread = 3;
    dm.user_mentions = 3;
    let mut mute_group = unread_sub("g", RoomType::Channel, 1);
    mute_group.group_mentions = 4;
    mute_group.mute_group_mentions = Some(true);
    let mut muted = unread_sub("m", RoomType::Channel, 1);
    muted.disable_notifications = Some(true);
    let ws = ws_with(vec![
        (a, Some(10)),
        (hidden, Some(10)),
        (dm, Some(10)),
        (mute_group, Some(10)),
        (muted, Some(10)),
    ]);
    let c = counts(&ws);
    // a: 2 + its thread mention; DMs are All unreads' business.
    assert_eq!(c.mentions, 3);
    assert_eq!(c.group_mentions, 1);
    assert_eq!(c.threads, 1);
    // a, h, d, g; the muted room aside.
    assert_eq!(c.unread_rooms, 4);
    let per = |rid: &str| entry_counts(ws.room(rid).unwrap());
    assert_eq!(
        per("a"),
        InboxCounts {
            unread_rooms: 1,
            mentions: 3,
            group_mentions: 1,
            threads: 1,
        }
    );
    assert_eq!(per("h").mentions, 0);
    assert_eq!(per("d").mentions, 0);
    assert_eq!(per("g").group_mentions, 0);
    assert_eq!(per("m").unread_rooms, 0);
    let sum =
        ["a", "h", "d", "g", "m"]
            .iter()
            .map(|r| per(r))
            .fold(InboxCounts::default(), |x, y| InboxCounts {
                unread_rooms: x.unread_rooms + y.unread_rooms,
                mentions: x.mentions + y.mentions,
                group_mentions: x.group_mentions + y.group_mentions,
                threads: x.threads + y.threads,
            });
    assert_eq!(sum, c);
}

#[test]
fn mention_predicates() {
    let mut m = msg("m", "r", 1, "bob");
    assert!(!mentions_me(&m, "me", "me"));
    assert!(!is_group_mention(&m));
    m.mentions = Some(vec![mention("me")]);
    assert!(mentions_me(&m, "me", "nobody"));
    m.mentions = Some(vec![Mention {
        id: "all".into(),
        username: Some("all".into()),
        ..Default::default()
    }]);
    assert!(is_group_mention(&m));
    assert!(!mentions_me(&m, "me", "me"));
    m.mentions = Some(vec![Mention {
        id: "here".into(),
        ..Default::default()
    }]);
    assert!(is_group_mention(&m));
}

/// Loads room `rid`'s feed with `msgs` (one page, nothing older).
fn load_feed(ws: &mut Workspace, rid: &str, msgs: Vec<Message>) {
    let req = ws.begin_feed(rid, false).unwrap();
    let n = msgs.len();
    ws.finish_feed(&req, Ok(history(msgs, n, Some(0))));
}

#[test]
fn feeds_keep_unread_main_timeline_messages() {
    let mut ws = ws_with(vec![(unread_sub("r", RoomType::Channel, 100), Some(300))]);
    let mut reply = msg("reply", "r", 250, "bob");
    reply.tmid = Some("t".into());
    let mut shown = msg("shown", "r", 260, "bob");
    shown.tmid = Some("t".into());
    shown.tshow = Some(true);
    ws.take_changes();
    load_feed(
        &mut ws,
        "r",
        vec![
            msg("old", "r", 100, "bob"),
            msg("a", "r", 200, "bob"),
            reply,
            shown,
            msg("b", "r", 300, "bob"),
        ],
    );
    assert!(ws.take_changes().areas.contains(Dirty::FEEDS));
    let f = ws.feeds().feed("r").unwrap();
    let ids: Vec<&str> = f.messages().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["a", "shown", "b"]);
    assert!(f.is_loaded() && !f.has_earlier());
    // Live: appended, a read message ignored, hidden removed, deleted removed.
    ws.apply_message(msg("c", "r", 400, "bob"));
    ws.apply_message(msg("before_ls", "r", 50, "bob"));
    let mut hidden = msg("a", "r", 200, "bob");
    hidden.hidden = Some(true);
    hidden.updated_at = Some(ts(500));
    ws.apply_message(hidden);
    ws.apply_message_deleted("r", "shown");
    let ids: Vec<String> = ws
        .feeds()
        .feed("r")
        .unwrap()
        .messages()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(ids, ["b", "c"]);
    // Rooms without a feed get nothing.
    ws.apply_message(msg("x", "other", 400, "bob"));
    assert!(ws.feeds().feed("other").is_none());
    // `ls` moved (read elsewhere): read messages leave.
    let mut s = unread_sub("r", RoomType::Channel, 300);
    s.updated_at = Some(ts(10_000));
    ws.apply_subscription(Change::Updated, s);
    let ids: Vec<&str> = ws
        .feeds()
        .feed("r")
        .unwrap()
        .messages()
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["c"]);
}

#[test]
fn feeds_page_earlier_and_dedup_the_boundary() {
    let mut ws = ws_with(vec![(unread_sub("r", RoomType::Channel, 0), Some(1000))]);
    let page: Vec<Message> = (0..FEED_PAGE as i64)
        .map(|i| msg(&format!("n{i}"), "r", 500 + i, "bob"))
        .collect();
    let req = ws.begin_feed("r", false).unwrap();
    assert!(ws.begin_feed("r", false).is_none(), "one at a time");
    ws.finish_feed(&req, Ok(history(page, FEED_PAGE as usize, Some(5))));
    let f = ws.feeds().feed("r").unwrap();
    assert_eq!(f.earlier(), 5);
    assert!(f.has_earlier());
    let req = ws.begin_feed("r", true).unwrap();
    assert_eq!(req.before, Some(ts(500)));
    assert!(ws.begin_feed("r", true).is_none());
    // The inclusive bound repeats n0.
    let older = vec![
        msg("o1", "r", 497, "bob"),
        msg("o2", "r", 498, "bob"),
        msg("n0", "r", 500, "bob"),
    ];
    ws.finish_feed(&req, Ok(history(older, 3, Some(0))));
    let f = ws.feeds().feed("r").unwrap();
    assert_eq!(f.messages().len(), FEED_PAGE as usize + 2);
    assert_eq!(f.messages()[0].id, "o1");
    assert!(!f.has_earlier());
    // A stale result (old ticket) is dropped.
    ws.finish_feed(&req, Ok(history(vec![msg("late", "r", 1, "x")], 1, None)));
    assert!(ws.feeds().feed("r").unwrap().messages()[0].id == "o1");
}

#[test]
fn feed_failures_retry_or_give_up() {
    let mut ws = ws_with(vec![(unread_sub("r", RoomType::Channel, 0), Some(1000))]);
    let req = ws.begin_feed("r", false).unwrap();
    ws.finish_feed(&req, Err(None));
    assert_eq!(ws.feeds().feed("r").unwrap().state(), &FeedState::Idle);
    let req = ws.begin_feed("r", false).unwrap();
    ws.finish_feed(&req, Err(Some("denied".into())));
    assert_eq!(
        ws.feeds().feed("r").unwrap().state(),
        &FeedState::Failed("denied".into())
    );
    ws.reset_feed("r");
    assert_eq!(ws.feeds().feed("r").unwrap().state(), &FeedState::Idle);
}

#[test]
fn feeds_go_stale_and_join_rewrites() {
    let mut ws = ws_with(vec![(unread_sub("r", RoomType::Channel, 0), Some(1000))]);
    ws.apply_connection(ConnectionState::Connected);
    load_feed(&mut ws, "r", vec![msg("a", "r", 10, "bob")]);
    assert!(ws.rooms_with_timelines().contains("r"));
    ws.take_changes();
    assert!(ws.rewrite_messages("r", &mut |m| {
        m.msg = m.msg.to_uppercase();
        true
    }));
    assert!(ws.take_changes().areas.contains(Dirty::FEEDS));
    assert_eq!(ws.feeds().feed("r").unwrap().messages()[0].msg, "TEXT A");
    ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
    assert!(ws.feeds().feed("r").unwrap().is_stale());
    // Leaving the room drops its feed.
    ws.apply_subscription(Change::Removed, unread_sub("r", RoomType::Channel, 0));
    assert!(ws.feeds().feed("r").is_none());
}

#[test]
fn activity_lists_mentions_then_threads() {
    let mut a = unread_sub("a", RoomType::Channel, 100);
    a.user_mentions = 1;
    a.group_mentions = 1;
    let mut b = unread_sub("b", RoomType::Channel, 100);
    b.group_mentions = 1;
    b.mute_group_mentions = Some(true);
    let mut c = unread_sub("c", RoomType::Channel, 100);
    c.user_mentions = 1;
    c.tunread = Some(vec!["t1".into(), "t2".into()]);
    c.tunread_user = Some(vec!["t1".into()]);
    let mut ws = ws_with(vec![(a, Some(500)), (b, Some(500)), (c, Some(400))]);
    let mut direct = msg("direct", "a", 300, "bob");
    direct.mentions = Some(vec![mention("me")]);
    let mut all = msg("all", "a", 200, "bob");
    all.mentions = Some(vec![Mention {
        id: "all".into(),
        username: Some("all".into()),
        ..Default::default()
    }]);
    let mut read = msg("read", "a", 50, "bob");
    read.mentions = Some(vec![mention("me")]);
    load_feed(
        &mut ws,
        "a",
        vec![read, all, direct, msg("plain", "a", 250, "bob")],
    );
    // Room c's feed is not loaded: a placeholder; its thread t1 (a direct
    // mention) stands in among the mentions until the mention is loaded.
    let kinds = |items: &[ActivityItem]| -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                ActivityItem::Mention { mid, group, .. } => format!("m:{mid}:{group}"),
                ActivityItem::RoomMentions { rid, pending, .. } => {
                    format!("room:{rid}:{pending:?}")
                }
                ActivityItem::ThreadMention { tmid, .. } => format!("tm:{tmid}"),
                ActivityItem::MentionedThread(t) => format!("mt:{}", t.tmid),
                ActivityItem::Thread(t) => format!("t:{}", t.tmid),
            })
            .collect()
    };
    let items = activity_items(&ws, ActivityFilter::All, false);
    assert_eq!(
        kinds(&items),
        [
            "m:direct:false",
            "m:all:true",
            "mt:t1",
            "room:c:Loading",
            "t:t2"
        ]
    );
    assert_eq!(
        kinds(&activity_items(&ws, ActivityFilter::Mentions, false)),
        ["m:direct:false", "m:all:true", "mt:t1", "room:c:Loading"]
    );
    // A feed given up shows its error (and Retry), not a spinner forever.
    let req = ws.begin_feed("c", false).unwrap();
    ws.finish_feed(&req, Err(Some("no access".into())));
    assert!(
        kinds(&activity_items(&ws, ActivityFilter::Mentions, false))
            .contains(&"room:c:Failed(\"no access\")".to_owned())
    );
    assert!(ws.begin_thread_mentions("c"));
    let mut tm = msg("tm", "c", 350, "bob");
    tm.tmid = Some("t1".into());
    tm.mentions = Some(vec![mention("me")]);
    ws.finish_thread_mentions("c", Ok(vec![tm, msg("main", "c", 340, "bob")]));
    let items = activity_items(&ws, ActivityFilter::All, false);
    assert!(matches!(&items[0], ActivityItem::ThreadMention { tmid, .. } if tmid == "t1"));
    // t1 is not repeated in the Threads block.
    let threads: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            ActivityItem::Thread(t) => Some(t.tmid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(threads, ["t2"]);
    assert!(
        activity_items(&ws, ActivityFilter::Threads, false)
            .iter()
            .all(|i| !i.is_mention())
    );
    assert!(
        activity_items(&ws, ActivityFilter::Mentions, false)
            .iter()
            .all(ActivityItem::is_mention)
    );
    // A live thread reply mentioning me updates the thread mention.
    let mut newer = msg("tm2", "c", 360, "bob");
    newer.tmid = Some("t1".into());
    newer.mentions = Some(vec![mention("me")]);
    ws.apply_message(newer);
    assert_eq!(ws.feeds().thread_mention("c", "t1").unwrap().id, "tm2");
}

#[test]
fn catch_up_walks_skips_and_follows_changes() {
    let mk = |rid: &str| UnreadRoom {
        rid: rid.into(),
        tier: Tier::Unread,
        ls: None,
        lm: None,
        badge: Default::default(),
    };
    let rooms = vec![mk("a"), mk("b"), mk("c")];
    let mut c = CatchUp::start(&rooms);
    assert_eq!((c.current(), c.progress()), (Some("a"), (1, 3)));
    c.advance(Advance::Read);
    c.advance(Advance::Skip);
    assert_eq!(c.current(), Some("c"));
    assert_eq!(c.upcoming(2).collect::<Vec<_>>(), ["c"]);
    c.back();
    assert_eq!(c.current(), Some("b"));
    c.advance(Advance::Skip);
    // a was read (gone), d became unread: c stays current.
    c.refresh(&[mk("b"), mk("c"), mk("d")]);
    assert_eq!((c.current(), c.progress()), (Some("c"), (2, 3)));
    c.advance(Advance::Read);
    c.advance(Advance::Read);
    assert!(c.is_done());
    assert_eq!(c.skipped_count(), 1);
    c.review_skipped();
    assert_eq!((c.current(), c.progress()), (Some("b"), (1, 1)));
    // The current room read elsewhere: the next one shows.
    let mut c = CatchUp::start(&rooms);
    c.advance(Advance::Skip);
    c.refresh(&[mk("a"), mk("c")]);
    assert_eq!(c.current(), Some("c"));
}

fn connected(entries: Vec<(Subscription, Option<i64>)>) -> Workspace {
    let mut ws = ws_with(entries);
    ws.apply_connection(ConnectionState::Connected);
    ws
}

#[test]
fn plan_paces_one_request_at_a_time() {
    let t0 = Instant::now();
    let mut ws = connected(vec![
        (unread_sub("a", RoomType::Channel, 0), Some(10)),
        (unread_sub("b", RoomType::Channel, 0), Some(10)),
    ]);
    let wanted = Wanted {
        feeds: vec!["a".into(), "b".into()],
        ..Default::default()
    };
    let mut st = LoadState::default();
    let fa = Fetch::Feed {
        rid: "a".into(),
        earlier: false,
    };
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Fetch(fa.clone()));
    st.begin(&fa, t0);
    let req = ws.begin_feed("a", false).unwrap();
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Idle, "in flight");
    ws.finish_feed(&req, Ok(history(vec![], 0, None)));
    st.finish(&fa, Ok(()), t0);
    let fb = Fetch::Feed {
        rid: "b".into(),
        earlier: false,
    };
    assert_eq!(
        load::plan(&ws, &wanted, &st, t0),
        Next::Wait(t0 + load::HISTORY_INTERVAL)
    );
    let t1 = t0 + load::HISTORY_INTERVAL;
    assert_eq!(load::plan(&ws, &wanted, &st, t1), Next::Fetch(fb.clone()));
    // Rate limited: the route pauses.
    st.begin(&fb, t1);
    st.finish(
        &fb,
        Err(LoadFailure::RateLimited(Duration::from_secs(30))),
        t1,
    );
    assert_eq!(
        load::plan(&ws, &wanted, &st, t1),
        Next::Wait(t1 + Duration::from_secs(30))
    );
    // Given up: nothing more for it until forgiven.
    let t2 = t1 + Duration::from_secs(30);
    st.begin(&fb, t2);
    st.finish(&fb, Err(LoadFailure::NotAllowed), t2);
    assert!(st.is_given_up(&fb));
    assert_eq!(load::plan(&ws, &wanted, &st, t2 + MIN), Next::Idle);
    st.forgive("b");
    assert_eq!(load::plan(&ws, &wanted, &st, t2 + MIN), Next::Fetch(fb));
    // Disconnected or from the cache: idle.
    ws.apply_connection(ConnectionState::Disconnected);
    assert_eq!(load::plan(&ws, &wanted, &st, t2 + MIN), Next::Idle);
}

const MIN: Duration = Duration::from_secs(60);

#[test]
fn plan_transient_backoff_earlier_pages_and_mentions() {
    let t0 = Instant::now();
    let mut ws = connected(vec![(unread_sub("a", RoomType::Channel, 0), Some(10))]);
    let mut st = LoadState::default();
    let page: Vec<Message> = (0..FEED_PAGE as i64)
        .map(|i| msg(&format!("n{i}"), "a", 500 + i, "bob"))
        .collect();
    let req = ws.begin_feed("a", false).unwrap();
    ws.finish_feed(&req, Ok(history(page, FEED_PAGE as usize, Some(3))));
    let wanted = Wanted {
        feeds: vec!["a".into()],
        earlier: vec!["a".into()],
        thread_mentions: vec!["a".into()],
    };
    let fe = Fetch::Feed {
        rid: "a".into(),
        earlier: true,
    };
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Fetch(fe.clone()));
    st.begin(&fe, t0);
    st.finish(&fe, Err(LoadFailure::Transient), t0);
    // The earlier page backs off 5 s; thread mentions are ready meanwhile.
    let fm = Fetch::ThreadMentions { rid: "a".into() };
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Fetch(fm.clone()));
    st.begin(&fm, t0);
    assert!(ws.begin_thread_mentions("a"));
    ws.finish_thread_mentions("a", Ok(vec![]));
    st.finish(&fm, Ok(()), t0);
    assert_eq!(
        load::plan(&ws, &wanted, &st, t0),
        Next::Wait(t0 + Duration::from_secs(5))
    );
    // Loaded mentions are not fetched again.
    let only_mentions = Wanted {
        thread_mentions: vec!["a".into()],
        ..Default::default()
    };
    assert_eq!(load::plan(&ws, &only_mentions, &st, t0 + MIN), Next::Idle);
}

#[test]
fn plan_spends_the_mentions_budget() {
    let t0 = Instant::now();
    let ws = connected(vec![(unread_sub("a", RoomType::Channel, 0), Some(10))]);
    let mut st = LoadState::default();
    let wanted = Wanted {
        thread_mentions: vec!["a".into()],
        ..Default::default()
    };
    let fm = Fetch::ThreadMentions { rid: "a".into() };
    let mut now = t0;
    for _ in 0..load::MENTIONS_BUDGET {
        assert_eq!(load::plan(&ws, &wanted, &st, now), Next::Fetch(fm.clone()));
        st.begin(&fm, now);
        st.finish(
            &fm,
            Err(LoadFailure::RateLimited(Duration::from_secs(1))),
            now,
        );
        now += load::MENTIONS_INTERVAL;
    }
    assert_eq!(load::plan(&ws, &wanted, &st, now), Next::Idle);
}

#[test]
fn feeds_are_evicted_least_recently_used() {
    use super::feeds::{FEED_MAX_ROOMS, FeedStore};
    let mut store = FeedStore::default();
    for i in 0..FEED_MAX_ROOMS {
        let req = store.begin(&format!("r{i}"), false, None).unwrap();
        store.finish(&req, Ok(History::default()), None);
    }
    let req = store.begin("new", false, None).unwrap();
    store.finish(&req, Ok(History::default()), None);
    assert!(store.feed("r0").is_none());
    assert!(store.feed("r1").is_some() && store.feed("new").is_some());
}

/// Released feeds stop absorbing live messages; thread mentions stay.
#[test]
fn released_feeds_go_and_stop_absorbing() {
    let mut c = unread_sub("c", RoomType::Channel, 100);
    c.tunread = Some(vec!["t1".into()]);
    c.tunread_user = Some(vec!["t1".into()]);
    let mut ws = connected(vec![(c, Some(400))]);
    let req = ws.begin_feed("c", false).unwrap();
    ws.finish_feed(&req, Ok(history(vec![msg("a", "c", 200, "bob")], 1, None)));
    assert!(ws.begin_thread_mentions("c"));
    let mut tm = msg("tm", "c", 350, "bob");
    tm.tmid = Some("t1".into());
    tm.mentions = Some(vec![mention("me")]);
    ws.finish_thread_mentions("c", Ok(vec![tm]));
    let rev = ws.revision(Dirty::FEEDS);
    ws.release_feeds();
    assert!(ws.revision(Dirty::FEEDS) > rev);
    assert!(ws.feeds().feed("c").is_none());
    assert!(ws.feeds().thread_mention("c", "t1").is_some());
    ws.apply_message(msg("b", "c", 300, "bob"));
    assert!(ws.feeds().feed("c").is_none(), "no feed grows back");
}

/// Feeds the open view wants are never evicted: past the cap the store
/// grows rather than refetching wanted rooms in a loop.
#[test]
fn wanted_feeds_are_not_evicted() {
    use super::feeds::{FEED_MAX_ROOMS, FeedStore};
    let mut store = FeedStore::default();
    let wanted: Vec<String> = (0..=FEED_MAX_ROOMS).map(|i| format!("r{i}")).collect();
    store.keep(&wanted);
    for rid in &wanted {
        let req = store.begin(rid, false, None).unwrap();
        store.finish(&req, Ok(History::default()), None);
    }
    assert!(wanted.iter().all(|r| store.feed(r).is_some()));
    // Once the view wants others, the old ones go again.
    store.keep(&["new".to_owned()]);
    let req = store.begin("new", false, None).unwrap();
    store.finish(&req, Ok(History::default()), None);
    assert!(store.feed("r0").is_none() && store.feed("r1").is_none());
    assert!(store.feed("new").is_some());
}

/// A disconnect keeps the loaded thread mentions (they are asked for again,
/// but stay shown when the fetches have run out).
#[test]
fn thread_mentions_survive_a_reconnect() {
    let t0 = Instant::now();
    let mut c = unread_sub("c", RoomType::Channel, 100);
    c.tunread = Some(vec!["t1".into()]);
    c.tunread_user = Some(vec!["t1".into()]);
    let mut ws = connected(vec![(c, Some(400))]);
    assert!(ws.begin_thread_mentions("c"));
    let mut tm = msg("tm", "c", 350, "bob");
    tm.tmid = Some("t1".into());
    tm.mentions = Some(vec![mention("me")]);
    ws.finish_thread_mentions("c", Ok(vec![tm]));
    let wanted = Wanted {
        thread_mentions: vec!["c".into()],
        ..Default::default()
    };
    let st = LoadState::default();
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Idle);
    ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
    assert!(ws.feeds().thread_mention("c", "t1").is_some());
    ws.apply_connection(ConnectionState::Connected);
    let fm = Fetch::ThreadMentions { rid: "c".into() };
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Fetch(fm));
    // The reload fails for now: the mention still shows.
    assert!(ws.begin_thread_mentions("c"));
    ws.finish_thread_mentions("c", Err(None));
    assert!(ws.feeds().thread_mention("c", "t1").is_some());
    assert!(matches!(
        &activity_items(&ws, ActivityFilter::Mentions, false)[0],
        ActivityItem::ThreadMention { tmid, .. } if tmid == "t1"
    ));
    // Budget spent: nothing more is asked for, the mention stays.
    let mut spent = LoadState::default();
    let f = Fetch::ThreadMentions { rid: "x".into() };
    for _ in 0..load::MENTIONS_BUDGET {
        spent.begin(&f, t0);
        spent.finish(&f, Ok(()), t0);
    }
    assert_eq!(load::plan(&ws, &wanted, &spent, t0 + MIN), Next::Idle);
    assert!(ws.feeds().thread_mention("c", "t1").is_some());
}

/// The planner: a stale loaded feed is wanted again, nothing goes while
/// the offline cache is shown, and catch-up asks for its rooms in order.
#[test]
fn plan_reloads_stale_feeds_and_idles_from_cache() {
    let t0 = Instant::now();
    let mut ws = connected(vec![
        (unread_sub("a", RoomType::Channel, 0), Some(10)),
        (unread_sub("b", RoomType::Channel, 0), Some(10)),
    ]);
    let req = ws.begin_feed("a", false).unwrap();
    ws.finish_feed(&req, Ok(history(vec![], 0, None)));
    let wanted = Wanted {
        feeds: vec!["a".into()],
        ..Default::default()
    };
    let st = LoadState::default();
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Idle, "loaded");
    ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
    ws.apply_connection(ConnectionState::Connected);
    let fa = Fetch::Feed {
        rid: "a".into(),
        earlier: false,
    };
    assert_eq!(load::plan(&ws, &wanted, &st, t0), Next::Fetch(fa));
    // Catch-up's rooms in their order (the current one first).
    let order = Wanted {
        feeds: vec!["b".into(), "a".into()],
        ..Default::default()
    };
    let fb = Fetch::Feed {
        rid: "b".into(),
        earlier: false,
    };
    assert_eq!(load::plan(&ws, &order, &st, t0), Next::Fetch(fb));
    // Showing the offline cache (connected, initial load not in yet).
    let mut cached = Workspace::new();
    assert!(cached.apply_cached(crate::cache::CachedState {
        subscriptions: vec![unread_sub("a", RoomType::Channel, 0)],
        rooms: vec![room("a", "a", RoomType::Channel)],
        ..Default::default()
    }));
    cached.apply_connection(ConnectionState::Connected);
    assert!(cached.from_cache());
    assert_eq!(load::plan(&cached, &wanted, &st, t0), Next::Idle);
}

/// T3 is orange in Activity and in the All unreads order like the row; an
/// R1 room's plain thread replies are not counted.
#[test]
fn thread_group_mentions_and_muted_threads() {
    let mut t3 = unread_sub("t3", RoomType::Channel, 100);
    t3.tunread = Some(vec!["t1".into()]);
    t3.tunread_group = Some(vec!["t1".into()]);
    let mut r4 = t3.clone();
    r4.rid = "r4".into();
    r4.name = "r4".into();
    r4.mute_group_mentions = Some(true);
    let mut r1 = sub("r1", "r1", RoomType::Channel);
    r1.tunread = Some(vec!["t9".into()]);
    r1.disable_notifications = Some(true);
    // Plain main unread (lm past ls) besides the thread @all.
    let ws = ws_with(vec![(t3, Some(200)), (r4, Some(200)), (r1, Some(200))]);
    let per = |rid: &str| entry_counts(ws.room(rid).unwrap());
    assert_eq!((per("t3").group_mentions, per("t3").threads), (1, 1));
    assert_eq!((per("r4").group_mentions, per("r4").threads), (0, 1));
    assert_eq!(per("r1").threads, 0);
    let v = unread_rooms(&ws, &InboxPrefs::default(), &Skipped::default());
    let tiers: Vec<(&str, Tier)> = v.iter().map(|r| (r.rid.as_str(), r.tier)).collect();
    assert_eq!(tiers, [("t3", Tier::GroupMention), ("r4", Tier::Unread)]);
}

/// R1 in Activity: plain thread replies are neither listed nor counted,
/// the room's `@all` neither counts nor is listed (the row is grey), while
/// mentions of me (in the room and in its threads) stay.
#[test]
fn r1_rooms_in_activity() {
    let mut r1 = unread_sub("r1", RoomType::Channel, 100);
    r1.disable_notifications = Some(true);
    r1.group_mentions = 2;
    r1.tunread = Some(vec!["plain".into(), "mine".into()]);
    r1.tunread_user = Some(vec!["mine".into()]);
    let mut quiet = unread_sub("q", RoomType::Channel, 100);
    quiet.disable_notifications = Some(true);
    quiet.group_mentions = 1;
    quiet.tunread = Some(vec!["other".into()]);
    let ws = ws_with(vec![(r1.clone(), Some(500)), (quiet, Some(500))]);
    let per = |ws: &Workspace, rid: &str| entry_counts(ws.room(rid).unwrap());
    assert_eq!(
        per(&ws, "r1"),
        InboxCounts {
            // Red through the thread mention.
            unread_rooms: 1,
            mentions: 1,
            group_mentions: 0,
            threads: 1,
        }
    );
    assert_eq!(per(&ws, "q"), InboxCounts::default());
    assert_eq!(counts(&ws).group_mentions, 0);
    assert!(mention_rooms(&ws).is_empty());
    let tmids: Vec<String> = activity_items(&ws, ActivityFilter::All, false)
        .iter()
        .map(|i| match i {
            ActivityItem::MentionedThread(t) => format!("mt:{}", t.tmid),
            ActivityItem::Thread(t) => format!("t:{}", t.tmid),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(tmids, ["mt:mine"]);
    assert!(activity_items(&ws, ActivityFilter::Threads, false).is_empty());
    // A direct mention in the R1 room's main timeline still counts.
    r1.user_mentions = 1;
    let ws = ws_with(vec![(r1, Some(500))]);
    assert_eq!(per(&ws, "r1").mentions, 2);
    assert_eq!(mention_rooms(&ws), [("r1".to_owned(), true, false)]);
}
