#![allow(clippy::unwrap_used, clippy::expect_used)]

use rocketchat::models::{
    Message, MessageAttachment, Room, RoomType, Setting, Subscription, UpdatedSince, User,
};
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::e2e::shown;
use crate::test_util::{fixture, me, msg, room, sub, ts};
use crate::workspace::Workspace;

fn open(dir: &Path, user: &str) -> CacheDb {
    CacheDb::open(dir, user).unwrap().0
}

fn fixture_state() -> CachedState {
    let me: User = serde_json::from_value(fixture("me.json")).unwrap();
    let subs: UpdatedSince<Subscription> =
        serde_json::from_value(fixture("subscriptions_get.json")).unwrap();
    let rooms: UpdatedSince<Room> = serde_json::from_value(fixture("rooms_get.json")).unwrap();
    CachedState {
        me: Some(me),
        settings: vec![Setting {
            id: "Site_Name".into(),
            value: json!("Rustket test"),
            ..Default::default()
        }],
        subscriptions: subs.update,
        rooms: rooms.update,
        ..Default::default()
    }
}

fn timeline(rid: &str, msgs: Vec<Message>) -> CachedTimeline {
    CachedTimeline {
        key: TimelineKey::room(rid),
        parent: None,
        messages: msgs,
    }
}

fn messages(rid: &str, n: usize) -> Vec<Message> {
    (0..n)
        .map(|i| {
            msg(
                &format!("{rid}-m{i:03}"),
                rid,
                1_000_000 + i as i64 * 1000,
                "u1",
            )
        })
        .collect()
}

/// An E2E message as the app shows it after decrypting it in place.
fn decrypted(id: &str, rid: &str, secret: &str) -> Message {
    let mut m = msg(id, rid, 2_000_000, "u1");
    m.t = Some("e2e".into());
    m.content = Some(json!({"algorithm": "rc.v2.aes-sha2", "ciphertext": "Q0lQSEVSVEVYVA=="}));
    m.msg = secret.into();
    m.attachments = Some(vec![MessageAttachment {
        title: Some(format!("{secret}.png")),
        ..Default::default()
    }]);
    shown::set_marker(&mut m, shown::DECRYPTED);
    m
}

#[test]
fn schema_and_file_modes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("srv");
    let db = open(&dir, "u1");
    assert_eq!(db.path(), dir.join(FILE_NAME));
    let conn = Connection::open(db.path()).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let d = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        let f = std::fs::metadata(db.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(d, 0o700);
        assert_eq!(f, 0o600);
    }
    // Reopening an up-to-date database keeps its data.
    let mut db = db;
    db.write(Batch {
        settings: Some(fixture_state().settings),
        ..Default::default()
    })
    .unwrap();
    drop(db);
    let (db, report) = CacheDb::open(&dir, "u1").unwrap();
    assert_eq!(report, OpenReport::default());
    assert_eq!(db.load_state().unwrap().settings.len(), 1);
}

#[test]
fn newer_schema_is_moved_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("srv");
    drop(open(&dir, "u1"));
    let conn = Connection::open(dir.join(FILE_NAME)).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);
    let (db, report) = CacheDb::open(&dir, "u1").unwrap();
    assert_eq!(report.moved_aside, Some(dir.join("cache.sqlite.broken")));
    assert!(db.load_state().unwrap().is_empty());
}

#[test]
fn corrupt_database_is_moved_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("srv");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(FILE_NAME), vec![0x5au8; 8192]).unwrap();
    let (mut db, report) = CacheDb::open(&dir, "u1").unwrap();
    assert!(report.moved_aside.is_some());
    db.write(Batch {
        timelines: vec![timeline("R1", messages("R1", 3))],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        db.load_timeline(&TimelineKey::room("R1"))
            .unwrap()
            .unwrap()
            .messages
            .len(),
        3
    );
}

