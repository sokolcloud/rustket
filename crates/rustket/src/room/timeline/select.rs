//! Selecting messages of the room timeline to forward them or copy their
//! links.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use rocketchat::ServerUrl;
use rustket_core::Workspace;
use rustket_core::timeline::{DeliveryState, TimelineItem};

use super::actions::{self, Act};
use super::interact::{self, Source};
use crate::room::ForwardRef;

/// Most messages selected at once.
pub const MAX_PICKED: usize = 20;

/// The selection is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Whether `item` can be selected: sent, and forwardable (not a system,
/// encrypted or omnichannel message).
pub fn pickable(ws: &Workspace, source: &Source, item: &TimelineItem, now: DateTime<Utc>) -> bool {
    item.state == DeliveryState::Sent
        && interact::available(ws, source, &item.message, now).contains(&Act::Forward)
}

/// Adds `mid` to `set`, or removes it when it is there; refused past `max`.
pub fn toggle(set: &mut BTreeSet<String>, mid: &str, max: usize) -> Result<(), Full> {
    if set.remove(mid) {
        return Ok(());
    }
    if set.len() >= max {
        return Err(Full);
    }
    set.insert(mid.to_owned());
    Ok(())
}

/// The selected messages of room `rid`, oldest first.
pub fn ordered_refs(ws: &Workspace, rid: &str, set: &BTreeSet<String>) -> Vec<ForwardRef> {
    ws.timeline(rid)
        .into_iter()
        .flat_map(|t| t.iter())
        .filter(|i| set.contains(&i.message.id))
        .map(|i| ForwardRef::of(&i.message))
        .collect()
}

/// Permalinks of the selected messages of room `rid`, oldest first, one per
/// line.
pub fn links(server: &ServerUrl, ws: &Workspace, rid: &str, set: &BTreeSet<String>) -> String {
    ws.timeline(rid)
        .into_iter()
        .flat_map(|t| t.iter())
        .filter(|i| set.contains(&i.message.id))
        .map(|i| actions::permalink(server, ws, &i.message))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::{Message, Room, RoomType, Setting, Subscription, User, UserRef};
    use rustket_core::sync::InitialData;

    const NOW: i64 = 1_800_000_000_000;

    fn ws(t: RoomType) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                roles: Some(vec!["user".into()]),
                ..Default::default()
            },
            subscriptions: vec![Subscription {
                id: "s1".into(),
                rid: "r1".into(),
                name: "dev".into(),
                t,
                open: true,
                u: UserRef {
                    id: "me".into(),
                    username: Some("me".into()),
                    name: None,
                },
                updated_at: RcDate::from_millis(NOW),
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "r1".into(),
                t,
                name: Some("dev".into()),
                updated_at: RcDate::from_millis(NOW),
                ..Default::default()
            }],
            settings: vec![Setting {
                id: "Site_Url".into(),
                value: serde_json::json!("https://site.example/"),
                ..Default::default()
            }],
            next_since: RcDate::from_millis(NOW),
            ..Default::default()
        });
        ws.open_room("r1");
        let msgs = [("m2", 2), ("m1", 1), ("m3", 3)].map(|(id, n)| Message {
            id: id.into(),
            rid: "r1".into(),
            msg: format!("text {id}\nmore"),
            ts: RcDate::from_millis(NOW - 60_000 + n * 1000),
            u: UserRef {
                id: "bob".into(),
                username: Some("bob".into()),
                name: None,
            },
            ..Default::default()
        });
        for m in msgs {
            ws.apply_message(m);
        }
        ws
    }

    fn now() -> DateTime<Utc> {
        RcDate::from_millis(NOW).unwrap().0
    }

    #[test]
    fn toggle_caps_the_selection() {
        let mut set = BTreeSet::new();
        for i in 0..MAX_PICKED {
            assert_eq!(toggle(&mut set, &format!("m{i}"), MAX_PICKED), Ok(()));
        }
        assert_eq!(toggle(&mut set, "extra", MAX_PICKED), Err(Full));
        // Removing still works when full.
        assert_eq!(toggle(&mut set, "m0", MAX_PICKED), Ok(()));
        assert_eq!(set.len(), MAX_PICKED - 1);
        assert_eq!(toggle(&mut set, "extra", MAX_PICKED), Ok(()));
    }

    #[test]
    fn refs_and_links_are_chronological() {
        let w = ws(RoomType::Channel);
        let set: BTreeSet<String> = ["m3", "m1"].map(String::from).into();
        let refs = ordered_refs(&w, "r1", &set);
        let ids: Vec<&str> = refs.iter().map(|r| r.mid.as_str()).collect();
        assert_eq!(ids, ["m1", "m3"]);
        assert_eq!(refs[0].author, "bob");
        assert_eq!(refs[0].excerpt, "text m1");
        assert_eq!(refs[0].rid, "r1");
        let server = ServerUrl::parse("https://chat.example.com").unwrap();
        assert_eq!(
            links(&server, &w, "r1", &set),
            "https://site.example/channel/dev?msg=m1\nhttps://site.example/channel/dev?msg=m3"
        );
    }

    #[test]
    fn only_forwardable_messages_are_pickable() {
        let w = ws(RoomType::Channel);
        let item = |w: &Workspace, id: &str| w.timeline("r1").unwrap().get(id).cloned().unwrap();
        assert!(pickable(&w, &Source::Room, &item(&w, "m1"), now()));
        // System messages and omnichannel rooms are not.
        let mut sys = item(&w, "m1");
        sys.message.t = Some("uj".into());
        assert!(!pickable(&w, &Source::Room, &sys, now()));
        let mut pending = item(&w, "m1");
        pending.state = DeliveryState::Sending;
        assert!(!pickable(&w, &Source::Room, &pending, now()));
        let live = ws(RoomType::Livechat);
        assert!(!pickable(&live, &Source::Room, &item(&live, "m1"), now()));
    }
}
