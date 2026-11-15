#![allow(clippy::unwrap_used)]
use super::*;
use crate::sidebar_layout::ThreadsMode;
use crate::sync::{DeltaData, InitialData, RoomSync};
use crate::test_util::{fixture_workspace, me, msg, room, sub, ts};
use rocketchat::models::{Paginated, Permission};

#[test]
fn initial_from_fixtures() {
    let mut ws = fixture_workspace();
    assert!(ws.me().is_some());
    assert!(ws.rooms().len() >= 3);
    assert!(ws.room("GENERAL").unwrap().sub.is_some());
    assert!(!ws.permissions().is_empty());
    let c = ws.take_changes();
    assert!(c.areas.contains(Dirty::ROOMS | Dirty::SETTINGS));
    assert!(ws.take_changes().is_empty());
}

#[test]
fn messages_are_rewritten_in_place() {
    let mut ws = Workspace::new();
    let mut r = room("r1", "one", RoomType::Private);
    r.last_message = Some(Box::new(msg("last", "r1", 5, "u")));
    ws.apply_room(Change::Inserted, r);
    ws.open_room("r1");
    ws.apply_message(msg("a", "r1", 1, "u"));
    ws.open_thread("r1", "a");
    let mut reply = msg("b", "r1", 2, "u");
    reply.tmid = Some("a".into());
    ws.apply_message(reply);
    ws.take_changes();
    let v = ws.timeline("r1").unwrap().version();
    assert_eq!(ws.rooms_with_timelines(), HashSet::from(["r1".to_owned()]));
    let changed = ws.rewrite_messages("r1", &mut |m| {
        m.msg = m.msg.to_uppercase();
        true
    });
    assert!(changed);
    let c = ws.take_changes();
    assert!(c.timelines.contains("r1") && c.threads.contains("a") && c.rooms.contains("r1"));
    assert!(ws.timeline("r1").unwrap().version() > v);
    assert_eq!(
        ws.timeline("r1").unwrap().get("a").unwrap().message.msg,
        "TEXT A"
    );
    assert_eq!(
        ws.thread("a").unwrap().get("b").unwrap().message.msg,
        "TEXT B"
    );
    assert_eq!(ws.thread("a").unwrap().parent().unwrap().msg, "TEXT A");
    // Nothing changed: no marks, no new version.
    assert!(!ws.rewrite_messages("r1", &mut |_| false));
    assert!(ws.take_changes().is_empty());
    assert!(ws.rewrite_last_messages(&mut |m| {
        m.msg.clear();
        true
    }));
    assert!(ws.take_changes().rooms.contains("r1"));
    let entry = ws.room("r1").unwrap();
    assert_eq!(
        entry
            .room
            .as_ref()
            .unwrap()
            .last_message
            .as_ref()
            .unwrap()
            .msg,
        ""
    );
}

#[test]
fn subscription_and_room_changes() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    let c = ws.take_changes();
    assert!(c.areas.contains(Dirty::ROOMS));
    assert!(c.rooms.contains("r1"));
    // Same data again: no change.
    ws.apply_subscription(Change::Updated, sub("r1", "one", RoomType::Channel));
    assert!(ws.take_changes().is_empty());
    let rev = ws.revision(Dirty::ROOMS);
    ws.apply_room(Change::Inserted, room("r1", "one", RoomType::Channel));
    assert!(ws.revision(Dirty::ROOMS) > rev);
    assert!(ws.room("r1").unwrap().room.is_some());
    ws.open_room("r1");
    ws.apply_subscription(Change::Removed, sub("r1", "one", RoomType::Channel));
    assert!(ws.room("r1").is_none());
    assert!(ws.timeline("r1").is_none());
}

#[test]
fn unread_divider_follows_ls_on_request() {
    let mut ws = Workspace::new();
    let mut s = sub("r1", "one", RoomType::Channel);
    s.ls = Some(ts(1000));
    ws.apply_subscription(Change::Inserted, s.clone());
    ws.open_room("r1");
    ws.take_changes();
    // A newer `ls` alone leaves the divider captured at open.
    s.ls = Some(ts(5000));
    ws.apply_subscription(Change::Updated, s);
    ws.take_changes();
    assert_eq!(ws.timeline("r1").unwrap().unread_since(), Some(ts(1000)));
    ws.refresh_unread_since("r1");
    assert_eq!(ws.timeline("r1").unwrap().unread_since(), Some(ts(5000)));
    assert!(ws.take_changes().timelines.contains("r1"));
    // Unchanged: no mark; closed rooms are ignored.
    ws.refresh_unread_since("r1");
    assert!(ws.take_changes().is_empty());
    ws.refresh_unread_since("unknown");
}

