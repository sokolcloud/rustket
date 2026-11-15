//! Read marks queued and paced for the server's rate limits, one request at
//! a time per account. DDP `readMessages` / `readThreads` are limited per
//! connection and method (10 / 10 s by default); REST `subscriptions.read`
//! per IP and route (10 / 60 s), so REST is only the fallback: without a
//! live DDP connection, or on a server without the methods.
//!
//! The queue is pure: the app sends what [`ReadQueue::next`] hands out and
//! reports the result with [`ReadQueue::finish`]. Callers tag requests with
//! an origin `O` and get it back in [`Done`].

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Time between two DDP read marks (under 10 per 10 s).
pub const DDP_INTERVAL: Duration = Duration::from_millis(1200);
/// Time between two REST read marks (under 10 per 60 s).
pub const REST_INTERVAL: Duration = Duration::from_secs(7);
/// Pause after a rate-limit answer without a reset time.
pub const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10);
/// Longest pause the queue accepts from the server.
pub const MAX_PAUSE: Duration = Duration::from_secs(300);
/// Attempts per request (rate limits and network errors) before it fails.
pub const MAX_TRIES: u32 = 3;
/// First wait after a network error (doubled per attempt).
const TRANSIENT_PAUSE: Duration = Duration::from_secs(5);

/// One read mark.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReadReq {
    /// Room `rid`; `threads`: its unread threads too.
    Room { rid: String, threads: bool },
    /// Thread `tmid` of room `rid`.
    Thread { rid: String, tmid: String },
}

impl ReadReq {
    pub fn rid(&self) -> &str {
        match self {
            ReadReq::Room { rid, .. } | ReadReq::Thread { rid, .. } => rid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Ddp,
    Rest,
}

impl Transport {
    pub fn interval(self) -> Duration {
        match self {
            Transport::Ddp => DDP_INTERVAL,
            Transport::Rest => REST_INTERVAL,
        }
    }
}

/// How a sent request ended, as far as the queue cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// Retry after the server's reset time (or [`RATE_LIMIT_PAUSE`]).
    RateLimited(Option<Duration>),
    /// The server has no such DDP method: use REST from now on.
    MethodMissing,
    /// Network error or timeout: retried with back-off.
    Transient,
    Failed,
}

impl Outcome {
    pub fn of(r: &Result<(), rocketchat::Error>) -> Self {
        match r {
            Ok(()) => Outcome::Ok,
            Err(e) => Self::from_error(e),
        }
    }

    pub fn from_error(e: &rocketchat::Error) -> Self {
        use rocketchat::Error;
        match e {
            Error::RateLimited { retry_after, .. } => Outcome::RateLimited(*retry_after),
            e if e.is_method_not_found() => Outcome::MethodMissing,
            e if e.is_transient() => Outcome::Transient,
            // E.g. the DDP connection dropped before the result arrived.
            Error::Http(_) | Error::Io(_) => Outcome::Transient,
            _ => Outcome::Failed,
        }
    }
}

/// A queued request and everyone waiting for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item<O> {
    pub req: ReadReq,
    pub origins: Vec<O>,
    tries: u32,
}

/// Identifies the request in flight; a stale [`ReadQueue::finish`] is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// What to do now.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Nothing to send (empty, a request in flight, or a timer is pending).
    Idle,
    /// Call [`ReadQueue::wake`] after this long, then [`ReadQueue::next`].
    Wait(Duration),
    /// Send `req` over `via`, then call [`ReadQueue::finish`] with `ticket`.
    Send {
        req: ReadReq,
        via: Transport,
        ticket: Ticket,
    },
}

/// A request that ended (sent, or given up).
#[derive(Debug, Clone, PartialEq)]
pub struct Done<O> {
    pub req: ReadReq,
    pub origins: Vec<O>,
    pub ok: bool,
}

