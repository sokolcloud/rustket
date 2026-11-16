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
    assert_eq!(run.progress(), (0, 3));
    assert_eq!(run.next(t0), Next::Fetch(Target::Room("a".into())));
    // Paced: the second waits for the interval.
    let gap = Backend::Provider.interval();
    assert_eq!(run.next(t0), Next::Wait(t0 + gap));
    let t1 = t0 + gap;
    assert_eq!(run.next(t1), Next::Fetch(Target::Room("b".into())));
    // Two in flight at most.
    assert_eq!(run.next(t1 + gap), Next::Idle);
    // Local hits show first; the server's answer for a room replaces them.
    run.merge_local([hit_msg("l1", "a", 5), hit_msg("s1", "a", 9)]);
    assert_eq!(run.len(), 2);
    run.finish(
        &Target::Room("a".into()),
        Ok(vec![hit_msg("s1", "a", 9)]),
        t1,
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
    );
    assert_eq!(run.next(t1 + gap), Next::Wait(t1 + Duration::from_secs(5)));
    let t2 = t1 + Duration::from_secs(5);
    assert_eq!(run.next(t2), Next::Fetch(Target::Room("b".into())));
    run.finish(&Target::Room("b".into()), Err(LoadFailure::NotAllowed), t2);
    assert!(run.failed.contains("b"));
    let t3 = t2 + gap;
    assert_eq!(run.next(t3), Next::Fetch(Target::Room("c".into())));
    let mut e2e = hit_msg("enc", "c", 1);
    e2e.t = Some("e2e".into());
    run.finish(
        &Target::Room("c".into()),
        Ok(vec![hit_msg("s2", "c", 20), e2e]),
        t3,
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
    );
    assert!(run.hit("zz").is_none());
}

#[test]
fn run_global_and_backoff() {
    let t0 = Instant::now();
    let mut run = SearchRun::new(2, "x".into(), Backend::Global, vec!["a".into()], 0, false);
    assert_eq!(run.progress(), (0, 1));
    assert_eq!(run.next(t0), Next::Fetch(Target::All));
    run.finish(&Target::All, Err(LoadFailure::Transient), t0);
    match run.next(t0 + Duration::from_secs(2)) {
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