#[test]
fn previewed_rooms_are_forgotten_joined_ones_kept() {
    let mut ws = Workspace::new();
    ws.apply_room(Change::Updated, room("p1", "preview", RoomType::Channel));
    ws.open_room("p1");
    ws.apply_message(msg("m1", "p1", 1000, "u"));
    ws.take_changes();
    assert!(ws.forget_preview("p1"));
    assert!(ws.room("p1").is_none());
    assert!(ws.timeline("p1").is_none());
    assert!(ws.take_changes().rooms.contains("p1"));
    // Joined: kept.
    ws.apply_room(Change::Updated, room("r1", "one", RoomType::Channel));
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    assert!(!ws.forget_preview("r1"));
    assert!(ws.room("r1").is_some());
    assert!(!ws.forget_preview("unknown"));
}

#[test]
fn messages_go_to_open_timelines_only() {
    let mut ws = Workspace::new();
    ws.apply_message(msg("m0", "r1", 1, "u"));
    assert!(ws.timeline("r1").is_none());
    let mut s = sub("r1", "one", RoomType::Channel);
    s.ls = Some(ts(1500));
    ws.apply_subscription(Change::Inserted, s);
    ws.open_room("r1");
    ws.take_changes();
    ws.apply_message(msg("m1", "r1", 1000, "u"));
    ws.apply_message(msg("m2", "r1", 2000, "u"));
    let c = ws.take_changes();
    assert!(c.timelines.contains("r1"));
    let t = ws.timeline("r1").unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t.unread_divider("me"), Some("m2"));
    ws.apply_message_deleted("r1", "m1");
    assert_eq!(ws.timeline("r1").unwrap().len(), 1);
}

#[test]
fn threads() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    ws.open_room("r1");
    ws.apply_message(msg("p", "r1", 1000, "u"));
    ws.open_thread("r1", "p");
    assert_eq!(ws.thread("p").unwrap().parent().unwrap().id, "p");
    let mut reply = msg("c1", "r1", 2000, "v");
    reply.tmid = Some("p".into());
    ws.apply_message(reply);
    let mut parent = msg("p", "r1", 1000, "u");
    parent.tcount = Some(1);
    parent.replies = Some(vec!["v".into()]);
    ws.apply_message(parent);
    let c = ws.take_changes();
    assert!(c.threads.contains("p"));
    assert_eq!(ws.thread("p").unwrap().len(), 1);
    assert_eq!(ws.thread("p").unwrap().parent().unwrap().tcount, Some(1));
    let room = ws.timeline("r1").unwrap();
    assert_eq!(room.len(), 1, "reply not in main timeline");
    assert_eq!(room.get("p").unwrap().message.tcount, Some(1));
    ws.apply_message_deleted("r1", "c1");
    assert!(ws.thread("p").unwrap().is_empty());
}

#[test]
fn pending_flow() {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    ws.open_room("r1");
    let local = ws.new_local_message("r1", "hi", None, false);
    let id = local.id.clone();
    ws.add_pending(local.clone());
    assert!(ws.timeline("r1").unwrap().get(&id).unwrap().is_pending());
    ws.mark_pending_failed("r1", None, &id, "offline");
    ws.mark_pending_sending("r1", None, &id);
    let mut echo = local;
    echo.temp = None;
    ws.apply_message(echo);
    assert!(!ws.timeline("r1").unwrap().get(&id).unwrap().is_pending());
}

#[test]
fn typing_and_presence() {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    let now = Instant::now();
    ws.apply_user_activity_at("r1", "me", &["user-typing".into()], None, now);
    assert!(
        ws.typing_label("r1", None, now).is_none(),
        "own activity ignored"
    );
    ws.apply_user_activity_at("r1", "bob", &["user-typing".into()], None, now);
    assert_eq!(ws.typing_label("r1", None, now).unwrap().usernames, ["bob"]);
    // Bob's message stops his typing.
    let mut m = msg("m", "r1", 1, "bobid");
    m.u.username = Some("bob".into());
    ws.apply_message(m);
    assert!(ws.typing_label("r1", None, now).is_none());

    ws.apply_user_status("me", Some("me"), UserStatus::Busy, Some("focus"));
    assert_eq!(ws.my_status(), UserStatus::Busy);
    assert_eq!(ws.me().unwrap().status_text.as_deref(), Some("focus"));
    assert!(
        ws.take_changes()
            .areas
            .contains(Dirty::PRESENCE | Dirty::ME | Dirty::TYPING)
    );
}

