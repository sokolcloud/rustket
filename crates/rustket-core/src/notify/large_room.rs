//! Mentions in large rooms.
//!
//! In a room with more members than `Notifications_Max_Room_Members` (≠ 0)
//! Rocket.Chat selects no subscription for a plain `@username` unless the
//! receiver has highlight words or follows the thread
//! (`sendNotificationsOnMessage.ts`, `disableAllMessageNotifications`), so
//! no `{uid}/notification` arrives. The mention still raises the
//! subscription's `userMentions`; this module turns that into the same
//! notification the server would have sent, built from the room's last
//! message, once [`DELAY`] passed without a server notification of it.
//! `@all`/`@here` get no fallback: Rocket.Chat stays silent there on
//! purpose.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rocketchat::RcDate;
use rocketchat::models::{Message, Room, Subscription};
use rocketchat::session::{DesktopNotification, NotificationMessage, NotificationPayload};

use crate::config::ServerId;
use crate::workspace::Workspace;

/// How long a server notification of the message may take to arrive (it
/// wins: the receiver has highlight words).
pub const DELAY: Duration = Duration::from_secs(3);
/// Rooms waiting for their check, over all servers.
const PENDING_CAP: usize = 64;
/// Message ids remembered as notified, over all servers.
const SEEN_CAP: usize = 512;

/// A live update of the subscription from `before` to `after` starts the
/// fallback: its `userMentions` grew in a room (`room`) with more members
/// than `limit` (`Notifications_Max_Room_Members`, 0 = no limit).
pub fn triggers(
    before: Option<&Subscription>,
    after: &Subscription,
    room: Option<&Room>,
    limit: u64,
) -> bool {
    limit != 0
        && before.is_some_and(|b| b.rid == after.rid && after.user_mentions > b.user_mentions)
        && room.and_then(|r| r.users_count).is_some_and(|n| n > limit)
}

/// The room-level gates the server would apply first: "Receive
/// notifications" off (`disableNotifications`) and a desktop alert of
/// "nothing" (the room's own, else the account's `account_desktop`).
pub fn allowed(sub: &Subscription, account_desktop: Option<&str>) -> bool {
    sub.disable_notifications != Some(true)
        && sub.desktop_notifications.as_deref().or(account_desktop) != Some("nothing")
}

/// The message to notify for a mention counted by a subscription update
/// that came after the subscription's change at `about` (the `_updatedAt`
/// the client held before the update: Rocket.Chat sends the update with the
/// `_updatedAt` it read before counting, so the update's own is no newer).
/// It is the room's last message if that is newer than `about`, mentions
/// the user by name, is not the user's own nor from a user the user ignores
/// in this room (the server skips ignored senders), and is still unread.
/// Anything else is skipped: the fallback never shows a different message.
pub fn mention<'a>(
    room: &'a Room,
    sub: &Subscription,
    about: RcDate,
    my_id: &str,
    my_username: &str,
) -> Option<&'a Message> {
    let m = room.last_message.as_deref()?;
    let ts = m.ts?;
    let unread = sub.ls.is_none_or(|ls| ts > ls);
    let ignored = sub.ignored.iter().flatten().any(|u| *u == m.u.id);
    // Plain messages, and encrypted ones (their mentions are in clear).
    let kind = m.t.as_deref().is_none_or(|t| t == "e2e");
    (m.rid == sub.rid
        && kind
        && !my_id.is_empty()
        && m.u.id != my_id
        && !ignored
        && ts > about
        && unread
        && crate::inbox::mentions_me(m, my_username, my_id))
    .then_some(m)
}

/// The account's `desktopNotifications` preference (`/me`).
fn account_desktop(ws: &Workspace) -> Option<&str> {
    ws.me()
        .and_then(|m| m.settings.as_ref())
        .and_then(|s| s.preferences.as_ref())
        .and_then(|p| p.get("desktopNotifications"))
        .and_then(|v| v.as_str())
}

