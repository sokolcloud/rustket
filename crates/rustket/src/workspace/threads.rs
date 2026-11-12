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
//! session (parents only starred or flagged: [`PIN_GET_BUDGET`]), and nothing is fetched while disconnected or showing the offline
//! cache. [`plan`] picks the next request and is pure;
//! [`SidebarThreads::tick`] runs it when its inputs changed.
//!
//! With a row limit ([`Cap`]) only loads that can still change the rows
//! within the limit run. For the newest-first sort a room's `lr` bounds the
//! activity of the threads the user replied in (followers get `lr` on each
//! reply), so rooms go newest `lr` first and stop once the limit is filled by
//! loaded rows newer than every remaining room's `lr`. Following a thread
//! without replying does not set `lr`: for a room not crawled yet, the newest
//! stored parent the user follows (e.g. a live follow) raises its bound. A
//! follow made while live updates were missed in a room whose `lr` is older
//! than the rows within the limit shows after "Refresh" or the thread's next
//! reply. By room name the bound is exact (rooms in name order).
//! Oldest-first has no such bound: loading stops once the loaded rows fill
//! the limit. Kept threads are always loaded. A room crawled with a page
//! smaller than [`FOLLOWED_PER_ROOM`] that had more ([`ShortCrawl`], kept in
//! the offline cache) is crawled again when the page grows, when the crawl
//! dropped threads (deleted, hidden) or when its set shrank since.
//!
//! Starred threads (Favorites) are tracked whatever the Threads mode is,
//! while the thread index has pins on. Live updates and every parent copy
//! keep their star state current; stars made while this device was away,
//! or on threads it never indexed, are found by scanning each listed room's
//! starred messages (`chat.getStarredMessages`, its own route and limit:
//! [`STARRED_INTERVAL`] apart, its own rate-limit pause, only when no other
//! request is ready). A room is scanned when it never was, when its scan
//! (restored from the offline cache) predates its last message or is older
//! than [`STARRED_RESCAN_TTL`], and after missed live updates when it had
//! messages meanwhile. Stars do not bump `lm`: a star made elsewhere on an
//! old thread of a quiet room shows after the TTL or a refresh. A room with
//! many starred messages is scanned [`STARRED_PAGES_PER_TURN`] pages at a
//! time, other rooms going in between.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::{Message, Paginated, RoomType, Subscription};
use rocketchat::{Error, RcDate};
use rustket_core::cache::{CachedFollowed, CachedParentsSynced, CachedStarred, CachedThreads};
use rustket_core::config::{SidebarSort, ThreadsMode, UiPrefs};
use rustket_core::room_list::{SectionKind, display_name, thread_unread};
use rustket_core::sidebar_layout::{SectionId, section_sort, section_unread_first};
use rustket_core::thread_index::{FOLLOWED_PER_ROOM, FollowedRoom, ShortCrawl, follows};
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
/// `chat.getMessage` calls per session for the Threads section (kept and
/// unread parents).
pub const GET_BUDGET: u32 = 20;
/// `chat.getMessage` calls per session for parents only starred or flagged
/// (e.g. flagged on another device): a budget of their own, so many of them
/// do not starve the Threads section.
pub const PIN_GET_BUDGET: u32 = 20;
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
/// Minimum time between two `chat.getStarredMessages` calls (at most 6 of
/// the 10 per minute allowed per IP: the rest is left to the room's
/// "Starred messages" panel).
pub const STARRED_INTERVAL: Duration = Duration::from_secs(10);
/// Page size of a starred-messages scan.
const STARRED_PAGE: u64 = 100;
/// Pages of one room's scan before other rooms due go first.
pub const STARRED_PAGES_PER_TURN: u32 = 5;
/// A scan restored from the offline cache older than this runs again even
/// for a room without new messages.
pub const STARRED_RESCAN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Tries of a request whose failure is not recognised (e.g. a proxy's
/// error page, an unreadable body) before it is given up for the session.
const MAX_OTHER_FAILURES: u32 = 3;

/// Wait after `failures` (1-based) transient failures of one request: 5 s,
/// doubling, at most 5 min.
fn retry_delay(failures: u32) -> Duration {
    let secs = 5u64.saturating_mul(1 << failures.saturating_sub(1).min(6));
    Duration::from_secs(secs.min(300))
}

/// What the Threads section shows at most, which bounds the loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// Most rows; 0: no limit.
    pub limit: u32,
    pub sort: SidebarSort,
    pub unread_first: bool,
    /// `ui.use_real_names` (room names order the by-name sort).
    pub real_names: Option<bool>,
}

impl Default for Cap {
    /// No limit.
    fn default() -> Self {
        Self {
            limit: 0,
            sort: SidebarSort::Activity,
            unread_first: true,
            real_names: None,
        }
    }
}

impl Cap {
    pub fn from_prefs(p: &UiPrefs) -> Self {
        let id = SectionId::Builtin(SectionKind::Threads);
        Self {
            limit: p.sidebar_threads_limit,
            sort: section_sort(p, &id),
            unread_first: section_unread_first(p, &id),
            real_names: p.use_real_names,
        }
    }

    fn limit(&self) -> Option<usize> {
        (self.limit > 0).then_some(self.limit as usize)
    }

    /// Page size of a room's followed list.
    fn followed_page(&self) -> usize {
        self.limit()
            .map_or(FOLLOWED_PER_ROOM, |l| l.min(FOLLOWED_PER_ROOM))
    }
}

/// The rows the Threads section has now, to tell which rows still to load
/// can land within the limit. Starred threads are in Favorites, not here.
#[derive(Debug, Default)]
struct Bound {
    /// `None`: no limit, everything is loaded.
    limit: Option<usize>,
    sort: SidebarSort,
    unread_first: bool,
    /// Unread rows, loaded or not.
    unread: usize,
    /// Activity of loaded unread / read rows, ascending.
    tlm_unread: Vec<Option<RcDate>>,
    tlm_read: Vec<Option<RcDate>>,
    /// By name: `(lower-case room name, rid)` of loaded unread / read rows,
    /// ascending, and the lower-case names of listed rooms.
    key_unread: Vec<(String, String)>,
    key_read: Vec<(String, String)>,
    names: HashMap<String, String>,
    /// Following: rooms without a followed set, with the newest activity of
    /// stored parents the user follows (`lr` misses follows without a reply).
    followed_hint: HashMap<String, Option<RcDate>>,
}

impl Bound {
    fn new(ws: &Workspace, mode: ThreadsMode, cap: Cap) -> Self {
        let idx = ws.thread_index();
        let mut followed_hint: HashMap<String, Option<RcDate>> = HashMap::new();
        if mode == ThreadsMode::Following {
            let me = ws.user_id();
            for p in idx.parents() {
                if idx.followed(&p.rid).is_none()
                    && p.t.as_deref() != Some("rm")
                    && p.hidden != Some(true)
                    && follows(p, me)
                {
                    let e = followed_hint.entry(p.rid.clone()).or_default();
                    *e = (*e).max(rustket_core::thread_index::activity(p));
                }
            }
        }
        let Some(limit) = cap.limit() else {
            return Self {
                followed_hint,
                ..Self::default()
            };
        };
        let mut b = Self {
            limit: Some(limit),
            sort: cap.sort,
            unread_first: cap.unread_first,
            followed_hint,
            ..Self::default()
        };
        if cap.sort == SidebarSort::Alphabetical {
            let real = ws.use_real_names(cap.real_names);
            b.names = listed(ws)
                .map(|(e, s)| (s.rid.clone(), display_name(ws, e, real).to_lowercase()))
                .collect();
        }
        // Starred and flagged threads take no row here.
        let mut seen: HashSet<&str> = idx.pinned().map(|(_, t)| t).collect();
        let mut loaded: Vec<(bool, &str, Option<RcDate>)> = Vec::new();
        for (_, s) in listed(ws) {
            for t in shown_unread(s) {
                if !seen.insert(t) {
                    continue;
                }
                b.unread += 1;
                if let Some(p) = idx.parent(t).filter(|p| p.rid == s.rid) {
                    loaded.push((true, &s.rid, rustket_core::thread_index::activity(p)));
                }
            }
        }
        let is_listed = |rid: &str| {
            ws.rooms()
                .get(rid)
                .is_some_and(|e| e.sub.as_ref().is_some_and(|s| s.open) && !e.is_archived())
        };
        let read = |rid: &'_ str, tmid: &'_ str, removed_shown: bool| -> Option<Option<RcDate>> {
            let p = idx.parent(tmid).filter(|p| p.rid == rid)?;
            (removed_shown || p.t.as_deref() != Some("rm"))
                .then(|| rustket_core::thread_index::activity(p))
        };
        if mode != ThreadsMode::Unread {
            for (rid, tmid) in idx.kept() {
                if is_listed(rid)
                    && seen.insert(tmid)
                    && let Some(tlm) = read(rid, tmid, true)
                {
                    loaded.push((false, rid, tlm));
                }
            }
        }
        if mode == ThreadsMode::Following {
            for (rid, f) in idx.followed_rooms() {
                if !is_listed(rid) {
                    continue;
                }
                for t in &f.tmids {
                    if seen.insert(t)
                        && let Some(tlm) = read(rid, t, false)
                    {
                        loaded.push((false, rid, tlm));
                    }
                }
            }
        }
        for (unread, rid, tlm) in loaded {
            if b.sort == SidebarSort::Alphabetical {
                let key = b.room_key(rid);
                if unread {
                    &mut b.key_unread
                } else {
                    &mut b.key_read
                }
                .push(key);
            } else {
                if unread {
                    &mut b.tlm_unread
                } else {
                    &mut b.tlm_read
                }
                .push(tlm);
            }
        }
        b.tlm_unread.sort();
        b.tlm_read.sort();
        b.key_unread.sort();
        b.key_read.sort();
        b
    }

    /// Upper bound of the activity of a room's followed threads: its `lr`,
    /// or newer followed parents stored for a room not crawled yet.
    fn crawl_lr(&self, s: &Subscription) -> Option<RcDate> {
        s.lr.max(self.followed_hint.get(&s.rid).copied().flatten())
    }