#[derive(Debug)]
pub struct ReadQueue<O> {
    queue: VecDeque<Item<O>>,
    in_flight: Option<(Item<O>, Transport, Ticket)>,
    next_ticket: u64,
    not_before: Option<Instant>,
    /// A [`Next::Wait`] was handed out and not woken yet.
    waiting: bool,
    /// The server lacks the DDP methods: REST only.
    rest_only: bool,
}

impl<O> Default for ReadQueue<O> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            in_flight: None,
            next_ticket: 0,
            not_before: None,
            waiting: false,
            rest_only: false,
        }
    }
}

impl<O: Clone + PartialEq> ReadQueue<O> {
    /// Queues `req` for `origin`. A request for the same room or thread
    /// already waiting takes the origin instead (a room read with threads
    /// covers one without).
    pub fn push(&mut self, req: ReadReq, origin: O) {
        for item in &mut self.queue {
            let merged = match (&mut item.req, &req) {
                (a, b) if a == b => true,
                (
                    ReadReq::Room { rid: a, threads },
                    ReadReq::Room {
                        rid: b,
                        threads: more,
                    },
                ) if a == b => {
                    *threads |= *more;
                    true
                }
                _ => false,
            };
            if merged {
                if !item.origins.contains(&origin) {
                    item.origins.push(origin);
                }
                return;
            }
        }
        self.queue.push_back(Item {
            req,
            origins: vec![origin],
            tries: 0,
        });
    }

    /// The next step; `ddp_up`: the session's DDP connection is live.
    pub fn next(&mut self, now: Instant, ddp_up: bool) -> Next {
        if self.in_flight.is_some() || self.waiting || self.queue.is_empty() {
            return Next::Idle;
        }
        if let Some(at) = self.not_before.filter(|at| *at > now) {
            self.waiting = true;
            return Next::Wait(at - now);
        }
        let Some(item) = self.queue.pop_front() else {
            return Next::Idle;
        };
        let via = if ddp_up && !self.rest_only {
            Transport::Ddp
        } else {
            Transport::Rest
        };
        self.next_ticket += 1;
        let ticket = Ticket(self.next_ticket);
        self.not_before = Some(now + via.interval());
        let req = item.req.clone();
        self.in_flight = Some((item, via, ticket));
        Next::Send { req, via, ticket }
    }

    /// The timer of a [`Next::Wait`] fired.
    pub fn wake(&mut self) {
        self.waiting = false;
    }

    /// The request of `ticket` ended with `outcome`. Returns it when it is
    /// done for good (a retry puts it back at the front).
    pub fn finish(&mut self, ticket: Ticket, outcome: Outcome, now: Instant) -> Option<Done<O>> {
        if self.in_flight.as_ref().is_none_or(|(_, _, t)| *t != ticket) {
            return None;
        }
        let (mut item, via, _) = self.in_flight.take()?;
        let pause = match outcome {
            Outcome::Ok => return Some(done(item, true)),
            Outcome::Failed => return Some(done(item, false)),
            Outcome::MethodMissing if via == Transport::Ddp => {
                self.rest_only = true;
                self.not_before = Some(now);
                self.queue.push_front(item);
                return None;
            }
            Outcome::MethodMissing => return Some(done(item, false)),
            Outcome::RateLimited(d) => d
                .unwrap_or(RATE_LIMIT_PAUSE)
                .clamp(Duration::from_secs(1), MAX_PAUSE),
            Outcome::Transient => TRANSIENT_PAUSE * 2u32.pow(item.tries.min(6)),
        };
        item.tries += 1;
        if item.tries >= MAX_TRIES {
            return Some(done(item, false));
        }
        self.not_before = Some(now + pause.min(MAX_PAUSE));
        self.queue.push_front(item);
        None
    }

    /// Whether a request of `origin` is queued or in flight.
    pub fn has_origin(&self, origin: &O) -> bool {
        self.in_flight
            .iter()
            .map(|(i, _, _)| i)
            .chain(self.queue.iter())
            .any(|i| i.origins.contains(origin))
    }

