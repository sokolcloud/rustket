//! Video conferences: the `video-conference.info` cache behind the `video_conf`
//! message blocks ([`VideoConfStore`]) and the pure direct-call ringing state
//! machine ([`CallManager`]), docs/teams-videoconf.md §7.
//!
//! Everything here is synchronous and `Instant`-driven (no I/O): the app turns
//! [`CallCommand`]s into REST calls, DDP publishes, popups and sounds.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rocketchat::models::{
    ProviderCapabilities, StartInstructions, VideoConfAction, VideoConfKind, VideoConfSignal,
    VideoConfStatus, VideoConference,
};

/// Caller re-sends `call` this often (`CALL_INTERVAL`).
pub const CALL_INTERVAL: Duration = Duration::from_millis(3_000);
/// The caller gives up after this many `call` signals.
pub const CALL_ATTEMPT_LIMIT: u32 = 10;
/// The callee drops a ring that is not refreshed for this long.
pub const CALL_TIMEOUT: Duration = Duration::from_millis(10_000);
/// The callee waits this long for `confirmed` after accepting.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_millis(5_000);
/// Dismissed / refused call ids are ignored for this long (`CALL_TIMEOUT × 20`).
pub const DISMISS_TTL: Duration = Duration::from_millis(200_000);
/// At most one new incoming call per sender in this window.
pub const SENDER_COOLDOWN: Duration = Duration::from_secs(5);
/// Incoming calls waiting for verification at once; more are dropped.
pub const MAX_UNVERIFIED: usize = 3;
/// Bound of the dismissed-call list.
pub const MAX_DISMISSED: usize = 256;
/// Cached provider capabilities expire after this long.
pub const CAPABILITIES_TTL: Duration = Duration::from_secs(300);
/// `video-conference.info` requests allowed per [`INFO_WINDOW`] (the server
/// allows 15 / 3 s; block and verification fetches share the budget).
pub const INFO_RATE: usize = 12;
pub const INFO_WINDOW: Duration = Duration::from_millis(3_000);
/// Back-off before a failed `video-conference.info` is retried.
pub const INFO_RETRY: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(20),
    Duration::from_secs(60),
];

// ---- info cache -----------------------------------------------------------------

/// What is known about a call shown in a `video_conf` block.
#[derive(Debug, Clone, PartialEq)]
pub enum InfoState {
    Loading,
    Loaded(Box<VideoConference>),
    /// `video-conference.info` failed (not accessible, unknown).
    Failed,
}

#[derive(Debug, Clone)]
struct InfoEntry {
    state: InfoState,
    version: u64,
    stale: bool,
    in_flight: bool,
    failures: usize,
    retry_at: Option<Instant>,
}

impl InfoEntry {
    fn new() -> Self {
        Self {
            state: InfoState::Loading,
            version: 0,
            stale: false,
            in_flight: false,
            failures: 0,
            retry_at: None,
        }
    }

    /// Refetch without the view asking again (invalidated or retry due).
    fn due(&self, now: Instant) -> bool {
        !self.in_flight && (self.stale || self.retry_at.is_some_and(|at| now >= at))
    }
}

/// Call ids the renderer asked for (interior mutability: `view` only has
/// `&Workspace`), drained by the app after each frame.
#[derive(Debug, Default)]
struct Wanted(Mutex<BTreeSet<String>>);

impl Wanted {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clone for Wanted {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.lock().clone()))
    }
}

/// Per-server video-conference data kept in the workspace.
#[derive(Debug, Clone, Default)]
pub struct VideoConfStore {
    entries: HashMap<String, InfoEntry>,
    wanted: Wanted,
    /// Start times of recent `video-conference.info` requests.
    sent: VecDeque<Instant>,
    capabilities: Option<(Instant, Result<ProviderCapabilities, String>)>,
    license_modules: Option<Vec<String>>,
}

impl VideoConfStore {
    /// Cached state of `call_id`.
    pub fn state(&self, call_id: &str) -> Option<&InfoState> {
        self.entries.get(call_id).map(|e| &e.state)
    }

    /// The loaded conference, if any.
    pub fn info(&self, call_id: &str) -> Option<&VideoConference> {
        match self.state(call_id) {
            Some(InfoState::Loaded(v)) => Some(v),
            _ => None,
        }
    }

    /// Changes whenever the entry of `call_id` changes (for `lazy` keys).
    pub fn version(&self, call_id: &str) -> u64 {
        self.entries.get(call_id).map_or(0, |e| e.version + 1)
    }

    fn needs_fetch(&self, call_id: &str) -> bool {
        match self.entries.get(call_id) {
            None => true,
            Some(e) => e.stale && !e.in_flight,
        }
    }

    /// Asks for `call_id` (from `view`): fetched after the frame unless known.
    pub fn want(&self, call_id: &str) {
        if call_id.is_empty() || !self.needs_fetch(call_id) {
            return;
        }
        self.wanted.lock().insert(call_id.to_owned());
    }

