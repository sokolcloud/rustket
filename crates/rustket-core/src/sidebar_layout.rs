//! Sidebar layout: section identities, user-defined groups, per-section
//! preferences, the resolved [`Layout`] and the [`apply`] reducer that every
//! layout editor (Settings, header menu, drag-and-drop) goes through. Pure.
//!
//! The persisted parts live in [`UiPrefs`] (global) and
//! [`crate::config::ServerEntry::kept_threads()`] (per server and account).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::config::{MAX_SIDEBAR_THREADS_LIMIT, SidebarSort, UiPrefs, lenient_opt, lenient_vec};
use crate::flags::FlagColor;
use crate::room_list::{RoomKind, SectionKind};

/// Most user-defined groups kept.
pub const MAX_GROUPS: usize = 16;
/// Longest group name in characters.
pub const MAX_GROUP_NAME_CHARS: usize = 40;
/// Most kept threads stored per server entry.
pub const MAX_KEPT_THREADS: usize = 200;
/// Most starred thread rows the Favorites section shows (no row limit
/// applies otherwise).
pub const MAX_STARRED_THREADS: usize = 500;
/// Thread limits the settings and the Threads header menu offer (0: none).
pub const THREADS_LIMIT_CHOICES: [u32; 6] = [5, 10, 20, 50, 100, 0];

/// [`THREADS_LIMIT_CHOICES`] plus `current` when it is another value
/// (set in the config file), ascending with 0 (no limit) last.
pub fn threads_limit_choices(current: u32) -> Vec<u32> {
    let mut v = THREADS_LIMIT_CHOICES.to_vec();
    if !v.contains(&current) {
        v.push(current);
        v.sort_by_key(|n| if *n == 0 { u32::MAX } else { *n });
    }
    v
}

const GROUP_PREFIX: &str = "group:";
const GROUP_COLLAPSE_PREFIX: &str = "sidebar.group.";
const FLAG_PREFIX: &str = "flag:";

/// Default order of the orderable built-in sections. Omnichannel is not
/// orderable: it is always rendered on top.
pub const DEFAULT_ORDER: [SectionKind; 9] = [
    SectionKind::Unread,
    SectionKind::Favorites,
    SectionKind::Threads,
    SectionKind::Teams,
    SectionKind::Discussions,
    SectionKind::Channels,
    SectionKind::PrivateGroups,
    SectionKind::DirectMessages,
    SectionKind::Conversations,
];

/// Persisted identity of a sidebar section; also the source of its collapse key.
/// Serialized as a string: `"unread"`, `"favorites"`, `"threads"`, `"teams"`,
/// `"discussions"`, `"channels"`, `"private_groups"`, `"direct_messages"`,
/// `"conversations"`, `"omnichannel"`, `"group:<id>"`, `"flag:<color>"`;
/// anything else is [`SectionId::Unknown`] (kept in the stored order,
/// otherwise ignored).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum SectionId {
    Builtin(SectionKind),
    Group(String),
    /// Rooms and threads carrying a flag of this color.
    Flag(FlagColor),
    Unknown(String),
}

fn builtin_str(k: SectionKind) -> &'static str {
    match k {
        SectionKind::Unread => "unread",
        SectionKind::Favorites => "favorites",
        SectionKind::Threads => "threads",
        SectionKind::Teams => "teams",
        SectionKind::Discussions => "discussions",
        SectionKind::Channels => "channels",
        SectionKind::PrivateGroups => "private_groups",
        SectionKind::DirectMessages => "direct_messages",
        SectionKind::Conversations => "conversations",
        SectionKind::Omnichannel => "omnichannel",
    }
}

const ALL_KINDS: [SectionKind; 10] = [
    SectionKind::Unread,
    SectionKind::Favorites,
    SectionKind::Threads,
    SectionKind::Teams,
    SectionKind::Discussions,
    SectionKind::Channels,
    SectionKind::PrivateGroups,
    SectionKind::DirectMessages,
    SectionKind::Conversations,
    SectionKind::Omnichannel,
];

impl SectionId {
    pub fn group(id: impl Into<String>) -> Self {
        Self::Group(id.into())
    }

    pub fn as_config_str(&self) -> String {
        match self {
            Self::Builtin(k) => builtin_str(*k).to_owned(),
            Self::Group(id) => format!("{GROUP_PREFIX}{id}"),
            Self::Flag(c) => format!("{FLAG_PREFIX}{}", c.as_str()),
            Self::Unknown(s) => s.clone(),
        }
    }

