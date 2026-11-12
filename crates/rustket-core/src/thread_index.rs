//! Thread parents and followed-thread sets behind the sidebar's Threads
//! section. Owned by the [`Workspace`](crate::workspace::Workspace), which
//! feeds it from realtime parent updates, timeline and thread pages and REST
//! results (`chat.getThreadsList`, `chat.getMessage`); it is the only source
//! of thread rows (`room_list::thread_rows`).
//!
//! Merge rules: a parent replaces the stored copy unless that copy is newer
//! (`_updatedAt`); a REST followed set is merged with follows and unfollows
//! seen live after the request started. Parents referenced by a row source
//! (followed sets, kept and starred threads, `tunread*`, open thread
//! timelines) are never evicted.
//!
//! Starred threads (parents the user starred, shown in Favorites) are pinned:
//! tracked and kept whatever the Threads mode is, while pins are on. A
//! per-room scan (`chat.getStarredMessages`) replaces the room's starred set,
//! keeping star changes applied after the scan started. Flagged threads
//! (synced flags, see [`crate::flags`]) are pinned the same way.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use rocketchat::RcDate;
use rocketchat::models::Message;

use crate::sidebar_layout::ThreadsMode;

/// Parents kept before unreferenced ones are evicted (oldest activity first).
pub const MAX_PARENTS: usize = 3000;
/// Followed threads listed per room (newest activity first).
pub const FOLLOWED_PER_ROOM: usize = 20;

/// Live follow marks older than this are forgotten (no request runs that long).
const LIVE_TTL: Duration = Duration::from_secs(600);
/// Live follow marks kept before old ones are pruned.
const LIVE_PRUNE_AT: usize = 1000;
/// Tombstones kept before the oldest is dropped.
const MAX_TOMBSTONES: usize = 1000;
/// After an eviction that could not get under the cap (everything left is
/// referenced), the next one waits until this many more parents are stored.
const EVICT_RETRY_STEP: usize = 100;

/// Followed threads of one room, from a `chat.getThreadsList(type=following)`
/// crawl kept current by live parent updates. Present = the room was crawled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FollowedRoom {
    pub tmids: BTreeSet<String>,
    /// The room's activity when the crawl was planned; a room whose activity
    /// is newer is crawled again after a reconnect.
    pub synced_activity: Option<RcDate>,
    /// Restored from the offline cache and not known to be current (not
    /// confirmed by the initial load, or the room had replies since): crawled
    /// again, shown until then.
    pub stale: bool,
    /// The crawl stopped at a page while the room had more (`None`: the set
    /// is complete up to [`FOLLOWED_PER_ROOM`]).
    pub short: Option<ShortCrawl>,
}

/// A followed-set crawl that asked for `count` threads while the room had
/// more; `kept` of them went into the set (hidden, deleted and unfollowed
/// ones do not). Persisted with the set: a larger page later crawls again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShortCrawl {
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub kept: u32,
}

/// A followed set from the offline cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredFollowed {
    pub rid: String,
    pub tmids: BTreeSet<String>,
    pub synced_activity: Option<RcDate>,
    pub short: Option<ShortCrawl>,
}

/// The last starred-messages scan of a room.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StarredScan {
    /// When the scan finished.
    pub scanned_at: Option<RcDate>,
    /// The room's last message (`lm`) when the scan started: a room with
    /// newer messages may hold stars this device missed.
    pub lm: Option<RcDate>,
}

/// Starred threads of one room from the offline cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredStarred {
    pub rid: String,
    pub tmids: BTreeSet<String>,
    pub scan: Option<StarredScan>,
}

/// A star state change (any source), re-applied over scans started earlier.
#[derive(Debug, Clone)]
struct StarChange {
    rid: String,
    on: bool,
    at: Instant,
}

/// A follow or unfollow seen live, re-applied over REST sets requested earlier.
#[derive(Debug, Clone)]
struct LiveFollow {
    rid: String,
    follows: bool,
    at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct ThreadIndex {
    mode: ThreadsMode,
    /// tmid -> parent message.
    parents: HashMap<String, Message>,
    /// rid -> followed set (Following mode only).
    followed: HashMap<String, FollowedRoom>,
    /// Parents that are not available (not found, hidden).
    gone: HashSet<String>,
    /// `_updatedAt` of hidden or deleted parents that were dropped: older
    /// copies (e.g. a REST result read before the change) must not revive them.
    tombstones: HashMap<String, RcDate>,
    /// Size at which the last eviction could not get under the cap (0: none).
    evict_blocked_at: usize,
    /// Kept threads of the signed-in account: tmid -> rid.
    kept: BTreeMap<String, String>,
    live: HashMap<String, LiveFollow>,
    /// Parents restored from the offline cache, with the `lr` up to which
    /// they were current, until the initial load confirms them.
    restored: HashMap<String, Option<RcDate>>,
    /// Restored parents whose room had replies since the cache was written
    /// (listed again; shown until then).
    stale: HashSet<String>,
    /// Starred threads are tracked (the Favorites switch, starring and
    /// threads allowed on the server).
    pins: bool,
    /// Threads whose parent the user starred: tmid -> rid.
    starred: BTreeMap<String, String>,
    /// Recent star changes: tmid -> change.
    star_changes: HashMap<String, StarChange>,
    /// rid -> last starred-messages scan.
    starred_scans: HashMap<String, StarredScan>,
    /// Flagged threads: tmid -> rid (mirrors the workspace's flags).
    flagged: BTreeMap<String, String>,
}

/// The user `me` is in the thread's `replies` (author, replied, mentioned or
/// followed explicitly): `chat.getThreadsList(type=following)`.
pub fn follows(m: &Message, me: Option<&str>) -> bool {
    me.is_some_and(|me| {
        m.replies
            .as_ref()
            .is_some_and(|r| r.iter().any(|u| u == me))
    })
}

/// The parent's activity: last reply, else its own time.
pub fn activity(m: &Message) -> Option<RcDate> {
    m.tlm.or(m.ts)
}

/// `t: "rm"`: a deleted thread parent (kept by the server, text emptied).
pub(crate) fn is_removed(m: &Message) -> bool {
    m.t.as_deref() == Some("rm")
}

impl ThreadIndex {
    pub fn mode(&self) -> ThreadsMode {
        self.mode
    }

