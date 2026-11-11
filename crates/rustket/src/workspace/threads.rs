//! Loads what the sidebar's Threads section shows: parents of unread threads
//! (`chat.getThreadsList(type=unread)`, paged), parents of kept threads and
//! of unread threads the list did not return (`chat.getMessage`) and, in
//! Following mode, each room's followed threads (`type=following`, the newest
//! [`FOLLOWED_PER_ROOM`] per room, rooms with recent replies first). The data
//! goes into the workspace's thread index; live updates keep it current.
//!
//! Rate limits are per IP and shared with the user's other clients and
//! colleagues behind the same NAT (production defaults: getThreadsList 20 per
//! 10 s, getMessage 10 per 60 s). So one request runs at a time, list calls
//! start at least [`LIST_INTERVAL`] apart, single parents [`GET_INTERVAL`]
//! apart (a third of the getMessage limit at most: the rest is left to
//! fetches the user asks for and to other clients) within [`GET_BUDGET`] per
//! session, and nothing is fetched while disconnected or showing the offline
//! cache. [`plan`] picks the next request and is pure;
//! [`SidebarThreads::tick`] runs it when its inputs changed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::{Message, Paginated, Subscription};
use rocketchat::{Error, RcDate};
use rustket_core::cache::{CachedFollowed, CachedParentsSynced, CachedThreads};
use rustket_core::config::ThreadsMode;
use rustket_core::room_list::thread_unread;
use rustket_core::thread_index::FOLLOWED_PER_ROOM;
use rustket_core::workspace::{ConnectionState, RoomEntry};
use rustket_core::{Dirty, Workspace};

use crate::error::{Failure, Res};
use crate::session::SessionHandle;

/// Minimum time between two `chat.getThreadsList` calls.
pub const LIST_INTERVAL: Duration = Duration::from_secs(2);
/// Following crawl of rooms without thread replies (`sub.lr` unset): they can
/// only hold followed threads nobody replied to, so they come last and slower.
pub const SLOW_LIST_INTERVAL: Duration = Duration::from_secs(6);
/// Minimum time between two `chat.getMessage` calls (at most 3 of the 10
/// per minute allowed per IP).
pub const GET_INTERVAL: Duration = Duration::from_secs(20);
/// `chat.getMessage` calls per session.
pub const GET_BUDGET: u32 = 20;
/// Page size of the unread list (`API_Upper_Count_Limit` default).
const UNREAD_PAGE: u64 = 100;
/// Pages of one room's unread list per pass.
const MAX_UNREAD_PAGES: u64 = 10;
/// A new unread thread usually arrives together with its parent broadcast:
/// give that a moment before asking the server.
const DEBOUNCE: Duration = Duration::from_secs(1);
/// Pause after a rate-limit response without a reset time.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10);
const MAX_PAUSE: Duration = Duration::from_secs(300);
/// Tries of a request whose failure is not recognised (e.g. a proxy's
/// error page, an unreadable body) before it is given up for the session.
const MAX_OTHER_FAILURES: u32 = 3;

/// Wait after `failures` (1-based) transient failures of one request: 5 s,
/// doubling, at most 5 min.
fn retry_delay(failures: u32) -> Duration {
    let secs = 5u64.saturating_mul(1 << failures.saturating_sub(1).min(6));
    Duration::from_secs(secs.min(300))
}

/// A request [`plan`] asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// One page of a room's unread threads.
    Unread { rid: String, offset: u64 },
    /// One parent (kept, or unread but missing from the list).
    Parent { rid: String, tmid: String },
    /// A room's followed threads; `activity` is the room's `lr` now.
    Followed {
        rid: String,
        activity: Option<RcDate>,
    },
}

impl Fetch {
    fn key(&self) -> Key {
        match self {
            Self::Unread { rid, .. } => Key::Unread(rid.clone()),
            Self::Parent { tmid, .. } => Key::Parent(tmid.clone()),
            Self::Followed { rid, .. } => Key::Followed(rid.clone()),
        }
    }
}

/// The target of a request, for back-off after transient failures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Unread(String),
    Parent(String),
    Followed(String),
}

/// What [`plan`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Fetch(Fetch),
    /// Something is due at this time (pacing, back-off, debounce).
    Wait(Instant),
    Idle,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A request of controller generation `epoch` finished.
    Done { epoch: u64, result: Done },
    /// The pacing timer set for `at` fired.
    Pace { at: Instant },
}

#[derive(Debug, Clone)]
pub enum Done {
    Unread {
        rid: String,
        offset: u64,
        res: Res<Paginated<Message>>,
    },
    Parent {
        rid: String,
        tmid: String,
        res: Res<Box<Message>>,
    },
    Followed {
        rid: String,
        activity: Option<RcDate>,
        res: Res<Paginated<Message>>,
    },
}

/// Requests to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// A kept thread's parent does not exist (any more): drop it from the
    /// config.
    Unkeep { rid: String, tmid: String },
}

#[derive(Debug, Clone)]
struct InFlight {
    fetch: Fetch,
    started: Instant,
    /// `SidebarThreads::mode_gen` when it started: a result that arrives
    /// after a mode change is dropped.
    mode_gen: u64,
    /// Unread pages: when the room's pass started.
    pass_start: Instant,
}

/// The rest of a room's unread pass (more pages wanted).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paging {
    rid: String,
    offset: u64,
    pass_start: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Backoff {
    failures: u32,
    until: Instant,
}

/// Per-server loader of the Threads section (`ServerState::threads`).
#[derive(Debug, Default)]
pub struct SidebarThreads {
    /// The session the state belongs to (`Arc` address).
    session: Option<usize>,
    /// Bumped with every new session: older results are dropped.
    epoch: u64,
    mode: ThreadsMode,
    /// Bumped with every mode change.
    mode_gen: u64,
    /// Inputs of the last [`plan`] run by `tick`; it plans again only when
    /// they or the loader state changed, or `due` passed.
    tick_key: Option<TickKey>,
    /// The loader state changed since the last plan.
    changed: bool,
    /// When the last plan wanted to run something.
    due: Option<Instant>,
    /// Following crawl progress as of the last plan.
    progress: Option<(usize, usize)>,
    /// A sync started after `lr_before` was captured: only such a sync
    /// brings what happened while live updates were missed.
    synced_after_capture: bool,
    /// `Dirty::SYNC` revision seen last; a change (initial load or delta
    /// sync) after live updates were missed re-checks what may be stale.
    sync_rev: Option<u64>,
    /// Live updates were missed (disconnected, or a resync): each listed
    /// room's `lr` when that was first noticed. Rooms whose `lr` differs
    /// after the next sync had replies in between.
    lr_before: Option<HashMap<String, Option<RcDate>>>,
    in_flight: Option<InFlight>,
    /// Start of the last list / single-parent request (kept across sessions
    /// and logouts: the limits are per IP).
    last_list: Option<Instant>,
    last_get: Option<Instant>,
    /// Rate limited: nothing before this (kept like `last_list`).
    paused_until: Option<Instant>,
    /// `chat.getMessage` calls in this session.
    gets: u32,
    /// The pending pacing timer.
    timer: Option<Instant>,
    /// Unread tmids without a parent, since when (debounce).
    missing_since: HashMap<String, Instant>,
    /// Unread tmids the room's unread list did not return.
    unresolved: BTreeSet<String>,
    /// tmids done with: fetched singly, or given up for this session.
    settled: HashSet<String>,
    /// The `settled` tmids given up after unrecognised failures: tried again
    /// after missed live updates (e.g. a reconnect).
    failed: HashSet<String>,
    /// Rooms whose unread parents may be stale (replies while live updates
    /// were missed).
    refresh_unread: BTreeSet<String>,
    paging: Option<Paging>,
    /// Crawled rooms to crawl again (replies while live updates were missed).
    recrawl: BTreeSet<String>,
    /// Rooms whose crawl failed (unrecognised failures); crawled again after
    /// missed live updates.
    crawl_failed: HashSet<String>,
    backoff: HashMap<Key, Backoff>,
}

/// What a plan depends on besides the loader state and the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickKey {
    session: usize,
    mode: ThreadsMode,
    collapsed: bool,
    rooms: u64,
    settings: u64,
    thread_index: u64,
    sync: u64,
    connection: u64,
    me: u64,
    from_cache: bool,
}

impl TickKey {
    fn new(ws: &Workspace, session: usize, mode: ThreadsMode, collapsed: bool) -> Self {
        Self {
            session,
            mode,
            collapsed,
            rooms: ws.revision(Dirty::ROOMS),
            settings: ws.revision(Dirty::SETTINGS),
            thread_index: ws.revision(Dirty::THREAD_INDEX),
            sync: ws.revision(Dirty::SYNC),
            connection: ws.revision(Dirty::CONNECTION),
            me: ws.revision(Dirty::ME),
            from_cache: ws.from_cache(),
        }
    }
}

/// Rooms of the room list: subscribed, open, not archived.
fn listed(ws: &Workspace) -> impl Iterator<Item = (&RoomEntry, &Subscription)> {
    ws.rooms().values().filter_map(|e| {
        e.sub
            .as_ref()
            .filter(|s| s.open && !e.is_archived())
            .map(|s| (e, s))
    })
}

/// Unread threads of a subscription that the Threads section shows.
fn shown_unread(s: &Subscription) -> impl Iterator<Item = &String> {
    [&s.tunread, &s.tunread_user, &s.tunread_group]
        .into_iter()
        .flatten()
        .flatten()
        .filter(|t| thread_unread(s, t).is_some())
}

/// Shown unread threads of `s` whose parent is not known (nor gone), or
/// restored from the offline cache and possibly out of date.
fn missing_in<'a>(ws: &'a Workspace, s: &'a Subscription) -> impl Iterator<Item = &'a String> {
    let idx = ws.thread_index();
    shown_unread(s).filter(move |t| {
        (idx.parent(t).filter(|p| p.rid == s.rid).is_none() || idx.is_stale(t)) && !idx.is_gone(t)
    })
}

