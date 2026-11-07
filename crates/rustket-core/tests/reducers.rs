//! Reducer regressions found in review: each test reproduces a scenario where a
//! sync result, a realtime event or a pagination step used to leave the workspace
//! in the wrong state.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::{
    Message, Removed, Room, RoomType, Subscription, User, UserRef, UserStatus, UserSummary,
};
use rustket_core::Workspace;
use rustket_core::sync::{DeltaData, InitialData, RoomSync};
use rustket_core::workspace::Change;

fn ts(ms: i64) -> RcDate {
    RcDate::from_millis(ms).unwrap()
}

fn msg(id: &str, rid: &str, ms: i64, uid: &str) -> Message {
    Message {
        id: id.into(),
        rid: rid.into(),
        msg: format!("text {id}"),
        ts: Some(ts(ms)),
        u: UserRef {
            id: uid.into(),
            username: Some(uid.into()),
            name: None,
        },
        updated_at: Some(ts(ms)),
        ..Default::default()
    }
}

fn sub(rid: &str, id: &str, upd: i64) -> Subscription {
    Subscription {
        id: id.into(),
        rid: rid.into(),
        name: rid.into(),
        t: RoomType::Channel,
        open: true,
        updated_at: Some(ts(upd)),
        ..Default::default()
    }
}

fn room(rid: &str, upd: i64) -> Room {
    Room {
        id: rid.into(),
        t: RoomType::Channel,
        name: Some(rid.into()),
        updated_at: Some(ts(upd)),
        ..Default::default()
    }
}

fn me(status: UserStatus) -> User {
    User {
        id: "me".into(),
        username: Some("me".into()),
        status: Some(status),
        ..Default::default()
    }
}

/// Delta sync result read before a realtime update, applied after it.
#[test]
fn stale_delta_subscription_overwrites_newer_realtime_state() {
    let mut ws = Workspace::new();
    let mut s = sub("r1", "s1", 1_000);
    s.unread = 5;
    s.alert = true;
    ws.apply_subscription(Change::Inserted, s.clone());
    // Delta request goes out; meanwhile the user reads the room (realtime event).
    let mut read = s.clone();
    read.unread = 0;
    read.alert = false;
    read.updated_at = Some(ts(3_000));
    ws.apply_subscription(Change::Updated, read);
    // Delta response (snapshot taken at t=2000, before the read).
    let mut stale = s;
    stale.updated_at = Some(ts(2_000));
    let mut d = DeltaData {
        next_since: Some(ts(1_500)),
        ..Default::default()
    };
    d.subscriptions.update.push(stale);
    ws.apply_delta(d);
    assert_eq!(
        ws.subscription("r1").unwrap().unread,
        0,
        "older delta copy must not overwrite newer realtime state"
    );
}

/// chat.syncMessages result applied after a newer realtime edit.
#[test]
fn stale_room_sync_overwrites_newer_edit() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    ws.apply_message(msg("x", "r1", 1_000, "u"));
    let mut v2 = msg("x", "r1", 1_000, "u");
    v2.msg = "v2".into();
    v2.edited_at = Some(ts(5_000));
    v2.updated_at = Some(ts(5_000));
    ws.apply_message(v2);
    let mut v1 = msg("x", "r1", 1_000, "u");
    v1.msg = "v1".into();
    v1.edited_at = Some(ts(4_000));
    v1.updated_at = Some(ts(4_000));
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![v1],
        deleted: vec![],
        dropped: 0,
    });
    assert_eq!(
        ws.timeline("r1").unwrap().get("x").unwrap().message.msg,
        "v2"
    );
}

/// A DM is deleted and re-created (same deterministic rid) while offline.
#[test]
fn delta_room_remove_wipes_recreated_room() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("dm", "old-sub", 100));
    ws.apply_room(Change::Inserted, room("dm", 100));
    let mut d = DeltaData {
        next_since: Some(ts(500)),
        ..Default::default()
    };
    // Old room + old sub went to the trash at t=1000, new ones created at t=2000.
    d.subscriptions.update.push(sub("dm", "new-sub", 2_000));
    d.subscriptions.remove.push(Removed {
        id: "old-sub".into(),
        rid: None,
        deleted_at: Some(ts(1_000)),
    });
    d.rooms.update.push(room("dm", 2_000));
    d.rooms.remove.push(Removed {
        id: "dm".into(),
        rid: None,
        deleted_at: Some(ts(1_000)),
    });
    ws.apply_delta(d);
    assert!(
        ws.room("dm").and_then(|e| e.sub.as_ref()).is_some(),
        "re-created room vanished from the sidebar"
    );
}

/// users.presence with an old `from` returns `full: true` = only non-offline users.
#[test]
fn presence_full_list_does_not_reset_absent_users() {
    let mut ws = Workspace::new();
    ws.apply_user_status("bob", Some("bob"), UserStatus::Online, None);
    // Bob went offline while disconnected; the full list no longer contains him.
    let d = DeltaData {
        presence: vec![UserSummary {
            id: "carol".into(),
            username: Some("carol".into()),
            status: Some(UserStatus::Online),
            ..Default::default()
        }],
        presence_full: true,
        // The delta request starts after bob's last realtime event.
        started_at: Some(Instant::now()),
        next_since: Some(ts(1)),
        ..Default::default()
    };
    ws.apply_delta(d);
    assert_eq!(
        ws.presence().status("bob"),
        UserStatus::Offline,
        "absent from a full presence list = offline"
    );
}