#[test]
fn state_round_trip_with_server_fixtures() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    let state = fixture_state();
    db.write(Batch {
        full: Some(Box::new(state.clone())),
        ..Default::default()
    })
    .unwrap();
    let loaded = db.load_state().unwrap();
    assert!(loaded.saved_at.is_some());
    let expect_subs: Vec<Subscription> = state
        .subscriptions
        .iter()
        .map(sanitize::subscription)
        .collect();
    let mut got_subs = loaded.subscriptions.clone();
    got_subs.sort_by(|a, b| a.rid.cmp(&b.rid));
    let mut expect_subs = expect_subs;
    expect_subs.sort_by(|a, b| a.rid.cmp(&b.rid));
    assert_eq!(got_subs, expect_subs);
    let mut got_rooms = loaded.rooms.clone();
    got_rooms.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expect_rooms: Vec<Room> = state.rooms.iter().map(sanitize::room).collect();
    expect_rooms.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(got_rooms, expect_rooms);
    assert_eq!(loaded.me, state.me.as_ref().map(sanitize::user));
    assert_eq!(loaded.settings, state.settings);

    // Partial updates: one room changes, one is gone.
    let mut changed = state.subscriptions[0].clone();
    changed.unread = 42;
    let gone = state.subscriptions[1].rid.clone();
    db.write(Batch {
        rooms: vec![
            (changed.rid.clone(), Some(changed.clone()), None),
            (gone.clone(), None, None),
        ],
        ..Default::default()
    })
    .unwrap();
    let loaded = db.load_state().unwrap();
    assert!(loaded.subscriptions.iter().any(|s| s.unread == 42));
    assert!(!loaded.subscriptions.iter().any(|s| s.rid == gone));
    assert!(!loaded.rooms.iter().any(|r| r.id == gone));
}

#[test]
fn timelines_keep_the_newest_messages_and_evict_lru() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    db.write(Batch {
        timelines: vec![timeline("R1", messages("R1", 150))],
        ..Default::default()
    })
    .unwrap();
    let t = db.load_timeline(&TimelineKey::room("R1")).unwrap().unwrap();
    assert_eq!(t.messages.len(), MESSAGES_PER_TIMELINE);
    assert_eq!(t.messages[0].id, "R1-m050");
    assert_eq!(t.messages.last().unwrap().id, "R1-m149");
    // A message of another room is never stored under this one.
    db.write(Batch {
        timelines: vec![timeline("R1", vec![msg("x", "R2", 5, "u1")])],
        ..Default::default()
    })
    .unwrap();
    assert!(
        db.load_timeline(&TimelineKey::room("R1"))
            .unwrap()
            .is_none()
    );

    // At most 3 timelines: the least recently used goes.
    db.set_limits(u64::MAX, 3);
    for rid in ["A", "B", "C"] {
        db.write(Batch {
            timelines: vec![timeline(rid, messages(rid, 2))],
            ..Default::default()
        })
        .unwrap();
    }
    // Reading A makes B the least recently used.
    assert!(db.load_timeline(&TimelineKey::room("A")).unwrap().is_some());
    db.write(Batch {
        timelines: vec![timeline("D", messages("D", 2))],
        ..Default::default()
    })
    .unwrap();
    let keys: Vec<String> = db
        .timeline_keys()
        .unwrap()
        .into_iter()
        .map(|k| k.rid)
        .collect();
    assert_eq!(keys, vec!["D", "A", "C"]);

    // Byte budget: only what fits stays.
    let per = db.message_bytes().unwrap() / 3;
    db.set_limits(per * 2 + per / 2, 100);
    db.write(Batch {
        timelines: vec![timeline("E", messages("E", 2))],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(db.timeline_keys().unwrap().len(), 2);
    assert!(db.message_bytes().unwrap() <= per * 2 + per / 2);
}

#[test]
fn thread_timelines_and_room_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    let parent = msg("P", "R1", 10, "u1");
    let mut reply = msg("r1", "R1", 20, "u2");
    reply.tmid = Some("P".into());
    db.write(Batch {
        timelines: vec![
            CachedTimeline {
                key: TimelineKey::thread("R1", "P"),
                parent: Some(parent.clone()),
                messages: vec![reply.clone()],
            },
            timeline("R1", vec![parent.clone()]),
        ],
        ..Default::default()
    })
    .unwrap();
    let t = db
        .load_timeline(&TimelineKey::thread("R1", "P"))
        .unwrap()
        .unwrap();
    assert_eq!(t.parent, Some(parent));
    assert_eq!(t.messages, vec![reply]);
    // The room is gone: its room and thread messages go with it.
    db.write(Batch {
        rooms: vec![("R1".into(), None, None)],
        ..Default::default()
    })
    .unwrap();
    assert!(db.timeline_keys().unwrap().is_empty());
    // A full snapshot drops messages of rooms it does not list.
    db.write(Batch {
        timelines: vec![timeline("R9", messages("R9", 1))],
        ..Default::default()
    })
    .unwrap();
    db.write(Batch {
        full: Some(Box::new(CachedState {
            subscriptions: vec![sub("R1", "one", RoomType::Channel)],
            ..Default::default()
        })),
        ..Default::default()
    })
    .unwrap();
    assert!(db.timeline_keys().unwrap().is_empty());
}

