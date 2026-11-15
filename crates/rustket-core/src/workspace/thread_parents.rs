use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::Message;

use crate::flags::{FlagBlob, FlagColor, Flags};
use crate::sidebar_layout::ThreadsMode;
use crate::thread_index::{ThreadIndex, follows};

use super::{Dirty, ParentSource, RoomEntry, Workspace, starred_parent};

impl Workspace {
    // --- flags -------------------------------------------------------------------------

    /// The effective flags: the stored blob with this device's pending edits.
    pub fn flags(&self) -> &Flags {
        &self.flags
    }

    pub fn room_flag(&self, rid: &str) -> Option<FlagColor> {
        self.flags.room(rid)
    }

    pub fn thread_flag(&self, tmid: &str) -> Option<FlagColor> {
        self.flags.thread(tmid)
    }

    /// The flags blob of the user's service rooms, as the subscriptions
    /// last showed it (live and from the offline cache).
    pub fn flag_blob(&self) -> &FlagBlob {
        &self.flag_blob
    }

    /// The pending edits set with [`Self::set_flag_overlay`].
    pub fn flag_overlay(&self) -> &FlagBlob {
        &self.flag_overlay
    }

    /// This device's flag edits not known to be stored yet (shown right
    /// away). Entries the stored blob already holds change nothing, so the
    /// overlay can be pruned at any time.
    pub fn set_flag_overlay(&mut self, overlay: FlagBlob) {
        if self.flag_overlay == overlay {
            return;
        }
        self.flag_overlay = overlay;
        self.mark(Dirty::FLAGS);
        self.update_flags();
    }

    /// The user's own service rooms (strictly: a private group the user
    /// created and alone is in, whose marker names the user), the primary
    /// one first.
    pub fn service_rooms(&self) -> &[String] {
        &self.service_rooms
    }

    /// Whether `rid` is the user's own service room (see
    /// [`RoomEntry::is_service_room`]).
    pub fn is_service_room(&self, rid: &str) -> bool {
        self.rooms.get(rid).is_some_and(RoomEntry::is_service_room)
    }

    /// Reads the flags blob from the user's service rooms: those the account
    /// bound in its preferences (`rocketchat::models::ServiceBinding`).
    pub(super) fn refresh_flags(&mut self) {
        let (rids, blob) = match self.me.as_ref() {
            Some(me) => {
                let subs = rocketchat::models::trusted_service_subscriptions(
                    self.rooms
                        .values()
                        .filter_map(|e| e.sub.as_ref().zip(e.room.as_ref())),
                    &me.id,
                    &rocketchat::models::ServiceBinding::of(me),
                );
                let mut blob = FlagBlob::default();
                for s in &subs {
                    if let Some(f) = s.service_fields() {
                        blob.merge(&FlagBlob::from_value(f.get(rocketchat::models::FLAGS_KEY)));
                    }
                }
                (subs.into_iter().map(|s| s.rid.clone()).collect(), blob)
            }
            None => (Vec::new(), FlagBlob::default()),
        };
        self.service_rooms = rids;
        if self.flag_blob != blob {
            self.flag_blob = blob;
            self.mark(Dirty::FLAGS);
            self.update_flags();
        }
    }