/// The notification the server would have sent for the mention in `rid`
/// counted after the subscription's change at `about` (see [`mention`]),
/// as `notifyDesktopUser` builds it for a room: title "#room", text "sender: message" (`real_names`: display names).
/// `None` when [`allowed`] or [`mention`] rule it out, or the mention was
/// read meanwhile.
pub fn notification(
    ws: &Workspace,
    rid: &str,
    about: RcDate,
    real_names: bool,
) -> Option<DesktopNotification> {
    let entry = ws.room(rid)?;
    let sub = entry.sub.as_ref()?;
    let room = entry.room.as_ref()?;
    if sub.user_mentions == 0 || !allowed(sub, account_desktop(ws)) {
        return None;
    }
    let m = mention(
        room,
        sub,
        about,
        ws.user_id().unwrap_or_default(),
        ws.username().unwrap_or_default(),
    )?;
    let sender = if real_names {
        m.u.name.as_deref().or(m.u.username.as_deref())
    } else {
        m.u.username.as_deref().or(m.u.name.as_deref())
    }
    .unwrap_or_default();
    let encrypted = m.content.is_some();
    let msg = if encrypted { "" } else { m.msg.as_str() };
    Some(DesktopNotification {
        title: format!("#{}", crate::room_list::display_name(ws, entry, real_names)),
        text: format!("{sender}: {msg}"),
        payload: NotificationPayload {
            id: m.id.clone(),
            rid: m.rid.clone(),
            tmid: m.tmid.clone(),
            sender: Some(m.u.clone()),
            room_type: Some(room.t),
            name: room.name.clone(),
            message: NotificationMessage {
                msg: msg.to_owned(),
                t: m.t.clone(),
                content: m.content.clone(),
            },
            audio_notification_value: sub.audio_notification_value.clone(),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// A room waiting for its check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub server: ServerId,
    pub rid: String,
    /// `_updatedAt` of the subscription before the update that counted
    /// the mention.
    pub about: RcDate,
    pub due: Instant,
}

/// The fallback's state for every account: rooms waiting for their check
/// and the message ids already notified (by the server or by the
/// fallback). Both are bounded.
#[derive(Debug, Default)]
pub struct Fallback {
    pending: Vec<Pending>,
    seen: VecDeque<(ServerId, String)>,
    /// The messages the fallback itself notified (a subset of `seen`).
    shown: VecDeque<(ServerId, String)>,
}

impl Fallback {
    /// A mention was counted in `rid` (see [`triggers`]): checks the room
    /// at `now + DELAY`. `true` when a check was added (the caller arms a
    /// timer for [`DELAY`]); a room already waiting keeps its time and its
    /// first `about`.
    pub fn arm(&mut self, server: &ServerId, rid: &str, about: RcDate, now: Instant) -> bool {
        if self
            .pending
            .iter()
            .any(|p| &p.server == server && p.rid == rid)
        {
            return false;
        }
        if self.pending.len() >= PENDING_CAP {
            self.pending.remove(0);
        }
        self.pending.push(Pending {
            server: server.clone(),
            rid: rid.to_owned(),
            about,
            due: now + DELAY,
        });
        true
    }

    /// The checks due at `now`, removed.
    pub fn take_due(&mut self, now: Instant) -> Vec<Pending> {
        let (due, wait) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| p.due <= now);
        self.pending = wait;
        due
    }

    /// Message `mid` of `server` was notified (a server `notification`
    /// event, shown or not, or the fallback itself).
    pub fn note(&mut self, server: &ServerId, mid: &str) {
        if mid.is_empty() || self.seen(server, mid) {
            return;
        }
        if self.seen.len() >= SEEN_CAP {
            self.seen.pop_front();
        }
        self.seen.push_back((server.clone(), mid.to_owned()));
    }

    pub fn seen(&self, server: &ServerId, mid: &str) -> bool {
        self.seen.iter().any(|(s, m)| s == server && m == mid)
    }

    /// The fallback notified message `mid` of `server` itself.
    pub fn showed(&mut self, server: &ServerId, mid: &str) {
        self.note(server, mid);
        if mid.is_empty() || self.shown_by_fallback(server, mid) {
            return;
        }
        if self.shown.len() >= SEEN_CAP {
            self.shown.pop_front();
        }
        self.shown.push_back((server.clone(), mid.to_owned()));
    }

    /// A server notification of `mid` that arrives late: the fallback
    /// already showed it, so it is not shown (nor heard) again.
    pub fn shown_by_fallback(&self, server: &ServerId, mid: &str) -> bool {
        self.shown.iter().any(|(s, m)| s == server && m == mid)
    }

    /// The account signed out or was removed.
    pub fn forget(&mut self, server: &ServerId) {
        self.pending.retain(|p| &p.server != server);
        self.seen.retain(|(s, _)| s != server);
        self.shown.retain(|(s, _)| s != server);
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{msg, room as mk_room, sub as mk_sub, ts};
    use crate::workspace::Change;
    use rocketchat::models::{Mention, RoomType, User};
    use serde_json::json;

    const T: i64 = 1_800_000_000_000;
    /// The subscription's change before the counting update, as the
    /// update itself carries it (stale).
    const BEFORE: i64 = T - 300_000;

    fn mention_of(id: &str, username: &str) -> Mention {
        Mention {
            id: id.into(),
            username: Some(username.into()),
            ..Default::default()
        }
    }

    fn big_room(users: u64, last: Option<Message>) -> Room {
        Room {
            users_count: Some(users),
            last_message: last.map(Box::new),
            ..mk_room("r1", "general", RoomType::Channel)
        }
    }

    fn mentioning(id: &str, ms: i64) -> Message {
        Message {
            mentions: Some(vec![mention_of("me", "me")]),
            ..msg(id, "r1", ms, "bob")
        }
    }

    fn sub_with(mentions: u64) -> Subscription {
        Subscription {
            user_mentions: mentions,
            unread: mentions,
            updated_at: Some(ts(T)),
            ls: Some(ts(T - 600_000)),
            ..mk_sub("r1", "general", RoomType::Channel)
        }
    }

    fn workspace(room: Room, sub: Subscription, prefs: serde_json::Value) -> Workspace {
        let mut ws = Workspace::new();
        let mut me: User = serde_json::from_value(json!({
            "_id": "me", "username": "me",
            "settings": {"preferences": prefs}
        }))
        .unwrap();
        me.name = Some("Me".into());
        ws.apply_me(me);
        ws.apply_room(Change::Inserted, room);
        ws.apply_subscription(Change::Inserted, sub);
        ws
    }

    #[test]
    fn trigger_needs_more_mentions_in_a_large_room() {
        let before = sub_with(0);
        let after = sub_with(1);
        let large = big_room(101, None);
        assert!(triggers(Some(&before), &after, Some(&large), 100));
        // At the limit, no limit, unknown size, no previous state.
        assert!(!triggers(
            Some(&before),
            &after,
            Some(&big_room(100, None)),
            100
        ));
        assert!(!triggers(Some(&before), &after, Some(&large), 0));
        assert!(!triggers(Some(&before), &after, None, 100));
        let unknown = Room {
            users_count: None,
            ..large.clone()
        };
        assert!(!triggers(Some(&before), &after, Some(&unknown), 100));
        assert!(!triggers(None, &after, Some(&large), 100));
        // Unchanged or fewer mentions (read), plain unread only.
        assert!(!triggers(Some(&after), &after, Some(&large), 100));
        assert!(!triggers(Some(&after), &before, Some(&large), 100));
        let plain = Subscription {
            unread: 5,
            ..sub_with(0)
        };
        assert!(!triggers(Some(&before), &plain, Some(&large), 100));
        // A group mention raises groupMentions only: no fallback.
        let group = Subscription {
            group_mentions: 1,
            ..sub_with(0)
        };
        assert!(!triggers(Some(&before), &group, Some(&large), 100));
    }

    #[test]
    fn room_gates() {
        let s = sub_with(1);
        assert!(allowed(&s, None));
        assert!(allowed(&s, Some("all")));
        assert!(!allowed(&s, Some("nothing")), "account alert nothing");
        let muted = Subscription {
            disable_notifications: Some(true),
            ..s.clone()
        };
        assert!(!allowed(&muted, None), "R1");
        let nothing = Subscription {
            desktop_notifications: Some("nothing".into()),
            ..s.clone()
        };
        assert!(!allowed(&nothing, Some("all")), "room alert nothing");
        let own = Subscription {
            desktop_notifications: Some("mentions".into()),
            ..s
        };
        assert!(allowed(&own, Some("nothing")), "the room's own alert wins");
    }

    #[test]
    fn only_the_mentioning_last_message() {
        let s = sub_with(1);
        let about = ts(BEFORE);
        let pick = |m: Option<Message>| {
            mention(&big_room(500, m), &s, about, "me", "me").map(|m| m.id.clone())
        };
        assert_eq!(
            pick(Some(mentioning("m1", T - 1_000))).as_deref(),
            Some("m1")
        );
        // By username only, or by id only.
        let by_name = Message {
            mentions: Some(vec![Mention {
                id: "x".into(),
                username: Some("me".into()),
                ..Default::default()
            }]),
            ..msg("m2", "r1", T, "bob")
        };
        assert!(pick(Some(by_name)).is_some());
        // No last message, no mention of me, @all, own message.
        assert_eq!(pick(None), None);
        assert_eq!(pick(Some(msg("m3", "r1", T, "bob"))), None);
        let all = Message {
            mentions: Some(vec![mention_of("all", "all")]),
            ..msg("m4", "r1", T, "bob")
        };
        assert_eq!(pick(Some(all)), None);
        let own = Message {
            mentions: Some(vec![mention_of("me", "me")]),
            ..msg("m5", "r1", T, "me")
        };
        assert_eq!(pick(Some(own)), None);
        // An older mention (the counted one is not the last message: e.g.
        // deleted, the room's last message went back), an already read
        // one, a system message.
        assert_eq!(pick(Some(mentioning("m6", BEFORE))), None);
        assert_eq!(pick(Some(mentioning("m6", T - 86_400_000))), None);
        // A sender the user ignores in this room: the server skips it.
        let ignoring = Subscription {
            ignored: Some(vec!["bob".into()]),
            ..s.clone()
        };
        assert!(
            mention(
                &big_room(500, Some(mentioning("m9", T))),
                &ignoring,
                about,
                "me",
                "me"
            )
            .is_none()
        );
        let read = Subscription {
            ls: Some(ts(T)),
            ..s.clone()
        };
        assert!(
            mention(
                &big_room(500, Some(mentioning("m7", T))),
                &read,
                about,
                "me",
                "me"
            )
            .is_none()
        );
        let system = Message {
            t: Some("uj".into()),
            ..mentioning("m8", T)
        };
        assert_eq!(pick(Some(system)), None);
    }

    #[test]
    fn builds_the_server_notification() {
        let last = Message {
            msg: "@me look".into(),
            tmid: Some("t1".into()),
            tshow: Some(true),
            ..mentioning("m1", T)
        };
        let s = Subscription {
            audio_notification_value: Some("ding".into()),
            ..sub_with(1)
        };
        let ws = workspace(big_room(500, Some(last)), s, json!({}));
        let n = notification(&ws, "r1", ts(BEFORE), false).unwrap();
        assert_eq!(n.title, "#general");
        assert_eq!(n.text, "bob: @me look");
        assert_eq!(n.payload.id, "m1");
        assert_eq!(n.payload.rid, "r1");
        assert_eq!(n.payload.tmid.as_deref(), Some("t1"));
        assert_eq!(n.payload.sender.as_ref().unwrap().id, "bob");
        assert_eq!(n.payload.room_type, Some(RoomType::Channel));
        assert_eq!(n.payload.message.msg, "@me look");
        assert_eq!(n.payload.audio_notification_value.as_deref(), Some("ding"));
        let real = notification(&ws, "r1", ts(BEFORE), true).unwrap();
        assert_eq!(real.text, "Name bob: @me look");

        // Encrypted: the content goes along for decryption, not the text.
        let enc = Message {
            msg: "ciphertext".into(),
            t: Some("e2e".into()),
            content: Some(json!({"algorithm": "rc.v2.aes-sha2"})),
            ..mentioning("m2", T)
        };
        let ws = workspace(big_room(500, Some(enc)), sub_with(1), json!({}));
        let n = notification(&ws, "r1", ts(BEFORE), false).unwrap();
        assert_eq!(
            (n.text.as_str(), n.payload.message.msg.as_str()),
            ("bob: ", "")
        );
        assert!(n.payload.message.content.is_some());
    }

    #[test]
    fn notification_gates() {
        let last = || Some(mentioning("m1", T));
        // Account alert "nothing".
        let ws = workspace(
            big_room(500, last()),
            sub_with(1),
            json!({"desktopNotifications": "nothing"}),
        );
        assert!(notification(&ws, "r1", ts(BEFORE), false).is_none());
        // R1.
        let s = Subscription {
            disable_notifications: Some(true),
            ..sub_with(1)
        };
        let ws = workspace(big_room(500, last()), s, json!({}));
        assert!(notification(&ws, "r1", ts(BEFORE), false).is_none());
        // Read meanwhile: no mention left.
        let ws = workspace(big_room(500, last()), sub_with(0), json!({}));
        assert!(notification(&ws, "r1", ts(BEFORE), false).is_none());
        // Unknown room.
        assert!(notification(&ws, "nope", ts(BEFORE), false).is_none());
    }

    #[test]
    fn dedupe_and_bounds() {
        let a = ServerId("a".into());
        let b = ServerId("b".into());
        let now = Instant::now();
        let mut f = Fallback::default();
        assert!(f.arm(&a, "r1", ts(T), now));
        // The same room waits once; another account's room is separate.
        assert!(!f.arm(&a, "r1", ts(T + 1), now + Duration::from_secs(1)));
        assert!(f.arm(&b, "r1", ts(T), now + Duration::from_secs(1)));
        assert_eq!(f.pending(), 2);
        assert!(f.take_due(now + Duration::from_secs(2)).is_empty());
        let due = f.take_due(now + DELAY);
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].server.clone(), due[0].about), (a.clone(), ts(T)));
        assert_eq!(f.take_due(now + DELAY + Duration::from_secs(1)).len(), 1);
        assert_eq!(f.pending(), 0);

        // Server notifications are remembered per account.
        f.note(&a, "m1");
        f.note(&a, "m1");
        f.note(&a, "");
        assert!(f.seen(&a, "m1"));
        assert!(!f.seen(&b, "m1"));
        assert!(!f.seen(&a, ""));

        // Bounded.
        for i in 0..SEEN_CAP + 10 {
            f.note(&b, &format!("x{i}"));
        }
        assert!(f.seen.len() <= SEEN_CAP);
        assert!(!f.seen(&a, "m1"), "the oldest went first");
        for i in 0..PENDING_CAP + 10 {
            f.arm(&a, &format!("r{i}"), ts(T), now);
        }
        assert_eq!(f.pending(), PENDING_CAP);

        // What the fallback showed itself is told apart.
        f.showed(&a, "m2");
        assert!(f.seen(&a, "m2") && f.shown_by_fallback(&a, "m2"));
        assert!(!f.shown_by_fallback(&a, "m1") && !f.shown_by_fallback(&b, "m2"));

        f.forget(&a);
        assert!(!f.shown_by_fallback(&a, "m2"));
        assert_eq!(f.pending(), 0);
        assert!(f.seen(&b, &format!("x{}", SEEN_CAP + 9)));
        f.forget(&b);
        assert!(f.seen.is_empty());
    }
}