/// Whether a room's followed threads are to be crawled (again): not crawled,
/// restored and not current, or re-queued.
fn wants_crawl(ws: &Workspace, st: &SidebarThreads, rid: &str) -> bool {
    ws.thread_index().followed(rid).is_none_or(|f| f.stale) || st.recrawl.contains(rid)
}

fn earliest(a: Option<Instant>, b: Instant) -> Option<Instant> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// The next request, given the workspace, the Threads mode, whether the
/// Threads section is collapsed (the Following crawl waits then) and the
/// loader state. Order: unread pages, then single parents, then the
/// Following crawl; a request that must wait for its pacing lets a ready
/// one of the other kind go first.
pub fn plan(
    ws: &Workspace,
    mode: ThreadsMode,
    threads_collapsed: bool,
    st: &SidebarThreads,
    now: Instant,
) -> Next {
    if mode == ThreadsMode::Off
        || st.in_flight.is_some()
        || !ws.settings().threads_enabled()
        || ws.from_cache()
        || *ws.connection() != ConnectionState::Connected
        || ws.user_id().is_none()
    {
        return Next::Idle;
    }
    if let Some(p) = st.paused_until.filter(|p| *p > now) {
        return Next::Wait(p);
    }
    let mut wake = None;
    let list = st.list_candidate(ws, mode, threads_collapsed, now, &mut wake);
    let get = st.get_candidate(ws, mode, threads_collapsed, now, &mut wake);
    if let Some((f, interval)) = list {
        let at = st.last_list.map_or(now, |t| t + interval);
        if at <= now {
            return Next::Fetch(f);
        }
        wake = earliest(wake, at);
    }
    if let Some(f) = get {
        let at = st.last_get.map_or(now, |t| t + GET_INTERVAL);
        if at <= now {
            return Next::Fetch(f);
        }
        wake = earliest(wake, at);
    }
    wake.map_or(Next::Idle, Next::Wait)
}

impl SidebarThreads {
    /// Whether a request for `key` may run now (else its back-off end is a
    /// wake-up time).
    fn ready(&self, key: &Key, now: Instant, wake: &mut Option<Instant>) -> bool {
        match self.backoff.get(key) {
            Some(b) if b.until > now => {
                *wake = earliest(*wake, b.until);
                false
            }
            _ => true,
        }
    }