    fn prune_sent(&mut self, now: Instant) {
        while self
            .sent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= INFO_WINDOW)
        {
            self.sent.pop_front();
        }
    }

    /// Takes one request from the shared `video-conference.info` budget.
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        self.prune_sent(now);
        if self.sent.len() >= INFO_RATE {
            return false;
        }
        self.sent.push_back(now);
        true
    }

    /// Call ids to fetch now: wanted by the view, invalidated, or due for a
    /// retry, within the request budget (the rest wait). They are marked in
    /// flight; report each with [`Self::finish`].
    pub fn take_wanted(&mut self, now: Instant) -> Vec<String> {
        let mut set = std::mem::take(&mut *self.wanted.lock());
        set.extend(
            self.entries
                .iter()
                .filter(|(_, e)| e.due(now))
                .map(|(id, _)| id.clone()),
        );
        let mut out = Vec::new();
        let mut rest = BTreeSet::new();
        while let Some(id) = set.pop_first() {
            let pending = match self.entries.get(&id) {
                None => true,
                Some(e) => e.due(now),
            };
            if !pending {
                continue;
            }
            if !self.try_acquire(now) {
                // Entries are found again by `due`; only new ids need keeping.
                if !self.entries.contains_key(&id) {
                    rest.insert(id);
                }
                continue;
            }
            let e = self
                .entries
                .entry(id.clone())
                .or_insert_with(InfoEntry::new);
            e.in_flight = true;
            e.stale = false;
            e.retry_at = None;
            out.push(id);
        }
        if !rest.is_empty() {
            self.wanted.lock().extend(rest);
        }
        out
    }

    /// When [`Self::take_wanted`] has something to do again (budget refill
    /// or a retry), if anything is waiting.
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        let waiting = !self.wanted.lock().is_empty()
            || self.entries.values().any(|e| !e.in_flight && e.stale);
        let refill = waiting.then(|| {
            if self.sent.len() >= INFO_RATE {
                self.sent.front().map_or(now, |at| *at + INFO_WINDOW)
            } else {
                now
            }
        });
        let retry = self
            .entries
            .values()
            .filter(|e| !e.in_flight)
            .filter_map(|e| e.retry_at)
            .min();
        match (refill, retry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Result of `video-conference.info` for `call_id`. A failure keeps
    /// earlier data and is retried with back-off. A document for another
    /// call id is refused.
    pub fn finish(&mut self, call_id: &str, result: Result<VideoConference, String>, now: Instant) {
        let e = self
            .entries
            .entry(call_id.to_owned())
            .or_insert_with(InfoEntry::new);
        e.in_flight = false;
        e.version += 1;
        match result {
            Ok(v) if v.id == call_id => {
                e.state = InfoState::Loaded(Box::new(v));
                e.failures = 0;
                e.retry_at = None;
            }
            Ok(_) => {
                if !matches!(e.state, InfoState::Loaded(_)) {
                    e.state = InfoState::Failed;
                }
            }
            Err(_) => {
                if !matches!(e.state, InfoState::Loaded(_)) {
                    e.state = InfoState::Failed;
                }
                e.retry_at = INFO_RETRY.get(e.failures).map(|d| now + *d);
                e.failures += 1;
            }
        }
    }

    /// `{rid}/videoconf` for `call_id`: refetch unless the call ended (final).
    /// The refetch does not wait for the view to ask again.
    pub fn invalidate(&mut self, call_id: &str) {
        if let Some(e) = self.entries.get_mut(call_id) {
            let ended = matches!(&e.state, InfoState::Loaded(v) if v.is_ended());
            if !ended {
                e.stale = true;
                e.failures = 0;
            }
        }
    }

    /// After a resync: refetch every running call.
    pub fn invalidate_all(&mut self) {
        let ids: Vec<String> = self.entries.keys().cloned().collect();
        for id in ids {
            self.invalidate(&id);
        }
    }

    /// Cached `video-conference.capabilities` result (within [`CAPABILITIES_TTL`]).
    pub fn capabilities(&self, now: Instant) -> Option<&Result<ProviderCapabilities, String>> {
        self.capabilities
            .as_ref()
            .filter(|(at, _)| now.duration_since(*at) < CAPABILITIES_TTL)
            .map(|(_, r)| r)
    }

    /// Stores a capabilities result (`Err` = the provider error code).
    pub fn set_capabilities(&mut self, now: Instant, r: Result<ProviderCapabilities, String>) {
        self.capabilities = Some((now, r));
    }

    /// Apps changed (`stream-apps`): forget the cached capabilities.
    pub fn clear_capabilities(&mut self) {
        self.capabilities = None;
    }

    /// `licenses.info` → `license.activeModules`.
    pub fn set_license_modules(&mut self, modules: Vec<String>) {
        self.license_modules = Some(modules);
    }

    /// The `videoconference-enterprise` module is active (ringing, call history).
    pub fn enterprise(&self) -> bool {
        self.license_modules
            .as_ref()
            .is_some_and(|m| m.iter().any(|x| x == "videoconference-enterprise"))
    }
}

// ---- ringing ---------------------------------------------------------------------

/// How an incoming call rings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingKind {
    /// Direct-call handshake (`call` → `accepted` → `confirmed`).
    Direct,
    /// One-shot ring of a group call (conference-window mode only).
    Ring,
}

/// A call ringing for the current user.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub call_id: String,
    /// The caller (signal sender, confirmed by `info.createdBy` for direct calls).
    pub uid: String,
    pub rid: String,
    pub kind: IncomingKind,
    /// Confirmed with `video-conference.info`; only verified calls are shown.
    pub verified: bool,
    /// Caller's display name from the confirmed info.
    pub caller: Option<String>,
    pub capabilities: Option<rocketchat::models::VideoConfCapabilities>,
    deadline: Instant,
    /// Accepted, waiting for `confirmed` until then.
    pub accept_deadline: Option<Instant>,
}