    /// Key in `ui_state.collapsed_sections`: built-ins keep their i18n key
    /// (`sidebar.<x>`, as before groups existed), groups use
    /// `sidebar.group.<id>`, flags `sidebar.flag.<color>`.
    pub fn collapse_key(&self) -> Cow<'static, str> {
        match self {
            Self::Builtin(k) => Cow::Borrowed(k.i18n_key()),
            Self::Group(id) => Cow::Owned(format!("{GROUP_COLLAPSE_PREFIX}{id}")),
            Self::Flag(c) => Cow::Borrowed(c.i18n_key()),
            Self::Unknown(s) => Cow::Owned(format!("sidebar.unknown.{s}")),
        }
    }

    /// The i18n key of the default title (built-ins and flags).
    pub fn i18n_key(&self) -> Option<&'static str> {
        match self {
            Self::Builtin(k) => Some(k.i18n_key()),
            Self::Flag(c) => Some(c.i18n_key()),
            _ => None,
        }
    }

    pub fn flag(&self) -> Option<FlagColor> {
        match self {
            Self::Flag(c) => Some(*c),
            _ => None,
        }
    }

    pub fn kind(&self) -> Option<SectionKind> {
        match self {
            Self::Builtin(k) => Some(*k),
            _ => None,
        }
    }

    pub fn group_id(&self) -> Option<&str> {
        match self {
            Self::Group(id) => Some(id),
            _ => None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl From<SectionKind> for SectionId {
    fn from(k: SectionKind) -> Self {
        Self::Builtin(k)
    }
}

impl From<String> for SectionId {
    fn from(s: String) -> Self {
        if let Some(k) = ALL_KINDS.into_iter().find(|k| builtin_str(*k) == s) {
            return Self::Builtin(k);
        }
        if let Some(c) = s.strip_prefix(FLAG_PREFIX).and_then(FlagColor::parse) {
            return Self::Flag(c);
        }
        match s.strip_prefix(GROUP_PREFIX) {
            Some(id) if !id.is_empty() => Self::Group(id.to_owned()),
            _ => Self::Unknown(s),
        }
    }
}

impl From<SectionId> for String {
    fn from(id: SectionId) -> Self {
        id.as_config_str()
    }
}

/// A room type a [`SidebarGroup`] can claim. Omnichannel rooms are never
/// grouped (their block stays on top); an `"omnichannel"` value in a stored
/// group is skipped like any unknown type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupType {
    Teams,
    Discussions,
    Channels,
    PrivateGroups,
    DirectMessages,
}

impl GroupType {
    pub const ALL: [GroupType; 5] = [
        Self::Teams,
        Self::Discussions,
        Self::Channels,
        Self::PrivateGroups,
        Self::DirectMessages,
    ];

    /// The built-in section rooms of this type go to when no group claims them.
    pub fn section(self) -> SectionKind {
        match self {
            Self::Teams => SectionKind::Teams,
            Self::Discussions => SectionKind::Discussions,
            Self::Channels => SectionKind::Channels,
            Self::PrivateGroups => SectionKind::PrivateGroups,
            Self::DirectMessages => SectionKind::DirectMessages,
        }
    }

    pub fn of_room(kind: RoomKind) -> Option<Self> {
        match kind {
            RoomKind::Team { .. } => Some(Self::Teams),
            RoomKind::Discussion => Some(Self::Discussions),
            RoomKind::Channel => Some(Self::Channels),
            RoomKind::Private => Some(Self::PrivateGroups),
            RoomKind::Direct | RoomKind::GroupDirect => Some(Self::DirectMessages),
            RoomKind::Omnichannel => None,
        }
    }

    pub fn i18n_key(self) -> &'static str {
        self.section().i18n_key()
    }
}

/// A user-defined sidebar section that takes the rooms of its types.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarGroup {
    /// Random and stable (`rocketchat::ids::random_id`).
    pub id: String,
    /// User text (trimmed, at most [`MAX_GROUP_NAME_CHARS`]); not translated.
    pub name: String,
    /// Exclusive between groups; may be empty (the group then shows no rooms).
    #[serde(deserialize_with = "lenient_vec")]
    pub types: Vec<GroupType>,
}

/// Per-section overrides; `None` means the default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionPrefs {
    /// `None`: the global `ui.sidebar_sort`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_opt"
    )]
    pub sort: Option<SidebarSort>,
    /// `None`: on for Threads, off otherwise.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_opt"
    )]
    pub unread_first: Option<bool>,
}

impl SectionPrefs {
    fn is_empty(&self) -> bool {
        self.sort.is_none() && self.unread_first.is_none()
    }
}

/// Which threads the sidebar's Threads section lists. An unknown stored value
/// (from a newer version) reads as `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadsMode {
    #[default]
    Off,
    Unread,
    /// Unread plus threads the user kept in the sidebar.
    UnreadKept,
    /// Unread, kept and every followed thread.
    Following,
}

/// A thread kept in the sidebar, owned by the account (`user_id`) that kept it;
/// entries of another account on the same server entry are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeptThread {
    pub rid: String,
    pub tmid: String,
    pub user_id: String,
}

/// The layout resolved from [`UiPrefs`]: complete order without unknown ids,
/// exclusive type claims, resolved sorts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Layout {
    /// Effective order: complete, deduplicated, no `Unknown`.
    pub order: Vec<SectionId>,
    /// `(id, name)` in display order.
    pub groups: Vec<(String, String)>,
    /// Room type -> id of the group that takes it.
    pub claims: BTreeMap<GroupType, String>,
    pub default_sort: SidebarSort,
    /// Resolved sort of every section in `order`.
    pub sorts: BTreeMap<SectionId, SidebarSort>,
    /// Sections that list unread entries first.
    pub unread_first: BTreeSet<SectionId>,
    pub threads: ThreadsMode,
    /// Most rows of the Threads section; 0 means no limit.
    pub threads_limit: u32,
}

impl Default for Layout {
    fn default() -> Self {
        Self::from_prefs(&UiPrefs::default())
    }
}

impl Layout {
    pub fn from_prefs(p: &UiPrefs) -> Self {
        let order = effective_order(p);
        let groups: Vec<(String, String)> = order
            .iter()
            .filter_map(SectionId::group_id)
            .filter_map(|id| p.sidebar_groups.iter().find(|g| g.id == id))
            .map(|g| (g.id.clone(), g.name.clone()))
            .collect();
        let mut claims = BTreeMap::new();
        for (id, _) in &groups {
            if let Some(g) = p.sidebar_groups.iter().find(|g| &g.id == id) {
                for ty in &g.types {
                    claims.entry(*ty).or_insert_with(|| id.clone());
                }
            }
        }
        let mut sorts = BTreeMap::new();
        let mut unread_first = BTreeSet::new();
        for id in &order {
            sorts.insert(id.clone(), section_sort(p, id));
            if section_unread_first(p, id) {
                unread_first.insert(id.clone());
            }
        }
        Self {
            order,
            groups,
            claims,
            default_sort: p.sidebar_sort,
            sorts,
            unread_first,
            threads: p.sidebar_threads,
            threads_limit: p.sidebar_threads_limit,
        }
    }

    pub fn sort_of(&self, id: &SectionId) -> SidebarSort {
        self.sorts.get(id).copied().unwrap_or(self.default_sort)
    }

