//! Picks the next fetch of the open inbox view, pure. Rate limits are per
//! IP and shared with the user's other clients: one request at a time,
//! `rooms.history` (20 / 10 s) at least [`HISTORY_INTERVAL`] apart,
//! `chat.getMentionedMessages` (10 / 60 s, shared with the room's Mentions
//! panel) at least [`MENTIONS_INTERVAL`] apart and at most
//! [`MENTIONS_BUDGET`] per session; nothing while disconnected or showing
//! the offline cache. A rate limit pauses its route; network errors back
//! off per request; a refusal gives the request up for the session.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::workspace::{ConnectionState, Workspace};

use super::feeds::FeedState;

/// Least time between two history pages.
pub const HISTORY_INTERVAL: Duration = Duration::from_secs(1);
/// Least time between two thread-mention fetches.
pub const MENTIONS_INTERVAL: Duration = Duration::from_secs(20);
/// Thread-mention fetches per session.
pub const MENTIONS_BUDGET: u32 = 15;
/// Pause after a rate limit without a reset time.
pub const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10);
/// Longest pause taken from the server.
pub const MAX_PAUSE: Duration = Duration::from_secs(300);
/// Unrecognised failures of one request before it is given up.
const MAX_OTHER_FAILURES: u32 = 3;

/// One request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fetch {
    /// A history page of the room's feed (`earlier`: the next older one).
    Feed { rid: String, earlier: bool },
    /// The room's mentions (`chat.getMentionedMessages`), for thread mentions.
    ThreadMentions { rid: String },
}

impl Fetch {
    pub fn rid(&self) -> &str {
        match self {
            Fetch::Feed { rid, .. } | Fetch::ThreadMentions { rid } => rid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Fetch(Fetch),
    /// Plan again at this time.
    Wait(Instant),
    Idle,
}

/// How a request failed, as far as pacing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadFailure {
    RateLimited(Duration),
    /// Network, timeout, 5xx: retried with back-off.
    Transient,
    /// No access: given up for the session.
    NotAllowed,
    /// Anything else: given up after a few tries.
    Other,
}

/// What the view needs, in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// Rooms whose feed must be loaded.
    pub feeds: Vec<String>,
    /// Rooms whose next earlier page was asked for.
    pub earlier: Vec<String>,
    /// Rooms whose thread mentions are wanted.
    pub thread_mentions: Vec<String>,
}

#[derive(Debug, Default)]
pub struct LoadState {
    in_flight: Option<Fetch>,
    last_history: Option<Instant>,
    last_mentions: Option<Instant>,
    history_paused: Option<Instant>,
    mentions_paused: Option<Instant>,
    mentions_used: u32,
    /// Failures and retry time of requests that failed transiently.
    backoff: HashMap<Fetch, (u32, Instant)>,
    others: HashMap<Fetch, u32>,
    given_up: HashSet<Fetch>,
}

/// Wait after `failures` (1-based) transient failures of one request: 5 s,
/// doubling, at most 5 min. Shared with the sidebar's Threads loader (both
/// hit the same per-IP limits).
pub fn retry_delay(failures: u32) -> Duration {
    let secs = 5u64.saturating_mul(1 << failures.saturating_sub(1).min(6));
    Duration::from_secs(secs.min(300))
}

impl LoadState {
    pub fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn in_flight(&self) -> Option<&Fetch> {
        self.in_flight.as_ref()
    }

    pub fn begin(&mut self, f: &Fetch, now: Instant) {
        match f {
            Fetch::Feed { .. } => self.last_history = Some(now),
            Fetch::ThreadMentions { .. } => {
                self.last_mentions = Some(now);
                self.mentions_used += 1;
            }
        }
        self.in_flight = Some(f.clone());
    }

