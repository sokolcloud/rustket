//! One search across the rooms of a server: which rooms, in what order,
//! the paced requests (pure; the app runs them) and the merged results.
//!
//! The server path is one server-wide request when the provider allows it
//! (`GlobalSearchEnabled`), else one request per room, most recently
//! active rooms first, at most [`MAX_IN_FLIGHT`] at a time and at least
//! the backend's interval apart (rate limits are per IP or connection and
//! shared with the user's other clients). A rate limit pauses the run; a
//! room that keeps failing is skipped and counted. End-to-end encrypted
//! rooms are not asked (the server holds ciphertext); their decrypted
//! messages come from the local search.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use rocketchat::models::Message;

use crate::inbox::load::{LoadFailure, MAX_PAUSE, RATE_LIMIT_PAUSE, retry_delay};
use crate::workspace::Workspace;

/// Requests of one run at once.
pub const MAX_IN_FLIGHT: usize = 2;
/// Results asked per room.
pub const PER_ROOM_LIMIT: u32 = 25;
/// Results asked of a server-wide search.
pub const GLOBAL_LIMIT: u32 = 100;
/// Unrecognised failures of one room before it is skipped.
const MAX_FAILURES: u32 = 3;

/// How the server is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `rocketchatSearch.search` of the default provider, server-wide
    /// (`GlobalSearchEnabled`): one request.
    Global,
    /// `rocketchatSearch.search` per room: a DDP method, 10 per 10 s per
    /// connection by default.
    Provider,
    /// `chat.search` per room (another provider is active): REST, 10 a
    /// minute by default.
    ChatSearch,
}

impl Backend {
    /// Least time between two request starts.
    pub fn interval(self) -> Duration {
        match self {
            Backend::Global | Backend::Provider => Duration::from_millis(1100),
            Backend::ChatSearch => Duration::from_millis(6500),
        }
    }
}

/// One request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Every room (server-wide search).
    All,
    Room(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Fetch(Target),
    Wait(Instant),
    /// Nothing to start now (requests in flight, or the run is over).
    Idle,
}

/// Where a hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Offline cache or loaded timelines (shown at once, may be dropped
    /// when the server's answer for the room arrives without them).
    Local,
    Server,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub message: Message,
    pub origin: Origin,
}

/// Rooms to search on the server, most recently active first: the user's
/// subscriptions (hidden and archived rooms too, the service room aside),
/// limited to `only` when set. Returns `(searched, encrypted)`: encrypted
/// rooms are left to the local search.
pub fn rooms_to_search(
    ws: &Workspace,
    only: Option<&HashSet<String>>,
) -> (Vec<String>, Vec<String>) {
    let mut rooms: Vec<(i64, &str, bool)> = ws
        .rooms()
        .values()
        .filter(|e| e.sub.is_some() && !e.is_service_room())
        .filter(|e| only.is_none_or(|o| o.contains(e.rid())))
        .map(|e| {
            let r = e.room.as_ref();
            let lm = r
                .and_then(|r| r.lm.or(r.last_message.as_ref().and_then(|m| m.ts)))
                .or(e.sub.as_ref().and_then(|s| s.updated_at))
                .map_or(0, |d| d.timestamp_millis());
            let encrypted = r.and_then(|r| r.encrypted) == Some(true);
            (lm, e.rid(), encrypted)
        })
        .collect();
    rooms.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let (enc, plain): (Vec<_>, Vec<_>) = rooms.into_iter().partition(|r| r.2);
    (
        plain.into_iter().map(|r| r.1.to_owned()).collect(),
        enc.into_iter().map(|r| r.1.to_owned()).collect(),
    )
}