    /// Whether the Threads section is on.
    pub fn enabled(&self) -> bool {
        self.mode != ThreadsMode::Off
    }

    /// Whether starred threads are tracked (see [`set_pins`](Self::set_pins)).
    pub fn pins_enabled(&self) -> bool {
        self.pins
    }

    /// Whether parents are stored or fetched at all: the Threads section or
    /// pinned (starred or flagged) threads need them.
    pub fn indexing(&self) -> bool {
        self.enabled() || self.pins || !self.flagged.is_empty()
    }

    /// Starred threads as `(rid, tmid)` (empty while pins are off).
    pub fn starred(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.starred.iter().map(|(t, r)| (r.as_str(), t.as_str()))
    }

    pub fn is_starred(&self, tmid: &str) -> bool {
        self.starred.contains_key(tmid)
    }

    /// Flagged threads as `(rid, tmid)`.
    pub fn flagged(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.flagged.iter().map(|(t, r)| (r.as_str(), t.as_str()))
    }

    pub fn is_flagged(&self, tmid: &str) -> bool {
        self.flagged.contains_key(tmid)
    }

    /// A parent that must stay stored whatever the mode is.
    pub fn is_pinned(&self, tmid: &str) -> bool {
        self.is_starred(tmid) || self.is_flagged(tmid)
    }

    /// Pinned threads (starred or flagged) as `(rid, tmid)`; a thread both
    /// starred and flagged comes twice.
    pub fn pinned(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.starred().chain(self.flagged())
    }

    /// The last starred-messages scan of a room.
    pub fn starred_scan(&self, rid: &str) -> Option<&StarredScan> {
        self.starred_scans.get(rid)
    }

    pub fn starred_scans(&self) -> impl Iterator<Item = (&str, &StarredScan)> + '_ {
        self.starred_scans.iter().map(|(r, s)| (r.as_str(), s))
    }

    /// Whether threads can be kept in the sidebar (menus offer it).
    pub fn kept_enabled(&self) -> bool {
        matches!(self.mode, ThreadsMode::UnreadKept | ThreadsMode::Following)
    }

    pub fn parent(&self, tmid: &str) -> Option<&Message> {
        self.parents.get(tmid)
    }