/// Own status changed elsewhere while disconnected; delta brings the new `me`.
#[test]
fn my_status_stale_after_delta_me() {
    let mut ws = Workspace::new();
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert_eq!(ws.my_status(), UserStatus::Online);
    ws.apply_delta(DeltaData {
        me: Some(me(UserStatus::Busy)),
        next_since: Some(ts(2)),
        ..Default::default()
    });
    assert_eq!(ws.me().unwrap().status, Some(UserStatus::Busy));
    assert_eq!(
        ws.my_status(),
        UserStatus::Busy,
        "presence cache shadows me.status"
    );
}

/// After a reset (sync with dropped entries) the timeline holds only the synced
/// (possibly old) messages; the next page resumes *below the oldest of them*.
#[test]
fn reset_then_load_older_resumes_below_old_synced_message() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    // Newest page: messages at 10_000..=19_000.
    let page: Vec<Message> = (10..20)
        .map(|i| msg(&format!("m{i}"), "r1", i * 1_000, "u"))
        .collect();
    ws.finish_load_older("r1", c, Ok(page));
    // Sync: a reaction on a month-old message + one unparsable entry.
    let mut old = msg("old", "r1", 1_000, "u");
    old.updated_at = Some(ts(30_000));
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![old],
        deleted: vec![],
        dropped: 1,
    });
    let cursor = ws.begin_load_older("r1").unwrap();
    let dbg = format!("{cursor:?}");
    // A correct reload starts at the newest message (before: None).
    assert!(
        dbg.contains("before: None"),
        "reload resumes below the old message; messages 2s..19s are never loaded: {dbg}"
    );
}

/// First page failed; meanwhile a reply in an old thread bumped the parent.
#[test]
fn failed_first_page_then_parent_bump_creates_gap() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    let mut parent = msg("p", "r1", 1_000, "u");
    parent.tcount = Some(3);
    parent.updated_at = Some(ts(50_000));
    ws.apply_message(parent);
    ws.finish_load_older("r1", c, Err("timeout".into()));
    let cursor = ws.begin_load_older("r1").unwrap();
    let dbg = format!("{cursor:?}");
    assert!(
        dbg.contains("before: None"),
        "retry resumes below the old parent: {dbg}"
    );
}

/// Old message of the typing user updated (reaction) -> typing indicator vanishes.
#[test]
fn reaction_update_on_old_message_stops_typing() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    let now = Instant::now();
    ws.apply_user_activity_at("r1", "bob", &["user-typing".into()], None, now);
    let mut old = msg("old", "r1", 1_000, "bob");
    old.reactions = Some(Default::default());
    old.updated_at = Some(ts(9_000));
    ws.apply_message(old);
    assert!(
        ws.typing_label("r1", None, now).is_some(),
        "bob is still typing"
    );
}

/// apply_initial drops a timeline without recording it in `changes.timelines`.
#[test]
fn apply_initial_dropped_timeline_not_in_changes() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("gone", "s1", 1));
    ws.open_room("gone");
    ws.take_changes();
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(ws.timeline("gone").is_none());
    let c = ws.take_changes();
    assert!(
        c.timelines.contains("gone") && c.rooms.contains("gone"),
        "{c:?}"
    );
}

/// After the reload the old synced message (below the new window) is gone and
/// the window is contiguous.
#[test]
fn reload_after_reset_prunes_messages_below_the_window() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let mut old = msg("old", "r1", 1_000, "u");
    old.updated_at = Some(ts(30_000));
    ws.apply_message(old);
    let c = ws.begin_load_older("r1").unwrap();
    let page: Vec<Message> = (10..20)
        .map(|i| msg(&format!("m{i}"), "r1", i * 1_000, "u"))
        .collect();
    ws.finish_load_older("r1", c, Ok(page));
    let t = ws.timeline("r1").unwrap();
    assert!(!t.contains("old"));
    assert_eq!(t.len(), 10);
    // An update of a message below the window is ignored...
    let mut old = msg("old", "r1", 1_000, "u");
    old.updated_at = Some(ts(40_000));
    ws.apply_message(old);
    assert!(!ws.timeline("r1").unwrap().contains("old"));
    // ...a new message is not.
    ws.apply_message(msg("new", "r1", 50_000, "u"));
    assert!(ws.timeline("r1").unwrap().contains("new"));
}

/// A thread reply bumps an old parent while the user is typing in a thread of
/// the same room: the parent update is not a new message.
#[test]
fn thread_parent_bump_does_not_stop_recording() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    let now = Instant::now();
    ws.apply_user_activity_at("r1", "bob", &["user-recording".into()], None, now);
    let mut parent = msg("p", "r1", 1_000, "bob");
    parent.tcount = Some(1);
    parent.updated_at = Some(ts(60_000));
    ws.apply_message(parent);
    assert!(
        ws.typing_label("r1", None, now).is_some(),
        "bob is still recording"
    );
    // His new message does stop it.
    ws.apply_message(msg("n", "r1", 70_000, "bob"));
    assert!(ws.typing_label("r1", None, now).is_none());
}

