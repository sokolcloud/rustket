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
    let (mut db, report) = CacheDb::open(&dir, "u1").unwrap();
    assert_eq!(report.moved_aside, Some(dir.join("cache.sqlite.broken")));
    assert!(db.load_state().unwrap().is_empty());
    // "Clear" also deletes the database moved aside.
    db.clear().unwrap();
    assert!(!dir.join("cache.sqlite.broken").exists());
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
            maybe_sent: false,
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
            maybe_sent: false,
            error: None,
        },
        OutboxEntry {
            message: reply,
            queued: false,
            maybe_sent: false,
            error: Some("boom".into()),
        },
        OutboxEntry {
            message: msg("o3", "R1", 9, "u1"),
            queued: true,
            maybe_sent: true,
            error: None,
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
    assert_eq!(db.load_outbox().unwrap().len(), 3);
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
    // A real pending message: plaintext, no `t`, no `content` (it is
    // encrypted at send time).
    let mut plain_pending = msg("o1", "R1", 5, "u1");
    plain_pending.msg = secret.into();
    let mut shaped_pending = msg("o2", "R2", 6, "u1");
    shaped_pending.msg = secret.into();
    shaped_pending.t = Some("e2e".into());
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
        outbox: Some(vec![
            OutboxEntry {
                message: plain_pending,
                queued: true,
                maybe_sent: false,
                error: None,
            },
            OutboxEntry {
                message: shaped_pending,
                queued: true,
                maybe_sent: false,
                error: None,
            },
        ]),
        ..Default::default()
    })
    .unwrap();
    // Nothing of it in any file, the WAL included, before a checkpoint
    // folds it into the database.
    let scan = |stage: &str| {
        for entry in std::fs::read_dir(tmp.path()).unwrap() {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            for needle in [secret, "ENCRYPTEDROOMKEY", "PRIVATEKEYMATERIAL"] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                    "{needle} found in {} ({stage})",
                    path.display()
                );
            }
        }
    };
    assert!(tmp.path().join("cache.sqlite-wal").exists());
    scan("before checkpoint");
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
    db.conn_checkpoint();
    scan("after checkpoint");
}

#[test]
fn coalesced_batches_do_not_bring_back_a_removed_room() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    db.write(Batch {
        full: Some(Box::new(fixture_state())),
        ..Default::default()
    })
    .unwrap();
    let mut a = Batch {
        timelines: vec![timeline("R1", messages("R1", 3))],
        ..Default::default()
    };
    a.merge(Batch {
        rooms: vec![("R1".into(), None, None)],
        ..Default::default()
    });
    assert!(a.timelines.is_empty());
    db.write(a).unwrap();
    assert!(
        db.load_timeline(&TimelineKey::room("R1"))
            .unwrap()
            .is_none()
    );
    // A later snapshot that no longer lists a room drops its timeline too.
    let mut b = Batch {
        timelines: vec![timeline("R9", messages("R9", 2))],
        ..Default::default()
    };
    b.merge(Batch {
        full: Some(Box::new(fixture_state())),
        ..Default::default()
    });
    assert!(b.timelines.is_empty());
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

// --- sidebar thread data ---------------------------------------------------------

fn thread_parent(id: &str, rid: &str, tlm: i64) -> Message {
    let mut m = msg(id, rid, 1000, "u2");
    m.tcount = Some(1);
    m.tlm = Some(ts(tlm));
    m.replies = Some(vec!["u1".into()]);
    m
}

fn threads_fixture() -> CachedThreads {
    CachedThreads {
        followed: vec![
            CachedFollowed {
                rid: "A".into(),
                tmids: vec!["t1".into(), "t2".into()],
                synced_activity: Some(ts(5000)),
                short: Some(crate::thread_index::ShortCrawl { count: 5, kept: 2 }),
            },
            CachedFollowed {
                rid: "B".into(),
                tmids: Vec::new(),
                synced_activity: None,
                short: None,
            },
        ],
        parents: vec![
            thread_parent("t1", "A", 4000),
            thread_parent("t2", "A", 3000),
        ],
        parents_synced: vec![CachedParentsSynced {
            rid: "A".into(),
            synced_activity: Some(ts(5000)),
        }],
        starred: vec![CachedStarred {
            rid: "A".into(),
            tmids: vec!["t1".into()],
            scanned_at: Some(ts(5100)),
            scanned_lm: Some(ts(4900)),
        }],
    }
}