/// Settings, permissions and roles changed in realtime while a snapshot
/// loaded are not overwritten by it; older changes are.
#[test]
fn realtime_config_survives_snapshots() {
    let mut ws = Workspace::new();
    let before = Instant::now();
    // Changed before the snapshot started: the snapshot wins.
    ws.apply_setting(
        Change::Updated,
        Setting {
            id: "Old".into(),
            value: serde_json::json!("realtime"),
            ..Default::default()
        },
    );
    std::thread::sleep(std::time::Duration::from_millis(2));
    let started = Instant::now();
    assert!(started > before);
    ws.apply_setting(
        Change::Updated,
        Setting {
            id: "Site_Name".into(),
            value: serde_json::json!("New name"),
            ..Default::default()
        },
    );
    ws.apply_permission(
        Change::Updated,
        Permission {
            id: "create-c".into(),
            roles: vec!["admin".into(), "user".into()],
            ..Default::default()
        },
    );
    ws.apply_role(
        Change::Removed,
        Role {
            id: "moderator".into(),
            ..Default::default()
        },
    );
    let data = InitialData {
        me: me("me", "me"),
        settings: vec![
            Setting {
                id: "Site_Name".into(),
                value: serde_json::json!("Stale"),
                ..Default::default()
            },
            Setting {
                id: "Old".into(),
                value: serde_json::json!("snapshot"),
                ..Default::default()
            },
        ],
        permissions: vec![Permission {
            id: "create-c".into(),
            roles: vec!["admin".into()],
            ..Default::default()
        }],
        roles: vec![Role {
            id: "moderator".into(),
            name: "Moderator".into(),
            ..Default::default()
        }],
        started_at: Some(started),
        ..Default::default()
    };
    ws.apply_initial(data);
    assert_eq!(ws.settings().str("Site_Name"), Some("New name"));
    assert_eq!(ws.settings().str("Old"), Some("snapshot"));
    assert_eq!(
        ws.permission_roles("create-c"),
        Some(&["admin".to_owned(), "user".to_owned()][..])
    );
    assert!(!ws.roles().contains_key("moderator"));
    // A later snapshot (started after those changes) wins again.
    let data = InitialData {
        me: me("me", "me"),
        settings: vec![Setting {
            id: "Site_Name".into(),
            value: serde_json::json!("Fresh"),
            ..Default::default()
        }],
        started_at: Some(Instant::now()),
        ..Default::default()
    };
    ws.apply_initial(data);
    assert_eq!(ws.settings().str("Site_Name"), Some("Fresh"));
}

#[test]
fn delta_removes_by_subscription_id() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    ws.apply_subscription(Change::Inserted, sub("r2", "two", RoomType::Channel));
    // The delta request starts after these realtime inserts.
    let mut d = DeltaData {
        next_since: Some(ts(99)),
        started_at: Some(Instant::now()),
        ..Default::default()
    };
    d.subscriptions.remove.push(rocketchat::models::Removed {
        id: "sub-r1".into(),
        rid: None,
        deleted_at: None,
    });
    d.rooms.remove.push(rocketchat::models::Removed {
        id: "r2".into(),
        rid: None,
        deleted_at: None,
    });
    ws.apply_delta(d);
    assert!(ws.rooms().is_empty());
    assert_eq!(ws.last_sync(), Some(ts(99)));
}

#[test]
fn room_sync_and_read_receipts() {
    let mut ws = Workspace::new();
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    ws.open_room("r1");
    let mut a = msg("a", "r1", 1000, "u");
    a.unread = Some(true);
    let mut b = msg("b", "r1", 2000, "u");
    b.unread = Some(true);
    ws.apply_room_sync(RoomSync {
        rid: "r1".into(),
        updated: vec![a, b, msg("c", "r1", 3000, "u")],
        deleted: vec!["c".into()],
        dropped: 0,
    });
    assert_eq!(ws.timeline("r1").unwrap().len(), 2);
    ws.apply_messages_read("r1", Some(ts(1001)), None);
    let t = ws.timeline("r1").unwrap();
    assert_eq!(t.get("a").unwrap().message.unread, None);
    assert_eq!(t.get("b").unwrap().message.unread, Some(true));
}

#[test]
fn drafts_and_settings() {
    let mut ws = Workspace::new();
    ws.set_draft("r1", None, "hello");
    ws.set_draft("r1", Some("t"), "in thread");
    assert_eq!(ws.draft("r1", None), Some("hello"));
    assert_eq!(ws.draft("r1", Some("t")), Some("in thread"));
    ws.set_draft("r1", None, "  ");
    assert_eq!(ws.draft("r1", None), None);
    assert_eq!(ws.take_draft("r1", Some("t")).as_deref(), Some("in thread"));

    ws.apply_setting(
        Change::Updated,
        Setting {
            id: "UI_Use_Real_Name".into(),
            value: serde_json::json!(true),
            ..Default::default()
        },
    );
    assert!(ws.use_real_names(None));
    assert!(!ws.use_real_names(Some(false)));
}

#[test]
fn role_changes() {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
    ws.apply_user_role_change("me", "admin", true, Some("Users"));
    assert!(ws.me().unwrap().has_role("admin"));
    ws.apply_user_role_change("me", "owner", true, Some("r1"));
    assert_eq!(
        ws.subscription("r1").unwrap().roles.as_deref(),
        Some(&["owner".to_owned()][..])
    );
    ws.apply_user_role_change("me", "admin", false, None);
    assert!(!ws.me().unwrap().has_role("admin"));
    ws.apply_user_role_change("other", "admin", true, None);
    assert!(!ws.me().unwrap().has_role("admin"));
}

