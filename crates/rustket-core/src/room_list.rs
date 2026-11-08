//! Sidebar sections computed from a [`Workspace`]: grouping, sorting, display
//! names, badges, previews and filtering. Pure; recompute when
//! `Dirty::ROOMS | Dirty::SETTINGS | Dirty::PRESENCE | Dirty::DRAFTS` changed.

use rocketchat::RcDate;
use rocketchat::models::{Message, RoomType, UserStatus};

use crate::config::{SidebarSort, UiPrefs};
use crate::workspace::{RoomEntry, Workspace};

/// Maximum preview length in characters.
pub const PREVIEW_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomListOptions {
    pub sort: SidebarSort,
    /// Split rooms into Teams, Discussions, Channels, Private groups, DMs;
    /// otherwise everything not in Unread/Favorites is one "Conversations" section.
    pub group_by_type: bool,
    pub unread_on_top: bool,
    /// With `unread_on_top`: a room that stays in the Unread section after it
    /// was read (the open room, until another one is opened).
    pub kept_unread: Option<String>,
    pub show_favorites: bool,
    /// Resolved: local override, else `UI_Use_Real_Name`.
    pub use_real_names: bool,
}

impl RoomListOptions {
    pub fn from_prefs(prefs: &UiPrefs, ws: &Workspace) -> Self {
        Self {
            sort: prefs.sidebar_sort,
            group_by_type: prefs.group_by_type,
            unread_on_top: prefs.unread_on_top,
            kept_unread: None,
            show_favorites: true,
            use_real_names: ws.use_real_names(prefs.use_real_names),
        }
    }
}