    /// The next list request and its pacing interval.
    fn list_candidate(
        &self,
        ws: &Workspace,
        mode: ThreadsMode,
        collapsed: bool,
        now: Instant,
        wake: &mut Option<Instant>,
    ) -> Option<(Fetch, Duration)> {
        if let Some(p) = &self.paging
            && ws.subscription(&p.rid).is_some()
            && self.ready(&Key::Unread(p.rid.clone()), now, wake)
        {
            let f = Fetch::Unread {
                rid: p.rid.clone(),
                offset: p.offset,
            };
            return Some((f, LIST_INTERVAL));
        }
        // Rooms with unread threads to load, newest replies first.
        let mut rooms: Vec<(Option<RcDate>, &str)> = Vec::new();
        for (_, s) in listed(ws) {
            let mut due = self.refresh_unread.contains(&s.rid);
            for t in missing_in(ws, s) {
                if self.unresolved.contains(t) || self.settled.contains(t) {
                    continue;
                }
                let at = self
                    .missing_since
                    .get(t)
                    .map_or(now + DEBOUNCE, |s| *s + DEBOUNCE);
                if at <= now {
                    due = true;
                } else {
                    *wake = earliest(*wake, at);
                }
            }
            if due {
                rooms.push((s.lr, &s.rid));
            }
        }
        rooms.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        if let Some((_, rid)) = rooms
            .into_iter()
            .find(|(_, rid)| self.ready(&Key::Unread((*rid).to_owned()), now, wake))
        {
            let f = Fetch::Unread {
                rid: rid.to_owned(),
                offset: 0,
            };
            return Some((f, LIST_INTERVAL));
        }
        if mode != ThreadsMode::Following || collapsed {
            return None;
        }
        // Following crawl: rooms with thread replies first (newest first),
        // then the others by activity.
        let mut queue: Vec<(bool, Option<RcDate>, &str, Option<RcDate>)> = listed(ws)
            .filter(|(_, s)| wants_crawl(ws, self, &s.rid) && !self.crawl_failed.contains(&s.rid))
            .map(|(e, s)| {
                let order = s.lr.or_else(|| e.last_activity());
                (s.lr.is_some(), order, s.rid.as_str(), s.lr)
            })
            .collect();
        queue.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(b.2))
        });
        queue
            .into_iter()
            .find(|(_, _, rid, _)| self.ready(&Key::Followed((*rid).to_owned()), now, wake))
            .map(|(has_lr, _, rid, lr)| {
                let f = Fetch::Followed {
                    rid: rid.to_owned(),
                    activity: lr,
                };
                let interval = if has_lr {
                    LIST_INTERVAL
                } else {
                    SLOW_LIST_INTERVAL
                };
                (f, interval)
            })
    }

    /// The next single parent: kept threads first (rooms the user is in),
    /// then unread ones the unread list did not return. A kept thread that a
    /// list request still to run may return (the room's unread list, the
    /// room's Following crawl) waits for it: `chat.getMessage` is scarce.
    fn get_candidate(
        &self,
        ws: &Workspace,
        mode: ThreadsMode,
        collapsed: bool,
        now: Instant,
        wake: &mut Option<Instant>,
    ) -> Option<Fetch> {
        if self.gets >= GET_BUDGET {
            return None;
        }
        let idx = ws.thread_index();
        let wanted =
            |t: &str| idx.parent(t).is_none() && !idx.is_gone(t) && !self.settled.contains(t);
        let listed_sub = |rid: &str| {
            ws.rooms()
                .get(rid)
                .and_then(|e| e.sub.as_ref().filter(|s| s.open && !e.is_archived()))
        };
        let awaits_list = |rid: &str, tmid: &str| {
            let Some(s) = listed_sub(rid) else {
                return false;
            };
            let unread_pass = shown_unread(s).any(|t| t == tmid) && !self.unresolved.contains(tmid);
            let crawl = mode == ThreadsMode::Following
                && !collapsed
                && wants_crawl(ws, self, rid)
                && !self.crawl_failed.contains(rid);
            unread_pass || crawl
        };
        if matches!(mode, ThreadsMode::UnreadKept | ThreadsMode::Following) {
            for (rid, tmid) in idx.kept() {
                if ws.subscription(rid).is_some()
                    && wanted(tmid)
                    && !awaits_list(rid, tmid)
                    && self.ready(&Key::Parent(tmid.to_owned()), now, wake)
                {
                    return Some(Fetch::Parent {
                        rid: rid.to_owned(),
                        tmid: tmid.to_owned(),
                    });
                }
            }
        }
        if self.unresolved.is_empty() {
            return None;
        }
        let mut found: Option<(&str, &str)> = None;
        for (_, s) in listed(ws) {
            for t in missing_in(ws, s) {
                if self.unresolved.contains(t)
                    && wanted(t)
                    && found.is_none_or(|(_, f)| t.as_str() < f)
                    && self.ready(&Key::Parent(t.clone()), now, wake)
                {
                    found = Some((&s.rid, t));
                }
            }
        }
        found.map(|(rid, tmid)| Fetch::Parent {
            rid: rid.to_owned(),
            tmid: tmid.to_owned(),
        })
    }

    /// Logout: everything but the pacing and a rate-limit pause, which are
    /// per IP and outlast the account.
    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch + 1,
            last_list: self.last_list,
            last_get: self.last_get,
            paused_until: self.paused_until,
            ..Self::default()
        };
    }

    /// Pending work and timers are dropped: a new session (or none).
    fn reset_session(&mut self) {
        self.epoch += 1;
        self.changed = true;
        self.progress = None;
        self.synced_after_capture = false;
        self.in_flight = None;
        self.timer = None;
        self.gets = 0;
        self.sync_rev = None;
        self.lr_before = None;
        self.missing_since.clear();
        self.unresolved.clear();
        self.settled.clear();
        self.failed.clear();
        self.refresh_unread.clear();
        self.paging = None;
        self.recrawl.clear();
        self.crawl_failed.clear();
        self.backoff.clear();
    }

    fn set_mode(&mut self, mode: ThreadsMode) {
        self.mode_gen += 1;
        if mode != ThreadsMode::Following {
            self.recrawl.clear();
            self.crawl_failed.clear();
            self.backoff.retain(|k, _| !matches!(k, Key::Followed(_)));
        }
        if mode == ThreadsMode::Off {
            self.missing_since.clear();
            self.unresolved.clear();
            self.settled.clear();
            self.failed.clear();
            self.refresh_unread.clear();
            self.paging = None;
            self.backoff.clear();
        }
        self.mode = mode;
    }

    /// Live updates were (or are being) missed: the connection is down, or
    /// the session dropped events (a resync while connected). The next sync
    /// re-checks the rooms that had replies since.
    pub fn note_missed_live(&mut self, ws: &Workspace) {
        if self.lr_before.is_none() {
            self.lr_before = Some(listed(ws).map(|(_, s)| (s.rid.clone(), s.lr)).collect());
            self.synced_after_capture = false;
            self.changed = true;
        }
    }

    /// A sync (initial load or delta) starts now. Call it before the sync's
    /// request runs: a sync that started before live updates were missed
    /// does not settle them when applied.
    pub fn sync_started(&mut self, ws: &Workspace) {
        // A sync applied before this one started is checked as such.
        self.check_sync(ws);
        if self.lr_before.is_some() {
            self.synced_after_capture = true;
        }
        self.changed = true;
    }

    /// A sync was applied (`Dirty::SYNC` changed): if it started after live
    /// updates were missed, the rooms that had replies meanwhile are
    /// re-checked.
    fn check_sync(&mut self, ws: &Workspace) {
        let rev = ws.revision(Dirty::SYNC);
        if self.sync_rev.is_some_and(|r| r != rev)
            && self.synced_after_capture
            && let Some(before) = self.lr_before.take()
        {
            self.synced_after_capture = false;
            self.requeue_stale(ws, &before);
        }
        self.sync_rev = Some(rev);
    }

    /// Bookkeeping before [`plan`]: the mode, stale data after a sync, and
    /// since when unread threads lack their parent.
    pub fn observe(&mut self, ws: &Workspace, mode: ThreadsMode, now: Instant) {
        if mode != self.mode {
            self.set_mode(mode);
        }
        if *ws.connection() != ConnectionState::Connected {
            self.note_missed_live(ws);
        }
        self.check_sync(ws);
        if mode == ThreadsMode::Off {
            return;
        }
        if self
            .paging
            .as_ref()
            .is_some_and(|p| ws.subscription(&p.rid).is_none())
        {
            self.paging = None;
        }
        let missing: HashSet<&String> = listed(ws).flat_map(|(_, s)| missing_in(ws, s)).collect();
        self.missing_since.retain(|t, _| missing.contains(t));
        for t in missing {
            if !self.missing_since.contains_key(t) {
                self.missing_since.insert(t.clone(), now);
            }
        }
    }

    /// After a sync that followed missed live updates (`before`: each
    /// room's `lr` when they started). Rooms whose `lr` changed in between
    /// had replies the loader did not see: their known unread parents are
    /// listed again and, if crawled, they are crawled again. What failed for
    /// unrecognised reasons is tried again. Follows made elsewhere without a
    /// reply are not seen until "Refresh".
    fn requeue_stale(&mut self, ws: &Workspace, before: &HashMap<String, Option<RcDate>>) {
        for t in self.failed.drain() {
            self.settled.remove(&t);
        }
        self.crawl_failed.clear();
        if self.mode == ThreadsMode::Off {
            return;
        }
        let idx = ws.thread_index();
        for (_, s) in listed(ws) {
            // Not listed before (restored from the offline cache after live
            // updates were first missed): `confirm_restored` decides. Restored
            // rooms listed before are checked by both, which is harmless.
            if before.get(&s.rid).is_none_or(|lr| *lr == s.lr) {
                continue;
            }
            if shown_unread(s).any(|t| idx.parent(t).is_some()) {
                self.refresh_unread.insert(s.rid.clone());
            }
            if self.mode == ThreadsMode::Following && idx.followed(&s.rid).is_some() {
                self.recrawl.insert(s.rid.clone());
            }
        }
    }

    /// Records that `f` starts now.
    fn begin(&mut self, f: &Fetch, now: Instant) {
        let pass_start = match (f, &self.paging) {
            (Fetch::Unread { rid, .. }, Some(p)) if &p.rid == rid => p.pass_start,
            _ => now,
        };
        match f {
            Fetch::Parent { .. } => {
                self.last_get = Some(now);
                self.gets += 1;
            }
            Fetch::Unread { .. } | Fetch::Followed { .. } => self.last_list = Some(now),
        }
        self.in_flight = Some(InFlight {
            fetch: f.clone(),
            started: now,
            mode_gen: self.mode_gen,
            pass_start,
        });
    }

    /// Per update: follows the session, plans and starts the next request or
    /// sets the pacing timer.
    pub fn tick(
        &mut self,
        ws: &Workspace,
        session: Option<&SessionHandle>,
        mode: ThreadsMode,
        threads_collapsed: bool,
        now: Instant,
    ) -> Task<Msg> {
        let Some(h) = session else {
            if self.session.take().is_some() {
                self.reset_session();
            }
            self.tick_key = None;
            return Task::none();
        };
        let ptr = Arc::as_ptr(&h.0) as usize;
        if self.session != Some(ptr) {
            self.reset_session();
            self.session = Some(ptr);
        }
        if !self.needs_plan(ws, ptr, mode, threads_collapsed, now) {
            return Task::none();
        }
        self.observe(ws, mode, now);
        let next = plan(ws, mode, threads_collapsed, self, now);
        self.progress = self.crawl_progress(ws, mode);
        match next {
            Next::Fetch(f) => {
                self.due = None;
                self.begin(&f, now);
                request(h.rest().clone(), self.epoch, f)
            }
            Next::Wait(at) => {
                self.due = Some(at);
                self.schedule(at, now)
            }
            Next::Idle => {
                self.due = None;
                Task::none()
            }
        }
    }

    /// Whether `tick` has to plan: the workspace, mode or session changed,
    /// the loader state changed, or something planned is due. Records the
    /// inputs when it does.
    fn needs_plan(
        &mut self,
        ws: &Workspace,
        session: usize,
        mode: ThreadsMode,
        collapsed: bool,
        now: Instant,
    ) -> bool {
        let key = TickKey::new(ws, session, mode, collapsed);
        if self.tick_key == Some(key) && !self.changed && self.due.is_none_or(|d| now < d) {
            return false;
        }
        self.tick_key = Some(key);
        self.changed = false;
        true
    }

    /// Following crawl progress as of the last `tick`.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.progress
    }

    /// Sets the pacing timer unless one fires earlier.
    fn schedule(&mut self, at: Instant, now: Instant) -> Task<Msg> {
        if self.timer.is_some_and(|t| t <= at && t >= now) {
            return Task::none();
        }
        self.timer = Some(at);
        let wait = at.saturating_duration_since(now);
        Task::perform(tokio::time::sleep(wait), move |_| Msg::Pace { at })
    }

    /// Applies a result to the workspace's thread index.
    pub fn update(&mut self, msg: Msg, ws: &mut Workspace, now: Instant) -> Vec<Out> {
        self.changed = true;
        let (epoch, result) = match msg {
            Msg::Pace { at } => {
                if self.timer == Some(at) {
                    self.timer = None;
                }
                return Vec::new();
            }
            Msg::Done { epoch, result } => (epoch, result),
        };
        if epoch != self.epoch {
            return Vec::new();
        }
        let Some(fl) = self.in_flight.take() else {
            return Vec::new();
        };
        let key = fl.fetch.key();
        let failure = match &result {
            Done::Unread { res: Err(f), .. }
            | Done::Followed { res: Err(f), .. }
            | Done::Parent { res: Err(f), .. } => Some(f.clone()),
            _ => None,
        };
        if fl.mode_gen != self.mode_gen {
            // Asked for under another mode (e.g. before Off, whose state was
            // dropped): only a rate limit, which is per IP, still counts.
            if let Some(Class::RateLimited(wait)) = failure.as_ref().map(classify) {
                self.paused_until = Some(now + wait);
                if matches!(fl.fetch, Fetch::Parent { .. }) {
                    self.gets = self.gets.saturating_sub(1);
                }
            }
            return Vec::new();
        }
        match failure.clone().map(|f| (classify(&f), f)) {
            None => {
                self.backoff.remove(&key);
            }
            Some((Class::RateLimited(wait), f)) => {
                tracing::debug!(error = %f, "sidebar threads: rate limited");
                self.paused_until = Some(now + wait);
                // Refused before it ran: not a spent call.
                if matches!(fl.fetch, Fetch::Parent { .. }) {
                    self.gets = self.gets.saturating_sub(1);
                }
                self.keep_paging(&result, &fl);
                return Vec::new();
            }
            Some((class, f)) => {
                let failures = self.backoff.get(&key).map_or(0, |b| b.failures) + 1;
                let retry = match class {
                    Class::Transient => true,
                    Class::Other => failures < MAX_OTHER_FAILURES,
                    _ => false,
                };
                if retry {
                    tracing::debug!(error = %f, failures, "sidebar threads: retrying failure");
                    self.backoff.insert(
                        key,
                        Backoff {
                            failures,
                            until: now + retry_delay(failures),
                        },
                    );
                    self.keep_paging(&result, &fl);
                    return Vec::new();
                }
                tracing::debug!(error = %f, "sidebar threads: request failed");
                self.backoff.remove(&key);
            }
        }
        let other = failure
            .as_ref()
            .is_some_and(|f| classify(f) == Class::Other);
        let mut out = Vec::new();
        match result {
            Done::Unread { rid, offset, res } => {
                let more = match res {
                    Ok(page) => {
                        let more = page.has_more()
                            && page.raw_len > 0
                            && offset / UNREAD_PAGE + 1 < MAX_UNREAD_PAGES;
                        let next = offset + page.raw_len as u64;
                        ws.apply_thread_parents(page.items);
                        more.then_some(next)
                    }
                    Err(_) => {
                        // Not allowed or unusable: give up on the room's
                        // missing parents (unusable: until a reconnect).
                        if let Some(s) = ws.subscription(&rid) {
                            let missing: Vec<String> = missing_in(ws, s).cloned().collect();
                            if other {
                                self.failed.extend(missing.iter().cloned());
                            }
                            self.settled.extend(missing);
                        }
                        self.paging = None;
                        self.refresh_unread.remove(&rid);
                        return out;
                    }
                };
                let pending: Vec<String> = ws
                    .subscription(&rid)
                    .map(|s| {
                        missing_in(ws, s)
                            .filter(|t| {
                                self.missing_since
                                    .get(*t)
                                    .is_none_or(|at| *at <= fl.pass_start)
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                match more {
                    Some(offset) if !pending.is_empty() => {
                        self.paging = Some(Paging {
                            rid,
                            offset,
                            pass_start: fl.pass_start,
                        });
                    }
                    _ => {
                        self.paging = None;
                        self.refresh_unread.remove(&rid);
                        self.unresolved.extend(pending);
                    }
                }
            }
            Done::Parent { rid, tmid, res } => {
                self.settled.insert(tmid.clone());
                self.unresolved.remove(&tmid);
                match res {
                    Ok(m) => {
                        ws.apply_thread_parents(vec![*m]);
                        // Not a thread parent, or hidden / deleted since.
                        if ws.thread_index().parent(&tmid).is_none() {
                            ws.thread_gone(&tmid);
                        }
                    }
                    Err(f) if classify(&f) == Class::NotFound => {
                        ws.thread_gone(&tmid);
                        if ws.thread_index().is_kept(&tmid) {
                            out.push(Out::Unkeep { rid, tmid });
                        }
                    }
                    // Not allowed (left the room, lost access): kept.
                    Err(_) => {
                        if other {
                            self.failed.insert(tmid);
                        }
                    }
                }
            }
            Done::Followed { rid, activity, res } => {
                self.recrawl.remove(&rid);
                match res {
                    Ok(page) => ws.apply_followed(&rid, page.items, activity, fl.started),
                    Err(f) if classify(&f) == Class::NotAllowed => {
                        ws.apply_followed(&rid, Vec::new(), activity, fl.started);
                    }
                    // A re-crawl that gives up leaves a set that may miss
                    // follows: not current (crawled again after the next
                    // missed live updates, or "Refresh").
                    Err(_) => {
                        ws.mark_followed_threads_stale(&rid);
                        self.crawl_failed.insert(rid);
                    }
                }
            }
        }
        out
    }

    /// A retried unread page continues the pass it belongs to.
    fn keep_paging(&mut self, result: &Done, fl: &InFlight) {
        if let Done::Unread { rid, offset, .. } = result
            && *offset > 0
        {
            self.paging = Some(Paging {
                rid: rid.clone(),
                offset: *offset,
                pass_start: fl.pass_start,
            });
        }
    }

    /// Following crawl progress `(done, rooms)` while it runs (not while
    /// disconnected: nothing loads then).
    pub fn crawl_progress(&self, ws: &Workspace, mode: ThreadsMode) -> Option<(usize, usize)> {
        if mode != ThreadsMode::Following || *ws.connection() != ConnectionState::Connected {
            return None;
        }
        let (mut done, mut total) = (0, 0);
        for (_, s) in listed(ws) {
            total += 1;
            if !wants_crawl(ws, self, &s.rid) || self.crawl_failed.contains(&s.rid) {
                done += 1;
            }
        }
        (done < total).then_some((done, total))
    }

    /// Loads everything again ("Refresh"): crawled rooms are crawled again
    /// and known unread parents listed again; the rows stay until the new
    /// results replace them. What was given up on is tried again.
    pub fn refresh(&mut self, ws: &Workspace) {
        self.changed = true;
        self.crawl_failed.clear();
        self.settled.clear();
        self.failed.clear();
        self.unresolved.clear();
        self.backoff.clear();
        if self.mode == ThreadsMode::Off {
            return;
        }
        let idx = ws.thread_index();
        for (_, s) in listed(ws) {
            if shown_unread(s).any(|t| idx.parent(t).is_some()) {
                self.refresh_unread.insert(s.rid.clone());
            }
            if self.mode == ThreadsMode::Following && idx.followed(&s.rid).is_some() {
                self.recrawl.insert(s.rid.clone());
            }
        }
    }
}

/// What the offline cache keeps of the Threads section: the followed sets
/// of subscribed rooms and the parents that rows refer to (followed, kept,
/// unread), each with the room's `lr` up to which it is current. Data kept
/// current by live updates (nothing pending for the room) is current up to
/// the room's `lr` now, or, while live updates are missed, up to its `lr`
/// when that started.
pub fn cached_threads(ws: &Workspace, st: &SidebarThreads) -> CachedThreads {
    let idx = ws.thread_index();
    if !idx.enabled() {
        return CachedThreads::default();
    }
    let live = ws.last_sync().is_some()
        && !ws.from_cache()
        && *ws.connection() == ConnectionState::Connected
        && st.lr_before.is_none();
    // Up to which `lr` live updates kept a room current (None: unknown).
    let live_upto = |rid: &str, sub: &Subscription| -> Option<RcDate> {
        if live {
            sub.lr
        } else {
            st.lr_before.as_ref()?.get(rid).copied().flatten()
        }
    };
    let mut wanted: HashSet<&str> = HashSet::new();
    let mut followed = Vec::new();
    for (rid, f) in idx.followed_rooms() {
        let Some(sub) = ws.subscription(rid) else {
            continue;
        };
        let current = !f.stale && !st.recrawl.contains(rid) && !st.crawl_failed.contains(rid);
        let synced_activity = if current {
            f.synced_activity.max(live_upto(rid, sub))
        } else {
            f.synced_activity
        };
        wanted.extend(f.tmids.iter().map(String::as_str));
        followed.push(CachedFollowed {
            rid: rid.to_owned(),
            tmids: f.tmids.iter().cloned().collect(),
            synced_activity,
        });
    }
    wanted.extend(idx.kept().map(|(_, t)| t));
    for e in ws.rooms().values() {
        if let Some(s) = &e.sub {
            wanted.extend(
                [&s.tunread, &s.tunread_user, &s.tunread_group]
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(String::as_str),
            );
        }
    }
    let parents: Vec<Message> = wanted
        .into_iter()
        .filter_map(|t| idx.parent(t))
        .filter(|m| m.hidden != Some(true) && ws.subscription(&m.rid).is_some())
        .cloned()
        .collect();
    // Restored and not confirmed yet: as current as when restored.
    let mut unconfirmed: HashMap<&str, Option<RcDate>> = HashMap::new();
    for (rid, at) in idx.unconfirmed() {
        let e = unconfirmed.entry(rid).or_insert(at);
        *e = (*e).min(at);
    }
    let mut marks: HashMap<&str, Option<Option<RcDate>>> = HashMap::new();
    for m in &parents {
        let rid = m.rid.as_str();
        // A stale parent (listed again) makes the room's parents unknown.
        if idx.is_stale(&m.id) {
            marks.insert(rid, None);
            continue;
        }
        if marks.contains_key(rid) {
            continue;
        }
        let mark = if let Some(at) = unconfirmed.get(rid) {
            Some(*at)
        } else if st.refresh_unread.contains(rid) {
            None
        } else {
            ws.subscription(rid)
                .and_then(|s| live_upto(rid, s))
                .map(Some)
        };
        marks.insert(rid, mark);
    }
    let parents_synced = marks
        .into_iter()
        .filter_map(|(rid, mark)| {
            Some(CachedParentsSynced {
                rid: rid.to_owned(),
                synced_activity: mark?,
            })
        })
        .collect();
    CachedThreads {
        followed,
        parents,
        parents_synced,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    RateLimited(Duration),
    /// Network, timeout, 5xx: retried with back-off.
    Transient,
    /// `chat.getMessage` for a missing id: a bare `400 {"success":false}`.
    NotFound,
    /// `error-not-allowed`: no access to the room (or Threads disabled,
    /// which the planner sees from the settings).
    NotAllowed,
    Other,
}

fn classify(f: &Failure) -> Class {
    match f.error() {
        Error::RateLimited { retry_after, .. } => Class::RateLimited(
            retry_after
                .unwrap_or(RATE_LIMIT_PAUSE)
                .clamp(Duration::from_secs(1), MAX_PAUSE),
        ),
        Error::Api {
            status: 400,
            error: None,
            error_type: None,
            ..
        } => Class::NotFound,
        e if e.is_transient() => Class::Transient,
        e if e.api_error_code() == Some("error-not-allowed") => Class::NotAllowed,
        // E.g. the connection dropped while the body was read.
        Error::Http(_) | Error::Io(_) => Class::Transient,
        _ => Class::Other,
    }
}

fn request(rest: rocketchat::RestClient, epoch: u64, f: Fetch) -> Task<Msg> {
    let done = move |result| Msg::Done { epoch, result };
    match f {
        Fetch::Unread { rid, offset } => Task::perform(
            {
                let rid = rid.clone();
                async move {
                    rest.threads_list(&rid, Some("unread"), None, offset, UNREAD_PAGE)
                        .await
                        .map_err(Failure::from)
                }
            },
            move |res| done(Done::Unread { rid, offset, res }),
        ),
        Fetch::Parent { rid, tmid } => Task::perform(
            {
                let tmid = tmid.clone();
                async move {
                    rest.get_message(&tmid)
                        .await
                        .map(Box::new)
                        .map_err(Failure::from)
                }
            },
            move |res| done(Done::Parent { rid, tmid, res }),
        ),
        Fetch::Followed { rid, activity } => Task::perform(
            {
                let rid = rid.clone();
                async move {
                    rest.threads_list(&rid, Some("following"), None, 0, FOLLOWED_PER_ROOM as u64)
                        .await
                        .map_err(Failure::from)
                }
            },
            move |res| done(Done::Followed { rid, activity, res }),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::{RoomType, User, UserRef};
    use rustket_core::Change;
    use rustket_core::sync::DeltaData;

    fn ts(ms: i64) -> RcDate {
        RcDate::from_millis(ms).unwrap()
    }

    fn ws(mode: ThreadsMode) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        ws.apply_connection(ConnectionState::Connected);
        ws.set_thread_config(mode, []);
        ws
    }

    fn sub(rid: &str, lr: Option<i64>, tunread: &[&str]) -> Subscription {
        Subscription {
            id: format!("sub-{rid}"),
            rid: rid.into(),
            name: rid.into(),
            t: RoomType::Channel,
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            open: true,
            lr: lr.map(ts),
            tunread: (!tunread.is_empty())
                .then(|| tunread.iter().map(|t| (*t).to_owned()).collect()),
            ..Default::default()
        }
    }

    fn add(ws: &mut Workspace, rid: &str, lr: Option<i64>, tunread: &[&str]) {
        ws.apply_subscription(Change::Inserted, sub(rid, lr, tunread));
    }

    fn parent(id: &str, rid: &str, tlm: i64) -> Message {
        Message {
            id: id.into(),
            rid: rid.into(),
            msg: format!("text {id}"),
            ts: Some(ts(1)),
            tlm: Some(ts(tlm)),
            tcount: Some(1),
            replies: Some(vec!["me".into()]),
            updated_at: Some(ts(tlm)),
            u: UserRef {
                id: "bob".into(),
                username: Some("bob".into()),
                name: None,
            },
            ..Default::default()
        }
    }

    fn page(items: Vec<Message>, total: u64) -> Res<Paginated<Message>> {
        Ok(Paginated {
            raw_len: items.len(),
            count: items.len() as u64,
            items,
            offset: 0,
            total,
            dropped: 0,
        })
    }

    fn fail(e: Error) -> Failure {
        Failure::from(e)
    }

    fn not_found() -> Failure {
        fail(Error::Api {
            status: 400,
            error: None,
            error_type: None,
            message: None,
            details: None,
        })
    }

    fn not_allowed() -> Failure {
        fail(Error::Api {
            status: 400,
            error: Some("error-not-allowed".into()),
            error_type: Some("error-not-allowed".into()),
            message: Some("Not Allowed".into()),
            details: None,
        })
    }

    fn unread(rid: &str, offset: u64) -> Fetch {
        Fetch::Unread {
            rid: rid.into(),
            offset,
        }
    }

    fn get(rid: &str, tmid: &str) -> Fetch {
        Fetch::Parent {
            rid: rid.into(),
            tmid: tmid.into(),
        }
    }

    fn followed(rid: &str, lr: Option<i64>) -> Fetch {
        Fetch::Followed {
            rid: rid.into(),
            activity: lr.map(ts),
        }
    }

    /// The loader around a workspace, driven like `tick` without a session.
    struct Fx {
        ws: Workspace,
        st: SidebarThreads,
        mode: ThreadsMode,
        collapsed: bool,
        t0: Instant,
    }

    impl Fx {
        fn new(mode: ThreadsMode) -> Self {
            Self {
                ws: ws(mode),
                st: SidebarThreads::default(),
                mode,
                collapsed: false,
                t0: Instant::now(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn plan(&mut self, ms: u64) -> Next {
            let now = self.at(ms);
            self.st.observe(&self.ws, self.mode, now);
            plan(&self.ws, self.mode, self.collapsed, &self.st, now)
        }

        /// Plans at `ms` and starts what was planned.
        fn start(&mut self, ms: u64) -> Next {
            let next = self.plan(ms);
            let now = self.at(ms);
            if let Next::Fetch(f) = &next {
                self.st.begin(f, now);
            }
            next
        }

        /// A delta sync starts and is applied.
        fn delta(&mut self) {
            self.st.sync_started(&self.ws);
            self.ws.apply_delta(DeltaData::default());
        }

        fn done(&mut self, ms: u64, result: Done) -> Vec<Out> {
            let msg = Msg::Done {
                epoch: self.st.epoch,
                result,
            };
            let now = self.at(ms);
            self.st.update(msg, &mut self.ws, now)
        }
    }

    #[test]
    fn nothing_while_off_offline_or_busy() {
        let mut fx = Fx::new(ThreadsMode::Off);
        add(&mut fx.ws, "a", Some(5), &["t"]);
        assert_eq!(fx.plan(0), Next::Idle);
        assert_eq!(fx.plan(5000), Next::Idle);
        fx.mode = ThreadsMode::Unread;
        fx.ws.set_thread_config(ThreadsMode::Unread, []);
        fx.ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(fx.plan(6000), Next::Idle);
        fx.ws.apply_connection(ConnectionState::Connected);
        assert_eq!(fx.start(8000), Next::Fetch(unread("a", 0)));
        assert_eq!(fx.plan(20_000), Next::Idle, "one request at a time");
    }

    #[test]
    fn unread_loads_only_rooms_with_missing_parents_once() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(20), &["t1"]);
        add(&mut fx.ws, "b", Some(30), &["t2"]);
        add(&mut fx.ws, "c", Some(40), &[]);
        fx.ws.apply_thread_parents(vec![parent("t2", "b", 30)]);
        // A new unread thread waits for its parent broadcast first.
        assert_eq!(fx.plan(0), Next::Wait(fx.at(1000)));
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        // The list does not have it: asked for singly, once.
        fx.done(
            1100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(Vec::new(), 0),
            },
        );
        assert_eq!(fx.start(1200), Next::Fetch(get("a", "t1")));
        fx.done(
            1300,
            Done::Parent {
                rid: "a".into(),
                tmid: "t1".into(),
                res: Ok(Box::new(parent("t1", "a", 20))),
            },
        );
        assert!(fx.ws.thread_index().parent("t1").is_some());
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn unread_lists_are_paged_and_paced() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(20), &["x", "y"]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        let mut first = page(vec![parent("x", "a", 20)], 150).unwrap();
        first.raw_len = 100;
        fx.done(
            1100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: Ok(first),
            },
        );
        // getThreadsList calls start at least 2 s apart.
        assert_eq!(fx.plan(1500), Next::Wait(fx.at(3000)));
        assert_eq!(fx.start(3000), Next::Fetch(unread("a", 100)));
        fx.done(
            3100,
            Done::Unread {
                rid: "a".into(),
                offset: 100,
                res: page(vec![parent("y", "a", 10)], 150),
            },
        );
        assert!(fx.ws.thread_index().parent("y").is_some());
        assert_eq!(fx.plan(10_000), Next::Idle);
    }

    #[test]
    fn kept_parents_are_fetched_singly_and_removed_when_not_found() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", None, &[]);
        fx.ws.set_thread_config(
            ThreadsMode::UnreadKept,
            [("a", "k1"), ("a", "k2"), ("left", "k3")],
        );
        assert_eq!(fx.start(0), Next::Fetch(get("a", "k1")));
        let out = fx.done(
            100,
            Done::Parent {
                rid: "a".into(),
                tmid: "k1".into(),
                res: Err(not_found()),
            },
        );
        assert_eq!(
            out,
            [Out::Unkeep {
                rid: "a".into(),
                tmid: "k1".into()
            }]
        );
        assert!(fx.ws.thread_index().is_gone("k1"));
        // getMessage calls start at least 20 s apart.
        assert_eq!(fx.plan(1000), Next::Wait(fx.at(20_000)));
        assert_eq!(fx.start(20_000), Next::Fetch(get("a", "k2")));
        // Not allowed (e.g. access lost): the pin stays.
        let out = fx.done(
            20_100,
            Done::Parent {
                rid: "a".into(),
                tmid: "k2".into(),
                res: Err(not_allowed()),
            },
        );
        assert!(out.is_empty());
        assert!(!fx.ws.thread_index().is_gone("k2"));
        // k3 is in a room the user left: not fetched.
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn single_parents_have_a_session_budget() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", None, &[]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        fx.st.gets = GET_BUDGET;
        assert_eq!(fx.plan(0), Next::Idle);
        fx.st.reset_session();
        assert_eq!(fx.plan(0), Next::Fetch(get("a", "k")));
    }

    #[test]
    fn following_crawls_expanded_rooms_with_replies_first() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "quiet", None, &[]);
        add(&mut fx.ws, "b", Some(10), &[]);
        add(&mut fx.ws, "c", Some(30), &[]);
        add(&mut fx.ws, "done", Some(50), &[]);
        fx.ws
            .apply_followed("done", Vec::new(), Some(ts(50)), fx.t0);
        fx.collapsed = true;
        assert_eq!(fx.plan(0), Next::Idle);
        fx.collapsed = false;
        assert_eq!(fx.start(0), Next::Fetch(followed("c", Some(30))));
        fx.done(
            100,
            Done::Followed {
                rid: "c".into(),
                activity: Some(ts(30)),
                res: page(vec![parent("p", "c", 30)], 1),
            },
        );
        assert_eq!(
            fx.ws
                .thread_index()
                .followed("c")
                .unwrap()
                .tmids
                .iter()
                .collect::<Vec<_>>(),
            ["p"]
        );
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), Some((2, 4)));
        fx.ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
        fx.ws.apply_connection(ConnectionState::Connected);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), Some((2, 4)));
        assert_eq!(fx.plan(1000), Next::Wait(fx.at(2000)));
        assert_eq!(fx.start(2000), Next::Fetch(followed("b", Some(10))));
        fx.done(
            2100,
            Done::Followed {
                rid: "b".into(),
                activity: Some(ts(10)),
                res: page(Vec::new(), 0),
            },
        );
        // Rooms without thread replies come last, at a slower pace.
        assert_eq!(fx.plan(4000), Next::Wait(fx.at(8000)));
        assert_eq!(fx.start(8000), Next::Fetch(followed("quiet", None)));
        fx.done(
            8100,
            Done::Followed {
                rid: "quiet".into(),
                activity: None,
                res: Err(not_allowed()),
            },
        );
        assert!(fx.ws.thread_index().followed("quiet").is_some());
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn rate_limits_pause_everything_and_transient_errors_back_off() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "a", Some(10), &[]);
        add(&mut fx.ws, "b", Some(5), &[]);
        assert_eq!(fx.start(0), Next::Fetch(followed("a", Some(10))));
        fx.done(
            100,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(10)),
                res: Err(fail(Error::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                    message: None,
                })),
            },
        );
        assert_eq!(fx.plan(5000), Next::Wait(fx.at(30_100)));
        assert_eq!(fx.start(30_100), Next::Fetch(followed("a", Some(10))));
        // A timeout: not crawled, retried after a back-off; others go on.
        fx.done(
            30_200,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(10)),
                res: Err(fail(Error::Timeout("test"))),
            },
        );
        assert!(fx.ws.thread_index().followed("a").is_none());
        assert_eq!(fx.start(32_100), Next::Fetch(followed("b", Some(5))));
        fx.done(
            32_200,
            Done::Followed {
                rid: "b".into(),
                activity: Some(ts(5)),
                res: page(Vec::new(), 0),
            },
        );
        assert_eq!(fx.plan(34_100), Next::Wait(fx.at(35_200)));
        assert_eq!(fx.start(35_200), Next::Fetch(followed("a", Some(10))));
    }

    #[test]
    fn rooms_with_newer_replies_are_reloaded_after_a_sync() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "a", Some(10), &["t"]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 10)]);
        fx.ws
            .apply_followed("a", vec![parent("t", "a", 10)], Some(ts(10)), fx.t0);
        assert_eq!(fx.plan(0), Next::Idle);
        // A sync while connected throughout: live updates kept up.
        fx.delta();
        assert_eq!(fx.plan(500), Next::Idle);
        // Replies while disconnected: the delta sync brings the newer `lr`.
        fx.ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(fx.plan(600), Next::Idle);
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(50), &["t"]));
        assert_eq!(fx.plan(1000), Next::Idle, "not before the sync is applied");
        fx.delta();
        assert_eq!(fx.start(2000), Next::Fetch(unread("a", 0)));
        fx.done(
            2100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("t", "a", 50)], 1),
            },
        );
        assert_eq!(fx.ws.thread_index().parent("t").unwrap().tlm, Some(ts(50)));
        assert_eq!(fx.start(4000), Next::Fetch(followed("a", Some(50))));
        fx.done(
            4100,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(50)),
                res: page(vec![parent("t", "a", 50)], 1),
            },
        );
        assert_eq!(fx.plan(60_000), Next::Idle);
        // A sync without newer replies reloads nothing.
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(65_000);
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.delta();
        assert_eq!(fx.plan(70_000), Next::Idle);
    }

    /// The connection drops and comes back; the delta sync then applies
    /// `subs` (the state the server reports).
    fn reconnect(fx: &mut Fx, ms: u64, subs: Vec<Subscription>) {
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(ms);
        fx.ws.apply_connection(ConnectionState::Connected);
        for s in subs {
            fx.ws.apply_subscription(Change::Updated, s);
        }
        fx.delta();
    }

    #[test]
    fn reconnects_recheck_only_rooms_with_replies_while_away() {
        let mut fx = Fx::new(ThreadsMode::Following);
        // The server stamps `lr` a few ms after the reply's `tlm`.
        add(&mut fx.ws, "a", Some(1003), &["t"]);
        add(&mut fx.ws, "b", Some(2000), &[]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 1000)]);
        fx.ws
            .apply_followed("a", vec![parent("t", "a", 1000)], Some(ts(1003)), fx.t0);
        fx.ws.apply_followed("b", Vec::new(), Some(ts(2000)), fx.t0);
        assert_eq!(fx.plan(0), Next::Idle);
        // A reply while connected: seen live.
        fx.ws
            .apply_subscription(Change::Updated, sub("b", Some(2500), &[]));
        assert_eq!(fx.plan(100), Next::Idle);
        // Nothing happened while away: nothing is loaded again.
        reconnect(&mut fx, 1000, Vec::new());
        assert_eq!(fx.plan(2000), Next::Idle);
        // A reply in `a` while away: its unread list, then its crawl.
        reconnect(&mut fx, 3000, vec![sub("a", Some(4003), &["t"])]);
        assert_eq!(fx.start(4000), Next::Fetch(unread("a", 0)));
        fx.done(
            4100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("t", "a", 4000)], 1),
            },
        );
        assert_eq!(fx.start(6000), Next::Fetch(followed("a", Some(4003))));
        fx.done(
            6100,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(4003)),
                res: page(vec![parent("t", "a", 4000)], 1),
            },
        );
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn refresh_reloads_known_threads_and_keeps_the_rows_meanwhile() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "a", Some(1003), &["t"]);
        add(&mut fx.ws, "b", Some(2000), &[]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 1000)]);
        fx.ws
            .apply_followed("a", vec![parent("t", "a", 1000)], Some(ts(1003)), fx.t0);
        fx.ws
            .apply_followed("b", vec![parent("u", "b", 1500)], Some(ts(2000)), fx.t0);
        assert_eq!(fx.plan(0), Next::Idle);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
        fx.st.refresh(&fx.ws);
        // The followed sets stay until the crawl replaces them.
        assert!(fx.ws.thread_index().followed("a").is_some());
        assert!(fx.ws.thread_index().followed("b").is_some());
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), Some((0, 2)));
        assert_eq!(fx.start(100), Next::Fetch(unread("a", 0)));
        fx.done(
            200,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("t", "a", 3000)], 1),
            },
        );
        assert_eq!(fx.start(2200), Next::Fetch(followed("b", Some(2000))));
        fx.done(
            2300,
            Done::Followed {
                rid: "b".into(),
                activity: Some(ts(2000)),
                res: page(Vec::new(), 0),
            },
        );
        assert!(fx.ws.thread_index().followed("a").is_some());
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), Some((1, 2)));
        assert_eq!(fx.start(4300), Next::Fetch(followed("a", Some(1003))));
        fx.done(
            4400,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(1003)),
                res: page(vec![parent("t", "a", 3000)], 1),
            },
        );
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn refresh_in_unread_mode_lists_known_unread_parents_again() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(1003), &["t"]);
        add(&mut fx.ws, "b", Some(2000), &[]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 1000)]);
        assert_eq!(fx.plan(0), Next::Idle);
        fx.st.refresh(&fx.ws);
        assert!(fx.ws.thread_index().parent("t").is_some());
        assert_eq!(fx.start(100), Next::Fetch(unread("a", 0)));
        fx.done(
            200,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("t", "a", 1000)], 1),
            },
        );
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    /// Review: a result that arrived after switching to Off settled its
    /// thread for the session, so it stayed "Loading…" once back on.
    #[test]
    fn results_from_before_a_mode_change_are_dropped() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", Some(10), &["u"]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        assert_eq!(fx.start(0), Next::Fetch(get("a", "k")));
        fx.mode = ThreadsMode::Off;
        fx.ws.set_thread_config(ThreadsMode::Off, []);
        assert_eq!(fx.plan(100), Next::Idle);
        fx.done(
            200,
            Done::Parent {
                rid: "a".into(),
                tmid: "k".into(),
                res: Ok(Box::new(parent("k", "a", 5))),
            },
        );
        assert!(!fx.st.settled.contains("k"));
        fx.mode = ThreadsMode::UnreadKept;
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        assert_eq!(fx.plan(1500), Next::Wait(fx.at(2500)), "debounced again");
        assert_eq!(fx.start(2500), Next::Fetch(unread("a", 0)));
        fx.mode = ThreadsMode::Off;
        fx.ws.set_thread_config(ThreadsMode::Off, []);
        fx.plan(2600);
        fx.done(
            2700,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(Vec::new(), 0),
            },
        );
        assert!(fx.st.unresolved.is_empty() && fx.st.paging.is_none());
        fx.mode = ThreadsMode::UnreadKept;
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        assert_eq!(fx.plan(2800), Next::Wait(fx.at(3800)));
        assert_eq!(fx.start(4500), Next::Fetch(unread("a", 0)));
        fx.done(
            4600,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("u", "a", 10)], 1),
            },
        );
        assert_eq!(fx.start(20_000), Next::Fetch(get("a", "k")));
    }

    /// Review: a sync that was already running when live updates were
    /// missed consumed the capture, so the delta after it re-checked
    /// nothing.
    #[test]
    fn a_sync_started_before_the_gap_does_not_settle_it() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(1003), &["t"]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 1000)]);
        assert_eq!(fx.plan(0), Next::Idle);
        // A sync runs while the link drops and comes back.
        fx.st.sync_started(&fx.ws);
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(100);
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.st.note_missed_live(&fx.ws);
        // It lands with the state from before the gap.
        fx.ws.apply_delta(DeltaData::default());
        assert_eq!(fx.plan(200), Next::Idle);
        assert!(fx.st.lr_before.is_some());
        // The pending resync brings the reply.
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(5003), &["t"]));
        fx.delta();
        assert_eq!(fx.start(300), Next::Fetch(unread("a", 0)));
    }

    /// Review: `tick` planned on every app update.
    #[test]
    fn ticks_plan_only_when_something_changed() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(10), &[]);
        let (mode, t0) = (fx.mode, fx.t0);
        assert!(fx.st.needs_plan(&fx.ws, 1, mode, false, t0));
        assert!(!fx.st.needs_plan(&fx.ws, 1, mode, false, fx.at(10)));
        assert!(fx.st.needs_plan(&fx.ws, 1, mode, true, fx.at(20)));
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(20)));
        add(&mut fx.ws, "b", Some(10), &["t"]);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(30)));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(40)));
        fx.st.due = Some(fx.at(1000));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(999)));
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(1000)));
        fx.st.due = None;
        let at = fx.at(1000);
        let _ = fx.st.update(Msg::Pace { at }, &mut fx.ws, at);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(1001)));
        fx.st.refresh(&fx.ws);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(1002)));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, true, fx.at(1003)));
    }

    #[test]
    fn a_resync_while_connected_rechecks_rooms() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(1003), &["t"]);
        fx.ws.apply_thread_parents(vec![parent("t", "a", 1000)]);
        assert_eq!(fx.plan(0), Next::Idle);
        // Events were dropped: the app notes it, the delta brings the reply.
        fx.st.note_missed_live(&fx.ws);
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(5003), &["t"]));
        assert_eq!(fx.plan(100), Next::Idle, "not before the sync is applied");
        fx.delta();
        assert_eq!(fx.start(200), Next::Fetch(unread("a", 0)));
    }

    #[test]
    fn unrecognised_failures_are_retried_then_given_up_until_a_reconnect() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "a", Some(10), &[]);
        let proxy = || {
            Err(fail(Error::Status {
                status: 403,
                body: "<html>".into(),
            }))
        };
        let mut at = 0;
        for (i, wait) in [5000, 10_000, 0].into_iter().enumerate() {
            assert_eq!(
                fx.start(at),
                Next::Fetch(followed("a", Some(10))),
                "try {i}"
            );
            fx.done(
                at + 100,
                Done::Followed {
                    rid: "a".into(),
                    activity: Some(ts(10)),
                    res: proxy(),
                },
            );
            assert!(fx.ws.thread_index().followed("a").is_none());
            if wait > 0 {
                assert_eq!(fx.plan(at + 200), Next::Wait(fx.at(at + 100 + wait)));
                at += 100 + wait;
            }
        }
        // Given up for now; tried again after a reconnect.
        assert_eq!(fx.plan(60_000), Next::Idle);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
        reconnect(&mut fx, 61_000, Vec::new());
        assert_eq!(fx.start(62_000), Next::Fetch(followed("a", Some(10))));
    }

    #[test]
    fn unrecognised_parent_failures_are_tried_again_after_a_reconnect() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", None, &[]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        let mut at = 0;
        for _ in 0..MAX_OTHER_FAILURES {
            assert_eq!(fx.start(at), Next::Fetch(get("a", "k")));
            fx.done(
                at + 100,
                Done::Parent {
                    rid: "a".into(),
                    tmid: "k".into(),
                    res: Err(fail(Error::Protocol("bad".into()))),
                },
            );
            at += 60_000;
        }
        assert_eq!(fx.plan(at), Next::Idle);
        assert!(!fx.ws.thread_index().is_gone("k"));
        reconnect(&mut fx, at + 1000, Vec::new());
        assert_eq!(fx.start(at + 2000), Next::Fetch(get("a", "k")));
    }

    #[test]
    fn a_failed_page_is_retried_at_its_offset() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(20), &["x", "y"]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        let mut first = page(vec![parent("x", "a", 20)], 150).unwrap();
        first.raw_len = 100;
        fx.done(
            1100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: Ok(first),
            },
        );
        assert_eq!(fx.start(3000), Next::Fetch(unread("a", 100)));
        fx.done(
            3100,
            Done::Unread {
                rid: "a".into(),
                offset: 100,
                res: Err(fail(Error::Timeout("test"))),
            },
        );
        assert_eq!(fx.plan(3200), Next::Wait(fx.at(8100)));
        assert_eq!(fx.start(8100), Next::Fetch(unread("a", 100)));
    }

    #[test]
    fn nothing_from_the_cache_or_with_threads_disabled() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.ws = Workspace::new();
        fx.ws.apply_cached(rustket_core::cache::CachedState {
            subscriptions: vec![sub("a", Some(10), &["t"])],
            ..Default::default()
        });
        fx.ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.ws.set_thread_config(ThreadsMode::Unread, []);
        assert!(fx.ws.from_cache());
        fx.plan(0);
        assert_eq!(fx.plan(5000), Next::Idle);

        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(10), &["t"]);
        fx.ws.apply_setting(
            Change::Inserted,
            rocketchat::models::Setting {
                id: "Threads_enabled".into(),
                value: serde_json::Value::Bool(false),
                ..Default::default()
            },
        );
        fx.plan(0);
        assert_eq!(fx.plan(5000), Next::Idle);
    }

    #[test]
    fn kept_parents_wait_for_the_lists_that_may_bring_them() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", Some(10), &["k"]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        // Unread too: the room's unread list first, getMessage only if
        // the list does not have it.
        assert_eq!(fx.plan(0), Next::Wait(fx.at(1000)));
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        fx.done(
            1100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(Vec::new(), 0),
            },
        );
        assert_eq!(fx.start(1200), Next::Fetch(get("a", "k")));

        // Following: the room's crawl first.
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "b", Some(10), &[]);
        fx.ws
            .set_thread_config(ThreadsMode::Following, [("b", "k")]);
        assert_eq!(fx.start(0), Next::Fetch(followed("b", Some(10))));
        fx.done(
            100,
            Done::Followed {
                rid: "b".into(),
                activity: Some(ts(10)),
                res: page(vec![parent("k", "b", 10)], 1),
            },
        );
        assert!(fx.ws.thread_index().parent("k").is_some());
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn rate_limited_parents_do_not_spend_the_budget() {
        let mut fx = Fx::new(ThreadsMode::UnreadKept);
        add(&mut fx.ws, "a", None, &[]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("a", "k")]);
        assert_eq!(fx.start(0), Next::Fetch(get("a", "k")));
        assert_eq!(fx.st.gets, 1);
        fx.done(
            100,
            Done::Parent {
                rid: "a".into(),
                tmid: "k".into(),
                res: Err(fail(Error::RateLimited {
                    retry_after: None,
                    message: None,
                })),
            },
        );
        assert_eq!(fx.st.gets, 0);
    }

    #[test]
    fn a_logout_keeps_the_pacing() {
        let mut st = SidebarThreads::default();
        let t = Instant::now();
        st.last_list = Some(t);
        st.last_get = Some(t);
        st.paused_until = Some(t + Duration::from_secs(30));
        st.gets = 7;
        let epoch = st.epoch;
        st.reset();
        assert_eq!(
            (st.last_list, st.last_get, st.paused_until),
            (Some(t), Some(t), Some(t + Duration::from_secs(30)))
        );
        assert_eq!(st.gets, 0);
        assert!(st.epoch > epoch);
    }

    #[test]
    fn results_of_an_older_session_are_dropped() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        add(&mut fx.ws, "a", Some(10), &["t"]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        let old = fx.st.epoch;
        fx.st.reset_session();
        let out = fx.st.update(
            Msg::Done {
                epoch: old,
                result: Done::Unread {
                    rid: "a".into(),
                    offset: 0,
                    res: page(vec![parent("t", "a", 10)], 1),
                },
            },
            &mut fx.ws,
            fx.t0 + Duration::from_millis(1100),
        );
        assert!(out.is_empty());
        assert!(fx.ws.thread_index().parent("t").is_none());
        // The new session plans afresh (after the debounce).
        fx.plan(2000);
        assert_eq!(fx.plan(3000), Next::Fetch(unread("a", 0)));
    }

    /// A restart: the loader saw the connection down before the offline
    /// cache (with thread data) was shown, then the initial load arrives.
    fn restart(
        fx: &mut Fx,
        cached: Vec<Subscription>,
        threads: CachedThreads,
        server: Vec<Subscription>,
    ) {
        restart_with(fx, cached, threads, server, false);
    }

    /// A restart; `cache_seen_offline`: the loader also saw the cached rooms
    /// while still connecting (it notes their cached `lr` as the point live
    /// updates were missed from), and the connection is up only right before
    /// the initial load.
    fn restart_with(
        fx: &mut Fx,
        cached: Vec<Subscription>,
        threads: CachedThreads,
        server: Vec<Subscription>,
        cache_seen_offline: bool,
    ) {
        fx.ws = Workspace::new();
        fx.st = SidebarThreads::default();
        fx.ws.set_thread_config(fx.mode, []);
        fx.ws.apply_connection(ConnectionState::Connecting);
        fx.plan(0);
        let me = User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        };
        assert!(fx.ws.apply_cached(rustket_core::cache::CachedState {
            me: Some(me.clone()),
            subscriptions: cached,
            threads,
            ..Default::default()
        }));
        if cache_seen_offline {
            assert_eq!(fx.plan(50), Next::Idle);
        }
        fx.ws.apply_connection(ConnectionState::Connected);
        if !cache_seen_offline {
            assert_eq!(fx.plan(100), Next::Idle, "nothing while the cache is shown");
        }
        fx.st.sync_started(&fx.ws);
        fx.ws.apply_initial(rustket_core::sync::InitialData {
            me,
            subscriptions: server,
            next_since: Some(ts(1)),
            ..Default::default()
        });
    }

    fn cached_set(rid: &str, tmids: &[&str], synced: i64) -> CachedFollowed {
        CachedFollowed {
            rid: rid.into(),
            tmids: tmids.iter().map(|t| (*t).to_owned()).collect(),
            synced_activity: Some(ts(synced)),
        }
    }

    fn mark(rid: &str, synced: i64) -> CachedParentsSynced {
        CachedParentsSynced {
            rid: rid.into(),
            synced_activity: Some(ts(synced)),
        }
    }

    #[test]
    fn after_a_restart_only_rooms_with_newer_activity_are_crawled() {
        for offline in [false, true] {
            after_a_restart_only_rooms_with_newer_activity_are_crawled_(offline);
        }
    }

    fn after_a_restart_only_rooms_with_newer_activity_are_crawled_(cache_seen_offline: bool) {
        let mut fx = Fx::new(ThreadsMode::Following);
        let threads = CachedThreads {
            followed: vec![
                cached_set("a", &["t"], 1003),
                cached_set("b", &[], 1500),
                cached_set("c", &[], 3000),
            ],
            parents: vec![parent("t", "a", 1000)],
            parents_synced: vec![mark("a", 1003)],
        };
        let cached = vec![
            sub("a", Some(1003), &[]),
            sub("b", Some(1500), &[]),
            sub("c", Some(3000), &[]),
        ];
        // b had a reply while the app was not running.
        let server = vec![
            sub("a", Some(1003), &[]),
            sub("b", Some(2500), &[]),
            sub("c", Some(3000), &[]),
        ];
        restart_with(&mut fx, cached, threads, server, cache_seen_offline);
        let idx = fx.ws.thread_index();
        assert!(idx.followed("a").unwrap().tmids.contains("t"));
        assert!(idx.parent("t").is_some());
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), Some((2, 3)));
        assert_eq!(fx.start(200), Next::Fetch(followed("b", Some(2500))));
        fx.done(
            300,
            Done::Followed {
                rid: "b".into(),
                activity: Some(ts(2500)),
                res: page(vec![parent("u", "b", 2400)], 1),
            },
        );
        assert!(
            fx.ws
                .thread_index()
                .followed("b")
                .unwrap()
                .tmids
                .contains("u")
        );
        assert_eq!(fx.plan(60_000), Next::Idle);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode), None);
    }

    #[test]
    fn after_a_restart_stale_unread_parents_are_listed_again() {
        for offline in [false, true] {
            after_a_restart_stale_unread_parents_are_listed_again_(offline);
        }
    }

    fn after_a_restart_stale_unread_parents_are_listed_again_(cache_seen_offline: bool) {
        let mut fx = Fx::new(ThreadsMode::Unread);
        let threads = CachedThreads {
            followed: Vec::new(),
            parents: vec![parent("x", "a", 1000), parent("y", "b", 2000)],
            parents_synced: vec![mark("a", 1003), mark("b", 2003)],
        };
        let cached = vec![sub("a", Some(1003), &["x"]), sub("b", Some(2003), &["y"])];
        // A reply in x after the cache was written; none in b.
        let server = vec![sub("a", Some(4003), &["x"]), sub("b", Some(2003), &["y"])];
        restart_with(&mut fx, cached, threads, server, cache_seen_offline);
        assert!(fx.ws.thread_index().is_stale("x"));
        assert!(!fx.ws.thread_index().is_stale("y"));
        fx.plan(200);
        assert_eq!(fx.start(1200), Next::Fetch(unread("a", 0)));
        fx.done(
            1300,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(vec![parent("x", "a", 4000)], 1),
            },
        );
        assert!(!fx.ws.thread_index().is_stale("x"));
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn a_stale_parent_the_list_misses_is_not_fetched_singly() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        let threads = CachedThreads {
            followed: Vec::new(),
            parents: vec![parent("x", "a", 1000)],
            parents_synced: vec![mark("a", 1003)],
        };
        restart(
            &mut fx,
            vec![sub("a", Some(1003), &["x"])],
            threads,
            vec![sub("a", Some(4003), &["x"])],
        );
        fx.plan(200);
        assert_eq!(fx.start(1200), Next::Fetch(unread("a", 0)));
        fx.done(
            1300,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: page(Vec::new(), 0),
            },
        );
        assert!(fx.ws.thread_index().parent("x").is_some(), "still shown");
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn cached_sets_kept_current_live_are_current_now() {
        let mut fx = Fx::new(ThreadsMode::Following);
        restart(
            &mut fx,
            vec![sub("a", Some(1000), &[]), sub("b", Some(1000), &[])],
            CachedThreads {
                followed: vec![cached_set("a", &[], 1000)],
                ..Default::default()
            },
            vec![sub("a", Some(1000), &[]), sub("b", Some(1000), &[])],
        );
        fx.ws.apply_followed("b", Vec::new(), Some(ts(1000)), fx.t0);
        // Replies seen live after the crawls.
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(5000), &[]));
        fx.ws
            .apply_subscription(Change::Updated, sub("b", Some(6000), &[]));
        fx.plan(200);
        let synced = |fx: &Fx| {
            let mut t = cached_threads(&fx.ws, &fx.st);
            t.followed.sort_by(|a, b| a.rid.cmp(&b.rid));
            t.followed
                .into_iter()
                .map(|f| (f.rid, f.synced_activity))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            synced(&fx),
            [("a".into(), Some(ts(5000))), ("b".into(), Some(ts(6000)))]
        );
        // While disconnected (replies may be missed): current up to the
        // `lr` when that started.
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(300);
        assert_eq!(
            synced(&fx),
            [("a".into(), Some(ts(5000))), ("b".into(), Some(ts(6000)))]
        );
        // A reply in `a` was missed: as crawled until crawled again.
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(7000), &[]));
        fx.delta();
        fx.plan(400);
        assert_eq!(
            synced(&fx),
            [("a".into(), Some(ts(1000))), ("b".into(), Some(ts(6000)))]
        );
        // The re-crawl gives up: still as crawled (also once it is no
        // longer queued), and crawled again after the next reconnect.
        let mut at = 1000;
        for _ in 0..MAX_OTHER_FAILURES {
            let next = loop {
                match fx.start(at) {
                    Next::Fetch(f) => break f,
                    _ => at += 1000,
                }
            };
            assert_eq!(next, followed("a", Some(7000)));
            fx.done(
                at + 100,
                Done::Followed {
                    rid: "a".into(),
                    activity: Some(ts(7000)),
                    res: Err(fail(Error::Protocol("bad".into()))),
                },
            );
        }
        assert_eq!(fx.plan(at + 60_000), Next::Idle);
        assert!(fx.ws.thread_index().followed("a").unwrap().stale);
        assert_eq!(
            synced(&fx),
            [("a".into(), Some(ts(1000))), ("b".into(), Some(ts(6000)))]
        );
        reconnect(&mut fx, at + 61_000, Vec::new());
        assert_eq!(
            fx.start(at + 62_000),
            Next::Fetch(followed("a", Some(7000)))
        );
    }

    #[test]
    fn cached_parents_are_current_up_to_what_live_updates_covered() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        restart(
            &mut fx,
            vec![sub("a", Some(1003), &["x"]), sub("b", Some(2003), &["y"])],
            CachedThreads {
                parents: vec![parent("x", "a", 1000), parent("y", "b", 2000)],
                parents_synced: vec![mark("a", 1003), mark("b", 1500)],
                ..Default::default()
            },
            vec![sub("a", Some(1003), &["x"]), sub("b", Some(2003), &["y"])],
        );
        let marks = |fx: &Fx| {
            let mut m: Vec<_> = cached_threads(&fx.ws, &fx.st)
                .parents_synced
                .into_iter()
                .map(|p| (p.rid, p.synced_activity))
                .collect();
            m.sort();
            m
        };
        // b's parent is stale (listed again): b has no mark until then.
        assert!(fx.ws.thread_index().is_stale("y"));
        fx.plan(200);
        assert_eq!(marks(&fx), [("a".into(), Some(ts(1003)))]);
        assert_eq!(fx.start(1200), Next::Fetch(unread("b", 0)));
        fx.done(
            1300,
            Done::Unread {
                rid: "b".into(),
                offset: 0,
                res: page(vec![parent("y", "b", 2000)], 1),
            },
        );
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(3003), &["x"]));
        fx.plan(1400);
        assert_eq!(
            marks(&fx),
            [("a".into(), Some(ts(3003))), ("b".into(), Some(ts(2003)))]
        );
        // Disconnected: up to the `lr` when live updates were missed.
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(1500);
        assert_eq!(
            marks(&fx),
            [("a".into(), Some(ts(3003))), ("b".into(), Some(ts(2003)))]
        );
        // A reply in `a` was missed: no mark until its parents are listed
        // again.
        fx.ws.apply_connection(ConnectionState::Connected);
        fx.ws
            .apply_subscription(Change::Updated, sub("a", Some(4003), &["x"]));
        fx.delta();
        fx.plan(1600);
        assert_eq!(marks(&fx), [("b".into(), Some(ts(2003)))]);
    }

    #[test]
    fn unconfirmed_parents_keep_their_cached_mark() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.ws = Workspace::new();
        fx.ws.set_thread_config(fx.mode, []);
        fx.ws.apply_connection(ConnectionState::Connecting);
        assert!(fx.ws.apply_cached(rustket_core::cache::CachedState {
            me: Some(User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            }),
            subscriptions: vec![sub("a", Some(5003), &["x"])],
            threads: CachedThreads {
                parents: vec![parent("x", "a", 1000)],
                parents_synced: vec![mark("a", 1003)],
                ..Default::default()
            },
            ..Default::default()
        }));
        fx.plan(0);
        // Never connected: what was written stays (not the cached `lr`).
        let t = cached_threads(&fx.ws, &fx.st);
        assert_eq!(t.parents_synced, [mark("a", 1003)]);
    }

    #[test]
    fn cached_parents_are_those_rows_refer_to() {
        let mut fx = Fx::new(ThreadsMode::Following);
        add(&mut fx.ws, "a", Some(10), &["u"]);
        fx.ws
            .set_thread_config(ThreadsMode::Following, [("a", "k")]);
        fx.ws.apply_thread_parents(vec![
            parent("u", "a", 10),
            parent("k", "a", 10),
            parent("f", "a", 10),
        ]);
        fx.ws
            .apply_followed("a", vec![parent("f", "a", 10)], Some(ts(10)), fx.t0);
        // Seen in a timeline page only (not referenced): not kept.
        let mut other = parent("o", "a", 10);
        other.replies = None;
        fx.ws.apply_thread_parents(vec![other]);
        let mut ids: Vec<String> = cached_threads(&fx.ws, &fx.st)
            .parents
            .into_iter()
            .map(|m| m.id)
            .collect();
        ids.sort();
        assert_eq!(ids, ["f", "k", "u"]);
        fx.ws.set_thread_config(ThreadsMode::Off, []);
        assert!(cached_threads(&fx.ws, &fx.st).is_empty());
    }

    #[test]
    fn errors_are_classified() {
        assert_eq!(classify(&not_found()), Class::NotFound);
        assert_eq!(classify(&not_allowed()), Class::NotAllowed);
        assert_eq!(classify(&fail(Error::Timeout("t"))), Class::Transient);
        assert_eq!(
            classify(&fail(Error::Api {
                status: 503,
                error: None,
                error_type: None,
                message: None,
                details: None,
            })),
            Class::Transient
        );
        assert_eq!(
            classify(&fail(Error::RateLimited {
                retry_after: None,
                message: None,
            })),
            Class::RateLimited(RATE_LIMIT_PAUSE)
        );
        assert_eq!(
            classify(&fail(Error::Io(std::io::Error::other("reset")))),
            Class::Transient
        );
        assert_eq!(
            classify(&fail(Error::Status {
                status: 404,
                body: String::new(),
            })),
            Class::Other
        );
    }
}
