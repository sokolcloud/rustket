#![allow(clippy::unwrap_used)]

use rocketchat::models::{Room, RoomType, Subscription, User};
use rustket_core::Workspace;
use rustket_core::search::Has;
use rustket_core::workspace::Change;

use super::*;

fn ws() -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(User {
        id: "u1".into(),
        username: Some("alice".into()),
        ..Default::default()
    });
    for (rid, name, t) in [
        ("r1", "general", RoomType::Channel),
        ("r2", "dev-team", RoomType::Private),
        ("d1", "bob", RoomType::Direct),
    ] {
        ws.apply_subscription(
            Change::Inserted,
            Subscription {
                id: format!("s-{rid}"),
                rid: rid.into(),
                name: name.into(),
                t,
                open: true,
                ..Default::default()
            },
        );
        ws.apply_room(
            Change::Inserted,
            Room {
                id: rid.into(),
                t,
                name: Some(name.into()),
                ..Default::default()
            },
        );
    }
    ws
}

fn starts(outs: &[Out]) -> bool {
    outs.iter().any(|o| matches!(o, Out::Start))
}

#[test]
fn typing_debounces_and_stale_debounces_are_ignored() {
    let ws = ws();
    let mut v = SearchView::new(String::new());
    let outs = v.update(Msg::Query("dep".into()), &ws);
    let Some(Out::Debounce(first)) = outs.first().cloned() else {
        panic!("{outs:?}")
    };
    let outs = v.update(Msg::Query("deploy".into()), &ws);
    let Some(Out::Debounce(second)) = outs.first().cloned() else {
        panic!("{outs:?}")
    };
    assert!(v.update(Msg::Debounced(first), &ws).is_empty());
    assert!(starts(&v.update(Msg::Debounced(second), &ws)));
    assert!(starts(&v.update(Msg::Submit, &ws)));
}

#[test]
fn builder_rewrites_the_query() {
    let ws = ws();
    let mut v = SearchView::new("deploy".into());
    assert!(starts(&v.update(Msg::ToggleHas(Has::Link), &ws)));
    assert_eq!(v.query, "has:link deploy");
    v.update(Msg::ToggleMentionsMe, &ws);
    assert_eq!(v.query, "mention:alice has:link deploy");
    v.update(Msg::ToggleMentionsMe, &ws);
    v.update(Msg::AddFrom("bob".into()), &ws);
    v.update(Msg::AddRoom("general".into()), &ws);
    assert_eq!(v.query, "from:bob has:link in:#general deploy");
    v.update(Msg::RemoveFrom("bob".into()), &ws);
    v.update(Msg::RemoveRoom("general".into()), &ws);
    v.update(Msg::ToggleHas(Has::Link), &ws);
    assert_eq!(v.query, "deploy");
    // Pickers: Enter takes the first suggestion, or a typed date.
    v.update(Msg::Pick(Some(Picker::In)), &ws);
    v.update(Msg::PickerInput("dev".into()), &ws);
    assert!(starts(&v.update(Msg::PickerSubmit, &ws)));
    assert!(v.picker.is_none());
    assert_eq!(v.parsed().rooms, ["dev-team"]);
    v.update(Msg::Pick(Some(Picker::From)), &ws);
    v.update(Msg::PickerInput("@carol".into()), &ws);
    v.update(Msg::PickerSubmit, &ws);
    assert_eq!(v.parsed().from, ["carol"], "an unknown username as typed");
    v.update(Msg::Pick(Some(Picker::Before)), &ws);
    v.update(Msg::PickerInput("bad".into()), &ws);
    assert!(v.update(Msg::PickerSubmit, &ws).is_empty());
    v.update(Msg::PickerInput("1/2/2026".into()), &ws);
    v.update(Msg::PickerSubmit, &ws);
    assert!(v.query.contains("before:01/02/2026"));
    v.update(Msg::ClearDate(Picker::Before), &ws);
    assert!(!v.query.contains("before:"));
    // Clicking the open picker's chip closes it.
    v.update(Msg::Pick(Some(Picker::After)), &ws);
    v.update(Msg::Pick(Some(Picker::After)), &ws);
    assert!(v.picker.is_none());
}

#[test]
fn suggestions() {
    let ws = ws();
    let users = user_suggestions(&ws, "");
    assert_eq!(users[0].0, "alice");
    assert!(users.iter().any(|u| u.0 == "bob"));
    assert_eq!(user_suggestions(&ws, "@bo")[0].0, "bob");
    let rooms: Vec<String> = room_suggestions(&ws, "#dev")
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(rooms, ["dev-team"]);
}

#[test]
fn highlights_index_the_text_shown() {
    // Bidi controls (3 bytes each) are dropped before the ranges are
    // computed: ranges into the raw text made view() panic mid-character.
    for (text, word) in [
        ("\u{202A}+7912\u{202C} звонить завтра", "звонить"),
        ("\u{202B}Привет, коллеги! Как дела?", "коллеги"),
        ("\u{2068}мир\u{2069} мир", "мир"),
    ] {
        let (snip, ranges) = hit_snippet(text, &[word.to_owned()], 220);
        assert!(!ranges.is_empty(), "{text:?}");
        for r in &ranges {
            assert_eq!(&snip[r.clone()].to_lowercase(), word, "{text:?}");
        }
    }
}