impl Default for RoomListOptions {
    fn default() -> Self {
        Self {
            sort: SidebarSort::Activity,
            group_by_type: true,
            unread_on_top: false,
            kept_unread: None,
            show_favorites: true,
            use_real_names: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SectionKind {
    Unread,
    Favorites,
    Teams,
    Discussions,
    Channels,
    PrivateGroups,
    DirectMessages,
    /// Everything else when not grouping by type.
    Conversations,
    Omnichannel,
}

impl SectionKind {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Unread => "sidebar.unread",
            Self::Favorites => "sidebar.favorites",
            Self::Teams => "sidebar.teams",
            Self::Discussions => "sidebar.discussions",
            Self::Channels => "sidebar.channels",
            Self::PrivateGroups => "sidebar.private_groups",
            Self::DirectMessages => "sidebar.direct_messages",
            Self::Conversations => "sidebar.conversations",
            Self::Omnichannel => "sidebar.omnichannel",
        }
    }
}

/// What kind of room a row is (icon selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomKind {
    Channel,
    Private,
    Direct,
    GroupDirect,
    Discussion,
    Team { private: bool },
    Omnichannel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Avatar {
    /// `/avatar/{username}`.
    User { username: String },
    /// `/avatar/room/{rid}`.
    Room { rid: String, etag: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BadgeVariant {
    /// Direct mention (red).
    Mention,
    /// `@all` / `@here` (orange).
    GroupMention,
    /// Unread thread replies (blue).
    Thread,
    #[default]
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Badge {
    /// Unread messages + unread threads; 0 = no counter.
    pub count: u64,
    pub variant: BadgeVariant,
    /// `alert` without a counter.
    pub dot: bool,
    /// Bold room name.
    pub highlighted: bool,
    pub mentions: u64,
    pub group_mentions: u64,
    pub threads: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Text,
    /// System message; see [`Preview::system_type`].
    System,
    Image,
    Video,
    Audio,
    File,
    /// Other attachment (quote, integration).
    Attachment,
    Encrypted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub kind: PreviewKind,
    /// i18n key: `preview.text`, `preview.image`, `preview.system.uj`, …
    pub i18n_key: String,
    /// The last message is the current user's ("You: ").
    pub own: bool,
    /// Author display name, shown in group rooms (not for own messages or 1:1 DMs).
    pub author: Option<String>,
    /// Flattened, truncated text (message text, file name or system message argument).
    pub text: Option<String>,
    pub system_type: Option<String>,
    pub ts: Option<RcDate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoomListItem {
    pub rid: String,
    pub room_type: RoomType,
    pub kind: RoomKind,
    /// Raw `name` (username for DMs).
    pub name: String,
    pub display_name: String,
    /// Other participants' usernames for DMs (one for 1:1).
    pub participants: Vec<String>,
    /// Presence of the partner of a 1:1 DM.
    pub dm_status: Option<UserStatus>,
    pub avatar: Avatar,
    pub badge: Badge,
    pub favorite: bool,
    /// `disableNotifications`.
    pub muted: bool,
    pub read_only: bool,
    pub archived: bool,
    pub encrypted: bool,
    /// Invitation pending (`status: INVITED`).
    pub invited: bool,
    pub has_draft: bool,
    pub last_activity: Option<RcDate>,
    pub preview: Option<Preview>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub items: Vec<RoomListItem>,
}

fn other_participants(ws: &Workspace, e: &RoomEntry) -> Vec<String> {
    let me = ws.username().unwrap_or_default();
    if let Some(list) = e.room.as_ref().and_then(|r| r.usernames.as_ref()) {
        let others: Vec<String> = list.iter().filter(|u| *u != me).cloned().collect();
        if !others.is_empty() {
            return others;
        }
    }
    // Subscription `name` of a DM: the partner's username, or "a, b" for group DMs.
    e.name()
        .split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty() && *u != me)
        .map(str::to_owned)
        .collect()
}

fn is_group_dm(e: &RoomEntry) -> bool {
    let r = e.room.as_ref();
    r.and_then(|r| r.uids.as_ref()).is_some_and(|u| u.len() > 2)
        || r.and_then(|r| r.usernames.as_ref())
            .is_some_and(|u| u.len() > 2)
        || (r.is_none() && e.name().contains(','))
}

/// Room display name following Rocket.Chat's `roomCoordinator.getRoomName`.
pub fn display_name(ws: &Workspace, e: &RoomEntry, use_real_names: bool) -> String {
    let name = e.name();
    let fname = e.fname();
    let s = match e.room_type() {
        RoomType::Direct => {
            if use_real_names {
                fname.unwrap_or(name)
            } else {
                name
            }
        }
        _ if e.is_discussion() => fname.unwrap_or(name),
        RoomType::Channel | RoomType::Private => {
            if ws.settings().allow_special_chars_in_room_names() {
                fname.unwrap_or(name)
            } else if name.is_empty() {
                fname.unwrap_or_default()
            } else {
                name
            }
        }
        _ => fname.unwrap_or(name),
    };
    s.to_owned()
}

/// Rocket.Chat's `getSubscriptionUnreadData`: mentions = `userMentions` +
/// `tunreadUser`, group mentions = `groupMentions`, total = `unread` + `tunread`;
/// variant mention > thread > group mention; the counter shows unless unread
/// status is hidden (mentions still show unless mention status is hidden too);
/// the name is bold only for `!hideUnreadStatus && (alert || unread)`.
pub fn badge(e: &RoomEntry) -> Badge {
    let Some(s) = e.sub.as_ref() else {
        return Badge::default();
    };
    let len = |v: &Option<Vec<String>>| v.as_ref().map_or(0, |v| v.len() as u64);
    let threads = len(&s.tunread);
    let mentions = s.user_mentions + len(&s.tunread_user);
    let group_mentions = s.group_mentions;
    let hide_unread = s.hide_unread_status == Some(true);
    let hide_mention = s.hide_mention_status == Some(true);
    let total = s.unread + threads;
    let variant = if mentions > 0 {
        BadgeVariant::Mention
    } else if threads > 0 {
        BadgeVariant::Thread
    } else if group_mentions > 0 {
        BadgeVariant::GroupMention
    } else {
        BadgeVariant::Normal
    };
    let show_count =
        total > 0 && (!hide_unread || (!hide_mention && (mentions > 0 || group_mentions > 0)));
    Badge {
        count: if show_count { total } else { 0 },
        variant,
        dot: !show_count && !hide_unread && s.alert,
        highlighted: !hide_unread && (s.alert || s.unread > 0),
        mentions,
        group_mentions,
        threads,
    }
}

/// `isUnreadRoom`: `!hideUnreadStatus && (alert || unread || tunread.length)`.
fn is_unread(e: &RoomEntry) -> bool {
    e.sub.as_ref().is_some_and(|s| {
        s.hide_unread_status != Some(true)
            && (s.alert || s.unread > 0 || s.tunread.as_ref().is_some_and(|t| !t.is_empty()))
    })
}

fn truncate(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PREVIEW_MAX_CHARS {
        flat
    } else {
        let mut t: String = flat.chars().take(PREVIEW_MAX_CHARS - 1).collect();
        t.push('…');
        t
    }
}

/// Structured last-message preview.
pub fn preview(
    ws: &Workspace,
    m: &Message,
    room_type: RoomType,
    group_dm: bool,
    use_real_names: bool,
) -> Preview {
    let own = ws.user_id().is_some_and(|id| id == m.u.id);
    let author_name = if use_real_names {
        m.u.name.clone().or(m.u.username.clone())
    } else {
        m.u.username.clone().or(m.u.name.clone())
    }
    .or(m.alias.clone());
    let author = (!own && (room_type != RoomType::Direct || group_dm))
        .then_some(author_name)
        .flatten();
    // Decrypted in place by the app: shown like any other message.
    let encrypted = m.is_encrypted() && !crate::e2e::shown::is_decrypted(m);
    let (kind, key, text): (PreviewKind, String, Option<String>) = if encrypted {
        (PreviewKind::Encrypted, "preview.encrypted".into(), None)
    } else if m.is_system() {
        let t = m.t.clone().unwrap_or_default();
        let arg = (!m.msg.is_empty()).then(|| truncate(&m.msg));
        (PreviewKind::System, format!("preview.system.{t}"), arg)
    } else if let Some(text) = Some(m.plain_text()).filter(|t| !t.trim().is_empty()) {
        (
            PreviewKind::Text,
            "preview.text".into(),
            Some(truncate(&text)),
        )
    } else if let Some(f) = m.file.as_ref().or(m.files.as_ref().and_then(|f| f.first())) {
        let mime = f.mime.as_deref().unwrap_or_default();
        let kind = if mime.starts_with("image/") {
            PreviewKind::Image
        } else if mime.starts_with("video/") {
            PreviewKind::Video
        } else if mime.starts_with("audio/") {
            PreviewKind::Audio
        } else {
            PreviewKind::File
        };
        let key = match kind {
            PreviewKind::Image => "preview.image",
            PreviewKind::Video => "preview.video",
            PreviewKind::Audio => "preview.audio",
            _ => "preview.file",
        };
        (kind, key.into(), Some(truncate(&f.name)))
    } else if let Some(a) = m.attachments.as_ref().and_then(|a| a.first()) {
        let kind = if a.image_url.is_some() {
            PreviewKind::Image
        } else if a.video_url.is_some() {
            PreviewKind::Video
        } else if a.audio_url.is_some() {
            PreviewKind::Audio
        } else {
            PreviewKind::Attachment
        };
        let key = match kind {
            PreviewKind::Image => "preview.image",
            PreviewKind::Video => "preview.video",
            PreviewKind::Audio => "preview.audio",
            _ => "preview.attachment",
        };
        let text = a
            .title
            .as_deref()
            .or(a.text.as_deref())
            .or(a.description.as_deref())
            .map(truncate);
        (kind, key.into(), text)
    } else {
        (PreviewKind::Text, "preview.text".into(), None)
    };
    Preview {
        kind,
        i18n_key: key,
        own,
        author,
        text,
        system_type: m.t.clone().filter(|_| kind == PreviewKind::System),
        ts: m.ts,
    }
}

fn kind_of(e: &RoomEntry) -> RoomKind {
    let t = e.room_type();
    match t {
        RoomType::Livechat | RoomType::Voip => RoomKind::Omnichannel,
        _ if e.is_team() => RoomKind::Team {
            private: t == RoomType::Private,
        },
        _ if e.is_discussion() => RoomKind::Discussion,
        RoomType::Direct if is_group_dm(e) => RoomKind::GroupDirect,
        RoomType::Direct => RoomKind::Direct,
        RoomType::Private => RoomKind::Private,
        _ => RoomKind::Channel,
    }
}

fn build_item(ws: &Workspace, e: &RoomEntry, opts: &RoomListOptions) -> RoomListItem {
    let rid = e.rid().to_owned();
    let kind = kind_of(e);
    let room_type = e.room_type();
    let participants = if room_type == RoomType::Direct {
        other_participants(ws, e)
    } else {
        Vec::new()
    };
    let dm_status = (kind == RoomKind::Direct)
        .then(|| participants.first())
        .flatten()
        .map(|u| ws.presence().status_by_username(u));
    let avatar = match (kind, participants.first()) {
        (RoomKind::Direct, Some(u)) => Avatar::User {
            username: u.clone(),
        },
        _ => Avatar::Room {
            rid: rid.clone(),
            etag: e.room.as_ref().and_then(|r| r.avatar_etag.clone()),
        },
    };
    let group_dm = kind == RoomKind::GroupDirect;
    let sub = e.sub.as_ref();
    RoomListItem {
        display_name: display_name(ws, e, opts.use_real_names),
        name: e.name().to_owned(),
        participants,
        dm_status,
        avatar,
        badge: badge(e),
        favorite: sub.is_some_and(|s| s.f),
        muted: sub.and_then(|s| s.disable_notifications) == Some(true),
        read_only: e.is_read_only(),
        archived: e.is_archived(),
        encrypted: e.is_encrypted(),
        invited: sub.and_then(|s| s.status.as_deref()) == Some("INVITED"),
        has_draft: ws.draft(&rid, None).is_some(),
        last_activity: e.last_activity(),
        preview: e
            .room
            .as_ref()
            .and_then(|r| r.last_message.as_deref())
            .map(|m| preview(ws, m, room_type, group_dm, opts.use_real_names)),
        kind,
        room_type,
        rid,
    }
}

fn matches_filter(ws: &Workspace, e: &RoomEntry, item: &RoomListItem, q: &str) -> bool {
    let hit = |s: &str| s.to_lowercase().contains(q);
    hit(&item.display_name)
        || hit(e.name())
        || e.fname().is_some_and(hit)
        || item.participants.iter().any(|u| hit(u))
        || item
            .participants
            .iter()
            .filter_map(|u| ws.presence().get_by_username(u))
            .filter_map(|p| p.name.as_deref())
            .any(hit)
}

fn section_for(e: &RoomEntry, item: &RoomListItem, opts: &RoomListOptions) -> SectionKind {
    if opts.unread_on_top
        && (is_unread(e) || opts.kept_unread.as_deref() == Some(item.rid.as_str()))
    {
        return SectionKind::Unread;
    }
    if opts.show_favorites && item.favorite {
        return SectionKind::Favorites;
    }
    if item.kind == RoomKind::Omnichannel {
        return SectionKind::Omnichannel;
    }
    if !opts.group_by_type {
        return SectionKind::Conversations;
    }
    match item.kind {
        RoomKind::Team { .. } => SectionKind::Teams,
        RoomKind::Discussion => SectionKind::Discussions,
        RoomKind::Direct | RoomKind::GroupDirect => SectionKind::DirectMessages,
        RoomKind::Private => SectionKind::PrivateGroups,
        RoomKind::Channel => SectionKind::Channels,
        RoomKind::Omnichannel => SectionKind::Omnichannel,
    }
}

fn sort_items(items: &mut [RoomListItem], sort: SidebarSort) {
    match sort {
        SidebarSort::Activity => items.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
                .then_with(|| a.rid.cmp(&b.rid))
        }),
        SidebarSort::Alphabetical => items.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| a.rid.cmp(&b.rid))
        }),
    }
}

