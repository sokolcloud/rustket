//! Message actions (hover toolbar, "more" menu, right-click menu) and their
//! permission gating; permalinks.

use chrono::{DateTime, Utc};
use rocketchat::ServerUrl;
use rocketchat::models::{Message, RoomType};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions as perm;
use rustket_ui::icon::Icon;
use rustket_ui::widget::{MenuEntry, MenuItem};

/// One message action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Act {
    React,
    ReplyInThread,
    Quote,
    CopyText,
    CopyLink,
    Edit,
    Pin,
    Unpin,
    Star,
    Unstar,
    MarkUnread,
    Follow,
    Unfollow,
    Report,
    Delete,
}

impl Act {
    pub fn icon(self) -> Icon {
        match self {
            Act::React => Icon::SmilePlus,
            Act::ReplyInThread => Icon::MessageCircle,
            Act::Quote => Icon::Quote,
            Act::CopyText => Icon::Copy,
            Act::CopyLink => Icon::Link,
            Act::Edit => Icon::Pencil,
            Act::Pin => Icon::Pin,
            Act::Unpin => Icon::PinOff,
            Act::Star => Icon::Star,
            Act::Unstar => Icon::StarOff,
            Act::MarkUnread => Icon::MailOpen,
            Act::Follow => Icon::Bell,
            Act::Unfollow => Icon::BellOff,
            Act::Report => Icon::Flag,
            Act::Delete => Icon::Trash2,
        }
    }

    pub fn label(self) -> String {
        let key = match self {
            Act::React => "room_timeline.act_react",
            Act::ReplyInThread => "room_timeline.act_reply_thread",
            Act::Quote => "room_timeline.act_quote",
            Act::CopyText => "room_timeline.act_copy_text",
            Act::CopyLink => "room_timeline.act_copy_link",
            Act::Edit => "room_timeline.act_edit",
            Act::Pin => "room_timeline.act_pin",
            Act::Unpin => "room_timeline.act_unpin",
            Act::Star => "room_timeline.act_star",
            Act::Unstar => "room_timeline.act_unstar",
            Act::MarkUnread => "room_timeline.act_mark_unread",
            Act::Follow => "room_timeline.act_follow",
            Act::Unfollow => "room_timeline.act_unfollow",
            Act::Report => "room_timeline.act_report",
            Act::Delete => "room_timeline.act_delete",
        };
        t!(key).into_owned()
    }

    /// Shown directly in the hover toolbar (the rest is in the "more" menu).
    pub fn in_toolbar(self) -> bool {
        matches!(self, Act::React | Act::ReplyInThread | Act::Quote)
    }
}

/// Actions the current user may take on `m` (sent messages only; pending and
/// failed messages have retry/discard instead). Every entry is gated with
/// the `rustket_core::permissions` helpers, in menu order.
pub fn available(ws: &Workspace, m: &Message, now: DateTime<Utc>) -> Vec<Act> {
    let mut v = Vec::new();
    if m.is_system() {
        return v;
    }
    let rid = m.rid.as_str();
    let me = ws.user_id();
    let own = me.is_some_and(|id| id == m.u.id);
    let subscribed = ws.subscription(rid).is_some();
    if perm::can_react(ws, rid) {
        v.push(Act::React);
    }
    if m.tmid.is_none() && perm::can_reply_in_thread(ws, m) {
        v.push(Act::ReplyInThread);
    }
    if perm::can_post(ws, rid) {
        v.push(Act::Quote);
    }
    if !super::super::render::plain_text(m).trim().is_empty() {
        v.push(Act::CopyText);
    }
    v.push(Act::CopyLink);
    if perm::can_edit_message(ws, m, now) {
        v.push(Act::Edit);
    }
    if perm::can_pin(ws, rid) {
        v.push(if m.pinned == Some(true) {
            Act::Unpin
        } else {
            Act::Pin
        });
    }
    if perm::can_star(ws, rid) {
        let starred = me.is_some_and(|id| m.is_starred_by(id));
        v.push(if starred { Act::Unstar } else { Act::Star });
    }
    let omnichannel = ws
        .room(rid)
        .is_some_and(|e| e.room_type() == RoomType::Livechat);
    if subscribed && !own && !omnichannel {
        v.push(Act::MarkUnread);
    }
    if ws.settings().threads_enabled() && !omnichannel {
        // Like the web: any message; a reply follows its parent thread.
        let replies = match m.tmid.as_deref() {
            Some(tmid) => ws
                .thread(tmid)
                .and_then(|t| t.parent())
                .or_else(|| {
                    ws.timeline(rid)
                        .and_then(|t| t.get(tmid))
                        .map(|p| &p.message)
                })
                .and_then(|p| p.replies.as_ref()),
            None => m.replies.as_ref(),
        };
        let following = me.is_some_and(|id| replies.is_some_and(|r| r.iter().any(|u| u == id)));
        v.push(if following {
            Act::Unfollow
        } else {
            Act::Follow
        });
    }
    if subscribed && !own && me.is_some() && !omnichannel {
        v.push(Act::Report);
    }
    if perm::can_delete_message(ws, m, now) {
        v.push(Act::Delete);
    }
    v
}