fn sorted(mut t: CachedThreads) -> CachedThreads {
    t.followed.sort_by(|a, b| a.rid.cmp(&b.rid));
    t.parents.sort_by(|a, b| a.id.cmp(&b.id));
    t.parents_synced.sort_by(|a, b| a.rid.cmp(&b.rid));
    t.starred.sort_by(|a, b| a.rid.cmp(&b.rid));
    t
}

#[test]
fn thread_data_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    db.write(Batch {
        threads: Some(threads_fixture()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(sorted(db.load_state().unwrap().threads), threads_fixture());
    drop(db);
    // Survives a restart; a full snapshot keeps it for rooms it still lists.
    let mut db = open(tmp.path(), "u1");
    assert_eq!(sorted(db.load_state().unwrap().threads), threads_fixture());
    let mut with_b = threads_fixture();
    with_b.parents.push(thread_parent("t5", "B", 100));
    with_b.parents_synced.push(CachedParentsSynced {
        rid: "B".into(),
        synced_activity: Some(ts(100)),
    });
    with_b.starred.push(CachedStarred {
        rid: "B".into(),
        tmids: vec!["t5".into()],
        scanned_at: None,
        scanned_lm: None,
    });
    db.write(Batch {
        threads: Some(with_b.clone()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(sorted(db.load_state().unwrap().threads), sorted(with_b));
    db.write(Batch {
        full: Some(Box::new(CachedState {
            subscriptions: vec![sub("A", "a", RoomType::Channel)],
            ..Default::default()
        })),
        ..Default::default()
    })
    .unwrap();
    let t = sorted(db.load_state().unwrap().threads);
    let mut only_a = threads_fixture();
    only_a.retain_rooms(|rid| rid == "A");
    assert_eq!(only_a.followed.len(), 1);
    assert_eq!(t, only_a, "B is no longer listed");
    // A room that is gone takes its thread data along.
    db.write(Batch {
        rooms: vec![("A".into(), None, None)],
        ..Default::default()
    })
    .unwrap();
    assert!(db.load_state().unwrap().threads.is_empty());
    // A new set replaces the old one, writing only what changed; "clear"
    // drops it.
    db.write(Batch {
        threads: Some(threads_fixture()),
        ..Default::default()
    })
    .unwrap();
    let before = db.conn_ref().total_changes();
    db.write(Batch {
        threads: Some(threads_fixture()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(db.conn_ref().total_changes(), before, "nothing changed");
    let mut next = threads_fixture();
    next.parents[0].tcount = Some(7);
    db.write(Batch {
        threads: Some(next),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(db.conn_ref().total_changes(), before + 1, "one parent");
    db.write(Batch {
        threads: Some(CachedThreads {
            parents: vec![thread_parent("t9", "A", 1)],
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let t = db.load_state().unwrap().threads;
    assert!(t.followed.is_empty() && t.parents_synced.is_empty());
    assert_eq!(t.parents.len(), 1);
    db.clear().unwrap();
    assert!(db.load_state().unwrap().threads.is_empty());
}

#[test]
fn thread_parents_keep_no_e2e_plaintext() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = open(tmp.path(), "u1");
    let mut p = decrypted("t1", "A", "top secret thread");
    p.tcount = Some(2);
    db.write(Batch {
        threads: Some(CachedThreads {
            parents: vec![p],
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    let raw: String = db
        .conn_ref()
        .query_row(
            "SELECT json FROM docs WHERE kind = 'thread_parent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("top secret"));
}

#[test]
fn batches_merge_thread_data_and_full_snapshots() {
    let mut a = Batch {
        threads: Some(threads_fixture()),
        ..Default::default()
    };
    a.merge(Batch {
        full: Some(Box::new(CachedState {
            subscriptions: vec![sub("B", "b", RoomType::Channel)],
            ..Default::default()
        })),
        ..Default::default()
    });
    let t = a.threads.as_ref().unwrap();
    assert_eq!(t.followed.len(), 1);
    assert_eq!(t.followed[0].rid, "B");
    assert!(t.parents.is_empty() && t.parents_synced.is_empty());
    // A room that is gone in a later batch: its thread data of the
    // earlier batch must not bring it back.
    let mut a = Batch {
        threads: Some(threads_fixture()),
        ..Default::default()
    };
    a.merge(Batch {
        rooms: vec![("A".into(), None, None)],
        ..Default::default()
    });
    let t = a.threads.as_ref().unwrap();
    assert_eq!(t.followed.len(), 1);
    assert_eq!(t.followed[0].rid, "B");
    assert!(t.parents.is_empty() && t.parents_synced.is_empty());
    a.merge(Batch {
        threads: Some(CachedThreads::default()),
        ..Default::default()
    });
    assert_eq!(a.threads, Some(CachedThreads::default()));
}

/// A cached workspace in Following mode with thread data: A had a reply
/// after its crawl (at 6000), B did not.
fn cached_thread_workspace(mode: crate::sidebar_layout::ThreadsMode) -> Workspace {
    let mut a = sub("A", "a", RoomType::Channel);
    a.lr = Some(ts(5000));
    a.tunread = Some(vec!["t2".into()]);
    let mut b = sub("B", "b", RoomType::Channel);
    b.lr = Some(ts(2000));
    let mut ws = Workspace::new();
    ws.set_thread_config(mode, []);
    let mut threads = threads_fixture();
    threads.followed[1].synced_activity = Some(ts(2000));
    threads.followed.push(CachedFollowed {
        rid: "LEFT".into(),
        tmids: vec!["t3".into()],
        synced_activity: None,
        short: None,
    });
    threads.parents.push(thread_parent("t3", "LEFT", 1));
    assert!(ws.apply_cached(CachedState {
        me: Some(me("u1", "alice")),
        subscriptions: vec![a, b],
        threads,
        ..Default::default()
    }));
    ws
}

#[test]
fn restored_thread_data_is_confirmed_by_the_initial_load() {
    use crate::sidebar_layout::ThreadsMode;
    let mut ws = cached_thread_workspace(ThreadsMode::Following);
    let idx = ws.thread_index();
    assert!(idx.parent("t1").is_some());
    assert!(idx.parent("t3").is_none(), "rooms the account has not");
    assert!(idx.followed("LEFT").is_none());
    let a = idx.followed("A").unwrap();
    assert!(a.stale, "not current until the initial load");
    assert_eq!(a.tmids.len(), 2);
    assert!(!idx.is_stale("t2"));

    // The server: A had a reply at 6000 (in t2), B had none.
    let mut a = sub("A", "a", RoomType::Channel);
    a.lr = Some(ts(6000));
    a.tunread = Some(vec!["t2".into()]);
    let mut b = sub("B", "b", RoomType::Channel);
    b.lr = Some(ts(2000));
    ws.take_changes();
    ws.apply_initial(crate::sync::InitialData {
        me: me("u1", "alice"),
        subscriptions: vec![a, b],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    let idx = ws.thread_index();
    assert!(idx.followed("A").unwrap().stale, "replies since its crawl");
    assert!(!idx.followed("B").unwrap().stale);
    assert!(idx.is_stale("t2") && idx.is_stale("t1"));
    // A newer copy of a stale parent makes it current.
    ws.apply_thread_parents(vec![thread_parent("t2", "A", 6000)]);
    assert!(!ws.thread_index().is_stale("t2"));
    // An identical copy too.
    let t1 = ws.thread_index().parent("t1").unwrap().clone();
    ws.apply_thread_parents(vec![t1]);
    assert!(!ws.thread_index().is_stale("t1"));
}

#[test]
fn restored_followed_sets_need_following_mode() {
    use crate::sidebar_layout::ThreadsMode;
    let ws = cached_thread_workspace(ThreadsMode::UnreadKept);
    assert!(ws.thread_index().followed("A").is_none());
    assert!(ws.thread_index().parent("t2").is_some());
    let ws = cached_thread_workspace(ThreadsMode::Off);
    assert!(ws.thread_index().is_empty());
}

#[test]
fn restored_starred_threads_with_threads_off() {
    use crate::sidebar_layout::ThreadsMode;
    let cached = |pins: bool| {
        let mut ws = Workspace::new();
        ws.set_thread_config(ThreadsMode::Off, []);
        ws.set_starred_threads(pins);
        let mut threads = threads_fixture();
        threads.starred.push(CachedStarred {
            rid: "LEFT".into(),
            tmids: vec!["t3".into()],
            scanned_at: None,
            scanned_lm: None,
        });
        assert!(ws.apply_cached(CachedState {
            me: Some(me("u1", "alice")),
            subscriptions: vec![sub("A", "a", RoomType::Channel)],
            threads,
            ..Default::default()
        }));
        ws
    };
    let ws = cached(true);
    let idx = ws.thread_index();
    assert_eq!(idx.starred().collect::<Vec<_>>(), [("A", "t1")]);
    assert!(idx.parent("t1").is_some() && idx.parent("t2").is_none());
    assert_eq!(idx.starred_scan("A").unwrap().lm, Some(ts(4900)));
    assert!(idx.starred_scan("LEFT").is_none());
    let ws = cached(false);
    assert!(ws.thread_index().is_empty());
    assert_eq!(ws.thread_index().starred().count(), 0);
    // The cached settings disallow starring: nothing is restored.
    let mut ws = Workspace::new();
    ws.set_thread_config(ThreadsMode::Off, []);
    ws.set_starred_threads(true);
    assert!(ws.thread_index().pins_enabled());
    assert!(ws.apply_cached(CachedState {
        me: Some(me("u1", "alice")),
        settings: vec![Setting {
            id: "Message_AllowStarring".into(),
            value: json!(false),
            ..Default::default()
        }],
        subscriptions: vec![sub("A", "a", RoomType::Channel)],
        threads: threads_fixture(),
        ..Default::default()
    }));
    assert!(!ws.thread_index().pins_enabled());
    assert_eq!(ws.thread_index().starred().count(), 0);
}

#[test]
fn restored_flags_pin_their_threads() {
    use crate::sidebar_layout::ThreadsMode;
    let mut svc = sub("SVC", "rustket-sync-u1", RoomType::Private);
    svc.u.id = "u1".into();
    svc.open = false;
    svc.roles = Some(vec!["owner".into()]);
    let mut fields = rocketchat::models::ServiceFields::new("u1");
    fields.set(
        rocketchat::models::FLAGS_KEY,
        json!({ "threads": { "t2": { "c": "red", "rid": "A", "t": 1 } } }),
    );
    svc.custom_fields = Some(fields.to_custom_fields());
    let mut ws = Workspace::new();
    ws.set_thread_config(ThreadsMode::Off, []);
    assert!(ws.apply_cached(CachedState {
        me: Some(me("u1", "alice")),
        rooms: vec![crate::test_util::service_room(&svc, "u1", 1)],
        subscriptions: vec![sub("A", "a", RoomType::Channel), svc],
        threads: threads_fixture(),
        ..Default::default()
    }));
    assert_eq!(ws.service_rooms(), ["SVC"]);
    assert_eq!(ws.thread_flag("t2"), Some(crate::flags::FlagColor::Red));
    let idx = ws.thread_index();
    assert!(idx.is_flagged("t2") && !idx.pins_enabled());
    assert!(idx.parent("t2").is_some() && idx.parent("t1").is_none());
}

#[test]
fn restored_parents_are_current_up_to_their_mark() {
    use crate::sidebar_layout::ThreadsMode;
    // A's parents were written before a reply the cached subscription has
    // (lr 5000): stale although the server's lr is the cached one.
    let mut a = sub("A", "a", RoomType::Channel);
    a.lr = Some(ts(5000));
    let mut b = sub("B", "b", RoomType::Channel);
    b.lr = Some(ts(2000));
    let mut threads = threads_fixture();
    threads.parents_synced[0].synced_activity = Some(ts(4500));
    // B's parent has no mark: not known to be current.
    threads.parents.push(thread_parent("t4", "B", 1500));
    let mut ws = Workspace::new();
    ws.set_thread_config(ThreadsMode::Unread, []);
    assert!(ws.apply_cached(CachedState {
        me: Some(me("u1", "alice")),
        subscriptions: vec![a.clone(), b.clone()],
        threads,
        ..Default::default()
    }));
    ws.apply_initial(crate::sync::InitialData {
        me: me("u1", "alice"),
        subscriptions: vec![a, b],
        next_since: Some(ts(1)),
        ..Default::default()
    });
    let idx = ws.thread_index();
    assert!(idx.is_stale("t1") && idx.is_stale("t2") && idx.is_stale("t4"));
}