/// Under `UI_Use_Real_Name` Rocket.Chat sends real names in `user-activity`.
#[test]
fn real_name_activity_of_the_current_user_is_ignored() {
    let mut ws = Workspace::new();
    let mut m = me(UserStatus::Online);
    m.name = Some("My Real Name".into());
    ws.apply_me(m);
    ws.apply_setting(
        Change::Updated,
        rocketchat::models::Setting {
            id: "UI_Use_Real_Name".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    let now = Instant::now();
    ws.apply_user_activity_at("r1", "My Real Name", &["user-typing".into()], None, now);
    assert!(ws.typing_label("r1", None, now).is_none());
    // Bob's typing is keyed by his real name; his new message stops it.
    ws.apply_user_activity_at("r1", "Bob B", &["user-typing".into()], None, now);
    assert!(ws.typing_label("r1", None, now).is_some());
    let mut b = msg("n", "r1", 70_000, "bob");
    b.u.name = Some("Bob B".into());
    ws.apply_message(b);
    assert!(ws.typing_label("r1", None, now).is_none());
}

/// A realtime insert that arrived while the initial load ran survives it, and
/// so does newer subscription state.
#[test]
fn apply_initial_keeps_realtime_state_newer_than_the_snapshot() {
    let mut ws = Workspace::new();
    // Realtime during the load: a new DM and a read of r1.
    ws.apply_subscription(Change::Inserted, sub("dm", "s-dm", 5_000));
    let mut read = sub("r1", "s1", 6_000);
    read.unread = 0;
    ws.apply_subscription(Change::Updated, read);
    ws.open_room("dm");
    // Snapshot read at t≈4000.
    let mut stale = sub("r1", "s1", 4_000);
    stale.unread = 3;
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![stale, sub("r2", "s2", 3_000)],
        rooms: vec![room("r1", 4_000), room("r2", 3_000)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(ws.room("dm").and_then(|e| e.sub.as_ref()).is_some());
    assert!(ws.timeline("dm").is_some());
    assert_eq!(ws.subscription("r1").unwrap().unread, 0);
    assert!(ws.room("r2").is_some());
}

/// apply_delta merges every data set.
#[test]
fn apply_delta_merges_everything() {
    use rocketchat::models::{
        CustomEmoji, CustomUserStatus, Permission, Role, Setting, UpdatedSince,
    };
    let mut ws = Workspace::new();
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![sub("r1", "s1", 1_000)],
        rooms: vec![room("r1", 1_000)],
        permissions: vec![
            Permission {
                id: "p-keep".into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
            Permission {
                id: "p-gone".into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
        ],
        custom_emoji: vec![CustomEmoji {
            id: "e1".into(),
            name: "parrot".into(),
            ..Default::default()
        }],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    let mut s = sub("r1", "s1", 2_000);
    s.unread = 7;
    let mut r = room("r1", 2_000);
    r.topic = Some("new topic".into());
    let mut m = me(UserStatus::Away);
    m.name = Some("Renamed".into());
    let mut perms = UpdatedSince::default();
    perms.update.push(Permission {
        id: "p-keep".into(),
        roles: vec!["admin".into()],
        updated_at: None,
    });
    perms.remove.push(Removed {
        id: "p-gone".into(),
        rid: None,
        deleted_at: None,
    });
    let mut emoji = UpdatedSince::default();
    emoji.update.push(CustomEmoji {
        id: "e2".into(),
        name: "cat".into(),
        ..Default::default()
    });
    emoji.remove.push(Removed {
        id: "e1".into(),
        rid: None,
        deleted_at: None,
    });
    let mut d = DeltaData {
        me: Some(m),
        permissions: Some(perms),
        custom_emoji: Some(emoji),
        settings: Some(vec![Setting {
            id: "Site_Name".into(),
            value: serde_json::json!("Corp"),
            ..Default::default()
        }]),
        roles: Some(vec![Role {
            id: "auditor".into(),
            ..Default::default()
        }]),
        custom_statuses: Some(vec![CustomUserStatus {
            id: "cs".into(),
            name: "Lunch".into(),
            ..Default::default()
        }]),
        next_since: Some(ts(99)),
        ..Default::default()
    };
    d.subscriptions.update.push(s);
    d.rooms.update.push(r);
    ws.apply_delta(d);
    assert_eq!(ws.subscription("r1").unwrap().unread, 7);
    assert_eq!(
        ws.room("r1")
            .unwrap()
            .room
            .as_ref()
            .unwrap()
            .topic
            .as_deref(),
        Some("new topic")
    );
    assert_eq!(ws.me().unwrap().name.as_deref(), Some("Renamed"));
    assert_eq!(ws.my_status(), UserStatus::Away);
    assert_eq!(
        ws.permission_roles("p-keep"),
        Some(&["admin".to_owned()][..])
    );
    assert!(ws.permission_roles("p-gone").is_none());
    assert!(ws.emoji().get_custom("cat").is_some());
    assert!(ws.emoji().get_custom("parrot").is_none());
    assert_eq!(ws.settings().site_name(), "Corp");
    assert!(ws.roles().contains_key("auditor"));
    assert_eq!(ws.custom_statuses().count(), 1);
    assert_eq!(ws.last_sync(), Some(ts(99)));
}

/// A full presence list marks absent users offline; a partial one does not.
#[test]
fn partial_presence_list_keeps_absent_users() {
    let mut ws = Workspace::new();
    ws.apply_user_status("bob", Some("bob"), UserStatus::Online, None);
    ws.apply_delta(DeltaData {
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert_eq!(ws.presence().status("bob"), UserStatus::Online);
    ws.apply_delta(DeltaData {
        presence_full: true,
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert_eq!(ws.presence().status("bob"), UserStatus::Offline);
}

/// Pending messages survive closing the room and late send results.
#[test]
fn pending_messages_survive_room_switches() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let local = ws.new_local_message("r1", "my long composed text", None, false);
    let id = local.id.clone();
    ws.add_pending(local.clone());
    ws.close_room("r1");
    // The send fails while the room is closed.
    ws.mark_pending_failed("r1", None, &id, "offline");
    ws.open_room("r1");
    let item = ws.timeline("r1").unwrap().get(&id).unwrap().clone();
    assert_eq!(item.message.msg, "my long composed text");
    assert!(matches!(
        item.state,
        rustket_core::timeline::DeliveryState::Failed { .. }
    ));
    assert_eq!(ws.pending_messages("r1").len(), 1);
    // The echo confirms it.
    let mut echo = local;
    echo.temp = None;
    ws.apply_message(echo);
    assert!(ws.pending_messages("r1").is_empty());
    assert!(!ws.timeline("r1").unwrap().get(&id).unwrap().is_pending());
}

/// Room sync with dropped entries resets; the next page starts at the newest
/// message instead of below the re-applied old one.
#[test]
fn room_sync_reset_does_not_hide_newer_messages() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    for (id, t) in [("a", 1000), ("b", 2000), ("c", 3000)] {
        ws.apply_message(msg(id, "r1", t, "u"));
    }
    let mut edited = msg("a", "r1", 1000, "u");
    edited.msg = "edited".into();
    edited.updated_at = Some(ts(9000));
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![edited],
        deleted: vec![],
        dropped: 1,
    });
    let c = ws.begin_load_older("r1").unwrap();
    let dbg = format!("{c:?}");
    assert!(dbg.contains("before: None"), "{dbg}");
}

/// With Message_KeepHistory && !Message_ShowDeletedStatus deletions are not in
/// the sync result, so the open timeline reloads.
#[test]
fn room_sync_reloads_when_deletions_are_invisible() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    ws.finish_load_older("r1", c, Ok(vec![msg("a", "r1", 1000, "u")]));
    ws.apply_setting(
        Change::Updated,
        rocketchat::models::Setting {
            id: "Message_KeepHistory".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        ..Default::default()
    });
    let t = ws.timeline("r1").unwrap();
    assert!(!t.pagination.loaded);
    assert!(t.is_empty());
}

/// Read receipts: the server marks `ts < until`; thread receipts only touch
/// the thread.
#[test]
fn read_receipts_exclusive_and_thread_scoped() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let mut p = msg("p", "r1", 500, "u");
    p.unread = Some(true);
    ws.apply_message(p);
    ws.open_thread("r1", "p");
    for (id, t) in [("a", 1000), ("b", 2000)] {
        let mut m = msg(id, "r1", t, "u");
        m.tmid = Some("p".into());
        m.unread = Some(true);
        ws.apply_message(m);
    }
    ws.apply_messages_read("r1", Some(ts(2000)), Some("p"));
    let th = ws.thread("p").unwrap();
    assert_eq!(th.get("a").unwrap().message.unread, None);
    assert_eq!(
        th.get("b").unwrap().message.unread,
        Some(true),
        "ts == until stays unread"
    );
    assert_eq!(
        ws.timeline("r1").unwrap().get("p").unwrap().message.unread,
        Some(true)
    );
    ws.apply_messages_read("r1", Some(ts(500)), None);
    assert_eq!(
        ws.timeline("r1").unwrap().get("p").unwrap().message.unread,
        Some(true)
    );
    ws.apply_messages_read("r1", Some(ts(501)), None);
    assert_eq!(
        ws.timeline("r1").unwrap().get("p").unwrap().message.unread,
        None
    );
}

/// Deleting a thread parent clears it in the open thread.
#[test]
fn deleting_thread_parent_reaches_thread_timeline() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    ws.apply_message(msg("p", "r1", 1000, "u"));
    ws.open_thread("r1", "p");
    ws.apply_message_deleted("r1", "p");
    assert!(ws.thread("p").unwrap().parent().is_none());
}

/// `MessagesImported`: the room history reloads from scratch.
#[test]
fn reset_room_timelines_reloads_from_newest() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    ws.finish_load_older("r1", c, Ok(vec![msg("a", "r1", 1000, "u")]));
    ws.reset_room_timelines("r1");
    assert!(ws.timeline("r1").unwrap().is_empty());
    let c = ws.begin_load_older("r1").unwrap();
    assert!(format!("{c:?}").contains("before: None"));
}

// --- re-review regressions ---------------------------------------------------

use rocketchat::models::Paginated;
use rocketchat::rest::HistoryCursor;
use serde_json::json;

fn setting(ws: &mut Workspace, id: &str, v: serde_json::Value) {
    ws.apply_setting(
        Change::Updated,
        rocketchat::models::Setting {
            id: id.into(),
            value: v,
            ..Default::default()
        },
    );
}

fn page(from: i64, to: i64) -> Vec<Message> {
    // Newest first, like loadHistory.
    (from..to)
        .rev()
        .map(|i| msg(&format!("m{i}"), "r1", i * 1_000, "u"))
        .collect()
}

fn ids(ws: &Workspace) -> Vec<String> {
    ws.timeline("r1")
        .unwrap()
        .messages()
        .map(|m| m.id.clone())
        .collect()
}

/// With tickets, a late page of an earlier request is dropped whatever the
/// arrival order.
#[test]
fn late_page_with_ticket_is_dropped_in_any_order() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let (_, t_old) = ws.begin_load_older_ticket("r1").unwrap();
    ws.close_room("r1");
    ws.open_room("r1");
    let (_, t_new) = ws.begin_load_older_ticket("r1").unwrap();
    // The old view's late page (start of the room) arrives first.
    let p3 = page(0, 50);
    let c3 = HistoryCursor::resume("r1", 50, ts(0), p3.iter().map(|m| m.id.clone()));
    assert!(!ws.finish_load_older_ticket("r1", t_old, c3, Ok(p3)));
    let p1 = page(100, 150);
    let c1 = HistoryCursor::resume("r1", 50, ts(100_000), p1.iter().map(|m| m.id.clone()));
    assert!(ws.finish_load_older_ticket("r1", t_new, c1, Ok(p1)));
    assert_eq!(ids(&ws).first().map(String::as_str), Some("m100"));
    let next = format!("{:?}", ws.begin_load_older("r1").unwrap());
    assert!(
        next.contains("before: Some(RcDate(1970-01-01T00:01:40Z))"),
        "{next}"
    );
}