/// The current user's ringing direct call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub call_id: String,
    pub rid: String,
    pub callee: String,
    pub attempts: u32,
    next_ring: Instant,
    pub joined: bool,
}

/// Why a call ended without being joined (shown to the user).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallNotice {
    /// The callee declined.
    Rejected,
    /// Nobody answered within the attempt limit.
    NoAnswer,
    /// Accepted, but the caller never confirmed.
    AcceptTimeout,
    /// Another call is already ringing.
    Busy,
}

/// What the app should do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallCommand {
    /// `video-conference.info` to verify an incoming signal.
    FetchInfo(String),
    /// Publish `{action, params:{callId, uid: me, rid}}` to `to`.
    Publish {
        to: String,
        action: VideoConfAction,
        call_id: String,
        rid: String,
    },
    RestCancel(String),
    RestDecline(String),
    /// `video-conference.join` and open the URL in the browser.
    Join(String),
    ShowIncoming(String),
    HideIncoming(String),
    ShowOutgoing(String),
    HideOutgoing(String),
    PlayRinger,
    StopRinger,
    PlayDialer,
    StopDialer,
    /// Desktop notification for a verified incoming call.
    Notify(String),
    Notice(CallNotice),
}

/// Direct-call ringing (`VideoConfManager` of the web client), fed with
/// realtime signals, REST results, user actions and ticks.
#[derive(Debug, Clone, Default)]
pub struct CallManager {
    me: String,
    incoming: Vec<Incoming>,
    outgoing: Option<Outgoing>,
    dismissed: HashMap<String, Instant>,
    last_from: HashMap<String, Instant>,
    /// `VideoConf_Conference_Window_Enabled`: honour group `ring` signals.
    pub honour_ring: bool,
}

impl CallManager {
    pub fn new(me: &str) -> Self {
        Self {
            me: me.to_owned(),
            ..Default::default()
        }
    }

    pub fn set_me(&mut self, me: &str) {
        if self.me != me {
            *self = Self {
                honour_ring: self.honour_ring,
                ..Self::new(me)
            };
        }
    }

    /// Verified incoming calls, oldest first.
    pub fn incoming(&self) -> impl Iterator<Item = &Incoming> {
        self.incoming.iter().filter(|c| c.verified)
    }

    pub fn incoming_call(&self, call_id: &str) -> Option<&Incoming> {
        self.incoming.iter().find(|c| c.call_id == call_id)
    }

    pub fn outgoing(&self) -> Option<&Outgoing> {
        self.outgoing.as_ref()
    }

    /// An outgoing call is ringing (another start is refused).
    pub fn is_busy(&self) -> bool {
        self.outgoing.as_ref().is_some_and(|o| !o.joined)
    }

    /// Anything to tick for.
    pub fn is_active(&self) -> bool {
        !self.incoming.is_empty() || self.outgoing.is_some()
    }

    fn is_dismissed(&self, call_id: &str, now: Instant) -> bool {
        self.dismissed
            .get(call_id)
            .is_some_and(|at| now.duration_since(*at) < DISMISS_TTL)
    }

    fn drop_incoming(&mut self, call_id: &str, out: &mut Vec<CallCommand>) {
        let before = self.incoming.iter().any(|c| c.verified);
        if let Some(i) = self.incoming.iter().position(|c| c.call_id == call_id) {
            let c = self.incoming.remove(i);
            if c.verified {
                out.push(CallCommand::HideIncoming(c.call_id));
            }
        }
        let ringing = self
            .incoming
            .iter()
            .any(|c| c.verified && c.accept_deadline.is_none());
        if before && !ringing {
            out.push(CallCommand::StopRinger);
        }
    }

