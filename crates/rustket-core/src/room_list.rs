//! Sidebar sections computed from a [`Workspace`]: grouping, sorting, display
//! names, badges, previews and filtering. Pure; recompute when
//! `Dirty::ROOMS | Dirty::SETTINGS | Dirty::PRESENCE | Dirty::DRAFTS` changed
//! (and `Dirty::THREAD_INDEX` while the Threads section is on).

use rocketchat::RcDate;
use rocketchat::models::{Message, RoomType, UserStatus};

use crate::config::{SidebarSort, UiPrefs};
use crate::sidebar_layout::{GroupType, Layout, SectionId, ThreadsMode};
use crate::workspace::{RoomEntry, Workspace};

/// Maximum preview length in characters.
pub const PREVIEW_MAX_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomListOptions {
    /// Section order, groups, per-section sorts (the global sort is
    /// `layout.default_sort`).
    pub layout: Layout,
    /// Split rooms into Teams, Discussions, Channels, Private groups, DMs;
    /// otherwise everything not in Unread/Favorites is one "Conversations" section.
    pub group_by_type: bool,
    pub unread_on_top: bool,
    /// With `unread_on_top`: a room that stays in the Unread section after it
    /// was read (the open room, until another one is opened).
    pub kept_unread: Option<String>,
    /// The open room kept only in the unread block of sections that list
    /// unread rooms first: it had a counter or dot without counting as unread
    /// (e.g. a mention in a room with hidden unread status), so it does not
    /// go to the Unread section.
    pub kept_unread_first: Option<String>,
    pub show_favorites: bool,
    /// Resolved: local override, else `UI_Use_Real_Name`.
    pub use_real_names: bool,
    /// `(rid, tmid)` of the open thread, kept in the Threads section (and in
    /// its unread block) after it was read, until another thread or room opens.
    pub kept_thread: Option<(String, String)>,
}

impl RoomListOptions {
    pub fn from_prefs(prefs: &UiPrefs, ws: &Workspace) -> Self {
        Self {
            layout: Layout::from_prefs(prefs),
            group_by_type: prefs.group_by_type,
            unread_on_top: prefs.unread_on_top,
            kept_unread: None,
            kept_unread_first: None,
            show_favorites: true,
            use_real_names: ws.use_real_names(prefs.use_real_names),
            kept_thread: None,
        }
    }
}