    /// Gives up every queued request (no session to send them with); the
    /// one in flight still ends through [`Self::finish`].
    pub fn fail_queued(&mut self) -> Vec<Done<O>> {
        self.queue.drain(..).map(|i| done(i, false)).collect()
    }

    pub fn is_idle(&self) -> bool {
        self.in_flight.is_none() && self.queue.is_empty()
    }

    /// Drops everything (sign-out); a late `finish` is ignored.
    pub fn clear(&mut self) {
        let ticket = self.next_ticket;
        *self = Self::default();
        self.next_ticket = ticket;
    }
}

fn done<O>(item: Item<O>, ok: bool) -> Done<O> {
    Done {
        req: item.req,
        origins: item.origins,
        ok,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn room(rid: &str, threads: bool) -> ReadReq {
        ReadReq::Room {
            rid: rid.into(),
            threads,
        }
    }

    fn send(q: &mut ReadQueue<u8>, now: Instant, ddp: bool) -> (ReadReq, Transport, Ticket) {
        match q.next(now, ddp) {
            Next::Send { req, via, ticket } => (req, via, ticket),
            other => panic!("expected a send, got {other:?}"),
        }
    }

    #[test]
    fn one_at_a_time_paced_per_transport() {
        let t0 = Instant::now();
        let mut q = ReadQueue::default();
        q.push(room("a", true), 1);
        q.push(room("b", true), 1);
        let (req, via, ticket) = send(&mut q, t0, true);
        assert_eq!((req, via), (room("a", true), Transport::Ddp));
        // In flight: nothing else goes out.
        assert_eq!(q.next(t0, true), Next::Idle);
        let d = q.finish(ticket, Outcome::Ok, t0).unwrap();
        assert!(d.ok && d.origins == [1]);
        assert_eq!(q.next(t0, true), Next::Wait(DDP_INTERVAL));
        // The timer is pending: no second one.
        assert_eq!(q.next(t0, true), Next::Idle);
        q.wake();
        let t1 = t0 + DDP_INTERVAL;
        let (req, via, ticket) = send(&mut q, t1, false);
        assert_eq!((req, via), (room("b", true), Transport::Rest));
        assert!(q.finish(ticket, Outcome::Ok, t1).unwrap().ok);
        assert!(q.is_idle());
        q.push(room("c", false), 2);
        assert_eq!(q.next(t1, true), Next::Wait(REST_INTERVAL));
    }

    #[test]
    fn duplicates_merge_and_threads_win() {
        let t0 = Instant::now();
        let mut q = ReadQueue::default();
        q.push(room("a", false), 1);
        q.push(room("a", true), 2);
        q.push(room("a", false), 2);
        let thread = ReadReq::Thread {
            rid: "a".into(),
            tmid: "t".into(),
        };
        q.push(thread.clone(), 3);
        assert!(q.has_origin(&2) && q.has_origin(&3) && !q.has_origin(&4));
        let (req, _, ticket) = send(&mut q, t0, true);
        assert_eq!(req, room("a", true));
        let d = q.finish(ticket, Outcome::Ok, t0).unwrap();
        assert_eq!(d.origins, [1, 2]);
        q.wake();
        let (req, _, _) = send(&mut q, t0 + DDP_INTERVAL, true);
        assert_eq!(req, thread);
        assert_eq!(req.rid(), "a");
    }

    #[test]
    fn rate_limits_pause_and_give_up_after_three_tries() {
        let t0 = Instant::now();
        let mut q = ReadQueue::default();
        q.push(room("a", true), 1);
        let (_, _, ticket) = send(&mut q, t0, true);
        let limited = Outcome::RateLimited(Some(Duration::from_secs(30)));
        assert!(q.finish(ticket, limited, t0).is_none());
        assert_eq!(q.next(t0, true), Next::Wait(Duration::from_secs(30)));
        q.wake();
        let t1 = t0 + Duration::from_secs(30);
        let (req, _, ticket) = send(&mut q, t1, true);
        assert_eq!(req, room("a", true), "the same request is retried");
        // Without a reset time: the default pause; a huge one is capped.
        assert!(q.finish(ticket, Outcome::RateLimited(None), t1).is_none());
        assert_eq!(q.next(t1, true), Next::Wait(RATE_LIMIT_PAUSE));
        q.wake();
        let t2 = t1 + RATE_LIMIT_PAUSE;
        let (_, _, ticket) = send(&mut q, t2, true);
        let huge = Outcome::RateLimited(Some(Duration::from_secs(9999)));
        let d = q.finish(ticket, huge, t2).unwrap();
        assert!(!d.ok);
        assert!(q.is_idle());
    }

    #[test]
    fn missing_method_switches_to_rest_for_good() {
        let t0 = Instant::now();
        let mut q = ReadQueue::default();
        q.push(room("a", true), 1);
        q.push(room("b", true), 1);
        let (_, via, ticket) = send(&mut q, t0, true);
        assert_eq!(via, Transport::Ddp);
        assert!(q.finish(ticket, Outcome::MethodMissing, t0).is_none());
        let (req, via, ticket) = send(&mut q, t0, true);
        assert_eq!((req, via), (room("a", true), Transport::Rest));
        // REST has no such failure: it is final there.
        assert!(!q.finish(ticket, Outcome::MethodMissing, t0).unwrap().ok);
        q.wake();
        let (_, via, _) = send(&mut q, t0 + REST_INTERVAL, true);
        assert_eq!(via, Transport::Rest);
    }

    #[test]
    fn failures_transients_and_stale_results() {
        let t0 = Instant::now();
        let mut q = ReadQueue::default();
        q.push(room("a", true), 1);
        let (_, _, ticket) = send(&mut q, t0, true);
        assert!(q.finish(ticket, Outcome::Transient, t0).is_none());
        assert_eq!(q.next(t0, true), Next::Wait(TRANSIENT_PAUSE));
        q.wake();
        let t1 = t0 + TRANSIENT_PAUSE;
        let (_, _, ticket) = send(&mut q, t1, true);
        // A stale ticket changes nothing.
        assert!(q.finish(Ticket(999), Outcome::Ok, t1).is_none());
        assert!(!q.finish(ticket, Outcome::Failed, t1).unwrap().ok);
        // Cleared while in flight: the late result is dropped.
        q.push(room("b", true), 2);
        q.wake();
        let (_, _, ticket) = send(&mut q, t1 + DDP_INTERVAL, true);
        q.clear();
        assert!(q.finish(ticket, Outcome::Ok, t1).is_none());
        assert!(q.is_idle() && !q.has_origin(&2));
        q.push(room("c", true), 3);
        q.push(room("d", true), 3);
        let failed = q.fail_queued();
        assert_eq!(failed.len(), 2);
        assert!(failed.iter().all(|d| !d.ok && d.origins == [3]));
        assert!(q.is_idle());
    }

    #[test]
    fn outcomes_from_errors() {
        use rocketchat::Error;
        assert_eq!(Outcome::of(&Ok(())), Outcome::Ok);
        let limited = Error::RateLimited {
            retry_after: Some(Duration::from_secs(4)),
            message: None,
        };
        assert_eq!(
            Outcome::of(&Err(limited)),
            Outcome::RateLimited(Some(Duration::from_secs(4)))
        );
        let missing = Error::from_method_error(rocketchat::MethodError {
            error: Some(serde_json::json!(404)),
            ..Default::default()
        });
        assert_eq!(Outcome::of(&Err(missing)), Outcome::MethodMissing);
        assert_eq!(Outcome::of(&Err(Error::Timeout("x"))), Outcome::Transient);
        let io = Error::Io(std::io::Error::other("gone"));
        assert_eq!(Outcome::of(&Err(io)), Outcome::Transient);
        let denied = Error::from_method_error(rocketchat::MethodError {
            error: Some(serde_json::json!("error-room-does-not-exist")),
            ..Default::default()
        });
        assert_eq!(Outcome::of(&Err(denied)), Outcome::Failed);
    }
}
