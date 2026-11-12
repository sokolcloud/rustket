//! Syncs the account's flags with its hidden service room
//! (`rocketchat::models::service_room`, [`rustket_core::flags`]).
//!
//! Edits go into a pending overlay ([`FlagBlob`]) that the sidebar shows at
//! once and the app persists in the config (`pending_flags`), so offline
//! edits survive a restart. While live, the overlay is pushed after
//! [`COALESCE`] with [`rustket_core::flags::push`] over [`ServiceStore`]:
//! read the room's `customFields`, merge, write the whole object, read
//! again, at most [`PUSH_ATTEMPTS`] times. The first push creates the room
//! (a deterministic name, so a second device racing gets a duplicate-name
//! error and finds it) and hides it; a service room seen open again is
//! hidden again.
//!
//! The overlay shrinks only when the workspace's blob, which comes from the
//! subscriptions (the server copies the fields into them and notifies them
//! live), holds an entry at least as new: a push's own read does not prune
//! it, so a flag never flickers back while the echo is on its way. Another
//! device that read before this one's write and wrote after its last read
//! drops the entries silently; written entries are therefore watched for
//! [`VERIFY_AFTER`] and written again when the subscriptions stop showing
//! them.
//!
//! Nothing is ever unflagged automatically (a parent that is gone for this
//! device may be fine for another one).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::{FLAGS_KEY, Room, ServiceFields, service_room_name};
use rocketchat::{Error, RestClient};
use rustket_core::flags::{self, FlagBlob, FlagEdit, FlagError, FlagStore, PushError};
use rustket_core::workspace::ConnectionState;
use rustket_core::{Dirty, Workspace};

use crate::error::{Failure, Res};
use crate::session::SessionHandle;

/// Edits within this time go out in one write (a push makes one
/// `groups.setCustomFields` and one `subscriptions.getOne` call, each rate
/// limited per IP and route, by default to 10 a minute). The overlay shows
/// and persists edits at once, so waiting costs nothing.
pub const COALESCE: Duration = Duration::from_secs(5);
/// Read-merge-write rounds of one push before it counts as a conflict.
pub const PUSH_ATTEMPTS: usize = 4;
/// How long written entries are watched for being dropped by another
/// device's write.
pub const VERIFY_AFTER: Duration = Duration::from_secs(30);
/// Names tried for a new service room (another user may hold the first).
const NAME_ATTEMPTS: u32 = 3;
/// Tries of a push whose failure is not recognised before giving up.
const MAX_OTHER_FAILURES: u32 = 3;
/// Pause after a rate-limit response without a reset time.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10);
const MAX_PAUSE: Duration = Duration::from_secs(300);

/// Wait after `failures` (1-based) failed pushes: 2 s, doubling, at most
/// 5 min.
fn retry_delay(failures: u32) -> Duration {
    let secs = 2u64.saturating_mul(1 << failures.saturating_sub(1).min(8));
    Duration::from_secs(secs.min(300))
}

/// The service room as a [`FlagStore`]: the primary room, plus the user's
/// other service rooms (two devices created one each), whose blobs are
/// read and so folded into the primary on the next write.
///
/// Reads cost no `rooms.info` (rate limited per IP and route, and shared
/// with other screens): the first read of a room uses the fields its
/// subscription showed when the push started ([`Self::with_seeds`]), later
/// ones `subscriptions.getOne`, which the server updates together with the
/// room.
pub struct ServiceStore {
    rest: RestClient,
    uid: String,
    rid: Option<String>,
    extra: Vec<String>,
    /// Fields known without a request, by room: the primary's is used once,
    /// the others' for every read (only the primary is ever written).
    seeds: HashMap<String, ServiceFields>,
    /// The primary's fields as last read: keys this version does not know
    /// are written back.
    fields: Option<ServiceFields>,
}

impl ServiceStore {
    /// `rooms`: the user's service rooms, primary first (none yet: the first
    /// write creates one).
    pub fn new(rest: RestClient, uid: impl Into<String>, rooms: &[String]) -> Self {
        Self {
            rest,
            uid: uid.into(),
            rid: rooms.first().cloned(),
            extra: rooms.iter().skip(1).cloned().collect(),
            seeds: HashMap::new(),
            fields: None,
        }
    }