    /// Recomputes the effective flags and the pinned flagged threads.
    fn update_flags(&mut self) {
        let flags = self.flag_blob.merged(&self.flag_overlay).view();
        if flags == self.flags {
            return;
        }
        let flagged = flags
            .threads()
            .map(|(rid, tmid, _)| (tmid.to_owned(), rid.to_owned()))
            .collect();
        self.flags = flags;
        self.mark(Dirty::FLAGS);
        if self.thread_index.set_flagged(flagged) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    // --- sidebar threads -------------------------------------------------------------

    /// Thread parents and followed sets behind the sidebar's Threads section.
    pub fn thread_index(&self) -> &ThreadIndex {
        &self.thread_index
    }

    /// Mirrors the Threads mode and the signed-in account's kept threads
    /// (`(rid, tmid)`) from the config. Cheap when nothing changed; call it on
    /// every update.
    pub fn set_thread_config<'k>(
        &mut self,
        mode: ThreadsMode,
        kept: impl IntoIterator<Item = (&'k str, &'k str)>,
    ) {
        let kept = kept
            .into_iter()
            .map(|(rid, tmid)| (tmid.to_owned(), rid.to_owned()))
            .collect();
        let mut changed = self.thread_index.set_mode(mode);
        changed |= self.thread_index.set_kept(kept);
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Mirrors the "starred threads in Favorites" switch. Tracking runs only
    /// while it is on and the server allows starring and threads; call it on
    /// every update (the server settings may change).
    pub fn set_starred_threads(&mut self, on: bool) {
        self.starred_threads = on;
        self.apply_starred_threads();
    }

    pub(super) fn apply_starred_threads(&mut self) {
        let on = self.starred_threads
            && self.settings.allow_starring()
            && self.settings.threads_enabled();
        if self.thread_index.set_pins(on) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// The user starred or unstarred the parent of thread `tmid`: applied
    /// right away (the realtime echo confirms it; revert on error).
    pub fn set_thread_starred(&mut self, rid: &str, tmid: &str, on: bool) {
        if self.thread_index.set_starred(rid, tmid, on, Instant::now()) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// A page of a `chat.getStarredMessages` scan of `rid` requested at
    /// `started`: thread parents are indexed (their star state applies unless
    /// it changed since `started`). Returns the starred thread parents of the
    /// page (collect them over every page for
    /// [`finish_starred_scan`](Self::finish_starred_scan)).
    pub fn apply_starred_page(
        &mut self,
        rid: &str,
        msgs: Vec<Message>,
        started: Instant,
    ) -> Vec<String> {
        if !self.thread_index.pins_enabled() {
            return Vec::new();
        }
        let me = self.user_id().map(str::to_owned);
        let mut found = Vec::new();
        let mut changed = false;
        for m in msgs {
            if m.rid != rid
                || m.tmid.is_some()
                || (m.tcount.unwrap_or(0) == 0 && self.thread_index.parent(&m.id).is_none())
            {
                continue;
            }
            // A newer stored copy (or a hide or deletion seen since) decides.
            let current = if self.thread_index.has_newer(&m) {
                self.thread_index.parent(&m.id)
            } else {
                Some(&m)
            };
            if current.is_some_and(|c| c.hidden != Some(true) && starred_parent(c, me.as_deref())) {
                found.push(m.id.clone());
            }
            // A star change seen since the scan started wins over its copy.
            let source = if self.thread_index.star_changed_since(&m.id, started) {
                ParentSource::Cached
            } else {
                ParentSource::Fetched
            };
            changed |= self.index_parent(m, source);
        }
        changed |= self.evict_thread_parents();
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
        found
    }

    /// A complete `chat.getStarredMessages` scan of `rid`, requested at
    /// `started` while the room's last message was `lm`: `found` (from
    /// [`apply_starred_page`](Self::apply_starred_page)) replaces the room's
    /// starred threads, keeping star changes applied since `started`.
    pub fn finish_starred_scan(
        &mut self,
        rid: &str,
        found: impl IntoIterator<Item = String>,
        started: Instant,
        lm: Option<RcDate>,
    ) {
        let scan = crate::thread_index::StarredScan {
            scanned_at: Some(RcDate::now()),
            lm,
        };
        if self
            .thread_index
            .finish_starred_scan(rid, found.into_iter().collect(), started, scan)
        {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Forgets a room's starred scan (scanned again).
    pub fn forget_starred_scan(&mut self, rid: &str) {
        if self.thread_index.forget_starred_scan(rid) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Thread parents fetched over REST (unread, kept or starred threads, a
    /// single `chat.getMessage`). Hidden ones are gone; older copies than the
    /// stored ones are ignored.
    pub fn apply_thread_parents(&mut self, msgs: Vec<Message>) {
        if !self.thread_index.indexing() {
            return;
        }
        let mut changed = false;
        for m in msgs.into_iter().filter(|m| m.tmid.is_none()) {
            changed |= self.index_parent(m, ParentSource::Fetched);
        }
        changed |= self.evict_thread_parents();
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// A `chat.getThreadsList(type=following)` result for `rid`, requested at
    /// `started` while the room's activity was `synced_activity`. Replaces the
    /// room's followed set, keeping follows and unfollows seen live since
    /// `started`. Ignored outside Following mode.
    pub fn apply_followed(
        &mut self,
        rid: &str,
        msgs: Vec<Message>,
        synced_activity: Option<RcDate>,
        started: Instant,
    ) {
        self.apply_followed_page(rid, msgs, synced_activity, started, None);
    }

    /// [`apply_followed`](Self::apply_followed) of a page of `short` threads
    /// when the room had more (`None`: the result is complete).
    pub fn apply_followed_page(
        &mut self,
        rid: &str,
        msgs: Vec<Message>,
        synced_activity: Option<RcDate>,
        started: Instant,
        short: Option<usize>,
    ) {
        if self.thread_index.mode() != ThreadsMode::Following {
            return;
        }
        let me = self.user_id().map(str::to_owned);
        let mut set = std::collections::BTreeSet::new();
        let mut changed = false;
        for m in msgs {
            if m.rid != rid || m.tmid.is_some() {
                continue;
            }
            // A newer stored copy (or a hide or deletion seen since) decides.
            let current = if self.thread_index.has_newer(&m) {
                self.thread_index.parent(&m.id)
            } else {
                Some(&m)
            };
            if current.is_some_and(|c| {
                c.hidden != Some(true)
                    && !crate::thread_index::is_removed(c)
                    && follows(c, me.as_deref())
            }) {
                set.insert(m.id.clone());
            }
            changed |= self.index_parent(m, ParentSource::Fetched);
        }
        changed |= self
            .thread_index
            .set_followed(rid, set, synced_activity, started, short);
        changed |= self.evict_thread_parents();
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// The parent of `tmid` is not available (not found): its rows show it;
    /// it is no longer starred. Flags stay (only the user clears them: a
    /// transient error here must not remove them on every device).
    pub fn thread_gone(&mut self, tmid: &str) {
        if self.thread_index.indexing()
            && self.thread_index.mark_gone_unstarred(tmid, Instant::now())
        {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Thread data of the offline cache, for subscribed rooms (see
    /// `ThreadIndex::restore`).
    pub(super) fn restore_threads(&mut self, t: crate::cache::CachedThreads) {
        let subscribed = |rid: &str| self.rooms.get(rid).is_some_and(|e| e.sub.is_some());
        let parents: Vec<Message> = t
            .parents
            .into_iter()
            .filter(|m| m.tmid.is_none() && m.hidden != Some(true) && subscribed(&m.rid))
            .collect();
        let followed: Vec<_> = t
            .followed
            .into_iter()
            .filter(|f| subscribed(&f.rid))
            .map(|f| crate::thread_index::RestoredFollowed {
                rid: f.rid,
                tmids: f.tmids.into_iter().collect(),
                synced_activity: f.synced_activity,
                short: f.short,
            })
            .collect();
        let starred: Vec<_> = t
            .starred
            .into_iter()
            .filter(|s| subscribed(&s.rid))
            .map(|s| crate::thread_index::RestoredStarred {
                scan: s.scanned_at.map(|at| crate::thread_index::StarredScan {
                    scanned_at: Some(at),
                    lm: s.scanned_lm,
                }),
                rid: s.rid,
                tmids: s.tmids.into_iter().collect(),
            })
            .collect();
        let synced: HashMap<String, Option<RcDate>> = t
            .parents_synced
            .into_iter()
            .map(|p| (p.rid, p.synced_activity))
            .collect();
        // A room without a mark: not known to be current.
        let synced = |rid: &str| synced.get(rid).copied().flatten();
        if self
            .thread_index
            .restore(parents, followed, starred, synced)
        {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// A room's followed set may be out of date: crawled again.
    pub fn mark_followed_threads_stale(&mut self, rid: &str) {
        if self.thread_index.mark_followed_stale(rid) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Forgets one room's followed set (crawled again).
    pub fn forget_followed_threads(&mut self, rid: &str) {
        if self.thread_index.forget_followed(rid) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Whether `tmid` is in any `tunread*` list of the room's subscription.
    fn is_unread_thread(&self, rid: &str, tmid: &str) -> bool {
        self.subscription(rid).is_some_and(|s| {
            [&s.tunread, &s.tunread_user, &s.tunread_group]
                .into_iter()
                .any(|v| v.as_ref().is_some_and(|v| v.iter().any(|t| t == tmid)))
        })
    }

    /// A row source refers to `m` (as a parent): stored already, starred
    /// (pins on), or with the Threads section on: kept, unread, its thread
    /// is open, or (Following) the user follows it.
    fn thread_relevant(&self, m: &Message) -> bool {
        let idx = &self.thread_index;
        idx.parent(&m.id).is_some()
            || idx.is_pinned(&m.id)
            || (idx.pins_enabled() && starred_parent(m, self.user_id()))
            || (idx.enabled()
                && (idx.is_kept(&m.id)
                    || self.threads.contains_key(&m.id)
                    || self.is_unread_thread(&m.rid, &m.id)
                    || (idx.mode() == ThreadsMode::Following
                        && self.subscription(&m.rid).is_some()
                        && follows(m, self.user_id()))))
    }

    /// Applies the star state of a parent copy (pins on, own uid only; the
    /// `starred` array of realtime copies lists every member's stars).
    /// Returns whether it changed, and whether a live copy confirmed a
    /// recent change instead.
    fn note_star(&mut self, m: &Message, live: bool) -> (bool, bool) {
        let Some(me) = self.user_id() else {
            return (false, false);
        };
        let on = starred_parent(m, Some(me));
        let now = Instant::now();
        if self.thread_index.set_starred(&m.rid, &m.id, on, now) {
            return (true, false);
        }
        (
            false,
            live && self.thread_index.confirm_star(&m.id, on, now),
        )
    }

    /// With the Threads section off, a parent that is not pinned is not kept.
    fn drop_unpinned_parent(&mut self, tmid: &str) -> bool {
        !self.thread_index.enabled()
            && !self.thread_index.is_pinned(tmid)
            && self.thread_index.drop_parent(tmid)
    }

    /// Stores, updates or drops a parent (see [`ParentSource`]). Returns
    /// whether the index changed.
    fn index_parent(&mut self, m: Message, source: ParentSource) -> bool {
        let live = source == ParentSource::Live;
        if m.hidden == Some(true) {
            self.thread_index.tombstone(&m);
            return self.thread_index.mark_gone_unstarred(&m.id, Instant::now());
        }
        if self.thread_index.has_newer(&m) {
            return false;
        }
        let (star, confirmed) = if source == ParentSource::Cached {
            (false, false)
        } else {
            self.note_star(&m, live)
        };
        if crate::thread_index::is_removed(&m) {
            let mut changed = star | self.thread_index.unfollow(&m.id);
            if live {
                changed |= self
                    .thread_index
                    .live_follow(&m.rid, &m.id, false, Instant::now());
            }
            let shown = self.thread_index.is_pinned(&m.id)
                || (self.thread_index.enabled()
                    && (self.thread_index.is_kept(&m.id)
                        || self.threads.contains_key(&m.id)
                        || self.is_unread_thread(&m.rid, &m.id)));
            changed |= if shown {
                self.thread_index.put_parent(m)
            } else {
                self.thread_index.tombstone(&m);
                self.thread_index.remove_parent(&m.id)
            };
            return changed;
        }
        // Every live follow state counts: a crawl may be in flight for the room.
        let follow = (live && self.subscription(&m.rid).is_some())
            .then(|| (m.rid.clone(), m.id.clone(), follows(&m, self.user_id())));
        let mut changed = star;
        // A star change (or its echo) of a parent that is not stored: older
        // copies must not undo it.
        let star_copy = (star || confirmed).then(|| m.clone());
        // Stored first: trimming the followed set ranks by the stored activity.
        if source == ParentSource::Fetched || self.thread_relevant(&m) {
            let id = m.id.clone();
            changed |= self.thread_index.put_parent(m);
            changed |= self.drop_unpinned_parent(&id);
        }
        if let Some(c) = star_copy
            && self.thread_index.parent(&c.id).is_none()
        {
            self.thread_index.tombstone(&c);
        }
        if let Some((rid, tmid, follows)) = follow {
            changed |= self
                .thread_index
                .live_follow(&rid, &tmid, follows, Instant::now());
        }
        changed
    }

    /// Parents from a page or an open thread (relevant ones only).
    pub(super) fn index_parents(&mut self, msgs: &[Message]) {
        self.index_parents_from(msgs, ParentSource::Page);
    }

    pub(super) fn index_parents_from(&mut self, msgs: &[Message], source: ParentSource) {
        if !self.thread_index.indexing() {
            return;
        }
        let mut changed = false;
        for m in msgs.iter().filter(|m| m.tmid.is_none()) {
            changed |= self.index_parent(m.clone(), source);
        }
        changed |= self.evict_thread_parents();
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// The realtime / sync hook: parent updates (counters, follows, deletion
    /// as `t: "rm"`, hiding) and replies (parent activity).
    pub(super) fn index_message(&mut self, m: &Message) {
        if !self.thread_index.indexing() {
            return;
        }
        let changed = match m.tmid.as_deref() {
            Some(tmid) => {
                m.hidden != Some(true)
                    && !crate::thread_index::is_removed(m)
                    && self.thread_index.bump_activity(tmid, m.ts)
            }
            None if m.hidden == Some(true) => {
                let known = self.thread_index.parent(&m.id).is_some()
                    || self.thread_index.is_followed(&m.id)
                    || self.thread_index.is_pinned(&m.id);
                if known || m.tcount.is_some() {
                    self.thread_index.tombstone(m);
                }
                known && self.thread_index.mark_gone_unstarred(&m.id, Instant::now())
            }
            None => {
                let parent = m.tcount.is_some()
                    || crate::thread_index::is_removed(m)
                    || self.thread_index.parent(&m.id).is_some();
                if parent {
                    let changed = self.index_parent(m.clone(), ParentSource::Live);
                    self.evict_thread_parents() || changed
                } else {
                    false
                }
            }
        };
        if changed {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    pub(super) fn index_remove_room(&mut self, rid: &str) {
        if self.thread_index.remove_room(rid) {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    /// Evicts parents no row source refers to once the index is full.
    fn evict_thread_parents(&mut self) -> bool {
        if !self.thread_index.needs_eviction() {
            return false;
        }
        let mut referenced: HashSet<String> = self.threads.keys().cloned().collect();
        referenced.extend(self.thread_index.kept().map(|(_, t)| t.to_owned()));
        referenced.extend(self.thread_index.pinned().map(|(_, t)| t.to_owned()));
        referenced.extend(self.thread_index.followed_tmids().map(str::to_owned));
        for s in self.rooms.values().filter_map(|e| e.sub.as_ref()) {
            for v in [&s.tunread, &s.tunread_user, &s.tunread_group]
                .into_iter()
                .flatten()
            {
                referenced.extend(v.iter().cloned());
            }
        }
        self.thread_index.evict(&referenced)
    }
}
