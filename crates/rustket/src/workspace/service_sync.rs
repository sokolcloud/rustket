//! Syncs the account's blobs (flags, and the keys later features add) with
//! its hidden service room (`rocketchat::models::service_room`,
//! [`rustket_core::service_sync`]).
//!
//! Every synced key has a [`BlobSync`]: edits go into a pending overlay that
//! the workspace shows at once and the app persists in the config, so
//! offline edits survive a restart. While live, a key's overlay is due
//! [`COALESCE`] after its last edit. The server's keys are then written
//! together ([`take_parts`], [`push_task`]): one [`rustket_core::service_sync::push_all`]
//! over [`ServiceStore`] reads the room's `customFields`, merges every key
//! that has pending entries, writes the whole object and reads again, at
//! most [`PUSH_ATTEMPTS`] times. Pushes of one server hold its lock
//! ([`Shared`]) and remember what they wrote, so a push never starts from
//! fields older than this client's own last write. The first push creates
//! the room (a deterministic name, so a second device racing gets a
//! duplicate-name error and finds it) and binds it to the account
//! (`ServiceBinding`: a room is only trusted when the user's own preferences
//! name it); hiding it is a flags duty (`super::flags`).
//!
//! An overlay shrinks only when the workspace's blob, which comes from the
//! subscriptions (the server copies the fields into them and notifies them
//! live), holds an entry at least as new: a push's own read does not prune
//! it, so an edit never flickers back while the echo is on its way. Another
//! device that read before this one's write and wrote after its last read
//! drops entries silently; written entries are therefore watched for
//! [`VERIFY_AFTER`] and written again when the subscriptions stop showing
//! them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::{
    FLAGS_KEY, LATER_KEY, MENTION_GROUPS_KEY, REMINDERS_KEY, Room, ServiceBinding, ServiceFields,
    service_room_name,
};
use rocketchat::{Error, RestClient};
use rustket_core::flags::{FlagBlob, FlagStore};
use rustket_core::later::LaterBlob;
use rustket_core::mention_groups::MentionGroupBlob;
use rustket_core::reminders::ReminderBlob;
use rustket_core::service_sync::{
    self as core, EditBlob, FieldsStore, KeyCheck, LastWrite, Part, Remembering, SyncBlob,
};
use rustket_core::workspace::ConnectionState;
use rustket_core::{Dirty, Workspace};
use serde_json::Value;

use crate::error::Failure;
use crate::session::SessionHandle;

/// Edits within this time go out in one write (a push makes one
/// `groups.setCustomFields` and one `subscriptions.getOne` call, each rate
/// limited per IP and route, by default to 10 a minute). The overlay shows
/// and persists edits at once, so waiting costs nothing.
pub const COALESCE: Duration = Duration::from_secs(5);
/// Minimum time between the starts of two pushes of a server: steady
/// edits (triaging Later item by item) stay well under both routes'
/// limits, and every device gets the whole fields object less often.
pub const PUSH_SPACING: Duration = Duration::from_secs(20);
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
pub(super) const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(10);
pub(super) const MAX_PAUSE: Duration = Duration::from_secs(300);