    /// Fields the subscriptions currently show for some of the rooms.
    pub fn with_seeds(mut self, seeds: impl IntoIterator<Item = (String, ServiceFields)>) -> Self {
        self.seeds.extend(seeds);
        self
    }

    /// The primary room (after a write: also one it created or found).
    pub fn rid(&self) -> Option<&str> {
        self.rid.as_deref()
    }

    fn adopt(&mut self, room: &Room) {
        self.rid = Some(room.id.clone());
        self.fields = room.service_fields();
    }

    /// Creates the service room holding `blob`, or finds the one another
    /// device just created. A found room is not written: the caller's next
    /// read shows what it holds.
    async fn create(&mut self, blob: &FlagBlob) -> Result<(), Error> {
        let mut fields = ServiceFields::new(&self.uid);
        fields.set(FLAGS_KEY, blob.to_value());
        for attempt in 0..NAME_ATTEMPTS {
            let name = service_room_name(&self.uid, attempt);
            match self.rest.create_service_room(&name, &fields).await {
                Ok(room) => {
                    tracing::debug!(rid = %room.id, "flags: service room created");
                    self.adopt(&room);
                    return Ok(());
                }
                Err(e) if e.is_duplicate_room_name() => {
                    // Not a member (someone else's room): not found.
                    match self.rest.rooms_info_by_name(&name).await {
                        Ok(room) if room.is_service_room_of(&self.uid) => {
                            tracing::debug!(rid = %room.id, "flags: service room found");
                            self.adopt(&room);
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(e) if e.is_transient() => return Err(e),
                        Err(_) => {}
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::Protocol("no free name for the service room".into()))
    }
}

impl FlagStore for ServiceStore {
    type Error = Error;

    async fn read(&mut self) -> Result<FlagBlob, Error> {
        let mut blob = FlagBlob::default();
        if let Some(rid) = self.rid.clone() {
            let read = match self.seeds.remove(&rid) {
                Some(f) => Some(Some(f)),
                None => self.rest.read_service_subscription(&rid).await?,
            };
            match read {
                Some(Some(f)) => {
                    blob.merge(&FlagBlob::from_value(f.get(FLAGS_KEY)));
                    self.fields = Some(f);
                }
                Some(None) => {
                    return Err(Error::Protocol("the service room lost its marker".into()));
                }
                // Deleted (e.g. from another client): a write makes a new one.
                None => {
                    tracing::debug!(%rid, "flags: service room gone");
                    self.rid = None;
                    self.fields = None;
                }
            }
        }
        for rid in &self.extra {
            if let Some(f) = self.seeds.get(rid) {
                blob.merge(&FlagBlob::from_value(f.get(FLAGS_KEY)));
                continue;
            }
            match self.rest.read_service_subscription(rid).await {
                Ok(f) => {
                    if let Some(f) = f.flatten() {
                        blob.merge(&FlagBlob::from_value(f.get(FLAGS_KEY)));
                        self.seeds.insert(rid.clone(), f);
                    }
                }
                Err(e) if e.is_transient() => return Err(e),
                Err(e) => tracing::debug!(%rid, error = %e, "flags: extra service room unreadable"),
            }
        }
        Ok(blob)
    }

    async fn write(&mut self, blob: &FlagBlob) -> Result<(), Error> {
        let (Some(rid), Some(mut fields)) = (self.rid.clone(), self.fields.take()) else {
            return self.create(blob).await;
        };
        fields.set(FLAGS_KEY, blob.to_value());
        self.fields = Some(self.rest.write_service_fields(&rid, &fields).await?);
        Ok(())
    }
}

/// Why a push failed.
#[derive(Debug, Clone)]
pub enum PushFailure {
    Store(Failure),
    /// Other devices kept replacing the blob.
    Conflict,
}

/// A finished push.
#[derive(Debug, Clone)]
pub struct Pushed {
    /// What was pushed.
    pub written: FlagBlob,
    /// The store's blob after the push (covers `written`).
    pub stored: FlagBlob,
    /// The primary service room.
    pub rid: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Pushed {
        epoch: u64,
        res: Result<Pushed, PushFailure>,
    },
    Hidden {
        epoch: u64,
        rid: String,
        res: Res<()>,
    },
    /// The pacing timer set for `at` fired.
    Pace { at: Instant },
}

/// Requests to the app.
#[derive(Debug, Clone)]
pub enum Out {
    /// Pushing was given up (until the next edit or session): tell the user.
    Failed(Failure),
}

/// Sync state, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing pending.
    Synced,
    /// Edits wait to be stored (offline, or on their way).
    Pending,
    /// Pushing was given up: the error. Edits stay pending and are shown.
    Failed(String),
}

/// What [`FlagSync::plan`] wants.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    Push,
    Wait(Instant),
    Idle,
}

/// Per-server flag sync (`ServerState::flags`).
#[derive(Debug, Default)]
pub struct FlagSync {
    /// The session the state belongs to (`Arc` address).
    session: Option<usize>,
    /// Bumped with every new session: older results are dropped.
    epoch: u64,
    /// The account whose overlay is loaded.
    account: Option<String>,
    /// Edits the subscriptions do not show yet.
    overlay: FlagBlob,
    /// Bumped with every overlay change; `applied` is the one the workspace
    /// has, `saved` the one the config has.
    rev: u64,
    applied: Option<u64>,
    saved: u64,
    /// `Dirty::FLAGS` revision the overlay was last pruned against.
    blob_rev: Option<u64>,
    /// The store's blob as the last push left it: the overlay needs a push
    /// while this does not cover it.
    confirmed: FlagBlob,
    /// Not before this (coalescing edits).
    due: Option<Instant>,
    in_flight: bool,
    failures: u32,
    retry_at: Option<Instant>,
    /// Given up with this error, until the next edit or session.
    failed: Option<String>,
    /// A service room the last push created or found (the subscription may
    /// not have arrived yet).
    created: Option<String>,
    /// Entries written recently, watched until `verify_at`.
    recent: FlagBlob,
    /// The subscriptions showed `recent` since it was written.
    recent_seen: bool,
    verify_at: Option<Instant>,
    /// Service rooms a hide was sent for in this session (a failed one is
    /// sent again).
    hide_sent: HashSet<String>,
    /// Failed hides: failures so far, and not before when to send again.
    hide_retry: HashMap<String, (u32, Instant)>,
    timer: Option<Instant>,
}

impl FlagSync {
    /// Loads the pending edits of the signed-in account (`uid`) when it
    /// changed; `load` reads them from the config.
    pub fn follow_account(&mut self, uid: Option<&str>, load: impl FnOnce(&str) -> FlagBlob) {
        if self.account.as_deref() == uid {
            return;
        }
        self.overlay = uid.map(load).unwrap_or_default();
        self.account = uid.map(str::to_owned);
        self.rev += 1;
        self.saved = self.rev;
        self.applied = None;
        self.blob_rev = None;
        self.confirmed = FlagBlob::default();
        self.recent = FlagBlob::default();
        self.recent_seen = false;
        self.verify_at = None;
        self.failed = None;
        self.failures = 0;
        self.retry_at = None;
        self.due = None;
    }

    /// The pending edits.
    pub fn overlay(&self) -> &FlagBlob {
        &self.overlay
    }

    pub fn status(&self) -> Status {
        if let Some(e) = &self.failed {
            Status::Failed(e.clone())
        } else if self.overlay.is_empty() {
            Status::Synced
        } else {
            Status::Pending
        }
    }

    /// Records a user's edit: shown at once, pushed after [`COALESCE`].
    /// Returns whether anything changed (`false` also while signed out).
    pub fn edit(
        &mut self,
        ws: &mut Workspace,
        edit: &FlagEdit,
        device: &str,
        now: Instant,
    ) -> Result<bool, FlagError> {
        if self.account.is_none() {
            return Ok(false);
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        if !self.overlay.edit(ws.flag_blob(), edit, now_ms, device)? {
            return Ok(false);
        }
        self.rev += 1;
        self.due = Some(now + COALESCE);
        self.failed = None;
        self.failures = 0;
        self.retry_at = None;
        self.apply(ws);
        Ok(true)
    }

    /// The overlay to persist for its account, when it changed since the
    /// last call.
    pub fn take_save(&mut self) -> Option<(String, FlagBlob)> {
        if self.saved == self.rev {
            return None;
        }
        self.saved = self.rev;
        Some((self.account.clone()?, self.overlay.clone()))
    }

    /// Logout: everything goes.
    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch + 1,
            ..Self::default()
        };
    }

    fn reset_session(&mut self) {
        self.epoch += 1;
        self.in_flight = false;
        self.timer = None;
        self.created = None;
        self.hide_sent.clear();
        self.hide_retry.clear();
        self.failed = None;
        self.failures = 0;
        self.retry_at = None;
        // The next session reads the store again.
        self.confirmed = FlagBlob::default();
    }

    fn apply(&mut self, ws: &mut Workspace) {
        if self.applied != Some(self.rev) {
            ws.set_flag_overlay(self.overlay.clone());
            self.applied = Some(self.rev);
        }
    }

    /// Whether this device can push now: live, and signed in as the
    /// overlay's account.
    fn live(&self, ws: &Workspace) -> bool {
        !ws.from_cache()
            && *ws.connection() == ConnectionState::Connected
            && self.account.is_some()
            && ws.me().map(|m| m.id.as_str()) == self.account.as_deref()
    }

    /// Follows the workspace's blob: prunes the overlay and checks recently
    /// written entries.
    fn observe(&mut self, ws: &Workspace, now: Instant) {
        let rev = ws.revision(Dirty::FLAGS);
        if self.blob_rev != Some(rev) {
            self.blob_rev = Some(rev);
            let stored = ws.flag_blob();
            if self.overlay.prune_covered(stored) {
                self.rev += 1;
            }
            if !self.recent.is_empty() {
                if stored.covers(&self.recent) {
                    self.recent_seen = true;
                } else if self.recent_seen {
                    // Shown, then gone: another device's write dropped it.
                    self.verify_at = Some(now);
                }
            }
        }
        if self.verify_at.is_some_and(|t| now >= t) && self.live(ws) {
            self.verify_at = None;
            let lost = std::mem::take(&mut self.recent).uncovered_by(ws.flag_blob());
            self.recent_seen = false;
            if !lost.is_empty() {
                tracing::debug!(
                    entries = lost.len(),
                    "flags: written entries were dropped, writing again"
                );
                self.overlay.merge(&lost);
                self.rev += 1;
                self.confirmed = FlagBlob::default();
            }
        }
    }

    /// Whether the overlay has entries to push, and when.
    fn plan(&self, ws: &Workspace, now: Instant) -> Next {
        if self.in_flight
            || self.failed.is_some()
            || self.overlay.is_empty()
            || self.confirmed.covers(&self.overlay)
            || !self.live(ws)
        {
            return Next::Idle;
        }
        let at = self.due.into_iter().chain(self.retry_at).max();
        match at {
            Some(at) if now < at => Next::Wait(at),
            _ => Next::Push,
        }
    }

    /// The pending times the planner waits for, for the pacing timer.
    fn next_wake(&self, ws: &Workspace, now: Instant) -> Option<Instant> {
        let push = match self.plan(ws, now) {
            Next::Wait(at) => Some(at),
            _ => None,
        };
        let hide = self
            .hide_retry
            .values()
            .map(|&(_, at)| at)
            .filter(|&at| at > now)
            .min();
        push.into_iter()
            .chain(self.verify_at.filter(|_| self.live(ws)))
            .chain(hide.filter(|_| self.live(ws)))
            .min()
    }

    /// Per update: follows the session, prunes and applies the overlay,
    /// hides service rooms seen open and starts a push when one is due.
    pub fn tick(
        &mut self,
        ws: &mut Workspace,
        session: Option<&SessionHandle>,
        now: Instant,
    ) -> Task<Msg> {
        let ptr = session.map(|h| Arc::as_ptr(&h.0) as usize);
        if self.session != ptr {
            self.reset_session();
            self.session = ptr;
        }
        self.observe(ws, now);
        self.apply(ws);
        let Some(h) = session else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        if self.live(ws) {
            tasks.extend(self.hide_open(ws, h, now));
        }
        match self.plan(ws, now) {
            Next::Push => {
                self.in_flight = true;
                self.due = None;
                tasks.push(self.push_task(ws, h));
            }
            Next::Wait(_) | Next::Idle => {}
        }
        if let Some(at) = self.next_wake(ws, now) {
            tasks.push(self.schedule(at, now));
        }
        Task::batch(tasks)
    }

    fn hide_open(&mut self, ws: &Workspace, h: &SessionHandle, now: Instant) -> Vec<Task<Msg>> {
        let mut tasks = Vec::new();
        for rid in ws.service_rooms() {
            let open = ws
                .room(rid)
                .and_then(|e| e.sub.as_ref())
                .is_some_and(|s| s.open);
            let waiting = self.hide_retry.get(rid).is_some_and(|&(_, at)| now < at);
            if !open || waiting || !self.hide_sent.insert(rid.clone()) {
                continue;
            }
            let (rest, epoch, rid) = (h.rest().clone(), self.epoch, rid.clone());
            tasks.push(Task::perform(
                {
                    let rid = rid.clone();
                    async move { rest.hide_service_room(&rid).await.map_err(Failure::from) }
                },
                move |res| Msg::Hidden {
                    epoch,
                    rid: rid.clone(),
                    res,
                },
            ));
        }
        tasks
    }

    fn push_task(&self, ws: &Workspace, h: &SessionHandle) -> Task<Msg> {
        let mut rooms = ws.service_rooms().to_vec();
        if rooms.is_empty() {
            rooms.extend(self.created.clone());
        }
        let uid = self.account.clone().unwrap_or_default();
        let seeds = rooms.iter().filter_map(|rid| {
            let f = ws.room(rid)?.sub.as_ref()?.service_fields()?;
            Some((rid.clone(), f))
        });
        let seeds: Vec<_> = seeds.collect();
        let mut store = ServiceStore::new(h.rest().clone(), uid, &rooms).with_seeds(seeds);
        let pending = self.overlay.clone();
        let epoch = self.epoch;
        Task::perform(
            async move {
                let res = flags::push(&mut store, &pending, PUSH_ATTEMPTS).await;
                match res {
                    Ok(stored) => Ok(Pushed {
                        written: pending,
                        stored,
                        rid: store.rid().map(str::to_owned),
                    }),
                    Err(PushError::Store(e)) => Err(PushFailure::Store(Failure::from(e))),
                    Err(PushError::Conflict) => Err(PushFailure::Conflict),
                }
            },
            move |res| Msg::Pushed { epoch, res },
        )
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

    /// Applies a result.
    pub fn update(&mut self, msg: Msg, now: Instant) -> Vec<Out> {
        match msg {
            Msg::Pace { at } => {
                if self.timer == Some(at) {
                    self.timer = None;
                }
                Vec::new()
            }
            Msg::Hidden { epoch, rid, res } => {
                if epoch != self.epoch {
                    return Vec::new();
                }
                match res {
                    Ok(()) => {
                        self.hide_retry.remove(&rid);
                    }
                    Err(f) => {
                        let failures = self.hide_retry.get(&rid).map_or(0, |&(n, _)| n) + 1;
                        let wait = match f.error() {
                            Error::RateLimited { retry_after, .. } => {
                                retry_after.unwrap_or(RATE_LIMIT_PAUSE)
                            }
                            _ => retry_delay(failures),
                        }
                        .clamp(Duration::from_secs(1), MAX_PAUSE);
                        tracing::warn!(%rid, error = %f, "flags: could not hide the service room");
                        self.hide_sent.remove(&rid);
                        self.hide_retry.insert(rid, (failures, now + wait));
                    }
                }
                Vec::new()
            }
            Msg::Pushed { epoch, res } => {
                if epoch != self.epoch {
                    return Vec::new();
                }
                self.in_flight = false;
                match res {
                    Ok(p) => {
                        self.failures = 0;
                        self.retry_at = None;
                        if p.rid.is_some() {
                            self.created = p.rid;
                        }
                        self.confirmed = p.stored;
                        self.recent.merge(&p.written);
                        self.recent_seen = false;
                        self.verify_at = Some(now + VERIFY_AFTER);
                        // The subscriptions' echo prunes the overlay.
                        self.blob_rev = None;
                        Vec::new()
                    }
                    Err(PushFailure::Conflict) => {
                        tracing::debug!("flags: push lost to concurrent writes, retrying");
                        self.retry(now, None);
                        Vec::new()
                    }
                    Err(PushFailure::Store(f)) => self.failed_push(f, now),
                }
            }
        }
    }

    fn retry(&mut self, now: Instant, wait: Option<Duration>) {
        self.failures += 1;
        self.retry_at = Some(now + wait.unwrap_or_else(|| retry_delay(self.failures)));
    }

    fn failed_push(&mut self, f: Failure, now: Instant) -> Vec<Out> {
        let e = f.error();
        let give_up = match e {
            Error::RateLimited { retry_after, .. } => {
                let wait = retry_after
                    .unwrap_or(RATE_LIMIT_PAUSE)
                    .clamp(Duration::from_secs(1), MAX_PAUSE);
                tracing::debug!(error = %f, "flags: rate limited");
                // Refused before it ran: not a failure of its own.
                self.retry_at = Some(now + wait);
                return Vec::new();
            }
            e if e.is_transient() || matches!(e, Error::Http(_) | Error::Io(_)) => false,
            e if e.is_unauthorized()
                || matches!(
                    e.api_error_code(),
                    Some("error-not-allowed" | "error-action-not-allowed" | "unauthorized")
                ) =>
            {
                true
            }
            _ => self.failures + 1 >= MAX_OTHER_FAILURES,
        };
        if give_up {
            tracing::warn!(error = %f, "flags: cannot store flags on the server");
            self.failed = Some(f.user_text());
            self.retry_at = None;
            return vec![Out::Failed(f)];
        }
        tracing::debug!(error = %f, failures = self.failures + 1, "flags: push failed, retrying");
        self.retry(now, None);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::{RoomType, Subscription, User, UserRef};
    use rustket_core::Change;
    use rustket_core::flags::FlagColor;
    use serde_json::json;

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        ws.apply_connection(ConnectionState::Connected);
        ws
    }

    fn service_sub(blob: &FlagBlob, open: bool) -> Subscription {
        let mut f = ServiceFields::new("me");
        f.set(FLAGS_KEY, blob.to_value());
        Subscription {
            id: "sub-svc".into(),
            rid: "svc".into(),
            name: "rustket-sync-me".into(),
            t: RoomType::Private,
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            open,
            roles: Some(vec!["owner".into()]),
            custom_fields: Some(f.to_custom_fields()),
            ts: RcDate::from_millis(1),
            ..Default::default()
        }
    }

    /// The service room of [`service_sub`]: the user's own.
    fn service_room() -> Room {
        Room {
            id: "svc".into(),
            t: RoomType::Private,
            u: Some(UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            }),
            users_count: Some(1),
            custom_fields: Some(ServiceFields::new("me").to_custom_fields()),
            ..Default::default()
        }
    }

    fn room(rid: &str, c: Option<FlagColor>) -> FlagEdit {
        FlagEdit::Room {
            rid: rid.into(),
            color: c,
        }
    }

    fn synced(ws: &Workspace) -> FlagSync {
        let mut s = FlagSync::default();
        s.follow_account(ws.me().map(|m| m.id.as_str()), |_| FlagBlob::default());
        s
    }

    fn pushed(s: &mut FlagSync, written: &FlagBlob, stored: &FlagBlob, now: Instant) {
        let epoch = s.epoch;
        s.in_flight = true;
        s.due = None;
        s.update(
            Msg::Pushed {
                epoch,
                res: Ok(Pushed {
                    written: written.clone(),
                    stored: stored.clone(),
                    rid: Some("svc".into()),
                }),
            },
            now,
        );
    }

    #[test]
    fn edits_show_at_once_and_push_after_coalescing() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        assert_eq!(s.plan(&ws, t0), Next::Idle);
        assert!(
            s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
                .unwrap()
        );
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        assert!(
            !s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
                .unwrap()
        );
        assert_eq!(s.status(), Status::Pending);
        let (uid, saved) = s.take_save().unwrap();
        assert_eq!((uid.as_str(), &saved), ("me", s.overlay()));
        assert!(s.take_save().is_none());
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + COALESCE));
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
        // Not while showing the offline cache or disconnected.
        ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Idle);
    }

    #[test]
    fn the_overlay_leaves_only_with_the_subscription_echo() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        s.observe(&ws, t0);
        // The push's own read does not prune: no flicker before the echo.
        assert_eq!(s.overlay(), &written);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Idle, "already stored");
        ws.apply_subscription(Change::Inserted, service_sub(&written, false));
        ws.apply_room(Change::Inserted, service_room());
        s.observe(&ws, t0);
        s.apply(&mut ws);
        assert!(s.overlay().is_empty());
        assert_eq!(s.status(), Status::Synced);
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        assert!(s.take_save().is_some_and(|(_, b)| b.is_empty()));
    }

    #[test]
    fn entries_dropped_by_another_device_are_written_again() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        ws.apply_subscription(Change::Inserted, service_sub(&written, false));
        ws.apply_room(Change::Inserted, service_room());
        s.observe(&ws, t0);
        assert!(s.overlay().is_empty());
        // Another device that read before our write replaces the blob.
        let mut theirs = FlagBlob::default();
        theirs
            .edit(
                &FlagBlob::default(),
                &room("r2", Some(FlagColor::Blue)),
                1,
                "other",
            )
            .unwrap();
        ws.apply_subscription(Change::Updated, service_sub(&theirs, false));
        s.observe(&ws, t0 + COALESCE);
        assert_eq!(s.overlay(), &written);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
    }

    #[test]
    fn entries_never_shown_are_checked_after_the_window() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        s.observe(&ws, t0 + VERIFY_AFTER / 2);
        assert_eq!(s.plan(&ws, t0 + VERIFY_AFTER / 2), Next::Idle);
        assert_eq!(s.next_wake(&ws, t0), Some(t0 + VERIFY_AFTER));
        s.observe(&ws, t0 + VERIFY_AFTER);
        assert_eq!(
            s.plan(&ws, t0 + VERIFY_AFTER),
            Next::Push,
            "read the store again"
        );
    }

    #[test]
    fn failures_back_off_and_give_up() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let fail = |s: &mut FlagSync, e: Error| {
            s.in_flight = true;
            s.due = None;
            let epoch = s.epoch;
            s.update(
                Msg::Pushed {
                    epoch,
                    res: Err(PushFailure::Store(Failure::from(e))),
                },
                t0,
            )
        };
        assert!(fail(&mut s, Error::Timeout("push")).is_empty());
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + retry_delay(1)));
        s.update(
            Msg::Pushed {
                epoch: s.epoch,
                res: Err(PushFailure::Conflict),
            },
            t0,
        );
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + retry_delay(2)));
        // A stale result of an earlier session changes nothing.
        s.update(
            Msg::Pushed {
                epoch: s.epoch.wrapping_sub(1),
                res: Err(PushFailure::Conflict),
            },
            t0,
        );
        assert_eq!(s.failures, 2);
        let denied = Error::from_response(
            400,
            None,
            br#"{"success":false,"error":"Not allowed [error-action-not-allowed]","errorType":"error-action-not-allowed"}"#,
        );
        let out = fail(&mut s, denied);
        assert!(matches!(out.as_slice(), [Out::Failed(_)]));
        assert!(matches!(s.status(), Status::Failed(_)));
        assert_eq!(s.plan(&ws, t0 + MAX_PAUSE), Next::Idle);
        // The edits stay shown; the next edit tries again.
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        s.edit(&mut ws, &room("r2", Some(FlagColor::Blue)), "dev", t0)
            .unwrap();
        assert_eq!(s.status(), Status::Pending);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
    }

    #[test]
    fn failed_hides_are_sent_again() {
        let mut ws = ws();
        ws.apply_subscription(Change::Inserted, service_sub(&FlagBlob::default(), true));
        ws.apply_room(Change::Inserted, service_room());
        assert_eq!(ws.service_rooms(), ["svc"]);
        let mut s = synced(&ws);
        let t0 = Instant::now();
        let hidden = |s: &mut FlagSync, res: Res<()>, now: Instant| {
            s.hide_sent.insert("svc".into());
            let epoch = s.epoch;
            s.update(
                Msg::Hidden {
                    epoch,
                    rid: "svc".into(),
                    res,
                },
                now,
            );
        };
        let limited = Error::RateLimited {
            retry_after: Some(Duration::from_secs(40)),
            message: None,
        };
        hidden(&mut s, Err(Failure::from(limited)), t0);
        assert!(!s.hide_sent.contains("svc"));
        let at = t0 + Duration::from_secs(40);
        assert_eq!(s.hide_retry.get("svc"), Some(&(1, at)));
        assert_eq!(s.next_wake(&ws, t0), Some(at));
        hidden(&mut s, Err(Failure::from(Error::Timeout("hide"))), at);
        assert_eq!(s.hide_retry.get("svc"), Some(&(2, at + retry_delay(2))));
        hidden(&mut s, Ok(()), at + retry_delay(2));
        assert!(s.hide_retry.is_empty() && s.hide_sent.contains("svc"));
        assert_eq!(s.next_wake(&ws, at + retry_delay(2)), None);
    }

    #[test]
    fn accounts_keep_their_own_pending_edits() {
        let mut ws = ws();
        let mut s = FlagSync::default();
        let mut stored = FlagBlob::default();
        stored
            .edit(
                &FlagBlob::default(),
                &room("r1", Some(FlagColor::Green)),
                1,
                "d",
            )
            .unwrap();
        s.follow_account(Some("me"), |uid| {
            assert_eq!(uid, "me");
            stored.clone()
        });
        assert!(s.take_save().is_none(), "loaded, not changed");
        s.apply(&mut ws);
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Green));
        // Same account: not reloaded.
        s.follow_account(Some("me"), |_| unreachable!());
        // Signed in as someone else than the overlay's account: no push.
        s.follow_account(Some("other"), |_| stored.clone());
        assert_eq!(s.plan(&ws, Instant::now()), Next::Idle);
        s.follow_account(None, |_| unreachable!());
        assert!(
            !s.edit(
                &mut ws,
                &room("r9", Some(FlagColor::Red)),
                "d",
                Instant::now()
            )
            .unwrap()
        );
    }

    #[test]
    fn service_store_payloads() {
        // The blob goes under its key next to the marker and unknown keys.
        let mut f = ServiceFields::parse(Some(&json!({
            "rustketService": { "v": 1, "owner": "me" },
            "other": 1
        })))
        .unwrap();
        let mut b = FlagBlob::default();
        b.edit(
            &FlagBlob::default(),
            &room("r1", Some(FlagColor::Red)),
            7,
            "d",
        )
        .unwrap();
        f.set(FLAGS_KEY, b.to_value());
        let v = f.to_custom_fields();
        assert_eq!(v["other"], 1);
        assert_eq!(v["rustketService"]["owner"], "me");
        assert_eq!(
            FlagBlob::from_value(v.get(FLAGS_KEY)).to_value(),
            b.to_value()
        );
        assert!(retry_delay(1) < retry_delay(2));
        assert_eq!(retry_delay(50), Duration::from_secs(300));
    }
}