    pub fn is_unread_first(&self, id: &SectionId) -> bool {
        self.unread_first.contains(id)
    }

    pub fn group_name(&self, id: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|(g, _)| g == id)
            .map(|(_, n)| n.as_str())
    }

    /// Id of the group that takes rooms of `ty`.
    pub fn claimed_by(&self, ty: GroupType) -> Option<&str> {
        self.claims.get(&ty).map(String::as_str)
    }
}

/// The resolved sort of a section.
pub fn section_sort(p: &UiPrefs, id: &SectionId) -> SidebarSort {
    p.sidebar_section_prefs
        .get(&id.as_config_str())
        .and_then(|s| s.sort)
        .unwrap_or(p.sidebar_sort)
}

/// Whether a section lists unread entries first (on for Threads by default,
/// never for Unread).
pub fn section_unread_first(p: &UiPrefs, id: &SectionId) -> bool {
    if id == &SectionId::Builtin(SectionKind::Unread) {
        return false;
    }
    let default = id == &SectionId::Builtin(SectionKind::Threads);
    p.sidebar_section_prefs
        .get(&id.as_config_str())
        .and_then(|s| s.unread_first)
        .unwrap_or(default)
}

fn orderable(id: &SectionId, groups: &HashSet<&str>) -> bool {
    match id {
        SectionId::Builtin(k) => DEFAULT_ORDER.contains(k),
        SectionId::Group(g) => groups.contains(g.as_str()),
        SectionId::Flag(_) | SectionId::Unknown(_) => true,
    }
}

/// The stored order made complete: deduplicated, gone groups dropped, unknown
/// ids kept in place, missing built-ins inserted after their nearest preceding
/// [`DEFAULT_ORDER`] neighbour, missing flag sections after Favorites (in
/// palette order, after the flags already placed there), missing groups
/// appended.
fn materialized_order(p: &UiPrefs) -> Vec<SectionId> {
    let groups: HashSet<&str> = p.sidebar_groups.iter().map(|g| g.id.as_str()).collect();
    let mut seen = HashSet::new();
    let mut out: Vec<SectionId> = p
        .sidebar_order
        .iter()
        .filter(|id| orderable(id, &groups) && seen.insert(*id))
        .cloned()
        .collect();
    for (i, k) in DEFAULT_ORDER.iter().enumerate() {
        let id = SectionId::Builtin(*k);
        if out.contains(&id) {
            continue;
        }
        let at = DEFAULT_ORDER[..i]
            .iter()
            .rev()
            .find_map(|prev| out.iter().position(|x| x == &SectionId::Builtin(*prev)))
            .map_or(0, |p| p + 1);
        out.insert(at, id);
    }
    let favorites = SectionId::Builtin(SectionKind::Favorites);
    for (i, c) in FlagColor::ALL.into_iter().enumerate() {
        let id = SectionId::Flag(c);
        if out.contains(&id) {
            continue;
        }
        let at = FlagColor::ALL[..i]
            .iter()
            .rev()
            .find_map(|prev| out.iter().position(|x| x == &SectionId::Flag(*prev)))
            .or_else(|| out.iter().position(|x| x == &favorites))
            .map_or(0, |p| p + 1);
        out.insert(at, id);
    }
    for g in &p.sidebar_groups {
        let id = SectionId::Group(g.id.clone());
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The complete display order of the orderable sections (no unknown ids).
pub fn effective_order(p: &UiPrefs) -> Vec<SectionId> {
    let mut v = materialized_order(p);
    v.retain(|id| !id.is_unknown());
    v
}

/// Stores `order` (a permutation of [`effective_order`]) with unknown ids kept
/// at their slots. Leaves the stored order alone when nothing moved.
fn store_order(p: &mut UiPrefs, order: Vec<SectionId>) {
    if effective_order(p) == order {
        return;
    }
    let mut m = materialized_order(p);
    let mut it = order.into_iter();
    for slot in m.iter_mut().filter(|x| !x.is_unknown()) {
        if let Some(id) = it.next() {
            *slot = id;
        }
    }
    p.sidebar_order = m;
}

fn clean_name(name: &str) -> Option<String> {
    let n: String = name.trim().chars().take(MAX_GROUP_NAME_CHARS).collect();
    let n = n.trim_end().to_owned();
    (!n.is_empty()).then_some(n)
}

fn new_group_id(p: &UiPrefs) -> String {
    loop {
        let id = rocketchat::ids::random_id();
        if p.sidebar_groups.iter().all(|g| g.id != id) {
            return id;
        }
    }
}

fn edit_section_prefs(p: &mut UiPrefs, id: &SectionId, f: impl FnOnce(&mut SectionPrefs)) {
    let key = id.as_config_str();
    let mut sp = p.sidebar_section_prefs.remove(&key).unwrap_or_default();
    f(&mut sp);
    if !sp.is_empty() {
        p.sidebar_section_prefs.insert(key, sp);
    }
}

/// An edit of the sidebar layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutEdit {
    /// Moves by `delta` places in the full effective order (Settings).
    Move {
        id: SectionId,
        delta: i32,
    },
    /// Moves next to `anchor` (header menu, drag-and-drop: visible neighbours).
    MoveTo {
        id: SectionId,
        anchor: SectionId,
        after: bool,
    },
    SetSort {
        id: SectionId,
        sort: Option<SidebarSort>,
    },
    SetUnreadFirst {
        id: SectionId,
        on: Option<bool>,
    },
    SetDefaultSort(SidebarSort),
    SetThreadsMode(ThreadsMode),
    /// 0 means no limit; clamped to [`MAX_SIDEBAR_THREADS_LIMIT`].
    SetThreadsLimit(u32),
    SetUnreadOnTop(bool),
    /// Placed before the earliest section of its types (else last); takes the
    /// types away from other groups.
    AddGroup {
        name: String,
        types: Vec<GroupType>,
    },
    RenameGroup {
        id: String,
        name: String,
    },
    /// Turning a type on takes it away from other groups.
    SetGroupType {
        id: String,
        ty: GroupType,
        on: bool,
    },
    DeleteGroup {
        id: String,
    },
    /// Clears the order, groups and section preferences.
    Reset,
}

/// Result of [`apply`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// The prefs changed (save the config).
    pub changed: bool,
    /// Collapse keys of removed sections (drop them from the UI state).
    pub dropped_collapse_keys: Vec<String>,
}

