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