/// A message deleted while its page is in flight does not come back with it,
/// and the thread offset steps back for replies deleted in the loaded prefix.
#[test]
fn deletions_during_a_page_request() {
    let reply = |i: i64| {
        let mut m = msg(&format!("c{i}"), "r1", 10_000 + i, "u");
        m.tmid = Some("p".into());
        m
    };
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_thread("r1", "p");
    let (off, t) = ws.begin_load_thread_ticket("p").unwrap();
    ws.finish_load_thread_ticket(
        "p",
        t,
        Ok(Paginated {
            items: (0..3).map(reply).collect(),
            count: 3,
            offset: off,
            total: 9,
            raw_len: 3,
            dropped: 0,
        }),
    );
    let (off, t) = ws.begin_load_thread_ticket("p").unwrap();
    assert_eq!(off, 3);
    ws.apply_message_deleted("r1", "c1");
    ws.apply_message_deleted("r1", "c4");
    // The page was built before the deletions.
    ws.finish_load_thread_ticket(
        "p",
        t,
        Ok(Paginated {
            items: (3..6).map(reply).collect(),
            count: 3,
            offset: off,
            total: 9,
            raw_len: 3,
            dropped: 0,
        }),
    );
    let th = ws.thread("p").unwrap();
    assert!(!th.contains("c4") && !th.contains("c1"));
    assert_eq!(ws.begin_load_thread("p"), Some(5));
}

