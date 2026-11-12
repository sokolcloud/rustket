//! Sidebar sections computed from a [`Workspace`]: grouping, sorting, display
//! names, badges, previews and filtering. Pure; recompute when
//! `Dirty::ROOMS | Dirty::SETTINGS | Dirty::PRESENCE | Dirty::DRAFTS` changed
//! (and `Dirty::THREAD_INDEX` while the thread index is in use, see
//! `ThreadIndex::indexing`).

use rocketchat::RcDate;
use rocketchat::models::{Message, RoomType, UserStatus};

use crate::config::{SidebarSort, UiPrefs};
use crate::flags::FlagColor;
use crate::sidebar_layout::{GroupType, Layout, MAX_STARRED_THREADS, SectionId, ThreadsMode};
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
    /// `(rid, tmid)` of the thread open in the panel: never cut by the row
    /// limit (unlike `kept_thread` it does not change the sort).
    pub open_thread: Option<(String, String)>,
    /// Threads whose parent the user starred show in Favorites (when the
    /// server allows starring and threads), not in Threads.
    pub starred_threads: bool,
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
            open_thread: None,
            starred_threads: prefs.sidebar_starred_threads,
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
            open_thread: None,
            starred_threads: true,
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
    /// The room's flag (never on omnichannel rooms).
    pub flag: Option<FlagColor>,
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

/// A thread row (the Threads section, starred threads in Favorites).
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
    /// The user starred the parent (tracked while starred threads are on).
    pub starred: bool,
    /// The thread's flag (never in omnichannel rooms).
    pub flag: Option<FlagColor>,
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
    /// Group name, or the user's name of a flag; `None` otherwise (title
    /// from [`SectionId::i18n_key`]).
    pub title: Option<String>,
    /// Room rows; listed before the thread rows.
    pub items: Vec<RoomListItem>,
    /// Thread rows (Threads; starred threads in Favorites; flagged threads
    /// in flag sections).
    pub threads: Vec<ThreadListItem>,
    /// Thread rows left out by the section's limit (`ui.sidebar_threads_limit`;
    /// [`MAX_STARRED_THREADS`] in Favorites).
    pub threads_hidden: usize,
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
    let flag = (kind != RoomKind::Omnichannel)
        .then(|| ws.room_flag(&rid))
        .flatten();
    RoomListItem {
        flag,
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

/// The section a room is listed in: Unread, its flag's section, Favorites,
/// Omnichannel, the group that claims its type, Conversations (not grouping
/// by type), its type's section. Display order is `opts.layout.order`,
/// independent of this.
fn section_for(e: &RoomEntry, item: &RoomListItem, opts: &RoomListOptions) -> SectionId {
    if opts.unread_on_top
        && (is_unread(e) || opts.kept_unread.as_deref() == Some(item.rid.as_str()))
    {
        return SectionKind::Unread.into();
    }
    // A flag is a deliberate, usually temporary state: it beats Favorites.
    if let Some(c) = item.flag {
        return SectionId::Flag(c);
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
/// archived are excluded (Rocket.Chat's `getRoomCategory`), and so are
/// service rooms; `filter` is a case-insensitive substring over names,
/// `fname`s and DM participants. Each section is sorted by its own sort;
/// empty sections are omitted, except the Threads section while it is on and
/// nothing is filtered (see [`thread_rows`]). Favorites lists favorite rooms,
/// then starred threads (see [`starred_thread_rows`]), and a flag section
/// its flagged rooms, then flagged threads ([`flagged_thread_rows`]), each
/// block by the section's sort. A flagged item is listed in its flag's
/// section only (rooms: unless unread rooms are on top and it is unread).
pub fn compute(ws: &Workspace, opts: &RoomListOptions, filter: &str) -> Vec<Section> {
    let q = filter.trim().to_lowercase();
    let mut buckets: std::collections::HashMap<SectionId, Vec<RoomListItem>> = Default::default();
    for e in ws.rooms().values() {
        let Some(sub) = e.sub.as_ref() else {
            continue;
        };
        if !sub.open || e.is_archived() || e.is_service_room() {
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
                let mut threads = all_thread_rows(ws, opts, &q);
                // A filter searches every row that is there.
                let threads_hidden = if q.is_empty() {
                    cap_thread_rows(&mut threads, opts)
                } else {
                    0
                };
                if !threads.is_empty() || q.is_empty() {
                    out.push(Section {
                        id: id.clone(),
                        title: None,
                        items: Vec::new(),
                        threads,
                        threads_hidden,
                    });
                }
            }
            continue;
        }
        let (mut threads, mut threads_hidden) = (Vec::new(), 0);
        if id == &SectionId::Builtin(SectionKind::Favorites) {
            threads = starred_thread_rows(ws, opts, &q);
            threads_hidden = threads.len().saturating_sub(MAX_STARRED_THREADS);
            threads.truncate(MAX_STARRED_THREADS);
        }
        if let Some(c) = id.flag() {
            threads = flagged_thread_rows(ws, opts, &q, c);
        }
        let mut items = buckets.remove(id).unwrap_or_default();
        if items.is_empty() && threads.is_empty() {
            continue;
        }
        sort_items(
            &mut items,
            layout.sort_of(id),
            layout.is_unread_first(id),
            opts,
        );
        out.push(Section {
            title: match id {
                SectionId::Flag(c) => ws.flags().name(*c),
                _ => id.group_id().and_then(|g| layout.group_name(g)),
            }
            .map(str::to_owned),
            id: id.clone(),
            items,
            threads,
            threads_hidden,
        });
    }
    out
}

/// Whether starred threads show in Favorites (and leave Threads).
fn starred_threads_shown(ws: &Workspace, opts: &RoomListOptions) -> bool {
    opts.starred_threads
        && opts.show_favorites
        && ws.thread_index().pins_enabled()
        && ws.settings().allow_starring()
        && ws.settings().threads_enabled()
}

/// A room whose rows the sidebar lists: subscribed, open, not archived, not
/// a service room.
fn listed<'w>(ws: &'w Workspace, rid: &str) -> Option<&'w RoomEntry> {
    ws.room(rid).filter(|e| {
        e.sub.as_ref().is_some_and(|s| s.open) && !e.is_archived() && !e.is_service_room()
    })
}