/// Room ids whose name (or display name) is one of `names`
/// (case-insensitive), for `in:`.
pub fn resolve_rooms(ws: &Workspace, names: &[String]) -> HashSet<String> {
    let wanted: Vec<String> = names
        .iter()
        .map(|n| n.trim_start_matches(['#', '@']).to_lowercase())
        .collect();
    ws.rooms()
        .values()
        .filter(|e| e.sub.is_some())
        .filter(|e| {
            let name = e.name().to_lowercase();
            let fname = e.fname().map(str::to_lowercase);
            wanted
                .iter()
                .any(|w| *w == name || fname.as_deref() == Some(w.as_str()))
        })
        .map(|e| e.rid().to_owned())
        .collect()
}

/// A running (or finished) search.
#[derive(Debug)]
pub struct SearchRun {
    /// Tags requests, so answers of an earlier run are dropped.
    pub id: u64,
    /// What goes to the server.
    pub server_query: String,
    pub backend: Backend,
    queue: VecDeque<Target>,
    in_flight: HashSet<Target>,
    total: usize,
    done: usize,
    last_start: Option<Instant>,
    paused: Option<Instant>,
    backoff: HashMap<Target, (u32, Instant)>,
    /// Rooms given up (no access, repeated failures).
    pub failed: BTreeSet<String>,
    /// Encrypted rooms left to the local search.
    pub encrypted: usize,
    hits: HashMap<String, Hit>,
    /// Bumped when the hits change (views rebuild their groups).
    pub revision: u64,
}

impl SearchRun {
    /// A run over `rooms` (ignored by the server-wide backend unless
    /// `rooms_only`: an `in:` filter searches those rooms one by one).
    pub fn new(
        id: u64,
        server_query: String,
        backend: Backend,
        rooms: Vec<String>,
        encrypted: usize,
        rooms_only: bool,
    ) -> Self {
        let (backend, queue): (Backend, VecDeque<Target>) =
            if backend == Backend::Global && !rooms_only {
                (backend, VecDeque::from([Target::All]))
            } else {
                let b = if backend == Backend::Global {
                    Backend::Provider
                } else {
                    backend
                };
                (b, rooms.into_iter().map(Target::Room).collect())
            };
        Self {
            id,
            server_query,
            backend,
            total: queue.len(),
            queue,
            in_flight: HashSet::new(),
            done: 0,
            last_start: None,
            paused: None,
            backoff: HashMap::new(),
            failed: BTreeSet::new(),
            encrypted,
            hits: HashMap::new(),
            revision: 0,
        }
    }

    /// `(done, total)` requests.
    pub fn progress(&self) -> (usize, usize) {
        (self.done, self.total)
    }

    pub fn is_finished(&self) -> bool {
        self.queue.is_empty() && self.in_flight.is_empty()
    }

    /// Paused by a rate limit until then.
    pub fn paused_until(&self, now: Instant) -> Option<Instant> {
        self.paused.filter(|p| *p > now)
    }

    /// The next request to start, if any may start now.
    pub fn next(&mut self, now: Instant) -> Next {
        if self.queue.is_empty() || self.in_flight.len() >= MAX_IN_FLIGHT {
            return Next::Idle;
        }
        let mut at = self.last_start.map(|l| l + self.backend.interval());
        if let Some(p) = self.paused {
            at = Some(at.map_or(p, |a| a.max(p)));
        }
        // The first target not backing off.
        let ready = self
            .queue
            .iter()
            .position(|t| self.backoff.get(t).is_none_or(|b| b.1 <= now));
        let Some(i) = ready else {
            let soonest = self
                .queue
                .iter()
                .filter_map(|t| self.backoff.get(t))
                .map(|b| b.1)
                .min();
            return soonest.map_or(Next::Idle, Next::Wait);
        };
        if let Some(at) = at.filter(|a| *a > now) {
            return Next::Wait(at);
        }
        let Some(t) = self.queue.remove(i) else {
            return Next::Idle;
        };
        self.last_start = Some(now);
        self.in_flight.insert(t.clone());
        Next::Fetch(t)
    }