/// The user leaves a room (realtime removal) while a delta sync is in flight; the
/// delta response, read before the removal, still lists the subscription.
#[test]
fn f01_realtime_removal_not_resurrected_by_older_delta() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1_000));
    ws.apply_room(Change::Inserted, room("r1", 1_000));
    // Delta request goes out at t=2000. The user leaves at t=3000 (realtime).
    ws.apply_subscription(Change::Removed, sub("r1", "s1", 3_000));
    assert!(ws.room("r1").is_none());
    // Delta response (snapshot at t=2000) still contains the subscription.
    let mut d = DeltaData {
        next_since: Some(ts(1_500)),
        ..Default::default()
    };
    d.subscriptions.update.push(sub("r1", "s1", 2_000));
    d.rooms.update.push(room("r1", 2_000));
    ws.apply_delta(d);
    assert!(
        ws.room("r1").and_then(|e| e.sub.as_ref()).is_none(),
        "room the user left is back in the sidebar"
    );
}

/// Same during the initial load.
#[test]
fn f01_realtime_removal_not_resurrected_by_older_initial_snapshot() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1_000));
    ws.apply_subscription(Change::Removed, sub("r1", "s1", 3_000));
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![sub("r1", "s1", 2_000)],
        rooms: vec![room("r1", 2_000)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(
        ws.room("r1").and_then(|e| e.sub.as_ref()).is_none(),
        "room the user left is back in the sidebar"
    );
}

/// A subscription created while initial_load ran (realtime insert at t=5000).
/// subscriptions.get was answered before it existed (t=4000); rooms.get ran a bit
/// later and lists a room updated at t=6000. The merge compares against the
/// newest timestamp of BOTH lists, so the new subscription is dropped.
#[test]
fn f01_new_subscription_survives_when_rooms_snapshot_is_newer() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("dm", "s-dm", 5_000));
    ws.apply_room(Change::Inserted, room("dm", 4_990));
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![sub("r1", "s1", 3_000)],
        rooms: vec![room("r1", 6_000), room("dm", 4_990)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(
        ws.room("dm").and_then(|e| e.sub.as_ref()).is_some(),
        "new subscription lost; entry = {:?}",
        ws.room("dm").map(|e| (e.room.is_some(), e.sub.is_some()))
    );
}

/// A history page that was in flight when the timeline was reset (room sync with
/// invisible deletions / dropped entries, or MessagesImported) is applied to the
/// reset timeline: it becomes the "first page" although it is an older page.
#[test]
fn f02_stale_page_after_reset_does_not_create_a_gap() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c1 = ws.begin_load_older("r1").unwrap();
    let newest: Vec<Message> = (10..20)
        .map(|i| msg(&format!("m{i}"), "r1", i * 1_000, "u"))
        .collect();
    ws.finish_load_older("r1", c1, Ok(newest));
    // User scrolls up: page 2 requested.
    let c2 = ws.begin_load_older("r1").unwrap();
    // Reconnect: room sync resets the timeline before page 2 answers.
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![],
        deleted: vec![],
        dropped: 1,
    });
    let older: Vec<Message> = (0..10)
        .map(|i| msg(&format!("m{i}"), "r1", i * 1_000, "u"))
        .collect();
    ws.finish_load_older("r1", c2, Ok(older));
    // A new message arrives in realtime.
    ws.apply_message(msg("new", "r1", 30_000, "u"));
    let ids: Vec<String> = ws
        .timeline("r1")
        .unwrap()
        .messages()
        .map(|m| m.id.clone())
        .collect();
    let t = ws.timeline("r1").unwrap();
    eprintln!(
        "timeline after stale page: {ids:?} loaded={} has_more={}",
        t.pagination.loaded, t.pagination.has_more_before
    );
    // Correct: either the stale page is ignored (and the next load starts at the
    // newest message) or the window still reaches the newest message.
    let gap = ids.contains(&"m0".to_owned())
        && !ids.contains(&"m19".to_owned())
        && ids.contains(&"new".to_owned());
    assert!(
        !gap,
        "gap: m10..m19 missing between the stale page and the new message: {ids:?}"
    );
}

