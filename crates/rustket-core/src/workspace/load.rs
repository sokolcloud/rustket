use std::collections::HashSet;
use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::{Message, Room, Subscription, User, UserStatus};

use crate::settings::Settings;
use crate::sync::{DeltaData, InitialData, RoomSync};
use crate::timeline::TimelineItem;

use super::{Change, Dirty, ParentSource, RealtimeMarks, Workspace, is_newer};

impl Workspace {
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
            // Its name before it is cleared (scheduled messages dropped with
            // the room are reported by name).
            let label = e.fname().unwrap_or(e.name()).to_owned();
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
                gone.push((rid.clone(), label));
            }
        }
        for (rid, label) in gone {
            self.remove_room_labelled(&rid, label);
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
        self.from_cache = false;
        self.cached_rooms.clear();
        let rooms = &self.rooms;
        let lr = |rid: &str| {
            rooms
                .get(rid)
                .and_then(|e| e.sub.as_ref())
                .and_then(|s| s.lr)
        };
        if self.thread_index.confirm_restored(lr) {
            self.mark(Dirty::THREAD_INDEX);
        }
        // Service rooms the snapshot dropped.
        self.refresh_service_data();
        self.realtime.snapshot_applied();
        self.changes.rooms.extend(self.rooms.keys().cloned());
        self.mark(Dirty::ALL);
    }

    // --- offline cache ----------------------------------------------------------

    /// Shows the offline cache of this account until the server's initial
    /// load arrives ([`Self::from_cache`]). Only applies to a workspace that
    /// holds no server data yet (no initial load, no rooms); values the
    /// session already delivered (`me`, settings) are kept. Presence is not
    /// cached. The initial load then replaces every cached room and
    /// subscription (also on an `_updatedAt` tie) and drops those the server
    /// no longer lists. Returns whether the cache was applied.
    pub fn apply_cached(&mut self, state: crate::cache::CachedState) -> bool {
        if self.last_sync.is_some() || !self.rooms.is_empty() {
            return false;
        }
        let crate::cache::CachedState {
            me,
            server_info,
            settings,
            permissions,
            roles,
            subscriptions,
            rooms,
            custom_emoji,
            custom_statuses,
            saved_at: _,
            threads,
            later,
        } = state;
        if self.me.is_none() {
            // Not connected: the own status is not known.
            self.me = me.map(|mut m| {
                m.status = Some(UserStatus::Offline);
                m
            });
        }
        if self.server_info.is_none() {
            self.server_info = server_info;
        }
        if self.settings.is_empty() {
            self.settings = Settings::from_list(settings);
        }
        if self.permissions.is_empty() {
            self.permissions = permissions.into_iter().map(|p| (p.id, p.roles)).collect();
        }
        if self.roles.is_empty() {
            self.roles = roles.into_iter().map(|r| (r.id.clone(), r)).collect();
        }
        if self.emoji.is_empty() {
            self.emoji.set_all(custom_emoji);
        }
        if self.custom_statuses.is_empty() {
            self.custom_statuses = custom_statuses
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect();
        }
        for s in subscriptions {
            let rid = s.rid.clone();
            self.rooms.entry(rid.clone()).or_default().sub = Some(s);
            self.cached_rooms.insert(rid);
        }
        for r in rooms {
            let rid = r.id.clone();
            self.rooms.entry(rid.clone()).or_default().room = Some(r);
            self.cached_rooms.insert(rid);
        }
        // Flagged threads are pinned before the threads are restored.
        self.refresh_service_data();
        // The settings may have come with the cache: stars are restored
        // only while pins are on.
        self.apply_starred_threads();
        self.restore_threads(threads);
        self.apply_later_tracking();
        self.restore_later(later);
        self.from_cache = true;
        self.changes.rooms.extend(self.rooms.keys().cloned());
        self.mark(Dirty::ALL);
        true
    }

    /// The workspace shows the offline cache: the server's initial load has
    /// not arrived yet. Automatic read marks wait until it has.
    pub fn from_cache(&self) -> bool {
        self.from_cache
    }

    /// Shows cached messages in the open room (`tmid: None`) or thread
    /// timeline before its first page arrived (see [`Timeline::seed_cached`]).
    /// Returns whether anything was added.
    pub fn seed_cached_messages(
        &mut self,
        rid: &str,
        tmid: Option<&str>,
        parent: Option<Message>,
        messages: Vec<Message>,
    ) -> bool {
        match tmid {
            None => {
                let changed = self
                    .timelines
                    .get_mut(rid)
                    .is_some_and(|t| t.seed_cached(None, messages));
                if changed {
                    self.mark_timeline(rid);
                }
                changed
            }
            Some(tmid) => {
                if let Some(p) = parent.as_ref().filter(|p| p.id == tmid && p.rid == rid) {
                    self.index_parents_from(std::slice::from_ref(p), ParentSource::Cached);
                }
                let changed = self
                    .threads
                    .get_mut(tmid)
                    .filter(|t| t.rid() == rid)
                    .is_some_and(|t| t.seed_cached(parent, messages));
                if changed {
                    self.mark_thread(tmid);
                }
                changed
            }
        }
    }

    /// Every local message not yet confirmed by the server (all rooms).
    pub fn outbox_items(&self) -> impl Iterator<Item = &TimelineItem> + '_ {
        self.outbox.values()
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
        let cached = self.cached_rooms.contains(&rid);
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.sub.as_ref()
            && old.updated_at.is_some()
            && !cached
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
        let cached = self.cached_rooms.contains(&rid);
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.room.as_ref()
            && old.updated_at.is_some()
            && !cached
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
            // The user, or the service rooms their preferences bind, changed.
            self.refresh_service_data();
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
}