/// Sidebar sections. Rooms without a subscription, with `open == false` or
/// archived are excluded (Rocket.Chat's `getRoomCategory`); `filter` is a case-insensitive substring over names, `fname`s and DM
/// participants. Empty sections are omitted.
pub fn compute(ws: &Workspace, opts: &RoomListOptions, filter: &str) -> Vec<Section> {
    let q = filter.trim().to_lowercase();
    let mut buckets: std::collections::BTreeMap<SectionKind, Vec<RoomListItem>> =
        Default::default();
    for e in ws.rooms().values() {
        let Some(sub) = e.sub.as_ref() else {
            continue;
        };
        if !sub.open || e.is_archived() {
            continue;
        }
        let item = build_item(ws, e, opts);
        if !q.is_empty() && !matches_filter(ws, e, &item, &q) {
            continue;
        }
        buckets
            .entry(section_for(e, &item, opts))
            .or_default()
            .push(item);
    }
    buckets
        .into_iter()
        .map(|(kind, mut items)| {
            sort_items(&mut items, opts.sort);
            Section { kind, items }
        })
        .collect()
}

/// The list item of one room, whatever its sidebar visibility (hidden and
/// archived rooms too; e.g. for search results).
pub fn item(ws: &Workspace, e: &RoomEntry, opts: &RoomListOptions) -> RoomListItem {
    build_item(ws, e, opts)
}