fn server() -> crate::workspace::ServerState {
    let entry = rustket_core::config::ServerEntry {
        id: rustket_core::ServerId("s-search".into()),
        url: "http://localhost:3000".into(),
        ..Default::default()
    };
    crate::workspace::ServerState::from_entry(&entry).unwrap()
}

#[test]
fn closed_view_keeps_its_run_until_the_account_resets() {
    use crate::workspace::MainView;
    use rustket_core::search::{Backend, SearchRun};
    let mut s = server();
    let mut v = SearchView::new("kumquat from:bob".into());
    v.set_run(Some(SearchRun::new(
        7,
        "kumquat".into(),
        Backend::Provider,
        vec!["r1".into()],
        0,
        false,
    )));
    s.main_view = Some(MainView::Search(Box::new(v)));
    // Another view (All unreads, Drafts …) replaces it: put away, run kept.
    s.show_main_view(MainView::Drafts(crate::drafts::DraftsView::new()));
    let stash = s.search.stash.as_ref().unwrap();
    assert_eq!(stash.query, "kumquat from:bob");
    assert_eq!(stash.run.as_ref().map(|r| r.id), Some(7));
    // Logout / another account: nothing of it is left.
    s.reset();
    assert!(s.search.stash.is_none());
}

#[test]
fn provider_lookup_is_retried_after_a_failure() {
    let t0 = std::time::Instant::now();
    // The app's sequence: Loading while a lookup runs, then its failure.
    let failed = Provider::Unknown.loading().failed(t0, None);
    assert!(!failed.due(t0));
    assert!(failed.due(t0 + Duration::from_secs(30)));
    let retry = failed.loading();
    assert!(!retry.due(t0));
    // The back-off doubles across the Loading state of the retry.
    let t1 = t0 + Duration::from_secs(30);
    let again = retry.failed(t1, None);
    assert_eq!(
        again,
        Provider::Failed {
            failures: 2,
            retry_at: t1 + Duration::from_secs(60)
        }
    );
    assert!(!again.due(t1 + Duration::from_secs(30)));
    assert!(again.due(t1 + Duration::from_secs(60)));
    // Capped at 10 minutes.
    let mut p = again;
    for _ in 0..10 {
        p = p.loading().failed(t1, None);
    }
    assert!(!p.due(t1 + Duration::from_secs(599)));
    assert!(p.due(t1 + Duration::from_secs(600)));
    // A success starts counting anew.
    assert_eq!(
        Provider::Known(None).loading(),
        Provider::Loading { failures: 0 }
    );
    // A rate limit's wait, when longer.
    let limited = Provider::Unknown.failed(t0, Some(Duration::from_secs(90)));
    assert!(!limited.due(t0 + Duration::from_secs(60)));
    assert!(Provider::Unknown.due(t0) && !Provider::Unknown.loading().due(t0));
}

#[test]
fn picker_suggestions_follow_the_input() {
    let ws = ws();
    let mut v = SearchView::new(String::new());
    v.update(Msg::Pick(Some(Picker::In)), &ws);
    assert!(v.suggestions.iter().any(|r| r.0 == "dev-team"));
    v.update(Msg::PickerInput("gen".into()), &ws);
    let rooms: Vec<&str> = v.suggestions.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(rooms, ["general"]);
    v.update(Msg::Pick(None), &ws);
    assert!(v.suggestions.is_empty());
}

#[test]
fn snippet_follows_a_server_copy_that_replaced_a_cached_hit() {
    use rustket_core::search::{Backend, SearchRun};
    let mut v = SearchView::new("deploy".into());
    v.set_run(Some(SearchRun::new(
        3,
        "deploy".into(),
        Backend::Provider,
        vec!["r1".into()],
        0,
        false,
    )));
    let words = ["deploy".to_owned()];
    let mut m = RcMessage {
        id: "m1".into(),
        rid: "r1".into(),
        msg: "deploy at 5pm, password hunter2".into(),
        ..Default::default()
    };
    assert!(v.snippet(&m, &words, 220).0.contains("hunter2"));
    // The room's server answer brings the edited text in the same run.
    m.msg = "deploy at 6pm".into();
    let snip = v.snippet(&m, &words, 220);
    assert_eq!(snip.0, "deploy at 6pm");
    assert!(!snip.1.is_empty());
}

#[test]
fn a_query_edited_before_closing_is_searched_on_reopening() {
    use rustket_core::search::{Backend, SearchRun};
    let ws = ws();
    let mut v = SearchView::new("kumquat".into());
    v.set_run(Some(SearchRun::new(
        5,
        "kumquat".into(),
        Backend::Provider,
        vec!["r1".into()],
        0,
        false,
    )));
    assert!(v.resumable());
    // Typed, then closed before the debounce ran out.
    let outs = v.update(Msg::Query("kumquat deploy".into()), &ws);
    assert!(matches!(outs.as_slice(), [Out::Debounce(_)]));
    let v = Box::new(v).stashed();
    assert_eq!(v.query, "kumquat deploy");
    assert!(!v.resumable());
    // No run at all: nothing to resume either.
    assert!(!SearchView::new("kumquat".into()).resumable());
}