/// Applies a layout edit to `p`.
pub fn apply(p: &mut UiPrefs, e: LayoutEdit) -> Applied {
    let before = p.clone();
    let mut dropped = Vec::new();
    match e {
        LayoutEdit::Move { id, delta } => {
            let mut order = effective_order(p);
            if let Some(i) = order.iter().position(|x| x == &id) {
                let last = order.len().saturating_sub(1) as i64;
                let j = (i as i64 + i64::from(delta)).clamp(0, last) as usize;
                let x = order.remove(i);
                order.insert(j, x);
                store_order(p, order);
            }
        }
        LayoutEdit::MoveTo { id, anchor, after } => {
            let mut order = effective_order(p);
            if id != anchor
                && order.contains(&anchor)
                && let Some(i) = order.iter().position(|x| x == &id)
            {
                let x = order.remove(i);
                if let Some(a) = order.iter().position(|x| x == &anchor) {
                    order.insert(a + usize::from(after), x);
                    store_order(p, order);
                }
            }
        }
        LayoutEdit::SetSort { id, sort } => {
            if !id.is_unknown() {
                edit_section_prefs(p, &id, |sp| sp.sort = sort);
            }
        }
        LayoutEdit::SetUnreadFirst { id, on } => {
            if !id.is_unknown() && id != SectionId::Builtin(SectionKind::Unread) {
                edit_section_prefs(p, &id, |sp| sp.unread_first = on);
            }
        }
        LayoutEdit::SetDefaultSort(s) => p.sidebar_sort = s,
        LayoutEdit::SetThreadsMode(m) => p.sidebar_threads = m,
        LayoutEdit::SetThreadsLimit(n) => {
            p.sidebar_threads_limit = n.min(MAX_SIDEBAR_THREADS_LIMIT);
        }
        LayoutEdit::SetUnreadOnTop(on) => p.unread_on_top = on,
        LayoutEdit::AddGroup { name, mut types } => {
            if let Some(name) = clean_name(&name)
                && p.sidebar_groups.len() < MAX_GROUPS
            {
                types.sort();
                types.dedup();
                let mut order = effective_order(p);
                for g in &mut p.sidebar_groups {
                    g.types.retain(|t| !types.contains(t));
                }
                let at = order
                    .iter()
                    .position(|x| {
                        x.kind()
                            .is_some_and(|k| types.iter().any(|t| t.section() == k))
                    })
                    .unwrap_or(order.len());
                let id = new_group_id(p);
                p.sidebar_groups.push(SidebarGroup {
                    id: id.clone(),
                    name,
                    types,
                });
                order.insert(at, SectionId::Group(id));
                store_order(p, order);
            }
        }
        LayoutEdit::RenameGroup { id, name } => {
            if let Some(name) = clean_name(&name)
                && let Some(g) = p.sidebar_groups.iter_mut().find(|g| g.id == id)
            {
                g.name = name;
            }
        }
        LayoutEdit::SetGroupType { id, ty, on } => {
            if p.sidebar_groups.iter().any(|g| g.id == id) {
                for g in &mut p.sidebar_groups {
                    if g.id == id {
                        if on && !g.types.contains(&ty) {
                            g.types.push(ty);
                            g.types.sort();
                        } else if !on {
                            g.types.retain(|t| *t != ty);
                        }
                    } else if on {
                        g.types.retain(|t| *t != ty);
                    }
                }
            }
        }
        LayoutEdit::DeleteGroup { id } => {
            if let Some(i) = p.sidebar_groups.iter().position(|g| g.id == id) {
                p.sidebar_groups.remove(i);
                let sid = SectionId::Group(id);
                p.sidebar_order.retain(|x| x != &sid);
                p.sidebar_section_prefs.remove(&sid.as_config_str());
                dropped.push(sid.collapse_key().into_owned());
            }
        }
        LayoutEdit::Reset => {
            dropped = p
                .sidebar_groups
                .iter()
                .map(|g| SectionId::Group(g.id.clone()).collapse_key().into_owned())
                .collect();
            p.sidebar_order.clear();
            p.sidebar_groups.clear();
            p.sidebar_section_prefs.clear();
        }
    }
    Applied {
        changed: *p != before,
        dropped_collapse_keys: dropped,
    }
}

