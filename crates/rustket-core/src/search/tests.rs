#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{FixedOffset, NaiveDate};
use rocketchat::models::{IdRef, Mention, MessageUrl, Room, RoomType, Subscription};

use super::*;
use crate::inbox::load::LoadFailure;
use crate::test_util::{me, msg, room, sub, ts};
use crate::workspace::{Change, Workspace};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[test]
fn parses_every_operator() {
    let q = SearchQuery::parse(
        "deploy FROM:bob from:me mention:carol has:url is:pinned has:star has:map \
         after:1/2/2026 before:15.03.2026 in:#dev-public in:general label:\"two words\" \
         order:asc  release",
    );
    assert_eq!(q.text, "deploy release");
    assert_eq!(q.from, ["bob", "me"]);
    assert_eq!(q.mentions, ["carol"]);
    assert_eq!(q.rooms, ["dev-public", "general"]);
    assert_eq!(q.has, [Has::Link, Has::Pin, Has::Star, Has::Location]);
    assert_eq!(q.after, Some(d(2026, 2, 1)));
    assert_eq!(q.before, Some(d(2026, 3, 15)));
    assert_eq!(q.other, ["label:\"two words\"", "order:asc"]);
    assert_eq!(
        q.server_query(),
        "from:bob from:me mention:carol has:link is:pinned has:star has:location \
         after:01/02/2026 before:15/03/2026 label:\"two words\" order:asc deploy release"
    );
    assert!(
        q.to_query()
            .contains("label:\"two words\" order:asc in:#dev-public in:#general deploy")
    );
}

#[test]
fn bad_operators_stay_text_and_roundtrip() {
    let q = SearchQuery::parse("from: after:31/02/2026 in: has:file hello");
    assert!(q.from.is_empty() && q.after.is_none() && q.rooms.is_empty());
    assert_eq!(q.text, "from: after:31/02/2026 in: has:file hello");
    let q = SearchQuery::parse("in:\"my room\" from:alice x");
    assert_eq!(q.rooms, ["my room"]);
    assert_eq!(SearchQuery::parse(&q.to_query()), q);
    assert_eq!(q.server_query(), "from:alice x");
}

#[test]
fn builder_edits_and_emptiness() {
    let mut q = SearchQuery::parse("");
    assert!(q.is_empty());
    q.set_has(Has::Star, true);
    q.set_has(Has::Link, true);
    q.set_has(Has::Star, true);
    assert_eq!(q.to_query(), "has:link has:star");
    assert!(!q.is_empty());
    q.set_has(Has::Link, false);
    assert_eq!(q.to_query(), "has:star");
    // A room filter alone sends nothing to the server.
    let q = SearchQuery::parse("in:general");
    assert!(q.is_empty());
}

#[test]
fn regex_text() {
    let q = SearchQuery::parse("/dep(loy|th)/i");
    assert_eq!(q.regex(), Some(("dep(loy|th)", "i")));
    assert!(q.words().is_empty());
    let q = SearchQuery::parse("from:bob /a b/");
    assert_eq!(q.from, ["bob"]);
    assert_eq!(q.regex(), Some(("a b", "")));
    assert_eq!(parse_date("1-2-2026"), Some(d(2026, 2, 1)));
    assert_eq!(parse_date("2026-02-01"), None);
}

fn ctx() -> MatchCtx {
    MatchCtx {
        user_id: "u-me".into(),
        username: "me".into(),
        offset: FixedOffset::east_opt(0).unwrap(),
    }
}