impl Default for RoomListOptions {
    fn default() -> Self {
        Self {
            layout: Layout::default(),
            group_by_type: true,
            unread_on_top: false,
            kept_unread: None,
            kept_unread_first: None,
            show_favorites: true,
            use_real_names: false,
            kept_thread: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SectionKind {
    Unread,
    Favorites,
    /// Unread, kept or followed threads (see `ThreadsMode`).
    Threads,
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
            Self::Threads => "sidebar.threads",
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

/// What a thread row shows as its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadTitle {
    /// Rocket.Chat's `makeThreadName`: the first line of the text, else the
    /// first attachment's title or text, else the file name.
    Text(String),
    /// An end-to-end encrypted parent not decrypted (yet).
    Encrypted,
    /// A deleted parent (`t: "rm"`), listed while unread or kept.
    Removed,
    /// The parent is not loaded yet.
    Loading,
    /// The parent is not available (not found or hidden).
    Unavailable,
}

impl ThreadTitle {
    /// The i18n key of a title without text.
    pub fn i18n_key(&self) -> Option<&'static str> {
        match self {
            Self::Text(_) => None,
            Self::Encrypted => Some("preview.encrypted"),
            Self::Removed => Some("sidebar.thread_removed"),
            Self::Loading => Some("sidebar.thread_loading"),
            Self::Unavailable => Some("sidebar.thread_unavailable"),
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// A row of the Threads section.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadListItem {
    pub rid: String,
    pub tmid: String,
    /// Display name of the thread's room.
    pub room_name: String,
    pub room_kind: RoomKind,
    pub title: ThreadTitle,
    /// Parent author's username.
    pub author: Option<String>,
    /// The parent author, else the room.
    pub avatar: Avatar,
    /// Last reply, else the parent's own time.
    pub tlm: Option<RcDate>,
    pub tcount: u64,
    /// Unread replies: mention > group mention > plain ([`BadgeVariant::Thread`]).
    pub unread: Option<BadgeVariant>,
    /// Kept in the sidebar by the user.
    pub kept: bool,
    /// The user follows the thread (in the parent's `replies`).
    pub following: bool,
}

impl ThreadListItem {
    /// The parent is known (the row is not a placeholder).
    pub fn is_loaded(&self) -> bool {
        !matches!(self.title, ThreadTitle::Loading | ThreadTitle::Unavailable)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub id: SectionId,
    /// Group name; `None` for built-ins (title from the kind's i18n key).
    pub title: Option<String>,
    pub items: Vec<RoomListItem>,
    /// Thread rows (the Threads section only).
    pub threads: Vec<ThreadListItem>,
}

impl Section {
    pub fn kind(&self) -> Option<SectionKind> {
        self.id.kind()
    }

    pub fn is(&self, k: SectionKind) -> bool {
        self.id == SectionId::Builtin(k)
    }

    /// Number of rows (rooms and threads).
    pub fn len(&self) -> usize {
        self.items.len() + self.threads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
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

/// The section a room is listed in: Unread, Favorites, Omnichannel, the group
/// that claims its type, Conversations (not grouping by type), its type's
/// section. Display order is `opts.layout.order`, independent of this.
fn section_for(e: &RoomEntry, item: &RoomListItem, opts: &RoomListOptions) -> SectionId {
    if opts.unread_on_top
        && (is_unread(e) || opts.kept_unread.as_deref() == Some(item.rid.as_str()))
    {
        return SectionKind::Unread.into();
    }
    if opts.show_favorites && item.favorite {
        return SectionKind::Favorites.into();
    }
    let Some(ty) = GroupType::of_room(item.kind) else {
        return SectionKind::Omnichannel.into();
    };
    if let Some(g) = opts.layout.claimed_by(ty) {
        return SectionId::group(g);
    }
    if !opts.group_by_type {
        return SectionKind::Conversations.into();
    }
    ty.section().into()
}

/// Ascending order with `None` after every `Some`.
fn cmp_none_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Sorts by `sort`; with `unread_first` rows with a counter or dot (and the
/// kept open room) come first.
fn sort_items(
    items: &mut [RoomListItem],
    sort: SidebarSort,
    unread_first: bool,
    opts: &RoomListOptions,
) {
    sort_by_kind(items, sort);
    if unread_first {
        let kept = |rid: &str| {
            opts.kept_unread.as_deref() == Some(rid)
                || opts.kept_unread_first.as_deref() == Some(rid)
        };
        let read = |i: &RoomListItem| !(i.badge.count > 0 || i.badge.dot || kept(&i.rid));
        // Stable: keeps the order of `sort` within both blocks.
        items.sort_by_key(read);
    }
}

fn sort_by_kind(items: &mut [RoomListItem], sort: SidebarSort) {
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
        SidebarSort::ActivityAsc => items.sort_by(|a, b| {
            cmp_none_last(&a.last_activity, &b.last_activity)
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

/// Sidebar sections in `opts.layout.order`, then Omnichannel (rendered
/// separately). Rooms without a subscription, with `open == false` or
/// archived are excluded (Rocket.Chat's `getRoomCategory`); `filter` is a
/// case-insensitive substring over names, `fname`s and DM participants. Each
/// section is sorted by its own sort; empty sections are omitted, except the
/// Threads section while it is on and nothing is filtered (see [`thread_rows`]).
pub fn compute(ws: &Workspace, opts: &RoomListOptions, filter: &str) -> Vec<Section> {
    let q = filter.trim().to_lowercase();
    let mut buckets: std::collections::HashMap<SectionId, Vec<RoomListItem>> = Default::default();
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
    let layout = &opts.layout;
    let omni = SectionId::from(SectionKind::Omnichannel);
    let mut out = Vec::new();
    for id in layout.order.iter().chain([&omni]) {
        if id == &SectionId::Builtin(SectionKind::Threads) {
            if layout.threads != ThreadsMode::Off && ws.settings().threads_enabled() {
                let threads = thread_rows(ws, opts, &q);
                if !threads.is_empty() || q.is_empty() {
                    out.push(Section {
                        id: id.clone(),
                        title: None,
                        items: Vec::new(),
                        threads,
                    });
                }
            }
            continue;
        }
        let Some(mut items) = buckets.remove(id).filter(|v| !v.is_empty()) else {
            continue;
        };
        sort_items(
            &mut items,
            layout.sort_of(id),
            layout.is_unread_first(id),
            opts,
        );
        out.push(Section {
            title: id
                .group_id()
                .and_then(|g| layout.group_name(g))
                .map(str::to_owned),
            id: id.clone(),
            items,
            threads: Vec::new(),
        });
    }
    out
}

/// Rocket.Chat's `makeThreadName`, with E2E and deleted parents named.
fn thread_title(m: &Message) -> ThreadTitle {
    if crate::thread_index::is_removed(m) {
        return ThreadTitle::Removed;
    }
    if m.is_encrypted() && !crate::e2e::shown::is_decrypted(m) {
        return ThreadTitle::Encrypted;
    }
    let text = m.plain_text();
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(truncate);
    let att = || {
        m.attachments
            .as_ref()
            .and_then(|a| a.first())
            .and_then(|a| {
                [&a.title, &a.text, &a.description]
                    .into_iter()
                    .flatten()
                    .find(|t| !t.trim().is_empty())
            })
            .map(|t| truncate(t))
    };
    let file = || {
        m.file
            .as_ref()
            .or(m.files.as_ref().and_then(|f| f.first()))
            .map(|f| truncate(&f.name))
    };
    ThreadTitle::Text(first.or_else(att).or_else(file).unwrap_or_default())
}

/// Unread state of thread `tmid` in a room (`None`: read or hidden).
/// Hidden unread status keeps only direct mentions (unless mention status is
/// hidden too), like the room badge.
pub fn thread_unread(s: &rocketchat::models::Subscription, tmid: &str) -> Option<BadgeVariant> {
    let has = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|v| v.iter().any(|t| t == tmid));
    let hide_unread = s.hide_unread_status == Some(true);
    let hide_mention = s.hide_mention_status == Some(true);
    let mention = has(&s.tunread_user) && !hide_mention;
    let group = has(&s.tunread_group) && !hide_mention;
    let any = has(&s.tunread) || has(&s.tunread_user) || has(&s.tunread_group);
    if mention {
        Some(BadgeVariant::Mention)
    } else if hide_unread || !any {
        None
    } else if group {
        Some(BadgeVariant::GroupMention)
    } else {
        Some(BadgeVariant::Thread)
    }
}

/// Rows of the Threads section (empty while it is off or threads are
/// disabled on the server), by `opts.layout.threads`:
/// - Unread: unread threads of listed rooms, plus the open thread
///   (`opts.kept_thread`);
/// - UnreadKept: also the threads kept in the sidebar;
/// - Following: also the followed threads of crawled rooms.
///
/// Parents come from the thread index only; unknown ones give placeholder
/// rows. Deleted parents are listed only while unread or kept. `q` (lower
/// case) matches the room name and the title. Sorted by the section's sort,
/// unread (and the open thread) first when the section lists unread first.
pub fn thread_rows(ws: &Workspace, opts: &RoomListOptions, q: &str) -> Vec<ThreadListItem> {
    let mode = opts.layout.threads;
    if mode == ThreadsMode::Off || !ws.settings().threads_enabled() {
        return Vec::new();
    }
    let idx = ws.thread_index();
    let listed = |rid: &str| {
        ws.room(rid)
            .filter(|e| e.sub.as_ref().is_some_and(|s| s.open) && !e.is_archived())
    };
    let kept_open = opts.kept_thread.as_ref().map(|(_, t)| t.as_str());
    let me = ws.user_id();
    // tmid -> rid, deduplicated.
    let mut wanted: std::collections::BTreeMap<&str, &str> = Default::default();
    for e in ws.rooms().values() {
        let Some(s) = e.sub.as_ref().filter(|_| listed(e.rid()).is_some()) else {
            continue;
        };
        for v in [&s.tunread, &s.tunread_user, &s.tunread_group]
            .into_iter()
            .flatten()
        {
            for t in v {
                if thread_unread(s, t).is_some() {
                    wanted.insert(t, e.rid());
                }
            }
        }
    }
    if let Some((rid, tmid)) = opts.kept_thread.as_ref() {
        wanted.insert(tmid, rid);
    }
    if mode != ThreadsMode::Unread {
        for (rid, tmid) in idx.kept() {
            wanted.insert(tmid, rid);
        }
    }
    if mode == ThreadsMode::Following {
        for (rid, f) in idx.followed_rooms() {
            for t in &f.tmids {
                wanted.insert(t, rid);
            }
        }
    }
    let mut out = Vec::new();
    for (tmid, rid) in wanted {
        let Some(e) = listed(rid) else {
            continue;
        };
        let parent = idx.parent(tmid).filter(|p| p.rid == rid);
        let unread = e.sub.as_ref().and_then(|s| thread_unread(s, tmid));
        let kept = idx.is_kept(tmid) && mode != ThreadsMode::Unread;
        if parent.is_some_and(crate::thread_index::is_removed)
            && unread.is_none()
            && !kept
            && kept_open != Some(tmid)
        {
            continue;
        }
        let title = match parent {
            Some(p) => thread_title(p),
            None if idx.is_gone(tmid) => ThreadTitle::Unavailable,
            None => ThreadTitle::Loading,
        };
        let room_name = display_name(ws, e, opts.use_real_names);
        if !q.is_empty() {
            let hit = |s: &str| s.to_lowercase().contains(q);
            if !hit(&room_name) && !title.text().is_some_and(hit) {
                continue;
            }
        }
        let author = parent.and_then(|p| p.u.username.clone());
        let avatar = match &author {
            Some(u) => Avatar::User {
                username: u.clone(),
            },
            None => Avatar::Room {
                rid: rid.to_owned(),
                etag: e.room.as_ref().and_then(|r| r.avatar_etag.clone()),
            },
        };
        out.push(ThreadListItem {
            rid: rid.to_owned(),
            tmid: tmid.to_owned(),
            room_name,
            room_kind: kind_of(e),
            title,
            author,
            avatar,
            tlm: parent.and_then(crate::thread_index::activity),
            tcount: parent.and_then(|p| p.tcount).unwrap_or(0),
            unread,
            kept,
            following: me.is_some_and(|id| {
                parent.is_some_and(|p| p.replies.iter().flatten().any(|u| u == id))
            }),
        });
    }
    sort_threads(&mut out, opts);
    out
}

fn sort_threads(rows: &mut [ThreadListItem], opts: &RoomListOptions) {
    let id = SectionId::Builtin(SectionKind::Threads);
    match opts.layout.sort_of(&id) {
        SidebarSort::Activity => {
            rows.sort_by(|a, b| b.tlm.cmp(&a.tlm).then_with(|| a.tmid.cmp(&b.tmid)))
        }
        SidebarSort::ActivityAsc => {
            rows.sort_by(|a, b| cmp_none_last(&a.tlm, &b.tlm).then_with(|| a.tmid.cmp(&b.tmid)))
        }
        SidebarSort::Alphabetical => rows.sort_by(|a, b| {
            let title = |t: &ThreadListItem| t.title.text().map(str::to_lowercase);
            a.room_name
                .to_lowercase()
                .cmp(&b.room_name.to_lowercase())
                .then_with(|| a.rid.cmp(&b.rid))
                .then_with(|| cmp_none_last(&title(a), &title(b)))
                .then_with(|| a.tmid.cmp(&b.tmid))
        }),
    }
    if opts.layout.is_unread_first(&id) {
        let kept = opts.kept_thread.as_ref().map(|(_, t)| t.as_str());
        // Stable: keeps the sort within both blocks.
        rows.sort_by_key(|r| r.unread.is_none() && kept != Some(r.tmid.as_str()));
    }
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
    use crate::sidebar_layout::ThreadsMode;
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
        sections.iter().map(|s| s.kind().unwrap()).collect()
    }

    fn layout(f: impl FnOnce(&mut UiPrefs)) -> Layout {
        let mut p = UiPrefs::default();
        f(&mut p);
        crate::sidebar_layout::sanitize_prefs(&mut p);
        Layout::from_prefs(&p)
    }

    fn sorted(sort: SidebarSort) -> Layout {
        layout(|p| p.sidebar_sort = sort)
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
                layout: sorted(SidebarSort::Alphabetical),
                ..Default::default()
            },
            "",
        );
        assert_eq!(names(&s[0]), ["alpha", "Beta", "charlie"]);
        let mut ws = ws;
        ws.apply_subscription(Change::Inserted, sub("n", "none", RoomType::Channel));
        let s = compute(
            &ws,
            &RoomListOptions {
                layout: sorted(SidebarSort::ActivityAsc),
                ..Default::default()
            },
            "",
        );
        assert_eq!(names(&s[0]), ["alpha", "charlie", "Beta", "none"]);
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
                layout: sorted(SidebarSort::Alphabetical),
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

    fn ids(sections: &[Section]) -> Vec<String> {
        sections.iter().map(|s| s.id.as_config_str()).collect()
    }

    fn add_group(p: &mut UiPrefs, name: &str, types: &[GroupType]) -> String {
        use crate::sidebar_layout::{LayoutEdit, apply};
        apply(
            p,
            LayoutEdit::AddGroup {
                name: name.into(),
                types: types.to_vec(),
            },
        );
        p.sidebar_groups.last().unwrap().id.clone()
    }

    /// One room of every kind, plus a favourite channel and an omnichannel room.
    fn mixed_ws() -> Workspace {
        let mut team = room("t", "team", RoomType::Private);
        team.team_main = Some(true);
        let mut disc = room("di", "disc", RoomType::Private);
        disc.prid = Some("c".into());
        let mut fav = sub("f", "fav", RoomType::Channel);
        fav.f = true;
        ws_with(vec![
            (sub("c", "chan", RoomType::Channel), None),
            (sub("p", "priv", RoomType::Private), None),
            (sub("d", "bob", RoomType::Direct), None),
            (sub("t", "team", RoomType::Private), Some(team)),
            (sub("di", "disc", RoomType::Private), Some(disc)),
            (fav, None),
            (sub("l", "visitor", RoomType::Livechat), None),
        ])
    }

    const WORK: [GroupType; 4] = [
        GroupType::Discussions,
        GroupType::Channels,
        GroupType::PrivateGroups,
        GroupType::DirectMessages,
    ];

    #[test]
    fn a_group_takes_the_rooms_of_its_types() {
        let ws = mixed_ws();
        let mut id = String::new();
        let opts = RoomListOptions {
            layout: layout(|p| id = add_group(p, "Work", &WORK)),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        let g = format!("group:{id}");
        assert_eq!(ids(&s), ["favorites", "teams", &g, "omnichannel"]);
        assert_eq!(s[2].title.as_deref(), Some("Work"));
        assert_eq!(s[2].kind(), None);
        assert_eq!(s[0].title, None);
        let mut rids: Vec<&str> = s[2].items.iter().map(|i| i.rid.as_str()).collect();
        rids.sort_unstable();
        assert_eq!(rids, ["c", "d", "di", "p"]);
        // The favourite channel stays in Favorites.
        assert_eq!(s[0].items[0].rid, "f");
        // An omnichannel room is never grouped.
        assert_eq!(s[3].items[0].rid, "l");
        assert!(s[3].is(SectionKind::Omnichannel));

        // With unread on top an unread room and the kept open room go to
        // Unread, not to the group that claims their type.
        let mut ws = ws;
        let mut c = sub("c", "chan", RoomType::Channel);
        c.unread = 1;
        ws.apply_subscription(Change::Updated, c);
        let opts = RoomListOptions {
            unread_on_top: true,
            kept_unread: Some("d".into()),
            ..opts
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(ids(&s), ["unread", "favorites", "teams", &g, "omnichannel"]);
        let mut unread: Vec<&str> = s[0].items.iter().map(|i| i.rid.as_str()).collect();
        unread.sort_unstable();
        assert_eq!(unread, ["c", "d"]);
        let mut rids: Vec<&str> = s[3].items.iter().map(|i| i.rid.as_str()).collect();
        rids.sort_unstable();
        assert_eq!(rids, ["di", "p"]);
    }

    #[test]
    fn a_group_without_rooms_or_types_is_omitted() {
        let ws = mixed_ws();
        let opts = RoomListOptions {
            layout: layout(|p| {
                add_group(p, "Nothing", &[]);
            }),
            ..Default::default()
        };
        assert!(
            compute(&ws, &opts, "")
                .iter()
                .all(|s| s.id.group_id().is_none())
        );

        // A group whose types have no rooms (no teams here).
        let ws = ws_with(vec![(sub("c", "chan", RoomType::Channel), None)]);
        let opts = RoomListOptions {
            layout: layout(|p| {
                add_group(p, "Teams", &[GroupType::Teams]);
            }),
            ..Default::default()
        };
        assert_eq!(ids(&compute(&ws, &opts, "")), ["channels"]);
    }

    #[test]
    fn a_group_while_not_grouping_by_type() {
        let ws = mixed_ws();
        let mut id = String::new();
        let opts = RoomListOptions {
            layout: layout(|p| id = add_group(p, "People", &[GroupType::DirectMessages])),
            group_by_type: false,
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        let g = format!("group:{id}");
        assert_eq!(ids(&s), ["favorites", &g, "conversations", "omnichannel"]);
        assert_eq!(names(&s[1]), ["bob"]);
        assert_eq!(s[2].len(), 4);
    }

    #[test]
    fn sections_follow_the_custom_order() {
        use crate::sidebar_layout::{LayoutEdit, apply};
        let ws = mixed_ws();
        let mut id = String::new();
        let opts = RoomListOptions {
            layout: layout(|p| {
                id = add_group(p, "Rooms", &[GroupType::Channels, GroupType::PrivateGroups]);
                apply(
                    p,
                    LayoutEdit::Move {
                        id: SectionKind::DirectMessages.into(),
                        delta: -100,
                    },
                );
                apply(
                    p,
                    LayoutEdit::MoveTo {
                        id: SectionKind::Favorites.into(),
                        anchor: SectionId::group(&id),
                        after: true,
                    },
                );
            }),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        let g = format!("group:{id}");
        assert_eq!(
            ids(&s),
            [
                "direct_messages",
                "teams",
                "discussions",
                &g,
                "favorites",
                "omnichannel"
            ]
        );
    }

    #[test]
    fn per_section_sort() {
        use crate::sidebar_layout::{LayoutEdit, apply};
        let ws = ws_with(vec![
            (
                sub("a", "alpha", RoomType::Channel),
                Some(with_lm(room("a", "alpha", RoomType::Channel), 1000)),
            ),
            (
                sub("b", "Beta", RoomType::Channel),
                Some(with_lm(room("b", "Beta", RoomType::Channel), 3000)),
            ),
            (sub("n", "none", RoomType::Channel), None),
            (
                sub("x", "xavier", RoomType::Direct),
                Some(with_lm(room("x", "xavier", RoomType::Direct), 500)),
            ),
            (
                sub("y", "yvonne", RoomType::Direct),
                Some(with_lm(room("y", "yvonne", RoomType::Direct), 2500)),
            ),
        ]);
        let set = |sort| {
            layout(move |p| {
                apply(
                    p,
                    LayoutEdit::SetSort {
                        id: SectionKind::Channels.into(),
                        sort: Some(sort),
                    },
                );
            })
        };
        let opts = RoomListOptions {
            layout: set(SidebarSort::ActivityAsc),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(names(&s[0]), ["alpha", "Beta", "none"]);
        // DMs keep the default (newest first).
        assert_eq!(names(&s[1]), ["yvonne", "xavier"]);
        let opts = RoomListOptions {
            layout: set(SidebarSort::Alphabetical),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(names(&s[0]), ["alpha", "Beta", "none"]);
        assert_eq!(names(&s[1]), ["yvonne", "xavier"]);
        let opts = RoomListOptions {
            layout: set(SidebarSort::Activity),
            ..Default::default()
        };
        assert_eq!(
            names(&compute(&ws, &opts, "")[0]),
            ["Beta", "alpha", "none"]
        );
    }

    #[test]
    fn unread_first_keeps_the_open_room_in_the_unread_block() {
        use crate::sidebar_layout::{LayoutEdit, apply};
        let mut u = sub("u", "unread", RoomType::Channel);
        u.unread = 2;
        let mut a = sub("a", "alert", RoomType::Channel);
        a.alert = true;
        let ws = ws_with(vec![
            (
                u,
                Some(with_lm(room("u", "unread", RoomType::Channel), 1000)),
            ),
            (
                a,
                Some(with_lm(room("a", "alert", RoomType::Channel), 2000)),
            ),
            (
                sub("k", "kept", RoomType::Channel),
                Some(with_lm(room("k", "kept", RoomType::Channel), 500)),
            ),
            (
                sub("r", "read", RoomType::Channel),
                Some(with_lm(room("r", "read", RoomType::Channel), 3000)),
            ),
        ]);
        let mut opts = RoomListOptions::default();
        assert_eq!(
            names(&compute(&ws, &opts, "")[0]),
            ["read", "alert", "unread", "kept"]
        );
        opts.layout = layout(|p| {
            apply(
                p,
                LayoutEdit::SetUnreadFirst {
                    id: SectionKind::Channels.into(),
                    on: Some(true),
                },
            );
        });
        assert_eq!(
            names(&compute(&ws, &opts, "")[0]),
            ["alert", "unread", "read", "kept"]
        );
        // The open room (read meanwhile) stays with the unread ones.
        opts.kept_unread = Some("k".into());
        assert_eq!(
            names(&compute(&ws, &opts, "")[0]),
            ["alert", "unread", "kept", "read"]
        );
    }

    // --- Threads section ----------------------------------------------------------

    fn parent(id: &str, rid: &str, tlm: i64, author: &str) -> Message {
        let mut m = msg(id, rid, 1, author);
        m.tcount = Some(2);
        m.tlm = Some(ts(tlm));
        m.replies = Some(vec![author.into()]);
        m
    }

    fn thread_opts(mode: ThreadsMode) -> RoomListOptions {
        RoomListOptions {
            layout: layout(|p| p.sidebar_threads = mode),
            ..Default::default()
        }
    }

    fn tmids(rows: &[ThreadListItem]) -> Vec<&str> {
        rows.iter().map(|r| r.tmid.as_str()).collect()
    }

    /// Room "a" with unread threads t1 (plain), t2 (mention), t3 (group
    /// mention); room "b" without; parents of t1 and t2 known.
    fn threads_ws(mode: ThreadsMode) -> Workspace {
        let mut a = sub("a", "alpha", RoomType::Channel);
        a.tunread = Some(vec!["t1".into(), "t2".into(), "t3".into()]);
        a.tunread_user = Some(vec!["t2".into()]);
        a.tunread_group = Some(vec!["t3".into()]);
        let mut ws = ws_with(vec![(a, None), (sub("b", "beta", RoomType::Channel), None)]);
        ws.set_thread_config(mode, []);
        ws.apply_thread_parents(vec![
            parent("t1", "a", 300, "bob"),
            parent("t2", "a", 100, "carol"),
        ]);
        ws
    }

    #[test]
    fn unread_threads_with_their_variants() {
        let ws = threads_ws(ThreadsMode::Unread);
        let opts = thread_opts(ThreadsMode::Unread);
        let s = compute(&ws, &opts, "");
        assert_eq!(ids(&s), ["threads", "channels"]);
        let rows = &s[0].threads;
        assert_eq!(s[0].len(), 3);
        // Unread first is on by default; newest reply first; t3 is a placeholder.
        assert_eq!(tmids(rows), ["t1", "t2", "t3"]);
        let by = |t: &str| rows.iter().find(|r| r.tmid == t).unwrap();
        assert_eq!(by("t1").unread, Some(BadgeVariant::Thread));
        assert_eq!(by("t2").unread, Some(BadgeVariant::Mention));
        assert_eq!(by("t3").unread, Some(BadgeVariant::GroupMention));
        let t1 = by("t1");
        assert_eq!(t1.title, ThreadTitle::Text("text t1".into()));
        assert_eq!((t1.room_name.as_str(), t1.tcount), ("alpha", 2));
        assert_eq!(t1.tlm, Some(ts(300)));
        assert_eq!(
            t1.avatar,
            Avatar::User {
                username: "bob".into()
            }
        );
        assert!(t1.is_loaded() && !t1.kept);
        let t3 = by("t3");
        assert_eq!((t3.title.clone(), t3.tlm), (ThreadTitle::Loading, None));
        assert!(matches!(t3.avatar, Avatar::Room { .. }));
        assert!(!t3.is_loaded());
        // Off (the default) and disabled threads: no section.
        assert_eq!(
            ids(&compute(&ws, &RoomListOptions::default(), "")),
            ["channels"]
        );
        let mut off = ws.clone();
        off.apply_setting(
            Change::Updated,
            rocketchat::models::Setting {
                id: "Threads_enabled".into(),
                value: serde_json::json!(false),
                ..Default::default()
            },
        );
        assert_eq!(ids(&compute(&off, &opts, "")), ["channels"]);
        assert!(thread_rows(&off, &opts, "").is_empty());
    }

    #[test]
    fn hidden_unread_and_mention_status() {
        let mut ws = threads_ws(ThreadsMode::Unread);
        let opts = thread_opts(ThreadsMode::Unread);
        let mut a = ws.subscription("a").unwrap().clone();
        a.hide_unread_status = Some(true);
        ws.apply_subscription(Change::Updated, a.clone());
        // Only the direct mention stays.
        let rows = thread_rows(&ws, &opts, "");
        assert_eq!(tmids(&rows), ["t2"]);
        assert_eq!(rows[0].unread, Some(BadgeVariant::Mention));
        a.hide_mention_status = Some(true);
        ws.apply_subscription(Change::Updated, a.clone());
        assert!(thread_rows(&ws, &opts, "").is_empty());
        // Mention status hidden alone: listed as plain unread threads.
        a.hide_unread_status = None;
        ws.apply_subscription(Change::Updated, a);
        let rows = thread_rows(&ws, &opts, "");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.unread == Some(BadgeVariant::Thread)));
    }

    /// Followed: the user is among the parent's `replies`.
    #[test]
    fn thread_rows_know_whether_the_user_follows() {
        let mut ws = threads_ws(ThreadsMode::Unread);
        ws.apply_thread_parents(vec![parent("t3", "a", 50, "me")]);
        let rows = thread_rows(&ws, &thread_opts(ThreadsMode::Unread), "");
        let by = |t: &str| rows.iter().find(|r| r.tmid == t).unwrap();
        assert!(by("t3").following);
        assert!(!by("t1").following);
    }

    #[test]
    fn the_open_thread_stays_listed_once_read() {
        let mut ws = threads_ws(ThreadsMode::Unread);
        let mut opts = thread_opts(ThreadsMode::Unread);
        let mut a = ws.subscription("a").unwrap().clone();
        a.tunread = Some(vec!["t2".into(), "t3".into()]);
        ws.apply_subscription(Change::Updated, a);
        assert_eq!(tmids(&thread_rows(&ws, &opts, "")), ["t2", "t3"]);
        opts.kept_thread = Some(("a".into(), "t1".into()));
        let rows = thread_rows(&ws, &opts, "");
        // Read, but kept in the unread block (above the older t2).
        assert_eq!(tmids(&rows), ["t1", "t2", "t3"]);
        assert_eq!(rows[0].unread, None);
        // Not in a listed room: not shown.
        opts.kept_thread = Some(("gone".into(), "x".into()));
        assert_eq!(thread_rows(&ws, &opts, "").len(), 2);
    }

    #[test]
    fn kept_threads_in_subscribed_rooms_only() {
        let mut ws = threads_ws(ThreadsMode::UnreadKept);
        let opts = thread_opts(ThreadsMode::UnreadKept);
        ws.set_thread_config(
            ThreadsMode::UnreadKept,
            [("b", "k1"), ("left", "k2"), ("a", "t1")],
        );
        ws.apply_thread_parents(vec![parent("k1", "b", 50, "bob")]);
        let rows = thread_rows(&ws, &opts, "");
        assert_eq!(tmids(&rows), ["t1", "t2", "t3", "k1"]);
        assert!(rows[0].kept && rows[3].kept && !rows[1].kept);
        assert_eq!(rows[3].unread, None);
        // Unread mode ignores kept threads.
        assert_eq!(
            tmids(&thread_rows(&ws, &thread_opts(ThreadsMode::Unread), "")),
            ["t1", "t2", "t3"]
        );
        assert!(ws.thread_index().kept_enabled());
    }

    #[test]
    fn following_lists_the_followed_sets() {
        let mut ws = threads_ws(ThreadsMode::Following);
        let opts = thread_opts(ThreadsMode::Following);
        ws.apply_followed(
            "b",
            vec![parent("f1", "b", 500, "me"), parent("f2", "b", 50, "bob")],
            None,
            std::time::Instant::now(),
        );
        // f2 does not list "me" in its replies: not followed.
        let rows = thread_rows(&ws, &opts, "");
        assert_eq!(tmids(&rows), ["t1", "t2", "t3", "f1"]);
        // Unread first off: by latest reply only.
        let mut opts = opts;
        opts.layout = layout(|p| {
            p.sidebar_threads = ThreadsMode::Following;
            crate::sidebar_layout::apply(
                p,
                crate::sidebar_layout::LayoutEdit::SetUnreadFirst {
                    id: SectionKind::Threads.into(),
                    on: Some(false),
                },
            );
        });
        assert_eq!(
            tmids(&thread_rows(&ws, &opts, "")),
            ["f1", "t1", "t2", "t3"]
        );
    }

    #[test]
    fn thread_sorts_and_filter() {
        let ws = threads_ws(ThreadsMode::Unread);
        let with = |sort| {
            let mut o = thread_opts(ThreadsMode::Unread);
            o.layout = layout(|p| {
                p.sidebar_threads = ThreadsMode::Unread;
                crate::sidebar_layout::apply(
                    p,
                    crate::sidebar_layout::LayoutEdit::SetSort {
                        id: SectionKind::Threads.into(),
                        sort: Some(sort),
                    },
                );
            });
            o
        };
        // Oldest reply first, unknown activity last.
        assert_eq!(
            tmids(&thread_rows(&ws, &with(SidebarSort::ActivityAsc), "")),
            ["t2", "t1", "t3"]
        );
        // By room, then title (placeholders last).
        let mut ws2 = ws.clone();
        let mut t2 = parent("t2", "a", 100, "carol");
        t2.msg = "Another topic\nsecond line".into();
        t2.updated_at = Some(ts(2));
        ws2.apply_thread_parents(vec![t2]);
        let rows = thread_rows(&ws2, &with(SidebarSort::Alphabetical), "");
        assert_eq!(tmids(&rows), ["t2", "t1", "t3"]);
        assert_eq!(rows[0].title, ThreadTitle::Text("Another topic".into()));
        // The filter matches titles and room names.
        let opts = thread_opts(ThreadsMode::Unread);
        assert_eq!(tmids(&thread_rows(&ws2, &opts, "another")), ["t2"]);
        assert_eq!(thread_rows(&ws2, &opts, "alph").len(), 3);
        // A filter without hits drops the section; no filter keeps it empty.
        assert_eq!(ids(&compute(&ws2, &opts, "beta")), ["channels"]);
        let empty = ws_with(vec![(sub("c", "chan", RoomType::Channel), None)]);
        let s = compute(&empty, &opts, "");
        assert_eq!(ids(&s), ["threads", "channels"]);
        assert!(s[0].is_empty());
    }

    #[test]
    fn titles_of_encrypted_removed_and_gone_parents() {
        let mut ws = threads_ws(ThreadsMode::UnreadKept);
        ws.set_thread_config(ThreadsMode::UnreadKept, [("b", "k")]);
        let opts = thread_opts(ThreadsMode::UnreadKept);
        let mut e = parent("t1", "a", 300, "bob");
        e.t = Some("e2e".into());
        e.msg = "Y2lwaGVydGV4dA==".into();
        e.updated_at = Some(ts(5));
        let mut rm = parent("t2", "a", 100, "carol");
        rm.t = Some("rm".into());
        rm.msg = String::new();
        rm.updated_at = Some(ts(5));
        ws.apply_message(e);
        ws.apply_message(rm);
        ws.thread_gone("k");
        let rows = thread_rows(&ws, &opts, "");
        let by = |t: &str| rows.iter().find(|r| r.tmid == t).unwrap().title.clone();
        assert_eq!(by("t1"), ThreadTitle::Encrypted);
        assert_eq!(by("t2"), ThreadTitle::Removed);
        assert_eq!(by("k"), ThreadTitle::Unavailable);
        assert_eq!(by("t3"), ThreadTitle::Loading);
        assert_eq!(
            ThreadTitle::Removed.i18n_key(),
            Some("sidebar.thread_removed")
        );
        // Once read, a removed parent is no longer listed.
        let mut a = ws.subscription("a").unwrap().clone();
        a.tunread = Some(vec!["t1".into()]);
        a.tunread_user = None;
        a.tunread_group = None;
        ws.apply_subscription(Change::Updated, a);
        assert_eq!(tmids(&thread_rows(&ws, &opts, "")), ["t1", "k"]);
    }

    #[test]
    fn thread_titles_follow_make_thread_name() {
        use rocketchat::models::MessageAttachment;
        let mut m = msg("p", "r", 1, "bob");
        m.msg = "\n  first line \nsecond".into();
        assert_eq!(thread_title(&m), ThreadTitle::Text("first line".into()));
        m.msg = String::new();
        m.attachments = Some(vec![MessageAttachment {
            title: Some("Report".into()),
            ..Default::default()
        }]);
        assert_eq!(thread_title(&m), ThreadTitle::Text("Report".into()));
        m.attachments = None;
        m.file = Some(FileInfo {
            id: "1".into(),
            name: "cat.png".into(),
            ..Default::default()
        });
        assert_eq!(thread_title(&m), ThreadTitle::Text("cat.png".into()));
    }

    #[test]
    fn default_layout_keeps_the_old_order() {
        let mut u = sub("u", "unread", RoomType::Channel);
        u.unread = 1;
        let mut ws = mixed_ws();
        ws.apply_subscription(Change::Inserted, u);
        let opts = RoomListOptions {
            unread_on_top: true,
            ..Default::default()
        };
        assert_eq!(
            ids(&compute(&ws, &opts, "")),
            [
                "unread",
                "favorites",
                "teams",
                "discussions",
                "channels",
                "private_groups",
                "direct_messages",
                "omnichannel"
            ]
        );
    }
}