    pub fn parents(&self) -> impl Iterator<Item = &Message> + '_ {
        self.parents.values()
    }

    pub fn len(&self) -> usize {
        self.parents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// The parent is known to be unavailable (not found or hidden).
    pub fn is_gone(&self, tmid: &str) -> bool {
        self.gone.contains(tmid)
    }

    /// Kept threads as `(rid, tmid)`.
    pub fn kept(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.kept.iter().map(|(t, r)| (r.as_str(), t.as_str()))
    }

    pub fn is_kept(&self, tmid: &str) -> bool {
        self.kept.contains_key(tmid)
    }

    /// The followed set of a crawled room.
    pub fn followed(&self, rid: &str) -> Option<&FollowedRoom> {
        self.followed.get(rid)
    }

    pub fn followed_rooms(&self) -> impl Iterator<Item = (&str, &FollowedRoom)> + '_ {
        self.followed.iter().map(|(r, f)| (r.as_str(), f))
    }

    /// A restored parent that may be out of date (see
    /// [`confirm_restored`](Self::confirm_restored)).
    pub fn is_stale(&self, tmid: &str) -> bool {
        self.stale.contains(tmid) && self.parents.contains_key(tmid)
    }

    /// Restored parents the initial load has not confirmed yet: `(rid, lr
    /// up to which they were current)`.
    pub fn unconfirmed(&self) -> impl Iterator<Item = (&str, Option<RcDate>)> + '_ {
        self.restored
            .iter()
            .filter_map(|(t, at)| self.parents.get(t).map(|p| (p.rid.as_str(), *at)))
    }

    /// Threads of a room's followed set that can show as rows: parent known,
    /// in the room, not deleted.
    pub fn followed_shown(&self, rid: &str) -> usize {
        self.followed.get(rid).map_or(0, |f| {
            f.tmids
                .iter()
                .filter(|t| {
                    self.parents
                        .get(*t)
                        .is_some_and(|p| p.rid == rid && !is_removed(p))
                })
                .count()
        })
    }

    pub fn is_followed(&self, tmid: &str) -> bool {
        self.followed.values().any(|f| f.tmids.contains(tmid))
    }

    // --- mutation (crate-internal; the workspace marks THREAD_INDEX) ----------

    /// Off drops every parent but the pinned ones (the kept list stays);
    /// leaving Following drops the followed sets. Returns whether anything
    /// changed.
    pub(crate) fn set_mode(&mut self, mode: ThreadsMode) -> bool {
        if self.mode == mode {
            return false;
        }
        self.mode = mode;
        if mode == ThreadsMode::Off {
            self.drop_unpinned();
        }
        if mode != ThreadsMode::Following {
            self.followed.clear();
            self.live.clear();
        }
        true
    }

    /// Turns tracking of starred threads on or off. Off forgets the starred
    /// set and the scans (and, with the Threads section off, every parent).
    pub(crate) fn set_pins(&mut self, on: bool) -> bool {
        if self.pins == on {
            return false;
        }
        self.pins = on;
        if !on {
            self.starred.clear();
            self.star_changes.clear();
            self.starred_scans.clear();
            if !self.enabled() {
                self.drop_unpinned();
            }
        }
        true
    }

    /// Drops parents (and their marks) that are not pinned.
    fn drop_unpinned(&mut self) {
        let (starred, flagged) = (&self.starred, &self.flagged);
        let pinned = |t: &String| starred.contains_key(t) || flagged.contains_key(t);
        self.parents.retain(|t, _| pinned(t));
        let parents = &self.parents;
        self.restored.retain(|t, _| parents.contains_key(t));
        self.stale.retain(|t| parents.contains_key(t));
        self.gone.retain(|t| pinned(t));
        self.tombstones.retain(|t, _| pinned(t));
        self.evict_blocked_at = 0;
    }

    /// Mirrors the flagged threads (tmid -> rid). With the Threads section
    /// off, parents that are no longer pinned are dropped.
    pub(crate) fn set_flagged(&mut self, flagged: BTreeMap<String, String>) -> bool {
        if self.flagged == flagged {
            return false;
        }
        self.flagged = flagged;
        if !self.enabled() {
            self.drop_unpinned();
        }
        true
    }

    /// Sets the star state of a thread (a parent copy, an own star action).
    /// The change is remembered for scans requested earlier. Ignored while
    /// pins are off.
    pub(crate) fn set_starred(&mut self, rid: &str, tmid: &str, on: bool, now: Instant) -> bool {
        if !self.pins {
            return false;
        }
        let changed = if on {
            self.starred
                .insert(tmid.to_owned(), rid.to_owned())
                .as_deref()
                != Some(rid)
        } else {
            self.starred.remove(tmid).is_some()
        };
        if changed {
            if self.star_changes.len() >= LIVE_PRUNE_AT {
                self.star_changes
                    .retain(|_, c| now.duration_since(c.at) < LIVE_TTL);
            }
            self.star_changes.insert(
                tmid.to_owned(),
                StarChange {
                    rid: rid.to_owned(),
                    on,
                    at: now,
                },
            );
        }
        changed
    }

    /// A copy confirms the star state of `tmid` set by a recent change (the
    /// realtime echo of an own star action): the change counts from `now`,
    /// so scans requested before the echo do not undo it. `false` when there
    /// is no such change.
    pub(crate) fn confirm_star(&mut self, tmid: &str, on: bool, now: Instant) -> bool {
        match self.star_changes.get_mut(tmid) {
            Some(c) if c.on == on && self.pins => {
                c.at = c.at.max(now);
                true
            }
            _ => false,
        }
    }

    /// A finished starred-messages scan of `rid` requested at `started`:
    /// `found` (starred thread parents) replaces the room's starred set,
    /// except for star changes applied since `started`.
    pub(crate) fn finish_starred_scan(
        &mut self,
        rid: &str,
        found: BTreeSet<String>,
        started: Instant,
        scan: StarredScan,
    ) -> bool {
        if !self.pins {
            return false;
        }
        let changed_since = |tmid: &str| {
            self.star_changes
                .get(tmid)
                .filter(|c| c.at >= started)
                .map(|c| (c.rid.as_str(), c.on))
        };
        let mut set: BTreeSet<String> = found
            .into_iter()
            .filter(|t| changed_since(t).is_none_or(|(_, on)| on))
            .collect();
        for (tmid, c) in &self.star_changes {
            if c.rid == rid && c.at >= started && c.on {
                set.insert(tmid.clone());
            }
        }
        let before = self.starred.len();
        let mut changed = false;
        self.starred.retain(|t, r| r != rid || set.contains(t));
        changed |= self.starred.len() != before;
        for t in set {
            changed |= self.starred.insert(t, rid.to_owned()).is_none();
        }
        changed |= self.starred_scans.insert(rid.to_owned(), scan) != Some(scan);
        changed
    }

    /// The star state of `tmid` changed at or after `since`.
    pub(crate) fn star_changed_since(&self, tmid: &str, since: Instant) -> bool {
        self.star_changes.get(tmid).is_some_and(|c| c.at >= since)
    }

    /// Forgets a room's scan (scanned again later).
    pub(crate) fn forget_starred_scan(&mut self, rid: &str) -> bool {
        self.starred_scans.remove(rid).is_some()
    }

    pub(crate) fn set_kept(&mut self, kept: BTreeMap<String, String>) -> bool {
        if self.kept == kept {
            return false;
        }
        self.kept = kept;
        true
    }

    /// Stores a parent unless the stored copy is newer. `false` when ignored
    /// or identical.
    pub(crate) fn put_parent(&mut self, m: Message) -> bool {
        if self.has_newer(&m) {
            return false;
        }
        let refreshed = self.stale.remove(&m.id);
        if self.parents.get(&m.id) == Some(&m) {
            return refreshed;
        }
        self.gone.remove(&m.id);
        self.parents.insert(m.id.clone(), m);
        true
    }

    /// Thread data from the offline cache (before the initial load;
    /// `synced`: up to which `lr` a room's parents were current): parents
    /// unless newer ones are stored,
    /// followed sets (Following mode) of rooms not crawled yet, marked stale
    /// until [`confirm_restored`](Self::confirm_restored).
    pub(crate) fn restore(
        &mut self,
        parents: Vec<Message>,
        followed: Vec<RestoredFollowed>,
        starred: Vec<RestoredStarred>,
        synced: impl Fn(&str) -> Option<RcDate>,
    ) -> bool {
        if !self.indexing() {
            return false;
        }
        let mut changed = false;
        if self.pins {
            for RestoredStarred { rid, tmids, scan } in starred {
                for t in tmids {
                    // A star change seen since decides.
                    if !self.star_changes.contains_key(&t) && !self.starred.contains_key(&t) {
                        self.starred.insert(t, rid.clone());
                        changed = true;
                    }
                }
                if let Some(scan) = scan
                    && !self.starred_scans.contains_key(&rid)
                {
                    self.starred_scans.insert(rid, scan);
                    changed = true;
                }
            }
        }
        for m in parents {
            if !self.enabled() && !self.is_pinned(&m.id) {
                continue;
            }
            let (id, at) = (m.id.clone(), synced(&m.rid));
            if self.put_parent(m) {
                self.restored.insert(id, at);
                changed = true;
            }
        }
        if self.mode != ThreadsMode::Following {
            return changed;
        }
        for RestoredFollowed {
            rid,
            mut tmids,
            synced_activity,
            short,
        } in followed
        {
            if self.followed.contains_key(&rid) {
                continue;
            }
            tmids.retain(|t| !self.gone.contains(t));
            trim(&self.parents, &mut tmids);
            self.followed.insert(
                rid,
                FollowedRoom {
                    tmids,
                    synced_activity,
                    stale: true,
                    short,
                },
            );
            changed = true;
        }
        changed
    }

    /// The initial load arrived (`lr`: each room's last reply). Restored
    /// followed sets of rooms without replies since their crawl are current
    /// (live updates keep them so); restored parents of rooms with replies
    /// since they were current are stale.
    pub(crate) fn confirm_restored(&mut self, lr: impl Fn(&str) -> Option<RcDate>) -> bool {
        let mut changed = false;
        for (rid, f) in &mut self.followed {
            if f.stale && lr(rid) <= f.synced_activity {
                f.stale = false;
                changed = true;
            }
        }
        for (tmid, cached) in std::mem::take(&mut self.restored) {
            if let Some(p) = self.parents.get(&tmid)
                && lr(&p.rid) > cached
            {
                changed |= self.stale.insert(tmid);
            }
        }
        changed
    }

    /// Whether a stored parent (or the hide or deletion that dropped it) is
    /// newer than `m` (so `m` must not apply).
    pub(crate) fn has_newer(&self, m: &Message) -> bool {
        let Some(n) = m.updated_at else {
            return false;
        };
        self.parents
            .get(&m.id)
            .and_then(|old| old.updated_at)
            .is_some_and(|o| n < o)
            || self.tombstones.get(&m.id).is_some_and(|t| n < *t)
    }

    /// Remembers that `m` (hidden or deleted) was dropped at its `_updatedAt`.
    pub(crate) fn tombstone(&mut self, m: &Message) {
        let Some(at) = m.updated_at else {
            return;
        };
        if self.tombstones.len() >= MAX_TOMBSTONES
            && !self.tombstones.contains_key(&m.id)
            && let Some(oldest) = self
                .tombstones
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(t, _)| t.clone())
        {
            self.tombstones.remove(&oldest);
        }
        let e = self.tombstones.entry(m.id.clone()).or_insert(at);
        *e = (*e).max(at);
    }

    pub(crate) fn remove_parent(&mut self, tmid: &str) -> bool {
        self.parents.remove(tmid).is_some()
    }

    /// Drops a parent nothing refers to any more; older copies must not
    /// bring back its state (e.g. a star).
    pub(crate) fn drop_parent(&mut self, tmid: &str) -> bool {
        let Some(m) = self.parents.remove(tmid) else {
            return false;
        };
        self.tombstone(&m);
        true
    }

    pub(crate) fn mark_gone(&mut self, tmid: &str) -> bool {
        let removed = self.parents.remove(tmid).is_some();
        let unfollowed = self.unfollow_everywhere(tmid);
        self.gone.insert(tmid.to_owned()) || removed || unfollowed
    }

    /// The parent is gone for good (not found): no longer starred either.
    pub(crate) fn mark_gone_unstarred(&mut self, tmid: &str, now: Instant) -> bool {
        let rid = self.starred.get(tmid).cloned();
        let unstarred = rid.is_some_and(|rid| self.set_starred(&rid, tmid, false, now));
        self.mark_gone(tmid) || unstarred
    }

    /// A reply arrived: the parent's last activity is at least `ts`.
    pub(crate) fn bump_activity(&mut self, tmid: &str, ts: Option<RcDate>) -> bool {
        let (Some(p), Some(ts)) = (self.parents.get_mut(tmid), ts) else {
            return false;
        };
        if p.tlm.is_some_and(|t| t >= ts) {
            return false;
        }
        p.tlm = Some(ts);
        true
    }

    fn unfollow_everywhere(&mut self, tmid: &str) -> bool {
        let mut changed = false;
        for f in self.followed.values_mut() {
            changed |= f.tmids.remove(tmid);
        }
        changed
    }

    /// A live parent update in Following mode: remembered for REST sets
    /// requested earlier, applied to the room's set if it was crawled.
    pub(crate) fn live_follow(
        &mut self,
        rid: &str,
        tmid: &str,
        follows: bool,
        now: Instant,
    ) -> bool {
        if self.mode != ThreadsMode::Following {
            return false;
        }
        if self.live.len() >= LIVE_PRUNE_AT {
            self.live.retain(|_, l| now.duration_since(l.at) < LIVE_TTL);
        }
        self.live.insert(
            tmid.to_owned(),
            LiveFollow {
                rid: rid.to_owned(),
                follows,
                at: now,
            },
        );
        let Some(f) = self.followed.get_mut(rid) else {
            return false;
        };
        if !follows {
            return f.tmids.remove(tmid);
        }
        if !f.tmids.insert(tmid.to_owned()) {
            return false;
        }
        trim(&self.parents, &mut f.tmids);
        // Trimmed right away (older than the room's newest): nothing changed.
        f.tmids.contains(tmid)
    }

    /// Replaces a room's followed set with a REST result requested at
    /// `started`, keeping follows and unfollows seen live since then.
    /// `short`: the page size, when the room had more.
    pub(crate) fn set_followed(
        &mut self,
        rid: &str,
        tmids: BTreeSet<String>,
        synced_activity: Option<RcDate>,
        started: Instant,
        short: Option<usize>,
    ) -> bool {
        if self.mode != ThreadsMode::Following {
            return false;
        }
        let mut set = tmids;
        for (tmid, l) in &self.live {
            if l.rid == rid && l.at >= started {
                if l.follows {
                    set.insert(tmid.clone());
                } else {
                    set.remove(tmid);
                }
            }
        }
        set.retain(|t| !self.gone.contains(t));
        trim(&self.parents, &mut set);
        let short = short.map(|count| ShortCrawl {
            count: u32::try_from(count).unwrap_or(u32::MAX),
            kept: u32::try_from(set.len()).unwrap_or(u32::MAX),
        });
        let new = FollowedRoom {
            tmids: set,
            synced_activity,
            stale: false,
            short,
        };
        if self.followed.get(rid) == Some(&new) {
            return false;
        }
        self.followed.insert(rid.to_owned(), new);
        true
    }

    /// A room's followed set may be out of date (its crawl failed after
    /// missed live updates): crawled again, shown until then.
    pub(crate) fn mark_followed_stale(&mut self, rid: &str) -> bool {
        match self.followed.get_mut(rid) {
            Some(f) if !f.stale => {
                f.stale = true;
                true
            }
            _ => false,
        }
    }

    /// Forgets a room's followed set (crawled again later).
    pub(crate) fn forget_followed(&mut self, rid: &str) -> bool {
        self.followed.remove(rid).is_some()
    }

    /// Drops what belongs to a room the user no longer has.
    pub(crate) fn remove_room(&mut self, rid: &str) -> bool {
        let before = self.parents.len();
        self.parents.retain(|_, m| m.rid != rid);
        let followed = self.followed.remove(rid).is_some();
        self.live.retain(|_, l| l.rid != rid);
        let parents = &self.parents;
        self.restored.retain(|t, _| parents.contains_key(t));
        self.stale.retain(|t| parents.contains_key(t));
        let starred = self.starred.len();
        self.starred.retain(|_, r| r != rid);
        self.star_changes.retain(|_, c| c.rid != rid);
        let scanned = self.starred_scans.remove(rid).is_some();
        followed || scanned || self.parents.len() != before || self.starred.len() != starred
    }

    /// Message `id` of `rid` was deleted.
    pub(crate) fn remove_message(&mut self, rid: &str, id: &str) -> bool {
        let removed = self.parents.remove(id).is_some();
        let unfollowed = self
            .followed
            .get_mut(rid)
            .is_some_and(|f| f.tmids.remove(id));
        let unstarred = self.starred.remove(id).is_some();
        removed || unfollowed || unstarred
    }

    pub(crate) fn unfollow(&mut self, tmid: &str) -> bool {
        self.unfollow_everywhere(tmid)
    }

    /// Rewrites the parents of a room in place (e.g. decrypted text).
    pub(crate) fn rewrite(&mut self, rid: &str, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut changed = false;
        for m in self.parents.values_mut().filter(|m| m.rid == rid) {
            changed |= f(m);
        }
        changed
    }

    /// Whether [`evict`](Self::evict) has work to do: over the cap, and (after
    /// an eviction that could not get under it) [`EVICT_RETRY_STEP`] parents
    /// more than then.
    pub(crate) fn needs_eviction(&self) -> bool {
        let len = self.parents.len();
        len > MAX_PARENTS
            && (self.evict_blocked_at == 0 || len >= self.evict_blocked_at + EVICT_RETRY_STEP)
    }

    /// Evicts parents not in `referenced` (which must include the followed
    /// sets), oldest activity first, once more than [`MAX_PARENTS`] are stored
    /// (down to 90 % of it).
    pub(crate) fn evict(&mut self, referenced: &HashSet<String>) -> bool {
        if self.parents.len() <= MAX_PARENTS {
            return false;
        }
        let target = MAX_PARENTS * 9 / 10;
        let mut free: Vec<(Option<RcDate>, String)> = self
            .parents
            .iter()
            .filter(|(t, _)| !referenced.contains(*t))
            .map(|(t, m)| (activity(m), t.clone()))
            .collect();
        free.sort();
        let extra = self.parents.len().saturating_sub(target);
        let mut changed = false;
        for (_, t) in free.into_iter().take(extra) {
            changed |= self.parents.remove(&t).is_some();
        }
        self.evict_blocked_at = if self.parents.len() > MAX_PARENTS {
            self.parents.len()
        } else {
            0
        };
        changed
    }

    /// Every followed tmid of every crawled room.
    pub(crate) fn followed_tmids(&self) -> impl Iterator<Item = &str> + '_ {
        self.followed
            .values()
            .flat_map(|f| f.tmids.iter().map(String::as_str))
    }

    /// Everything but the mode, the kept and flagged lists and whether pins
    /// are on.
    pub(crate) fn clear(&mut self) -> bool {
        let changed = !self.parents.is_empty()
            || !self.followed.is_empty()
            || !self.gone.is_empty()
            || !self.starred.is_empty();
        self.starred.clear();
        self.star_changes.clear();
        self.starred_scans.clear();
        self.parents.clear();
        self.followed.clear();
        self.gone.clear();
        self.tombstones.clear();
        self.live.clear();
        self.restored.clear();
        self.stale.clear();
        self.evict_blocked_at = 0;
        changed
    }
}