/// Repairs loaded sidebar prefs: group names cleaned (empty ones dropped),
/// empty or duplicate group ids replaced, at most [`MAX_GROUPS`] groups, type
/// claims made exclusive in display order, the stored order deduplicated
/// (unknown ids kept), entries and section prefs of gone groups dropped.
pub(crate) fn sanitize_prefs(p: &mut UiPrefs) {
    let mut ids = HashSet::new();
    p.sidebar_groups.retain_mut(|g| {
        let Some(name) = clean_name(&g.name) else {
            tracing::warn!("config: sidebar group without a name ignored");
            return false;
        };
        g.name = name;
        g.types.sort();
        g.types.dedup();
        true
    });
    for i in 0..p.sidebar_groups.len() {
        if p.sidebar_groups[i].id.trim().is_empty() || !ids.insert(p.sidebar_groups[i].id.clone()) {
            tracing::warn!("config: empty or duplicate sidebar group id replaced");
            let id = new_group_id(p);
            ids.insert(id.clone());
            p.sidebar_groups[i].id = id;
        }
    }
    if p.sidebar_groups.len() > MAX_GROUPS {
        tracing::warn!(
            count = p.sidebar_groups.len(),
            "config: too many sidebar groups"
        );
        p.sidebar_groups.truncate(MAX_GROUPS);
    }
    let groups: HashSet<&str> = p.sidebar_groups.iter().map(|g| g.id.as_str()).collect();
    let mut seen = HashSet::new();
    p.sidebar_order
        .retain(|id| orderable(id, &groups) && seen.insert(id.clone()));
    // The first group in display order keeps a contested type.
    let mut claimed = HashSet::new();
    for gid in effective_order(p)
        .iter()
        .filter_map(SectionId::group_id)
        .map(str::to_owned)
        .collect::<Vec<_>>()
    {
        if let Some(g) = p.sidebar_groups.iter_mut().find(|g| g.id == gid) {
            g.types.retain(|t| claimed.insert(*t));
        }
    }
    let groups: HashSet<&str> = p.sidebar_groups.iter().map(|g| g.id.as_str()).collect();
    p.sidebar_section_prefs.retain(|k, v| {
        !v.is_empty()
            && match SectionId::from(k.clone()) {
                SectionId::Group(g) => groups.contains(g.as_str()),
                _ => true,
            }
    });
}

