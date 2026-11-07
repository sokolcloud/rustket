//! Opening a room found outside the sidebar (directory, quick switcher, new
//! rooms): the room (and subscription, if any) is put into the workspace first,
//! because the app only opens rooms it knows. Rooms the user has not joined
//! open as a preview.

use rocketchat::models::{Room, RoomType, Subscription};
use rocketchat::{RestClient, Result};
use rustket_core::{Change, Workspace};

use crate::component::{Action, Ctx, Effect};
use crate::error::Res;

/// What to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A room by id (joined or not).
    Room(String),
    /// Join a public room, then open it.
    Join(String),
    /// A direct message with a user (created if needed).
    User(String),
    /// A group DM with these users.
    Users(Vec<String>),
}

/// A room fetched from the server, ready for the workspace.
#[derive(Debug, Clone)]
pub struct Opened {
    pub room: Room,
    pub sub: Option<Subscription>,
}

/// The existing 1:1 DM with `username`, if the sidebar has it.
pub fn local_dm(ws: &Workspace, username: &str) -> Option<String> {
    ws.rooms()
        .values()
        .find(|e| {
            e.room_type() == RoomType::Direct && e.sub.as_ref().is_some_and(|s| s.name == username)
        })
        .map(|e| e.rid().to_owned())
}

/// Whether `target` can be opened without a request. A hidden room
/// (`open: false`) needs `rooms.open` first to come back to the sidebar.
pub fn local(ws: &Workspace, target: &Target) -> Option<String> {
    let rid = match target {
        Target::Room(rid) => ws.room(rid).map(|_| rid.clone()),
        Target::Join(rid) => ws.subscription(rid).map(|_| rid.clone()),
        Target::User(u) => local_dm(ws, u),
        Target::Users(_) => None,
    }?;
    let hidden = ws.subscription(&rid).is_some_and(|s| !s.open);
    (!hidden).then_some(rid)
}

/// The room and the user's subscription; a hidden subscription is opened
/// again (`rooms.open`).
async fn fetch(rest: &RestClient, rid: &str) -> Result<Opened> {
    let room = rest.rooms_info(rid).await?;
    let mut sub = rest.subscriptions_get_one(rid).await.ok();
    if let Some(s) = sub.as_mut().filter(|s| !s.open) {
        rest.rooms_open(rid).await?;
        s.open = true;
    }
    Ok(Opened { room, sub })
}

pub async fn resolve(rest: RestClient, target: Target) -> Result<Opened> {
    match target {
        Target::Room(rid) => fetch(&rest, &rid).await,
        Target::Join(rid) => {
            rest.rooms_join(&rid, None).await?;
            fetch(&rest, &rid).await
        }
        Target::User(u) => {
            let r = rest.im_create(&u).await?;
            fetch(&rest, &r.id).await
        }
        Target::Users(list) => {
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            let r = if refs.len() == 1 {
                rest.im_create(refs[0]).await?
            } else {
                rest.im_create_group(&refs).await?
            };
            fetch(&rest, &r.id).await
        }
    }
}

/// Puts a fetched room into the workspace; returns its id.
pub fn apply(ws: &mut Workspace, o: Opened) -> String {
    let rid = o.room.id.clone();
    ws.apply_room(Change::Updated, o.room);
    if let Some(s) = o.sub {
        ws.apply_subscription(Change::Updated, s);
    }
    rid
}

/// Opens `target`: right away when the workspace has it, else after fetching
/// it (`map` wraps the result; feed it back to [`finish`]).
pub fn open<M: Send + 'static>(
    ctx: &Ctx<'_>,
    target: Target,
    map: impl FnOnce(Res<Opened>) -> M + Send + 'static,
) -> Action<M> {
    if let Some(rid) = local(ctx.workspace, &target) {
        return Action::effect(Effect::OpenRoom(rid));
    }
    Action::task(ctx.rest_task(move |rest| resolve(rest, target), map))
}

/// Applies a fetched room and asks the app to open it.
pub fn finish<M>(ws: &mut Workspace, r: Res<Opened>) -> std::result::Result<Action<M>, String> {
    match r {
        Ok(o) => {
            let rid = apply(ws, o);
            Ok(Action::effect(Effect::OpenRoom(rid)))
        }
        Err(e) => Err(e.user_text()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::UserRef;

    #[test]
    fn local_targets() {
        let mut ws = Workspace::new();
        let room = Room {
            id: "d1".into(),
            t: RoomType::Direct,
            ..Default::default()
        };
        let sub = Subscription {
            id: "s".into(),
            rid: "d1".into(),
            name: "bob".into(),
            t: RoomType::Direct,
            open: true,
            u: UserRef {
                id: "me".into(),
                username: None,
                name: None,
            },
            ..Default::default()
        };
        assert_eq!(
            apply(
                &mut ws,
                Opened {
                    room,
                    sub: Some(sub)
                }
            ),
            "d1"
        );
        assert_eq!(local(&ws, &Target::User("bob".into())), Some("d1".into()));
        assert_eq!(local(&ws, &Target::User("carol".into())), None);
        assert_eq!(local(&ws, &Target::Room("d1".into())), Some("d1".into()));
        assert_eq!(local(&ws, &Target::Join("d1".into())), Some("d1".into()));
        // A previewed room (no subscription) still needs the join.
        ws.apply_room(
            Change::Updated,
            Room {
                id: "c1".into(),
                t: RoomType::Channel,
                ..Default::default()
            },
        );
        assert_eq!(local(&ws, &Target::Room("c1".into())), Some("c1".into()));
        assert_eq!(local(&ws, &Target::Join("c1".into())), None);
        // A hidden DM is opened through the server (`rooms.open`).
        let mut sub = ws.subscription("d1").unwrap().clone();
        sub.open = false;
        ws.apply_subscription(Change::Updated, sub);
        assert_eq!(local(&ws, &Target::User("bob".into())), None);
        assert_eq!(local(&ws, &Target::Room("d1".into())), None);
    }
}