/// Keeps the [`FOLLOWED_PER_ROOM`] most recently active threads of a followed
/// set (unknown parents count as oldest).
fn trim(parents: &HashMap<String, Message>, set: &mut BTreeSet<String>) {
    if set.len() <= FOLLOWED_PER_ROOM {
        return;
    }
    let mut v: Vec<(Option<RcDate>, String)> = std::mem::take(set)
        .into_iter()
        .map(|t| (parents.get(&t).and_then(activity), t))
        .collect();
    // Newest first; `None` sorts below every date.
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    *set = v
        .into_iter()
        .take(FOLLOWED_PER_ROOM)
        .map(|(_, t)| t)
        .collect();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{msg, ts};

    fn parent(id: &str, rid: &str, tlm: i64) -> Message {
        let mut m = msg(id, rid, 1, "u");
        m.tcount = Some(1);
        m.tlm = Some(ts(tlm));
        m
    }

    fn following() -> ThreadIndex {
        let mut i = ThreadIndex::default();
        i.set_mode(ThreadsMode::Following);
        i
    }

    #[test]
    fn older_copies_do_not_replace_newer_ones() {
        let mut i = following();
        let mut a = parent("t", "r", 10);
        a.updated_at = Some(ts(20));
        assert!(i.put_parent(a.clone()));
        assert!(!i.put_parent(a.clone()), "identical");
        let mut old = a.clone();
        old.updated_at = Some(ts(5));
        old.msg = "old".into();
        assert!(!i.put_parent(old));
        let mut tie = a;
        tie.msg = "tie".into();
        assert!(i.put_parent(tie), "ties apply");
        assert_eq!(i.parent("t").unwrap().msg, "tie");
    }

    #[test]
    fn rest_sets_keep_live_changes_made_while_in_flight() {
        let mut i = following();
        let started = Instant::now();
        // Seen live after the request started: followed "b", unfollowed "a".
        i.live_follow("r", "b", true, started + Duration::from_millis(1));
        i.live_follow("r", "a", false, started + Duration::from_millis(2));
        // Before the request started: ignored.
        i.live_follow("r", "z", true, started - Duration::from_secs(1));
        let rest: BTreeSet<String> = ["a".into(), "c".into()].into();
        assert!(i.set_followed("r", rest, Some(ts(5)), started, None));
        let f = i.followed("r").unwrap();
        assert_eq!(
            f.tmids.iter().map(String::as_str).collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(f.synced_activity, Some(ts(5)));
    }

    #[test]
    fn followed_sets_keep_the_newest_per_room() {
        let mut i = following();
        let mut set = BTreeSet::new();
        for n in 0..(FOLLOWED_PER_ROOM as i64 + 5) {
            let id = format!("t{n:02}");
            i.put_parent(parent(&id, "r", 100 + n));
            set.insert(id);
        }
        i.set_followed("r", set, None, Instant::now(), None);
        let f = i.followed("r").unwrap();
        assert_eq!(f.tmids.len(), FOLLOWED_PER_ROOM);
        assert!(!f.tmids.contains("t00") && f.tmids.contains("t24"));
    }

    #[test]
    fn live_follows_into_a_full_set_rank_by_the_new_activity() {
        let mut i = following();
        let mut set = BTreeSet::new();
        for n in 0..FOLLOWED_PER_ROOM as i64 {
            let id = format!("t{n:02}");
            i.put_parent(parent(&id, "r", 100 + n));
            set.insert(id);
        }
        i.set_followed("r", set, None, Instant::now(), None);
        // Stored first (as the workspace does), then followed: the newest wins.
        i.put_parent(parent("new", "r", 500));
        assert!(i.live_follow("r", "new", true, Instant::now()));
        let f = i.followed("r").unwrap();
        assert_eq!(f.tmids.len(), FOLLOWED_PER_ROOM);
        assert!(f.tmids.contains("new") && !f.tmids.contains("t00"));
        // An older thread does not make it: no change reported.
        i.put_parent(parent("old", "r", 1));
        assert!(!i.live_follow("r", "old", true, Instant::now()));
        assert!(!i.followed("r").unwrap().tmids.contains("old"));
    }

    #[test]
    fn recrawls_of_a_trimmed_room_change_nothing() {
        let mut i = following();
        let mut set = BTreeSet::new();
        for n in 0..(FOLLOWED_PER_ROOM as i64 + 5) {
            let id = format!("t{n:02}");
            i.put_parent(parent(&id, "r", 100 + n));
            set.insert(id);
        }
        let started = Instant::now();
        assert!(i.set_followed("r", set.clone(), None, started, None));
        assert!(!i.set_followed("r", set, None, started, None));
    }

    #[test]
    fn hidden_parents_are_not_revived_by_older_copies() {
        let mut i = following();
        let mut p = parent("t", "r", 10);
        p.updated_at = Some(ts(20));
        i.put_parent(p.clone());
        let mut hidden = p.clone();
        hidden.hidden = Some(true);
        hidden.updated_at = Some(ts(30));
        i.tombstone(&hidden);
        i.mark_gone("t");
        assert!(i.has_newer(&p), "read before the hide");
        let mut unhidden = p;
        unhidden.updated_at = Some(ts(40));
        assert!(!i.has_newer(&unhidden));
        assert!(i.put_parent(unhidden));
        assert!(!i.is_gone("t"));
    }

    #[test]
    fn a_fully_referenced_index_is_not_rescanned_on_every_insert() {
        let mut i = following();
        for n in 0..=MAX_PARENTS {
            i.put_parent(parent(&format!("p{n}"), "r", n as i64 + 1));
        }
        let all: HashSet<String> = i.parents().map(|m| m.id.clone()).collect();
        assert!(i.needs_eviction());
        assert!(!i.evict(&all));
        assert!(!i.needs_eviction(), "blocked until it grows");
        for n in 0..EVICT_RETRY_STEP {
            i.put_parent(parent(&format!("q{n}"), "r", 1));
        }
        assert!(i.needs_eviction());
        assert!(i.evict(&all));
        assert!(!i.needs_eviction(), "back under the cap");
    }

    #[test]
    fn eviction_spares_referenced_parents() {
        let mut i = following();
        for n in 0..=MAX_PARENTS {
            i.put_parent(parent(&format!("p{n}"), "r", n as i64 + 1));
        }
        i.set_followed("r", ["p1".to_owned()].into(), None, Instant::now(), None);
        let mut referenced: HashSet<String> = ["p0".to_owned()].into();
        referenced.extend(i.followed_tmids().map(str::to_owned));
        assert!(i.needs_eviction());
        assert!(i.evict(&referenced));
        assert_eq!(i.len(), MAX_PARENTS * 9 / 10);
        assert!(i.parent("p0").is_some() && i.parent("p1").is_some());
        assert!(i.parent("p2").is_none(), "oldest unreferenced evicted");
        assert!(i.parent(&format!("p{MAX_PARENTS}")).is_some());
        assert!(!i.evict(&referenced), "under the cap");
    }

    #[test]
    fn modes_drop_what_they_do_not_use() {
        let mut i = following();
        i.put_parent(parent("t", "r", 1));
        i.set_followed("r", ["t".to_owned()].into(), None, Instant::now(), None);
        i.set_kept([("k".to_owned(), "r".to_owned())].into());
        assert!(i.set_mode(ThreadsMode::UnreadKept));
        assert!(i.followed("r").is_none());
        assert!(i.parent("t").is_some());
        assert!(!i.live_follow("r", "t", true, Instant::now()));
        assert!(i.set_mode(ThreadsMode::Off));
        assert!(i.is_empty());
        assert!(i.is_kept("k"), "the kept list mirrors the config");
        assert!(!i.set_mode(ThreadsMode::Off));
    }

    #[test]
    fn starred_parents_survive_mode_off() {
        let mut i = following();
        assert!(!i.set_starred("r", "s", true, Instant::now()), "pins off");
        assert!(i.set_pins(true));
        assert!(i.indexing());
        i.put_parent(parent("s", "r", 1));
        i.put_parent(parent("t", "r", 1));
        assert!(i.set_starred("r", "s", true, Instant::now()));
        assert!(!i.set_starred("r", "s", true, Instant::now()), "unchanged");
        assert!(i.set_mode(ThreadsMode::Off));
        assert!(!i.enabled() && i.indexing());
        assert!(i.parent("s").is_some() && i.parent("t").is_none());
        assert_eq!(i.starred().collect::<Vec<_>>(), [("r", "s")]);
        // Pins off with the Threads section off: nothing is indexed.
        assert!(i.set_pins(false));
        assert!(!i.indexing() && i.is_empty());
        assert_eq!(i.starred().count(), 0);
    }

    #[test]
    fn scans_replace_the_room_slice_keeping_newer_changes() {
        let mut i = ThreadIndex::default();
        i.set_pins(true);
        let t0 = Instant::now();
        for t in ["a", "b", "c"] {
            i.set_starred("r", t, true, t0);
        }
        i.set_starred("other", "o", true, t0);
        let started = t0 + Duration::from_millis(10);
        // While the scan was in flight: "d" starred, "a" unstarred.
        i.set_starred("r", "d", true, started + Duration::from_millis(1));
        i.set_starred("r", "a", false, started + Duration::from_millis(2));
        // The scan, read before both, finds a and e (b and c were unstarred
        // elsewhere).
        let scan = StarredScan {
            scanned_at: Some(ts(100)),
            lm: Some(ts(90)),
        };
        assert!(i.finish_starred_scan("r", ["a".into(), "e".into()].into(), started, scan));
        let mut got: Vec<_> = i.starred().collect();
        got.sort();
        assert_eq!(got, [("other", "o"), ("r", "d"), ("r", "e")]);
        assert_eq!(i.starred_scan("r"), Some(&scan));
        assert!(!i.finish_starred_scan("r", ["d".into(), "e".into()].into(), started, scan));
        // Leaving the room forgets its stars and its scan.
        assert!(i.remove_room("r"));
        assert_eq!(i.starred().collect::<Vec<_>>(), [("other", "o")]);
        assert!(i.starred_scan("r").is_none());
    }

    #[test]
    fn flagged_parents_are_pinned() {
        let mut i = following();
        let flagged = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(r, t)| ((*t).to_owned(), (*r).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        i.put_parent(parent("f", "r", 1));
        i.put_parent(parent("g", "r", 1));
        assert!(i.set_flagged(flagged(&[("r", "f"), ("r", "g")])));
        assert!(
            !i.set_flagged(flagged(&[("r", "f"), ("r", "g")])),
            "unchanged"
        );
        assert!(i.is_flagged("f") && i.is_pinned("f") && !i.is_starred("f"));
        assert!(i.set_mode(ThreadsMode::Off));
        assert!(!i.enabled() && i.indexing() && !i.pins_enabled());
        assert!(i.parent("f").is_some() && i.parent("g").is_some());
        // Unflagging with the Threads section off drops the parent.
        assert!(i.set_flagged(flagged(&[("r", "f")])));
        assert!(i.parent("f").is_some() && i.parent("g").is_none());
        // Gone flagged parents stay flagged (only the user clears flags);
        // leaving the room or logging out keeps them too.
        i.mark_gone_unstarred("f", Instant::now());
        assert!(i.is_gone("f") && i.is_flagged("f"));
        i.remove_room("r");
        i.remove_message("r", "f");
        i.clear();
        assert_eq!(i.flagged().collect::<Vec<_>>(), [("r", "f")]);
        assert!(i.indexing());
        assert!(i.set_flagged(BTreeMap::new()));
        assert!(!i.indexing());
    }

    #[test]
    fn restored_stars_need_pins_and_yield_to_changes_seen() {
        let restored = || {
            vec![RestoredStarred {
                rid: "r".into(),
                tmids: ["a".to_owned(), "b".to_owned()].into(),
                scan: Some(StarredScan {
                    scanned_at: Some(ts(5)),
                    lm: None,
                }),
            }]
        };
        let mut i = ThreadIndex::default();
        assert!(!i.restore(vec![parent("a", "r", 1)], Vec::new(), restored(), |_| None));
        i.set_pins(true);
        i.set_starred("r", "a", true, Instant::now());
        i.set_starred("r", "a", false, Instant::now());
        // "a" was unstarred since: stays so; "b" comes back. Mode Off keeps
        // only pinned parents.
        assert!(i.restore(
            vec![parent("b", "r", 1), parent("u", "r", 1)],
            Vec::new(),
            restored(),
            |_| None
        ));
        assert_eq!(i.starred().collect::<Vec<_>>(), [("r", "b")]);
        assert!(i.parent("b").is_some() && i.parent("u").is_none());
        assert!(i.starred_scan("r").is_some());
    }
}