/// Menu entries for `acts`, with a separator before the destructive group.
pub fn menu_entries<M: Clone>(acts: &[Act], msg: impl Fn(Act) -> M) -> Vec<MenuEntry<M>> {
    let mut out = Vec::new();
    for a in acts {
        let destructive = |x: &Act| matches!(x, Act::Report | Act::Delete);
        if destructive(a)
            && !out.is_empty()
            && !acts.iter().take_while(|x| *x != a).any(destructive)
        {
            out.push(MenuEntry::Separator);
        }
        out.push(MenuEntry::Item(
            MenuItem::new(a.label(), msg(*a))
                .icon(a.icon())
                .danger(matches!(a, Act::Delete | Act::Report)),
        ));
    }
    out
}

/// Base of links shared with others: the `Site_Url` setting (the server
/// turns only links containing it into quotes), else the connection URL.
fn site_base(server: &ServerUrl, ws: &Workspace) -> String {
    ws.settings()
        .str("Site_Url")
        .map(str::trim)
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .filter(|s| url::Url::parse(s).is_ok())
        .map_or_else(|| server.join(""), str::to_owned)
        .trim_end_matches('/')
        .to_owned()
}

/// Percent-encodes one path segment (spaces as `%20`).
fn segment(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// Web URL of channel `name` (for channels the user has not joined).
pub fn channel_url(server: &ServerUrl, ws: &Workspace, name: &str) -> Option<String> {
    let name = name.trim().trim_start_matches('#');
    (!name.is_empty()).then(|| format!("{}/channel/{}", site_base(server, ws), segment(name)))
}

/// `{Site_Url}/{channel|group|direct|live}/{name or rid}?msg={id}` like the
/// web client's `getPermaLink`.
pub fn permalink(server: &ServerUrl, ws: &Workspace, m: &Message) -> String {
    let entry = ws.room(&m.rid);
    let kind = entry.map(|e| e.room_type()).unwrap_or_default();
    let (route, target) = match kind {
        RoomType::Channel => ("channel", entry.map(|e| e.name().to_owned())),
        RoomType::Private => ("group", entry.map(|e| e.name().to_owned())),
        RoomType::Livechat => ("live", None),
        _ => ("direct", None),
    };
    let target = target
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| m.rid.clone());
    format!(
        "{}/{route}/{}?msg={}",
        site_base(server, ws),
        segment(&target),
        segment(&m.id)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::{Permission, Room, Setting, Subscription, User, UserRef};
    use rustket_core::sync::InitialData;

    const NOW: i64 = 1_800_000_000_000;

    fn ws(
        perms: &[(&str, &[&str])],
        settings: &[(&str, serde_json::Value)],
        t: RoomType,
    ) -> Workspace {
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
                name: "dev-team".into(),
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
                name: Some("dev-team".into()),
                updated_at: RcDate::from_millis(NOW),
                ..Default::default()
            }],
            permissions: perms
                .iter()
                .map(|(p, r)| Permission {
                    id: (*p).into(),
                    roles: r.iter().map(|s| (*s).to_owned()).collect(),
                    updated_at: None,
                })
                .collect(),
            settings: settings
                .iter()
                .map(|(k, v)| Setting {
                    id: (*k).into(),
                    value: v.clone(),
                    ..Default::default()
                })
                .collect(),
            next_since: RcDate::from_millis(NOW),
            ..Default::default()
        });
        ws
    }

    fn msg(id: &str, uid: &str) -> Message {
        Message {
            id: id.into(),
            rid: "r1".into(),
            msg: "hello".into(),
            ts: RcDate::from_millis(NOW - 60_000),
            u: UserRef {
                id: uid.into(),
                username: Some(uid.into()),
                name: None,
            },
            ..Default::default()
        }
    }

    fn now() -> DateTime<Utc> {
        RcDate::from_millis(NOW).unwrap().0
    }

    #[test]
    fn gating_follows_permissions() {
        use serde_json::json;
        let w = ws(
            &[
                ("delete-own-message", &["user"]),
                ("pin-message", &["owner"]),
            ],
            &[
                ("Message_AllowEditing", json!(true)),
                ("Message_AllowDeleting", json!(true)),
                ("Message_AllowPinning", json!(true)),
                ("Message_AllowStarring", json!(true)),
                ("Threads_enabled", json!(true)),
            ],
            RoomType::Channel,
        );
        let own = available(&w, &msg("a", "me"), now());
        assert!(own.contains(&Act::Edit));
        assert!(own.contains(&Act::Delete));
        assert!(own.contains(&Act::Star));
        assert!(!own.contains(&Act::Pin), "pin needs pin-message");
        assert!(!own.contains(&Act::Report), "no reporting own messages");
        assert!(!own.contains(&Act::MarkUnread));
        let other = available(&w, &msg("b", "bob"), now());
        assert!(!other.contains(&Act::Edit));
        assert!(!other.contains(&Act::Delete));
        assert!(other.contains(&Act::Report));
        assert!(other.contains(&Act::MarkUnread));
        assert!(other.contains(&Act::CopyLink));
        // Any message can be followed; following shows unfollow.
        assert!(other.contains(&Act::Follow));
        let mut parent = msg("c", "bob");
        parent.tcount = Some(2);
        parent.replies = Some(vec!["me".into()]);
        assert!(available(&w, &parent, now()).contains(&Act::Unfollow));
        parent.replies = Some(vec!["bob".into()]);
        assert!(available(&w, &parent, now()).contains(&Act::Follow));
        // System messages have no actions.
        let mut sys = msg("d", "bob");
        sys.t = Some("uj".into());
        assert!(available(&w, &sys, now()).is_empty());
    }

    #[test]
    fn gating_respects_settings() {
        use serde_json::json;
        let w = ws(
            &[
                ("delete-own-message", &["user"]),
                ("pin-message", &["user"]),
            ],
            &[
                ("Message_AllowEditing", json!(false)),
                ("Message_AllowDeleting", json!(false)),
                ("Message_AllowPinning", json!(true)),
                ("Message_AllowStarring", json!(false)),
            ],
            RoomType::Channel,
        );
        let mut own = msg("a", "me");
        own.pinned = Some(true);
        let acts = available(&w, &own, now());
        assert!(!acts.contains(&Act::Edit));
        assert!(!acts.contains(&Act::Delete));
        assert!(!acts.contains(&Act::Star) && !acts.contains(&Act::Unstar));
        assert!(acts.contains(&Act::Unpin));
    }

    #[test]
    fn permalinks() {
        let server = ServerUrl::parse("https://chat.example.com/sub").unwrap();
        let w = ws(&[], &[], RoomType::Channel);
        assert_eq!(
            permalink(&server, &w, &msg("abc", "me")),
            "https://chat.example.com/sub/channel/dev-team?msg=abc"
        );
        let w = ws(&[], &[], RoomType::Private);
        assert_eq!(
            permalink(&server, &w, &msg("abc", "me")),
            "https://chat.example.com/sub/group/dev-team?msg=abc"
        );
        let w = ws(&[], &[], RoomType::Direct);
        assert_eq!(
            permalink(&server, &w, &msg("abc", "me")),
            "https://chat.example.com/sub/direct/r1?msg=abc"
        );
        // Site_Url wins over the connection URL.
        let w = ws(
            &[],
            &[("Site_Url", serde_json::json!("https://chat.example.org/"))],
            RoomType::Channel,
        );
        assert_eq!(
            permalink(&server, &w, &msg("abc", "me")),
            "https://chat.example.org/channel/dev-team?msg=abc"
        );
        assert_eq!(
            channel_url(&server, &w, "my room").as_deref(),
            Some("https://chat.example.org/channel/my%20room")
        );
        // Omnichannel rooms: no mark unread, report or follow.
        let w = ws(
            &[],
            &[("Threads_enabled", serde_json::json!(true))],
            RoomType::Livechat,
        );
        let acts = available(&w, &msg("x", "bob"), now());
        assert!(!acts.contains(&Act::MarkUnread));
        assert!(!acts.contains(&Act::Report));
        assert!(!acts.contains(&Act::Follow));
    }

    #[test]
    fn menu_separates_destructive() {
        let entries = menu_entries(&[Act::CopyLink, Act::Report, Act::Delete], |a| a);
        assert_eq!(entries.len(), 4);
        assert!(matches!(entries[1], MenuEntry::Separator));
    }
}