#[test]
fn another_account_starts_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    db.write(Batch {
        full: Some(Box::new(fixture_state())),
        timelines: vec![timeline("R1", messages("R1", 2))],
        outbox: Some(vec![OutboxEntry {
            message: msg("o1", "R1", 5, "u1"),
            queued: true,
            error: None,
        }]),
        ..Default::default()
    })
    .unwrap();
    drop(db);
    let (db, report) = CacheDb::open(tmp.path(), "u2").unwrap();
    assert!(report.user_changed);
    assert!(db.load_state().unwrap().is_empty());
    assert!(db.load_outbox().unwrap().is_empty());
    assert!(db.timeline_keys().unwrap().is_empty());
}

#[test]
fn outbox_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    let mut reply = msg("o2", "R1", 7, "u1");
    reply.tmid = Some("P".into());
    let entries = vec![
        OutboxEntry {
            message: msg("o1", "R1", 5, "u1"),
            queued: true,
            error: None,
        },
        OutboxEntry {
            message: reply,
            queued: false,
            error: Some("boom".into()),
        },
    ];
    db.write(Batch {
        outbox: Some(entries.clone()),
        interrupted_uploads: Some(vec!["photo.jpg".into()]),
        ..Default::default()
    })
    .unwrap();
    drop(db);
    let mut db = open(tmp.path(), "u1");
    assert_eq!(db.load_outbox().unwrap(), entries);
    assert_eq!(db.take_interrupted_uploads().unwrap(), vec!["photo.jpg"]);
    assert!(db.take_interrupted_uploads().unwrap().is_empty());
    // "Clear cache" keeps unsent messages.
    db.write(Batch {
        full: Some(Box::new(fixture_state())),
        ..Default::default()
    })
    .unwrap();
    db.clear().unwrap();
    assert!(db.load_state().unwrap().is_empty());
    assert_eq!(db.load_outbox().unwrap().len(), 2);
}

#[test]
fn e2e_plaintext_never_reaches_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    let secret = "TOPSECRETPLAINTEXT";
    let kept = decrypted("e1", "R1", secret);
    let mut no_envelope = decrypted("e2", "R1", secret);
    no_envelope.content = None;
    let mut pinned = msg("e3", "R1", 2_000_100, "u1");
    pinned.t = Some("message_pinned_e2e".into());
    pinned.attachments = Some(vec![MessageAttachment {
        text: Some(secret.into()),
        ..Default::default()
    }]);
    shown::set_marker(&mut pinned, shown::DECRYPTED);
    let mut r: Room = room("R1", "secret-room", RoomType::Private);
    r.encrypted = Some(true);
    r.last_message = Some(Box::new(decrypted("e4", "R1", secret)));
    let mut s = sub("R1", "secret-room", RoomType::Private);
    s.e2e_key = Some("ENCRYPTEDROOMKEY".into());
    let mut user = me("u1", "alice");
    user.e2e = Some(json!({"private_key": "PRIVATEKEYMATERIAL"}));
    let mut plain_pending = msg("o1", "R1", 5, "u1");
    plain_pending.msg = secret.into();
    plain_pending.t = Some("e2e".into());
    db.write(Batch {
        full: Some(Box::new(CachedState {
            me: Some(user),
            subscriptions: vec![s],
            rooms: vec![r],
            ..Default::default()
        })),
        timelines: vec![CachedTimeline {
            key: TimelineKey::room("R1"),
            parent: None,
            messages: vec![kept, no_envelope, pinned],
        }],
        outbox: Some(vec![OutboxEntry {
            message: plain_pending,
            queued: true,
            error: None,
        }]),
        ..Default::default()
    })
    .unwrap();
    let t = db.load_timeline(&TimelineKey::room("R1")).unwrap().unwrap();
    assert_eq!(
        t.messages.len(),
        1,
        "only the message with an envelope stays"
    );
    let m = &t.messages[0];
    assert_eq!(m.id, "e1");
    assert!(m.msg.is_empty() && m.attachments.is_none() && m.e2e.is_none());
    assert!(m.content.is_some(), "the ciphertext stays");
    let state = db.load_state().unwrap();
    let last = state.rooms[0].last_message.as_ref().unwrap();
    assert!(last.msg.is_empty() && last.content.is_some());
    assert_eq!(state.subscriptions[0].e2e_key, None);
    assert_eq!(state.me.unwrap().e2e, None);
    assert!(db.load_outbox().unwrap().is_empty());
    // Nothing of it in any file (database and WAL).
    db.conn_checkpoint();
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for needle in [secret, "ENCRYPTEDROOMKEY", "PRIVATEKEYMATERIAL"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle} found on disk"
            );
        }
    }
}