/// Rocket.Chat 8.9 broadcasts the subscription change of a new message as the
/// document read BEFORE the update with unread/alert patched in memory
/// (notifyUsersOnMessage.ts:104-140), so its `_updatedAt` equals the one of a
/// REST snapshot taken before the message. Observed live: read at T (unread 0,
/// _updatedAt T) -> mention -> realtime {unread 1, alert, _updatedAt T} -> delta
/// read before the mention {unread 0, _updatedAt T} overwrites it.
#[test]
fn f01_realtime_unread_patch_with_equal_updated_at_survives_delta() {
    let mut ws = Workspace::new();
    let mut read = sub("r1", "s1", 45_959);
    read.unread = 0;
    ws.apply_subscription(Change::Updated, read.clone());
    let mut mention = read.clone();
    mention.unread = 1;
    mention.alert = true;
    mention.user_mentions = 1;
    ws.apply_subscription(Change::Updated, mention);
    let mut d = DeltaData {
        next_since: Some(ts(1)),
        ..Default::default()
    };
    d.subscriptions.update.push(read.clone());
    ws.apply_delta(d);
    let s = ws.subscription("r1").unwrap();
    assert_eq!(
        (s.unread, s.alert, s.user_mentions),
        (1, true, 1),
        "mention badge lost after delta"
    );
}

#[test]
fn f01_realtime_unread_patch_with_equal_updated_at_survives_initial() {
    let mut ws = Workspace::new();
    let mut read = sub("r1", "s1", 45_959);
    read.unread = 0;
    let mut mention = read.clone();
    mention.unread = 1;
    mention.alert = true;
    ws.apply_subscription(Change::Updated, mention);
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![read],
        rooms: vec![room("r1", 1)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    let s = ws.subscription("r1").unwrap();
    assert_eq!(
        (s.unread, s.alert),
        (1, true),
        "mention badge lost after initial load"
    );
}

/// Message_KeepHistory on, Message_ShowDeletedStatus off: a thread reply deleted
/// while offline is only hidden, chat.syncMessages reports nothing, and the open
/// THREAD timeline keeps showing it (only the room timeline is reset).
#[test]
fn f13_invisible_deletion_also_reloads_open_threads() {
    let mut ws = Workspace::new();
    setting(&mut ws, "Message_KeepHistory", json!(true));
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    ws.apply_message(msg("p", "r1", 1_000, "u"));
    ws.open_thread("r1", "p");
    let mut reply = msg("c1", "r1", 2_000, "v");
    reply.tmid = Some("p".into());
    let off = ws.begin_load_thread("p").unwrap();
    ws.finish_load_thread(
        "p",
        Ok(Paginated {
            items: vec![reply],
            count: 1,
            offset: off,
            total: 1,
            raw_len: 1,
            dropped: 0,
        }),
    );
    assert!(ws.thread("p").unwrap().contains("c1"));
    // Reconnect; c1 was deleted (hidden) meanwhile; the sync reports nothing.
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![],
        deleted: vec![],
        dropped: 0,
    });
    let t = ws.thread("p").unwrap();
    assert!(
        !t.contains("c1") || !t.pagination.loaded,
        "thread still shows the hidden reply and will not reload"
    );
}

/// reset_room_timelines (MessagesImported) while a thread page is in flight: the
/// stale page becomes the thread's first page and the offset continues past it.
#[test]
fn f02_stale_thread_page_after_reset() {
    let reply = |i: i64| {
        let mut m = msg(&format!("c{i}"), "r1", 10_000 + i, "u");
        m.tmid = Some("p".into());
        m
    };
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_thread("r1", "p");
    let off = ws.begin_load_thread("p").unwrap();
    ws.finish_load_thread(
        "p",
        Ok(Paginated {
            items: (0..3).map(reply).collect(),
            count: 3,
            offset: off,
            total: 9,
            raw_len: 3,
            dropped: 0,
        }),
    );
    let off = ws.begin_load_thread("p").unwrap();
    assert_eq!(off, 3);
    ws.reset_room_timelines("r1");
    ws.finish_load_thread(
        "p",
        Ok(Paginated {
            items: (3..6).map(reply).collect(),
            count: 3,
            offset: off,
            total: 9,
            raw_len: 3,
            dropped: 0,
        }),
    );
    let next = ws.begin_load_thread("p");
    let ids: Vec<String> = ws
        .thread("p")
        .unwrap()
        .messages()
        .map(|m| m.id.clone())
        .collect();
    assert!(
        ids.contains(&"c0".to_owned()) || next == Some(0),
        "c0..c2 will never load: {ids:?}, next offset {next:?}"
    );
}