fn day_ms(y: i32, m: u32, day: u32, h: u32) -> i64 {
    d(y, m, day)
        .and_hms_opt(h, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

#[test]
fn local_matcher_follows_the_server() {
    let mut m = msg("m1", "r1", day_ms(2026, 3, 10, 12), "bob");
    m.msg = "Deploy of the Release went fine".into();
    let t = |s: &str| LocalMatcher::new(&SearchQuery::parse(s), &ctx()).unwrap();
    assert!(t("deploy release").matches(&m));
    assert!(!t("deploy rollback").matches(&m));
    assert!(t("from:bo").matches(&m), "from: is a regex on the username");
    assert!(!t("from:me deploy").matches(&m));
    assert!(t("/rel.ase/i").matches(&m));
    assert!(!t("/rel.ase/").matches(&m), "case-sensitive without i");
    // after: starts the next day, before: ends at the day's start.
    assert!(t("after:9/3/2026").matches(&m));
    assert!(!t("after:10/3/2026").matches(&m));
    assert!(t("before:11/3/2026").matches(&m));
    assert!(!t("before:10/3/2026").matches(&m));
    assert!(t("on:10/3/2026").matches(&m));
    assert!(!t("on:11/3/2026").matches(&m));
    assert!(!t("has:link").matches(&m));
    m.urls = Some(vec![MessageUrl::default()]);
    m.pinned = Some(true);
    m.starred = Some(vec![IdRef { id: "u-me".into() }]);
    m.mentions = Some(vec![Mention {
        id: "u-c".into(),
        username: Some("carol".into()),
        ..Default::default()
    }]);
    assert!(t("has:link is:pinned has:star mention:carol deploy").matches(&m));
    let mut sys = m.clone();
    sys.t = Some("au".into());
    assert!(!t("deploy").matches(&sys));
    // What cannot be evaluated locally has no matcher.
    assert!(LocalMatcher::new(&SearchQuery::parse("has:location x"), &ctx()).is_none());
    assert!(LocalMatcher::new(&SearchQuery::parse("file-title:a"), &ctx()).is_none());
    assert!(LocalMatcher::new(&SearchQuery::parse("/(/"), &ctx()).is_none());
    assert!(LocalMatcher::new(&SearchQuery::parse("in:general"), &ctx()).is_none());
}

#[test]
fn highlight_and_snippet() {
    let words = vec!["приве".to_owned(), "ab".to_owned()];
    let text = "ПРИВЕТ abab xab";
    let r = highlight(text, &words);
    let parts: Vec<&str> = r.iter().map(|r| &text[r.clone()]).collect();
    assert_eq!(parts, ["ПРИВЕ", "abab", "ab"]);
    let long = format!("{} needle {}", "a ".repeat(200), "b ".repeat(200));
    let (s, r) = snippet(&long, &["needle".into()], 60);
    assert!(s.starts_with('…') && s.ends_with('…'));
    assert_eq!(r.len(), 1);
    assert_eq!(&s[r[0].clone()], "needle");
    let (s, r) = snippet("short\nline  needle", &["needle".into()], 60);
    assert_eq!(s, "short line needle");
    assert_eq!(&s[r[0].clone()], "needle");
}

fn ws_with(entries: Vec<(Subscription, Option<i64>, bool)>) -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    for (s, lm, enc) in entries {
        let mut r: Room = room(&s.rid, &s.name, s.t);
        r.lm = lm.map(ts);
        r.encrypted = Some(enc);
        ws.apply_subscription(Change::Inserted, s);
        ws.apply_room(Change::Inserted, r);
    }
    ws
}

#[test]
fn rooms_newest_first_encrypted_apart() {
    let ws = ws_with(vec![
        (sub("a", "alpha", RoomType::Channel), Some(10), false),
        (sub("b", "beta", RoomType::Private), Some(30), false),
        (sub("c", "secret", RoomType::Private), Some(20), true),
        (sub("d", "bob", RoomType::Direct), None, false),
    ]);
    let (plain, enc) = rooms_to_search(&ws, None);
    assert_eq!(plain, ["b", "a", "d"]);
    assert_eq!(enc, ["c"]);
    let only: HashSet<String> = resolve_rooms(&ws, &["#Alpha".into(), "@bob".into()]);
    assert_eq!(only, HashSet::from(["a".to_owned(), "d".to_owned()]));
    let (plain, _) = rooms_to_search(&ws, Some(&only));
    assert_eq!(plain, ["a", "d"]);
}

fn hit_msg(id: &str, rid: &str, ms: i64) -> rocketchat::models::Message {
    msg(id, rid, ms, "bob")
}

#[test]
fn run_paces_and_merges() {
    let t0 = Instant::now();
    let rooms = vec!["a".to_owned(), "b".into(), "c".into()];
    let mut run = SearchRun::new(1, "x".into(), Backend::Provider, rooms, 1, false);
    let mut p = Pacing::default();
    assert_eq!(run.progress(), (0, 3));
    assert_eq!(run.next(t0, &mut p), Next::Fetch(Target::Room("a".into())));
    // Paced: the second waits for the interval.
    let gap = Backend::Provider.interval();
    assert_eq!(run.next(t0, &mut p), Next::Wait(t0 + gap));
    let t1 = t0 + gap;
    assert_eq!(run.next(t1, &mut p), Next::Fetch(Target::Room("b".into())));
    // Two in flight at most.
    assert_eq!(run.next(t1 + gap, &mut p), Next::Idle);
    // Local hits show first; the server's answer for a room replaces them.
    run.merge_local([hit_msg("l1", "a", 5), hit_msg("s1", "a", 9)]);
    assert_eq!(run.len(), 2);
    run.finish(
        &Target::Room("a".into()),
        Ok(vec![hit_msg("s1", "a", 9)]),
        t1,
        &mut p,
    );
    assert_eq!(
        run.len(),
        1,
        "unconfirmed local hit of a searched room dropped"
    );
    assert_eq!(run.hit("s1").unwrap().origin, Origin::Server);
    // A rate limit pauses and requeues.
    run.finish(
        &Target::Room("b".into()),
        Err(LoadFailure::RateLimited(Duration::from_secs(5))),
        t1,
        &mut p,
    );
    assert_eq!(
        run.next(t1 + gap, &mut p),
        Next::Wait(t1 + Duration::from_secs(5))
    );
    let t2 = t1 + Duration::from_secs(5);
    assert_eq!(run.next(t2, &mut p), Next::Fetch(Target::Room("b".into())));
    run.finish(
        &Target::Room("b".into()),
        Err(LoadFailure::NotAllowed),
        t2,
        &mut p,
    );
    assert!(run.failed.contains("b"));
    let t3 = t2 + gap;
    assert_eq!(run.next(t3, &mut p), Next::Fetch(Target::Room("c".into())));
    let mut e2e = hit_msg("enc", "c", 1);
    e2e.t = Some("e2e".into());
    run.finish(
        &Target::Room("c".into()),
        Ok(vec![hit_msg("s2", "c", 20), e2e]),
        t3,
        &mut p,
    );
    assert!(run.is_finished());
    assert_eq!(run.progress(), (3, 3));
    let groups = run.groups();
    let order: Vec<&str> = groups.iter().map(|g| g.0.as_str()).collect();
    assert_eq!(order, ["c", "a"], "rooms by newest hit, ciphertext dropped");
    // Answers of requests not in flight are ignored.
    run.finish(
        &Target::Room("a".into()),
        Ok(vec![hit_msg("zz", "a", 1)]),
        t3,
        &mut p,
    );
    assert!(run.hit("zz").is_none());
}

#[test]
fn run_global_and_backoff() {
    let t0 = Instant::now();
    let mut run = SearchRun::new(2, "x".into(), Backend::Global, vec!["a".into()], 0, false);
    let mut p = Pacing::default();
    assert_eq!(run.progress(), (0, 1));
    assert_eq!(run.next(t0, &mut p), Next::Fetch(Target::All));
    run.finish(&Target::All, Err(LoadFailure::Transient), t0, &mut p);
    match run.next(t0 + Duration::from_secs(2), &mut p) {
        Next::Wait(at) => assert!(at > t0 + Duration::from_secs(2)),
        n => panic!("{n:?}"),
    }
    // An `in:` filter searches its rooms one by one, even with global search.
    let run = SearchRun::new(
        3,
        "x".into(),
        Backend::Global,
        vec!["a".into(), "b".into()],
        0,
        true,
    );
    assert_eq!(run.backend, Backend::Provider);
    assert_eq!(run.progress(), (0, 2));
}

#[test]
fn pacing_and_pauses_outlive_a_run() {
    let t0 = Instant::now();
    let mut p = Pacing::default();
    let mut first = SearchRun::new(1, "x".into(), Backend::Provider, vec!["a".into()], 0, false);
    assert_eq!(
        first.next(t0, &mut p),
        Next::Fetch(Target::Room("a".into()))
    );
    first.finish(
        &Target::Room("a".into()),
        Err(LoadFailure::RateLimited(Duration::from_secs(40))),
        t0,
        &mut p,
    );
    // The next edit's run waits out the same pause instead of asking now.
    let mut second = SearchRun::new(2, "y".into(), Backend::Provider, vec!["a".into()], 0, false);
    let t1 = t0 + Duration::from_millis(10);
    assert_eq!(
        second.next(t1, &mut p),
        Next::Wait(t0 + Duration::from_secs(40))
    );
    assert_eq!(p.paused_until(t1), Some(t0 + Duration::from_secs(40)));
    // And the interval since the last request of any run.
    let mut p = Pacing::default();
    let mut a = SearchRun::new(3, "x".into(), Backend::Provider, vec!["a".into()], 0, false);
    assert!(matches!(a.next(t0, &mut p), Next::Fetch(_)));
    let mut b = SearchRun::new(4, "y".into(), Backend::Provider, vec!["a".into()], 0, false);
    assert_eq!(
        b.next(t0, &mut p),
        Next::Wait(t0 + Backend::Provider.interval())
    );
}

#[test]
fn rejected_query_stops_the_server_part() {
    let t0 = Instant::now();
    let mut p = Pacing::default();
    let rooms: Vec<String> = (0..10).map(|i| format!("r{i}")).collect();
    let mut run = SearchRun::new(1, "/[\\d-z]/".into(), Backend::Provider, rooms, 0, false);
    let mut t = t0;
    for i in 0..3 {
        let Next::Fetch(target) = run.next(t, &mut p) else {
            panic!("fetch {i}");
        };
        // Not retried: the same query gets the same error.
        run.finish(&target, Err(LoadFailure::Other), t, &mut p);
        t += Backend::Provider.interval();
    }
    assert!(run.stopped);
    assert!(run.is_finished());
    assert_eq!(run.progress(), (10, 10));
    assert_eq!(run.failed.len(), 10);
    assert_eq!(run.next(t, &mut p), Next::Idle);
    // A success in between resets the count.
    let rooms: Vec<String> = (0..4).map(|i| format!("r{i}")).collect();
    let mut run = SearchRun::new(2, "x".into(), Backend::Provider, rooms, 0, false);
    let mut p = Pacing::default();
    let mut t = t0;
    for r in [
        Err(LoadFailure::Other),
        Err(LoadFailure::Other),
        Ok(Vec::new()),
        Err(LoadFailure::Other),
    ] {
        let Next::Fetch(target) = run.next(t, &mut p) else {
            panic!("fetch");
        };
        run.finish(&target, r, t, &mut p);
        t += Backend::Provider.interval();
    }
    assert!(!run.stopped);
    assert_eq!(run.failed.len(), 3);
}

#[test]
fn failed_server_wide_search_is_not_an_empty_answer() {
    let t0 = Instant::now();
    let mut p = Pacing::default();
    let mut run = SearchRun::new(1, "x".into(), Backend::Global, vec!["a".into()], 0, false);
    assert_eq!(run.next(t0, &mut p), Next::Fetch(Target::All));
    run.finish(&Target::All, Err(LoadFailure::NotAllowed), t0, &mut p);
    assert!(run.is_finished());
    assert!(run.stopped, "the view shows an error, not 'no results'");
}

#[test]
fn late_local_hits_of_answered_rooms_are_dropped() {
    let t0 = Instant::now();
    let mut p = Pacing::default();
    let mut run = SearchRun::new(
        1,
        "x".into(),
        Backend::Provider,
        vec!["a".into(), "b".into()],
        0,
        false,
    );
    assert!(matches!(run.next(t0, &mut p), Next::Fetch(_)));
    run.finish(
        &Target::Room("a".into()),
        Ok(vec![hit_msg("s1", "a", 9)]),
        t0,
        &mut p,
    );
    // The offline cache answers after the server did for room "a".
    run.merge_local([hit_msg("stale", "a", 5), hit_msg("l-b", "b", 5)]);
    assert!(run.hit("stale").is_none(), "room a was answered completely");
    assert!(run.hit("l-b").is_some(), "room b not answered yet");
    // A full room answer keeps unconfirmed local hits (older ones missing).
    let mut run = SearchRun::new(2, "x".into(), Backend::Provider, vec!["a".into()], 0, false);
    let mut p = Pacing::default();
    assert!(matches!(run.next(t0, &mut p), Next::Fetch(_)));
    let full: Vec<_> = (0..PER_ROOM_LIMIT)
        .map(|i| hit_msg(&format!("s{i}"), "a", 100 + i64::from(i)))
        .collect();
    run.finish(&Target::Room("a".into()), Ok(full), t0, &mut p);
    run.merge_local([hit_msg("old", "a", 1)]);
    assert!(run.hit("old").is_some());
    // Server-wide: a complete answer covers every searched room, not the
    // encrypted ones (left to the local search).
    let mut run = SearchRun::new(3, "x".into(), Backend::Global, vec!["a".into()], 1, false);
    let mut p = Pacing::default();
    run.merge_local([hit_msg("early", "a", 1), hit_msg("enc", "secret", 1)]);
    assert_eq!(run.next(t0, &mut p), Next::Fetch(Target::All));
    run.finish(&Target::All, Ok(vec![hit_msg("s1", "a", 9)]), t0, &mut p);
    assert!(run.hit("early").is_none());
    assert!(run.hit("enc").is_some());
    run.merge_local([hit_msg("late", "a", 2)]);
    assert!(run.hit("late").is_none());
}

#[test]
fn deferred_run_takes_local_hits_before_the_backend_is_known() {
    let t0 = Instant::now();
    let mut p = Pacing::default();
    let mut run = SearchRun::deferred(1, "x".into(), vec!["a".into()], 0, false);
    assert!(run.awaiting_backend());
    assert!(!run.is_finished());
    assert_eq!(run.next(t0, &mut p), Next::Idle);
    run.merge_local([hit_msg("l1", "a", 1)]);
    assert_eq!(run.len(), 1);
    run.start_server(Backend::Global);
    assert!(!run.awaiting_backend());
    assert_eq!(run.backend, Backend::Global);
    assert_eq!(run.next(t0, &mut p), Next::Fetch(Target::All));
}

#[test]
fn regex_with_a_slash_is_not_run() {
    // The server splits on every '/': `/usr/bin/` would be `RegExp('usr', 'bin')`.
    for q in ["/usr/bin/", "/https?:\\/\\/example/"] {
        let q = SearchQuery::parse(q);
        assert!(q.regex_unsupported(), "{q:?}");
        assert_eq!(q.regex(), None);
        assert!(LocalMatcher::new(&q, &ctx()).is_none());
    }
    let q = SearchQuery::parse("/usr\\x2Fbin/");
    assert!(!q.regex_unsupported());
    let mut m = msg("m1", "r1", 1, "bob");
    m.msg = "see /usr/bin/env".into();
    assert!(LocalMatcher::new(&q, &ctx()).unwrap().matches(&m));
}

#[test]
fn me_is_sent_as_the_username() {
    let q = SearchQuery::parse("mention:me FROM:Me from:bob x");
    assert_eq!(
        q.server_query_as(Some("alice")),
        "from:alice from:bob mention:alice x"
    );
    assert_eq!(q.server_query(), "from:Me from:bob mention:me x");
    // The builder keeps what was typed.
    assert_eq!(q.to_query(), "from:Me from:bob mention:me x");
}

#[test]
fn day_bounds_follow_the_server() {
    // RC on a UTC server: `after:16/11/2026` starts at UTC midnight of the
    // 17th plus the user's utcOffset (3 h), likewise `before:` and `on:`.
    let ctx = MatchCtx {
        offset: FixedOffset::east_opt(3 * 3600).unwrap(),
        ..ctx()
    };
    let t = |s: &str| LocalMatcher::new(&SearchQuery::parse(s), &ctx).unwrap();
    let at = |h: u32| {
        let mut m = msg("m1", "r1", day_ms(2026, 11, 17, h), "bob");
        m.msg = "x".into();
        m
    };
    assert!(!t("after:16/11/2026 x").matches(&at(2)));
    assert!(t("after:16/11/2026 x").matches(&at(3)));
    assert!(t("before:17/11/2026 x").matches(&at(3)));
    assert!(!t("before:17/11/2026 x").matches(&at(4)));
    assert!(t("on:17/11/2026 x").matches(&at(3)));
    assert!(!t("on:17/11/2026 x").matches(&at(2)));
    assert_eq!(
        t("on:17/11/2026 x").ts_bounds(),
        (
            Some(day_ms(2026, 11, 17, 3)),
            Some(day_ms(2026, 11, 18, 3) - 1)
        )
    );
}