    fn room_key(&self, rid: &str) -> (String, String) {
        (
            self.names.get(rid).cloned().unwrap_or_default(),
            rid.to_owned(),
        )
    }

    /// Loaded unread (or read) rows that sort before any row of room `rid`
    /// whose activity is at most `lr`.
    fn before(&self, unread: bool, rid: &str, lr: Option<RcDate>) -> usize {
        let (tlms, keys) = if unread {
            (&self.tlm_unread, &self.key_unread)
        } else {
            (&self.tlm_read, &self.key_read)
        };
        match self.sort {
            SidebarSort::Activity => tlms.len() - tlms.partition_point(|t| *t <= lr),
            // No bound: whatever is loaded counts.
            SidebarSort::ActivityAsc => tlms.len(),
            SidebarSort::Alphabetical => {
                let k = self.room_key(rid);
                keys.partition_point(|x| *x < k)
            }
        }
    }

    /// Whether a row still to load in room `rid`, with activity at most `lr`
    /// (the room's `lr`), can land within the limit.
    fn reaches(&self, rid: &str, lr: Option<RcDate>, unread: bool) -> bool {
        let Some(limit) = self.limit else {
            return true;
        };
        let before = match (self.unread_first, unread) {
            (true, true) => self.before(true, rid, lr),
            // Every unread row, loaded or not, comes first.
            (true, false) => self.unread + self.before(false, rid, lr),
            (false, _) => self.before(true, rid, lr) + self.before(false, rid, lr),
        };
        before < limit
    }

    /// Orders rooms the way the section's sort lists their threads.
    fn room_order(
        &self,
        a: (Option<RcDate>, &str),
        b: (Option<RcDate>, &str),
    ) -> std::cmp::Ordering {
        match self.sort {
            SidebarSort::Activity => b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)),
            SidebarSort::ActivityAsc => match (a.0, b.0) {
                (Some(x), Some(y)) => x.cmp(&y),
                (x, y) => x.is_none().cmp(&y.is_none()),
            }
            .then_with(|| a.1.cmp(b.1)),
            SidebarSort::Alphabetical => self.room_key(a.1).cmp(&self.room_key(b.1)),
        }
    }
}

/// A request [`plan`] asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// One page of a room's unread threads.
    Unread { rid: String, offset: u64 },
    /// One parent (kept, or unread but missing from the list; `pinned`:
    /// only starred or flagged, from [`PIN_GET_BUDGET`]).
    Parent {
        rid: String,
        tmid: String,
        pinned: bool,
    },
    /// A room's newest `count` followed threads; `activity` is the room's
    /// `lr` now.
    Followed {
        rid: String,
        activity: Option<RcDate>,
        count: usize,
    },
    /// One page of a room's starred messages; `lm` is the room's last
    /// message now.
    Starred {
        rid: String,
        offset: u64,
        lm: Option<RcDate>,
    },
}

impl Fetch {
    fn key(&self) -> Key {
        match self {
            Self::Unread { rid, .. } => Key::Unread(rid.clone()),
            Self::Parent { tmid, .. } => Key::Parent(tmid.clone()),
            Self::Followed { rid, .. } => Key::Followed(rid.clone()),
            Self::Starred { rid, .. } => Key::Starred(rid.clone()),
        }
    }
}

/// The target of a request, for back-off after transient failures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Unread(String),
    Parent(String),
    Followed(String),
    Starred(String),
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
    Starred {
        rid: String,
        offset: u64,
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
    /// Unread pages: when the room's pass started and the oldest activity
    /// listed so far.
    pass_start: Instant,
    floor: Option<RcDate>,
}

/// The rest of a room's unread pass (more pages wanted). A pass whose
/// next page cannot reach the rows within the limit waits (until the limit
/// grows, or a newer unread thread starts a new pass).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Paging {
    offset: u64,
    pass_start: Instant,
    /// Oldest activity listed so far (the list is newest first): what is
    /// left of the room is at most this old.
    floor: Option<RcDate>,
}

/// A room's starred-messages scan in progress (more pages to come).
#[derive(Debug, Clone, PartialEq, Eq)]
struct StarredPass {
    /// Offset of the next page.
    offset: u64,
    /// When the first page was asked for: star changes seen since win.
    started: Instant,
    /// The room's `lm` then.
    lm: Option<RcDate>,
    /// Starred thread parents of the pages so far.
    found: Vec<String>,
    pages: u32,
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
    /// Each listed room's last message when `lr_before` was captured.
    lm_before: Option<HashMap<String, Option<RcDate>>>,
    in_flight: Option<InFlight>,
    /// Start of the last list / single-parent request (kept across sessions
    /// and logouts: the limits are per IP).
    last_list: Option<Instant>,
    last_get: Option<Instant>,
    /// Rate limited: nothing before this (kept like `last_list`).
    paused_until: Option<Instant>,
    /// The same for the starred-messages route, limited on its own.
    last_starred: Option<Instant>,
    starred_paused_until: Option<Instant>,
    /// `chat.getMessage` calls in this session (Threads section, pins).
    gets: u32,
    pin_gets: u32,
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
    /// Unfinished unread passes by room.
    paging: BTreeMap<String, Paging>,
    /// Crawled rooms to crawl again (replies while live updates were missed).
    recrawl: BTreeSet<String>,
    /// Rooms whose crawl failed (unrecognised failures); crawled again after
    /// missed live updates.
    crawl_failed: HashSet<String>,
    /// Scanned rooms to scan again (restored scans not current, messages
    /// while live updates were missed, a refresh).
    starred_due: BTreeSet<String>,
    /// Unfinished starred scans by room.
    starred_passes: BTreeMap<String, StarredPass>,
    /// Rooms whose scan was checked against the room's `lm` (or made) in
    /// this session.
    starred_checked: HashSet<String>,
    /// Rooms whose scan failed (not allowed, unrecognised): scanned again
    /// after missed live updates or a refresh.
    starred_failed: HashSet<String>,
    backoff: HashMap<Key, Backoff>,
}