/// A full reload (session restarted after `Ended` → Ready → initial_load) must
/// drop a room the user lost while offline. If that room happened to carry the
/// newest `_updatedAt` the client knew, it is kept forever.
#[test]
fn apply_initial_keeps_room_deleted_offline_when_it_was_the_newest() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    ws.apply_subscription(Change::Inserted, sub("a", "sa", 1_000));
    ws.apply_room(Change::Inserted, room("a", 1_000));
    // Last activity before going offline happened in room x.
    ws.apply_subscription(Change::Inserted, sub("x", "sx", 5_000));
    ws.apply_room(Change::Inserted, room("x", 5_000));
    // Offline: the user is kicked from x (nothing else changes). Full reload,
    // started after those events.
    ws.apply_initial(InitialData {
        started_at: Some(Instant::now()),
        me: me(UserStatus::Online),
        subscriptions: vec![sub("a", "sa", 1_000)],
        rooms: vec![room("a", 1_000)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(
        ws.room("x").is_none(),
        "room x was removed on the server but stays in the sidebar: {:?}",
        ws.room("x").map(|e| (e.sub.is_some(), e.room.is_some()))
    );
}

/// Rocket.Chat 8.9 `notifyUsersOnMessage` broadcasts the *pre-update*
/// subscription document patched with `alert/open/unread+1` (its `_updatedAt` is
/// the old one). A REST snapshot read just before that message has the same
/// `_updatedAt` and the old counters; applied afterwards it wins the tie.
#[test]
fn stale_snapshot_wins_tie_against_realtime_unread_patch() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    // State known before the reconnect.
    let mut s0 = sub("dm", "sdm", 1_000);
    s0.t = RoomType::Direct;
    s0.unread = 0;
    s0.alert = false;
    ws.apply_subscription(Change::Inserted, s0.clone());
    // delta_sync's subscriptions.get reads s0 (T=1000) ... meanwhile a DM
    // arrives: realtime patch {..s0, alert, unread: 1} (still T=1000).
    let mut patch = s0.clone();
    patch.alert = true;
    patch.unread = 1;
    ws.apply_subscription(Change::Updated, patch);
    assert_eq!(ws.subscription("dm").unwrap().unread, 1);
    // ...then the delta result (read before the message) is applied.
    let mut d = DeltaData {
        next_since: Some(ts(2)),
        ..Default::default()
    };
    d.subscriptions.update.push(s0.clone());
    ws.apply_delta(d);
    assert_eq!(
        ws.subscription("dm").unwrap().unread,
        1,
        "the unread badge of the new DM was rolled back by the older snapshot"
    );
}

/// Same with apply_initial at startup.
#[test]
fn stale_initial_snapshot_wins_tie_against_realtime_unread_patch() {
    let mut ws = Workspace::new();
    let mut s0 = sub("dm", "sdm", 1_000);
    s0.t = RoomType::Direct;
    let mut patch = s0.clone();
    patch.alert = true;
    patch.unread = 1;
    // Streams are live while initial_load runs.
    ws.apply_subscription(Change::Inserted, patch);
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![s0],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(
        ws.subscription("dm").unwrap().alert,
        "new-message alert lost to the snapshot read before it"
    );
}

/// A sync reset while an older page is in flight: the late page becomes the
/// "first" page, the newest messages are never reloaded and the window has a gap.
#[test]
fn reset_with_page_in_flight_leaves_gap() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let _ = ws.begin_load_older("r1").unwrap();
    // Simulate the cursor having served the newest page (ts 100..150).
    let p1 = page(100, 150);
    // We cannot run next_page offline; use resume to emulate an advanced cursor.
    let c1 = HistoryCursor::resume("r1", 50, ts(100_000), p1.iter().map(|m| m.id.clone()));
    ws.finish_load_older("r1", c1.clone(), Ok(p1));
    // User scrolls up: request for 50..100 in flight.
    let c2 = ws.begin_load_older("r1").unwrap();
    // Reconnect resync on a KeepHistory server resets the timeline.
    ws.apply_setting(
        Change::Updated,
        rocketchat::models::Setting {
            id: "Message_KeepHistory".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        ..Default::default()
    });
    // The old request completes.
    let c2_adv = HistoryCursor::resume("r1", 50, ts(50_000), Vec::<String>::new());
    let _ = c2;
    ws.finish_load_older("r1", c2_adv, Ok(page(50, 100)));
    // A new message arrives.
    ws.apply_message(msg("new", "r1", 200_000, "u"));
    let shown = ids(&ws);
    let next = ws.begin_load_older("r1").map(|c| format!("{c:?}"));
    // Correct: the late page is dropped and the next load starts at the newest
    // message, or the window reaches the newest message.
    assert!(
        shown.iter().any(|i| i == "m149")
            || next.as_deref().is_some_and(|c| c.contains("before: None")),
        "newest history (m100..m149) missing after the late page; shown {} msgs from {:?} to {:?}; next cursor {:?}",
        shown.len(),
        shown.first(),
        shown.last(),
        next
    );
}

/// Old view scrolled to page 3 (request in flight), user switches away and back;
/// the new first page arrives, then the late page 3: its cursor replaces the new
/// one, so page 2 is skipped for good.
#[test]
fn late_page_after_reopen_skips_a_page_permanently() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let _ = ws.begin_load_older("r1").unwrap(); // old view: page 3 in flight
    ws.close_room("r1");
    ws.open_room("r1");
    let _ = ws.begin_load_older("r1").unwrap(); // new view: first page
    let p1 = page(100, 150);
    let c1 = HistoryCursor::resume("r1", 50, ts(100_000), p1.iter().map(|m| m.id.clone()));
    ws.finish_load_older("r1", c1, Ok(p1));
    let p3 = page(0, 50);
    let mut c3 = HistoryCursor::resume("r1", 50, ts(0), p3.iter().map(|m| m.id.clone()));
    // Pretend the old cursor reached the start of the room.
    let _ = &mut c3;
    ws.finish_load_older("r1", c3, Ok(p3));
    let next = ws.begin_load_older("r1").map(|c| format!("{c:?}"));
    let shown = ids(&ws);
    assert!(
        next.as_deref()
            .is_some_and(|c| c.contains("before: Some(RcDate(1970-01-01T00:01:40Z))")),
        "next request continues below the late page, m50..m99 are never loaded: shown {} (m{}..), next {:?}",
        shown.len(),
        shown.first().unwrap(),
        next
    );
}