/// Whether flagged threads show in flag sections (and leave Threads and
/// Favorites): threads are enabled on the server.
fn flagged_threads_shown(ws: &Workspace) -> bool {
    ws.settings().threads_enabled()
}

/// Whether thread `tmid` of `rid` is listed in a flag section.
fn in_flag_section(ws: &Workspace, rid: &str, tmid: &str) -> bool {
    ws.thread_flag(tmid).is_some()
        && flagged_threads_shown(ws)
        && ws
            .room(rid)
            .is_some_and(|e| kind_of(e) != RoomKind::Omnichannel)
}

/// Thread rows of the flag section of `color`: flagged threads in listed,
/// non-omnichannel rooms, whatever the Threads mode is (empty while threads
/// are disabled on the server). Gone or deleted parents are listed while
/// flagged. `q` (lower case) matches the room name and the title. Sorted by
/// the section's sort and unread-first setting.
pub fn flagged_thread_rows(
    ws: &Workspace,
    opts: &RoomListOptions,
    q: &str,
    color: FlagColor,
) -> Vec<ThreadListItem> {
    if !flagged_threads_shown(ws) {
        return Vec::new();
    }
    let mut out: Vec<ThreadListItem> = ws
        .flags()
        .threads()
        .filter(|(_, _, c)| *c == color)
        .filter_map(|(rid, tmid, _)| thread_item(ws, listed(ws, rid)?, tmid, opts, q))
        .filter(|t| t.flag == Some(color))
        .collect();
    sort_threads(&mut out, &SectionId::Flag(color), opts);
    out
}

/// Flag usage per color: `(listed, other)` items. Listed ones show in the
/// sidebar; the others are in rooms the user left, hid or that are
/// archived (kept, in case the room comes back) or in omnichannel rooms.
/// Flagged threads count as listed while threads are enabled on the
/// server.
pub fn flag_usage(ws: &Workspace) -> std::collections::BTreeMap<FlagColor, (usize, usize)> {
    let mut out: std::collections::BTreeMap<FlagColor, (usize, usize)> = Default::default();
    let shown = |rid: &str| listed(ws, rid).is_some_and(|e| kind_of(e) != RoomKind::Omnichannel);
    let flags = ws.flags();
    for (rid, c) in flags.rooms() {
        let n = out.entry(c).or_default();
        if shown(rid) { n.0 += 1 } else { n.1 += 1 }
    }
    let threads = flagged_threads_shown(ws);
    for (rid, _, c) in flags.threads() {
        let n = out.entry(c).or_default();
        if threads && shown(rid) {
            n.0 += 1
        } else {
            n.1 += 1
        }
    }
    out
}

