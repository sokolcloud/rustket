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
    CustomEmoji, CustomUserStatus, Message, Paginated, Permission, Role, Room, RoomType,
    ServerInfo, Setting, Subscription, User, UserStatus,
};
use rocketchat::rest::HistoryCursor;

use crate::emoji::EmojiRegistry;
use crate::presence::Presence;
use crate::settings::Settings;
use crate::sync::{DeltaData, InitialData, RoomSync};
use crate::timeline::{DeliveryState, PageTicket, Timeline, TimelineItem};
use crate::typing::{TypingLabel, TypingState};
use crate::uikit::UiKitStore;

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
    pub const ALL: Self = Self((1 << 15) - 1);
    const BITS: usize = 15;

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

/// `new` is an older version than `old` (both known).
fn is_older(new: Option<RcDate>, old: Option<RcDate>) -> bool {
    matches!((new, old), (Some(n), Some(o)) if n < o)
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

    // --- bulk loads -----------------------------------------------------------

    /// Applies a fresh [`InitialData`]. Settings, permissions, roles, custom emoji
    /// and statuses are replaced. Rooms and subscriptions are merged: a snapshot
    /// entry replaces a stored one only when strictly newer, entries the realtime
    /// stream removed after the snapshot started stay removed, and stored entries
    /// missing from the snapshot are dropped (with their timelines, recorded in
    /// [`Changes`]) unless realtime events touched them after the snapshot
    /// started. Presence and status changed in realtime meanwhile are kept.
    /// Loaded room timelines (and their threads) are reset so they reload from
    /// the newest message. Drafts and pending messages are kept.
    pub fn apply_initial(&mut self, data: InitialData) {
        let InitialData {
            me,
            server_info,
            settings,
            permissions,
            roles,
            subscriptions,
            rooms,
            custom_emoji,
            custom_statuses,
            presence,
            next_since,
            started_at,
            warnings: _,
        } = data;
        self.snapshot_me(me, started_at);
        if server_info.is_some() {
            self.server_info = server_info;
        }
        self.settings = Settings::from_list(settings);
        self.permissions = permissions.into_iter().map(|p| (p.id, p.roles)).collect();
        self.roles = roles.into_iter().map(|r| (r.id.clone(), r)).collect();
        self.reapply_realtime_config(started_at);

        let newest_sub = subscriptions.iter().filter_map(|s| s.updated_at).max();
        let newest_room = rooms.iter().filter_map(|r| r.updated_at).max();
        let snapshot_subs: HashSet<String> = subscriptions.iter().map(|s| s.rid.clone()).collect();
        let snapshot_rooms: HashSet<String> = rooms.iter().map(|r| r.id.clone()).collect();
        for s in subscriptions {
            self.snapshot_subscription(s, started_at);
        }
        for r in rooms {
            self.snapshot_room(r, started_at);
        }
        // Kept although missing from the snapshot: touched by realtime after it
        // started (when the start is unknown: since the last snapshot, and newer
        // than the snapshot's own list).
        let keep = |marks: &RealtimeMarks, rid: &str, d: Option<RcDate>, newest: Option<RcDate>| {
            marks.after(marks.rooms.get(rid), started_at)
                && (started_at.is_some() || is_newer(d, newest))
        };
        let mut gone = Vec::new();
        for (rid, e) in &mut self.rooms {
            if !snapshot_subs.contains(rid)
                && e.sub
                    .as_ref()
                    .is_some_and(|s| !keep(&self.realtime, rid, s.updated_at, newest_sub))
            {
                e.sub = None;
            }
            if !snapshot_rooms.contains(rid)
                && e.room
                    .as_ref()
                    .is_some_and(|r| !keep(&self.realtime, rid, r.updated_at, newest_room))
            {
                e.room = None;
            }
            if e.sub.is_none() && e.room.is_none() {
                gone.push(rid.clone());
            }
        }
        for rid in gone {
            self.remove_room(&rid);
        }
        self.emoji.set_all(custom_emoji);
        self.custom_statuses = custom_statuses
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        self.snapshot_presence(&presence, false, started_at);
        // Loaded timelines may have missed messages while no stream was
        // connected (a reload over an earlier session): load them again.
        let stale: Vec<String> = self
            .timelines
            .iter()
            .filter(|(_, t)| t.pagination.loaded)
            .map(|(k, _)| k.clone())
            .collect();
        for rid in stale {
            self.reset_room_timelines(&rid);
        }
        if next_since.is_some() {
            self.last_sync = next_since;
        }
        self.realtime.snapshot_applied();
        self.changes.rooms.extend(self.rooms.keys().cloned());
        self.mark(Dirty::ALL);
    }

    /// Merges a [`DeltaData`] (after a reconnect), with the same guards as
    /// [`Self::apply_initial`].
    pub fn apply_delta(&mut self, d: DeltaData) {
        let started = d.started_at;
        if let Some(me) = d.me {
            self.snapshot_me(me, started);
        }
        for s in d.subscriptions.update {
            self.snapshot_subscription(s, started);
        }
        for removed in d.subscriptions.remove {
            let rid = removed.rid.clone().or_else(|| {
                self.rooms
                    .values()
                    .find(|e| e.sub.as_ref().is_some_and(|s| s.id == removed.id))
                    .map(|e| e.rid().to_owned())
            });
            let Some(rid) = rid else { continue };
            // A subscription re-created after the removal (new `_id`, updated
            // later, or touched by realtime meanwhile) stays.
            let stale = self
                .subscription(&rid)
                .is_some_and(|s| s.id != removed.id || is_newer(s.updated_at, removed.deleted_at))
                || self.realtime.after(self.realtime.rooms.get(&rid), started);
            if !stale {
                self.remove_subscription(&rid);
            }
        }
        for r in d.rooms.update {
            self.snapshot_room(r, started);
        }
        for removed in d.rooms.remove {
            let recreated = self
                .room(&removed.id)
                .and_then(|e| e.room.as_ref())
                .is_some_and(|r| is_newer(r.updated_at, removed.deleted_at))
                || self
                    .realtime
                    .after(self.realtime.rooms.get(&removed.id), started);
            if !recreated {
                self.remove_room(&removed.id);
            }
        }
        if let Some(p) = d.permissions {
            for perm in p.update {
                if self.permissions.get(&perm.id) != Some(&perm.roles) {
                    self.permissions.insert(perm.id, perm.roles);
                    self.mark(Dirty::PERMISSIONS);
                }
            }
            for removed in p.remove {
                if self.permissions.remove(&removed.id).is_some() {
                    self.mark(Dirty::PERMISSIONS);
                }
            }
        }
        if let Some(e) = d.custom_emoji {
            for emoji in e.update {
                self.apply_custom_emoji(Change::Updated, emoji);
            }
            for removed in e.remove {
                if self.emoji.remove(&removed.id) {
                    self.mark(Dirty::EMOJI);
                }
            }
        }
        if let Some(settings) = d.settings {
            self.settings = Settings::from_list(settings);
            self.mark(Dirty::SETTINGS);
        }
        if let Some(roles) = d.roles {
            self.roles = roles.into_iter().map(|r| (r.id.clone(), r)).collect();
            self.mark(Dirty::ROLES);
        }
        if let Some(statuses) = d.custom_statuses {
            self.custom_statuses = statuses.into_iter().map(|s| (s.id.clone(), s)).collect();
            self.mark(Dirty::CUSTOM_STATUSES);
        }
        self.reapply_realtime_config(started);
        self.snapshot_presence(&d.presence, d.presence_full, started);
        if d.next_since.is_some() {
            self.last_sync = d.next_since;
        }
        self.realtime.snapshot_applied();
        self.mark(Dirty::SYNC);
    }

    /// A subscription from a REST snapshot: skipped when realtime removed the
    /// room after the snapshot started; replaces a stored copy only when strictly
    /// newer (Rocket.Chat's realtime unread patch keeps the old `_updatedAt`, so
    /// on a tie the stored realtime copy is the newer one).
    fn snapshot_subscription(&mut self, sub: Subscription, started: Option<Instant>) {
        if self
            .realtime
            .after(self.realtime.removed.get(&sub.rid), started)
        {
            return;
        }
        let rid = sub.rid.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.sub.as_ref()
            && old.updated_at.is_some()
            && !is_newer(sub.updated_at, old.updated_at)
        {
            return;
        }
        if entry.sub.as_ref() == Some(&sub) {
            return;
        }
        entry.sub = Some(sub);
        self.mark_room(&rid);
    }

    /// A room from a REST snapshot (same rules as [`Self::snapshot_subscription`]).
    fn snapshot_room(&mut self, room: Room, started: Option<Instant>) {
        if self
            .realtime
            .after(self.realtime.removed.get(&room.id), started)
        {
            return;
        }
        let rid = room.id.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.room.as_ref()
            && old.updated_at.is_some()
            && !is_newer(room.updated_at, old.updated_at)
        {
            return;
        }
        if entry.room.as_ref() == Some(&room) {
            return;
        }
        entry.room = Some(room);
        self.mark_room(&rid);
    }

    /// `me` from a REST snapshot: a status the stream changed after the snapshot
    /// started is kept.
    fn snapshot_me(&mut self, mut me: User, started: Option<Instant>) {
        if self
            .realtime
            .after(self.realtime.presence.get(&me.id), started)
            && let Some(cur) = self.me.as_ref().filter(|m| m.id == me.id)
        {
            me.status = cur.status;
            me.status_text = cur.status_text.clone();
        } else if let Some(status) = me.status
            && self.presence.update(
                &me.id,
                me.username.as_deref(),
                status,
                Some(me.status_text.as_deref().unwrap_or("")),
            )
        {
            self.mark(Dirty::PRESENCE);
        }
        if self.me.as_ref() != Some(&me) {
            self.me = Some(me);
            self.mark(Dirty::ME);
        }
    }

    /// Presence from a REST snapshot. With `full`, everyone missing from the list
    /// is offline. Users whose status the stream changed after the snapshot
    /// started are left alone; so is the current user (see [`Self::snapshot_me`]).
    fn snapshot_presence(
        &mut self,
        list: &[rocketchat::models::UserSummary],
        full: bool,
        started: Option<Instant>,
    ) {
        let touched = |w: &Self, uid: &str| w.realtime.after(w.realtime.presence.get(uid), started);
        let mut changed = false;
        if full {
            let listed: HashSet<&str> = list.iter().map(|u| u.id.as_str()).collect();
            let my_id = self.user_id().map(str::to_owned);
            let absent: Vec<String> = self
                .presence
                .user_ids()
                .filter(|id| !listed.contains(id) && Some(*id) != my_id.as_deref())
                .filter(|id| !touched(self, id))
                .map(str::to_owned)
                .collect();
            for id in absent {
                changed |= self.presence.update(&id, None, UserStatus::Offline, None);
            }
        }
        for u in list {
            if !touched(self, &u.id) {
                changed |= self.presence.update_from_summary(u);
            }
        }
        if changed {
            self.mark(Dirty::PRESENCE);
        }
    }

    /// Applies a `chat.syncMessages` result to the room's open timelines. The
    /// room timeline is reset (so it reloads from the newest message; see
    /// `Timeline::pagination.loaded`) when entries were dropped, or when the
    /// server hides deleted messages without reporting them to the sync
    /// ([`Settings::deletions_invisible_to_sync`]).
    pub fn apply_room_sync(&mut self, s: RoomSync) {
        if s.dropped > 0 || self.settings.deletions_invisible_to_sync() {
            self.reset_room_timelines(&s.rid);
        }
        for m in s.updated {
            self.apply_message_from(m, false);
        }
        for id in &s.deleted {
            self.apply_message_deleted(&s.rid, id);
        }
    }

    // --- realtime: user, server, settings ---------------------------------------

    /// The current user's document (realtime `userData`, or a fresh `/me`). Its
    /// status also updates the presence cache that [`Self::my_status`] reads.
    pub fn apply_me(&mut self, me: User) {
        self.me_generation += 1;
        if me.status.is_some() {
            let m = self.realtime.next();
            self.realtime.presence.insert(me.id.clone(), m);
        }
        if let Some(status) = me.status
            && self.presence.update(
                &me.id,
                me.username.as_deref(),
                status,
                Some(me.status_text.as_deref().unwrap_or("")),
            )
        {
            self.mark(Dirty::PRESENCE);
        }
        if self.me.as_ref() != Some(&me) {
            self.me = Some(me);
            self.mark(Dirty::ME);
        }
    }

    pub fn apply_server_info(&mut self, info: ServerInfo) {
        if self.server_info.as_ref() != Some(&info) {
            self.server_info = Some(info);
            self.mark(Dirty::SERVER_INFO);
        }
    }

    /// Realtime setting change (kept over REST snapshots that started earlier).
    pub fn apply_setting(&mut self, change: Change, setting: Setting) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.settings.insert(setting.id.clone(), (m, None));
                self.settings.remove(&setting.id);
            }
            _ => {
                self.realtime
                    .settings
                    .insert(setting.id.clone(), (m, Some(setting.clone())));
                self.settings.set(setting);
            }
        }
        self.mark(Dirty::SETTINGS);
    }

    /// Realtime permission change (kept over REST snapshots that started earlier).
    pub fn apply_permission(&mut self, change: Change, p: Permission) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.permissions.insert(p.id.clone(), (m, None));
                self.permissions.remove(&p.id);
            }
            _ => {
                self.realtime
                    .permissions
                    .insert(p.id.clone(), (m, Some(p.roles.clone())));
                if self.permissions.get(&p.id) == Some(&p.roles) {
                    return;
                }
                self.permissions.insert(p.id, p.roles);
            }
        }
        self.mark(Dirty::PERMISSIONS);
    }

    /// Realtime role change (kept over REST snapshots that started earlier).
    pub fn apply_role(&mut self, change: Change, role: Role) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.roles.insert(role.id.clone(), (m, None));
                self.roles.remove(&role.id);
            }
            _ => {
                self.realtime
                    .roles
                    .insert(role.id.clone(), (m, Some(role.clone())));
                self.roles.insert(role.id.clone(), role);
            }
        }
        self.mark(Dirty::ROLES);
    }

    /// Re-applies settings, permissions and roles changed in realtime after a
    /// snapshot (started at `started`) was requested.
    fn reapply_realtime_config(&mut self, started: Option<Instant>) {
        let marks = &self.realtime;
        let settings: Vec<(String, Option<Setting>)> = marks
            .settings
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        let permissions: Vec<(String, Option<Vec<String>>)> = marks
            .permissions
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        let roles: Vec<(String, Option<Role>)> = marks
            .roles
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        for (id, v) in settings {
            match v {
                Some(s) => self.settings.set(s),
                None => {
                    self.settings.remove(&id);
                }
            }
            self.mark(Dirty::SETTINGS);
        }
        for (id, v) in permissions {
            match v {
                Some(r) => {
                    self.permissions.insert(id, r);
                }
                None => {
                    self.permissions.remove(&id);
                }
            }
            self.mark(Dirty::PERMISSIONS);
        }
        for (id, v) in roles {
            match v {
                Some(r) => {
                    self.roles.insert(id, r);
                }
                None => {
                    self.roles.remove(&id);
                }
            }
            self.mark(Dirty::ROLES);
        }
    }

    /// The current user's global roles changed (`roles-change` with scope `Users`)
    /// or a room role changed (`scope` = rid; updates the subscription's roles).
    pub fn apply_user_role_change(
        &mut self,
        uid: &str,
        role: &str,
        added: bool,
        scope: Option<&str>,
    ) {
        if self.user_id() != Some(uid) {
            return;
        }
        let edit = |roles: &mut Option<Vec<String>>| {
            let list = roles.get_or_insert_with(Vec::new);
            let has = list.iter().any(|r| r == role);
            if added && !has {
                list.push(role.to_owned());
                true
            } else if !added && has {
                list.retain(|r| r != role);
                true
            } else {
                false
            }
        };
        match scope {
            Some(rid) if rid != "Users" => {
                if let Some(sub) = self.rooms.get_mut(rid).and_then(|e| e.sub.as_mut())
                    && edit(&mut sub.roles)
                {
                    self.mark_room(rid);
                    self.mark(Dirty::PERMISSIONS);
                }
            }
            _ => {
                if let Some(me) = self.me.as_mut()
                    && edit(&mut me.roles)
                {
                    self.mark(Dirty::ME | Dirty::PERMISSIONS);
                }
            }
        }
    }

    pub fn apply_custom_emoji(&mut self, change: Change, e: CustomEmoji) {
        match change {
            Change::Removed => {
                if !self.emoji.remove(&e.id) && !self.emoji.remove(&e.name) {
                    return;
                }
            }
            _ => self.emoji.upsert(e),
        }
        self.mark(Dirty::EMOJI);
    }

    pub fn apply_custom_status(&mut self, change: Change, s: CustomUserStatus) {
        match change {
            Change::Removed => {
                self.custom_statuses.remove(&s.id);
            }
            _ => {
                self.custom_statuses.insert(s.id.clone(), s);
            }
        }
        self.mark(Dirty::CUSTOM_STATUSES);
    }

    pub fn apply_user_status(
        &mut self,
        uid: &str,
        username: Option<&str>,
        status: UserStatus,
        status_text: Option<&str>,
    ) {
        let m = self.realtime.next();
        self.realtime.presence.insert(uid.to_owned(), m);
        if self.presence.update(uid, username, status, status_text) {
            self.mark(Dirty::PRESENCE);
            if self.user_id() == Some(uid) {
                if let Some(me) = self.me.as_mut() {
                    me.status = Some(status);
                    if let Some(t) = status_text {
                        me.status_text = (!t.is_empty()).then(|| t.to_owned());
                    }
                }
                self.mark(Dirty::ME);
            }
        }
    }

    pub fn apply_connection(&mut self, state: ConnectionState) {
        if self.connection != state {
            if state != ConnectionState::Connected {
                self.typing.clear();
                self.mark(Dirty::TYPING);
            }
            self.connection = state;
            self.mark(Dirty::CONNECTION);
        }
    }

    pub fn apply_logged_out(&mut self) {
        self.apply_connection(ConnectionState::LoggedOut);
    }

    // --- realtime: rooms and subscriptions --------------------------------------

    /// A realtime subscription change. A copy older than the stored one (by
    /// `_updatedAt`) is ignored; ties apply.
    pub fn apply_subscription(&mut self, change: Change, sub: Subscription) {
        let m = self.realtime.next();
        if change == Change::Removed {
            self.realtime.rooms.remove(&sub.rid);
            self.realtime.removed.insert(sub.rid.clone(), m);
            self.remove_subscription(&sub.rid);
            return;
        }
        self.realtime.removed.remove(&sub.rid);
        self.realtime.rooms.insert(sub.rid.clone(), m);
        let rid = sub.rid.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.sub.as_ref()
            && (old == &sub || is_older(sub.updated_at, old.updated_at))
        {
            return;
        }
        entry.sub = Some(sub);
        self.mark_room(&rid);
    }

    /// The user left or was removed: drops the room, its timelines, typing and
    /// pending messages.
    fn remove_subscription(&mut self, rid: &str) {
        if self.rooms.remove(rid).is_some() {
            self.mark_room(rid);
        }
        self.drop_room_timelines(rid);
    }

    fn drop_room_timelines(&mut self, rid: &str) {
        self.outbox.retain(|_, i| i.message.rid != rid);
        self.ephemeral.remove(rid);
        if self.timelines.remove(rid).is_some() {
            self.mark_timeline(rid);
        }
        let threads: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.rid() == rid)
            .map(|(k, _)| k.clone())
            .collect();
        for tmid in threads {
            self.threads.remove(&tmid);
            self.mark_thread(&tmid);
        }
    }

    /// A realtime room change (same rules as [`Self::apply_subscription`]).
    pub fn apply_room(&mut self, change: Change, room: Room) {
        let m = self.realtime.next();
        if change == Change::Removed {
            self.realtime.rooms.remove(&room.id);
            self.realtime.removed.insert(room.id.clone(), m);
            self.remove_room(&room.id);
            return;
        }
        self.realtime.removed.remove(&room.id);
        self.realtime.rooms.insert(room.id.clone(), m);
        let rid = room.id.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.room.as_ref()
            && (old == &room || is_older(room.updated_at, old.updated_at))
        {
            return;
        }
        entry.room = Some(room);
        self.mark_room(&rid);
    }

    /// Drops a room that is only known because it was previewed (no
    /// subscription: opened from the directory or the quick switcher without
    /// joining): the room entry, its timelines and pending messages. Joined
    /// rooms are left alone. Returns whether something was dropped.
    pub fn forget_preview(&mut self, rid: &str) -> bool {
        if !self.rooms.get(rid).is_some_and(|e| e.sub.is_none()) {
            return false;
        }
        self.realtime.rooms.remove(rid);
        self.remove_room(rid);
        true
    }

    fn remove_room(&mut self, rid: &str) {
        if self.rooms.remove(rid).is_some() {
            self.mark_room(rid);
        }
        self.drop_room_timelines(rid);
    }

    // --- realtime: messages -------------------------------------------------------

    /// A new or changed message from the stream (or a REST call result). Updates
    /// the room timeline and the thread timeline if they are open; a parent update
    /// (`tcount`, `tlm`, `replies`) reaches both. Confirms a pending message with
    /// the same `_id`. A newly created message stops its author's typing indicator.
    pub fn apply_message(&mut self, m: Message) {
        self.apply_message_from(m, true);
    }

    fn apply_message_from(&mut self, m: Message, realtime: bool) {
        if m.hidden == Some(true) {
            return;
        }
        let rid = m.rid.clone();
        self.forget_ephemeral(&rid, &m.id);
        if self.outbox.remove(&m.id).is_some() {
            self.mark(Dirty::TIMELINES);
        }
        if realtime && self.is_newly_created(&m) {
            let name = self.activity_name(&m.u).map(str::to_owned);
            if let Some(name) = name
                && self
                    .typing
                    .apply(&rid, m.tmid.as_deref(), &name, &[], Instant::now())
            {
                self.mark(Dirty::TYPING);
            }
        }
        if let Some(tmid) = m.tmid.clone()
            && let Some(t) = self.threads.get_mut(&tmid)
            && t.upsert(m.clone())
        {
            self.mark_thread(&tmid);
        }
        if let Some(t) = self.threads.get_mut(&m.id)
            && t.upsert(m.clone())
        {
            let id = m.id.clone();
            self.mark_thread(&id);
        }
        if let Some(t) = self.timelines.get_mut(&rid)
            && t.upsert(m)
        {
            self.mark_timeline(&rid);
        }
    }

    /// A message that was just created rather than an update of an existing one
    /// (edit, reaction, thread counters, URL preview of an old message).
    fn is_newly_created(&self, m: &Message) -> bool {
        if m.edited_at.is_some() {
            return false;
        }
        let shown = self
            .timelines
            .get(&m.rid)
            .is_some_and(|t| t.contains(&m.id))
            || m.tmid
                .as_ref()
                .and_then(|t| self.threads.get(t))
                .is_some_and(|t| t.contains(&m.id));
        if shown {
            return false;
        }
        match (m.ts, m.updated_at) {
            (Some(ts), Some(u)) => {
                u.timestamp_millis() - ts.timestamp_millis() < NEW_MESSAGE_WINDOW_MS
            }
            _ => true,
        }
    }

    pub fn apply_message_deleted(&mut self, rid: &str, id: &str) {
        self.forget_ephemeral(rid, id);
        if let Some(t) = self.timelines.get_mut(rid)
            && t.remove(id)
        {
            self.mark_timeline(rid);
        }
        let mut changed = Vec::new();
        for (tmid, t) in self.threads.iter_mut().filter(|(_, t)| t.rid() == rid) {
            if t.remove(id) {
                changed.push(tmid.clone());
            }
        }
        for tmid in changed {
            self.mark_thread(&tmid);
        }
    }

    /// Read receipts: messages before `until` (exclusive, like the server's
    /// `ts: {$lt: until}`) are no longer unread.
    pub fn apply_messages_read(&mut self, rid: &str, until: Option<RcDate>, tmid: Option<&str>) {
        let limit = until.map_or(i64::MAX, |d| d.timestamp_millis());
        let mark = |t: &mut Timeline| {
            let ids: Vec<Message> = t
                .messages()
                .filter(|m| m.unread == Some(true))
                .filter(|m| m.ts.is_some_and(|ts| ts.timestamp_millis() < limit))
                .cloned()
                .collect();
            let mut changed = false;
            for mut m in ids {
                m.unread = None;
                changed |= t.upsert(m);
            }
            changed
        };
        match tmid {
            Some(tmid) => {
                if let Some(t) = self.threads.get_mut(tmid)
                    && mark(t)
                {
                    self.mark_thread(tmid);
                }
            }
            None => {
                if let Some(t) = self.timelines.get_mut(rid)
                    && mark(t)
                {
                    self.mark_timeline(rid);
                }
            }
        }
    }

    /// `user-activity` event (empty `activities` = stopped).
    pub fn apply_user_activity(
        &mut self,
        rid: &str,
        username: &str,
        activities: &[String],
        tmid: Option<&str>,
    ) {
        self.apply_user_activity_at(rid, username, activities, tmid, Instant::now());
    }

    pub fn apply_user_activity_at(
        &mut self,
        rid: &str,
        username: &str,
        activities: &[String],
        tmid: Option<&str>,
        now: Instant,
    ) {
        if self.is_my_activity_name(username) {
            return;
        }
        if self.typing.apply(rid, tmid, username, activities, now) {
            self.mark(Dirty::TYPING);
        }
    }

    /// Expires stale typing indicators; schedule with `typing().next_expiry()`.
    pub fn expire_typing(&mut self, now: Instant) {
        if self.typing.expire(now) {
            self.mark(Dirty::TYPING);
        }
    }

    // --- ephemeral messages and UIKit ----------------------------------------------

    /// An ephemeral message (`SessionEvent::EphemeralMessage`): upserted like any
    /// message (by `_id`, room and thread timelines), and flagged as ephemeral
    /// while that version is the one shown. The flag is dropped when a message
    /// with the same `_id` arrives any other way (stream, history, sync), is
    /// deleted, or the room timeline is closed or reset.
    pub fn apply_ephemeral_message(&mut self, m: Message) {
        if m.hidden == Some(true) {
            return;
        }
        let (rid, id) = (m.rid.clone(), m.id.clone());
        let tmid = m.tmid.clone();
        let shown = m.clone();
        self.apply_message_from(m, true);
        let stored = |t: Option<&Timeline>| {
            t.and_then(|t| t.get(&id))
                .is_some_and(|i| i.message == shown)
        };
        let in_room = stored(self.timelines.get(&rid));
        let in_thread = stored(tmid.as_ref().and_then(|t| self.threads.get(t)));
        if in_room || in_thread {
            self.ephemeral.entry(rid.clone()).or_default().insert(id);
            self.mark_timeline(&rid);
        }
    }

    /// Whether the shown version of message `mid` in `rid` arrived as an
    /// ephemeral message (never derived from message fields).
    pub fn is_ephemeral(&self, rid: &str, mid: &str) -> bool {
        self.ephemeral.get(rid).is_some_and(|s| s.contains(mid))
    }

    fn forget_ephemeral(&mut self, rid: &str, id: &str) {
        if let Some(set) = self.ephemeral.get_mut(rid)
            && set.remove(id)
        {
            if set.is_empty() {
                self.ephemeral.remove(rid);
            }
            self.mark_timeline(rid);
        }
    }

    /// UIKit state (open views, triggers, action buttons, app translations).
    pub fn uikit(&self) -> &UiKitStore {
        &self.uikit
    }

    pub fn uikit_mut(&mut self) -> &mut UiKitStore {
        &mut self.uikit
    }

    // --- timelines ----------------------------------------------------------------

    /// Opens (or re-opens) a room timeline, captures the subscription's `ls` for
    /// the unread divider and restores the room's pending messages.
    pub fn open_room(&mut self, rid: &str) -> &Timeline {
        let ls = self.subscription(rid).and_then(|s| s.ls);
        let t = self
            .timelines
            .entry(rid.to_owned())
            .or_insert_with(|| Timeline::new_room(rid));
        t.set_unread_since(ls);
        for item in self.outbox.values() {
            if t.accepts(&item.message) {
                t.restore_pending(item.clone());
            }
        }
        self.mark_timeline(rid);
        &self.timelines[rid]
    }

    /// Reloads a room's history from scratch (e.g. `MessagesImported`): the
    /// open room and thread timelines keep only pending messages and the next
    /// `begin_load_older` / `begin_load_thread` starts over.
    pub fn reset_room_timelines(&mut self, rid: &str) {
        self.ephemeral.remove(rid);
        if let Some(t) = self.timelines.get_mut(rid) {
            t.reset();
            self.mark_timeline(rid);
        }
        let threads: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.rid() == rid)
            .map(|(k, _)| k.clone())
            .collect();
        for tmid in threads {
            if let Some(t) = self.threads.get_mut(&tmid) {
                t.reset();
            }
            self.mark_thread(&tmid);
        }
    }

    /// Forgets a room timeline (memory); it is reloaded on the next open.
    /// Pending messages survive in the outbox.
    pub fn close_room(&mut self, rid: &str) {
        self.ephemeral.remove(rid);
        if self.timelines.remove(rid).is_some() {
            self.mark_timeline(rid);
        }
    }

    /// Opens a thread timeline; the parent is taken from the room timeline if known.
    pub fn open_thread(&mut self, rid: &str, tmid: &str) -> &Timeline {
        let parent = self
            .timelines
            .get(rid)
            .and_then(|t| t.get(tmid))
            .map(|i| i.message.clone());
        let t = self
            .threads
            .entry(tmid.to_owned())
            .or_insert_with(|| Timeline::new_thread(rid, tmid));
        if let Some(p) = parent {
            t.upsert(p);
        }
        for item in self.outbox.values() {
            if t.accepts(&item.message) {
                t.restore_pending(item.clone());
            }
        }
        self.mark_thread(tmid);
        &self.threads[tmid]
    }

    pub fn close_thread(&mut self, tmid: &str) {
        if self.threads.remove(tmid).is_some() {
            self.mark_thread(tmid);
        }
    }

    /// See [`Timeline::begin_load_older`]. Prefer [`Self::begin_load_older_ticket`]:
    /// without a ticket a page requested before a reset or a close/reopen cannot
    /// always be told apart from the current one.
    pub fn begin_load_older(&mut self, rid: &str) -> Option<HistoryCursor> {
        self.begin_load_older_ticket(rid).map(|(c, _)| c)
    }

    /// Starts a history request; pass the ticket to [`Self::finish_load_older_ticket`].
    pub fn begin_load_older_ticket(&mut self, rid: &str) -> Option<(HistoryCursor, PageTicket)> {
        let c = self.timelines.get_mut(rid)?.begin_load_older_ticket();
        if c.is_some() {
            self.mark_timeline(rid);
        }
        c
    }

    /// Completes a history request (dropped when none is outstanding).
    pub fn finish_load_older(
        &mut self,
        rid: &str,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) {
        self.finish_older(rid, None, cursor, result);
    }

    /// Completes [`Self::begin_load_older_ticket`]; a stale result (the timeline
    /// was reset, closed or replaced meanwhile) is dropped. Returns whether it
    /// was applied.
    pub fn finish_load_older_ticket(
        &mut self,
        rid: &str,
        ticket: PageTicket,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        self.finish_older(rid, Some(ticket), cursor, result)
    }

    fn finish_older(
        &mut self,
        rid: &str,
        ticket: Option<PageTicket>,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        let watermark = self.room_watermark(rid);
        let Some(t) = self.timelines.get_mut(rid) else {
            return false;
        };
        let ids: Vec<String> = result
            .as_ref()
            .map(|ms| ms.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let applied = match ticket {
            Some(tk) => t.finish_load_older_ticket(tk, cursor, result),
            None => t.finish_load_older(cursor, result),
        };
        if applied {
            if let Some(w) = watermark {
                t.set_sync_watermark_if_missing(w);
            }
            self.confirm_delivered(&ids);
            self.mark_timeline(rid);
        }
        applied
    }

    /// A server time not later than now that bounds the room's history, used as
    /// the sync watermark of a timeline without messages.
    fn room_watermark(&self, rid: &str) -> Option<RcDate> {
        let e = self.rooms.get(rid);
        let r = e.and_then(|e| e.room.as_ref());
        let s = e.and_then(|e| e.sub.as_ref());
        [
            r.and_then(|r| r.updated_at),
            r.and_then(|r| r.lm),
            s.and_then(|s| s.updated_at),
            s.and_then(|s| s.ts),
        ]
        .into_iter()
        .flatten()
        .max()
        .or_else(|| Some(RcDate(RcDate::now().0 - crate::sync::CLOCK_SKEW_MARGIN)))
    }

    /// Server copies of pending messages arrived (e.g. in a page): they are sent.
    /// Persisted copies also end the ephemeral trust of a message with that id.
    fn confirm_delivered(&mut self, ids: &[String]) {
        if !self.ephemeral.is_empty() {
            let mut changed = false;
            for set in self.ephemeral.values_mut() {
                for id in ids {
                    changed |= set.remove(id);
                }
            }
            self.ephemeral.retain(|_, s| !s.is_empty());
            if changed {
                self.mark(Dirty::TIMELINES);
            }
        }
        let before = self.outbox.len();
        for id in ids {
            self.outbox.remove(id);
        }
        if self.outbox.len() != before {
            self.mark(Dirty::TIMELINES);
        }
    }

    /// See [`Timeline::begin_load_thread`]; prefer [`Self::begin_load_thread_ticket`].
    pub fn begin_load_thread(&mut self, tmid: &str) -> Option<u64> {
        self.begin_load_thread_ticket(tmid).map(|(o, _)| o)
    }

    /// Starts a thread page request; pass the ticket to [`Self::finish_load_thread_ticket`].
    /// The offset counts from the newest reply: request it with
    /// `RestClient::thread_messages_latest` (not the oldest-first
    /// `thread_messages`).
    pub fn begin_load_thread_ticket(&mut self, tmid: &str) -> Option<(u64, PageTicket)> {
        let o = self.threads.get_mut(tmid)?.begin_load_thread_ticket();
        if o.is_some() {
            self.mark_thread(tmid);
        }
        o
    }

    /// Completes a thread page request (dropped when none is outstanding).
    pub fn finish_load_thread(&mut self, tmid: &str, result: Result<Paginated<Message>, String>) {
        self.finish_thread(tmid, None, result);
    }

    /// Completes [`Self::begin_load_thread_ticket`]; a stale result is dropped.
    pub fn finish_load_thread_ticket(
        &mut self,
        tmid: &str,
        ticket: PageTicket,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        self.finish_thread(tmid, Some(ticket), result)
    }

    fn finish_thread(
        &mut self,
        tmid: &str,
        ticket: Option<PageTicket>,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        let rid = self.threads.get(tmid).map(|t| t.rid().to_owned());
        let watermark = rid.as_deref().and_then(|r| self.room_watermark(r));
        let Some(t) = self.threads.get_mut(tmid) else {
            return false;
        };
        let ids: Vec<String> = result
            .as_ref()
            .map(|p| p.items.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let applied = match ticket {
            Some(tk) => t.finish_load_thread_ticket(tk, result),
            None => t.finish_load_thread(result),
        };
        if applied {
            if let Some(w) = watermark {
                t.set_sync_watermark_if_missing(w);
            }
            self.confirm_delivered(&ids);
            self.mark_thread(tmid);
        }
        applied
    }

    /// Builds a local message for the composer (client `_id`, author = me, ts = now).
    pub fn new_local_message(
        &self,
        rid: &str,
        text: &str,
        tmid: Option<&str>,
        tshow: bool,
    ) -> Message {
        let u = self
            .me
            .as_ref()
            .map(|m| rocketchat::models::UserRef {
                id: m.id.clone(),
                username: m.username.clone(),
                name: m.name.clone(),
            })
            .unwrap_or_default();
        Message {
            id: rocketchat::ids::random_id(),
            rid: rid.to_owned(),
            msg: text.to_owned(),
            ts: Some(RcDate::now()),
            u,
            tmid: tmid.map(str::to_owned),
            tshow: (tmid.is_some() && tshow).then_some(true),
            temp: Some(true),
            ..Default::default()
        }
    }

    /// Adds a pending message to the outbox and the open room/thread timelines.
    pub fn add_pending(&mut self, m: Message) {
        self.outbox.insert(
            m.id.clone(),
            TimelineItem {
                message: m.clone(),
                state: DeliveryState::Sending,
            },
        );
        self.mark(Dirty::TIMELINES);
        if let Some(tmid) = m.tmid.clone()
            && let Some(t) = self.threads.get_mut(&tmid)
            && t.insert_pending(m.clone())
        {
            self.mark_thread(&tmid);
        }
        let rid = m.rid.clone();
        if let Some(t) = self.timelines.get_mut(&rid)
            && t.accepts(&m)
            && t.insert_pending(m)
        {
            self.mark_timeline(&rid);
        }
    }

    fn for_pending(&mut self, rid: &str, tmid: Option<&str>, f: impl Fn(&mut Timeline) -> bool) {
        if let Some(tmid) = tmid
            && let Some(t) = self.threads.get_mut(tmid)
            && f(t)
        {
            self.mark_thread(tmid);
        }
        if let Some(t) = self.timelines.get_mut(rid)
            && f(t)
        {
            self.mark_timeline(rid);
        }
    }

    fn set_outbox_state(&mut self, id: &str, state: DeliveryState) {
        if let Some(item) = self.outbox.get_mut(id)
            && item.state != state
        {
            item.state = state;
            self.mark(Dirty::TIMELINES);
        }
    }

    pub fn mark_pending_failed(&mut self, rid: &str, tmid: Option<&str>, id: &str, error: &str) {
        self.set_outbox_state(
            id,
            DeliveryState::Failed {
                error: error.to_owned(),
            },
        );
        self.for_pending(rid, tmid, |t| t.mark_failed(id, error));
    }

    pub fn mark_pending_sending(&mut self, rid: &str, tmid: Option<&str>, id: &str) {
        self.set_outbox_state(id, DeliveryState::Sending);
        self.for_pending(rid, tmid, |t| t.mark_sending(id));
    }

    pub fn discard_pending(&mut self, rid: &str, tmid: Option<&str>, id: &str) {
        if self.outbox.remove(id).is_some() {
            self.mark(Dirty::TIMELINES);
        }
        self.for_pending(rid, tmid, |t| t.discard_pending(id));
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
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{fixture_workspace, me, msg, room, sub, ts};

    #[test]
    fn initial_from_fixtures() {
        let mut ws = fixture_workspace();
        assert!(ws.me().is_some());
        assert!(ws.rooms().len() >= 3);
        assert!(ws.room("GENERAL").unwrap().sub.is_some());
        assert!(!ws.permissions().is_empty());
        let c = ws.take_changes();
        assert!(c.areas.contains(Dirty::ROOMS | Dirty::SETTINGS));
        assert!(ws.take_changes().is_empty());
    }

    #[test]
    fn subscription_and_room_changes() {
        let mut ws = Workspace::new();
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        let c = ws.take_changes();
        assert!(c.areas.contains(Dirty::ROOMS));
        assert!(c.rooms.contains("r1"));
        // Same data again: no change.
        ws.apply_subscription(Change::Updated, sub("r1", "one", RoomType::Channel));
        assert!(ws.take_changes().is_empty());
        let rev = ws.revision(Dirty::ROOMS);
        ws.apply_room(Change::Inserted, room("r1", "one", RoomType::Channel));
        assert!(ws.revision(Dirty::ROOMS) > rev);
        assert!(ws.room("r1").unwrap().room.is_some());
        ws.open_room("r1");
        ws.apply_subscription(Change::Removed, sub("r1", "one", RoomType::Channel));
        assert!(ws.room("r1").is_none());
        assert!(ws.timeline("r1").is_none());
    }

    #[test]
    fn previewed_rooms_are_forgotten_joined_ones_kept() {
        let mut ws = Workspace::new();
        ws.apply_room(Change::Updated, room("p1", "preview", RoomType::Channel));
        ws.open_room("p1");
        ws.apply_message(msg("m1", "p1", 1000, "u"));
        ws.take_changes();
        assert!(ws.forget_preview("p1"));
        assert!(ws.room("p1").is_none());
        assert!(ws.timeline("p1").is_none());
        assert!(ws.take_changes().rooms.contains("p1"));
        // Joined: kept.
        ws.apply_room(Change::Updated, room("r1", "one", RoomType::Channel));
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        assert!(!ws.forget_preview("r1"));
        assert!(ws.room("r1").is_some());
        assert!(!ws.forget_preview("unknown"));
    }

    #[test]
    fn messages_go_to_open_timelines_only() {
        let mut ws = Workspace::new();
        ws.apply_message(msg("m0", "r1", 1, "u"));
        assert!(ws.timeline("r1").is_none());
        let mut s = sub("r1", "one", RoomType::Channel);
        s.ls = Some(ts(1500));
        ws.apply_subscription(Change::Inserted, s);
        ws.open_room("r1");
        ws.take_changes();
        ws.apply_message(msg("m1", "r1", 1000, "u"));
        ws.apply_message(msg("m2", "r1", 2000, "u"));
        let c = ws.take_changes();
        assert!(c.timelines.contains("r1"));
        let t = ws.timeline("r1").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t.unread_divider("me"), Some("m2"));
        ws.apply_message_deleted("r1", "m1");
        assert_eq!(ws.timeline("r1").unwrap().len(), 1);
    }

    #[test]
    fn threads() {
        let mut ws = Workspace::new();
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        ws.open_room("r1");
        ws.apply_message(msg("p", "r1", 1000, "u"));
        ws.open_thread("r1", "p");
        assert_eq!(ws.thread("p").unwrap().parent().unwrap().id, "p");
        let mut reply = msg("c1", "r1", 2000, "v");
        reply.tmid = Some("p".into());
        ws.apply_message(reply);
        let mut parent = msg("p", "r1", 1000, "u");
        parent.tcount = Some(1);
        parent.replies = Some(vec!["v".into()]);
        ws.apply_message(parent);
        let c = ws.take_changes();
        assert!(c.threads.contains("p"));
        assert_eq!(ws.thread("p").unwrap().len(), 1);
        assert_eq!(ws.thread("p").unwrap().parent().unwrap().tcount, Some(1));
        let room = ws.timeline("r1").unwrap();
        assert_eq!(room.len(), 1, "reply not in main timeline");
        assert_eq!(room.get("p").unwrap().message.tcount, Some(1));
        ws.apply_message_deleted("r1", "c1");
        assert!(ws.thread("p").unwrap().is_empty());
    }

    #[test]
    fn pending_flow() {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        ws.open_room("r1");
        let local = ws.new_local_message("r1", "hi", None, false);
        let id = local.id.clone();
        ws.add_pending(local.clone());
        assert!(ws.timeline("r1").unwrap().get(&id).unwrap().is_pending());
        ws.mark_pending_failed("r1", None, &id, "offline");
        ws.mark_pending_sending("r1", None, &id);
        let mut echo = local;
        echo.temp = None;
        ws.apply_message(echo);
        assert!(!ws.timeline("r1").unwrap().get(&id).unwrap().is_pending());
    }

    #[test]
    fn typing_and_presence() {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        let now = Instant::now();
        ws.apply_user_activity_at("r1", "me", &["user-typing".into()], None, now);
        assert!(
            ws.typing_label("r1", None, now).is_none(),
            "own activity ignored"
        );
        ws.apply_user_activity_at("r1", "bob", &["user-typing".into()], None, now);
        assert_eq!(ws.typing_label("r1", None, now).unwrap().usernames, ["bob"]);
        // Bob's message stops his typing.
        let mut m = msg("m", "r1", 1, "bobid");
        m.u.username = Some("bob".into());
        ws.apply_message(m);
        assert!(ws.typing_label("r1", None, now).is_none());

        ws.apply_user_status("me", Some("me"), UserStatus::Busy, Some("focus"));
        assert_eq!(ws.my_status(), UserStatus::Busy);
        assert_eq!(ws.me().unwrap().status_text.as_deref(), Some("focus"));
        assert!(
            ws.take_changes()
                .areas
                .contains(Dirty::PRESENCE | Dirty::ME | Dirty::TYPING)
        );
    }

    /// Settings, permissions and roles changed in realtime while a snapshot
    /// loaded are not overwritten by it; older changes are.
    #[test]
    fn realtime_config_survives_snapshots() {
        let mut ws = Workspace::new();
        let before = Instant::now();
        // Changed before the snapshot started: the snapshot wins.
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: "Old".into(),
                value: serde_json::json!("realtime"),
                ..Default::default()
            },
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let started = Instant::now();
        assert!(started > before);
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: "Site_Name".into(),
                value: serde_json::json!("New name"),
                ..Default::default()
            },
        );
        ws.apply_permission(
            Change::Updated,
            Permission {
                id: "create-c".into(),
                roles: vec!["admin".into(), "user".into()],
                ..Default::default()
            },
        );
        ws.apply_role(
            Change::Removed,
            Role {
                id: "moderator".into(),
                ..Default::default()
            },
        );
        let data = InitialData {
            me: me("me", "me"),
            settings: vec![
                Setting {
                    id: "Site_Name".into(),
                    value: serde_json::json!("Stale"),
                    ..Default::default()
                },
                Setting {
                    id: "Old".into(),
                    value: serde_json::json!("snapshot"),
                    ..Default::default()
                },
            ],
            permissions: vec![Permission {
                id: "create-c".into(),
                roles: vec!["admin".into()],
                ..Default::default()
            }],
            roles: vec![Role {
                id: "moderator".into(),
                name: "Moderator".into(),
                ..Default::default()
            }],
            started_at: Some(started),
            ..Default::default()
        };
        ws.apply_initial(data);
        assert_eq!(ws.settings().str("Site_Name"), Some("New name"));
        assert_eq!(ws.settings().str("Old"), Some("snapshot"));
        assert_eq!(
            ws.permission_roles("create-c"),
            Some(&["admin".to_owned(), "user".to_owned()][..])
        );
        assert!(!ws.roles().contains_key("moderator"));
        // A later snapshot (started after those changes) wins again.
        let data = InitialData {
            me: me("me", "me"),
            settings: vec![Setting {
                id: "Site_Name".into(),
                value: serde_json::json!("Fresh"),
                ..Default::default()
            }],
            started_at: Some(Instant::now()),
            ..Default::default()
        };
        ws.apply_initial(data);
        assert_eq!(ws.settings().str("Site_Name"), Some("Fresh"));
    }

    #[test]
    fn delta_removes_by_subscription_id() {
        let mut ws = Workspace::new();
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        ws.apply_subscription(Change::Inserted, sub("r2", "two", RoomType::Channel));
        // The delta request starts after these realtime inserts.
        let mut d = DeltaData {
            next_since: Some(ts(99)),
            started_at: Some(Instant::now()),
            ..Default::default()
        };
        d.subscriptions.remove.push(rocketchat::models::Removed {
            id: "sub-r1".into(),
            rid: None,
            deleted_at: None,
        });
        d.rooms.remove.push(rocketchat::models::Removed {
            id: "r2".into(),
            rid: None,
            deleted_at: None,
        });
        ws.apply_delta(d);
        assert!(ws.rooms().is_empty());
        assert_eq!(ws.last_sync(), Some(ts(99)));
    }

    #[test]
    fn room_sync_and_read_receipts() {
        let mut ws = Workspace::new();
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        ws.open_room("r1");
        let mut a = msg("a", "r1", 1000, "u");
        a.unread = Some(true);
        let mut b = msg("b", "r1", 2000, "u");
        b.unread = Some(true);
        ws.apply_room_sync(RoomSync {
            rid: "r1".into(),
            updated: vec![a, b, msg("c", "r1", 3000, "u")],
            deleted: vec!["c".into()],
            dropped: 0,
        });
        assert_eq!(ws.timeline("r1").unwrap().len(), 2);
        ws.apply_messages_read("r1", Some(ts(1001)), None);
        let t = ws.timeline("r1").unwrap();
        assert_eq!(t.get("a").unwrap().message.unread, None);
        assert_eq!(t.get("b").unwrap().message.unread, Some(true));
    }

    #[test]
    fn drafts_and_settings() {
        let mut ws = Workspace::new();
        ws.set_draft("r1", None, "hello");
        ws.set_draft("r1", Some("t"), "in thread");
        assert_eq!(ws.draft("r1", None), Some("hello"));
        assert_eq!(ws.draft("r1", Some("t")), Some("in thread"));
        ws.set_draft("r1", None, "  ");
        assert_eq!(ws.draft("r1", None), None);
        assert_eq!(ws.take_draft("r1", Some("t")).as_deref(), Some("in thread"));

        ws.apply_setting(
            Change::Updated,
            Setting {
                id: "UI_Use_Real_Name".into(),
                value: serde_json::json!(true),
                ..Default::default()
            },
        );
        assert!(ws.use_real_names(None));
        assert!(!ws.use_real_names(Some(false)));
    }

    #[test]
    fn role_changes() {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        ws.apply_subscription(Change::Inserted, sub("r1", "one", RoomType::Channel));
        ws.apply_user_role_change("me", "admin", true, Some("Users"));
        assert!(ws.me().unwrap().has_role("admin"));
        ws.apply_user_role_change("me", "owner", true, Some("r1"));
        assert_eq!(
            ws.subscription("r1").unwrap().roles.as_deref(),
            Some(&["owner".to_owned()][..])
        );
        ws.apply_user_role_change("me", "admin", false, None);
        assert!(!ws.me().unwrap().has_role("admin"));
        ws.apply_user_role_change("other", "admin", true, None);
        assert!(!ws.me().unwrap().has_role("admin"));
    }

    #[test]
    fn connection_clears_typing() {
        let mut ws = Workspace::new();
        let now = Instant::now();
        ws.apply_user_activity_at("r", "bob", &["user-typing".into()], None, now);
        ws.apply_connection(ConnectionState::Connected);
        ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
        assert!(ws.typing_label("r", None, now).is_none());
        ws.apply_logged_out();
        assert_eq!(ws.connection(), &ConnectionState::LoggedOut);
    }
}