    fn dismiss(&mut self, call_id: &str, now: Instant, out: &mut Vec<CallCommand>) {
        self.dismissed
            .retain(|_, at| now.duration_since(*at) < DISMISS_TTL);
        if self.dismissed.len() >= MAX_DISMISSED
            && let Some(oldest) = self
                .dismissed
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, _)| id.clone())
        {
            self.dismissed.remove(&oldest);
        }
        self.dismissed.insert(call_id.to_owned(), now);
        self.drop_incoming(call_id, out);
    }

    fn end_outgoing(&mut self, out: &mut Vec<CallCommand>) -> Option<Outgoing> {
        let o = self.outgoing.take()?;
        if !o.joined {
            out.push(CallCommand::HideOutgoing(o.call_id.clone()));
            out.push(CallCommand::StopDialer);
        }
        Some(o)
    }

    /// A `{uid}/video-conference` signal. `room_known`: `rid` is a room of the
    /// workspace (unknown rooms are dropped).
    pub fn on_signal(
        &mut self,
        s: VideoConfSignal,
        room_known: bool,
        now: Instant,
    ) -> Vec<CallCommand> {
        let mut out = Vec::new();
        if self.me.is_empty() {
            return out;
        }
        match s.action {
            VideoConfAction::Call | VideoConfAction::Ring => {
                let kind = if s.action == VideoConfAction::Call {
                    IncomingKind::Direct
                } else {
                    IncomingKind::Ring
                };
                if (kind == IncomingKind::Ring && !self.honour_ring)
                    || s.uid == self.me
                    || !room_known
                    || self.is_dismissed(&s.call_id, now)
                {
                    return out;
                }
                if let Some(c) = self.incoming.iter_mut().find(|c| c.call_id == s.call_id) {
                    // A refresh only counts from the verified caller.
                    if c.verified && c.uid == s.uid && c.rid == s.rid && c.kind == kind {
                        c.deadline = now + CALL_TIMEOUT;
                    }
                    return out;
                }
                // Nothing about other calls changes before verification
                // (§7.5): the cooldown is renewed only by verified calls and
                // unverified entries are capped globally.
                if self
                    .last_from
                    .get(&s.uid)
                    .is_some_and(|at| now.duration_since(*at) < SENDER_COOLDOWN)
                    || self.incoming.iter().filter(|c| !c.verified).count() >= MAX_UNVERIFIED
                {
                    return out;
                }
                self.incoming.push(Incoming {
                    call_id: s.call_id.clone(),
                    uid: s.uid,
                    rid: s.rid,
                    kind,
                    verified: false,
                    caller: None,
                    capabilities: None,
                    deadline: now + CALL_TIMEOUT,
                    accept_deadline: None,
                });
                out.push(CallCommand::FetchInfo(s.call_id));
            }
            VideoConfAction::Canceled | VideoConfAction::End => {
                if let Some(c) = self.incoming_call(&s.call_id) {
                    // `canceled` only from the caller; `end` comes from the server.
                    if s.action == VideoConfAction::End || c.uid == s.uid {
                        self.dismiss(&s.call_id, now, &mut out);
                    }
                } else if s.action == VideoConfAction::End
                    && self
                        .outgoing
                        .as_ref()
                        .is_some_and(|o| o.call_id == s.call_id)
                {
                    // "Stop ringing", not "the call is over" (the 40 s timer
                    // also ends accepted calls): a joined call stays as it is.
                    if self.outgoing.as_ref().is_some_and(|o| !o.joined) {
                        self.end_outgoing(&mut out);
                    } else {
                        self.outgoing = None;
                    }
                }
            }
            VideoConfAction::Join => {
                if self.incoming_call(&s.call_id).is_some() && s.uid == self.me {
                    // Joined from another device.
                    self.dismiss(&s.call_id, now, &mut out);
                } else if let Some(o) = self.outgoing.as_ref().filter(|o| o.call_id == s.call_id) {
                    if s.uid == self.me {
                        // Joined elsewhere: stop ringing here.
                        self.end_outgoing(&mut out);
                    } else if !o.joined {
                        self.accepted(&mut out);
                    }
                }
            }
            VideoConfAction::Accepted => {
                if self
                    .outgoing
                    .as_ref()
                    .is_some_and(|o| o.call_id == s.call_id && o.callee == s.uid && !o.joined)
                {
                    self.accepted(&mut out);
                }
            }
            VideoConfAction::Rejected => {
                if self
                    .outgoing
                    .as_ref()
                    .is_some_and(|o| o.call_id == s.call_id && o.callee == s.uid)
                    && let Some(o) = self.end_outgoing(&mut out)
                {
                    if !o.joined {
                        out.push(CallCommand::RestCancel(o.call_id));
                    }
                    out.push(CallCommand::Notice(CallNotice::Rejected));
                }
            }
            VideoConfAction::Confirmed => {
                if let Some(c) = self.incoming_call(&s.call_id)
                    && c.accept_deadline.is_some()
                    && c.uid == s.uid
                {
                    let id = c.call_id.clone();
                    self.dismiss(&id, now, &mut out);
                    out.push(CallCommand::Join(id));
                }
            }
            VideoConfAction::Started | VideoConfAction::Other(_) => {}
        }
        out
    }

    /// The callee accepted our outgoing call: join and confirm.
    fn accepted(&mut self, out: &mut Vec<CallCommand>) {
        let Some(o) = self.outgoing.as_mut() else {
            return;
        };
        o.joined = true;
        let (call_id, rid, callee) = (o.call_id.clone(), o.rid.clone(), o.callee.clone());
        out.push(CallCommand::HideOutgoing(call_id.clone()));
        out.push(CallCommand::StopDialer);
        out.push(CallCommand::Join(call_id.clone()));
        out.push(CallCommand::Publish {
            to: callee,
            action: VideoConfAction::Confirmed,
            call_id,
            rid,
        });
    }

    /// `video-conference.info` for an unverified incoming call. The signal is
    /// trusted only if the document matches it (§7.5).
    pub fn on_verified(
        &mut self,
        call_id: &str,
        info: Result<&VideoConference, ()>,
        now: Instant,
    ) -> Vec<CallCommand> {
        let mut out = Vec::new();
        let me = self.me.clone();
        let Some(c) = self
            .incoming
            .iter_mut()
            .find(|c| c.call_id == call_id && !c.verified)
        else {
            return out;
        };
        let ok = info.is_ok_and(|v| {
            v.id == c.call_id
                && v.rid == c.rid
                && v.ended_at.is_none()
                && match c.kind {
                    IncomingKind::Direct => {
                        v.kind == VideoConfKind::Direct
                            && v.status == VideoConfStatus::Calling
                            && v.creator_id() == Some(c.uid.as_str())
                    }
                    IncomingKind::Ring => v.users.iter().any(|u| u.id == me),
                }
        });
        if !ok {
            let id = call_id.to_owned();
            self.dismiss(&id, now, &mut out);
            return out;
        }
        if let Ok(v) = info {
            c.caller = v
                .created_by
                .as_ref()
                .and_then(|u| u.name.clone().or(u.username.clone()));
            c.capabilities = v.capabilities;
        }
        c.verified = true;
        c.deadline = now + CALL_TIMEOUT;
        let uid = c.uid.clone();
        self.last_from
            .retain(|_, at| now.duration_since(*at) < SENDER_COOLDOWN);
        self.last_from.insert(uid.clone(), now);
        // A new verified call from the same user replaces an older ringing one.
        let older: Vec<String> = self
            .incoming
            .iter()
            .filter(|o| {
                o.uid == uid && o.verified && o.call_id != call_id && o.accept_deadline.is_none()
            })
            .map(|o| o.call_id.clone())
            .collect();
        for id in older {
            self.drop_incoming(&id, &mut out);
        }
        out.push(CallCommand::ShowIncoming(call_id.to_owned()));
        out.push(CallCommand::PlayRinger);
        out.push(CallCommand::Notify(call_id.to_owned()));
        out
    }

    /// `video-conference.start` succeeded for `rid`.
    pub fn on_started(
        &mut self,
        rid: &str,
        s: &StartInstructions,
        now: Instant,
    ) -> Vec<CallCommand> {
        let mut out = Vec::new();
        match s {
            StartInstructions::Direct {
                call_id, callee_id, ..
            } => {
                if self.is_busy() {
                    out.push(CallCommand::Notice(CallNotice::Busy));
                    return out;
                }
                self.outgoing = Some(Outgoing {
                    call_id: call_id.clone(),
                    rid: rid.to_owned(),
                    callee: callee_id.clone(),
                    attempts: 1,
                    next_ring: now + CALL_INTERVAL,
                    joined: false,
                });
                out.push(CallCommand::Publish {
                    to: callee_id.clone(),
                    action: VideoConfAction::Call,
                    call_id: call_id.clone(),
                    rid: rid.to_owned(),
                });
                out.push(CallCommand::ShowOutgoing(call_id.clone()));
                out.push(CallCommand::PlayDialer);
            }
            StartInstructions::Conference { call_id, .. }
            | StartInstructions::Livechat { call_id, .. } => {
                out.push(CallCommand::Join(call_id.clone()));
            }
        }
        out
    }

    /// No request budget to verify `call_id`: forget it without dismissing
    /// (the caller's next `call` tries again).
    pub fn drop_unverified(&mut self, call_id: &str) {
        self.incoming.retain(|c| c.verified || c.call_id != call_id);
    }

    /// The user accepted incoming `call_id`.
    pub fn accept(&mut self, call_id: &str, now: Instant) -> Vec<CallCommand> {
        let mut out = Vec::new();
        let Some(c) = self
            .incoming
            .iter_mut()
            .find(|c| c.call_id == call_id && c.verified && c.accept_deadline.is_none())
        else {
            return out;
        };
        match c.kind {
            IncomingKind::Direct => {
                c.accept_deadline = Some(now + ACCEPT_TIMEOUT);
                let (to, rid) = (c.uid.clone(), c.rid.clone());
                out.push(CallCommand::Publish {
                    to,
                    action: VideoConfAction::Accepted,
                    call_id: call_id.to_owned(),
                    rid,
                });
                if !self
                    .incoming
                    .iter()
                    .any(|c| c.verified && c.accept_deadline.is_none())
                {
                    out.push(CallCommand::StopRinger);
                }
            }
            IncomingKind::Ring => {
                self.dismiss(call_id, now, &mut out);
                out.push(CallCommand::Join(call_id.to_owned()));
            }
        }
        out
    }

    /// The user declined incoming `call_id`.
    pub fn decline(&mut self, call_id: &str, now: Instant) -> Vec<CallCommand> {
        let mut out = Vec::new();
        let Some(c) = self.incoming_call(call_id).filter(|c| c.verified).cloned() else {
            return out;
        };
        self.dismiss(call_id, now, &mut out);
        match c.kind {
            IncomingKind::Direct => out.push(CallCommand::Publish {
                to: c.uid,
                action: VideoConfAction::Rejected,
                call_id: c.call_id,
                rid: c.rid,
            }),
            IncomingKind::Ring => out.push(CallCommand::RestDecline(c.call_id)),
        }
        out
    }

    /// "Mute and dismiss": local only, no signal.
    pub fn mute(&mut self, call_id: &str, now: Instant) -> Vec<CallCommand> {
        let mut out = Vec::new();
        self.dismiss(call_id, now, &mut out);
        out
    }

    /// The caller hangs up the ringing outgoing call.
    pub fn hangup(&mut self) -> Vec<CallCommand> {
        let mut out = Vec::new();
        if let Some(o) = self.end_outgoing(&mut out) {
            out.push(CallCommand::Publish {
                to: o.callee.clone(),
                action: VideoConfAction::Canceled,
                call_id: o.call_id.clone(),
                rid: o.rid.clone(),
            });
            if !o.joined {
                out.push(CallCommand::RestCancel(o.call_id));
            }
        }
        out
    }

    /// Timers: re-ring, give up, ring and accept timeouts, expiry of the
    /// dismissed list.
    pub fn tick(&mut self, now: Instant) -> Vec<CallCommand> {
        let mut out = Vec::new();
        if let Some(o) = self.outgoing.as_mut()
            && !o.joined
            && now >= o.next_ring
        {
            if o.attempts >= CALL_ATTEMPT_LIMIT {
                let mut hang = self.hangup();
                out.append(&mut hang);
                out.push(CallCommand::Notice(CallNotice::NoAnswer));
            } else {
                o.attempts += 1;
                o.next_ring = now + CALL_INTERVAL;
                out.push(CallCommand::Publish {
                    to: o.callee.clone(),
                    action: VideoConfAction::Call,
                    call_id: o.call_id.clone(),
                    rid: o.rid.clone(),
                });
            }
        }
        let expired: Vec<(String, bool)> = self
            .incoming
            .iter()
            .filter_map(|c| match c.accept_deadline {
                Some(d) if now >= d => Some((c.call_id.clone(), true)),
                None if now >= c.deadline => Some((c.call_id.clone(), false)),
                _ => None,
            })
            .collect();
        for (id, accept) in expired {
            self.dismiss(&id, now, &mut out);
            if accept {
                out.push(CallCommand::Notice(CallNotice::AcceptTimeout));
            }
        }
        self.dismissed
            .retain(|_, at| now.duration_since(*at) < DISMISS_TTL);
        self.last_from
            .retain(|_, at| now.duration_since(*at) < SENDER_COOLDOWN);
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::{UserRef, VideoConfUser};

    fn sig(action: VideoConfAction, call: &str, uid: &str) -> VideoConfSignal {
        VideoConfSignal {
            action,
            call_id: call.into(),
            uid: uid.into(),
            rid: "R".into(),
        }
    }

    fn info(call: &str, creator: &str) -> VideoConference {
        VideoConference {
            id: call.into(),
            kind: VideoConfKind::Direct,
            rid: "R".into(),
            status: VideoConfStatus::Calling,
            created_by: Some(UserRef {
                id: creator.into(),
                username: Some(creator.into()),
                name: Some(format!("Name {creator}")),
            }),
            users: vec![VideoConfUser {
                id: "me".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn has(out: &[CallCommand], c: &CallCommand) -> bool {
        out.contains(c)
    }

    /// Callee: `call` → verify → popup → accept → `confirmed` → join.
    #[test]
    fn callee_handshake() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        let out = m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t0);
        assert_eq!(out, vec![CallCommand::FetchInfo("c1".into())]);
        assert_eq!(m.incoming().count(), 0, "not shown before verification");
        let out = m.on_verified("c1", Ok(&info("c1", "bob")), t0);
        assert!(has(&out, &CallCommand::ShowIncoming("c1".into())));
        assert!(has(&out, &CallCommand::PlayRinger));
        assert_eq!(
            m.incoming().next().unwrap().caller.as_deref(),
            Some("Name bob")
        );
        let out = m.accept("c1", t0);
        assert!(has(
            &out,
            &CallCommand::Publish {
                to: "bob".into(),
                action: VideoConfAction::Accepted,
                call_id: "c1".into(),
                rid: "R".into()
            }
        ));
        assert!(has(&out, &CallCommand::StopRinger));
        // `confirmed` from someone else is ignored.
        assert!(
            m.on_signal(sig(VideoConfAction::Confirmed, "c1", "eve"), true, t0)
                .is_empty()
        );
        let out = m.on_signal(sig(VideoConfAction::Confirmed, "c1", "bob"), true, t0);
        assert!(has(&out, &CallCommand::Join("c1".into())));
        assert!(!m.is_active());
    }

    /// Caller: start → ring every 3 s → `accepted` → join + `confirmed`.
    #[test]
    fn caller_handshake() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        let start = StartInstructions::Direct {
            call_id: "c1".into(),
            callee_id: "bob".into(),
            provider_name: "jitsi".into(),
        };
        let out = m.on_started("R", &start, t0);
        assert!(has(&out, &CallCommand::ShowOutgoing("c1".into())));
        assert!(m.is_busy());
        assert!(m.tick(t0 + Duration::from_millis(1_000)).is_empty());
        let out = m.tick(t0 + CALL_INTERVAL);
        assert!(matches!(
            &out[..],
            [CallCommand::Publish {
                action: VideoConfAction::Call,
                ..
            }]
        ));
        // A foreign `accepted` is ignored.
        assert!(
            m.on_signal(sig(VideoConfAction::Accepted, "c1", "eve"), true, t0)
                .is_empty()
        );
        let out = m.on_signal(sig(VideoConfAction::Accepted, "c1", "bob"), true, t0);
        assert!(has(&out, &CallCommand::Join("c1".into())));
        assert!(out.iter().any(
            |c| matches!(c, CallCommand::Publish { action: VideoConfAction::Confirmed, to, .. } if to == "bob")
        ));
        // The server's 40 s `end` for the accepted call is a no-op.
        let out = m.on_signal(sig(VideoConfAction::End, "c1", "server"), true, t0);
        assert!(out.is_empty());
    }

    #[test]
    fn caller_gives_up_after_attempt_limit() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        let start = StartInstructions::Direct {
            call_id: "c1".into(),
            callee_id: "bob".into(),
            provider_name: String::new(),
        };
        m.on_started("R", &start, t0);
        let mut t = t0;
        let mut calls = 1;
        let mut last = Vec::new();
        for _ in 0..20 {
            t += CALL_INTERVAL;
            last = m.tick(t);
            calls += last
                .iter()
                .filter(|c| {
                    matches!(
                        c,
                        CallCommand::Publish {
                            action: VideoConfAction::Call,
                            ..
                        }
                    )
                })
                .count();
            if m.outgoing().is_none() {
                break;
            }
        }
        assert_eq!(calls, CALL_ATTEMPT_LIMIT as usize);
        assert!(has(&last, &CallCommand::RestCancel("c1".into())));
        assert!(has(&last, &CallCommand::Notice(CallNotice::NoAnswer)));
        assert!(last.iter().any(|c| matches!(
            c,
            CallCommand::Publish {
                action: VideoConfAction::Canceled,
                ..
            }
        )));
    }

    #[test]
    fn rejected_cancels_unless_joined() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        let start = StartInstructions::Direct {
            call_id: "c1".into(),
            callee_id: "bob".into(),
            provider_name: String::new(),
        };
        m.on_started("R", &start, t0);
        let out = m.on_signal(sig(VideoConfAction::Rejected, "c1", "bob"), true, t0);
        assert!(has(&out, &CallCommand::RestCancel("c1".into())));
        assert!(has(&out, &CallCommand::Notice(CallNotice::Rejected)));
        assert!(!m.is_busy());
    }

    #[test]
    fn accept_timeout_and_end_during_accept() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t0);
        m.on_verified("c1", Ok(&info("c1", "bob")), t0);
        m.accept("c1", t0);
        let out = m.tick(t0 + ACCEPT_TIMEOUT);
        assert!(has(&out, &CallCommand::Notice(CallNotice::AcceptTimeout)));
        assert!(!m.is_active());

        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c2", "bob"), true, t0);
        m.on_verified("c2", Ok(&info("c2", "bob")), t0);
        m.accept("c2", t0);
        let out = m.on_signal(sig(VideoConfAction::End, "c2", "x"), true, t0);
        assert!(has(&out, &CallCommand::HideIncoming("c2".into())));
        // A late `confirmed` no longer joins.
        assert!(
            m.on_signal(sig(VideoConfAction::Confirmed, "c2", "bob"), true, t0)
                .is_empty()
        );
    }

    #[test]
    fn join_from_another_device_and_ring_timeout() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t0);
        m.on_verified("c1", Ok(&info("c1", "bob")), t0);
        let out = m.on_signal(sig(VideoConfAction::Join, "c1", "me"), true, t0);
        assert!(has(&out, &CallCommand::HideIncoming("c1".into())));
        // Dismissed: the caller's next `call` is ignored.
        assert!(
            m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t0)
                .is_empty()
        );

        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c3", "ann"), true, t0);
        m.on_verified("c3", Ok(&info("c3", "ann")), t0);
        // A refresh within the timeout keeps it ringing.
        m.on_signal(
            sig(VideoConfAction::Call, "c3", "ann"),
            true,
            t0 + Duration::from_secs(8),
        );
        assert!(m.tick(t0 + Duration::from_secs(12)).is_empty());
        let out = m.tick(t0 + Duration::from_secs(19));
        assert!(has(&out, &CallCommand::HideIncoming("c3".into())));
    }

    #[test]
    fn spoofed_signals_are_refused() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        // Unknown room, own uid, group ring without conference-window mode.
        assert!(
            m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), false, t0)
                .is_empty()
        );
        assert!(
            m.on_signal(sig(VideoConfAction::Call, "c1", "me"), true, t0)
                .is_empty()
        );
        assert!(
            m.on_signal(sig(VideoConfAction::Ring, "c1", "bob"), true, t0)
                .is_empty()
        );
        // The info does not confirm the claimed caller → dropped and dismissed.
        m.on_signal(sig(VideoConfAction::Call, "c1", "eve"), true, t0);
        let out = m.on_verified("c1", Ok(&info("c1", "bob")), t0);
        assert!(out.is_empty());
        assert!(!m.is_active());
        // A failed verification does not block the claimed sender.
        assert_eq!(
            m.on_signal(sig(VideoConfAction::Call, "c2", "eve"), true, t0),
            vec![CallCommand::FetchInfo("c2".into())]
        );
        m.on_verified("c2", Err(()), t0);
        let t1 = t0 + SENDER_COOLDOWN;
        let mut ended = info("c4", "eve");
        ended.ended_at = rocketchat::RcDate::from_millis(1);
        m.on_signal(sig(VideoConfAction::Call, "c4", "eve"), true, t1);
        assert!(m.on_verified("c4", Ok(&ended), t1).is_empty());
        m.on_signal(sig(VideoConfAction::Call, "c5", "zed"), true, t1);
        assert!(m.on_verified("c5", Err(()), t1).is_empty());
        // `canceled` from someone other than the caller is ignored.
        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c6", "bob"), true, t0);
        m.on_verified("c6", Ok(&info("c6", "bob")), t0);
        assert!(
            m.on_signal(sig(VideoConfAction::Canceled, "c6", "eve"), true, t0)
                .is_empty()
        );
        assert_eq!(m.incoming().count(), 1);
        let out = m.on_signal(sig(VideoConfAction::Canceled, "c6", "bob"), true, t0);
        assert!(has(&out, &CallCommand::StopRinger));
    }

    /// An unverified spoof claiming the real caller neither evicts the
    /// ringing call nor blocks its refreshes; unverified entries are capped.
    #[test]
    fn spoof_cannot_evict_or_suppress_a_verified_call() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "real", "bob"), true, t0);
        m.on_verified("real", Ok(&info("real", "bob")), t0);
        let out = m.on_signal(sig(VideoConfAction::Call, "fake", "bob"), true, t0);
        assert!(out.is_empty(), "within the verified caller's cooldown");
        let t1 = t0 + SENDER_COOLDOWN;
        let out = m.on_signal(sig(VideoConfAction::Call, "fake2", "bob"), true, t1);
        assert_eq!(out, vec![CallCommand::FetchInfo("fake2".into())]);
        assert_eq!(m.incoming().count(), 1, "real call still shown");
        let out = m.on_verified("fake2", Err(()), t1);
        assert!(!has(&out, &CallCommand::HideIncoming("real".into())));
        // The real caller's refresh still works.
        m.on_signal(sig(VideoConfAction::Call, "real", "bob"), true, t1);
        assert!(m.tick(t1 + Duration::from_secs(9)).is_empty());
        assert_eq!(m.incoming().next().unwrap().call_id, "real");
        // A verified newer call from bob replaces the older one.
        let t2 = t1 + SENDER_COOLDOWN;
        m.on_signal(sig(VideoConfAction::Call, "new", "bob"), true, t2);
        let out = m.on_verified("new", Ok(&info("new", "bob")), t2);
        assert!(has(&out, &CallCommand::HideIncoming("real".into())));
        assert_eq!(m.incoming().count(), 1);

        let mut m = CallManager::new("me");
        for i in 0..10 {
            m.on_signal(
                sig(VideoConfAction::Call, &format!("s{i}"), &format!("u{i}")),
                true,
                t0,
            );
        }
        let pending = (0..10)
            .filter(|i| m.incoming_call(&format!("s{i}")).is_some())
            .count();
        assert_eq!(pending, MAX_UNVERIFIED);
        m.drop_unverified("s0");
        assert!(m.incoming_call("s0").is_none());
        // Not dismissed: a re-send is verified again.
        assert_eq!(
            m.on_signal(sig(VideoConfAction::Call, "s0", "u0"), true, t0),
            vec![CallCommand::FetchInfo("s0".into())]
        );
    }

    #[test]
    fn dismissed_list_is_bounded() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        for i in 0..(MAX_DISMISSED + 50) {
            let id = format!("c{i}");
            m.on_signal(sig(VideoConfAction::Call, &id, &format!("u{i}")), true, t0);
            m.on_verified(&id, Err(()), t0);
        }
        assert!(m.dismissed.len() <= MAX_DISMISSED);
    }

    #[test]
    fn dismissal_expires() {
        let t0 = Instant::now();
        let mut m = CallManager::new("me");
        m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t0);
        m.on_verified("c1", Ok(&info("c1", "bob")), t0);
        m.mute("c1", t0);
        let t1 = t0 + DISMISS_TTL;
        m.tick(t1);
        let out = m.on_signal(sig(VideoConfAction::Call, "c1", "bob"), true, t1);
        assert_eq!(out, vec![CallCommand::FetchInfo("c1".into())]);
    }

    #[test]
    fn info_store() {
        let now = Instant::now();
        let mut s = VideoConfStore::default();
        s.want("c1");
        s.want("c2");
        assert_eq!(s.take_wanted(now), vec!["c1", "c2"]);
        s.want("c1");
        assert!(s.take_wanted(now).is_empty(), "in flight");
        s.finish("c1", Ok(info("c1", "bob")), now);
        let v1 = s.version("c1");
        s.finish("c2", Ok(info("other", "bob")), now);
        assert_eq!(s.state("c2"), Some(&InfoState::Failed), "wrong id refused");
        // Invalidation refetches without the view asking again.
        s.invalidate("c1");
        assert_eq!(s.take_wanted(now), vec!["c1"]);
        s.finish("c1", Err("x".into()), now);
        assert!(s.info("c1").is_some(), "failure keeps data");
        assert!(s.version("c1") > v1);
        let mut ended = info("c3", "bob");
        ended.ended_at = rocketchat::RcDate::from_millis(5);
        s.want("c3");
        s.take_wanted(now);
        s.finish("c3", Ok(ended), now);
        s.invalidate("c3");
        s.want("c3");
        assert!(
            !s.take_wanted(now + INFO_WINDOW).contains(&"c3".to_owned()),
            "ended calls are final"
        );
        s.set_capabilities(now, Err("no-videoconf-provider-app".into()));
        assert!(s.capabilities(now).is_some());
        assert!(s.capabilities(now + CAPABILITIES_TTL).is_none());
    }

    #[test]
    fn info_fetches_respect_the_rate_limit_and_retry() {
        let t0 = Instant::now();
        let mut s = VideoConfStore::default();
        for i in 0..20 {
            s.want(&format!("c{i:02}"));
        }
        let first = s.take_wanted(t0);
        assert_eq!(first.len(), INFO_RATE);
        assert!(s.take_wanted(t0).is_empty(), "budget used up");
        assert_eq!(s.next_wakeup(t0), Some(t0 + INFO_WINDOW));
        let rest = s.take_wanted(t0 + INFO_WINDOW);
        assert_eq!(rest.len(), 20 - INFO_RATE);
        assert_eq!(s.next_wakeup(t0 + INFO_WINDOW), None);
        // A failure (e.g. 429) is retried after a back-off, a few times.
        let t1 = t0 + INFO_WINDOW * 2;
        s.finish("c00", Err("429".into()), t1);
        assert_eq!(s.state("c00"), Some(&InfoState::Failed));
        assert!(s.take_wanted(t1).is_empty());
        assert_eq!(s.next_wakeup(t1), Some(t1 + INFO_RETRY[0]));
        let mut t = t1;
        for d in INFO_RETRY {
            t += d;
            assert_eq!(s.take_wanted(t), vec!["c00"]);
            s.finish("c00", Err("x".into()), t);
        }
        assert!(s.take_wanted(t + Duration::from_secs(3600)).is_empty());
        // A room event brings it back.
        s.invalidate("c00");
        assert_eq!(s.take_wanted(t), vec!["c00"]);
        s.finish("c00", Ok(info("c00", "bob")), t);
        assert!(s.info("c00").is_some());
    }
}