/// Starred thread rows of Favorites (empty while they are off): threads
/// whose parent the user starred, in listed rooms, whatever the Threads
/// mode is. Unread ones stay here too (Threads does not repeat them);
/// flagged ones are in their flag's section.
/// Deleted parents are listed while starred. `q` (lower case) matches the
/// room name and the title. Sorted by the Favorites sort and unread-first
/// setting; not cut.
pub fn starred_thread_rows(ws: &Workspace, opts: &RoomListOptions, q: &str) -> Vec<ThreadListItem> {
    if !starred_threads_shown(ws, opts) {
        return Vec::new();
    }
    let mut out: Vec<ThreadListItem> = ws
        .thread_index()
        .starred()
        .filter(|(rid, tmid)| !in_flag_section(ws, rid, tmid))
        .filter_map(|(rid, tmid)| thread_item(ws, listed(ws, rid)?, tmid, opts, q))
        .collect();
    sort_threads(&mut out, &SectionKind::Favorites.into(), opts);
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
/// unread (and the open thread) first when the section lists unread first,
/// then cut to the section's limit (see [`cap_thread_rows`]).
pub fn thread_rows(ws: &Workspace, opts: &RoomListOptions, q: &str) -> Vec<ThreadListItem> {
    let mut rows = all_thread_rows(ws, opts, q);
    cap_thread_rows(&mut rows, opts);
    rows
}

/// Cuts sorted thread rows to `opts.layout.threads_limit` (0: no limit) and
/// returns how many were cut. The open thread (`opts.open_thread`, and
/// `opts.kept_thread`) is never cut: past the limit it takes the place of
/// the last rows.
pub fn cap_thread_rows(rows: &mut Vec<ThreadListItem>, opts: &RoomListOptions) -> usize {
    let limit = opts.layout.threads_limit as usize;
    if limit == 0 || rows.len() <= limit {
        return 0;
    }
    let hidden = rows.len() - limit;
    let mut keep: Vec<usize> = [&opts.open_thread, &opts.kept_thread]
        .into_iter()
        .flatten()
        .filter_map(|(rid, tmid)| {
            rows.iter()
                .position(|r| &r.tmid == tmid && &r.rid == rid)
                .filter(|i| *i >= limit)
        })
        .collect();
    keep.sort_unstable();
    keep.dedup();
    let tail: Vec<ThreadListItem> = keep.iter().map(|i| rows[*i].clone()).collect();
    rows.truncate(limit.saturating_sub(tail.len()));
    rows.extend(tail);
    hidden
}

/// [`thread_rows`] without the limit.
pub fn all_thread_rows(ws: &Workspace, opts: &RoomListOptions, q: &str) -> Vec<ThreadListItem> {
    let mode = opts.layout.threads;
    if mode == ThreadsMode::Off || !ws.settings().threads_enabled() {
        return Vec::new();
    }
    let idx = ws.thread_index();
    let kept_open = opts.kept_thread.as_ref().map(|(_, t)| t.as_str());
    // tmid -> rid, deduplicated.
    let mut wanted: std::collections::BTreeMap<&str, &str> = Default::default();
    for e in ws.rooms().values() {
        let Some(s) = e.sub.as_ref().filter(|_| listed(ws, e.rid()).is_some()) else {
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
    // Starred threads are in Favorites, flagged ones in flag sections.
    let starred = starred_threads_shown(ws, opts);
    wanted.retain(|t, r| !(starred && idx.is_starred(t)) && !in_flag_section(ws, r, t));
    let mut out = Vec::new();
    for (tmid, rid) in wanted {
        let Some(e) = listed(ws, rid) else {
            continue;
        };
        let Some(row) = thread_item(ws, e, tmid, opts, q) else {
            continue;
        };
        if matches!(row.title, ThreadTitle::Removed)
            && row.unread.is_none()
            && !row.kept
            && kept_open != Some(tmid)
        {
            continue;
        }
        out.push(row);
    }
    sort_threads(&mut out, &SectionKind::Threads.into(), opts);
    out
}

/// The row of thread `tmid` of the listed room `e` (`None`: `q`, lower
/// case, matches neither the room name nor the title).
fn thread_item(
    ws: &Workspace,
    e: &RoomEntry,
    tmid: &str,
    opts: &RoomListOptions,
    q: &str,
) -> Option<ThreadListItem> {
    let idx = ws.thread_index();
    let rid = e.rid();
    let me = ws.user_id();
    let parent = idx.parent(tmid).filter(|p| p.rid == rid);
    let unread = e.sub.as_ref().and_then(|s| thread_unread(s, tmid));
    let kept = idx.is_kept(tmid) && opts.layout.threads != ThreadsMode::Unread;
    let title = match parent {
        Some(p) => thread_title(p),
        None if idx.is_gone(tmid) => ThreadTitle::Unavailable,
        None => ThreadTitle::Loading,
    };
    let room_name = display_name(ws, e, opts.use_real_names);
    if !q.is_empty() {
        let hit = |s: &str| s.to_lowercase().contains(q);
        if !hit(&room_name) && !title.text().is_some_and(hit) {
            return None;
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
    Some(ThreadListItem {
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
        following: me
            .is_some_and(|id| parent.is_some_and(|p| p.replies.iter().flatten().any(|u| u == id))),
        starred: idx.is_starred(tmid),
        flag: (kind_of(e) != RoomKind::Omnichannel)
            .then(|| ws.thread_flag(tmid))
            .flatten(),
    })
}

/// Sorts thread rows by the sort of section `id`; with its unread-first
/// setting, unread rows (and the open thread) come first.
fn sort_threads(rows: &mut [ThreadListItem], id: &SectionId, opts: &RoomListOptions) {
    match opts.layout.sort_of(id) {
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
    if opts.layout.is_unread_first(id) {
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
/// non-archived rooms (service rooms aside); muted rooms (`disableNotifications`) don't count as
/// unread rooms, hidden mention status hides mentions.
pub fn unread_totals(ws: &Workspace) -> (u64, u64) {
    let mut rooms = 0;
    let mut mentions = 0;
    for e in ws.rooms().values() {
        let Some(s) = e
            .sub
            .as_ref()
            .filter(|s| s.open && !e.is_archived() && !e.is_service_room())
        else {
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
    use crate::workspace::{Change, Dirty};
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
    fn thread_rows_are_cut_to_the_limit() {
        let mut ws = threads_ws(ThreadsMode::UnreadKept);
        // Kept, read threads in "b": k1 newest of all, k2 oldest.
        ws.set_thread_config(ThreadsMode::UnreadKept, [("b", "k1"), ("b", "k2")]);
        ws.apply_thread_parents(vec![
            parent("k1", "b", 900, "bob"),
            parent("k2", "b", 10, "bob"),
        ]);
        let with = |limit: u32, unread_first: Option<bool>| RoomListOptions {
            layout: layout(|p| {
                p.sidebar_threads = ThreadsMode::UnreadKept;
                p.sidebar_threads_limit = limit;
                if let Some(on) = unread_first {
                    p.sidebar_section_prefs.insert(
                        "threads".into(),
                        crate::sidebar_layout::SectionPrefs {
                            sort: None,
                            unread_first: Some(on),
                        },
                    );
                }
            }),
            ..Default::default()
        };
        // Unread first: t1, t2, t3 (placeholder) then k1, k2.
        assert_eq!(
            tmids(&thread_rows(&ws, &with(0, None), "")),
            ["t1", "t2", "t3", "k1", "k2"]
        );
        let opts = with(2, None);
        assert_eq!(tmids(&thread_rows(&ws, &opts, "")), ["t1", "t2"]);
        let s = compute(&ws, &opts, "");
        assert_eq!((s[0].threads.len(), s[0].threads_hidden), (2, 3));
        // Without unread first the newest kept thread comes first.
        assert_eq!(
            tmids(&thread_rows(&ws, &with(2, Some(false)), "")),
            ["k1", "t1"]
        );
        // The open thread past the limit takes the last row (with unread
        // first it sorts into the unread block: t1, t2, k2, t3, k1).
        let mut open = with(2, None);
        open.kept_thread = Some(("b".into(), "k2".into()));
        assert_eq!(tmids(&thread_rows(&ws, &open, "")), ["t1", "k2"]);
        let mut open = with(2, Some(false));
        open.kept_thread = Some(("b".into(), "k2".into()));
        assert_eq!(tmids(&thread_rows(&ws, &open, "")), ["k1", "k2"]);
        let s = compute(&ws, &open, "");
        assert_eq!(s[0].threads_hidden, 3);
        // Inside the limit it stays where it is.
        open.kept_thread = Some(("a".into(), "t1".into()));
        assert_eq!(tmids(&thread_rows(&ws, &open, "")), ["k1", "t1"]);
        // A read thread open in the panel (not kept: it keeps its place in
        // the sort) is not cut either.
        let mut open = with(2, None);
        open.open_thread = Some(("b".into(), "k2".into()));
        assert_eq!(tmids(&thread_rows(&ws, &open, "")), ["t1", "k2"]);
        let s = compute(&ws, &open, "");
        assert_eq!(s[0].threads_hidden, 3);
        // Exactly the limit: nothing hidden; a filter searches every row.
        let s = compute(&ws, &with(5, None), "");
        assert_eq!((s[0].threads.len(), s[0].threads_hidden), (5, 0));
        let s = compute(&ws, &with(1, None), "alpha");
        assert_eq!((s[0].threads.len(), s[0].threads_hidden), (3, 0));
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

    // --- Starred threads in Favorites ------------------------------------------------

    fn star(mut m: Message) -> Message {
        m.starred = Some(vec![rocketchat::models::IdRef { id: "me".into() }]);
        m
    }

    /// [`threads_ws`] with starred threads on: t1 (unread in "a") and s1, s2
    /// (read, in "b") starred; "fav" is a favorite room.
    fn starred_ws(mode: ThreadsMode) -> Workspace {
        let mut ws = threads_ws(mode);
        let mut fav = sub("fav", "favorite", RoomType::Channel);
        fav.f = true;
        ws.apply_subscription(Change::Inserted, fav);
        ws.set_starred_threads(true);
        ws.apply_thread_parents(vec![
            star(parent("t1", "a", 300, "bob")),
            star(parent("s1", "b", 200, "carol")),
            star(parent("s2", "b", 400, "bob")),
        ]);
        ws
    }

    fn section<'s>(sections: &'s [Section], id: &str) -> &'s Section {
        sections
            .iter()
            .find(|s| s.id.as_config_str() == id)
            .unwrap()
    }

    #[test]
    fn starred_threads_join_favorites() {
        for mode in [ThreadsMode::Off, ThreadsMode::Following] {
            let ws = starred_ws(mode);
            let s = compute(&ws, &thread_opts(mode), "");
            let fav = section(&s, "favorites");
            assert_eq!(names(fav), ["favorite"], "{mode:?}");
            // Newest reply first; unread first is off in Favorites.
            assert_eq!(tmids(&fav.threads), ["s2", "t1", "s1"], "{mode:?}");
            assert!(fav.threads.iter().all(|t| t.starred));
            assert_eq!(fav.threads[1].unread, Some(BadgeVariant::Thread));
            assert_eq!(fav.len(), 4);
        }
        // A room the user left or hid: its starred threads are not listed.
        let mut ws = starred_ws(ThreadsMode::Off);
        let mut b = ws.subscription("b").unwrap().clone();
        b.open = false;
        ws.apply_subscription(Change::Updated, b);
        let s = compute(&ws, &thread_opts(ThreadsMode::Off), "");
        assert_eq!(tmids(&section(&s, "favorites").threads), ["t1"]);
    }

    #[test]
    fn starred_thread_is_not_repeated_in_threads() {
        let mut ws = starred_ws(ThreadsMode::UnreadKept);
        ws.set_thread_config(ThreadsMode::UnreadKept, [("b", "s1"), ("b", "k")]);
        let mut opts = thread_opts(ThreadsMode::UnreadKept);
        opts.kept_thread = Some(("b".into(), "s2".into()));
        opts.open_thread = Some(("b".into(), "s2".into()));
        let s = compute(&ws, &opts, "");
        assert_eq!(tmids(&section(&s, "threads").threads), ["t2", "t3", "k"]);
        assert_eq!(tmids(&section(&s, "favorites").threads), ["s2", "t1", "s1"]);
        // Unstarred: back to Threads (t1 unread, s1 kept).
        ws.set_thread_starred("a", "t1", false);
        ws.set_thread_starred("b", "s1", false);
        let s = compute(&ws, &opts, "");
        assert_eq!(
            tmids(&section(&s, "threads").threads),
            ["t1", "t2", "t3", "s1", "k"]
        );
        // The switch off: everything back in Threads.
        opts.starred_threads = false;
        let s = compute(&ws, &opts, "");
        assert_eq!(
            tmids(&section(&s, "threads").threads),
            ["s2", "t1", "t2", "t3", "s1", "k"]
        );
        assert!(section(&s, "favorites").threads.is_empty());
    }

    /// Which section shows a thread row, by Threads mode and state.
    #[test]
    fn thread_row_section_matrix() {
        let modes = [
            ThreadsMode::Off,
            ThreadsMode::Unread,
            ThreadsMode::UnreadKept,
            ThreadsMode::Following,
        ];
        for mode in modes {
            let mut ws = starred_ws(mode);
            // k: kept; ks: kept and starred; f: followed; fs: followed and
            // starred.
            ws.set_thread_config(mode, [("b", "k"), ("b", "ks")]);
            ws.apply_thread_parents(vec![
                parent("k", "b", 10, "bob"),
                star(parent("ks", "b", 10, "bob")),
            ]);
            ws.apply_followed(
                "b",
                vec![
                    parent("f", "b", 10, "me"),
                    star(parent("fs", "b", 10, "me")),
                ],
                None,
                std::time::Instant::now(),
            );
            let s = compute(&ws, &thread_opts(mode), "");
            let rows = |id: &str| {
                s.iter()
                    .find(|s| s.id.as_config_str() == id)
                    .map(|s| tmids(&s.threads).join(","))
                    .unwrap_or_default()
            };
            let threads = match mode {
                ThreadsMode::Off => "",
                ThreadsMode::Unread => "t2,t3",
                ThreadsMode::UnreadKept => "t2,t3,k",
                ThreadsMode::Following => "t2,t3,f,k",
            };
            assert_eq!(rows("threads"), threads, "{mode:?}");
            // Only Following crawls the followed sets (and learns of fs).
            let fav = match mode {
                ThreadsMode::Following => "s2,t1,s1,fs,ks",
                _ => "s2,t1,s1,ks",
            };
            assert_eq!(rows("favorites"), fav, "{mode:?}");
        }
    }

    #[test]
    fn favorites_with_only_threads_is_emitted() {
        let mut ws = starred_ws(ThreadsMode::Off);
        ws.apply_subscription(Change::Removed, sub("fav", "favorite", RoomType::Channel));
        let opts = thread_opts(ThreadsMode::Off);
        let s = compute(&ws, &opts, "");
        assert_eq!(ids(&s), ["favorites", "channels"]);
        assert!(s[0].items.is_empty());
        assert_eq!(s[0].threads.len(), 3);
        // A filter searches them like Threads rows.
        let s = compute(&ws, &opts, "beta");
        assert_eq!(ids(&s), ["favorites", "channels"]);
        assert_eq!(tmids(&s[0].threads), ["s2", "s1"]);
        assert_eq!(ids(&compute(&ws, &opts, "nothing")), Vec::<String>::new());
    }

    #[test]
    fn favorites_sort_blocks() {
        let ws = starred_ws(ThreadsMode::Off);
        let with = |sort: Option<SidebarSort>, unread_first: Option<bool>| {
            let mut o = thread_opts(ThreadsMode::Off);
            o.layout = layout(|p| {
                for e in [
                    crate::sidebar_layout::LayoutEdit::SetSort {
                        id: SectionKind::Favorites.into(),
                        sort,
                    },
                    crate::sidebar_layout::LayoutEdit::SetUnreadFirst {
                        id: SectionKind::Favorites.into(),
                        on: unread_first,
                    },
                ] {
                    crate::sidebar_layout::apply(p, e);
                }
            });
            o
        };
        let fav = |o: &RoomListOptions| {
            let s = compute(&ws, o, "");
            let f = section(&s, "favorites");
            (names(f).len(), tmids(&f.threads).join(","))
        };
        assert_eq!(fav(&with(None, None)), (1, "s2,t1,s1".into()));
        assert_eq!(
            fav(&with(Some(SidebarSort::ActivityAsc), None)),
            (1, "s1,t1,s2".into())
        );
        // By room name ("alpha" < "beta"), then title.
        assert_eq!(
            fav(&with(Some(SidebarSort::Alphabetical), None)),
            (1, "t1,s1,s2".into())
        );
        assert_eq!(fav(&with(None, Some(true))), (1, "t1,s2,s1".into()));
    }

    #[test]
    fn starred_threads_hidden_when_starring_disallowed() {
        for (id, threads) in [("Message_AllowStarring", "t1"), ("Threads_enabled", "")] {
            let mut ws = starred_ws(ThreadsMode::Unread);
            ws.apply_setting(
                Change::Updated,
                rocketchat::models::Setting {
                    id: id.into(),
                    value: serde_json::json!(false),
                    ..Default::default()
                },
            );
            let s = compute(&ws, &thread_opts(ThreadsMode::Unread), "");
            assert!(section(&s, "favorites").threads.is_empty(), "{id}");
            // Back in Threads (while threads are on at all).
            let rows = s
                .iter()
                .find(|s| s.is(SectionKind::Threads))
                .map(|s| tmids(&s.threads).join(","))
                .unwrap_or_default();
            assert!(rows.starts_with(threads), "{id}: {rows}");
        }
    }

    // --- flags ---

    fn flag_edits(ws: &mut Workspace, edits: &[crate::flags::FlagEdit]) {
        let mut overlay = ws.flag_overlay().clone();
        for e in edits {
            overlay.edit(ws.flag_blob(), e, 1_000, "dev").unwrap();
        }
        ws.set_flag_overlay(overlay);
    }

    fn flag_room(rid: &str, c: Option<FlagColor>) -> crate::flags::FlagEdit {
        crate::flags::FlagEdit::Room {
            rid: rid.into(),
            color: c,
        }
    }

    fn flag_thread(rid: &str, tmid: &str, c: Option<FlagColor>) -> crate::flags::FlagEdit {
        crate::flags::FlagEdit::Thread {
            rid: rid.into(),
            tmid: tmid.into(),
            color: c,
        }
    }

    fn setting(ws: &mut Workspace, id: &str, on: bool) {
        ws.apply_setting(
            Change::Updated,
            rocketchat::models::Setting {
                id: id.into(),
                value: serde_json::json!(on),
                ..Default::default()
            },
        );
    }

    /// The user's service room, open (as before it is hidden), with unread
    /// messages and `flags` stored.
    fn service_sub(rid: &str, flags: serde_json::Value) -> Subscription {
        let mut s = sub(rid, "rustket-sync-me", RoomType::Private);
        s.roles = Some(vec!["owner".into()]);
        s.unread = 3;
        s.user_mentions = 1;
        let mut f = rocketchat::models::ServiceFields::new("me");
        f.set(rocketchat::models::FLAGS_KEY, flags);
        s.custom_fields = Some(f.to_custom_fields());
        s
    }

    #[test]
    fn flag_wins_over_favorites_and_groups() {
        use FlagColor::{Blue, Red};
        let mut ws = starred_ws(ThreadsMode::Following);
        flag_edits(
            &mut ws,
            &[
                flag_room("fav", Some(Red)),
                flag_room("a", Some(Red)),
                flag_room("b", Some(Blue)),
                flag_thread("a", "t1", Some(Red)),
                flag_thread("b", "s1", Some(Blue)),
                crate::flags::FlagEdit::Rename {
                    color: Red,
                    name: Some("Срочно".into()),
                },
            ],
        );
        let mut p = UiPrefs {
            sidebar_threads: ThreadsMode::Following,
            ..Default::default()
        };
        add_group(&mut p, "Work", &[GroupType::Channels]);
        crate::sidebar_layout::sanitize_prefs(&mut p);
        let opts = RoomListOptions {
            layout: Layout::from_prefs(&p),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        // Favorites keeps the starred thread that is not flagged; the group
        // has no room left.
        assert_eq!(ids(&s), ["favorites", "flag:red", "flag:blue", "threads"]);
        let fav = section(&s, "favorites");
        assert!(fav.items.is_empty());
        assert_eq!(tmids(&fav.threads), ["s2"]);
        let red = section(&s, "flag:red");
        assert_eq!(red.title.as_deref(), Some("Срочно"));
        assert_eq!(names(red), ["alpha", "favorite"]);
        assert!(red.items.iter().all(|i| i.flag == Some(Red)));
        assert_eq!(tmids(&red.threads), ["t1"]);
        assert_eq!(red.threads[0].flag, Some(Red));
        assert!(red.threads[0].starred);
        let blue = section(&s, "flag:blue");
        assert_eq!(blue.title, None);
        assert_eq!(names(blue), ["beta"]);
        assert_eq!(tmids(&blue.threads), ["s1"]);
        // Flagged threads leave Threads too.
        assert_eq!(tmids(&section(&s, "threads").threads), ["t2", "t3"]);
        // Unflagged: back where they were.
        flag_edits(
            &mut ws,
            &[
                flag_room("fav", None),
                flag_thread("a", "t1", None),
                crate::flags::FlagEdit::ClearColor(Blue),
            ],
        );
        let s = compute(&ws, &opts, "");
        assert_eq!(
            ids(&s),
            [
                "favorites",
                "flag:red",
                "threads",
                &format!("group:{}", p.sidebar_groups[0].id)
            ]
        );
        assert_eq!(names(section(&s, "favorites")), ["favorite"]);
        assert_eq!(tmids(&section(&s, "favorites").threads), ["s2", "t1", "s1"]);
        assert_eq!(names(section(&s, "flag:red")), ["alpha"]);
    }

    #[test]
    fn unread_wins_over_flag() {
        let mut a = sub("a", "alpha", RoomType::Channel);
        a.unread = 2;
        let mut ws = ws_with(vec![(a, None), (sub("b", "beta", RoomType::Channel), None)]);
        flag_edits(
            &mut ws,
            &[
                flag_room("a", Some(FlagColor::Green)),
                flag_room("b", Some(FlagColor::Green)),
            ],
        );
        let opts = RoomListOptions {
            unread_on_top: true,
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(ids(&s), ["unread", "flag:green"]);
        assert_eq!(s[0].items[0].flag, Some(FlagColor::Green));
        assert_eq!(names(&s[1]), ["beta"]);
        // A filter keeps the routing.
        let s = compute(&ws, &RoomListOptions::default(), "bet");
        assert_eq!(ids(&s), ["flag:green"]);
    }

    #[test]
    fn flag_section_sort_and_unread_first() {
        let mut a = sub("a", "alpha", RoomType::Channel);
        a.unread = 1;
        let ws_rooms = vec![
            (a, Some(with_lm(room("a", "alpha", RoomType::Channel), 100))),
            (
                sub("b", "beta", RoomType::Channel),
                Some(with_lm(room("b", "beta", RoomType::Channel), 300)),
            ),
            (
                sub("c", "gamma", RoomType::Channel),
                Some(with_lm(room("c", "gamma", RoomType::Channel), 200)),
            ),
        ];
        let mut ws = ws_with(ws_rooms);
        flag_edits(
            &mut ws,
            &["a", "b", "c"].map(|r| flag_room(r, Some(FlagColor::Yellow))),
        );
        let id = SectionId::Flag(FlagColor::Yellow);
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(
            names(section(&s, "flag:yellow")),
            ["beta", "gamma", "alpha"]
        );
        let opts = RoomListOptions {
            layout: layout(|p| {
                p.sidebar_section_prefs.insert(
                    id.as_config_str(),
                    crate::sidebar_layout::SectionPrefs {
                        sort: Some(SidebarSort::Alphabetical),
                        unread_first: Some(true),
                    },
                );
            }),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(
            names(section(&s, "flag:yellow")),
            ["alpha", "beta", "gamma"]
        );
        let opts = RoomListOptions {
            layout: layout(|p| {
                p.sidebar_section_prefs.insert(
                    id.as_config_str(),
                    crate::sidebar_layout::SectionPrefs {
                        sort: None,
                        unread_first: Some(true),
                    },
                );
            }),
            ..Default::default()
        };
        let s = compute(&ws, &opts, "");
        assert_eq!(
            names(section(&s, "flag:yellow")),
            ["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn flagged_threads_with_threads_off() {
        let mut ws = threads_ws(ThreadsMode::Off);
        assert!(!ws.thread_index().indexing());
        flag_edits(
            &mut ws,
            &[
                flag_thread("b", "g", Some(FlagColor::Purple)),
                flag_thread("a", "t1", Some(FlagColor::Purple)),
            ],
        );
        assert!(ws.thread_index().indexing() && ws.thread_index().is_flagged("g"));
        // Parents of flagged threads are stored with the section off.
        ws.apply_thread_parents(vec![
            parent("t1", "a", 300, "bob"),
            parent("x", "a", 1, "bob"),
        ]);
        assert!(ws.thread_index().parent("t1").is_some());
        assert!(ws.thread_index().parent("x").is_none());
        let opts = thread_opts(ThreadsMode::Off);
        let s = compute(&ws, &opts, "");
        assert_eq!(ids(&s), ["flag:purple", "channels"]);
        let rows = &s[0].threads;
        assert_eq!(tmids(rows), ["t1", "g"]);
        assert_eq!(rows[1].title, ThreadTitle::Loading);
        assert_eq!(rows[0].unread, Some(BadgeVariant::Thread));
        // Gone: still listed (the user clears the flag).
        ws.thread_gone("g");
        let s = compute(&ws, &opts, "");
        assert_eq!(s[0].threads[1].title, ThreadTitle::Unavailable);
        assert_eq!(ws.thread_flag("g"), Some(FlagColor::Purple));
        // A filter searches flagged threads too.
        let s = compute(&ws, &opts, "text t1");
        assert_eq!(tmids(&section(&s, "flag:purple").threads), ["t1"]);
        // Threads disabled on the server: no flagged thread rows anywhere.
        setting(&mut ws, "Threads_enabled", false);
        let s = compute(&ws, &thread_opts(ThreadsMode::Unread), "");
        assert_eq!(ids(&s), ["channels"]);
        let usage = flag_usage(&ws);
        assert_eq!(usage.get(&FlagColor::Purple), Some(&(0, 2)));
    }

    #[test]
    fn flags_outside_the_list() {
        let mut ws = mixed_ws();
        let (omni, channel) = ("l", "c");
        flag_edits(
            &mut ws,
            &[
                flag_room(omni, Some(FlagColor::Red)),
                flag_room(channel, Some(FlagColor::Red)),
                flag_room("left", Some(FlagColor::Red)),
                flag_thread(omni, "ot", Some(FlagColor::Blue)),
            ],
        );
        let s = compute(&ws, &RoomListOptions::default(), "");
        // Omnichannel rooms ignore flags; rooms the user is not in are
        // not listed.
        let omni_section = section(&s, "omnichannel");
        assert!(omni_section.items.iter().all(|i| i.flag.is_none()));
        assert!(omni_section.items.iter().any(|i| i.rid == omni));
        let red = section(&s, "flag:red");
        assert_eq!(red.items.len(), 1);
        assert_eq!(red.items[0].rid, channel);
        assert!(s.iter().all(|x| x.id != SectionId::Flag(FlagColor::Blue)));
        let usage = flag_usage(&ws);
        assert_eq!(usage.get(&FlagColor::Red), Some(&(1, 2)));
        assert_eq!(usage.get(&FlagColor::Blue), Some(&(0, 1)));
        // Hidden: kept, not listed.
        let mut hidden = ws.subscription(channel).unwrap().clone();
        hidden.open = false;
        ws.apply_subscription(Change::Updated, hidden);
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert!(s.iter().all(|x| x.id != SectionId::Flag(FlagColor::Red)));
        assert_eq!(ws.room_flag(channel), Some(FlagColor::Red));
    }

    #[test]
    fn service_rooms_hold_flags_and_are_never_listed() {
        let stored = serde_json::json!({
            "v": 1,
            "rooms": { "a": { "c": "orange", "t": 5, "d": "x" } },
            "names": { "orange": { "n": "Ждёт ответа", "t": 5 } },
        });
        let svc = service_sub("svc", stored);
        let svc_room = crate::test_util::service_room(&svc, "me", 1);
        let mut ws = ws_with(vec![
            (sub("a", "alpha", RoomType::Channel), None),
            (svc, Some(svc_room)),
        ]);
        assert_eq!(ws.service_rooms(), ["svc"]);
        assert!(ws.is_service_room("svc") && !ws.is_service_room("a"));
        assert_eq!(ws.room_flag("a"), Some(FlagColor::Orange));
        assert_eq!(ws.flags().name(FlagColor::Orange), Some("Ждёт ответа"));
        let s = compute(&ws, &RoomListOptions::default(), "");
        assert_eq!(ids(&s), ["flag:orange"]);
        assert_eq!(s[0].title.as_deref(), Some("Ждёт ответа"));
        assert!(compute(&ws, &RoomListOptions::default(), "rustket").is_empty());
        assert_eq!(unread_totals(&ws), (0, 0));
        // A marked room someone else created is an ordinary room, even when
        // the user was made its owner: its data is not trusted.
        let forged = service_sub(
            "forged",
            serde_json::json!({ "rooms": { "a": { "c": "red", "t": i64::MAX } } }),
        );
        let forged_room = crate::test_util::service_room(&forged, "bob", 2);
        ws.apply_subscription(Change::Inserted, forged.clone());
        ws.apply_room(Change::Inserted, forged_room.clone());
        assert_eq!(ws.room_flag("a"), Some(FlagColor::Orange));
        assert!(!ws.is_service_room("forged"));
        assert_eq!(ws.service_rooms(), ["svc"]);
        assert_eq!(unread_totals(&ws), (1, 1));
        assert!(
            compute(&ws, &RoomListOptions::default(), "")
                .iter()
                .any(|x| x.items.iter().any(|i| i.rid == "forged"))
        );
        // Still not once its creator has left.
        ws.apply_room(
            Change::Updated,
            Room {
                users_count: Some(1),
                ..forged_room
            },
        );
        assert!(!ws.is_service_room("forged"));
        assert_eq!(ws.room_flag("a"), Some(FlagColor::Orange));
        ws.apply_subscription(Change::Removed, forged);
        // A live write arrives as a subscription change.
        let rev = ws.revision(Dirty::FLAGS);
        let mut svc = ws.subscription("svc").unwrap().clone();
        let mut f = svc.service_fields().unwrap();
        f.set(
            rocketchat::models::FLAGS_KEY,
            serde_json::json!({ "rooms": { "a": { "c": null, "t": 6 } } }),
        );
        svc.custom_fields = Some(f.to_custom_fields());
        ws.apply_subscription(Change::Updated, svc);
        assert!(ws.revision(Dirty::FLAGS) > rev);
        assert_eq!(ws.room_flag("a"), None);
        assert_eq!(
            ids(&compute(&ws, &RoomListOptions::default(), "")),
            ["channels"]
        );
        // Unrelated changes leave the flags alone.
        let rev = ws.revision(Dirty::FLAGS);
        let mut a = ws.subscription("a").unwrap().clone();
        a.unread = 4;
        ws.apply_subscription(Change::Updated, a);
        assert_eq!(ws.revision(Dirty::FLAGS), rev);
        // Leaving the service room drops its flags.
        let svc = ws.subscription("svc").unwrap().clone();
        let mut svc2 = svc.clone();
        let mut f = svc.service_fields().unwrap();
        f.set(
            rocketchat::models::FLAGS_KEY,
            serde_json::json!({ "rooms": { "a": { "c": "blue", "t": 7 } } }),
        );
        svc2.custom_fields = Some(f.to_custom_fields());
        ws.apply_subscription(Change::Updated, svc2.clone());
        assert_eq!(ws.room_flag("a"), Some(FlagColor::Blue));
        ws.apply_subscription(Change::Removed, svc2);
        assert!(ws.service_rooms().is_empty());
        assert_eq!(ws.room_flag("a"), None);
    }
}