/// Deduplicates kept threads (per account and tmid, first entry wins), drops
/// incomplete ones and keeps the newest [`MAX_KEPT_THREADS`].
pub(crate) fn sanitize_kept_threads(v: &mut Vec<KeptThread>) {
    let mut seen = HashSet::new();
    v.retain(|k| {
        !k.rid.is_empty()
            && !k.tmid.is_empty()
            && !k.user_id.is_empty()
            && seen.insert((k.user_id.clone(), k.tmid.clone()))
    });
    if v.len() > MAX_KEPT_THREADS {
        let extra = v.len() - MAX_KEPT_THREADS;
        v.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use SectionKind::*;

    fn b(k: SectionKind) -> SectionId {
        SectionId::Builtin(k)
    }

    fn ids(v: &[&str]) -> Vec<SectionId> {
        v.iter().map(|s| SectionId::from((*s).to_owned())).collect()
    }

    fn group(id: &str, name: &str, types: &[GroupType]) -> SidebarGroup {
        SidebarGroup {
            id: id.into(),
            name: name.into(),
            types: types.to_vec(),
        }
    }

    fn flag_ids() -> Vec<SectionId> {
        FlagColor::ALL.into_iter().map(SectionId::Flag).collect()
    }

    /// The default order: the built-ins, flags after Favorites.
    fn default_ids() -> Vec<SectionId> {
        let mut v: Vec<SectionId> = DEFAULT_ORDER.iter().map(|k| b(*k)).collect();
        v.splice(2..2, flag_ids());
        v
    }

    #[test]
    fn section_id_strings() {
        for k in ALL_KINDS {
            let id = b(k);
            assert_eq!(SectionId::from(id.as_config_str()), id);
            assert_eq!(id.collapse_key(), k.i18n_key());
        }
        let g = SectionId::from("group:abc".to_owned());
        assert_eq!(g, SectionId::group("abc"));
        assert_eq!(g.collapse_key(), "sidebar.group.abc");
        assert_eq!(g.as_config_str(), "group:abc");
        for c in FlagColor::ALL {
            let f = SectionId::Flag(c);
            assert_eq!(SectionId::from(f.as_config_str()), f);
            assert_eq!(f.collapse_key(), c.i18n_key());
            assert_eq!(f.i18n_key(), Some(c.i18n_key()));
            assert_eq!(f.flag(), Some(c));
        }
        assert_eq!(
            SectionId::Flag(FlagColor::Blue).as_config_str(),
            "flag:blue"
        );
        assert_eq!(b(Favorites).i18n_key(), Some("sidebar.favorites"));
        assert_eq!(g.i18n_key(), None);
        for s in ["group:", "future", "Unread", "flag:", "flag:pink"] {
            assert_eq!(
                SectionId::from(s.to_owned()),
                SectionId::Unknown(s.to_owned())
            );
        }
    }

    #[test]
    fn default_layout_is_the_old_behaviour() {
        let p = UiPrefs::default();
        assert_eq!(effective_order(&p), default_ids());
        let l = Layout::from_prefs(&p);
        assert!(l.claims.is_empty());
        assert_eq!(l.threads, ThreadsMode::Off);
        assert_eq!(l.sort_of(&b(Channels)), SidebarSort::Activity);
        assert_eq!(l.unread_first, [b(Threads)].into());
    }

    #[test]
    fn flag_sections_default_after_favorites() {
        // Missing flags go after Favorites; placed ones keep their place,
        // and missing ones follow the nearest earlier flag.
        let p = UiPrefs {
            sidebar_order: ids(&[
                "flag:blue",
                "channels",
                "unread",
                "favorites",
                "flag:orange",
            ]),
            ..Default::default()
        };
        let order = effective_order(&p);
        assert_eq!(
            order[..9],
            ids(&[
                "flag:blue",
                "flag:purple",
                "channels",
                "private_groups",
                "direct_messages",
                "conversations",
                "unread",
                "favorites",
                "flag:red",
            ])
        );
        assert_eq!(
            order[12..],
            ids(&["flag:orange", "flag:yellow", "flag:green"])
        );
        // Flags move, sort and stay unread-first like any section.
        let mut p = UiPrefs::default();
        let red = SectionId::Flag(FlagColor::Red);
        apply(
            &mut p,
            LayoutEdit::MoveTo {
                id: red.clone(),
                anchor: b(DirectMessages),
                after: true,
            },
        );
        apply(
            &mut p,
            LayoutEdit::SetSort {
                id: red.clone(),
                sort: Some(SidebarSort::Alphabetical),
            },
        );
        apply(
            &mut p,
            LayoutEdit::SetUnreadFirst {
                id: red.clone(),
                on: Some(true),
            },
        );
        sanitize_prefs(&mut p);
        let l = Layout::from_prefs(&p);
        let at = |id: &SectionId| l.order.iter().position(|x| x == id).unwrap();
        assert_eq!(at(&red), at(&b(DirectMessages)) + 1);
        assert_eq!(l.sort_of(&red), SidebarSort::Alphabetical);
        assert!(l.is_unread_first(&red));
        assert!(!l.is_unread_first(&SectionId::Flag(FlagColor::Blue)));
        assert!(p.sidebar_section_prefs.contains_key("flag:red"));
    }

    #[test]
    fn effective_order_fills_in_missing_builtins() {
        let p = UiPrefs {
            sidebar_order: ids(&["channels", "future", "unread", "group:w", "group:gone"]),
            sidebar_groups: vec![group("w", "Work", &[])],
            ..Default::default()
        };
        assert_eq!(
            effective_order(&p),
            ids(&[
                "channels",
                "private_groups",
                "direct_messages",
                "conversations",
                "unread",
                "favorites",
                "flag:red",
                "flag:orange",
                "flag:yellow",
                "flag:green",
                "flag:blue",
                "flag:purple",
                "threads",
                "teams",
                "discussions",
                "group:w",
            ])
        );
        // Groups missing from the stored order are appended.
        let p = UiPrefs {
            sidebar_groups: vec![group("w", "Work", &[])],
            ..Default::default()
        };
        assert_eq!(effective_order(&p).last(), Some(&SectionId::group("w")));
        // Omnichannel is never ordered.
        let p = UiPrefs {
            sidebar_order: ids(&["omnichannel", "channels"]),
            ..Default::default()
        };
        assert!(!effective_order(&p).contains(&b(Omnichannel)));
    }

    #[test]
    fn move_and_move_to() {
        let mut p = UiPrefs::default();
        // At the ends nothing changes and nothing is materialized.
        for e in [
            LayoutEdit::Move {
                id: b(Unread),
                delta: -1,
            },
            LayoutEdit::Move {
                id: b(Conversations),
                delta: 5,
            },
            LayoutEdit::MoveTo {
                id: b(Favorites),
                anchor: b(Unread),
                after: true,
            },
            LayoutEdit::MoveTo {
                id: b(Favorites),
                anchor: b(Favorites),
                after: true,
            },
            LayoutEdit::Move {
                id: SectionId::group("nope"),
                delta: 1,
            },
        ] {
            assert!(!apply(&mut p, e).changed);
            assert!(p.sidebar_order.is_empty());
        }
        assert!(
            apply(
                &mut p,
                LayoutEdit::Move {
                    id: b(Unread),
                    delta: 1
                }
            )
            .changed
        );
        assert_eq!(effective_order(&p)[..2], [b(Favorites), b(Unread)]);
        assert!(
            apply(
                &mut p,
                LayoutEdit::Move {
                    id: b(Teams),
                    delta: -100
                }
            )
            .changed
        );
        assert_eq!(effective_order(&p)[0], b(Teams));
        apply(
            &mut p,
            LayoutEdit::MoveTo {
                id: b(DirectMessages),
                anchor: b(Teams),
                after: false,
            },
        );
        assert_eq!(effective_order(&p)[..2], [b(DirectMessages), b(Teams)]);
        apply(
            &mut p,
            LayoutEdit::MoveTo {
                id: b(DirectMessages),
                anchor: b(Conversations),
                after: true,
            },
        );
        assert_eq!(effective_order(&p).last(), Some(&b(DirectMessages)));
    }

    #[test]
    fn edits_preserve_unknown_ids() {
        let mut p = UiPrefs {
            sidebar_order: ids(&["unread", "future_section", "favorites"]),
            ..Default::default()
        };
        apply(
            &mut p,
            LayoutEdit::Move {
                id: b(Favorites),
                delta: -1,
            },
        );
        assert_eq!(
            p.sidebar_order[..3],
            ids(&["favorites", "future_section", "unread"])
        );
        assert!(!effective_order(&p).iter().any(SectionId::is_unknown));
        assert!(Layout::from_prefs(&p).order.iter().all(|x| !x.is_unknown()));
    }

    #[test]
    fn add_group_position_and_exclusive_claims() {
        let mut p = UiPrefs::default();
        let a = apply(
            &mut p,
            LayoutEdit::AddGroup {
                name: "  Work  ".into(),
                types: vec![
                    GroupType::PrivateGroups,
                    GroupType::Channels,
                    GroupType::Channels,
                ],
            },
        );
        assert!(a.changed);
        let work = p.sidebar_groups[0].clone();
        assert_eq!(work.name, "Work");
        assert_eq!(work.types, [GroupType::Channels, GroupType::PrivateGroups]);
        let order = effective_order(&p);
        let pos = |id: &SectionId| order.iter().position(|x| x == id).unwrap();
        assert_eq!(pos(&SectionId::group(&work.id)) + 1, pos(&b(Channels)));
        // A second group takes Channels; Work keeps the rest.
        apply(
            &mut p,
            LayoutEdit::AddGroup {
                name: "x".repeat(50),
                types: vec![GroupType::Channels],
            },
        );
        let other = p.sidebar_groups[1].clone();
        assert_eq!(other.name.chars().count(), MAX_GROUP_NAME_CHARS);
        assert_eq!(p.sidebar_groups[0].types, [GroupType::PrivateGroups]);
        // Taking its last type keeps the group (it just shows no rooms).
        apply(
            &mut p,
            LayoutEdit::SetGroupType {
                id: other.id.clone(),
                ty: GroupType::PrivateGroups,
                on: true,
            },
        );
        assert!(p.sidebar_groups[0].types.is_empty());
        assert_eq!(p.sidebar_groups.len(), 2);
        let l = Layout::from_prefs(&p);
        assert_eq!(
            l.claimed_by(GroupType::PrivateGroups),
            Some(other.id.as_str())
        );
        assert_eq!(l.claimed_by(GroupType::Teams), None);
        assert_eq!(l.group_name(&work.id), Some("Work"));
        // Turning a type off.
        apply(
            &mut p,
            LayoutEdit::SetGroupType {
                id: other.id.clone(),
                ty: GroupType::Channels,
                on: false,
            },
        );
        assert_eq!(p.sidebar_groups[1].types, [GroupType::PrivateGroups]);
        // Invalid names change nothing.
        assert!(
            !apply(
                &mut p,
                LayoutEdit::AddGroup {
                    name: "   ".into(),
                    types: vec![]
                }
            )
            .changed
        );
        assert!(
            !apply(
                &mut p,
                LayoutEdit::RenameGroup {
                    id: work.id.clone(),
                    name: "".into()
                }
            )
            .changed
        );
        assert!(
            apply(
                &mut p,
                LayoutEdit::RenameGroup {
                    id: work.id.clone(),
                    name: "Job".into()
                }
            )
            .changed
        );
        assert_eq!(p.sidebar_groups[0].name, "Job");
    }

    #[test]
    fn group_count_is_bounded() {
        let mut p = UiPrefs::default();
        for i in 0..MAX_GROUPS + 2 {
            apply(
                &mut p,
                LayoutEdit::AddGroup {
                    name: format!("g{i}"),
                    types: vec![],
                },
            );
        }
        assert_eq!(p.sidebar_groups.len(), MAX_GROUPS);
    }

    #[test]
    fn delete_group_and_reset_drop_collapse_keys() {
        let mut p = UiPrefs::default();
        for name in ["A", "B"] {
            apply(
                &mut p,
                LayoutEdit::AddGroup {
                    name: name.into(),
                    types: vec![GroupType::Teams],
                },
            );
        }
        let (a, bid) = (
            p.sidebar_groups[0].id.clone(),
            p.sidebar_groups[1].id.clone(),
        );
        apply(
            &mut p,
            LayoutEdit::SetSort {
                id: SectionId::group(&a),
                sort: Some(SidebarSort::Alphabetical),
            },
        );
        assert_eq!(p.sidebar_section_prefs.len(), 1);
        let r = apply(&mut p, LayoutEdit::DeleteGroup { id: a.clone() });
        assert!(r.changed);
        assert_eq!(r.dropped_collapse_keys, [format!("sidebar.group.{a}")]);
        assert!(p.sidebar_section_prefs.is_empty());
        assert!(!p.sidebar_order.contains(&SectionId::group(&a)));
        assert_eq!(
            apply(&mut p, LayoutEdit::DeleteGroup { id: a }),
            Applied::default()
        );

        apply(
            &mut p,
            LayoutEdit::SetSort {
                id: b(Channels),
                sort: Some(SidebarSort::ActivityAsc),
            },
        );
        let r = apply(&mut p, LayoutEdit::Reset);
        assert!(r.changed);
        assert_eq!(r.dropped_collapse_keys, [format!("sidebar.group.{bid}")]);
        assert!(p.sidebar_order.is_empty());
        assert!(p.sidebar_groups.is_empty());
        assert!(p.sidebar_section_prefs.is_empty());
        assert!(!apply(&mut p, LayoutEdit::Reset).changed);
    }

    #[test]
    fn section_prefs_resolve() {
        let mut p = UiPrefs::default();
        apply(
            &mut p,
            LayoutEdit::SetDefaultSort(SidebarSort::Alphabetical),
        );
        apply(
            &mut p,
            LayoutEdit::SetSort {
                id: b(Threads),
                sort: Some(SidebarSort::ActivityAsc),
            },
        );
        apply(
            &mut p,
            LayoutEdit::SetUnreadFirst {
                id: b(Threads),
                on: Some(false),
            },
        );
        apply(
            &mut p,
            LayoutEdit::SetUnreadFirst {
                id: b(Channels),
                on: Some(true),
            },
        );
        // Unread first is meaningless for Unread; unknown ids are not stored.
        for id in [b(Unread), SectionId::Unknown("x".into())] {
            assert!(!apply(&mut p, LayoutEdit::SetUnreadFirst { id, on: Some(true) }).changed);
        }
        apply(&mut p, LayoutEdit::SetThreadsMode(ThreadsMode::Following));
        apply(&mut p, LayoutEdit::SetUnreadOnTop(true));
        let l = Layout::from_prefs(&p);
        assert_eq!(l.sort_of(&b(Threads)), SidebarSort::ActivityAsc);
        assert_eq!(l.sort_of(&b(Teams)), SidebarSort::Alphabetical);
        assert_eq!(l.unread_first, [b(Channels)].into());
        assert_eq!(l.threads, ThreadsMode::Following);
        assert!(p.unread_on_top);
        // Back to the defaults removes the entries.
        apply(
            &mut p,
            LayoutEdit::SetSort {
                id: b(Threads),
                sort: None,
            },
        );
        apply(
            &mut p,
            LayoutEdit::SetUnreadFirst {
                id: b(Threads),
                on: None,
            },
        );
        assert_eq!(
            p.sidebar_section_prefs.keys().collect::<Vec<_>>(),
            ["channels"]
        );
    }

    #[test]
    fn threads_limit_choices_include_the_configured_one() {
        assert_eq!(threads_limit_choices(20), THREADS_LIMIT_CHOICES);
        assert_eq!(threads_limit_choices(0), THREADS_LIMIT_CHOICES);
        assert_eq!(threads_limit_choices(30), [5, 10, 20, 30, 50, 100, 0]);
        assert_eq!(threads_limit_choices(500), [5, 10, 20, 50, 100, 500, 0]);
    }

    #[test]
    fn threads_limit_edit() {
        let mut p = UiPrefs::default();
        assert_eq!(Layout::from_prefs(&p).threads_limit, 20);
        assert!(!apply(&mut p, LayoutEdit::SetThreadsLimit(20)).changed);
        assert!(apply(&mut p, LayoutEdit::SetThreadsLimit(50)).changed);
        assert_eq!(Layout::from_prefs(&p).threads_limit, 50);
        assert_ne!(Layout::from_prefs(&p), Layout::default());
        apply(&mut p, LayoutEdit::SetThreadsLimit(0));
        assert_eq!(p.sidebar_threads_limit, 0);
        apply(&mut p, LayoutEdit::SetThreadsLimit(100_000));
        assert_eq!(p.sidebar_threads_limit, MAX_SIDEBAR_THREADS_LIMIT);
        // Reset leaves the threads settings alone, like the mode.
        apply(&mut p, LayoutEdit::Reset);
        assert_eq!(p.sidebar_threads_limit, MAX_SIDEBAR_THREADS_LIMIT);
    }

    #[test]
    fn sanitize_dedupes_ids_and_claims() {
        let mut p = UiPrefs {
            sidebar_order: ids(&[
                "channels",
                "group:b",
                "future",
                "channels",
                "group:a",
                "group:gone",
                "omnichannel",
                "future",
            ]),
            sidebar_groups: vec![
                group("a", "A", &[GroupType::Teams, GroupType::Channels]),
                group(
                    "b",
                    " B ",
                    &[GroupType::Channels, GroupType::DirectMessages],
                ),
                group("a", "A2", &[GroupType::Channels, GroupType::Teams]),
                group("", "E", &[]),
                group("n", "   ", &[GroupType::Discussions]),
            ],
            sidebar_section_prefs: [
                (
                    "group:gone".to_owned(),
                    SectionPrefs {
                        sort: Some(SidebarSort::Alphabetical),
                        unread_first: None,
                    },
                ),
                (
                    "group:a".to_owned(),
                    SectionPrefs {
                        sort: Some(SidebarSort::Alphabetical),
                        unread_first: None,
                    },
                ),
                (
                    "future".to_owned(),
                    SectionPrefs {
                        sort: None,
                        unread_first: Some(true),
                    },
                ),
                ("teams".to_owned(), SectionPrefs::default()),
            ]
            .into(),
            ..Default::default()
        };
        sanitize_prefs(&mut p);
        assert_eq!(
            p.sidebar_order,
            ids(&["channels", "group:b", "future", "group:a"])
        );
        let names: Vec<&str> = p.sidebar_groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "A2", "E"]);
        let gids: HashSet<&str> = p.sidebar_groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(gids.len(), 4);
        assert!(!gids.contains(""));
        // B comes first in display order and keeps Channels.
        assert_eq!(
            p.sidebar_groups[1].types,
            [GroupType::Channels, GroupType::DirectMessages]
        );
        assert_eq!(p.sidebar_groups[0].types, [GroupType::Teams]);
        assert!(p.sidebar_groups[2].types.is_empty());
        assert_eq!(
            p.sidebar_section_prefs.keys().collect::<Vec<_>>(),
            ["future", "group:a"]
        );
        let mut many = UiPrefs {
            sidebar_groups: (0..MAX_GROUPS + 3)
                .map(|i| group(&format!("g{i}"), "G", &[]))
                .collect(),
            ..Default::default()
        };
        sanitize_prefs(&mut many);
        assert_eq!(many.sidebar_groups.len(), MAX_GROUPS);
    }

    #[test]
    fn kept_threads_are_deduped_and_capped() {
        let k = |t: &str, u: &str| KeptThread {
            rid: "r".into(),
            tmid: t.into(),
            user_id: u.into(),
        };
        let mut v = vec![
            k("1", "u"),
            k("1", "u"),
            k("1", "v"),
            k("", "u"),
            k("2", ""),
        ];
        sanitize_kept_threads(&mut v);
        assert_eq!(v, [k("1", "u"), k("1", "v")]);
        let mut v: Vec<KeptThread> = (0..MAX_KEPT_THREADS + 5)
            .map(|i| k(&i.to_string(), "u"))
            .collect();
        sanitize_kept_threads(&mut v);
        assert_eq!(v.len(), MAX_KEPT_THREADS);
        assert_eq!(v[0].tmid, "5", "the oldest are dropped");
    }

    #[test]
    fn kept_threads_belong_to_the_account() {
        let mut e = crate::config::ServerEntry::new("https://a", "A");
        assert!(!e.set_thread_kept("r", "t1", true), "signed out");
        e.user_id = Some("u1".into());
        assert!(e.set_thread_kept("r", "t1", true));
        assert!(!e.set_thread_kept("r", "t1", true));
        e.user_id = Some("u2".into());
        assert_eq!(e.kept_threads().count(), 0);
        assert!(e.set_thread_kept("r", "t1", true));
        assert!(e.set_thread_kept("r", "t1", false));
        assert!(!e.set_thread_kept("r", "t1", false));
        e.user_id = Some("u1".into());
        assert_eq!(
            e.kept_threads()
                .map(|k| k.tmid.as_str())
                .collect::<Vec<_>>(),
            ["t1"]
        );
        for i in 0..MAX_KEPT_THREADS {
            e.set_thread_kept("r", &format!("x{i}"), true);
        }
        assert_eq!(e.kept_threads_all_accounts.len(), MAX_KEPT_THREADS);
    }

    #[test]
    fn group_types_map_room_kinds() {
        assert_eq!(GroupType::of_room(RoomKind::Omnichannel), None);
        assert_eq!(
            GroupType::of_room(RoomKind::GroupDirect),
            Some(GroupType::DirectMessages)
        );
        assert_eq!(
            GroupType::of_room(RoomKind::Team { private: true }),
            Some(GroupType::Teams)
        );
        for t in GroupType::ALL {
            assert!(DEFAULT_ORDER.contains(&t.section()));
            assert_eq!(t.i18n_key(), t.section().i18n_key());
        }
    }
}
