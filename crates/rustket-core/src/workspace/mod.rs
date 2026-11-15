//! Per-server state store and its reducers.
//!
//! Every `apply_*` method is synchronous, idempotent and records what changed in a
//! [`Dirty`] set (plus per-room / per-thread sets and per-area revision counters)
//! so the UI can recompute derived views (room list, timelines, labels) only when
//! needed. A realtime-event dispatcher maps each session event onto one apply call.
//!
//! Realtime events keep arriving while a REST sync runs, so a sync result can be
//! older than what the stream already delivered. Subscriptions, rooms and messages
//! therefore carry `_updatedAt` guards: a document older than the stored one is
//! ignored, and [`Workspace::apply_initial`] merges instead of replacing.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::{
    CustomUserStatus, Message, Role, Room, RoomType, ServerInfo, Setting, Subscription, User,
    UserStatus,
};

use crate::emoji::EmojiRegistry;
use crate::flags::{FlagBlob, Flags};
use crate::mention_groups::{MentionGroupBlob, MentionGroups};
use crate::presence::Presence;
use crate::settings::Settings;
use crate::thread_index::ThreadIndex;
use crate::timeline::{Timeline, TimelineItem};
use crate::typing::{TypingLabel, TypingState};
use crate::uikit::UiKitStore;

mod load;
mod mention_groups;
mod realtime;
mod thread_parents;
mod timelines;

/// Kind of a realtime collection change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Inserted,
    Updated,
    Removed,
}

/// Set of changed areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Dirty(u32);