/// Wait after `failures` (1-based) failed pushes: 2 s, doubling, at most
/// 5 min.
pub(super) fn retry_delay(failures: u32) -> Duration {
    let secs = 2u64.saturating_mul(1 << failures.saturating_sub(1).min(8));
    Duration::from_secs(secs.min(300))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The service room as a [`FieldsStore`]: the primary room, plus the user's
/// other service rooms (two devices created one each), whose blobs are
/// read and so folded into the primary on the next write.
///
/// Reads cost no `rooms.info` (rate limited per IP and route, and shared
/// with other screens): the first read of a room uses the fields its
/// subscription showed when the push started ([`Self::with_seeds`]) unless
/// they predate this client's last write ([`Remembering`]), later ones
/// `subscriptions.getOne`, which the server updates together with the room.
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
    /// The account's binding as the workspace knew it when the push started.
    binding: ServiceBinding,
    /// The rooms were checked against the account's current binding.
    vetted: bool,
    /// What this client last wrote to the server's service fields.
    last: LastWrite,
    /// The synced keys, to tell whether a seed shows `last`.
    keys: Vec<KeyCheck>,
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
            binding: ServiceBinding::default(),
            vetted: false,
            last: LastWrite::default(),
            keys: key_checks(),
        }
    }

    /// The account's binding as the workspace shows it. Rooms it names are
    /// used as they are; any other is checked against a fresh `/me` first.
    pub fn with_binding(mut self, binding: ServiceBinding) -> Self {
        self.binding = binding;
        self
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

    /// Checks rooms the workspace's binding does not name, once per store,
    /// before anything is read from them. They are rooms from before
    /// bindings existed (the account has none: they are bound now) or rooms
    /// bound since the workspace last read `/me`; a room the account's
    /// current binding does not name is dropped.
    async fn vet(&mut self) -> Result<(), Error> {
        if self.vetted {
            return Ok(());
        }
        let rids: Vec<String> = self.rid.iter().chain(&self.extra).cloned().collect();
        if rids.iter().any(|r| !self.binding.contains(r)) {
            let fresh = self.rest.read_service_binding().await?;
            if fresh.is_empty() {
                self.binding = self.rest.bind_service_rooms(&rids).await?;
                tracing::debug!(rooms = rids.len(), "service sync: service rooms bound");
            } else {
                if self.rid.as_ref().is_some_and(|r| !fresh.contains(r)) {
                    tracing::warn!("service sync: the service room is not bound to the account");
                    self.rid = None;
                    self.fields = None;
                }
                self.extra.retain(|r| fresh.contains(r));
                self.seeds.retain(|r, _| fresh.contains(r));
                if self.rid.is_none() && !self.extra.is_empty() {
                    self.rid = Some(self.extra.remove(0));
                }
                self.binding = fresh;
            }
        }
        self.vetted = true;
        Ok(())
    }

    /// Creates the service room holding `values`, or finds the one another
    /// device just created. A found room is not written: the caller's next
    /// read shows what it holds.
    async fn create(&mut self, values: &[(&'static str, Value)]) -> Result<(), Error> {
        let mut fields = ServiceFields::new(&self.uid);
        for (key, value) in values {
            fields.set(key, value.clone());
        }
        for attempt in 0..NAME_ATTEMPTS {
            let name = service_room_name(&self.uid, attempt);
            match self.rest.create_service_room(&name, &fields).await {
                Ok(room) => {
                    tracing::debug!(rid = %room.id, "service sync: service room created");
                    let bound = self
                        .rest
                        .bind_service_rooms(std::slice::from_ref(&room.id))
                        .await;
                    match bound {
                        Ok(binding) => self.binding = binding,
                        Err(e) => {
                            // Unbound it would never be trusted and would
                            // keep its name taken.
                            if let Err(e) = self.rest.rooms_delete(&room.id).await {
                                tracing::warn!(rid = %room.id, error = %e, "service sync: unbound room kept");
                            }
                            return Err(e);
                        }
                    }
                    self.adopt(&room);
                    self.last = match &self.fields {
                        Some(f) => LastWrite::Wrote {
                            rid: room.id.clone(),
                            fields: f.clone(),
                        },
                        None => LastWrite::Unknown,
                    };
                    return Ok(());
                }
                Err(e) if e.is_duplicate_room_name() => {
                    // Not a member (someone else's room): not found.
                    match self.rest.rooms_info_by_name(&name).await {
                        Ok(room) if room.is_service_room_of(&self.uid) => {
                            // A room with this name and shape may have been
                            // created in the user's name by someone else.
                            let fresh = self.rest.read_service_binding().await?;
                            if fresh.trusts(&room) {
                                tracing::debug!(rid = %room.id, "service sync: service room found");
                                self.binding = if fresh.contains(&room.id) {
                                    fresh
                                } else {
                                    self.rest
                                        .bind_service_rooms(std::slice::from_ref(&room.id))
                                        .await?
                                };
                                self.adopt(&room);
                                return Ok(());
                            }
                            tracing::warn!(rid = %room.id, "service sync: unbound room holds the name");
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

impl FieldsStore for ServiceStore {
    type Error = Error;

    async fn read(&mut self) -> Result<Vec<ServiceFields>, Error> {
        self.vet().await?;
        let mut copies = Vec::new();
        if let Some(rid) = self.rid.clone() {
            let now = now_ms();
            let seed = self
                .seeds
                .remove(&rid)
                .filter(|f| self.last.trusts(&rid, f, &self.keys, now));
            let read = match seed {
                Some(f) => Some(Some(f)),
                None => self.rest.read_service_subscription(&rid).await?,
            };
            match read {
                Some(Some(f)) => {
                    copies.push(f.clone());
                    self.fields = Some(f);
                }
                Some(None) => {
                    return Err(Error::Protocol("the service room lost its marker".into()));
                }
                // Deleted (e.g. from another client): a write makes a new one.
                None => {
                    tracing::debug!(%rid, "service sync: service room gone");
                    self.rid = None;
                    self.fields = None;
                }
            }
        }
        for rid in &self.extra {
            if let Some(f) = self.seeds.get(rid) {
                copies.push(f.clone());
                continue;
            }
            match self.rest.read_service_subscription(rid).await {
                Ok(f) => {
                    if let Some(f) = f.flatten() {
                        copies.push(f.clone());
                        self.seeds.insert(rid.clone(), f);
                    }
                }
                Err(e) if e.is_transient() => return Err(e),
                Err(e) => {
                    tracing::debug!(%rid, error = %e, "service sync: extra service room unreadable")
                }
            }
        }
        Ok(copies)
    }

    async fn write(&mut self, values: &[(&'static str, Value)]) -> Result<(), Error> {
        self.vet().await?;
        let (Some(rid), Some(mut fields)) = (self.rid.clone(), self.fields.take()) else {
            return self.create(values).await;
        };
        for (key, value) in values {
            fields.set(key, value.clone());
        }
        match self.rest.write_service_fields(&rid, &fields).await {
            Ok(f) => {
                self.last = LastWrite::Wrote {
                    rid,
                    fields: f.clone(),
                };
                self.fields = Some(f);
                Ok(())
            }
            Err(e) => {
                if may_be_stored(&e) {
                    self.last = LastWrite::Unknown;
                }
                Err(e)
            }
        }
    }
}

/// Whether a failed `groups.setCustomFields` may have been stored anyway:
/// not when the server refused it before or while checking it (rate limit,
/// authentication, a 4xx answer).
fn may_be_stored(e: &Error) -> bool {
    match e {
        Error::RateLimited { .. } | Error::Unauthorized { .. } | Error::TwoFactorRequired(_) => {
            false
        }
        Error::Api { status, .. } | Error::Status { status, .. } => !(400..500).contains(status),
        _ => true,
    }
}

/// A seed of the primary that does not show the last write is not used.
impl Remembering for ServiceStore {
    fn remember(&mut self, last: LastWrite) {
        self.last = last;
    }

    fn last(&self) -> &LastWrite {
        &self.last
    }
}

/// The store as the flags alone see it (`rustket_core::flags::push`).
impl FlagStore for ServiceStore {
    type Error = Error;

    async fn read(&mut self) -> Result<FlagBlob, Error> {
        let copies = FieldsStore::read(self).await?;
        Ok(core::stored(&copies, now_ms()))
    }

    async fn write(&mut self, blob: &FlagBlob) -> Result<(), Error> {
        FieldsStore::write(self, &[(FLAGS_KEY, blob.to_value())]).await
    }
}

/// Why a push failed.
#[derive(Debug, Clone)]
pub enum PushFailure {
    Store(Failure),
    /// Other devices kept replacing the blob.
    Conflict,
}

/// A finished push of one key.
#[derive(Debug, Clone)]
pub struct Pushed<B> {
    /// What was pushed.
    pub written: B,
    /// The store's blob after the push (covers `written`).
    pub stored: B,
    /// The primary service room.
    pub rid: Option<String>,
}

/// A key's push result.
pub type PushResult<B> = Result<Pushed<B>, PushFailure>;

/// Messages of a key without duties of its own (flags have theirs,
/// `super::flags::Msg`).
#[derive(Debug, Clone)]
pub enum Msg<B> {
    Pushed {
        epoch: u64,
        res: Result<Pushed<B>, PushFailure>,
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

/// What [`BlobSync::plan`] wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Next {
    Push,
    Wait(Instant),
    Idle,
}

/// One synced key: its blob type and where the workspace keeps it.
pub trait ServiceKey: std::fmt::Debug + Default + Send + 'static {
    type Blob: EditBlob;
    /// The workspace area of the stored blob.
    const DIRTY: Dirty;
    /// Name in log lines.
    const LOG: &'static str;
    /// Edits growing the synced keys past [`core::BUDGET_BYTES`] are
    /// refused.
    const BUDGETED: bool = true;
    /// The blob as the subscriptions last showed it.
    fn stored(ws: &Workspace) -> &Self::Blob;
    /// The pending edits the workspace shows.
    fn overlay(ws: &Workspace) -> &Self::Blob;
    fn set_overlay(ws: &mut Workspace, overlay: Self::Blob);
    /// The overlay holds bookkeeping only (Later's seed): it does not count
    /// as pending and never creates the service room on its own; it goes
    /// out once a service room exists.
    fn bookkeeping(_overlay: &Self::Blob) -> bool {
        false
    }
    /// Edits the budget lets through (Later's seed: one small entry that
    /// must not be refused and retried on every update when unbudgeted
    /// keys fill the budget).
    fn unbudgeted(_edit: &<Self::Blob as EditBlob>::Edit) -> bool {
        false
    }
}

/// What every synced key as this device shows it (stored entries with its
/// pending edits) charges against [`core::BUDGET_BYTES`]
/// ([`SyncBlob::budget_bytes`]), by key. Keys added later belong here.
fn charged(ws: &Workspace, now_ms: i64) -> Vec<(&'static str, usize)> {
    vec![
        (FLAGS_KEY, shown_bytes::<super::flags::FlagsKey>(ws, now_ms)),
        (
            MENTION_GROUPS_KEY,
            shown_bytes::<super::mention_groups::MentionGroupsKey>(ws, now_ms),
        ),
        (
            LATER_KEY,
            shown_bytes::<super::later_sync::LaterKey>(ws, now_ms),
        ),
        (
            REMINDERS_KEY,
            shown_bytes::<super::later_sync::RemindersKey>(ws, now_ms),
        ),
    ]
}

/// Every synced key, for telling whether a copy of the fields shows this
/// client's last write ([`LastWrite::trusts`]). Keys added later belong here.
pub fn key_checks() -> Vec<KeyCheck> {
    vec![
        KeyCheck::of::<FlagBlob>(),
        KeyCheck::of::<MentionGroupBlob>(),
        KeyCheck::of::<LaterBlob>(),
        KeyCheck::of::<ReminderBlob>(),
    ]
}

/// What all synced keys charge against [`core::BUDGET_BYTES`].
pub fn service_bytes(ws: &Workspace, now_ms: i64) -> usize {
    charged(ws, now_ms).iter().map(|&(_, n)| n).sum()
}

fn shown_bytes<K: ServiceKey>(ws: &Workspace, now_ms: i64) -> usize {
    blob_bytes::<K>(ws, K::overlay(ws), now_ms)
}

fn blob_bytes<K: ServiceKey>(ws: &Workspace, overlay: &K::Blob, now_ms: i64) -> usize {
    K::stored(ws).merged_with(overlay).budget_bytes(now_ms)
}

/// Per-server sync state of one key (`ServerState::flags` holds the flags').
#[derive(Debug, Default)]
pub struct BlobSync<K: ServiceKey> {
    /// The session the state belongs to (`Arc` address).
    pub(super) session: Option<usize>,
    /// Bumped with every new session: older results are dropped.
    pub(super) epoch: u64,
    /// The account whose overlay is loaded.
    pub(super) account: Option<String>,
    /// Edits the subscriptions do not show yet.
    pub(super) overlay: K::Blob,
    /// Bumped with every overlay change; `applied` is the one the workspace
    /// has, `saved` the one the config has.
    pub(super) rev: u64,
    pub(super) applied: Option<u64>,
    pub(super) saved: u64,
    /// `K::DIRTY` revision the overlay was last pruned against.
    pub(super) blob_rev: Option<u64>,
    /// The store's blob as the last push left it: the overlay needs a push
    /// while this does not cover it.
    pub(super) confirmed: K::Blob,
    /// Not before this (coalescing edits).
    pub(super) due: Option<Instant>,
    pub(super) in_flight: bool,
    /// The overlay as the running push writes it.
    pub(super) pushing: Option<K::Blob>,
    pub(super) failures: u32,
    pub(super) retry_at: Option<Instant>,
    /// The server asked to wait until then (rate limits are per IP and
    /// route, so every key of the server keeps the pause, [`pause_on_rate_limit`];
    /// edits do not lift it).
    pub(super) paused_until: Option<Instant>,
    /// No push of the server before this ([`PUSH_SPACING`] after the last
    /// one started).
    pub(super) spaced_until: Option<Instant>,
    /// Given up with this error, until the next edit or session.
    pub(super) failed: Option<String>,
    /// A service room a push created or found (the subscription may not
    /// have arrived yet).
    pub(super) created: Option<String>,
    /// Entries written recently, watched until `verify_at`.
    pub(super) recent: K::Blob,
    /// The subscriptions showed `recent` since it was written.
    pub(super) recent_seen: bool,
    pub(super) verify_at: Option<Instant>,
    pub(super) timer: Option<Instant>,
    _key: K,
}

impl<K: ServiceKey> BlobSync<K> {
    /// Loads the pending edits of the signed-in account (`uid`) when it
    /// changed; `load` reads them from the config.
    pub fn follow_account(&mut self, uid: Option<&str>, load: impl FnOnce(&str) -> K::Blob) {
        if self.account.as_deref() == uid {
            return;
        }
        self.overlay = uid.map(load).unwrap_or_default();
        self.account = uid.map(str::to_owned);
        self.rev += 1;
        self.saved = self.rev;
        self.applied = None;
        self.blob_rev = None;
        self.confirmed = K::Blob::default();
        self.recent = K::Blob::default();
        self.recent_seen = false;
        self.verify_at = None;
        self.failed = None;
        self.failures = 0;
        self.retry_at = None;
        self.due = None;
    }

    /// The pending edits.
    pub fn overlay(&self) -> &K::Blob {
        &self.overlay
    }

    pub fn status(&self) -> Status {
        if let Some(e) = &self.failed {
            Status::Failed(e.clone())
        } else if self.overlay.is_empty() || K::bookkeeping(&self.overlay) {
            Status::Synced
        } else {
            Status::Pending
        }
    }

    /// Records a user's edit: shown at once, pushed after [`COALESCE`].
    /// Returns whether anything changed (`false` also while signed out).
    /// Refused when it would grow the synced keys past
    /// [`core::BUDGET_BYTES`] ([`ServiceKey::BUDGETED`]).
    pub fn edit(
        &mut self,
        ws: &mut Workspace,
        edit: &<K::Blob as EditBlob>::Edit,
        device: &str,
        now: Instant,
    ) -> Result<bool, <K::Blob as EditBlob>::Error> {
        if self.account.is_none() {
            return Ok(false);
        }
        let now_ms = now_ms();
        let mut next = self.overlay.clone();
        if !next.apply_edit(K::stored(ws), edit, now_ms, device)? {
            return Ok(false);
        }
        if K::BUDGETED && !K::unbudgeted(edit) {
            let key = <K::Blob as SyncBlob>::KEY;
            let others: usize = charged(ws, now_ms)
                .iter()
                .filter(|&&(k, _)| k != key)
                .map(|&(_, n)| n)
                .sum();
            let before = others + blob_bytes::<K>(ws, &self.overlay, now_ms);
            let after = others + blob_bytes::<K>(ws, &next, now_ms);
            if !core::fits_budget(before, after) {
                tracing::debug!(
                    key = K::LOG,
                    bytes = after,
                    "service sync: edit over the budget"
                );
                return Err(K::Blob::too_large());
            }
        }
        self.overlay = next;
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
    pub fn take_save(&mut self) -> Option<(String, K::Blob)> {
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
        self.pushing = None;
        self.timer = None;
        self.created = None;
        self.failed = None;
        self.failures = 0;
        self.retry_at = None;
        self.spaced_until = None;
        // The next session reads the store again.
        self.confirmed = K::Blob::default();
    }

    pub(super) fn apply(&mut self, ws: &mut Workspace) {
        if self.applied != Some(self.rev) {
            K::set_overlay(ws, self.overlay.clone());
            self.applied = Some(self.rev);
        }
    }

    /// Whether this device can push now: live, and signed in as the
    /// overlay's account.
    pub(super) fn live(&self, ws: &Workspace) -> bool {
        !ws.from_cache()
            && *ws.connection() == ConnectionState::Connected
            && self.account.is_some()
            && ws.me().map(|m| m.id.as_str()) == self.account.as_deref()
    }

    /// Follows the workspace's blob: prunes the overlay and checks recently
    /// written entries.
    pub(super) fn observe(&mut self, ws: &Workspace, now: Instant) {
        let now_ms = now_ms();
        let rev = ws.revision(K::DIRTY);
        if self.blob_rev != Some(rev) {
            self.blob_rev = Some(rev);
            let stored = K::stored(ws);
            if self.overlay.prune_covered_at(stored, now_ms) {
                self.rev += 1;
            }
            if !self.recent.is_empty() {
                if stored.covers_at(&self.recent, now_ms) {
                    self.recent_seen = true;
                } else if self.recent_seen {
                    // Shown, then gone: another device's write dropped it.
                    self.verify_at = Some(now);
                }
            }
        }
        if self.verify_at.is_some_and(|t| now >= t) && self.live(ws) {
            self.verify_at = None;
            let lost = std::mem::take(&mut self.recent).uncovered_by_at(K::stored(ws), now_ms);
            self.recent_seen = false;
            if !lost.is_empty() {
                tracing::debug!(
                    key = K::LOG,
                    entries = lost.len(),
                    "service sync: written entries were dropped, writing again"
                );
                self.overlay.merge(&lost);
                self.rev += 1;
                self.confirmed = K::Blob::default();
            }
        }
    }

    /// The overlay has entries to store and nothing keeps it from going out
    /// with the next push of the server (it may still wait for its own
    /// time, [`Self::plan`]). Bookkeeping alone waits for a service room
    /// ([`ServiceKey::bookkeeping`]).
    pub(super) fn ready(&self, ws: &Workspace) -> bool {
        !self.in_flight
            && self.failed.is_none()
            && !self.overlay.is_empty()
            && !(K::bookkeeping(&self.overlay)
                && self.created.is_none()
                && ws.service_rooms().is_empty())
            && !self.confirmed.covers_at(&self.overlay, now_ms())
            && self.live(ws)
    }

    /// Whether the overlay has entries to push, and when.
    pub(super) fn plan(&self, ws: &Workspace, now: Instant) -> Next {
        if !self.ready(ws) {
            return Next::Idle;
        }
        let at = self
            .due
            .into_iter()
            .chain(self.retry_at)
            .chain(self.paused_until)
            .chain(self.spaced_until)
            .max();
        match at {
            Some(at) if now < at => Next::Wait(at),
            _ => Next::Push,
        }
    }

    /// The pending times the planner waits for, for the pacing timer.
    pub(super) fn next_wake(&self, ws: &Workspace, now: Instant) -> Option<Instant> {
        let push = match self.plan(ws, now) {
            Next::Wait(at) => Some(at),
            _ => None,
        };
        push.into_iter()
            .chain(self.verify_at.filter(|_| self.live(ws)))
            .min()
    }

    /// Per update, before the server's push starts: follows the session,
    /// prunes and applies the overlay. Returns whether the session changed.
    pub fn follow(
        &mut self,
        ws: &mut Workspace,
        session: Option<&SessionHandle>,
        now: Instant,
    ) -> bool {
        let ptr = session.map(|h| Arc::as_ptr(&h.0) as usize);
        let changed = self.session != ptr;
        if changed {
            self.reset_session();
            self.session = ptr;
        }
        self.observe(ws, now);
        self.apply(ws);
        changed
    }

    /// Per update of a key without duties of its own: [`Self::follow`] and
    /// the pacing timer. The push itself starts with [`take_parts`].
    pub fn tick(
        &mut self,
        ws: &mut Workspace,
        session: Option<&SessionHandle>,
        now: Instant,
    ) -> Task<Msg<K::Blob>> {
        self.follow(ws, session, now);
        if session.is_none() {
            return Task::none();
        }
        match self.next_wake(ws, now) {
            Some(at) => self.schedule(at, now).map(|at| Msg::Pace { at }),
            None => Task::none(),
        }
    }

    /// Sets the pacing timer unless one fires earlier; the task yields `at`.
    pub(super) fn schedule(&mut self, at: Instant, now: Instant) -> Task<Instant> {
        if self.timer.is_some_and(|t| t <= at && t >= now) {
            return Task::none();
        }
        self.timer = Some(at);
        let wait = at.saturating_duration_since(now);
        Task::perform(tokio::time::sleep(wait), move |_| at)
    }

    /// The pacing timer set for `at` fired.
    pub(super) fn paced(&mut self, at: Instant) {
        if self.timer == Some(at) {
            self.timer = None;
        }
    }

    /// A service room created or found by a push of any key of the server.
    pub fn adopt_created(&mut self, rid: &str) {
        self.created = Some(rid.to_owned());
    }

    /// This key's result of a combined push it took part in, for
    /// [`Self::update`] (`None`: not part of it, or of an older session).
    pub fn finish(&mut self, o: &Outcome) -> Option<(u64, PushResult<K::Blob>)> {
        let epoch = o.epoch_of(<K::Blob as SyncBlob>::KEY)?;
        if epoch != self.epoch {
            return None;
        }
        let written = self.pushing.take().unwrap_or_default();
        let res = match &o.res {
            Ok(s) => {
                let now_ms = now_ms();
                let stored: K::Blob = core::stored(&s.copies, now_ms);
                if stored.covers_at(&written, now_ms) {
                    Ok(Pushed {
                        written,
                        stored,
                        rid: s.rid.clone(),
                    })
                } else {
                    Err(PushFailure::Conflict)
                }
            }
            Err(f) => Err(PushFailure::Store(f.clone())),
        };
        Some((epoch, res))
    }

    /// Applies a result.
    pub fn update(&mut self, msg: Msg<K::Blob>, now: Instant) -> Vec<Out> {
        match msg {
            Msg::Pace { at } => {
                self.paced(at);
                Vec::new()
            }
            Msg::Pushed { epoch, res } => self.pushed(epoch, res, now),
        }
    }

    /// Applies a push result.
    pub(super) fn pushed(
        &mut self,
        epoch: u64,
        res: Result<Pushed<K::Blob>, PushFailure>,
        now: Instant,
    ) -> Vec<Out> {
        if epoch != self.epoch {
            return Vec::new();
        }
        self.in_flight = false;
        self.pushing = None;
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
                tracing::debug!(
                    key = K::LOG,
                    "service sync: push lost to concurrent writes, retrying"
                );
                self.retry(now, None);
                Vec::new()
            }
            Err(PushFailure::Store(f)) => self.failed_push(f, now),
        }
    }

    /// Nothing goes out before `until` (a rate limit of the server).
    pub fn pause_until(&mut self, until: Instant) {
        self.paused_until = self.paused_until.max(Some(until));
    }

    fn retry(&mut self, now: Instant, wait: Option<Duration>) {
        self.failures += 1;
        self.retry_at = Some(now + wait.unwrap_or_else(|| retry_delay(self.failures)));
    }

    fn failed_push(&mut self, f: Failure, now: Instant) -> Vec<Out> {
        let e = f.error();
        if let Some(wait) = rate_limit_wait(e) {
            tracing::debug!(key = K::LOG, error = %f, "service sync: rate limited");
            // Refused before it ran: not a failure of its own.
            self.pause_until(now + wait);
            return Vec::new();
        }
        let give_up = match e {
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
            tracing::warn!(key = K::LOG, error = %f, "service sync: cannot store on the server");
            self.failed = Some(f.user_text());
            self.retry_at = None;
            return vec![Out::Failed(f)];
        }
        tracing::debug!(
            key = K::LOG,
            error = %f,
            failures = self.failures + 1,
            "service sync: push failed, retrying"
        );
        self.retry(now, None);
        Vec::new()
    }
}

/// How long a rate-limit answer asks to wait (`None`: not one).
fn rate_limit_wait(e: &Error) -> Option<Duration> {
    match e {
        Error::RateLimited { retry_after, .. } => Some(
            retry_after
                .unwrap_or(RATE_LIMIT_PAUSE)
                .clamp(Duration::from_secs(1), MAX_PAUSE),
        ),
        _ => None,
    }
}

/// A combined push was rate limited: the limit is per IP and route, not per
/// key, so no key of the server pushes before it ends (call before the keys
/// take their parts).
pub fn pause_on_rate_limit(o: &Outcome, syncs: &mut [&mut dyn Syncing], now: Instant) {
    if let Err(f) = &o.res
        && let Some(wait) = rate_limit_wait(f.error())
    {
        for s in syncs.iter_mut() {
            s.pause(now + wait);
        }
    }
}

/// What every synced key of a server shares (`ServerState::service`).
#[derive(Debug, Default)]
pub struct Shared {
    /// Held by a push for its whole read-merge-write loop; holds what this
    /// client last wrote ([`core::push_locked`]).
    lock: Arc<tokio::sync::Mutex<LastWrite>>,
}

/// One key's part of a combined push.
pub struct Outgoing {
    key: &'static str,
    epoch: u64,
    account: Option<String>,
    created: Option<String>,
    part: Box<dyn Part>,
}

impl Outgoing {
    /// The key and the epoch of its sync (as an [`Outcome`] lists them).
    pub fn key(&self) -> (&'static str, u64) {
        (self.key, self.epoch)
    }
}

/// The result of a combined push, for every key that took part.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The keys pushed, with the epoch of their sync.
    keys: Vec<(&'static str, u64)>,
    res: Result<StoredFields, Failure>,
}

/// The service fields after a push.
#[derive(Debug, Clone)]
pub struct StoredFields {
    /// Every copy (several service rooms: one each).
    copies: Vec<ServiceFields>,
    /// The primary service room.
    rid: Option<String>,
}

impl Outcome {
    /// A successful push of `keys` that left `copies` (tests of the keys).
    #[cfg(test)]
    pub(super) fn for_tests(keys: Vec<(&'static str, u64)>, copies: Vec<ServiceFields>) -> Self {
        Self {
            keys,
            res: Ok(StoredFields {
                copies,
                rid: Some("svc".into()),
            }),
        }
    }

    fn epoch_of(&self, key: &str) -> Option<u64> {
        self.keys.iter().find(|(k, _)| *k == key).map(|&(_, e)| e)
    }

    /// The primary service room the push wrote to (or created).
    pub fn rid(&self) -> Option<&str> {
        self.res.as_ref().ok()?.rid.as_deref()
    }
}

/// A key's sync as [`take_parts`] sees it.
pub trait Syncing {
    /// The key's own time to push has come.
    fn wants_push(&self, ws: &Workspace, now: Instant) -> bool;
    /// The key has edits to store and can join a push now.
    fn ready(&self, ws: &Workspace) -> bool;
    /// Joins the push: marks it in flight and hands over the overlay.
    fn start(&mut self) -> Outgoing;
    /// No push before `until` ([`pause_on_rate_limit`]).
    fn pause(&mut self, until: Instant);
    /// A push of the server started: none before `until` ([`PUSH_SPACING`]).
    fn space(&mut self, until: Instant);
}

impl<K: ServiceKey> Syncing for BlobSync<K> {
    fn wants_push(&self, ws: &Workspace, now: Instant) -> bool {
        self.plan(ws, now) == Next::Push
    }

    fn ready(&self, ws: &Workspace) -> bool {
        BlobSync::ready(self, ws)
    }

    fn start(&mut self) -> Outgoing {
        self.in_flight = true;
        self.due = None;
        self.pushing = Some(self.overlay.clone());
        Outgoing {
            key: <K::Blob as SyncBlob>::KEY,
            epoch: self.epoch,
            account: self.account.clone(),
            created: self.created.clone(),
            part: Box::new(self.overlay.clone()),
        }
    }

    fn pause(&mut self, until: Instant) {
        self.pause_until(until);
    }

    fn space(&mut self, until: Instant) {
        self.spaced_until = Some(until);
    }
}

/// When the time of one key of the server has come, the parts of one push
/// of every key that has edits to store, each marked in flight (a key
/// still coalescing or backing off goes along: the write happens anyway).
/// [`push_task`] runs them.
pub fn take_parts(
    ws: &Workspace,
    syncs: &mut [&mut dyn Syncing],
    now: Instant,
) -> Option<Vec<Outgoing>> {
    if !syncs.iter().any(|s| s.wants_push(ws, now)) {
        return None;
    }
    let parts = syncs
        .iter_mut()
        .filter(|s| s.ready(ws))
        .map(|s| s.start())
        .collect();
    for s in syncs.iter_mut() {
        s.space(now + PUSH_SPACING);
    }
    Some(parts)
}

/// Runs one combined push of `parts` ([`take_parts`]).
pub fn push_task(
    ws: &Workspace,
    h: &SessionHandle,
    shared: &Shared,
    parts: Vec<Outgoing>,
) -> Task<Outcome> {
    let mut rooms = ws.service_rooms().to_vec();
    if rooms.is_empty() {
        rooms.extend(parts.iter().find_map(|p| p.created.clone()));
    }
    let uid = parts
        .iter()
        .find_map(|p| p.account.clone())
        .unwrap_or_default();
    let seeds = rooms.iter().filter_map(|rid| {
        let f = ws.room(rid)?.sub.as_ref()?.service_fields()?;
        Some((rid.clone(), f))
    });
    let seeds: Vec<_> = seeds.collect();
    let binding = ws.me().map(ServiceBinding::of).unwrap_or_default();
    let store = ServiceStore::new(h.rest().clone(), uid, &rooms)
        .with_seeds(seeds)
        .with_binding(binding);
    let keys: Vec<(&'static str, u64)> = parts.iter().map(|p| (p.key, p.epoch)).collect();
    let parts: Vec<Box<dyn Part>> = parts.into_iter().map(|p| p.part).collect();
    let lock = shared.lock.clone();
    Task::perform(
        async move {
            let mut store = store;
            let res = core::push_locked(&lock, &mut store, &parts, PUSH_ATTEMPTS, now_ms()).await;
            res.map(|copies| StoredFields {
                copies,
                rid: store.rid().map(str::to_owned),
            })
            .map_err(Failure::from)
        },
        move |res| Outcome { keys, res },
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::workspace::flags::{FlagSync, FlagsKey};
    use rocketchat::models::User;
    use rustket_core::flags::{FlagColor, FlagEdit, FlagError};

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

    fn synced() -> FlagSync {
        let mut s = FlagSync::default();
        s.follow_account(Some("me"), |_| FlagBlob::default());
        s
    }

    fn room(rid: &str) -> FlagEdit {
        FlagEdit::Room {
            rid: rid.into(),
            color: Some(FlagColor::Red),
        }
    }

    fn outcome(keys: Vec<(&'static str, u64)>, copies: Vec<ServiceFields>) -> Outcome {
        Outcome {
            keys,
            res: Ok(StoredFields {
                copies,
                rid: Some("svc".into()),
            }),
        }
    }

    #[test]
    fn keys_join_a_push_and_take_their_part_of_the_result() {
        let mut ws = ws();
        let mut s = synced();
        let t0 = Instant::now();
        assert!(s.edit(&mut ws, &room("r1"), "dev", t0).unwrap());
        let flags: &mut BlobSync<FlagsKey> = &mut s;
        assert!(!flags.wants_push(&ws, t0), "still coalescing");
        assert!(Syncing::ready(flags, &ws), "but can go along");
        assert!(flags.wants_push(&ws, t0 + COALESCE));
        let out = flags.start();
        assert_eq!(out.key, FLAGS_KEY);
        assert!(s.in_flight && !Syncing::ready(&*s, &ws));
        let written = s.overlay().clone();
        let mut f = ServiceFields::new("me");
        f.set(FLAGS_KEY, written.to_value());
        // Another key's result, or an older session's: not ours.
        let e = s.epoch;
        assert!(
            s.finish(&outcome(vec![("rustketOther", e)], vec![f.clone()]))
                .is_none()
        );
        assert!(
            s.finish(&outcome(vec![(FLAGS_KEY, e + 1)], vec![f.clone()]))
                .is_none()
        );
        let (epoch, res) = s.finish(&outcome(vec![(FLAGS_KEY, e)], vec![f])).unwrap();
        let p = res.unwrap();
        assert_eq!(
            (p.written, p.rid.as_deref()),
            (written.clone(), Some("svc"))
        );
        let p = Pushed {
            written: written.clone(),
            stored: written.clone(),
            rid: None,
        };
        s.pushed(epoch, Ok(p), t0);
        assert!(!s.in_flight);
        // A result that does not hold the entries lost to another writer.
        s.edit(&mut ws, &room("r2"), "dev", t0).unwrap();
        let flags: &mut BlobSync<FlagsKey> = &mut s;
        flags.start();
        let lost = outcome(vec![(FLAGS_KEY, s.epoch)], vec![ServiceFields::new("me")]);
        let (_, res) = s.finish(&lost).unwrap();
        assert!(matches!(res, Err(PushFailure::Conflict)));
    }

    /// A budgeted key for the tests, with flag-shaped entries; the
    /// workspace never shows it (stored and overlay stay empty there).
    #[derive(Debug, Clone, Default, PartialEq)]
    struct Toy(FlagBlob);

    impl SyncBlob for Toy {
        const KEY: &'static str = "rustketToy";
        fn from_value_at(v: Option<&Value>, now_ms: i64) -> Self {
            Self(FlagBlob::from_value_at(v, now_ms))
        }
        fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value {
            let copies: Vec<FlagBlob> = copies.iter().map(|c| c.0.clone()).collect();
            self.0.to_value_over_at(&copies, now_ms)
        }
        fn merge(&mut self, other: &Self) -> bool {
            self.0.merge(&other.0)
        }
        fn covers_at(&self, other: &Self, now_ms: i64) -> bool {
            self.0.covers_at(&other.0, now_ms)
        }
        fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self {
            Self(self.0.uncovered_by_at(&stored.0, now_ms))
        }
        fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool {
            self.0.prune_covered_at(&stored.0, now_ms)
        }
        fn is_empty(&self) -> bool {
            SyncBlob::is_empty(&self.0)
        }
        fn len(&self) -> usize {
            SyncBlob::len(&self.0)
        }
    }

    impl EditBlob for Toy {
        type Edit = FlagEdit;
        type Error = FlagError;
        fn apply_edit(
            &mut self,
            stored: &Self,
            edit: &FlagEdit,
            now_ms: i64,
            device: &str,
        ) -> Result<bool, FlagError> {
            self.0.apply_edit(&stored.0, edit, now_ms, device)
        }
        fn too_large() -> FlagError {
            FlagError::Full
        }
    }

    #[derive(Debug, Default)]
    struct ToyKey;

    static EMPTY: std::sync::LazyLock<Toy> = std::sync::LazyLock::new(Toy::default);

    impl ServiceKey for ToyKey {
        type Blob = Toy;
        const DIRTY: Dirty = Dirty::LATER;
        const LOG: &'static str = "toy";
        fn stored(_: &Workspace) -> &Toy {
            &EMPTY
        }
        fn overlay(_: &Workspace) -> &Toy {
            &EMPTY
        }
        fn set_overlay(_: &mut Workspace, _: Toy) {}
    }

    fn toy_synced() -> BlobSync<ToyKey> {
        let mut s = BlobSync::<ToyKey>::default();
        s.follow_account(Some("me"), |_| Toy::default());
        s
    }

    fn long_id(n: usize) -> String {
        format!("{n:0>120}")
    }

    #[test]
    fn edits_over_the_budget_are_refused() {
        let mut ws = ws();
        let mut s = toy_synced();
        let t0 = Instant::now();
        // Long ids fill the budget long before the item cap.
        let mut n = 0;
        let refused = loop {
            match s.edit(&mut ws, &room(&long_id(n)), "dev", t0) {
                Ok(true) => n += 1,
                Ok(false) => unreachable!(),
                Err(e) => break e,
            }
            assert!(n < rustket_core::flags::MAX_FLAGGED_ITEMS, "never refused");
        };
        assert_eq!(refused, Toy::too_large());
        assert!(s.overlay().budget_bytes(now_ms()) <= core::BUDGET_BYTES);
        // Removing still works.
        let gone = FlagEdit::Room {
            rid: long_id(0),
            color: None,
        };
        assert_eq!(s.edit(&mut ws, &gone, "dev", t0), Ok(true));
    }

    #[test]
    fn flags_are_not_refused_by_the_budget_but_their_live_entries_count() {
        let mut ws = ws();
        let mut flags = synced();
        let t0 = Instant::now();
        // Flagged and cleared over time: the tombstones alone are far over
        // the budget.
        for n in 0..500 {
            let rid = long_id(n);
            assert_eq!(flags.edit(&mut ws, &room(&rid), "dev", t0), Ok(true));
            let off = FlagEdit::Room { rid, color: None };
            assert_eq!(flags.edit(&mut ws, &off, "dev", t0), Ok(true));
        }
        let full = core::key_bytes(FLAGS_KEY, &flags.overlay().to_value());
        assert!(full > core::BUDGET_BYTES, "{full}");
        assert!(service_bytes(&ws, now_ms()) < 1024, "tombstones are free");
        assert_eq!(
            flags.edit(&mut ws, &room("new"), "dev", t0),
            Ok(true),
            "a new flag is still accepted"
        );
        let mut toy = toy_synced();
        assert_eq!(toy.edit(&mut ws, &room("t0"), "dev", t0), Ok(true));
        // Live flags past the budget: accepted (flags keep their own cap),
        // but the budgeted keys cannot grow any more.
        let mut m = 0;
        while service_bytes(&ws, now_ms()) <= core::BUDGET_BYTES {
            let rid = format!("x{m:0>119}");
            assert_eq!(flags.edit(&mut ws, &room(&rid), "dev", t0), Ok(true));
            m += 1;
        }
        assert!(m < rustket_core::flags::MAX_FLAGGED_ITEMS);
        assert_eq!(
            toy.edit(&mut ws, &room("t1"), "dev", t0),
            Err(Toy::too_large())
        );
        let gone = FlagEdit::Room {
            rid: "t0".into(),
            color: None,
        };
        assert_eq!(toy.edit(&mut ws, &gone, "dev", t0), Ok(true));
    }

    #[test]
    fn seeds_older_than_the_last_write_are_not_used() {
        let before = ServiceFields::new("me");
        let mut mine = FlagBlob::default();
        mine.edit(&FlagBlob::default(), &room("r1"), 5, "dev")
            .unwrap();
        let mut after = before.clone();
        after.set(FLAGS_KEY, mine.to_value());
        let last = LastWrite::Wrote {
            rid: "svc".into(),
            fields: after.clone(),
        };
        let (keys, now) = (key_checks(), now_ms());
        assert!(!last.trusts("svc", &before, &keys, now));
        assert!(last.trusts("svc", &after, &keys, now));
        assert!(LastWrite::Nothing.trusts("svc", &before, &keys, now));
        // Another device's flag on top of the write: used as it is.
        let mut theirs = mine.clone();
        theirs.edit(&mine, &room("r2"), 6, "other").unwrap();
        let mut newer = after.clone();
        newer.set(FLAGS_KEY, theirs.to_value());
        assert!(last.trusts("svc", &newer, &keys, now));
        let mut flagged = LastWrite::Wrote {
            rid: "svc".into(),
            fields: newer.clone(),
        };
        assert!(!flagged.trusts("svc", &after, &keys, now));
        flagged = LastWrite::Unknown;
        assert!(!flagged.trusts("svc", &newer, &keys, now));
    }

    #[test]
    fn refused_writes_keep_what_was_last_written() {
        let limited = Error::RateLimited {
            retry_after: None,
            message: None,
        };
        assert!(!may_be_stored(&limited));
        let denied = Error::from_response(
            400,
            None,
            br#"{"success":false,"error":"Not allowed [error-action-not-allowed]","errorType":"error-action-not-allowed"}"#,
        );
        assert!(!may_be_stored(&denied));
        assert!(!may_be_stored(&Error::Unauthorized {
            message: None,
            code: None
        }));
        assert!(may_be_stored(&Error::Timeout("write")));
        assert!(may_be_stored(&Error::Status {
            status: 502,
            body: String::new()
        }));
    }

    #[test]
    fn a_rate_limit_pauses_every_key_of_the_server() {
        let mut ws = ws();
        let mut flags = synced();
        let mut toy = toy_synced();
        let t0 = Instant::now();
        flags.edit(&mut ws, &room("r1"), "dev", t0).unwrap();
        let out = Syncing::start(&mut flags.sync);
        let limited = Error::RateLimited {
            retry_after: Some(Duration::from_secs(60)),
            message: None,
        };
        let o = Outcome {
            keys: vec![(out.key, out.epoch)],
            res: Err(Failure::from(limited)),
        };
        let end = t0 + Duration::from_secs(60);
        {
            let syncs: &mut [&mut dyn Syncing] = &mut [&mut flags.sync, &mut toy];
            pause_on_rate_limit(&o, syncs, t0);
        }
        let (epoch, res) = flags.finish(&o).unwrap();
        assert!(flags.sync.update(Msg::Pushed { epoch, res }, t0).is_empty());
        assert_eq!(flags.plan(&ws, t0), Next::Wait(end));
        // The other key, edited during the pause, waits for its end too,
        // and so do new edits of the limited one.
        toy.edit(&mut ws, &room("t1"), "dev", t0).unwrap();
        flags.edit(&mut ws, &room("r2"), "dev", t0).unwrap();
        for at in [t0 + COALESCE, end - Duration::from_secs(1)] {
            assert!(!toy.wants_push(&ws, at) && !flags.sync.wants_push(&ws, at));
        }
        assert_eq!(toy.next_wake(&ws, t0 + COALESCE), Some(end));
        assert_eq!(flags.sync.next_wake(&ws, t0 + COALESCE), Some(end));
        assert!(toy.wants_push(&ws, end) && flags.sync.wants_push(&ws, end));
        // Other failures are not a pause of the server.
        let mut other = toy_synced();
        let o = Outcome {
            keys: Vec::new(),
            res: Err(Failure::from(Error::Timeout("push"))),
        };
        pause_on_rate_limit(&o, &mut [&mut other], t0);
        assert_eq!(other.paused_until, None);
    }
}