#[test]
fn batches_merge_later_wins() {
    let mut a = Batch {
        rooms: vec![("R1".into(), Some(sub("R1", "a", RoomType::Channel)), None)],
        timelines: vec![timeline("R1", messages("R1", 1))],
        ..Default::default()
    };
    a.merge(Batch {
        rooms: vec![("R1".into(), None, None)],
        timelines: vec![timeline("R1", messages("R1", 2))],
        outbox: Some(Vec::new()),
        ..Default::default()
    });
    assert_eq!(a.rooms, vec![("R1".to_owned(), None, None)]);
    assert_eq!(a.timelines.len(), 1);
    assert_eq!(a.timelines[0].messages.len(), 2);
    a.merge(Batch {
        full: Some(Box::default()),
        ..Default::default()
    });
    assert!(a.rooms.is_empty() && a.full.is_some() && a.outbox.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_reads_writes_and_destroys() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("srv");
    let opened = CacheHandle::open(dir.clone(), "u1".into(), None)
        .await
        .unwrap();
    assert!(opened.state.is_empty());
    let h = opened.handle;
    h.write(Batch {
        full: Some(Box::new(fixture_state())),
        timelines: vec![timeline("R1", messages("R1", 5))],
        ..Default::default()
    });
    assert!(h.flush_blocking(std::time::Duration::from_secs(5)));
    assert_eq!(
        h.timeline(TimelineKey::room("R1"))
            .await
            .unwrap()
            .messages
            .len(),
        5
    );
    assert!(h.size().await > 0);
    drop(h);
    // Reopened with the room preloaded.
    let opened = CacheHandle::open(dir.clone(), "u1".into(), Some(TimelineKey::room("R1")))
        .await
        .unwrap();
    assert!(!opened.state.subscriptions.is_empty());
    assert_eq!(opened.preloaded.unwrap().messages.len(), 5);
    opened.handle.clear().await.unwrap();
    opened.handle.destroy().await.unwrap();
    assert!(!dir.exists());
}

// --- workspace -------------------------------------------------------------------

fn cached_workspace() -> Workspace {
    let mut a = sub("A", "a", RoomType::Channel);
    a.updated_at = Some(ts(100));
    a.unread = 3;
    let mut gone = sub("GONE", "gone", RoomType::Channel);
    gone.updated_at = Some(ts(100));
    let mut ws = Workspace::new();
    assert!(ws.apply_cached(CachedState {
        me: Some(me("u1", "alice")),
        subscriptions: vec![a, gone],
        rooms: vec![room("A", "a", RoomType::Channel)],
        ..Default::default()
    }));
    ws
}

#[test]
fn initial_load_replaces_the_cached_workspace() {
    let mut ws = cached_workspace();
    assert!(ws.from_cache());
    assert_eq!(ws.subscription("A").unwrap().unread, 3);
    // Applying again (or over live data) does nothing.
    assert!(!ws.apply_cached(CachedState::default()));
    // The server: same `_updatedAt` (a tie), but read meanwhile; GONE was left.
    let mut a = sub("A", "a", RoomType::Channel);
    a.updated_at = Some(ts(100));
    a.unread = 0;
    ws.apply_initial(crate::sync::InitialData {
        me: me("u1", "alice"),
        subscriptions: vec![a],
        rooms: vec![room("A", "a", RoomType::Channel)],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert!(!ws.from_cache());
    assert_eq!(
        ws.subscription("A").unwrap().unread,
        0,
        "the server wins a tie"
    );
    assert!(ws.room("GONE").is_none(), "tombstones win");
}

#[test]
fn realtime_touch_keeps_the_newer_copy() {
    let mut ws = cached_workspace();
    // A realtime update after the cache was shown.
    let mut a = sub("A", "a", RoomType::Channel);
    a.updated_at = Some(ts(300));
    a.unread = 7;
    ws.apply_subscription(crate::Change::Updated, a);
    // The snapshot started before it (older copy).
    let mut old = sub("A", "a", RoomType::Channel);
    old.updated_at = Some(ts(200));
    ws.apply_initial(crate::sync::InitialData {
        me: me("u1", "alice"),
        subscriptions: vec![old],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    assert_eq!(ws.subscription("A").unwrap().unread, 7);
}

#[test]
fn seeded_messages_give_way_to_the_first_page() {
    let mut ws = cached_workspace();
    ws.open_room("A");
    let cached = vec![
        msg("m1", "A", 1_000, "u2"),
        msg("m2", "A", 2_000, "u2"),
        msg("m3", "A", 3_000, "u2"),
    ];
    assert!(ws.seed_cached_messages("A", None, None, cached));
    let t = ws.timeline("A").unwrap();
    assert_eq!(t.len(), 3);
    assert!(t.has_seeded());
    assert_eq!(
        t.last_updated_at(),
        None,
        "no sync watermark from the cache"
    );
    // A pending message and a realtime message meanwhile.
    let pending = ws.new_local_message("A", "hi", None, false);
    ws.add_pending(pending.clone());
    ws.apply_message(msg("live", "A", 4_000, "u2"));
    // Offline: the first page fails; the cache stays.
    let (c, tk) = ws.begin_load_older_ticket("A").unwrap();
    ws.finish_load_older_ticket("A", tk, c, Err("not connected".into()));
    assert_eq!(ws.timeline("A").unwrap().len(), 5);
    // Online: m2 was deleted and m3 edited while offline.
    let mut m3 = msg("m3", "A", 3_000, "u2");
    m3.msg = "edited".into();
    m3.updated_at = Some(ts(9_000));
    let (c, tk) = ws.begin_load_older_ticket("A").unwrap();
    ws.finish_load_older_ticket("A", tk, c, Ok(vec![msg("m1", "A", 1_000, "u2"), m3]));
    let t = ws.timeline("A").unwrap();
    let ids: Vec<&str> = t.messages().map(|m| m.id.as_str()).collect();
    assert!(!ids.contains(&"m2"), "deleted while offline: {ids:?}");
    assert!(ids.contains(&"live") && ids.contains(&pending.id.as_str()));
    assert_eq!(t.get("m3").unwrap().message.msg, "edited");
    assert!(!t.has_seeded());
    // Seeding a loaded timeline does nothing.
    assert!(!ws.seed_cached_messages("A", None, None, vec![msg("m9", "A", 5, "u2")]));
}

#[test]
fn seeded_thread_gives_way_to_the_first_page() {
    let mut ws = cached_workspace();
    ws.open_thread("A", "P");
    let mut r1 = msg("r1", "A", 1_000, "u2");
    r1.tmid = Some("P".into());
    let mut r2 = msg("r2", "A", 2_000, "u2");
    r2.tmid = Some("P".into());
    let parent = msg("P", "A", 500, "u2");
    assert!(ws.seed_cached_messages("A", Some("P"), Some(parent.clone()), vec![r1.clone(), r2]));
    let t = ws.thread("P").unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t.parent(), Some(&parent));
    let (_, tk) = ws.begin_load_thread_ticket("P").unwrap();
    let page = rocketchat::models::Paginated {
        items: vec![r1],
        count: 1,
        offset: 0,
        total: 1,
        raw_len: 1,
        dropped: 0,
    };
    ws.finish_load_thread_ticket("P", tk, Ok(page));
    let ids: Vec<&str> = ws
        .thread("P")
        .unwrap()
        .messages()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, vec!["r1"]);
}

impl CacheDb {
    /// Moves the WAL into the database file (tests that scan the files).
    fn conn_checkpoint(&self) {
        let _: (i64, i64, i64) = self
            .conn_ref()
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
    }
}