fn thread_parent(id: &str, rid: &str, tlm: i64, replies: &[&str], updated: i64) -> Message {
    let mut m = msg(id, rid, 1, "bob");
    m.tcount = Some(1);
    m.tlm = Some(ts(tlm));
    m.replies = Some(replies.iter().map(|r| (*r).to_owned()).collect());
    m.updated_at = Some(ts(updated));
    m
}

fn threads_ws(mode: ThreadsMode) -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    ws.apply_subscription(Change::Inserted, sub("r", "room", RoomType::Channel));
    ws.set_thread_config(mode, []);
    ws.take_changes();
    ws
}

fn followed(ws: &Workspace, rid: &str) -> Vec<String> {
    ws.thread_index()
        .followed(rid)
        .map(|f| f.tmids.iter().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn live_parent_updates_keep_followed_sets_current() {
    let mut ws = threads_ws(ThreadsMode::Following);
    ws.apply_followed(
        "r",
        vec![thread_parent("a", "r", 10, &["me"], 10)],
        None,
        Instant::now(),
    );
    assert_eq!(followed(&ws, "r"), ["a"]);
    // A reply in a thread the user joins re-broadcasts its parent.
    ws.apply_message(thread_parent("b", "r", 20, &["bob", "me"], 20));
    assert_eq!(followed(&ws, "r"), ["a", "b"]);
    assert!(ws.thread_index().parent("b").is_some());
    // Unfollowed elsewhere: the parent arrives without "me".
    ws.apply_message(thread_parent("a", "r", 10, &["bob"], 30));
    assert_eq!(followed(&ws, "r"), ["b"]);
    // A reply bumps the parent's activity.
    let mut reply = msg("x", "r", 40, "bob");
    reply.tmid = Some("b".into());
    ws.apply_message(reply);
    assert_eq!(ws.thread_index().parent("b").unwrap().tlm, Some(ts(40)));
    // Deleting a parent with replies turns it into `t: "rm"`.
    let mut rm = thread_parent("b", "r", 40, &["bob", "me"], 50);
    rm.t = Some("rm".into());
    rm.msg = String::new();
    ws.apply_message(rm);
    assert!(followed(&ws, "r").is_empty());
    assert!(
        ws.thread_index().parent("b").is_none(),
        "not unread or kept"
    );
    // A plain deletion removes it too.
    ws.apply_message(thread_parent("c", "r", 60, &["me"], 60));
    assert_eq!(followed(&ws, "r"), ["c"]);
    ws.apply_message_deleted("r", "c");
    assert!(followed(&ws, "r").is_empty());
    assert!(ws.thread_index().parent("c").is_none());
    // Parents of uncrawled rooms are stored, not added to any set.
    ws.apply_subscription(Change::Inserted, sub("s", "other", RoomType::Channel));
    ws.apply_message(thread_parent("d", "s", 70, &["me"], 70));
    assert!(ws.thread_index().followed("s").is_none());
    assert!(ws.thread_index().parent("d").is_some());
    // Leaving the room drops its parents and set.
    ws.apply_subscription(Change::Removed, sub("r", "room", RoomType::Channel));
    assert!(ws.thread_index().followed("r").is_none());
    assert!(ws.thread_index().parents().all(|p| p.rid != "r"));
}

#[test]
fn a_followed_set_keeps_live_changes_made_during_the_request() {
    let mut ws = threads_ws(ThreadsMode::Following);
    let started = Instant::now();
    // While the request runs: "a" is unfollowed, "b" followed, "c" replied to.
    ws.apply_message(thread_parent("a", "r", 10, &["bob"], 30));
    ws.apply_message(thread_parent("b", "r", 20, &["me"], 30));
    ws.apply_message(thread_parent("c", "r", 25, &["me"], 30));
    // The response was read before: "a" followed (older copy), "b" not.
    ws.apply_followed(
        "r",
        vec![
            thread_parent("a", "r", 10, &["me"], 10),
            thread_parent("c", "r", 5, &["me"], 5),
        ],
        Some(ts(99)),
        started,
    );
    assert_eq!(followed(&ws, "r"), ["b", "c"]);
    assert_eq!(
        ws.thread_index().parent("c").unwrap().tlm,
        Some(ts(25)),
        "the older REST copy does not replace the live one"
    );
    assert_eq!(
        ws.thread_index().followed("r").unwrap().synced_activity,
        Some(ts(99))
    );
}

#[test]
fn hidden_parents_are_gone() {
    let mut ws = threads_ws(ThreadsMode::Following);
    ws.apply_followed(
        "r",
        vec![thread_parent("a", "r", 10, &["me"], 10)],
        None,
        Instant::now(),
    );
    let mut h = thread_parent("a", "r", 10, &["me"], 20);
    h.hidden = Some(true);
    ws.apply_message(h.clone());
    assert!(ws.thread_index().is_gone("a"));
    assert!(ws.thread_index().parent("a").is_none());
    assert!(followed(&ws, "r").is_empty());
    // A hidden REST result too.
    h.id = "b".into();
    ws.apply_thread_parents(vec![h]);
    assert!(ws.thread_index().is_gone("b"));
    // A visible copy brings it back.
    ws.apply_thread_parents(vec![thread_parent("b", "r", 10, &[], 30)]);
    assert!(!ws.thread_index().is_gone("b"));
}

#[test]
fn a_live_follow_in_a_full_room_replaces_the_oldest() {
    let mut ws = threads_ws(ThreadsMode::Following);
    let n = crate::thread_index::FOLLOWED_PER_ROOM as i64;
    let crawl = (0..n)
        .map(|i| thread_parent(&format!("t{i:02}"), "r", 100 + i, &["me"], 100 + i))
        .collect();
    ws.apply_followed("r", crawl, None, Instant::now());
    ws.take_changes();
    ws.apply_message(thread_parent("new", "r", 500, &["bob", "me"], 500));
    let set = followed(&ws, "r");
    assert_eq!(set.len(), n as usize);
    assert!(set.contains(&"new".to_owned()) && !set.contains(&"t00".to_owned()));
    assert!(ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
}

#[test]
fn a_hidden_parent_is_not_revived_by_an_older_rest_copy() {
    let mut ws = threads_ws(ThreadsMode::Following);
    let started = Instant::now();
    ws.apply_followed(
        "r",
        vec![thread_parent("a", "r", 10, &["me"], 10)],
        None,
        started,
    );
    let mut h = thread_parent("a", "r", 10, &["me"], 20);
    h.hidden = Some(true);
    ws.apply_message(h);
    // Read on the server before the hide, applied after it.
    ws.apply_followed(
        "r",
        vec![thread_parent("a", "r", 10, &["me"], 10)],
        None,
        started,
    );
    ws.apply_thread_parents(vec![thread_parent("a", "r", 10, &["me"], 10)]);
    assert!(ws.thread_index().is_gone("a"));
    assert!(ws.thread_index().parent("a").is_none());
    assert!(followed(&ws, "r").is_empty());
}

#[test]
fn pages_and_cached_parents_do_not_change_followed_sets() {
    let mut ws = threads_ws(ThreadsMode::Following);
    ws.apply_followed(
        "r",
        vec![thread_parent("a", "r", 10, &["me"], 10)],
        None,
        Instant::now(),
    );
    // Unfollowed elsewhere; a stale cached copy still lists "me".
    ws.apply_message(thread_parent("a", "r", 10, &["bob"], 30));
    // A crawl in flight while the cache is seeded.
    let started = Instant::now();
    ws.open_thread("r", "b");
    ws.seed_cached_messages(
        "r",
        Some("b"),
        Some(thread_parent("b", "r", 10, &["me"], 5)),
        Vec::new(),
    );
    assert!(followed(&ws, "r").is_empty());
    ws.apply_followed("r", Vec::new(), None, started);
    assert!(followed(&ws, "r").is_empty(), "no live mark recorded");
}

#[test]
fn unread_and_kept_parents_are_indexed_from_pages_and_threads() {
    let mut ws = threads_ws(ThreadsMode::UnreadKept);
    let mut s = sub("r", "room", RoomType::Channel);
    s.tunread = Some(vec!["u".into()]);
    ws.apply_subscription(Change::Updated, s);
    ws.set_thread_config(ThreadsMode::UnreadKept, [("r", "k")]);
    ws.open_room("r");
    let c = ws.begin_load_older("r").unwrap();
    ws.finish_load_older(
        "r",
        c,
        Ok(vec![
            thread_parent("u", "r", 10, &[], 10),
            thread_parent("k", "r", 10, &[], 10),
            thread_parent("other", "r", 10, &[], 10),
        ]),
    );
    let idx = ws.thread_index();
    assert!(idx.parent("u").is_some() && idx.parent("k").is_some());
    assert!(idx.parent("other").is_none(), "nothing refers to it");
    // Opening a thread indexes its parent, and seeds it from the index.
    ws.open_thread("r", "other");
    assert!(ws.thread_index().parent("other").is_some());
    ws.close_room("r");
    ws.close_thread("other");
    ws.open_thread("r", "u");
    assert_eq!(ws.thread("u").unwrap().parent().unwrap().id, "u");
    // A thread page's replies bump the parent's activity.
    ws.begin_load_thread("u").unwrap();
    let mut reply = msg("x", "r", 50, "bob");
    reply.tmid = Some("u".into());
    ws.finish_load_thread(
        "u",
        Ok(Paginated {
            items: vec![reply],
            ..Default::default()
        }),
    );
    assert_eq!(ws.thread_index().parent("u").unwrap().tlm, Some(ts(50)));
}

#[test]
fn thread_index_is_marked_only_on_real_changes() {
    let mut ws = threads_ws(ThreadsMode::Unread);
    let mut s = sub("r", "room", RoomType::Channel);
    s.tunread = Some(vec!["a".into()]);
    ws.apply_subscription(Change::Updated, s.clone());
    ws.take_changes();
    let rev = ws.revision(Dirty::THREAD_INDEX);
    let p = thread_parent("a", "r", 10, &[], 10);
    ws.apply_message(p.clone());
    assert!(ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
    ws.apply_message(p.clone());
    assert!(!ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
    // Unrelated parents and messages.
    ws.apply_message(thread_parent("z", "r", 10, &["me"], 10));
    ws.apply_message(msg("m", "r", 5, "bob"));
    ws.set_thread_config(ThreadsMode::Unread, []);
    assert!(!ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
    assert_eq!(ws.revision(Dirty::THREAD_INDEX), rev + 1);
    // Off: nothing is indexed, everything is dropped.
    ws.set_thread_config(ThreadsMode::Off, []);
    assert!(ws.thread_index().is_empty());
    ws.take_changes();
    ws.apply_message(thread_parent("a", "r", 20, &[], 20));
    assert!(ws.thread_index().is_empty());
    assert!(!ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
    assert!(Dirty::ALL.contains(Dirty::THREAD_INDEX));
}

#[test]
fn reserved_dirty_bits_have_their_own_revisions() {
    let all = [
        Dirty::FLAGS,
        Dirty::LATER,
        Dirty::REMINDERS,
        Dirty::MENTION_GROUPS,
        Dirty::FEEDS,
    ];
    let mut ws = Workspace::new();
    for (i, d) in all.into_iter().enumerate() {
        assert!(Dirty::ALL.contains(d));
        assert!(all[..i].iter().all(|o| !o.intersects(d)), "{i}");
        let rev = ws.revision(d);
        ws.mark(d);
        assert_eq!(ws.revision(d), rev + 1);
        assert!(ws.take_changes().areas.contains(d));
    }
    assert_eq!(Dirty::ALL.bits().count_ones() as usize, Dirty::BITS);
}

fn starred(mut m: Message, by: &[&str]) -> Message {
    m.starred = Some(
        by.iter()
            .map(|u| rocketchat::models::IdRef {
                id: (*u).to_owned(),
            })
            .collect(),
    );
    m
}

fn starred_tmids(ws: &Workspace) -> Vec<&str> {
    ws.thread_index().starred().map(|(_, t)| t).collect()
}

#[test]
fn starred_parents_are_tracked_with_threads_off() {
    let mut ws = threads_ws(ThreadsMode::Off);
    ws.set_starred_threads(true);
    assert!(ws.take_changes().areas.contains(Dirty::THREAD_INDEX));
    // Someone else's star (realtime copies list every member's stars).
    ws.apply_message(starred(thread_parent("o", "r", 10, &[], 10), &["bob"]));
    // A starred message without replies is not a thread.
    let mut plain = starred(msg("p", "r", 5, "bob"), &["me"]);
    plain.updated_at = Some(ts(5));
    ws.apply_message(plain);
    assert!(ws.thread_index().is_empty());
    ws.apply_message(starred(
        thread_parent("s", "r", 10, &[], 10),
        &["bob", "me"],
    ));
    assert_eq!(starred_tmids(&ws), ["s"]);
    assert!(ws.thread_index().parent("s").is_some());
    // A reply bumps its activity.
    let mut reply = msg("x", "r", 40, "bob");
    reply.tmid = Some("s".into());
    ws.apply_message(reply);
    assert_eq!(ws.thread_index().parent("s").unwrap().tlm, Some(ts(40)));
    // Unstarred: dropped (nothing else needs it with Threads off).
    ws.apply_message(starred(thread_parent("s", "r", 40, &[], 50), &[]));
    assert!(starred_tmids(&ws).is_empty());
    assert!(ws.thread_index().is_empty());
    // An older copy (read before the unstar) does not star it again.
    ws.set_thread_starred("r", "s", true);
    ws.apply_message(starred(thread_parent("s", "r", 40, &[], 60), &[]));
    ws.apply_thread_parents(vec![starred(thread_parent("s", "r", 40, &[], 55), &["me"])]);
    assert!(starred_tmids(&ws).is_empty());
    // Starring a parent shown elsewhere keeps it when Threads go off.
    let mut ws = threads_ws(ThreadsMode::Following);
    ws.set_starred_threads(true);
    ws.apply_message(starred(thread_parent("a", "r", 10, &["me"], 10), &["me"]));
    ws.apply_message(thread_parent("b", "r", 10, &["me"], 10));
    ws.set_thread_config(ThreadsMode::Off, []);
    assert!(ws.thread_index().parent("a").is_some());
    assert!(ws.thread_index().parent("b").is_none());
}

/// A scan requested after an own (un)star but read on the server before
/// it does not undo it once the echo confirmed it.
#[test]
fn star_echoes_outlast_scans_read_before_them() {
    for mode in [ThreadsMode::Off, ThreadsMode::Following] {
        let mut ws = threads_ws(mode);
        ws.set_starred_threads(true);
        // Starred, its parent not loaded (e.g. known from a scan).
        ws.set_thread_starred("r", "x", true);
        assert_eq!(starred_tmids(&ws), ["x"]);
        // Unstarred from the panel, then a scan starts, then the echo.
        ws.set_thread_starred("r", "x", false);
        let started = Instant::now();
        ws.apply_message(starred(thread_parent("x", "r", 10, &[], 20), &[]));
        assert!(starred_tmids(&ws).is_empty(), "{mode:?}");
        // The page was read before the unstar write.
        let stale = vec![starred(thread_parent("x", "r", 10, &[], 10), &["me"])];
        let found = ws.apply_starred_page("r", stale, started);
        ws.finish_starred_scan("r", found, started, Some(ts(10)));
        assert!(starred_tmids(&ws).is_empty(), "{mode:?}");

        // The mirror case: starred, scan started, echo, stale page.
        ws.set_thread_starred("r", "x", true);
        let started = Instant::now();
        ws.apply_message(starred(thread_parent("x", "r", 10, &[], 30), &["me"]));
        let stale = vec![starred(thread_parent("x", "r", 10, &[], 20), &[])];
        let found = ws.apply_starred_page("r", stale, started);
        ws.finish_starred_scan("r", found, started, Some(ts(10)));
        assert_eq!(starred_tmids(&ws), ["x"], "{mode:?}");
    }
}

#[test]
fn starred_threads_need_starring_and_threads_on_the_server() {
    for id in ["Message_AllowStarring", "Threads_enabled"] {
        let mut ws = threads_ws(ThreadsMode::Off);
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: id.into(),
                value: serde_json::json!(false),
                ..Default::default()
            },
        );
        ws.set_starred_threads(true);
        assert!(!ws.thread_index().indexing(), "{id}");
        ws.apply_message(starred(thread_parent("s", "r", 10, &[], 10), &["me"]));
        assert!(ws.thread_index().is_empty(), "{id}");
    }
    let mut ws = threads_ws(ThreadsMode::Off);
    ws.set_starred_threads(false);
    ws.apply_message(starred(thread_parent("s", "r", 10, &[], 10), &["me"]));
    assert!(ws.thread_index().is_empty());
}

#[test]
fn starred_scans_and_gone_parents() {
    let mut ws = threads_ws(ThreadsMode::Off);
    ws.set_starred_threads(true);
    ws.set_thread_starred("r", "old", true);
    let started = Instant::now();
    let page = vec![
        starred(thread_parent("a", "r", 10, &[], 10), &["me"]),
        // Starred, but not a thread: ignored.
        starred(msg("m", "r", 5, "bob"), &["me"]),
        // Another room's message: ignored.
        starred(thread_parent("z", "other", 10, &[], 10), &["me"]),
    ];
    let found = ws.apply_starred_page("r", page, started);
    assert_eq!(found, ["a"]);
    assert_eq!(
        starred_tmids(&ws),
        ["a", "old"],
        "shown before the scan ends"
    );
    ws.finish_starred_scan("r", found, started, Some(ts(10)));
    assert_eq!(starred_tmids(&ws), ["a"]);
    let scan = ws.thread_index().starred_scan("r").unwrap();
    assert_eq!(scan.lm, Some(ts(10)));
    assert!(scan.scanned_at.is_some());
    assert!(ws.thread_index().parent("a").is_some());
    assert!(ws.thread_index().parent("m").is_none());
    // Not found: no longer starred.
    ws.thread_gone("a");
    assert!(starred_tmids(&ws).is_empty());
    // Hidden: no longer starred.
    ws.set_thread_starred("r", "h", true);
    let mut h = thread_parent("h", "r", 10, &[], 10);
    h.hidden = Some(true);
    ws.apply_message(h);
    assert!(starred_tmids(&ws).is_empty());
    // Deleted with replies (`t: "rm"`): stays while starred.
    let mut rm = starred(thread_parent("d", "r", 10, &[], 10), &["me"]);
    rm.t = Some("rm".into());
    ws.apply_message(rm);
    assert_eq!(starred_tmids(&ws), ["d"]);
    assert!(ws.thread_index().parent("d").is_some());
    ws.forget_starred_scan("r");
    assert!(ws.thread_index().starred_scan("r").is_none());
    // Logging out forgets them.
    ws.apply_logged_out();
    assert!(starred_tmids(&ws).is_empty());
}

#[test]
fn connection_clears_typing() {
    let mut ws = Workspace::new();
    let now = Instant::now();
    ws.apply_user_activity_at("r", "bob", &["user-typing".into()], None, now);
    ws.apply_connection(ConnectionState::Connected);
    ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
    assert!(ws.typing_label("r", None, now).is_none());
    ws.apply_logged_out();
    assert_eq!(ws.connection(), &ConnectionState::LoggedOut);
}

/// A room someone else got created in the user's name (`teams.create` with
/// `owner` whose team insert failed) has the shape of a service room. Its
/// flags count only once the account's own preferences bind it.
#[test]
fn unbound_service_rooms_hold_no_flags() {
    use crate::flags::FlagColor;
    use rocketchat::models::{FLAGS_KEY, ServiceFields, UNBOUND_CUTOFF_MS};
    use serde_json::json;

    let mut fields = ServiceFields::new("u1");
    fields.set(
        FLAGS_KEY,
        json!({ "rooms": { "A": { "c": "red", "t": 5 } } }),
    );
    let mut svc = sub("SVC", "rustket-sync-u1", RoomType::Private);
    svc.u.id = "u1".into();
    svc.open = false;
    svc.roles = Some(vec!["owner".into()]);
    svc.custom_fields = Some(fields.to_custom_fields());
    let mut svc_room = crate::test_util::service_room(&svc, "u1", 1);
    svc_room.ts = Some(ts(UNBOUND_CUTOFF_MS + 1));
    let bound = |rid: &str| {
        let mut u = me("u1", "alice");
        u.settings = serde_json::from_value(json!({
            "preferences": { "dontAskAgainList": [
                { "action": "hideRoom", "label": "Hide room" },
                { "action": format!("rustket-service-room:{rid}"), "label": "Rustket sync" },
            ] },
        }))
        .unwrap();
        u
    };

    let mut ws = Workspace::new();
    ws.apply_me(me("u1", "alice"));
    ws.apply_subscription(Change::Inserted, sub("A", "a", RoomType::Channel));
    ws.apply_subscription(Change::Inserted, svc);
    ws.apply_room(Change::Inserted, svc_room);
    assert!(ws.is_service_room("SVC"), "its shape keeps it out of lists");
    assert!(ws.service_rooms().is_empty());
    assert_eq!(ws.room_flag("A"), None);

    // Another room is bound: still not this one.
    ws.apply_me(bound("OTHER"));
    assert!(ws.service_rooms().is_empty());
    assert_eq!(ws.room_flag("A"), None);

    // The account binds it (a `/me` re-read after the preference change).
    ws.apply_me(bound("SVC"));
    assert_eq!(ws.service_rooms(), ["SVC"]);
    assert_eq!(ws.room_flag("A"), Some(FlagColor::Red));

    // The binding is removed again.
    ws.apply_me(me("u1", "alice"));
    assert!(ws.service_rooms().is_empty());
    assert_eq!(ws.room_flag("A"), None);
}

/// Mention groups come from the bound service room, with this device's
/// pending edits on top.
#[test]
fn mention_groups_come_from_the_service_room_and_the_overlay() {
    use crate::mention_groups::{MentionGroupBlob, MentionGroupEdit};
    use rocketchat::models::{MENTION_GROUPS_KEY, ServiceFields, UNBOUND_CUTOFF_MS};
    use serde_json::json;

    let mut fields = ServiceFields::new("u1");
    fields.set(
        MENTION_GROUPS_KEY,
        json!({ "v": 1, "groups": {
            "g1": { "n": "backend", "u": ["bob", "alice"], "t": 5, "d": "a" }
        } }),
    );
    let mut svc = sub("SVC", "rustket-sync-u1", RoomType::Private);
    svc.u.id = "u1".into();
    svc.open = false;
    svc.roles = Some(vec!["owner".into()]);
    svc.custom_fields = Some(fields.to_custom_fields());
    let mut svc_room = crate::test_util::service_room(&svc, "u1", 1);
    svc_room.ts = Some(ts(UNBOUND_CUTOFF_MS + 1));
    let mut bound = me("u1", "alice");
    bound.settings = serde_json::from_value(json!({
        "preferences": { "dontAskAgainList": [
            { "action": "rustket-service-room:SVC", "label": "Rustket sync" },
        ] },
    }))
    .unwrap();

    let mut ws = Workspace::new();
    ws.apply_me(me("u1", "alice"));
    ws.apply_subscription(Change::Inserted, svc);
    ws.apply_room(Change::Inserted, svc_room);
    assert!(ws.mention_groups().is_empty(), "not bound: not trusted");
    let rev = ws.revision(Dirty::MENTION_GROUPS);
    ws.apply_me(bound);
    assert!(ws.revision(Dirty::MENTION_GROUPS) > rev);
    assert_eq!(
        ws.mention_groups().get("backend").unwrap().members,
        ["alice", "bob"]
    );

    let mut overlay = MentionGroupBlob::default();
    let edit = MentionGroupEdit::Upsert {
        id: "g2".into(),
        name: "qa".into(),
        members: vec!["carol".into()],
    };
    overlay
        .edit(ws.mention_group_blob(), &edit, 20, "dev")
        .unwrap();
    ws.set_mention_group_overlay(overlay);
    assert_eq!(ws.mention_groups().all().len(), 2);
    assert_eq!(ws.mention_group_overlay().groups.len(), 1);
}