impl Dirty {
    pub const NONE: Self = Self(0);
    pub const ME: Self = Self(1 << 0);
    pub const SERVER_INFO: Self = Self(1 << 1);
    pub const SETTINGS: Self = Self(1 << 2);
    pub const PERMISSIONS: Self = Self(1 << 3);
    pub const ROLES: Self = Self(1 << 4);
    /// Rooms or subscriptions (room list).
    pub const ROOMS: Self = Self(1 << 5);
    pub const TIMELINES: Self = Self(1 << 6);
    pub const THREADS: Self = Self(1 << 7);
    pub const PRESENCE: Self = Self(1 << 8);
    pub const TYPING: Self = Self(1 << 9);
    pub const EMOJI: Self = Self(1 << 10);
    pub const CUSTOM_STATUSES: Self = Self(1 << 11);
    pub const DRAFTS: Self = Self(1 << 12);
    pub const CONNECTION: Self = Self(1 << 13);
    pub const SYNC: Self = Self(1 << 14);
    /// Thread parents, followed sets or kept threads (sidebar Threads section).
    pub const THREAD_INDEX: Self = Self(1 << 15);
    /// Synced flags: the stored blob, the pending overlay or the effective
    /// flags (see [`Workspace::flags`]).
    pub const FLAGS: Self = Self(1 << 16);
    /// Synced Later state of starred messages (reserved).
    pub const LATER: Self = Self(1 << 17);
    /// Synced message reminders (reserved).
    pub const REMINDERS: Self = Self(1 << 18);
    /// Synced mention groups: the stored blob, the pending overlay or the
    /// effective groups (see [`Workspace::mention_groups`]).
    pub const MENTION_GROUPS: Self = Self(1 << 19);
    /// All unreads / Activity feeds (see [`Workspace::feeds`]).
    pub const FEEDS: Self = Self(1 << 20);
    pub const ALL: Self = Self((1 << 21) - 1);
    const BITS: usize = 21;

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for Dirty {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Dirty {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Accumulated changes since the last [`Workspace::take_changes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub areas: Dirty,
    /// Rooms whose room/subscription data changed.
    pub rooms: HashSet<String>,
    /// Rooms whose main timeline changed.
    pub timelines: HashSet<String>,
    /// Threads (by `tmid`) whose timeline changed.
    pub threads: HashSet<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}

/// Realtime connection state as shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    /// Waiting before the next reconnect attempt.
    Reconnecting {
        attempt: u32,
    },
    /// The token was rejected; the user must log in again.
    LoggedOut,
}

/// A room as the sidebar sees it: the room document and the current user's
/// subscription. Either may be missing for a while during sync.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoomEntry {
    pub room: Option<Room>,
    pub sub: Option<Subscription>,
}

impl RoomEntry {
    pub fn rid(&self) -> &str {
        self.sub
            .as_ref()
            .map(|s| s.rid.as_str())
            .or(self.room.as_ref().map(|r| r.id.as_str()))
            .unwrap_or_default()
    }

    pub fn room_type(&self) -> RoomType {
        self.sub
            .as_ref()
            .map(|s| s.t)
            .or(self.room.as_ref().map(|r| r.t))
            .unwrap_or_default()
    }

    /// `name` (username for DMs, slug for channels).
    pub fn name(&self) -> &str {
        self.sub
            .as_ref()
            .map(|s| s.name.as_str())
            .filter(|n| !n.is_empty())
            .or(self.room.as_ref().and_then(|r| r.name.as_deref()))
            .unwrap_or_default()
    }

    pub fn fname(&self) -> Option<&str> {
        self.sub
            .as_ref()
            .and_then(|s| s.fname.as_deref())
            .or(self.room.as_ref().and_then(|r| r.fname.as_deref()))
            .filter(|f| !f.is_empty())
    }

    pub fn is_discussion(&self) -> bool {
        self.sub.as_ref().and_then(|s| s.prid.as_ref()).is_some()
            || self.room.as_ref().is_some_and(Room::is_discussion)
    }

    pub fn is_team(&self) -> bool {
        self.sub.as_ref().and_then(|s| s.team_main) == Some(true)
            || self.room.as_ref().is_some_and(Room::is_team)
    }

    pub fn is_read_only(&self) -> bool {
        self.room.as_ref().and_then(|r| r.ro) == Some(true)
    }

    pub fn is_archived(&self) -> bool {
        self.sub.as_ref().and_then(|s| s.archived) == Some(true)
            || self.room.as_ref().and_then(|r| r.archived) == Some(true)
    }

    pub fn is_encrypted(&self) -> bool {
        self.room.as_ref().and_then(|r| r.encrypted) == Some(true)
    }

    /// A room with the shape of the user's own Rustket service room (see
    /// `rocketchat::models::is_own_service_room`; the subscription names the
    /// user): never listed, searched or counted. Rooms anyone else marked
    /// are ordinary rooms. Its data is used only when the account bound it
    /// ([`Workspace::service_rooms`]).
    pub fn is_service_room(&self) -> bool {
        match (&self.sub, &self.room) {
            (Some(s), Some(r)) => rocketchat::models::is_own_service_room(s, r, &s.u.id),
            _ => false,
        }
    }

    /// Activity time for sorting, as Rocket.Chat's `SubscriptionsCachedStore`
    /// computes `lm`: `max(sub.lr, room.lm || sub.ts || room.ts)` (thread replies
    /// set `lr` without touching `room.lm`). `lastMessage.ts` stands in for a
    /// missing `room.lm`.
    pub fn last_activity(&self) -> Option<RcDate> {
        let r = self.room.as_ref();
        let s = self.sub.as_ref();
        let base = r
            .and_then(|r| r.lm)
            .or(r.and_then(|r| r.last_message.as_ref().and_then(|m| m.ts)))
            .or(s.and_then(|s| s.ts))
            .or(r.and_then(|r| r.ts));
        match (s.and_then(|s| s.lr), base) {
            (Some(lr), Some(b)) => Some(if lr > b { lr } else { b }),
            (lr, b) => lr.or(b),
        }
    }
}

/// Key of a composer draft: a room or a thread inside it.
pub type DraftKey = (String, Option<String>);

#[derive(Debug, Clone, Default)]
pub struct Workspace {
    me: Option<User>,
    /// Number of [`Self::apply_me`] calls (fresh `/me` documents).
    me_generation: u64,
    server_info: Option<ServerInfo>,
    settings: Settings,
    permissions: HashMap<String, Vec<String>>,
    roles: HashMap<String, Role>,
    rooms: HashMap<String, RoomEntry>,
    timelines: HashMap<String, Timeline>,
    threads: HashMap<String, Timeline>,
    presence: Presence,
    typing: TypingState,
    emoji: EmojiRegistry,
    custom_statuses: HashMap<String, CustomUserStatus>,
    drafts: HashMap<DraftKey, String>,
    /// Local messages not yet confirmed by the server, by `_id`; kept while their
    /// timelines are closed and merged back on open.
    outbox: HashMap<String, TimelineItem>,
    /// Realtime activity, so REST snapshots read earlier do not undo it.
    realtime: RealtimeMarks,
    connection: ConnectionState,
    last_sync: Option<RcDate>,
    changes: Changes,
    revisions: [u64; Dirty::BITS],
    /// Ids of messages per room whose shown version arrived as an ephemeral
    /// message (`{uid}/message`); see `uikit::trust` (docs/uikit.md §3.2).
    ephemeral: HashMap<String, HashSet<String>>,
    uikit: UiKitStore,
    videoconf: crate::videoconf::VideoConfStore,
    /// End-to-end encryption of this account (set by the app while logged in).
    e2e: Option<crate::e2e::E2eManager>,
    /// Shows the offline cache ([`Self::apply_cached`]) until the first
    /// initial load from the server.
    from_cache: bool,
    /// Rooms whose room/subscription came from the offline cache and were
    /// not touched by the server since: the initial load replaces them even
    /// on an `_updatedAt` tie.
    cached_rooms: HashSet<String>,
    /// Thread parents and followed sets for the sidebar's Threads section.
    thread_index: ThreadIndex,
    /// The "starred threads in Favorites" switch as last set (the server
    /// settings gate it).
    starred_threads: bool,
    /// The flags blob of the user's service rooms (merged when there are
    /// several).
    flag_blob: FlagBlob,
    /// This device's flag edits not known to be stored yet.
    flag_overlay: FlagBlob,
    /// `flag_blob` merged with `flag_overlay`.
    flags: Flags,
    /// The mention groups blob of the user's service rooms.
    mention_group_blob: MentionGroupBlob,
    /// This device's mention group edits not known to be stored yet.
    mention_group_overlay: MentionGroupBlob,
    /// `mention_group_blob` merged with `mention_group_overlay`.
    mention_groups: MentionGroups,
    /// The user's own service rooms, primary first.
    service_rooms: Vec<String>,
    /// Unread messages and thread mentions of the All unreads and Activity
    /// views (memory only). Kept while the views are closed, so reopening
    /// one fetches nothing loaded; dropped with the workspace on sign-out.
    feeds: crate::inbox::feeds::FeedStore,
}

/// When a realtime event touched something.
#[derive(Debug, Clone, Copy)]
struct Mark {
    seq: u64,
    at: Instant,
}

/// Realtime touches since recent snapshots: rooms/subscriptions inserted or
/// updated, rooms/subscriptions removed (tombstones), presence changes.
#[derive(Debug, Clone, Default)]
struct RealtimeMarks {
    seq: u64,
    /// `seq` when the last snapshot was applied.
    snapshot_seq: u64,
    rooms: HashMap<String, Mark>,
    removed: HashMap<String, Mark>,
    presence: HashMap<String, Mark>,
    /// Settings / permissions / roles changed in realtime, with the value
    /// (`None` = removed), re-applied over snapshots that started earlier.
    settings: HashMap<String, (Mark, Option<Setting>)>,
    permissions: HashMap<String, (Mark, Option<Vec<String>>)>,
    roles: HashMap<String, (Mark, Option<Role>)>,
}

/// Marks older than this are forgotten (no sync runs that long).
const MARK_TTL: std::time::Duration = std::time::Duration::from_secs(600);

impl RealtimeMarks {
    fn next(&mut self) -> Mark {
        self.seq += 1;
        Mark {
            seq: self.seq,
            at: Instant::now(),
        }
    }

    /// Whether `m` happened after a snapshot started (`started`), or, when its
    /// start is unknown, after the previous snapshot was applied.
    fn after(&self, m: Option<&Mark>, started: Option<Instant>) -> bool {
        m.is_some_and(|m| match started {
            Some(s) => m.at >= s,
            None => m.seq > self.snapshot_seq,
        })
    }

    fn snapshot_applied(&mut self) {
        self.snapshot_seq = self.seq;
        for map in [&mut self.rooms, &mut self.removed, &mut self.presence] {
            map.retain(|_, m| m.at.elapsed() < MARK_TTL);
        }
        self.settings.retain(|_, (m, _)| m.at.elapsed() < MARK_TTL);
        self.permissions
            .retain(|_, (m, _)| m.at.elapsed() < MARK_TTL);
        self.roles.retain(|_, (m, _)| m.at.elapsed() < MARK_TTL);
    }
}

/// Within this much of its `ts`, a message's `_updatedAt` marks it as newly
/// created (not an edit, reaction or thread-counter update of an old message).
const NEW_MESSAGE_WINDOW_MS: i64 = 2_000;

/// `m` is a thread parent (with replies) the user `me` starred.
fn starred_parent(m: &Message, me: Option<&str>) -> bool {
    m.tcount.unwrap_or(0) > 0 && me.is_some_and(|me| m.is_starred_by(me))
}

/// `new` is an older version than `old` (both known).
fn is_older(new: Option<RcDate>, old: Option<RcDate>) -> bool {
    matches!((new, old), (Some(n), Some(o)) if n < o)
}

/// Where a thread parent passed to `Workspace::index_parent` comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentSource {
    /// A realtime (or fresh REST call) update: its follow state is applied to
    /// the room's followed set and remembered for crawls in flight.
    Live,
    /// A timeline or thread page, an opened thread or the offline cache:
    /// stored if relevant, follow state not applied (it may be stale).
    Page,
    /// Asked for over REST: stored even when nothing refers to it yet.
    Fetched,
    /// The offline cache: like `Page`, but its star state may be stale and
    /// is not applied.
    Cached,
}

/// `d` is known and later than `than` (known).
fn is_newer(d: Option<RcDate>, than: Option<RcDate>) -> bool {
    matches!((d, than), (Some(d), Some(t)) if d > t)
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    // --- change tracking ------------------------------------------------------

    fn mark(&mut self, d: Dirty) {
        self.changes.areas |= d;
        for (i, rev) in self.revisions.iter_mut().enumerate() {
            if d.0 & (1 << i) != 0 {
                *rev = rev.wrapping_add(1);
            }
        }
    }

    fn mark_room(&mut self, rid: &str) {
        self.mark(Dirty::ROOMS);
        self.changes.rooms.insert(rid.to_owned());
        // Whatever moved the room's `ls` (read here, elsewhere, a sync).
        let ls = self.subscription(rid).and_then(|s| s.ls);
        if self.feeds.prune(rid, ls) {
            self.mark(Dirty::FEEDS);
        }
        if self.service_rooms.iter().any(|r| r == rid)
            || self.rooms.get(rid).is_some_and(RoomEntry::is_service_room)
        {
            self.refresh_service_data();
        }
    }

    fn mark_timeline(&mut self, rid: &str) {
        self.mark(Dirty::TIMELINES);
        self.changes.timelines.insert(rid.to_owned());
    }

    fn mark_thread(&mut self, tmid: &str) {
        self.mark(Dirty::THREADS);
        self.changes.threads.insert(tmid.to_owned());
    }

    /// Returns and clears the accumulated changes.
    pub fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }

    pub fn pending_changes(&self) -> &Changes {
        &self.changes
    }

    /// Monotonic counter of an area (the max over the bits of `area`). Use it to
    /// memoise derived views without consuming [`Self::take_changes`].
    pub fn revision(&self, area: Dirty) -> u64 {
        self.revisions
            .iter()
            .enumerate()
            .filter(|(i, _)| area.0 & (1 << i) != 0)
            .map(|(_, r)| *r)
            .max()
            .unwrap_or(0)
    }

    // --- read access ----------------------------------------------------------

    pub fn me(&self) -> Option<&User> {
        self.me.as_ref()
    }

    /// Bumped by every [`Self::apply_me`], even when `me` is unchanged: lets
    /// derived state tell a fresh `/me` from a stale one.
    pub fn me_generation(&self) -> u64 {
        self.me_generation
    }

    pub fn user_id(&self) -> Option<&str> {
        self.me.as_ref().map(|m| m.id.as_str())
    }

    pub fn username(&self) -> Option<&str> {
        self.me.as_ref().and_then(|m| m.username.as_deref())
    }

    /// The current user's status (presence cache first, then `me`; both are
    /// updated by realtime `user-status` events and by [`Self::apply_me`]).
    pub fn my_status(&self) -> UserStatus {
        self.user_id()
            .and_then(|id| self.presence.get(id))
            .map(|p| p.status)
            .or(self.me.as_ref().and_then(|m| m.status))
            .unwrap_or(UserStatus::Online)
    }

    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    pub fn server_version(&self) -> Option<(u32, u32, u32)> {
        self.server_info.as_ref()?.version_tuple()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Permission id → role names.
    pub fn permissions(&self) -> &HashMap<String, Vec<String>> {
        &self.permissions
    }

    pub fn permission_roles(&self, perm: &str) -> Option<&[String]> {
        self.permissions.get(perm).map(Vec::as_slice)
    }

    pub fn roles(&self) -> &HashMap<String, Role> {
        &self.roles
    }

    pub fn rooms(&self) -> &HashMap<String, RoomEntry> {
        &self.rooms
    }

    pub fn room(&self, rid: &str) -> Option<&RoomEntry> {
        self.rooms.get(rid)
    }

    pub fn subscription(&self, rid: &str) -> Option<&Subscription> {
        self.rooms.get(rid)?.sub.as_ref()
    }

    pub fn timeline(&self, rid: &str) -> Option<&Timeline> {
        self.timelines.get(rid)
    }

    pub fn thread(&self, tmid: &str) -> Option<&Timeline> {
        self.threads.get(tmid)
    }

    pub fn presence(&self) -> &Presence {
        &self.presence
    }

    pub fn typing(&self) -> &TypingState {
        &self.typing
    }

    /// Typing label for a room/thread, excluding the current user.
    pub fn typing_label(&self, rid: &str, tmid: Option<&str>, now: Instant) -> Option<TypingLabel> {
        self.typing.label(rid, tmid, now, self.username())
    }

    /// Whether a `user-activity` name is the current user: Rocket.Chat sends the
    /// real name instead of the username under `UI_Use_Real_Name`.
    fn is_my_activity_name(&self, name: &str) -> bool {
        let Some(me) = self.me.as_ref() else {
            return false;
        };
        me.username.as_deref() == Some(name)
            || (self.settings.use_real_name() && me.name.as_deref() == Some(name))
    }

    /// The name a user's `user-activity` events carry (see [`Self::is_my_activity_name`]).
    fn activity_name<'a>(&self, u: &'a rocketchat::models::UserRef) -> Option<&'a str> {
        if self.settings.use_real_name() {
            u.name.as_deref().or(u.username.as_deref())
        } else {
            u.username.as_deref()
        }
    }

    /// Unconfirmed local messages of a room (all threads included), oldest first.
    pub fn pending_messages(&self, rid: &str) -> Vec<&TimelineItem> {
        let mut v: Vec<&TimelineItem> = self
            .outbox
            .values()
            .filter(|i| i.message.rid == rid)
            .collect();
        v.sort_by_key(|i| (i.message.ts, i.message.id.clone()));
        v
    }

    pub fn emoji(&self) -> &EmojiRegistry {
        &self.emoji
    }

    pub fn custom_statuses(&self) -> impl Iterator<Item = &CustomUserStatus> + '_ {
        self.custom_statuses.values()
    }

    pub fn draft(&self, rid: &str, tmid: Option<&str>) -> Option<&str> {
        self.drafts
            .get(&(rid.to_owned(), tmid.map(str::to_owned)))
            .map(String::as_str)
    }

    pub fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// `since` for the next delta sync (server clock; see [`crate::sync`]).
    pub fn last_sync(&self) -> Option<RcDate> {
        self.last_sync
    }

    /// Real names vs usernames: the local override wins over `UI_Use_Real_Name`.
    pub fn use_real_names(&self, local_override: Option<bool>) -> bool {
        local_override.unwrap_or_else(|| self.settings.use_real_name())
    }

    // --- drafts -------------------------------------------------------------------

    /// Stores a draft; empty text removes it.
    pub fn set_draft(&mut self, rid: &str, tmid: Option<&str>, text: &str) {
        let key = (rid.to_owned(), tmid.map(str::to_owned));
        let changed = if text.trim().is_empty() {
            self.drafts.remove(&key).is_some()
        } else if self.drafts.get(&key).map(String::as_str) != Some(text) {
            self.drafts.insert(key, text.to_owned());
            true
        } else {
            false
        };
        if changed {
            self.mark(Dirty::DRAFTS);
            self.changes.rooms.insert(rid.to_owned());
        }
    }

    pub fn take_draft(&mut self, rid: &str, tmid: Option<&str>) -> Option<String> {
        let d = self
            .drafts
            .remove(&(rid.to_owned(), tmid.map(str::to_owned)));
        if d.is_some() {
            self.mark(Dirty::DRAFTS);
        }
        d
    }

    pub fn drafts(&self) -> &HashMap<DraftKey, String> {
        &self.drafts
    }

    // --- end-to-end encryption ------------------------------------------------------

    /// The account's E2E manager, if the app started one.
    pub fn e2e(&self) -> Option<&crate::e2e::E2eManager> {
        self.e2e.as_ref()
    }

    pub fn set_e2e(&mut self, manager: Option<crate::e2e::E2eManager>) {
        self.e2e = manager;
    }

    /// Rooms with an open room or thread timeline, or with an inbox feed.
    pub fn rooms_with_timelines(&self) -> HashSet<String> {
        self.timelines
            .keys()
            .cloned()
            .chain(self.threads.values().map(|t| t.rid().to_owned()))
            .chain(self.feeds.rooms().cloned())
            .collect()
    }

    /// Rewrites the messages of room `rid` in place: its room timeline, its
    /// thread timelines, its inbox feed and the room's last message (see
    /// [`Timeline::rewrite`]). Changed timelines and rooms are marked. Returns
    /// whether anything changed.
    pub fn rewrite_messages(&mut self, rid: &str, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut changed = false;
        if self.feeds.rewrite(rid, f) {
            self.mark(Dirty::FEEDS);
            changed = true;
        }
        if let Some(t) = self.timelines.get_mut(rid)
            && t.rewrite(f)
        {
            self.mark_timeline(rid);
            changed = true;
        }
        let threads: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.rid() == rid)
            .map(|(k, _)| k.clone())
            .collect();
        for tmid in threads {
            if let Some(t) = self.threads.get_mut(&tmid)
                && t.rewrite(f)
            {
                self.mark_thread(&tmid);
                changed = true;
            }
        }
        if let Some(m) = self
            .rooms
            .get_mut(rid)
            .and_then(|e| e.room.as_mut())
            .and_then(|r| r.last_message.as_deref_mut())
            && f(m)
        {
            self.mark_room(rid);
            changed = true;
        }
        if self.thread_index.rewrite(rid, f) {
            self.mark(Dirty::THREAD_INDEX);
            changed = true;
        }
        changed
    }

    // --- inbox feeds (All unreads, Activity) ------------------------------------

    pub fn feeds(&self) -> &crate::inbox::feeds::FeedStore {
        &self.feeds
    }

    /// The rooms the open inbox view wants: their feeds are not evicted
    /// (empty: the view closed). Changes nothing shown.
    pub fn keep_feeds<'a>(&mut self, rids: impl IntoIterator<Item = &'a String>) {
        self.feeds.keep(rids);
    }

    /// Starts a page of room `rid`'s feed (`earlier`: the next older one),
    /// against the subscription's current `ls`.
    pub fn begin_feed(&mut self, rid: &str, earlier: bool) -> Option<crate::inbox::feeds::FeedReq> {
        let ls = self.subscription(rid).and_then(|s| s.ls);
        let req = self.feeds.begin(rid, earlier, ls)?;
        self.mark(Dirty::FEEDS);
        Some(req)
    }

    /// A feed page ended (see [`crate::inbox::feeds::FeedStore::finish`]).
    pub fn finish_feed(
        &mut self,
        req: &crate::inbox::feeds::FeedReq,
        res: Result<rocketchat::models::History, Option<String>>,
    ) {
        let ls = self.subscription(&req.rid).and_then(|s| s.ls);
        if self.feeds.finish(req, res, ls) {
            self.mark(Dirty::FEEDS);
        }
    }

    /// Forgets a failed feed so it loads again.
    pub fn reset_feed(&mut self, rid: &str) {
        if self.feeds.reset(rid) {
            self.mark(Dirty::FEEDS);
        }
    }

    pub fn begin_thread_mentions(&mut self, rid: &str) -> bool {
        let started = self.feeds.begin_mentions(rid);
        if started {
            self.mark(Dirty::FEEDS);
        }
        started
    }

    pub fn finish_thread_mentions(&mut self, rid: &str, res: Result<Vec<Message>, Option<String>>) {
        if self.feeds.finish_mentions(rid, res) {
            self.mark(Dirty::FEEDS);
        }
    }

    /// Rewrites the last message of every room (sidebar previews).
    pub fn rewrite_last_messages(&mut self, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut touched = Vec::new();
        for (rid, e) in self.rooms.iter_mut() {
            if let Some(m) = e.room.as_mut().and_then(|r| r.last_message.as_deref_mut())
                && f(m)
            {
                touched.push(rid.clone());
            }
        }
        for rid in &touched {
            self.mark_room(rid);
        }
        !touched.is_empty()
    }
}

#[cfg(test)]
mod tests;