    /// A request ended. Results of targets not in flight are ignored.
    pub fn finish(&mut self, t: &Target, r: Result<Vec<Message>, LoadFailure>, now: Instant) {
        if !self.in_flight.remove(t) {
            return;
        }
        match r {
            Ok(found) => {
                self.done += 1;
                self.backoff.remove(t);
                self.merge_server(t, found);
            }
            Err(LoadFailure::RateLimited(d)) => {
                let d = if d.is_zero() { RATE_LIMIT_PAUSE } else { d };
                self.paused = Some(now + d.clamp(Duration::from_secs(1), MAX_PAUSE));
                self.queue.push_front(t.clone());
            }
            Err(LoadFailure::NotAllowed) => self.give_up(t),
            Err(LoadFailure::Transient | LoadFailure::Other) => {
                let n = self.backoff.get(t).map_or(0, |b| b.0) + 1;
                if n >= MAX_FAILURES {
                    self.give_up(t);
                } else {
                    self.backoff.insert(t.clone(), (n, now + retry_delay(n)));
                    self.queue.push_back(t.clone());
                }
            }
        }
    }

    fn give_up(&mut self, t: &Target) {
        self.done += 1;
        self.backoff.remove(t);
        if let Target::Room(rid) = t {
            self.failed.insert(rid.clone());
        }
    }

    fn merge_server(&mut self, t: &Target, found: Vec<Message>) {
        let ids: HashSet<&str> = found.iter().map(|m| m.id.as_str()).collect();
        // A room's local hits the server did not confirm stay only when
        // the server's answer was cut at its limit (older ones may be
        // missing) — otherwise they no longer match (deleted, edited).
        if let Target::Room(rid) = t
            && found.len() < PER_ROOM_LIMIT as usize
        {
            let before = self.hits.len();
            self.hits.retain(|id, h| {
                h.origin == Origin::Server || h.message.rid != *rid || ids.contains(id.as_str())
            });
            if self.hits.len() != before {
                self.revision += 1;
            }
        }
        for m in found {
            // Ciphertext and system messages are not results.
            if m.is_encrypted() || m.is_system() || m.id.is_empty() {
                continue;
            }
            self.hits.insert(
                m.id.clone(),
                Hit {
                    message: m,
                    origin: Origin::Server,
                },
            );
            self.revision += 1;
        }
    }

    /// Local hits (offline cache, loaded timelines): added unless the
    /// server already answered for that message.
    pub fn merge_local(&mut self, found: impl IntoIterator<Item = Message>) {
        for m in found {
            if m.id.is_empty() || self.hits.contains_key(&m.id) {
                continue;
            }
            self.hits.insert(
                m.id.clone(),
                Hit {
                    message: m,
                    origin: Origin::Local,
                },
            );
            self.revision += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// Hits grouped by room: rooms by their newest hit, hits newest first.
    pub fn groups(&self) -> Vec<(String, Vec<&Hit>)> {
        let ts = |h: &Hit| h.message.ts.map_or(0, |d| d.timestamp_millis());
        let mut by_room: HashMap<&str, Vec<&Hit>> = HashMap::new();
        for h in self.hits.values() {
            by_room.entry(h.message.rid.as_str()).or_default().push(h);
        }
        let mut groups: Vec<(String, Vec<&Hit>)> = by_room
            .into_iter()
            .map(|(rid, mut hits)| {
                hits.sort_by(|a, b| ts(b).cmp(&ts(a)).then(a.message.id.cmp(&b.message.id)));
                (rid.to_owned(), hits)
            })
            .collect();
        groups.sort_by(|a, b| {
            let newest = |g: &[&Hit]| g.first().map_or(0, |h| ts(h));
            newest(&b.1).cmp(&newest(&a.1)).then(a.0.cmp(&b.0))
        });
        groups
    }

    pub fn hit(&self, mid: &str) -> Option<&Hit> {
        self.hits.get(mid)
    }
}