/// What a plan depends on besides the loader state and the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickKey {
    session: usize,
    mode: ThreadsMode,
    cap: Cap,
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
    fn new(ws: &Workspace, session: usize, mode: ThreadsMode, cap: Cap, collapsed: bool) -> Self {
        Self {
            session,
            mode,
            cap,
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

/// Rooms of the room list: subscribed, open, not archived, not service
/// rooms.
fn listed(ws: &Workspace) -> impl Iterator<Item = (&RoomEntry, &Subscription)> {
    ws.rooms().values().filter_map(|e| {
        e.sub
            .as_ref()
            .filter(|s| s.open && !e.is_archived() && !e.is_service_room())
            .map(|s| (e, s))
    })
}

/// Rooms whose starred messages are scanned: listed, not omnichannel.
fn scanned_rooms(ws: &Workspace) -> impl Iterator<Item = (&RoomEntry, &Subscription)> {
    listed(ws).filter(|(e, _)| !matches!(e.room_type(), RoomType::Livechat | RoomType::Voip))
}

/// A room's last message.
fn room_lm(e: &RoomEntry) -> Option<RcDate> {
    let r = e.room.as_ref()?;
    r.lm.or_else(|| r.last_message.as_ref().and_then(|m| m.ts))
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

/// Threads to ask for in a room's crawl: `page`, plus what the last short
/// crawl dropped (deleted, hidden), at most [`FOLLOWED_PER_ROOM`].
fn crawl_count(f: Option<&FollowedRoom>, page: usize) -> usize {
    let dropped = f
        .and_then(|f| f.short)
        .map_or(0, |s| s.count.saturating_sub(s.kept) as usize);
    (page + dropped).min(FOLLOWED_PER_ROOM).max(page)
}

/// Whether a room's followed threads are to be crawled (again): not crawled,
/// restored and not current, re-queued, or a short crawl ([`ShortCrawl`])
/// whose page is now too small or whose set shrank since (unfollowed,
/// deleted).
fn wants_crawl(ws: &Workspace, st: &SidebarThreads, rid: &str, page: usize) -> bool {
    let idx = ws.thread_index();
    let Some(f) = idx.followed(rid) else {
        return true;
    };
    let short = |s: ShortCrawl| {
        (s.count as usize) < crawl_count(Some(f), page) || idx.followed_shown(rid) < s.kept as usize
    };
    f.stale || st.recrawl.contains(rid) || f.short.is_some_and(short)
}

fn earliest(a: Option<Instant>, b: Instant) -> Option<Instant> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// The next request, given the workspace, the Threads mode, the section's
/// row limit and sort, whether the Threads section is collapsed (the
/// Following crawl waits then) and the loader state. Order: unread pages,
/// then single parents, then the Following crawl; a request that must wait
/// for its pacing lets a ready one of the other kind go first. Starred
/// scans (pins on, any mode) come last: only when nothing else is ready.
/// With the Threads section off only starred threads are loaded.
pub fn plan(
    ws: &Workspace,
    mode: ThreadsMode,
    cap: Cap,
    threads_collapsed: bool,
    st: &SidebarThreads,
    now: Instant,
) -> Next {
    let pins = ws.thread_index().pins_enabled();
    if !ws.thread_index().indexing()
        || st.in_flight.is_some()
        || !ws.settings().threads_enabled()
        || ws.from_cache()
        || *ws.connection() != ConnectionState::Connected
        || ws.user_id().is_none()
    {
        return Next::Idle;
    }
    let mut wake = None;
    if let Some(p) = st.paused_until.filter(|p| *p > now) {
        wake = Some(p);
    } else {
        let bound = Bound::new(ws, mode, cap);
        let page = cap.followed_page();
        let list = (mode != ThreadsMode::Off)
            .then(|| st.list_candidate(ws, mode, &bound, page, threads_collapsed, now, &mut wake))
            .flatten();
        let get = st.get_candidate(ws, mode, &bound, page, threads_collapsed, now, &mut wake);
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
    }
    if pins {
        if let Some(p) = st.starred_paused_until.filter(|p| *p > now) {
            wake = earliest(wake, p);
        } else if let Some(f) = st.starred_candidate(ws, now, &mut wake) {
            let at = st.last_starred.map_or(now, |t| t + STARRED_INTERVAL);
            if at <= now {
                return Next::Fetch(f);
            }
            wake = earliest(wake, at);
        }
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
    #[allow(clippy::too_many_arguments)]
    fn list_candidate(
        &self,
        ws: &Workspace,
        mode: ThreadsMode,
        bound: &Bound,
        page: usize,
        collapsed: bool,
        now: Instant,
        wake: &mut Option<Instant>,
    ) -> Option<(Fetch, Duration)> {
        let mut passes: Vec<(Option<RcDate>, &str, u64)> = self
            .paging
            .iter()
            .filter(|(rid, p)| ws.subscription(rid).is_some() && bound.reaches(rid, p.floor, true))
            .map(|(rid, p)| (p.floor, rid.as_str(), p.offset))
            .collect();
        passes.sort_by(|a, b| bound.room_order((a.0, a.1), (b.0, b.1)));
        if let Some((_, rid, offset)) = passes
            .into_iter()
            .find(|(_, rid, _)| self.ready(&Key::Unread((*rid).to_owned()), now, wake))
        {
            let f = Fetch::Unread {
                rid: rid.to_owned(),
                offset,
            };
            return Some((f, LIST_INTERVAL));
        }
        // Rooms with unread threads to load that can reach the rows within
        // the limit, in the section's order (newest replies first). A room
        // with an unfinished pass starts a new one only for unread threads
        // that came after it started.
        let mut rooms: Vec<(Option<RcDate>, &str)> = Vec::new();
        for (_, s) in listed(ws) {
            if !bound.reaches(&s.rid, s.lr, true) {
                continue;
            }
            let pass = self.paging.get(&s.rid);
            let mut due = self.refresh_unread.contains(&s.rid) && pass.is_none();
            for t in missing_in(ws, s) {
                if pass.is_some_and(|p| {
                    self.missing_since
                        .get(t)
                        .is_none_or(|at| *at <= p.pass_start)
                }) {
                    continue;
                }
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
        rooms.sort_by(|a, b| bound.room_order(*a, *b));
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
        // Following crawl: rooms with thread replies first (in the section's
        // order, newest first), then the others by activity; only rooms whose
        // followed threads can reach the rows within the limit.
        let mut queue: Vec<(bool, Option<RcDate>, &str, Option<RcDate>)> = listed(ws)
            .filter(|(_, s)| self.crawl_due(ws, bound, page, s))
            .map(|(e, s)| {
                let upper = bound.crawl_lr(s);
                let order = upper.or_else(|| e.last_activity());
                (upper.is_some(), order, s.rid.as_str(), s.lr)
            })
            .collect();
        queue.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                if a.0 {
                    bound.room_order((a.1, a.2), (b.1, b.2))
                } else {
                    b.1.cmp(&a.1).then_with(|| a.2.cmp(b.2))
                }
            })
        });
        queue
            .into_iter()
            .find(|(_, _, rid, _)| self.ready(&Key::Followed((*rid).to_owned()), now, wake))
            .map(|(has_lr, _, rid, lr)| {
                let f = Fetch::Followed {
                    rid: rid.to_owned(),
                    activity: lr,
                    count: crawl_count(ws.thread_index().followed(rid), page),
                };
                let interval = if has_lr {
                    LIST_INTERVAL
                } else {
                    SLOW_LIST_INTERVAL
                };
                (f, interval)
            })
    }

    /// Whether a room's crawl is to run: wanted, not given up, and its
    /// followed threads can reach the rows within the limit.
    fn crawl_due(&self, ws: &Workspace, bound: &Bound, page: usize, s: &Subscription) -> bool {
        wants_crawl(ws, self, &s.rid, page)
            && !self.crawl_failed.contains(&s.rid)
            && bound.reaches(&s.rid, bound.crawl_lr(s), false)
    }

    /// Whether room `rid` is to be scanned for starred threads, and how
    /// urgently: 0 a scan under way, 1 not scanned or due, 2 a scan that had
    /// its turn of pages (other rooms first).
    fn starred_tier(&self, ws: &Workspace, rid: &str) -> Option<u8> {
        let omnichannel =
            |e: &RoomEntry| matches!(e.room_type(), RoomType::Livechat | RoomType::Voip);
        if self.starred_failed.contains(rid) || ws.room(rid).is_none_or(omnichannel) {
            return None;
        }
        match self.starred_passes.get(rid) {
            Some(p) if p.pages > 0 && p.pages % STARRED_PAGES_PER_TURN == 0 => Some(2),
            Some(_) => Some(0),
            None if self.starred_due.contains(rid)
                || ws.thread_index().starred_scan(rid).is_none() =>
            {
                Some(1)
            }
            None => None,
        }
    }

    /// The next starred-messages page: scans under way, then rooms not
    /// scanned or due, newest activity first.
    fn starred_candidate(
        &self,
        ws: &Workspace,
        now: Instant,
        wake: &mut Option<Instant>,
    ) -> Option<Fetch> {
        let mut queue: Vec<(u8, Option<RcDate>, &str, &RoomEntry)> = scanned_rooms(ws)
            .filter_map(|(e, s)| {
                let tier = self.starred_tier(ws, &s.rid)?;
                Some((tier, e.last_activity(), s.rid.as_str(), e))
            })
            .collect();
        queue.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(b.2))
        });
        queue
            .into_iter()
            .find(|(_, _, rid, _)| self.ready(&Key::Starred((*rid).to_owned()), now, wake))
            .map(|(_, _, rid, e)| Fetch::Starred {
                rid: rid.to_owned(),
                offset: self.starred_passes.get(rid).map_or(0, |p| p.offset),
                lm: room_lm(e),
            })
    }

    /// The next single parent: kept threads first (rooms the user is in),
    /// then unread ones the unread list did not return, then (from a budget
    /// of their own) starred and flagged ones. A kept thread that a
    /// list request still to run may return (the room's unread list, the
    /// room's Following crawl) waits for it: `chat.getMessage` is scarce.
    #[allow(clippy::too_many_arguments)]
    fn get_candidate(
        &self,
        ws: &Workspace,
        mode: ThreadsMode,
        bound: &Bound,
        page: usize,
        collapsed: bool,
        now: Instant,
        wake: &mut Option<Instant>,
    ) -> Option<Fetch> {
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
            let unread_pass = shown_unread(s).any(|t| t == tmid)
                && !self.unresolved.contains(tmid)
                && bound.reaches(rid, self.paging.get(rid).map_or(s.lr, |p| p.floor), true);
            let crawl =
                mode == ThreadsMode::Following && !collapsed && self.crawl_due(ws, bound, page, s);
            unread_pass || crawl
        };
        let parent = |rid: &str, tmid: &str, pinned| Fetch::Parent {
            rid: rid.to_owned(),
            tmid: tmid.to_owned(),
            pinned,
        };
        if self.gets < GET_BUDGET {
            if matches!(mode, ThreadsMode::UnreadKept | ThreadsMode::Following) {
                for (rid, tmid) in idx.kept() {
                    if ws.subscription(rid).is_some()
                        && wanted(tmid)
                        && !awaits_list(rid, tmid)
                        && self.ready(&Key::Parent(tmid.to_owned()), now, wake)
                    {
                        return Some(parent(rid, tmid, false));
                    }
                }
            }
            if !self.unresolved.is_empty() {
                let mut found: Option<(&str, &str)> = None;
                for (_, s) in listed(ws) {
                    if !bound.reaches(&s.rid, s.lr, true) {
                        continue;
                    }
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
                if let Some((rid, tmid)) = found {
                    return Some(parent(rid, tmid, false));
                }
            }
        }
        if self.pin_gets >= PIN_GET_BUDGET {
            return None;
        }
        // Starred threads without a parent (e.g. restored from the offline
        // cache), unless the room's scan, which brings it, is to run.
        for (rid, tmid) in idx.starred() {
            if listed_sub(rid).is_some()
                && wanted(tmid)
                && self.starred_tier(ws, rid).is_none()
                && (mode == ThreadsMode::Off || !awaits_list(rid, tmid))
                && self.ready(&Key::Parent(tmid.to_owned()), now, wake)
            {
                return Some(parent(rid, tmid, true));
            }
        }
        // Flagged threads without a parent (flagged on another device).
        for (rid, tmid) in idx.flagged() {
            if listed_sub(rid).is_some()
                && wanted(tmid)
                && (mode == ThreadsMode::Off || !awaits_list(rid, tmid))
                && self.ready(&Key::Parent(tmid.to_owned()), now, wake)
            {
                return Some(parent(rid, tmid, true));
            }
        }
        None
    }

    /// Logout: everything but the pacing and a rate-limit pause, which are
    /// per IP and outlast the account.
    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch + 1,
            last_list: self.last_list,
            last_get: self.last_get,
            paused_until: self.paused_until,
            last_starred: self.last_starred,
            starred_paused_until: self.starred_paused_until,
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
        self.pin_gets = 0;
        self.sync_rev = None;
        self.lr_before = None;
        self.lm_before = None;
        self.missing_since.clear();
        self.unresolved.clear();
        self.settled.clear();
        self.failed.clear();
        self.refresh_unread.clear();
        self.paging.clear();
        self.recrawl.clear();
        self.crawl_failed.clear();
        self.starred_due.clear();
        self.starred_passes.clear();
        self.starred_checked.clear();
        self.starred_failed.clear();
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
            self.paging.clear();
            self.backoff.retain(|k, _| matches!(k, Key::Starred(_)));
        }
        self.mode = mode;
    }

    /// Live updates were (or are being) missed: the connection is down, or
    /// the session dropped events (a resync while connected). The next sync
    /// re-checks the rooms that had replies since.
    pub fn note_missed_live(&mut self, ws: &Workspace) {
        if self.lr_before.is_none() {
            self.lr_before = Some(listed(ws).map(|(_, s)| (s.rid.clone(), s.lr)).collect());
            self.lm_before = Some(
                listed(ws)
                    .map(|(e, s)| (s.rid.clone(), room_lm(e)))
                    .collect(),
            );
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
            let lm_before = self.lm_before.take().unwrap_or_default();
            self.requeue_stale(ws, &before, &lm_before);
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
        self.observe_starred(ws);
        if mode == ThreadsMode::Off {
            return;
        }
        self.paging.retain(|rid, _| ws.subscription(rid).is_some());
        let missing: HashSet<&String> = listed(ws).flat_map(|(_, s)| missing_in(ws, s)).collect();
        self.missing_since.retain(|t, _| missing.contains(t));
        for t in missing {
            if !self.missing_since.contains_key(t) {
                self.missing_since.insert(t.clone(), now);
            }
        }
    }

    /// Starred-scan bookkeeping: everything goes with pins off; scans under
    /// way of rooms left are dropped; once the workspace is live, each scan
    /// not checked yet in this session (restored from the offline cache) is
    /// due when the room had messages since or it is older than
    /// [`STARRED_RESCAN_TTL`].
    fn observe_starred(&mut self, ws: &Workspace) {
        let idx = ws.thread_index();
        if !idx.pins_enabled() {
            self.starred_due.clear();
            self.starred_passes.clear();
            self.starred_checked.clear();
            self.starred_failed.clear();
            return;
        }
        self.starred_passes
            .retain(|rid, _| ws.subscription(rid).is_some());
        if ws.from_cache() || *ws.connection() != ConnectionState::Connected {
            return;
        }
        let now_ms = RcDate::now().timestamp_millis();
        let ttl_ms = STARRED_RESCAN_TTL.as_millis() as i64;
        for (rid, scan) in idx.starred_scans() {
            if self.starred_checked.contains(rid) {
                continue;
            }
            self.starred_checked.insert(rid.to_owned());
            let old = scan
                .scanned_at
                .is_none_or(|at| now_ms.saturating_sub(at.timestamp_millis()) > ttl_ms);
            if old || ws.room(rid).and_then(room_lm) > scan.lm {
                self.starred_due.insert(rid.to_owned());
                self.changed = true;
            }
        }
    }

    /// After a sync that followed missed live updates (`before`: each
    /// room's `lr` when they started). Rooms whose `lr` changed in between
    /// had replies the loader did not see: their known unread parents are
    /// listed again and, if crawled, they are crawled again. What failed for
    /// unrecognised reasons is tried again. Follows made elsewhere without a
    /// reply are not seen until "Refresh". Scanned rooms with messages since
    /// (`lm_before`: each room's last message then) are scanned again, and
    /// scans under way start over (stars made meanwhile were missed).
    fn requeue_stale(
        &mut self,
        ws: &Workspace,
        before: &HashMap<String, Option<RcDate>>,
        lm_before: &HashMap<String, Option<RcDate>>,
    ) {
        for t in self.failed.drain() {
            self.settled.remove(&t);
        }
        self.crawl_failed.clear();
        self.starred_failed.clear();
        self.starred_passes.clear();
        let idx = ws.thread_index();
        for (e, s) in scanned_rooms(ws) {
            if idx.starred_scan(&s.rid).is_some()
                && lm_before.get(&s.rid).is_some_and(|lm| *lm != room_lm(e))
            {
                self.starred_due.insert(s.rid.clone());
            }
        }
        if self.mode == ThreadsMode::Off {
            return;
        }
        for (_, s) in listed(ws) {
            // Not listed before (restored from the offline cache after live
            // updates were first missed): `confirm_restored` decides. Restored
            // rooms listed before are checked by both, which is harmless.
            if before.get(&s.rid).is_none_or(|lr| *lr == s.lr) {
                continue;
            }
            self.paging.remove(&s.rid);
            if shown_unread(s).any(|t| idx.parent(t).is_some()) {
                self.refresh_unread.insert(s.rid.clone());
            }
            if self.mode == ThreadsMode::Following && idx.followed(&s.rid).is_some() {
                self.recrawl.insert(s.rid.clone());
            }
        }
    }

    /// The session's `chat.getMessage` counter for (non-)pinned parents.
    fn budget(&mut self, pinned: bool) -> &mut u32 {
        if pinned {
            &mut self.pin_gets
        } else {
            &mut self.gets
        }
    }

    /// Records that `f` starts now.
    fn begin(&mut self, f: &Fetch, now: Instant) {
        let (pass_start, floor) = match f {
            Fetch::Unread { rid, offset } if *offset > 0 => self
                .paging
                .get(rid)
                .map_or((now, None), |p| (p.pass_start, p.floor)),
            _ => (now, None),
        };
        match f {
            Fetch::Parent { pinned, .. } => {
                self.last_get = Some(now);
                *self.budget(*pinned) += 1;
            }
            Fetch::Unread { .. } | Fetch::Followed { .. } => self.last_list = Some(now),
            Fetch::Starred { rid, lm, .. } => {
                self.last_starred = Some(now);
                self.starred_passes
                    .entry(rid.clone())
                    .or_insert_with(|| StarredPass {
                        offset: 0,
                        started: now,
                        lm: *lm,
                        found: Vec::new(),
                        pages: 0,
                    });
            }
        }
        self.in_flight = Some(InFlight {
            fetch: f.clone(),
            started: now,
            mode_gen: self.mode_gen,
            pass_start,
            floor,
        });
    }

    /// Per update: follows the session, plans and starts the next request or
    /// sets the pacing timer.
    pub fn tick(
        &mut self,
        ws: &Workspace,
        session: Option<&SessionHandle>,
        mode: ThreadsMode,
        cap: Cap,
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
        if !self.needs_plan(ws, ptr, mode, cap, threads_collapsed, now) {
            return Task::none();
        }
        self.observe(ws, mode, now);
        let next = plan(ws, mode, cap, threads_collapsed, self, now);
        self.progress = self.crawl_progress(ws, mode, cap);
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
        cap: Cap,
        collapsed: bool,
        now: Instant,
    ) -> bool {
        let key = TickKey::new(ws, session, mode, cap, collapsed);
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
            | Done::Starred { res: Err(f), .. }
            | Done::Parent { res: Err(f), .. } => Some(f.clone()),
            _ => None,
        };
        let starred = matches!(fl.fetch, Fetch::Starred { .. });
        // Starred scans do not depend on the mode.
        if fl.mode_gen != self.mode_gen && !starred {
            // Asked for under another mode (e.g. before Off, whose state was
            // dropped): only a rate limit, which is per IP, still counts.
            if let Some(Class::RateLimited(wait)) = failure.as_ref().map(classify) {
                self.paused_until = Some(now + wait);
                if let Fetch::Parent { pinned, .. } = fl.fetch {
                    let n = self.budget(pinned);
                    *n = n.saturating_sub(1);
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
                // Limits are per route: a starred scan pauses only scans.
                if starred {
                    self.starred_paused_until = Some(now + wait);
                } else {
                    self.paused_until = Some(now + wait);
                }
                // Refused before it ran: not a spent call.
                if let Fetch::Parent { pinned, .. } = fl.fetch {
                    let n = self.budget(pinned);
                    *n = n.saturating_sub(1);
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
        if let Done::Starred { rid, offset, res } = result {
            self.starred_done(ws, rid, offset, fl.started, res);
            return Vec::new();
        }
        let other = failure
            .as_ref()
            .is_some_and(|f| classify(f) == Class::Other);
        let mut out = Vec::new();
        match result {
            Done::Unread { rid, offset, res } => {
                let floor;
                let more = match res {
                    Ok(page) => {
                        let more = page.has_more()
                            && page.raw_len > 0
                            && offset / UNREAD_PAGE + 1 < MAX_UNREAD_PAGES;
                        let next = offset + page.raw_len as u64;
                        floor = page
                            .items
                            .iter()
                            .filter_map(rustket_core::thread_index::activity)
                            .min();
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
                        self.paging.remove(&rid);
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
                        let floor = match (floor, fl.floor) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        }
                        .or_else(|| ws.subscription(&rid).and_then(|s| s.lr));
                        self.paging.insert(
                            rid,
                            Paging {
                                offset,
                                pass_start: fl.pass_start,
                                floor,
                            },
                        );
                    }
                    _ => {
                        self.paging.remove(&rid);
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
                    Ok(page) => {
                        let count = match fl.fetch {
                            Fetch::Followed { count, .. } => count,
                            _ => FOLLOWED_PER_ROOM,
                        };
                        let short = page.has_more().then_some(count);
                        ws.apply_followed_page(&rid, page.items, activity, fl.started, short);
                    }
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
            // Handled above.
            Done::Starred { .. } => {}
        }
        out
    }

    /// A page of a starred scan (not one to retry): the scan goes on, ends
    /// (the room's starred threads are replaced) or, failed, is given up
    /// until missed live updates or a refresh.
    fn starred_done(
        &mut self,
        ws: &mut Workspace,
        rid: String,
        offset: u64,
        started: Instant,
        res: Res<Paginated<Message>>,
    ) {
        let page = match res {
            Ok(page) => page,
            Err(_) => {
                self.starred_passes.remove(&rid);
                self.starred_failed.insert(rid);
                return;
            }
        };
        // Dropped meanwhile (room left, pins off, missed live updates): the
        // copies still count, not as a scan.
        let pass = self
            .starred_passes
            .remove(&rid)
            .filter(|p| p.offset == offset);
        let started = pass.as_ref().map_or(started, |p| p.started);
        let found = ws.apply_starred_page(&rid, page.items, started);
        let Some(mut pass) = pass else {
            return;
        };
        pass.found.extend(found);
        pass.pages += 1;
        let next = offset + page.raw_len as u64;
        if page.raw_len > 0 && next < page.total {
            pass.offset = next;
            self.starred_passes.insert(rid, pass);
            return;
        }
        ws.finish_starred_scan(&rid, pass.found, pass.started, pass.lm);
        self.starred_due.remove(&rid);
        self.starred_checked.insert(rid);
    }

    /// A retried unread page continues the pass it belongs to.
    fn keep_paging(&mut self, result: &Done, fl: &InFlight) {
        if let Done::Unread { rid, offset, .. } = result
            && *offset > 0
        {
            self.paging.insert(
                rid.clone(),
                Paging {
                    offset: *offset,
                    pass_start: fl.pass_start,
                    floor: fl.floor,
                },
            );
        }
    }

    /// Following crawl progress `(done, rooms)` while it runs (not while
    /// disconnected: nothing loads then). With a row limit, rooms that
    /// cannot reach the rows within it are not counted.
    pub fn crawl_progress(
        &self,
        ws: &Workspace,
        mode: ThreadsMode,
        cap: Cap,
    ) -> Option<(usize, usize)> {
        if mode != ThreadsMode::Following || *ws.connection() != ConnectionState::Connected {
            return None;
        }
        let bound = Bound::new(ws, mode, cap);
        let page = cap.followed_page();
        let (mut done, mut total) = (0, 0);
        for (_, s) in listed(ws) {
            if !wants_crawl(ws, self, &s.rid, page) || self.crawl_failed.contains(&s.rid) {
                done += 1;
                total += 1;
            } else if bound.reaches(&s.rid, bound.crawl_lr(s), false) {
                total += 1;
            }
        }
        (done < total).then_some((done, total))
    }

    /// Scans every scanned room for starred threads again (Favorites'
    /// "Refresh"); scans under way finish as they are.
    pub fn refresh_starred(&mut self, ws: &Workspace) {
        self.changed = true;
        self.starred_failed.clear();
        self.backoff.retain(|k, _| !matches!(k, Key::Starred(_)));
        for (rid, _) in ws.thread_index().starred_scans() {
            if !self.starred_passes.contains_key(rid) {
                self.starred_due.insert(rid.to_owned());
            }
        }
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
        self.paging.clear();
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
/// when that started. Starred threads are kept per room with the room's
/// scan, whose `lm` advances the same way once checked and not due.
pub fn cached_threads(ws: &Workspace, st: &SidebarThreads) -> CachedThreads {
    let idx = ws.thread_index();
    if !idx.indexing() {
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
            short: f.short,
        });
    }
    wanted.extend(idx.kept().map(|(_, t)| t));
    wanted.extend(idx.pinned().map(|(_, t)| t));
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
    // Up to which `lm` a scanned room's stars are current.
    let scan_upto = |rid: &str, e: &RoomEntry| -> Option<RcDate> {
        if !st.starred_checked.contains(rid)
            || st.starred_due.contains(rid)
            || st.starred_passes.contains_key(rid)
        {
            return None;
        }
        if live {
            room_lm(e)
        } else {
            st.lm_before.as_ref()?.get(rid).copied().flatten()
        }
    };
    let mut starred: BTreeMap<&str, CachedStarred> = BTreeMap::new();
    fn entry<'a, 'k>(
        m: &'a mut BTreeMap<&'k str, CachedStarred>,
        rid: &'k str,
    ) -> &'a mut CachedStarred {
        m.entry(rid).or_insert_with(|| CachedStarred {
            rid: rid.to_owned(),
            ..Default::default()
        })
    }
    for (rid, tmid) in idx.starred() {
        if ws.subscription(rid).is_some() {
            entry(&mut starred, rid).tmids.push(tmid.to_owned());
        }
    }
    for (rid, scan) in idx.starred_scans() {
        if let Some(e) = ws.room(rid).filter(|e| e.sub.is_some()) {
            let c = entry(&mut starred, rid);
            c.scanned_at = scan.scanned_at;
            c.scanned_lm = scan.lm.max(scan_upto(rid, e));
        }
    }
    CachedThreads {
        followed,
        parents,
        parents_synced,
        starred: starred.into_values().collect(),
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
        Fetch::Parent { rid, tmid, .. } => Task::perform(
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
        Fetch::Followed {
            rid,
            activity,
            count,
        } => Task::perform(
            {
                let rid = rid.clone();
                async move {
                    rest.threads_list(&rid, Some("following"), None, 0, count as u64)
                        .await
                        .map_err(Failure::from)
                }
            },
            move |res| done(Done::Followed { rid, activity, res }),
        ),
        Fetch::Starred { rid, offset, .. } => Task::perform(
            {
                let rid = rid.clone();
                async move {
                    rest.get_starred(&rid, offset, STARRED_PAGE)
                        .await
                        .map_err(Failure::from)
                }
            },
            move |res| done(Done::Starred { rid, offset, res }),
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
            pinned: false,
        }
    }

    /// A parent only starred or flagged.
    fn get_pin(rid: &str, tmid: &str) -> Fetch {
        Fetch::Parent {
            rid: rid.into(),
            tmid: tmid.into(),
            pinned: true,
        }
    }

    fn followed(rid: &str, lr: Option<i64>) -> Fetch {
        followed_n(rid, lr, FOLLOWED_PER_ROOM)
    }

    fn followed_n(rid: &str, lr: Option<i64>, count: usize) -> Fetch {
        Fetch::Followed {
            rid: rid.into(),
            activity: lr.map(ts),
            count,
        }
    }

    /// The loader around a workspace, driven like `tick` without a session.
    struct Fx {
        ws: Workspace,
        st: SidebarThreads,
        mode: ThreadsMode,
        cap: Cap,
        collapsed: bool,
        t0: Instant,
        /// Starred threads on (also after `restart`).
        pins: bool,
    }

    impl Fx {
        fn new(mode: ThreadsMode) -> Self {
            Self {
                ws: ws(mode),
                st: SidebarThreads::default(),
                mode,
                cap: Cap::default(),
                collapsed: false,
                t0: Instant::now(),
                pins: false,
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn plan(&mut self, ms: u64) -> Next {
            let now = self.at(ms);
            self.st.observe(&self.ws, self.mode, now);
            plan(&self.ws, self.mode, self.cap, self.collapsed, &self.st, now)
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
    fn flagged_parents_load_with_threads_off() {
        use rustket_core::flags::{FlagBlob, FlagColor, FlagEdit};
        let mut fx = Fx::new(ThreadsMode::Off);
        add(&mut fx.ws, "a", Some(5), &[]);
        assert_eq!(fx.plan(0), Next::Idle);
        let mut overlay = FlagBlob::default();
        let edit = FlagEdit::Thread {
            rid: "a".into(),
            tmid: "f1".into(),
            color: Some(FlagColor::Red),
        };
        overlay.edit(&FlagBlob::default(), &edit, 1, "dev").unwrap();
        fx.ws.set_flag_overlay(overlay);
        assert_eq!(fx.start(1000), Next::Fetch(get_pin("a", "f1")));
        fx.done(
            1100,
            Done::Parent {
                rid: "a".into(),
                tmid: "f1".into(),
                res: Ok(Box::new(parent("f1", "a", 5))),
            },
        );
        assert!(fx.ws.thread_index().parent("f1").is_some());
        assert_eq!(fx.plan(60_000), Next::Idle);
        let cached = cached_threads(&fx.ws, &fx.st);
        assert!(cached.parents.iter().any(|p| p.id == "f1"));
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

    fn capped(limit: u32, sort: SidebarSort) -> Cap {
        Cap {
            limit,
            sort,
            ..Cap::default()
        }
    }

    fn unread_done(fx: &mut Fx, ms: u64, rid: &str, items: Vec<Message>) {
        let n = items.len() as u64;
        fx.done(
            ms,
            Done::Unread {
                rid: rid.into(),
                offset: 0,
                res: page(items, n),
            },
        );
    }

    #[test]
    fn unread_parents_load_only_up_to_the_limit() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "c", Some(100), &["c1"]);
        add(&mut fx.ws, "a", Some(300), &["a1"]);
        add(&mut fx.ws, "b", Some(200), &["b1"]);
        // Rooms go newest `lr` first.
        assert_eq!(fx.plan(0), Next::Wait(fx.at(1000)));
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        unread_done(&mut fx, 1100, "a", vec![parent("a1", "a", 290)]);
        assert_eq!(fx.start(3100), Next::Fetch(unread("b", 0)));
        unread_done(&mut fx, 3200, "b", vec![parent("b1", "b", 190)]);
        // Two loaded rows newer than c's `lr`: c1 cannot make it.
        assert_eq!(fx.plan(10_000), Next::Idle);
        // Lowering loads nothing; raising resumes.
        fx.cap.limit = 1;
        assert_eq!(fx.plan(11_000), Next::Idle);
        fx.cap.limit = 3;
        assert_eq!(fx.start(12_000), Next::Fetch(unread("c", 0)));
        unread_done(&mut fx, 12_100, "c", Vec::new());
        // Not in the list: fetched singly, within the limit too.
        assert_eq!(fx.start(14_000), Next::Fetch(get("c", "c1")));
    }

    #[test]
    fn single_unread_parents_respect_the_limit() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.cap = capped(1, SidebarSort::Activity);
        add(&mut fx.ws, "a", Some(300), &["a1", "a2"]);
        add(&mut fx.ws, "b", Some(100), &["b1"]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        unread_done(&mut fx, 1100, "a", vec![parent("a1", "a", 290)]);
        // a2 is unresolved but its room can still reach the first row.
        assert_eq!(fx.start(1200), Next::Fetch(get("a", "a2")));
        fx.done(
            1300,
            Done::Parent {
                rid: "a".into(),
                tmid: "a2".into(),
                res: Ok(Box::new(parent("a2", "a", 280))),
            },
        );
        assert_eq!(fx.plan(60_000), Next::Idle);
    }

    #[test]
    fn oldest_first_stops_once_the_limit_is_loaded() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.cap = capped(1, SidebarSort::ActivityAsc);
        add(&mut fx.ws, "a", Some(300), &["a1"]);
        add(&mut fx.ws, "b", Some(200), &["b1"]);
        fx.plan(0);
        // No bound from below: oldest `lr` first, then stop.
        assert_eq!(fx.start(1000), Next::Fetch(unread("b", 0)));
        unread_done(&mut fx, 1100, "b", vec![parent("b1", "b", 150)]);
        assert_eq!(fx.plan(10_000), Next::Idle);
        fx.cap.limit = 2;
        assert_eq!(fx.start(11_000), Next::Fetch(unread("a", 0)));
    }

    #[test]
    fn by_name_loads_rooms_in_name_order() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.cap = capped(1, SidebarSort::Alphabetical);
        add(&mut fx.ws, "zz", Some(300), &["z1"]);
        add(&mut fx.ws, "aa", Some(100), &["a1"]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("aa", 0)));
        unread_done(&mut fx, 1100, "aa", vec![parent("a1", "aa", 90)]);
        assert_eq!(fx.plan(10_000), Next::Idle);
    }

    #[test]
    fn an_unread_pass_waits_past_the_limit() {
        let mut fx = Fx::new(ThreadsMode::Unread);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "b", Some(500), &["b1"]);
        add(&mut fx.ws, "a", Some(360), &["x", "w", "y"]);
        fx.ws.apply_thread_parents(vec![parent("b1", "b", 400)]);
        fx.plan(0);
        assert_eq!(fx.start(1000), Next::Fetch(unread("a", 0)));
        let mut first = page(vec![parent("x", "a", 350), parent("w", "a", 340)], 150).unwrap();
        first.raw_len = 100;
        fx.done(
            1100,
            Done::Unread {
                rid: "a".into(),
                offset: 0,
                res: Ok(first),
            },
        );
        // b1 and x are newer than anything left in the list: y waits (and
        // is not fetched singly).
        assert_eq!(fx.plan(10_000), Next::Idle);
        fx.cap.limit = 3;
        assert_eq!(fx.start(11_000), Next::Fetch(unread("a", 100)));
    }

    #[test]
    fn following_crawl_stops_at_the_limit() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "quiet", None, &[]);
        add(&mut fx.ws, "f3", Some(100), &[]);
        add(&mut fx.ws, "f1", Some(300), &[]);
        add(&mut fx.ws, "f2", Some(200), &[]);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((0, 4)));
        assert_eq!(fx.start(0), Next::Fetch(followed_n("f1", Some(300), 2)));
        fx.done(
            100,
            Done::Followed {
                rid: "f1".into(),
                activity: Some(ts(300)),
                res: page(vec![parent("p1", "f1", 300), parent("p2", "f1", 250)], 5),
            },
        );
        // Two followed threads newer than every other room's `lr`.
        assert_eq!(fx.plan(10_000), Next::Idle);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
        // A bigger limit crawls f1 again (it had more) with the bigger page.
        fx.cap.limit = 3;
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((0, 4)));
        assert_eq!(
            fx.start(11_000),
            Next::Fetch(followed_n("f1", Some(300), 3))
        );
        fx.done(
            11_100,
            Done::Followed {
                rid: "f1".into(),
                activity: Some(ts(300)),
                res: page(
                    vec![
                        parent("p1", "f1", 300),
                        parent("p2", "f1", 250),
                        parent("p3", "f1", 120),
                    ],
                    5,
                ),
            },
        );
        assert_eq!(
            fx.start(13_100),
            Next::Fetch(followed_n("f2", Some(200), 3))
        );
        fx.done(
            13_200,
            Done::Followed {
                rid: "f2".into(),
                activity: Some(ts(200)),
                res: page(vec![parent("q1", "f2", 210)], 1),
            },
        );
        // p1, p2, q1 are newer than f3's `lr`; the room without `lr` is
        // skipped too.
        assert_eq!(fx.plan(60_000), Next::Idle);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
        // No limit: f1 (it had more) gets a full page, then the rest.
        fx.cap.limit = 0;
        assert_eq!(fx.start(61_000), Next::Fetch(followed("f1", Some(300))));
        fx.done(
            61_100,
            Done::Followed {
                rid: "f1".into(),
                activity: Some(ts(300)),
                res: page(vec![parent("p1", "f1", 300)], 1),
            },
        );
        assert_eq!(fx.start(64_000), Next::Fetch(followed("f3", Some(100))));
    }

    #[test]
    fn unread_rows_filling_the_limit_skip_the_crawl() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "u", Some(50), &["u1", "u2"]);
        add(&mut fx.ws, "f", Some(900), &[]);
        fx.ws
            .apply_thread_parents(vec![parent("u1", "u", 40), parent("u2", "u", 30)]);
        // Unread first: both unread rows come before any followed one.
        assert_eq!(fx.plan(0), Next::Idle);
        fx.cap.unread_first = false;
        assert_eq!(fx.plan(1), Next::Fetch(followed_n("f", Some(900), 2)));
    }

    fn followed_done(
        fx: &mut Fx,
        ms: u64,
        rid: &str,
        lr: Option<i64>,
        items: Vec<Message>,
        total: u64,
    ) {
        fx.done(
            ms,
            Done::Followed {
                rid: rid.into(),
                activity: lr.map(ts),
                res: page(items, total),
            },
        );
    }

    #[test]
    fn a_follow_without_a_reply_gets_an_uncrawled_room_crawled() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "f1", Some(300), &[]);
        add(&mut fx.ws, "r", None, &[]);
        assert_eq!(fx.start(0), Next::Fetch(followed_n("f1", Some(300), 2)));
        let items = vec![parent("p1", "f1", 300), parent("p2", "f1", 250)];
        followed_done(&mut fx, 100, "f1", Some(300), items, 2);
        assert_eq!(fx.plan(10_000), Next::Idle);
        // "Follow" on an active thread: the parent update adds the user to
        // `replies`, `lr` stays unset.
        fx.ws.apply_message(parent("t", "r", 500));
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((1, 2)));
        assert_eq!(fx.start(20_000), Next::Fetch(followed_n("r", None, 2)));
    }

    #[test]
    fn a_short_crawl_runs_again_when_its_set_shrinks() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "r", Some(300), &[]);
        add(&mut fx.ws, "q", Some(50), &[]);
        assert_eq!(fx.start(0), Next::Fetch(followed_n("r", Some(300), 2)));
        let items = vec![parent("r1", "r", 300), parent("r2", "r", 290)];
        followed_done(&mut fx, 100, "r", Some(300), items, 5);
        assert_eq!(fx.plan(10_000), Next::Idle);
        // Unfollowed: r's third thread now belongs within the limit.
        let mut r1 = parent("r1", "r", 300);
        r1.replies = Some(Vec::new());
        r1.updated_at = Some(ts(400));
        fx.ws.apply_message(r1);
        assert!(!fx.ws.thread_index().is_followed("r1"));
        assert_eq!(fx.start(20_000), Next::Fetch(followed_n("r", Some(300), 2)));
        let items = vec![parent("r2", "r", 290), parent("r3", "r", 280)];
        followed_done(&mut fx, 20_100, "r", Some(300), items, 4);
        assert_eq!(fx.plan(30_000), Next::Idle);
    }

    #[test]
    fn a_short_crawl_that_dropped_threads_asks_for_more() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "r", Some(300), &[]);
        add(&mut fx.ws, "q", Some(50), &[]);
        assert_eq!(fx.start(0), Next::Fetch(followed_n("r", Some(300), 2)));
        let mut deleted = parent("r1", "r", 300);
        deleted.t = Some("rm".into());
        let items = vec![deleted.clone(), parent("r2", "r", 290)];
        followed_done(&mut fx, 100, "r", Some(300), items, 5);
        // The deleted parent is not a row: one more makes up for it.
        assert_eq!(fx.start(10_000), Next::Fetch(followed_n("r", Some(300), 3)));
        let items = vec![deleted, parent("r2", "r", 290), parent("r3", "r", 280)];
        followed_done(&mut fx, 10_100, "r", Some(300), items, 5);
        assert_eq!(fx.plan(20_000), Next::Idle);
        // Lowering the limit does not crawl again.
        fx.cap.limit = 1;
        assert_eq!(fx.plan(30_000), Next::Idle);
    }

    #[test]
    fn short_crawls_survive_a_restart() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(2, SidebarSort::Activity);
        add(&mut fx.ws, "r", Some(300), &[]);
        assert_eq!(fx.start(0), Next::Fetch(followed_n("r", Some(300), 2)));
        let items = vec![parent("r1", "r", 300), parent("r2", "r", 290)];
        followed_done(&mut fx, 100, "r", Some(300), items, 12);
        assert_eq!(fx.plan(10_000), Next::Idle);
        let cached = cached_threads(&fx.ws, &fx.st);
        assert_eq!(
            cached.followed[0].short,
            Some(ShortCrawl { count: 2, kept: 2 })
        );
        let subs = vec![sub("r", Some(300), &[])];
        restart(&mut fx, subs.clone(), cached, subs);
        // Quiet room: current after the restart, nothing to load.
        assert!(!fx.ws.thread_index().followed("r").unwrap().stale);
        assert_eq!(fx.plan(20_000), Next::Idle);
        // A bigger limit crawls it again with the bigger page.
        fx.cap.limit = 5;
        assert_eq!(fx.start(30_000), Next::Fetch(followed_n("r", Some(300), 5)));
    }

    #[test]
    fn kept_parents_do_not_wait_for_skipped_crawls() {
        let mut fx = Fx::new(ThreadsMode::Following);
        fx.cap = capped(1, SidebarSort::Activity);
        add(&mut fx.ws, "u", Some(500), &["u1"]);
        add(&mut fx.ws, "k", Some(10), &[]);
        fx.ws
            .set_thread_config(ThreadsMode::Following, [("k", "kept")]);
        fx.ws.apply_thread_parents(vec![parent("u1", "u", 400)]);
        // k's crawl cannot reach the row: the kept parent is fetched singly.
        assert_eq!(fx.plan(0), Next::Fetch(get("k", "kept")));
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
        // Pinned-only parents have a budget of their own, both ways.
        use rustket_core::flags::{FlagBlob, FlagColor, FlagEdit};
        let mut overlay = FlagBlob::default();
        let edit = FlagEdit::Thread {
            rid: "a".into(),
            tmid: "f".into(),
            color: Some(FlagColor::Red),
        };
        overlay.edit(&FlagBlob::default(), &edit, 1, "dev").unwrap();
        fx.ws.set_flag_overlay(overlay);
        fx.st.pin_gets = PIN_GET_BUDGET;
        assert_eq!(fx.plan(0), Next::Fetch(get("a", "k")));
        fx.st.pin_gets = 0;
        fx.st.gets = GET_BUDGET;
        assert_eq!(fx.plan(0), Next::Fetch(get_pin("a", "f")));
        fx.st.begin(&get_pin("a", "f"), fx.at(0));
        assert_eq!((fx.st.gets, fx.st.pin_gets), (GET_BUDGET, 1));
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((2, 4)));
        fx.ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
        fx.ws.apply_connection(ConnectionState::Connected);
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((2, 4)));
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
        fx.st.refresh(&fx.ws);
        // The followed sets stay until the crawl replaces them.
        assert!(fx.ws.thread_index().followed("a").is_some());
        assert!(fx.ws.thread_index().followed("b").is_some());
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((0, 2)));
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((1, 2)));
        assert_eq!(fx.start(4300), Next::Fetch(followed("a", Some(1003))));
        fx.done(
            4400,
            Done::Followed {
                rid: "a".into(),
                activity: Some(ts(1003)),
                res: page(vec![parent("t", "a", 3000)], 1),
            },
        );
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
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
        assert!(fx.st.unresolved.is_empty() && fx.st.paging.is_empty());
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
        assert!(fx.st.needs_plan(&fx.ws, 1, mode, fx.cap, false, t0));
        assert!(!fx.st.needs_plan(&fx.ws, 1, mode, fx.cap, false, fx.at(10)));
        assert!(fx.st.needs_plan(&fx.ws, 1, mode, fx.cap, true, fx.at(20)));
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(20)));
        add(&mut fx.ws, "b", Some(10), &["t"]);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(30)));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(40)));
        fx.st.due = Some(fx.at(1000));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(999)));
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(1000)));
        fx.st.due = None;
        let at = fx.at(1000);
        let _ = fx.st.update(Msg::Pace { at }, &mut fx.ws, at);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(1001)));
        fx.st.refresh(&fx.ws);
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(1002)));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, fx.cap, true, fx.at(1003)));
        // A new limit or sort plans again.
        let cap = Cap { limit: 5, ..fx.cap };
        assert!(fx.st.needs_plan(&fx.ws, 2, mode, cap, true, fx.at(1004)));
        assert!(!fx.st.needs_plan(&fx.ws, 2, mode, cap, true, fx.at(1005)));
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
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
        fx.ws.set_starred_threads(fx.pins);
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
            short: None,
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
            starred: Vec::new(),
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), Some((2, 3)));
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
        assert_eq!(fx.st.crawl_progress(&fx.ws, fx.mode, fx.cap), None);
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
            starred: Vec::new(),
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
            starred: Vec::new(),
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

    // --- Starred threads ---------------------------------------------------------

    fn star(mut m: Message) -> Message {
        m.starred = Some(vec![rocketchat::models::IdRef { id: "me".into() }]);
        m
    }

    fn scan(rid: &str, offset: u64, lm: Option<i64>) -> Fetch {
        Fetch::Starred {
            rid: rid.into(),
            offset,
            lm: lm.map(ts),
        }
    }

    fn set_lm(ws: &mut Workspace, rid: &str, lm: i64) {
        ws.apply_room(
            Change::Updated,
            rocketchat::models::Room {
                id: rid.into(),
                t: RoomType::Channel,
                lm: Some(ts(lm)),
                ..Default::default()
            },
        );
    }

    /// A subscribed room whose last message is at `lm`.
    fn add_lm(ws: &mut Workspace, rid: &str, lm: i64) {
        add(ws, rid, None, &[]);
        set_lm(ws, rid, lm);
    }

    fn pinned(mode: ThreadsMode) -> Fx {
        let mut fx = Fx::new(mode);
        fx.pins = true;
        fx.ws.set_starred_threads(true);
        fx
    }

    fn starred_page(
        items: Vec<Message>,
        offset: u64,
        raw_len: usize,
        total: u64,
    ) -> Res<Paginated<Message>> {
        Ok(Paginated {
            count: raw_len as u64,
            items,
            offset,
            total,
            raw_len,
            dropped: 0,
        })
    }

    fn scan_done(fx: &mut Fx, ms: u64, rid: &str, offset: u64, res: Res<Paginated<Message>>) {
        fx.done(
            ms,
            Done::Starred {
                rid: rid.into(),
                offset,
                res,
            },
        );
    }

    fn starred_tmids(fx: &Fx) -> Vec<&str> {
        fx.ws.thread_index().starred().map(|(_, t)| t).collect()
    }

    #[test]
    fn starred_scans_run_with_threads_off_newest_rooms_first_and_paced() {
        let mut fx = pinned(ThreadsMode::Off);
        add_lm(&mut fx.ws, "a", 100);
        add_lm(&mut fx.ws, "b", 200);
        let mut omni = sub("l", None, &[]);
        omni.t = RoomType::Livechat;
        fx.ws.apply_subscription(Change::Inserted, omni);
        assert_eq!(fx.start(0), Next::Fetch(scan("b", 0, Some(200))));
        assert_eq!(fx.plan(10), Next::Idle, "one request at a time");
        let items = vec![
            star(parent("s1", "b", 150)),
            // Starred, not a thread: not listed.
            star(Message {
                tcount: None,
                ..parent("m", "b", 1)
            }),
        ];
        scan_done(&mut fx, 100, "b", 0, starred_page(items, 0, 2, 2));
        assert_eq!(starred_tmids(&fx), ["s1"]);
        assert!(fx.ws.thread_index().parent("s1").is_some());
        assert_eq!(
            fx.ws.thread_index().starred_scan("b").unwrap().lm,
            Some(ts(200))
        );
        assert_eq!(fx.plan(1000), Next::Wait(fx.at(10_000)));
        assert_eq!(fx.start(10_000), Next::Fetch(scan("a", 0, Some(100))));
        scan_done(&mut fx, 10_100, "a", 0, starred_page(Vec::new(), 0, 0, 0));
        // Omnichannel rooms are not scanned; live updates keep the rest current.
        assert_eq!(fx.plan(30_000), Next::Idle);
        set_lm(&mut fx.ws, "a", 300);
        assert_eq!(
            fx.plan(40_000),
            Next::Idle,
            "messages seen live need no scan"
        );
        // Pins off: nothing (the Threads section is off too).
        fx.ws.set_starred_threads(false);
        add_lm(&mut fx.ws, "c", 50);
        assert_eq!(fx.plan(50_000), Next::Idle);
    }

    #[test]
    fn starred_scans_page_and_let_other_rooms_go_in_between() {
        let mut fx = pinned(ThreadsMode::Off);
        add_lm(&mut fx.ws, "big", 200);
        add_lm(&mut fx.ws, "small", 100);
        let mut ms = 0;
        let total = (STARRED_PAGES_PER_TURN as u64 + 2) * STARRED_PAGE;
        for i in 0..STARRED_PAGES_PER_TURN as u64 {
            let offset = i * STARRED_PAGE;
            assert_eq!(
                fx.start(ms),
                Next::Fetch(scan("big", offset, Some(200))),
                "page {i}"
            );
            let items = vec![star(parent(&format!("p{i}"), "big", 10))];
            scan_done(
                &mut fx,
                ms + 100,
                "big",
                offset,
                starred_page(items, offset, 100, total),
            );
            ms += STARRED_INTERVAL.as_millis() as u64;
        }
        // Not done yet: the stars found so far count, nothing is dropped.
        assert_eq!(starred_tmids(&fx).len(), STARRED_PAGES_PER_TURN as usize);
        assert!(fx.ws.thread_index().starred_scan("big").is_none());
        // Its turn is over: the other room first.
        assert_eq!(fx.start(ms), Next::Fetch(scan("small", 0, Some(100))));
        scan_done(
            &mut fx,
            ms + 100,
            "small",
            0,
            starred_page(Vec::new(), 0, 0, 0),
        );
        ms += STARRED_INTERVAL.as_millis() as u64;
        let mut offset = STARRED_PAGES_PER_TURN as u64 * STARRED_PAGE;
        // A transient failure is retried at its offset.
        assert_eq!(fx.start(ms), Next::Fetch(scan("big", offset, Some(200))));
        scan_done(
            &mut fx,
            ms + 100,
            "big",
            offset,
            Err(fail(Error::Timeout("t"))),
        );
        ms += 10_000;
        assert_eq!(fx.start(ms), Next::Fetch(scan("big", offset, Some(200))));
        // Unstarred elsewhere meanwhile: p0 is not on the last pages.
        scan_done(
            &mut fx,
            ms + 100,
            "big",
            offset,
            starred_page(Vec::new(), offset, 100, total),
        );
        ms += STARRED_INTERVAL.as_millis() as u64;
        offset += STARRED_PAGE;
        assert_eq!(fx.start(ms), Next::Fetch(scan("big", offset, Some(200))));
        let last = vec![star(parent("last", "big", 10))];
        scan_done(
            &mut fx,
            ms + 100,
            "big",
            offset,
            starred_page(last, offset, 100, total),
        );
        let mut got = starred_tmids(&fx);
        got.sort();
        assert_eq!(got, ["last", "p0", "p1", "p2", "p3", "p4"]);
        assert!(fx.ws.thread_index().starred_scan("big").is_some());
        assert_eq!(fx.plan(ms + 20_000), Next::Idle);
    }

    #[test]
    fn a_scan_keeps_stars_changed_while_it_ran() {
        let mut fx = pinned(ThreadsMode::Off);
        add_lm(&mut fx.ws, "r", 100);
        fx.ws
            .apply_thread_parents(vec![star(parent("old", "r", 10))]);
        assert_eq!(starred_tmids(&fx), ["old"]);
        assert_eq!(fx.start(0), Next::Fetch(scan("r", 0, Some(100))));
        // Starred and unstarred while the request runs.
        fx.ws.set_thread_starred("r", "new", true);
        fx.ws.set_thread_starred("r", "old", false);
        let items = vec![star(parent("old", "r", 10))];
        scan_done(&mut fx, 100, "r", 0, starred_page(items, 0, 1, 1));
        assert_eq!(starred_tmids(&fx), ["new"]);
        // "new" has no parent: fetched singly (before scans) now that the
        // room's scan ran.
        add_lm(&mut fx.ws, "x", 50);
        assert_eq!(fx.start(10_000), Next::Fetch(get_pin("r", "new")));
        fx.done(
            10_100,
            Done::Parent {
                rid: "r".into(),
                tmid: "new".into(),
                res: Err(not_found()),
            },
        );
        assert!(
            starred_tmids(&fx).is_empty(),
            "not found: no longer starred"
        );
        // A scan that is not allowed changes nothing and is not retried.
        fx.ws.set_thread_starred("r", "kept", true);
        assert_eq!(fx.start(10_200), Next::Fetch(scan("x", 0, Some(50))));
        scan_done(&mut fx, 10_300, "x", 0, Err(not_allowed()));
        assert_eq!(starred_tmids(&fx), ["kept"]);
        // ("kept" has no parent and is asked for.)
        assert_eq!(fx.start(40_000), Next::Fetch(get_pin("r", "kept")));
        fx.done(
            40_100,
            Done::Parent {
                rid: "r".into(),
                tmid: "kept".into(),
                res: Err(not_allowed()),
            },
        );
        assert_eq!(fx.plan(80_000), Next::Idle);
    }

    #[test]
    fn rate_limited_scans_do_not_delay_other_loads() {
        let mut fx = pinned(ThreadsMode::Unread);
        add_lm(&mut fx.ws, "a", 100);
        assert_eq!(fx.start(0), Next::Fetch(scan("a", 0, Some(100))));
        scan_done(
            &mut fx,
            100,
            "a",
            0,
            Err(fail(Error::RateLimited {
                retry_after: Some(Duration::from_secs(60)),
                message: None,
            })),
        );
        assert_eq!(fx.plan(200), Next::Wait(fx.at(60_100)));
        // A new unread thread is loaded right away.
        let mut s = sub("a", Some(150), &["t"]);
        s.lr = Some(ts(150));
        fx.ws.apply_subscription(Change::Updated, s);
        assert_eq!(fx.plan(300), Next::Wait(fx.at(1300)));
        assert_eq!(fx.start(1300), Next::Fetch(unread("a", 0)));
        unread_done(&mut fx, 1400, "a", vec![parent("t", "a", 150)]);
        assert_eq!(fx.plan(1500), Next::Wait(fx.at(60_100)));
        assert_eq!(fx.start(60_100), Next::Fetch(scan("a", 0, Some(100))));
        // Other loads that are rate limited do not stop scans.
        scan_done(&mut fx, 60_200, "a", 0, starred_page(Vec::new(), 0, 0, 0));
        fx.st.paused_until = Some(fx.at(500_000));
        add_lm(&mut fx.ws, "b", 10);
        assert_eq!(fx.start(70_100), Next::Fetch(scan("b", 0, Some(10))));
    }

    #[test]
    fn starred_threads_take_no_threads_rows() {
        let mut fx = pinned(ThreadsMode::UnreadKept);
        fx.cap = Cap {
            limit: 1,
            sort: SidebarSort::Activity,
            unread_first: false,
            real_names: None,
        };
        add(&mut fx.ws, "k", Some(500), &[]);
        add(&mut fx.ws, "u", Some(100), &["u1"]);
        fx.ws
            .set_thread_config(ThreadsMode::UnreadKept, [("k", "k1")]);
        fx.ws
            .apply_thread_parents(vec![star(parent("k1", "k", 500))]);
        fx.plan(0);
        // The kept thread is starred (in Favorites): the unread one fits.
        assert_eq!(fx.start(1000), Next::Fetch(unread("u", 0)));
    }

    #[test]
    fn restored_scans_run_again_only_for_rooms_with_new_messages() {
        let mut fx = pinned(ThreadsMode::Off);
        let now = RcDate::now().timestamp_millis();
        let entry = |rid: &str, tmid: &str, at: i64, lm: i64| CachedStarred {
            rid: rid.into(),
            tmids: vec![tmid.into()],
            scanned_at: Some(ts(at)),
            scanned_lm: Some(ts(lm)),
        };
        let threads = CachedThreads {
            parents: vec![star(parent("n1", "new", 10)), star(parent("o1", "old", 10))],
            starred: vec![
                entry("quiet", "q1", now, 50),
                entry("new", "n1", now, 50),
                entry("old", "o1", now - 30 * 24 * 3600 * 1000, 50),
            ],
            ..Default::default()
        };
        let subs = vec![
            sub("quiet", None, &[]),
            sub("new", None, &[]),
            sub("old", None, &[]),
        ];
        restart(&mut fx, subs.clone(), threads, subs);
        set_lm(&mut fx.ws, "quiet", 50);
        set_lm(&mut fx.ws, "new", 80);
        set_lm(&mut fx.ws, "old", 40);
        let mut got = starred_tmids(&fx);
        got.sort();
        assert_eq!(got, ["n1", "o1", "q1"]);
        // The quiet room's restored star has no parent: fetched singly first.
        assert_eq!(fx.start(1000), Next::Fetch(get_pin("quiet", "q1")));
        fx.done(
            1100,
            Done::Parent {
                rid: "quiet".into(),
                tmid: "q1".into(),
                res: Ok(Box::new(star(parent("q1", "quiet", 10)))),
            },
        );
        // Rooms with messages since their scan, and scans past the TTL.
        assert_eq!(fx.start(1200), Next::Fetch(scan("new", 0, Some(80))));
        scan_done(&mut fx, 1300, "new", 0, starred_page(Vec::new(), 0, 0, 0));
        assert_eq!(fx.start(11_200), Next::Fetch(scan("old", 0, Some(40))));
        scan_done(&mut fx, 11_300, "old", 0, starred_page(Vec::new(), 0, 0, 0));
        assert_eq!(starred_tmids(&fx), ["q1"]);
        assert_eq!(fx.plan(30_000), Next::Idle);
    }

    #[test]
    fn missed_live_updates_rescan_rooms_with_new_messages() {
        let mut fx = pinned(ThreadsMode::Off);
        add_lm(&mut fx.ws, "a", 100);
        add_lm(&mut fx.ws, "b", 90);
        assert_eq!(fx.start(0), Next::Fetch(scan("a", 0, Some(100))));
        scan_done(&mut fx, 100, "a", 0, starred_page(Vec::new(), 0, 0, 0));
        // Disconnected while b's scan runs: it starts over.
        assert_eq!(fx.start(10_000), Next::Fetch(scan("b", 0, Some(90))));
        let page1 = vec![star(parent("b1", "b", 10))];
        scan_done(&mut fx, 10_100, "b", 0, starred_page(page1, 0, 100, 200));
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(11_000);
        fx.ws.apply_connection(ConnectionState::Connected);
        set_lm(&mut fx.ws, "a", 150);
        fx.delta();
        assert_eq!(fx.start(20_000), Next::Fetch(scan("a", 0, Some(150))));
        scan_done(&mut fx, 20_100, "a", 0, starred_page(Vec::new(), 0, 0, 0));
        assert_eq!(fx.start(30_000), Next::Fetch(scan("b", 0, Some(90))));
        scan_done(&mut fx, 30_100, "b", 0, starred_page(Vec::new(), 0, 0, 0));
        assert!(starred_tmids(&fx).is_empty());
        assert_eq!(fx.plan(40_000), Next::Idle);
        // "Refresh" scans every room again.
        fx.st.refresh_starred(&fx.ws);
        assert_eq!(fx.start(40_000), Next::Fetch(scan("a", 0, Some(150))));
    }

    #[test]
    fn cached_starred_threads_are_current_up_to_what_live_updates_covered() {
        let mut fx = pinned(ThreadsMode::Off);
        add_lm(&mut fx.ws, "a", 100);
        add_lm(&mut fx.ws, "b", 100);
        fx.ws.apply_initial(rustket_core::sync::InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![sub("a", None, &[]), sub("b", None, &[])],
            rooms: ["a", "b"]
                .map(|rid| rocketchat::models::Room {
                    id: rid.into(),
                    t: RoomType::Channel,
                    lm: Some(ts(100)),
                    ..Default::default()
                })
                .into(),
            next_since: Some(ts(1)),
            ..Default::default()
        });
        assert_eq!(fx.start(0), Next::Fetch(scan("a", 0, Some(100))));
        let items = vec![star(parent("a1", "a", 10))];
        scan_done(&mut fx, 100, "a", 0, starred_page(items, 0, 1, 1));
        set_lm(&mut fx.ws, "a", 150);
        set_lm(&mut fx.ws, "b", 120);
        let c = cached_threads(&fx.ws, &fx.st);
        assert_eq!(
            c.starred,
            [CachedStarred {
                rid: "a".into(),
                tmids: vec!["a1".into()],
                scanned_at: fx.ws.thread_index().starred_scan("a").unwrap().scanned_at,
                // Live since the scan: current now.
                scanned_lm: Some(ts(150)),
            }]
        );
        assert_eq!(
            c.parents.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a1"]
        );
        // While live updates are missed: as of when that started.
        fx.ws.apply_connection(ConnectionState::Disconnected);
        fx.plan(1000);
        set_lm(&mut fx.ws, "a", 200);
        assert_eq!(
            cached_threads(&fx.ws, &fx.st).starred[0].scanned_lm,
            Some(ts(150))
        );
        // With the Threads section off and pins off: nothing.
        fx.ws.set_starred_threads(false);
        assert!(cached_threads(&fx.ws, &fx.st).is_empty());
    }
}