/// Whether the room counts as unread (for window badges / "next unread room").
pub fn room_is_unread(e: &RoomEntry) -> bool {
    is_unread(e)
}

/// Totals for a window/tray badge: (unread rooms, mentions) over open,
/// non-archived rooms; muted rooms (`disableNotifications`) don't count as
/// unread rooms, hidden mention status hides mentions.
pub fn unread_totals(ws: &Workspace) -> (u64, u64) {
    let mut rooms = 0;
    let mut mentions = 0;
    for e in ws.rooms().values() {
        let Some(s) = e.sub.as_ref().filter(|s| s.open && !e.is_archived()) else {
            continue;
        };
        let b = badge(e);
        if s.disable_notifications != Some(true) && is_unread(e) {
            rooms += 1;
        }
        mentions += if s.hide_mention_status == Some(true) {
            0
        } else {
            b.mentions
        };
    }
    (rooms, mentions)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{fixture_workspace, me, msg, room, sub, ts};
    use crate::workspace::Change;
    use rocketchat::models::{FileInfo, Room, Subscription};

    fn ws_with(entries: Vec<(Subscription, Option<Room>)>) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        for (s, r) in entries {
            ws.apply_subscription(Change::Inserted, s);
            if let Some(r) = r {
                ws.apply_room(Change::Inserted, r);
            }
        }
        ws
    }

    fn kinds(sections: &[Section]) -> Vec<SectionKind> {
        sections.iter().map(|s| s.kind).collect()
    }

    fn names(s: &Section) -> Vec<&str> {
        s.items.iter().map(|i| i.display_name.as_str()).collect()
    }

    fn with_lm(mut r: Room, ms: i64) -> Room {
        r.lm = Some(ts(ms));
        r
    }

    #[test]
    fn grouping_by_type() {
        let mut team = room("t", "team", RoomType::Private);
        team.team_main = Some(true);
        let mut disc = room("di", "disc", RoomType::Private);
        disc.prid = Some("c".into());
        disc.fname = Some("A discussion".into());
        let mut fav = sub("f", "fav", RoomType::Channel);
        fav.f = true;
        let mut hidden = sub("h", "hidden", RoomType::Channel);
        hidden.open = false;
        let ws = ws_with(vec![
            (
                sub("c", "chan", RoomType::Channel),
                Some(room("c", "chan", RoomType::Channel)),
            ),
            (sub("p", "priv", RoomType::Private), None),
            (sub("d", "bob", RoomType::Direct), None),
            (sub("t", "team", RoomType::Private), Some(team)),
            (sub("di", "disc", RoomType::Private), Some(disc)),
            (fav, None),
            (hidden, None),
            (sub("l", "visitor", RoomType::Livechat), None),
        ]);
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(
            kinds(&s),
            [
                SectionKind::Favorites,
                SectionKind::Teams,
                SectionKind::Discussions,
                SectionKind::Channels,
                SectionKind::PrivateGroups,
                SectionKind::DirectMessages,
                SectionKind::Omnichannel
            ]
        );
        assert_eq!(names(&s[2]), ["A discussion"]);
        assert_eq!(s[1].items[0].kind, RoomKind::Team { private: true });
        let flat = compute(
            &ws,
            &RoomListOptions {
                group_by_type: false,
                ..Default::default()
            },
            "",
        );
        assert_eq!(
            kinds(&flat),
            [
                SectionKind::Favorites,
                SectionKind::Conversations,
                SectionKind::Omnichannel
            ]
        );
        assert_eq!(flat[1].items.len(), 5);
    }

    #[test]
    fn sorting() {
        let ws = ws_with(vec![
            (
                sub("a", "alpha", RoomType::Channel),
                Some(with_lm(room("a", "alpha", RoomType::Channel), 1000)),
            ),
            (
                sub("b", "Beta", RoomType::Channel),
                Some(with_lm(room("b", "Beta", RoomType::Channel), 3000)),
            ),
            (
                sub("c", "charlie", RoomType::Channel),
                Some(with_lm(room("c", "charlie", RoomType::Channel), 2000)),
            ),
        ]);
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(names(&s[0]), ["Beta", "charlie", "alpha"]);
        let s = compute(
            &ws,
            &RoomListOptions {
                sort: SidebarSort::Alphabetical,
                ..Default::default()
            },
            "",
        );
        assert_eq!(names(&s[0]), ["alpha", "Beta", "charlie"]);
    }

    #[test]
    fn unread_on_top_and_badges() {
        let mut u = sub("u", "unread", RoomType::Channel);
        u.unread = 3;
        u.alert = true;
        u.user_mentions = 1;
        let mut hidden_status = sub("h", "quiet", RoomType::Channel);
        hidden_status.unread = 5;
        hidden_status.hide_unread_status = Some(true);
        let mut alert_only = sub("a", "alert", RoomType::Channel);
        alert_only.alert = true;
        let mut threads = sub("t", "threads", RoomType::Channel);
        threads.tunread = Some(vec!["x".into(), "y".into()]);
        let mut muted = sub("m", "muted", RoomType::Channel);
        muted.disable_notifications = Some(true);
        let ws = ws_with(vec![
            (u, None),
            (hidden_status, None),
            (alert_only, None),
            (threads, None),
            (muted, None),
        ]);
        let s = compute(
            &ws,
            &RoomListOptions {
                unread_on_top: true,
                sort: SidebarSort::Alphabetical,
                ..Default::default()
            },
            "",
        );
        assert_eq!(kinds(&s), [SectionKind::Unread, SectionKind::Channels]);
        assert_eq!(names(&s[0]), ["alert", "threads", "unread"]);
        let get = |n: &str| {
            s.iter()
                .flat_map(|s| &s.items)
                .find(|i| i.name == n)
                .unwrap()
                .clone()
        };
        let b = get("unread").badge;
        assert_eq!(
            (b.count, b.variant, b.dot),
            (3, BadgeVariant::Mention, false)
        );
        let b = get("quiet").badge;
        assert_eq!((b.count, b.dot, b.highlighted), (0, false, false));
        let b = get("alert").badge;
        assert_eq!((b.count, b.dot), (0, true));
        let b = get("threads").badge;
        assert_eq!((b.count, b.variant), (2, BadgeVariant::Thread));
        assert!(get("muted").muted);
        assert_eq!(unread_totals(&ws), (3, 1));
    }

    #[test]
    fn hidden_unread_still_shows_mentions() {
        let mut s = sub("h", "quiet", RoomType::Channel);
        s.unread = 2;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
        let e = RoomEntry {
            room: None,
            sub: Some(s.clone()),
        };
        assert_eq!(badge(&e).count, 2);
        s.hide_mention_status = Some(true);
        let e = RoomEntry {
            room: None,
            sub: Some(s),
        };
        assert_eq!(badge(&e).count, 0);
        // RC's variant ignores hideMentionStatus.
        assert_eq!(badge(&e).variant, BadgeVariant::Mention);
    }

    #[test]
    fn display_names_and_dms() {
        let mut dm = sub("d", "bob", RoomType::Direct);
        dm.fname = Some("Bob Builder".into());
        let mut dmr = room("d", "", RoomType::Direct);
        dmr.usernames = Some(vec!["me".into(), "bob".into()]);
        dmr.uids = Some(vec!["me".into(), "bobid".into()]);
        let mut g = sub("g", "bob, carol", RoomType::Direct);
        g.fname = Some("Bob Builder, Carol".into());
        let mut gr = room("g", "", RoomType::Direct);
        gr.usernames = Some(vec!["me".into(), "bob".into(), "carol".into()]);
        gr.uids = Some(vec!["me".into(), "b".into(), "c".into()]);
        let mut ch = sub("c", "dev-team", RoomType::Channel);
        ch.fname = Some("Dev Team ✨".into());
        let mut ws = ws_with(vec![(dm, Some(dmr)), (g, Some(gr)), (ch, None)]);
        ws.apply_user_status("bobid", Some("bob"), UserStatus::Away, None);
        let opts = RoomListOptions::default();
        let s = compute(&ws, &opts, "");
        let all: Vec<&RoomListItem> = s.iter().flat_map(|s| &s.items).collect();
        let get = |rid: &str| all.iter().find(|i| i.rid == rid).unwrap();
        assert_eq!(get("d").display_name, "bob");
        assert_eq!(get("d").kind, RoomKind::Direct);
        assert_eq!(get("d").dm_status, Some(UserStatus::Away));
        assert_eq!(
            get("d").avatar,
            Avatar::User {
                username: "bob".into()
            }
        );
        assert_eq!(get("g").kind, RoomKind::GroupDirect);
        assert_eq!(get("g").participants, ["bob", "carol"]);
        assert_eq!(get("c").display_name, "dev-team");
        let real = RoomListOptions {
            use_real_names: true,
            ..Default::default()
        };
        let s = compute(&ws, &real, "");
        let all: Vec<&RoomListItem> = s.iter().flat_map(|s| &s.items).collect();
        let get = |rid: &str| all.iter().find(|i| i.rid == rid).unwrap();
        assert_eq!(get("d").display_name, "Bob Builder");
        assert_eq!(get("g").display_name, "Bob Builder, Carol");
        ws.apply_setting(
            Change::Updated,
            rocketchat::models::Setting {
                id: "UI_Allow_room_names_with_special_chars".into(),
                value: serde_json::json!(true),
                ..Default::default()
            },
        );
        let s = compute(&ws, &opts, "dev");
        assert_eq!(names(&s[0]), ["Dev Team ✨"]);
    }

    #[test]
    fn filtering() {
        let mut dm = sub("d", "bob", RoomType::Direct);
        dm.fname = Some("Robert".into());
        let ws = ws_with(vec![
            (dm, None),
            (sub("c", "General", RoomType::Channel), None),
            (sub("x", "random", RoomType::Channel), None),
        ]);
        let all = |q: &str| -> Vec<String> {
            compute(&ws, &RoomListOptions::default(), q)
                .into_iter()
                .flat_map(|s| s.items)
                .map(|i| i.rid)
                .collect()
        };
        assert_eq!(all("GEN"), ["c"]);
        assert_eq!(all("rob"), ["d"], "matches fname");
        let mut o = all("o");
        o.sort();
        assert_eq!(o, ["d", "x"]);
        assert!(all("zzz").is_empty());
        assert_eq!(all("  ").len(), 3);
    }

    #[test]
    fn previews() {
        let ws = {
            let mut ws = Workspace::new();
            ws.apply_me(me("me", "me"));
            ws
        };
        let m = msg("m", "r", 1, "me");
        let p = preview(&ws, &m, RoomType::Channel, false, false);
        assert!(p.own);
        assert_eq!(p.author, None);
        assert_eq!(p.kind, PreviewKind::Text);
        assert_eq!(p.text.as_deref(), Some("text m"));

        let mut m = msg("m", "r", 1, "bob");
        m.msg = "line one\n\n  line   two".into();
        let p = preview(&ws, &m, RoomType::Channel, false, false);
        assert_eq!(p.author.as_deref(), Some("bob"));
        assert_eq!(p.text.as_deref(), Some("line one line two"));
        let p = preview(&ws, &m, RoomType::Channel, false, true);
        assert_eq!(p.author.as_deref(), Some("Name bob"));
        let p = preview(&ws, &m, RoomType::Direct, false, false);
        assert_eq!(p.author, None, "no author in 1:1 DMs");

        let mut f = msg("f", "r", 1, "bob");
        f.msg = String::new();
        f.file = Some(FileInfo {
            id: "1".into(),
            name: "cat.png".into(),
            mime: Some("image/png".into()),
            ..Default::default()
        });
        let p = preview(&ws, &f, RoomType::Channel, false, false);
        assert_eq!(
            (p.kind, p.i18n_key.as_str()),
            (PreviewKind::Image, "preview.image")
        );
        assert_eq!(p.text.as_deref(), Some("cat.png"));

        let mut s = msg("s", "r", 1, "bob");
        s.t = Some("uj".into());
        let p = preview(&ws, &s, RoomType::Channel, false, false);
        assert_eq!(p.kind, PreviewKind::System);
        assert_eq!(p.i18n_key, "preview.system.uj");
        assert_eq!(p.system_type.as_deref(), Some("uj"));

        let mut e = msg("e", "r", 1, "bob");
        e.t = Some("e2e".into());
        e.msg = "ciphertext".into();
        let p = preview(&ws, &e, RoomType::Direct, false, false);
        assert_eq!(p.kind, PreviewKind::Encrypted);
        assert_eq!(p.text, None);
        // Decrypted in place by the app: the plaintext preview.
        e.msg = "hello there".into();
        crate::e2e::shown::set_marker(&mut e, crate::e2e::shown::DECRYPTED);
        let p = preview(&ws, &e, RoomType::Direct, false, false);
        assert_eq!(p.kind, PreviewKind::Text);
        assert_eq!(p.text.as_deref(), Some("hello there"));

        let mut long = msg("l", "r", 1, "bob");
        long.msg = "x".repeat(500);
        let p = preview(&ws, &long, RoomType::Channel, false, false);
        assert_eq!(p.text.unwrap().chars().count(), PREVIEW_MAX_CHARS);
    }

    fn entry(s: Subscription) -> RoomEntry {
        RoomEntry {
            room: None,
            sub: Some(s),
        }
    }

    #[test]
    fn badge_variant_priority_follows_rocket_chat() {
        // (mentions && danger) || (threads && primary) || (groupMentions && warning)
        let mut s = sub("r", "room", RoomType::Channel);
        s.unread = 1;
        s.group_mentions = 1;
        s.tunread = Some(vec!["t".into()]);
        assert_eq!(badge(&entry(s.clone())).variant, BadgeVariant::Thread);
        s.tunread = None;
        assert_eq!(badge(&entry(s.clone())).variant, BadgeVariant::GroupMention);
        s.tunread_user = Some(vec!["t".into()]);
        let b = badge(&entry(s));
        assert_eq!((b.variant, b.mentions), (BadgeVariant::Mention, 1));
    }

    #[test]
    fn thread_group_mentions_are_not_group_mentions() {
        // A thread @all sets tunread + tunreadGroup, never groupMentions.
        let mut s = sub("r", "room", RoomType::Channel);
        s.alert = true;
        s.tunread = Some(vec!["t".into()]);
        s.tunread_group = Some(vec!["t".into()]);
        let b = badge(&entry(s.clone()));
        assert_eq!(
            (b.group_mentions, b.variant, b.count),
            (0, BadgeVariant::Thread, 1)
        );
        s.hide_unread_status = Some(true);
        assert_eq!(badge(&entry(s)).count, 0);
    }

    #[test]
    fn highlight_only_for_alert_or_unread() {
        let mut s = sub("r", "room", RoomType::Channel);
        s.tunread = Some(vec!["t".into()]);
        assert!(!badge(&entry(s.clone())).highlighted, "threads only");
        s.unread = 1;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
        let b = badge(&entry(s));
        assert!(!b.highlighted);
        assert_eq!(b.count, 2, "mentions still show the counter");
    }

    #[test]
    fn hidden_unread_room_with_mention_is_not_in_unread_section() {
        let mut s = sub("h", "quiet", RoomType::Channel);
        s.unread = 2;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
        let ws = ws_with(vec![(s, None)]);
        assert!(!room_is_unread(ws.room("h").unwrap()));
        let secs = compute(
            &ws,
            &RoomListOptions {
                unread_on_top: true,
                ..Default::default()
            },
            "",
        );
        assert_eq!(kinds(&secs), [SectionKind::Channels]);
    }

    #[test]
    fn a_kept_room_stays_in_the_unread_section_once_read() {
        let read = sub("k", "kept", RoomType::Channel);
        let ws = ws_with(vec![(read, None)]);
        let mut opts = RoomListOptions {
            unread_on_top: true,
            kept_unread: Some("k".into()),
            ..Default::default()
        };
        assert_eq!(kinds(&compute(&ws, &opts, "")), [SectionKind::Unread]);
        opts.unread_on_top = false;
        assert_eq!(kinds(&compute(&ws, &opts, "")), [SectionKind::Channels]);
    }

    #[test]
    fn unread_wins_over_favorites() {
        let mut s = sub("f", "fav", RoomType::Channel);
        s.f = true;
        s.unread = 1;
        let ws = ws_with(vec![(s, None)]);
        let opts = RoomListOptions {
            unread_on_top: true,
            ..Default::default()
        };
        assert_eq!(kinds(&compute(&ws, &opts, "")), [SectionKind::Unread]);
        let opts = RoomListOptions::default();
        assert_eq!(kinds(&compute(&ws, &opts, "")), [SectionKind::Favorites]);
    }

    #[test]
    fn archived_rooms_are_not_listed() {
        // Archived, then opened again (rooms.open sets open: true).
        let mut s = sub("a", "old", RoomType::Private);
        s.archived = Some(true);
        s.unread = 3;
        let mut r = room("b", "gone", RoomType::Channel);
        r.archived = Some(true);
        let ws = ws_with(vec![
            (s, None),
            (sub("b", "gone", RoomType::Channel), Some(r)),
        ]);
        assert!(compute(&ws, &RoomListOptions::default(), "").is_empty());
        assert_eq!(unread_totals(&ws), (0, 0));
    }

    #[test]
    fn item_of_a_hidden_archived_room() {
        let mut s = sub("a", "old", RoomType::Private);
        s.archived = Some(true);
        s.open = false;
        let ws = ws_with(vec![(s, None)]);
        assert!(compute(&ws, &RoomListOptions::default(), "").is_empty());
        let e = ws.rooms().values().next().unwrap();
        let i = item(&ws, e, &RoomListOptions::default());
        assert_eq!((i.rid.as_str(), i.kind), ("a", RoomKind::Private));
        assert!(i.archived);
    }

    #[test]
    fn activity_key_follows_rocket_chat() {
        // max(sub.lr, room.lm || sub.ts || room.ts)
        let mut a = sub("a", "a", RoomType::Channel);
        a.lr = Some(ts(31_000));
        let mut ar = with_lm(room("a", "a", RoomType::Channel), 28_000);
        ar.updated_at = Some(ts(32_000));
        let br = with_lm(room("b", "b", RoomType::Channel), 29_000);
        // A room without messages: sub.ts, not room._updatedAt (a topic change).
        let mut c = sub("c", "c", RoomType::Channel);
        c.ts = Some(ts(10_000));
        let mut cr = room("c", "c", RoomType::Channel);
        cr.updated_at = Some(ts(99_000));
        cr.ts = Some(ts(9_000));
        // lm wins over lastMessage.ts (deleting the last message keeps lm).
        let mut dr = with_lm(room("d", "d", RoomType::Channel), 20_000);
        dr.last_message = Some(Box::new(msg("m", "d", 5_000, "u")));
        let ws = ws_with(vec![
            (a, Some(ar)),
            (sub("b", "b", RoomType::Channel), Some(br)),
            (c, Some(cr)),
            (sub("d", "d", RoomType::Channel), Some(dr)),
        ]);
        assert_eq!(ws.room("a").unwrap().last_activity(), Some(ts(31_000)));
        assert_eq!(ws.room("c").unwrap().last_activity(), Some(ts(10_000)));
        assert_eq!(ws.room("d").unwrap().last_activity(), Some(ts(20_000)));
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(names(&s[0]), ["a", "b", "d", "c"]);
        // Without a room document: lr / sub.ts.
        let mut e = sub("e", "e", RoomType::Channel);
        e.ts = Some(ts(7));
        assert_eq!(entry(e.clone()).last_activity(), Some(ts(7)));
        e.lr = Some(ts(5));
        assert_eq!(entry(e).last_activity(), Some(ts(7)));
    }

    #[test]
    fn unread_totals_rules() {
        let mut plain = sub("p", "p", RoomType::Channel);
        plain.unread = 2;
        plain.user_mentions = 1;
        let mut muted = sub("m", "m", RoomType::Channel);
        muted.unread = 5;
        muted.user_mentions = 2;
        muted.disable_notifications = Some(true);
        let mut hidden_mentions = sub("h", "h", RoomType::Channel);
        hidden_mentions.unread = 1;
        hidden_mentions.user_mentions = 4;
        hidden_mentions.hide_mention_status = Some(true);
        let mut closed = sub("c", "c", RoomType::Channel);
        closed.unread = 9;
        closed.user_mentions = 9;
        closed.open = false;
        let mut group = sub("g", "g", RoomType::Channel);
        group.alert = true;
        group.group_mentions = 3;
        let ws = ws_with(vec![
            (plain, None),
            (muted, None),
            (hidden_mentions, None),
            (closed, None),
            (group, None),
        ]);
        // Rooms: p, h, g (muted and closed don't count); mentions: p 1 + m 2.
        assert_eq!(unread_totals(&ws), (3, 3));
    }

    #[test]
    fn item_flags_and_group_dm_section() {
        let mut inv = sub("i", "invite", RoomType::Private);
        inv.status = Some("INVITED".into());
        let mut g = sub("g", "bob, carol", RoomType::Direct);
        g.fname = Some("Bob, Carol".into());
        let mut ws = ws_with(vec![(inv, None), (g, None)]);
        ws.set_draft("i", None, "draft text");
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(
            kinds(&s),
            [SectionKind::PrivateGroups, SectionKind::DirectMessages]
        );
        let i = &s[0].items[0];
        assert!(i.invited && i.has_draft);
        assert_eq!(s[1].items[0].kind, RoomKind::GroupDirect);
        assert!(!s[1].items[0].has_draft);
    }

    #[test]
    fn attachment_previews() {
        use rocketchat::models::MessageAttachment;
        let ws = {
            let mut ws = Workspace::new();
            ws.apply_me(me("me", "me"));
            ws
        };
        let att = |f: fn(&mut MessageAttachment)| {
            let mut m = msg("a", "r", 1, "bob");
            m.msg = String::new();
            let mut a = MessageAttachment {
                title: Some("clip".into()),
                ..Default::default()
            };
            f(&mut a);
            m.attachments = Some(vec![a]);
            preview(&ws, &m, RoomType::Direct, true, false)
        };
        let p = att(|a| a.video_url = Some("/v.mp4".into()));
        assert_eq!(
            (p.kind, p.i18n_key.as_str()),
            (PreviewKind::Video, "preview.video")
        );
        assert_eq!(
            p.author.as_deref(),
            Some("bob"),
            "authors shown in group DMs"
        );
        let p = att(|a| a.audio_url = Some("/a.ogg".into()));
        assert_eq!(p.kind, PreviewKind::Audio);
        let p = att(|a| a.image_url = Some("/i.png".into()));
        assert_eq!(p.kind, PreviewKind::Image);
        let p = att(|_| {});
        assert_eq!(
            (p.kind, p.text.as_deref()),
            (PreviewKind::Attachment, Some("clip"))
        );
        let mut f = msg("f", "r", 1, "bob");
        f.msg = String::new();
        f.file = Some(FileInfo {
            id: "1".into(),
            name: "movie.webm".into(),
            mime: Some("video/webm".into()),
            ..Default::default()
        });
        assert_eq!(
            preview(&ws, &f, RoomType::Channel, false, false).kind,
            PreviewKind::Video
        );
    }

    #[test]
    fn fixture_room_list() {
        let ws = fixture_workspace();
        let s = compute(&ws, &RoomListOptions::default(), "");
        let all: Vec<&RoomListItem> = s.iter().flat_map(|s| &s.items).collect();
        assert!(!all.is_empty());
        // Hidden DM with admin (open: false) is excluded.
        assert!(all.iter().all(|i| i.rid != "6aef4af4f7fd708252117519"));
        let general = all.iter().find(|i| i.rid == "GENERAL").unwrap();
        assert_eq!(general.badge.count, 1);
        assert_eq!(general.badge.variant, BadgeVariant::Mention);
        assert!(all.iter().any(|i| i.preview.is_some()));
    }
}