    /// The request in flight ended (`Ok` or how it failed). Results of
    /// other requests (an earlier session) are ignored.
    pub fn finish(&mut self, f: &Fetch, r: Result<(), LoadFailure>, now: Instant) {
        if self.in_flight.as_ref() != Some(f) {
            return;
        }
        self.in_flight = None;
        match r {
            Ok(()) => {
                self.backoff.remove(f);
                self.others.remove(f);
            }
            Err(LoadFailure::RateLimited(d)) => {
                let until = Some(now + d.clamp(Duration::from_secs(1), MAX_PAUSE));
                match f {
                    Fetch::Feed { .. } => self.history_paused = until,
                    Fetch::ThreadMentions { .. } => self.mentions_paused = until,
                }
            }
            Err(LoadFailure::Transient) => {
                let n = self.backoff.get(f).map_or(0, |b| b.0) + 1;
                self.backoff.insert(f.clone(), (n, now + retry_delay(n)));
            }
            Err(LoadFailure::NotAllowed) => {
                self.given_up.insert(f.clone());
            }
            Err(LoadFailure::Other) => {
                let n = self.others.entry(f.clone()).or_default();
                *n += 1;
                if *n >= MAX_OTHER_FAILURES {
                    self.given_up.insert(f.clone());
                } else {
                    let n = *n;
                    self.backoff.insert(f.clone(), (n, now + retry_delay(n)));
                }
            }
        }
    }

    /// The request will fail for good: whether it is given up now.
    pub fn is_given_up(&self, f: &Fetch) -> bool {
        self.given_up.contains(f)
    }

    /// "Retry": the room's requests start over.
    pub fn forgive(&mut self, rid: &str) {
        self.given_up.retain(|f| f.rid() != rid);
        self.backoff.retain(|f, _| f.rid() != rid);
        self.others.retain(|f, _| f.rid() != rid);
    }

    /// A new session: everything starts over (a request in flight of the
    /// old one is dropped when it ends).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether `f` may go now; `Err(at)`: not before `at`; `None` in the
    /// error: never (given up).
    fn ready(&self, f: &Fetch, now: Instant) -> Result<(), Option<Instant>> {
        if self.given_up.contains(f) {
            return Err(None);
        }
        let (last, interval, paused) = match f {
            Fetch::Feed { .. } => (self.last_history, HISTORY_INTERVAL, self.history_paused),
            Fetch::ThreadMentions { .. } => {
                if self.mentions_used >= MENTIONS_BUDGET {
                    return Err(None);
                }
                (self.last_mentions, MENTIONS_INTERVAL, self.mentions_paused)
            }
        };
        let at = [
            last.map(|l| l + interval),
            paused,
            self.backoff.get(f).map(|b| b.1),
        ]
        .into_iter()
        .flatten()
        .max();
        match at {
            Some(at) if at > now => Err(Some(at)),
            _ => Ok(()),
        }
    }
}

/// The next request for `wanted`.
pub fn plan(ws: &Workspace, wanted: &Wanted, st: &LoadState, now: Instant) -> Next {
    if st.in_flight.is_some() || *ws.connection() != ConnectionState::Connected || ws.from_cache() {
        return Next::Idle;
    }
    let feeds = ws.feeds();
    let earlier = wanted.earlier.iter().filter_map(|rid| {
        let f = feeds.feed(rid)?;
        (f.is_loaded() && f.has_earlier() && !f.is_loading_earlier()).then(|| Fetch::Feed {
            rid: rid.clone(),
            earlier: true,
        })
    });
    let first = wanted.feeds.iter().filter_map(|rid| {
        let due = match feeds.feed(rid) {
            None => true,
            Some(f) => match f.state() {
                FeedState::Idle => true,
                FeedState::Loaded => f.is_stale(),
                FeedState::Loading | FeedState::Failed(_) => false,
            },
        };
        due.then(|| Fetch::Feed {
            rid: rid.clone(),
            earlier: false,
        })
    });
    let mentions = wanted
        .thread_mentions
        .iter()
        .filter(|rid| feeds.mentions_due(rid))
        .map(|rid| Fetch::ThreadMentions { rid: rid.clone() });
    let mut wait: Option<Instant> = None;
    for f in earlier.chain(first).chain(mentions) {
        match st.ready(&f, now) {
            Ok(()) => return Next::Fetch(f),
            Err(Some(at)) => wait = Some(wait.map_or(at, |w| w.min(at))),
            Err(None) => {}
        }
    }
    match wait {
        Some(at) => Next::Wait(at),
        None => Next::Idle,
    }
}