/// The outbox is only reconciled by apply_message; a history page (or thread
/// page) carrying the server copy leaves the local copy pending forever.
#[test]
fn outbox_not_reconciled_by_history_page() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let local = ws.new_local_message("r1", "hello", None, false);
    let id = local.id.clone();
    ws.add_pending(local.clone());
    ws.mark_pending_failed("r1", None, &id, "timeout");
    ws.close_room("r1");
    // The server did store it (the echo was missed while offline). Reopen.
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    let mut server_copy = local;
    server_copy.temp = None;
    server_copy.updated_at = server_copy.ts;
    ws.finish_load_older("r1", c, Ok(vec![server_copy]));
    assert!(
        !ws.timeline("r1").unwrap().get(&id).unwrap().is_pending(),
        "timeline shows it as sent"
    );
    assert!(
        ws.pending_messages("r1").is_empty(),
        "outbox still lists the delivered message as failed: {:?}",
        ws.pending_messages("r1")
            .iter()
            .map(|i| &i.state)
            .collect::<Vec<_>>()
    );
}

/// The my_status fix made apply_me write the presence cache, so a `/me` read
/// before a realtime status change and applied after it (delta sync) rolls the
/// status back.
#[test]
fn stale_me_from_delta_rolls_back_realtime_status() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    // delta_sync reads /me (online) ... user picks Busy, realtime event:
    ws.apply_user_status("me", Some("me"), UserStatus::Busy, None);
    assert_eq!(ws.my_status(), UserStatus::Busy);
    // ... delta applied.
    ws.apply_delta(DeltaData {
        me: Some(me(UserStatus::Online)),
        next_since: Some(ts(2)),
        ..Default::default()
    });
    assert_eq!(
        ws.my_status(),
        UserStatus::Busy,
        "status rolled back by the older /me"
    );
}

/// users.presence (full) is read when delta_sync starts; bob comes online in
/// realtime before the delta is applied; the full list (without bob) marks him
/// offline and nothing corrects it until his next status change.
#[test]
fn full_presence_list_marks_user_who_just_came_online_offline() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    ws.apply_user_status("bob", Some("bob"), UserStatus::Offline, None);
    // Realtime during the delta request:
    ws.apply_user_status("bob", Some("bob"), UserStatus::Online, None);
    ws.apply_delta(DeltaData {
        presence: vec![],
        presence_full: true,
        next_since: Some(ts(2)),
        ..Default::default()
    });
    assert_eq!(
        ws.presence().status("bob"),
        UserStatus::Online,
        "bob shown offline although his online event is newer than the list"
    );
}

/// Session restarted (Ended → RetryConnection → Ready → initial_load): the open
/// timeline missed everything posted while no stream was connected, but
/// apply_initial keeps it with its window and cursor; the next realtime message
/// is appended right after the pre-outage messages and paging continues below.
#[test]
fn apply_initial_keeps_stale_timeline_window() {
    let mut ws = Workspace::new();
    ws.apply_me(me(UserStatus::Online));
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.apply_room(Change::Inserted, room("r1", 1));
    ws.open_room("r1");
    let _ = ws.begin_load_older("r1").unwrap();
    let p1 = page(100, 150);
    let c1 = HistoryCursor::resume("r1", 50, ts(100_000), p1.iter().map(|m| m.id.clone()));
    ws.finish_load_older("r1", c1, Ok(p1));
    // Outage: m150..m299 posted. Full reload.
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![sub("r1", "s1", 299_000)],
        rooms: vec![room("r1", 299_000)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    ws.apply_message(msg("m300", "r1", 300_000, "u"));
    let next = ws.begin_load_older("r1").map(|c| format!("{c:?}"));
    let t = ws.timeline("r1").unwrap();
    assert!(
        !t.pagination.loaded || next.as_deref().is_some_and(|c| c.contains("before: None")),
        "stale window kept: {} msgs ending ..m149, m300; next page {:?}",
        t.len(),
        next.map(|c| c.chars().take(90).collect::<String>())
    );
}

/// An open room with no messages has no sync watermark: the app's resync
/// (`timeline(rid)?.last_updated_at()?`) skips it, so messages posted while
/// disconnected never appear.
#[test]
fn empty_open_room_has_no_sync_watermark() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "s1", 1));
    ws.open_room("r1");
    let c = ws.begin_load_older("r1").unwrap();
    ws.finish_load_older("r1", c, Ok(vec![]));
    assert!(
        ws.timeline("r1").unwrap().last_updated_at().is_some(),
        "no watermark for chat.syncMessages after loading an empty room"
    );
}

/// Kicked from x while initial_load runs: the realtime removal is applied,
/// then the snapshot (read before the kick) brings x back for the session.
#[test]
fn apply_initial_resurrects_room_removed_during_load() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Removed, sub("x", "sx", 5_000));
    ws.apply_room(Change::Removed, room("x", 5_000));
    ws.apply_initial(InitialData {
        me: me(UserStatus::Online),
        subscriptions: vec![sub("x", "sx", 5_000)],
        rooms: vec![room("x", 5_000)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(
        ws.room("x").is_none(),
        "removed room resurrected by the older snapshot"
    );
}
