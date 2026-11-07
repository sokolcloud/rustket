#![allow(clippy::unwrap_used, clippy::expect_used)]

use rocketchat::RcDate;
use rocketchat::models::{
    Message, Permission, Room, RoomType, Subscription, UpdatedSince, User, UserRef,
};

use crate::sync::InitialData;
use crate::workspace::Workspace;
use serde_json::Value;

/// A JSON fixture captured by the `rocketchat` crate.
pub fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../rocketchat/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

pub fn ts(ms: i64) -> RcDate {
    RcDate::from_millis(ms).unwrap()
}

pub fn user_ref(id: &str, username: &str) -> UserRef {
    UserRef {
        id: id.into(),
        username: Some(username.into()),
        name: Some(format!("Name {username}")),
    }
}

pub fn msg(id: &str, rid: &str, ms: i64, uid: &str) -> Message {
    Message {
        id: id.into(),
        rid: rid.into(),
        msg: format!("text {id}"),
        ts: Some(ts(ms)),
        u: user_ref(uid, uid),
        updated_at: Some(ts(ms)),
        ..Default::default()
    }
}

pub fn me(id: &str, username: &str) -> User {
    User {
        id: id.into(),
        username: Some(username.into()),
        name: Some(format!("Name {username}")),
        roles: Some(vec!["user".into()]),
        ..Default::default()
    }
}

pub fn sub(rid: &str, name: &str, t: RoomType) -> Subscription {
    Subscription {
        id: format!("sub-{rid}"),
        rid: rid.into(),
        name: name.into(),
        t,
        u: user_ref("me", "me"),
        open: true,
        ..Default::default()
    }
}

pub fn room(rid: &str, name: &str, t: RoomType) -> Room {
    Room {
        id: rid.into(),
        t,
        name: Some(name.into()),
        ..Default::default()
    }
}

/// Workspace loaded from the captured `me`, `subscriptions.get`, `rooms.get` and
/// `permissions.listAll` fixtures (user alice).
pub fn fixture_workspace() -> Workspace {
    let mut ws = Workspace::new();
    let me: User = serde_json::from_value(fixture("me.json")).unwrap();
    let subs: UpdatedSince<Subscription> =
        serde_json::from_value(fixture("subscriptions_get.json")).unwrap();
    let rooms: UpdatedSince<Room> = serde_json::from_value(fixture("rooms_get.json")).unwrap();
    let perms: UpdatedSince<Permission> =
        serde_json::from_value(fixture("permissions_list_all.json")).unwrap();
    ws.apply_initial(InitialData {
        me,
        subscriptions: subs.update,
        rooms: rooms.update,
        permissions: perms.update,
        next_since: Some(ts(1)),
        ..Default::default()
    });
    ws
}
