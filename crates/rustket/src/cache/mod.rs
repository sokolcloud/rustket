//! Offline cache glue (`rustket_core::cache`). Owner: cache agent.
//! Strings: `locales/cache.yml` (`cache.*`).
//!
//! - Opening: each logged-in server opens its cache (worker thread) as soon
//!   as its credentials are known. A workspace with no server data yet shows
//!   the cached one right away (`Workspace::apply_cached`), together with the
//!   last room and its cached messages, until the server's initial load
//!   replaces it (cached rooms the server no longer lists disappear, the
//!   server wins every tie). Automatic read marks wait for that
//!   (`Workspace::from_cache`); cached data never notifies.
//! - Writing: only data the server delivered in this session (after the
//!   initial load) is written: documents of changed areas, room entries of
//!   changed rooms and the newest messages of loaded timelines; the storage
//!   layer keeps E2E messages as ciphertext only. The sidebar's thread data
//!   (followed sets, parents rows refer to) is written at most every
//!   minute and when quitting; after a restart only rooms with replies
//!   since are loaded again (`workspace::threads`).
//! - Offline reading: while the server is unreachable (or the cache is
//!   shown), an opened room or thread without its first page gets its
//!   cached messages (`Workspace::seed_cached_messages`); the first page from
//!   the server replaces them.
//! - Outbox: pending text messages are persisted with their client ids
//!   (only those of rooms known to be unencrypted, see `docs/e2e.md`).
//!   Messages written offline, and those still sending when the app quit,
//!   are sent once the server is back: one at a time, oldest first, a room
//!   stops at its first failure (the server stamps `ts` on arrival). The
//!   server rejects a second message with the same id; a failed resend
//!   checks whether the message exists, and a message that may have been
//!   delivered before quitting is first looked up, and dropped when it was
//!   deleted meanwhile. Messages of encrypted rooms wait until the E2E
//!   manager can encrypt them (never sent as plaintext). Failed ones stay
//!   for retry or discard. Uploads are not persisted: one interrupted by
//!   quitting is reported on the next start.
//! - Scheduled messages (`rustket_core::scheduled`) are outbox entries with a
//!   send time, persisted like pending ones (same rooms only) but kept out
//!   of the timelines; once due they become queued entries
//!   ([`enqueue_scheduled`]) and go out on the path above.
//! - Cleanup: logout, a rejected token and server removal delete the
//!   server's cache; Settings → Storage clears every cache (unsent messages
//!   stay).

pub mod view;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::{SinkExt, Stream};
use rocketchat::models::{Message as RcMessage, OutgoingMessage, Permission, Setting};
use rocketchat::{RcDate, RestClient};
use rust_i18n::t;
use rustket_core::cache::{
    Batch, CacheHandle, CachedState, CachedTimeline, MESSAGES_PER_TIMELINE, Opened, OutboxEntry,
    TimelineKey, server_dir,
};
use rustket_core::e2e::OutgoingEncrypted;
use rustket_core::timeline::{DeliveryState, Timeline};
use rustket_core::workspace::{Changes, ConnectionState};
use rustket_core::{Dirty, ServerId, Workspace};
use rustket_ui::widget::BannerKind;

use crate::app::App;
use crate::e2e::outgoing::Mode;
use crate::error::Failure;
use crate::message::Message;
use crate::workspace::ServerState;

/// Longest wait for queued writes when the app quits.
const QUIT_FLUSH: Duration = Duration::from_secs(2);
/// How far before a message's local time deletions are looked up (clock
/// differences between the client and the server).
const DELETED_LOOKBACK_MS: i64 = 15 * 60 * 1000;
/// Minimum time between two writes of the sidebar's thread data (it changes
/// with every thread parent update; quitting writes it too).
const THREADS_WRITE_INTERVAL: Duration = Duration::from_secs(60);
/// Tries of a queued message failing with a transient error (network,
/// 5xx, rate limit) while connected before it is left failed: these go out
/// with nobody watching (written offline, scheduled).
const MAX_SEND_TRIES: u32 = 5;

/// Wait before the next try of a queued message after `tries` transient
/// failures: 15 s doubling, at most 5 min (at least what a rate limit asks).
fn retry_delay_ms(tries: u32, e: &rocketchat::Error) -> i64 {
    let backoff = (15_000_i64 << tries.saturating_sub(1).min(5)).min(300_000);
    match e {
        rocketchat::Error::RateLimited {
            retry_after: Some(d),
            ..
        } => backoff.max(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        _ => backoff,
    }
}

/// Whether a queued message that failed (`tried`: it was sent and failed,
/// not skipped after an earlier one of its room) with `e` while connected
/// waits for another try, after `tries` failed tries.
fn retries(e: &rocketchat::Error, tried: bool, tries: u32) -> bool {
    e.is_transient() && (!tried || tries < MAX_SEND_TRIES)
}

/// A timeline instance (see `Timeline::epoch`).
type Epoch = (u64, u64);
/// An outbox entry as last written: id, queued, maybe sent, error, send
/// time, a hash of the text (scheduled texts can be edited) and the
/// reactions to add once sent.
type OutboxSig = (
    String,
    bool,
    bool,
    Option<String>,
    Option<i64>,
    u64,
    Vec<String>,
);

/// App-wide cache state.
#[derive(Debug, Default)]
pub struct Caches {
    /// Base directory (`None`: caching is off, e.g. tests and snapshots).
    base: Option<PathBuf>,
    /// Bytes on disk of every server's cache (Settings → Storage).
    pub size: Option<u64>,
    pub clearing: bool,
    /// Directories purged while their cache was still opening: deleted
    /// again when the late open arrives.
    purged: HashSet<PathBuf>,
}

impl Caches {
    pub fn new(base: Option<PathBuf>) -> Self {
        Self {
            base,
            size: None,
            clearing: false,
            purged: HashSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.base.is_some()
    }
}

/// The default base directory (the binary passes it in `Flags::offline_cache`).
pub fn default_base() -> Option<PathBuf> {
    rustket_core::cache::default_base()
}

/// Per-server cache state (`ServerState::cache`).
#[derive(Debug, Default)]
pub struct ServerCache {
    handle: Option<CacheHandle>,
    /// Account the open cache belongs to.
    user: Option<String>,
    /// Account whose cache is being opened.
    opening: Option<String>,
    /// Opening failed for this account (not retried in this run).
    failed_for: Option<String>,
    /// When the shown cache was written (Unix ms).
    pub saved_at: Option<i64>,
    /// Write every document and loaded timeline at the next chance (the
    /// cache was opened after the initial load, or cleared).
    needs_full: bool,
    /// Timeline epoch and version last written.
    captured: HashMap<TimelineKey, (Epoch, u64)>,
    /// Cached messages asked for, per timeline epoch.
    seed_asked: HashSet<(TimelineKey, Epoch)>,
    /// The persisted outbox was put back into the workspace.
    outbox_restored: bool,
    /// What the outbox looked like when last written.
    outbox_sig: Option<Vec<OutboxSig>>,
    /// Outbox ids read from disk (kept on disk while their room is not
    /// known yet).
    restored: HashSet<String>,
    /// Restored ids that were sending when the app quit (possibly
    /// delivered, possibly deleted since).
    unconfirmed: HashSet<String>,
    /// Outbox ids seen so far.
    known: HashSet<String>,
    /// Outbox ids to send once the server is reachable.
    queued: HashSet<String>,
    /// Outbox ids being sent by this module.
    sending: HashSet<String>,
    /// The handle of the last send of queued messages started
    /// ([`abort_sends`]); aborting one that already finished does nothing.
    send_task: Option<iced::task::Handle>,
    /// Queued ids that came from the scheduled queue (a final failure is
    /// announced: nobody watches them go out).
    scheduled: HashSet<String>,
    /// Transient failures per queued id.
    tries: HashMap<String, u32>,
    /// No queued message is sent before this (UTC ms; after a transient
    /// failure).
    retry_at: Option<i64>,
    /// The `THREAD_INDEX` revision last written.
    threads_rev: Option<u64>,
    /// When the thread data was last written.
    threads_written: Option<Instant>,
    /// The `LATER` revision last written.
    later_rev: Option<u64>,
    /// When the Later data was last written.
    later_written: Option<Instant>,
    /// Later's messages as last written (`room::render::message_key`):
    /// later writes carry only the ones that changed.
    later_keys: HashMap<String, u64>,
}

impl ServerCache {
    /// Stops a running send of queued messages: no further queued message
    /// is posted with the session's credentials ([`close`]).
    fn abort_sends(&mut self) {
        if let Some(h) = self.send_task.take() {
            h.abort();
        }
    }

    /// A queued message was sent, dropped or given up.
    fn forget_queued(&mut self, id: &str) {
        self.queued.remove(id);
        self.scheduled.remove(id);
        self.tries.remove(id);
    }

    /// The next pass writes everything shown, as if nothing was written.
    fn write_all_again(&mut self) {
        self.captured.clear();
        self.needs_full = true;
        self.outbox_sig = None;
        self.later_keys.clear();
    }

    /// The open cache (searches read it).
    pub fn handle(&self) -> Option<&CacheHandle> {
        self.handle.as_ref()
    }

    pub fn is_open(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether the cache is being opened for an account.
    pub fn is_opening(&self) -> bool {
        self.opening.is_some()
    }

    /// Whether the outbox (and scheduled messages) of this account are kept
    /// on disk: caching is on and opening did not fail.
    pub fn persists(&self, caches: &Caches) -> bool {
        caches.enabled() && self.failed_for.is_none()
    }

    /// Whether a message waits to be sent automatically.
    pub fn is_queued(&self, id: &str) -> bool {
        self.queued.contains(id)
    }

    /// When queued messages are tried again after a transient failure
    /// (UTC ms), for the app's timer.
    pub fn retry_at(&self) -> Option<i64> {
        self.retry_at.filter(|_| !self.queued.is_empty())
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Opened(ServerId, String, Box<Result<Opened, String>>),
    Timeline(ServerId, TimelineKey, Epoch, Option<Box<CachedTimeline>>),
    /// A queued message was handled: server, session generation, result.
    Sent(ServerId, u64, Box<Sent>),
    Size(u64),
    /// "Clear" finished (the first error, if any).
    Cleared(Result<(), String>),
}

/// A queued message to send.
#[derive(Debug)]
pub struct QueuedSend {
    pub message: RcMessage,
    /// The encrypted form (encrypted rooms).
    pub enc: Option<Result<OutgoingEncrypted, rocketchat::Error>>,
    /// It may have been delivered (and deleted) before the app quit.
    pub check_deleted: bool,
}

/// What became of a queued message.
#[derive(Debug, Clone)]
pub struct Sent {
    pub id: String,
    pub rid: String,
    pub tmid: Option<String>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Sent(Box<RcMessage>),
    /// It was delivered before and deleted since: dropped.
    Deleted,
    Failed(Failure),
    /// Not tried: an earlier message of its room failed.
    Skipped(Failure),
}

fn wrap(m: Msg) -> Message {
    Message::Cache(m)
}

/// The server answers (realtime up, initial load in).
pub(crate) fn online(s: &ServerState) -> bool {
    s.session.is_some()
        && s.workspace.last_sync().is_some()
        && !s.workspace.from_cache()
        && *s.workspace.connection() == ConnectionState::Connected
}

/// Data in the workspace came from the server in this session.
fn live(ws: &Workspace) -> bool {
    ws.last_sync().is_some() && !ws.from_cache()
}

/// Pending messages hold the plaintext until they are encrypted at send
/// time, so only those of rooms known to be unencrypted are persisted. One
/// read from disk stays while its room is not known yet (the cached rooms
/// were cleared, the initial load is not in); a room holding a room key
/// counts as encrypted.
fn persistable(ws: &Workspace, m: &RcMessage, from_disk: bool) -> bool {
    if m.is_encrypted() || m.e2e.is_some() {
        return false;
    }
    room_plaintext_on_disk(ws, &m.rid).unwrap_or(from_disk && !live(ws))
}

/// Whether plaintext of room `rid` may be written to disk: its room and
/// subscription are known, it is not encrypted and holds no room key (it is
/// not, and was not, an E2E room). `None`: its room object is not known.
pub(crate) fn room_plaintext_on_disk(ws: &Workspace, rid: &str) -> Option<bool> {
    match ws.room(rid) {
        Some(e) if e.sub.as_ref().is_some_and(|s| s.e2e_key.is_some()) => Some(false),
        Some(e) if e.room.is_some() => Some(e.sub.is_some() && !e.is_encrypted()),
        _ => None,
    }
}

/// After every update (before the changes are taken): opens the cache,
/// writes changes, persists the outbox, seeds opened timelines while
/// offline and sends queued messages once the server is back.
pub fn after_update(
    s: &mut ServerState,
    caches: &Caches,
    last_room: Option<&str>,
) -> Task<Message> {
    let Some(base) = caches.base.as_ref() else {
        // No cache: queued messages (due scheduled ones) still go out.
        return send_queued(s);
    };
    let Some(user) = s.credentials.as_ref().map(|c| c.user_id.clone()) else {
        return Task::none();
    };
    if s.cache.user.as_deref() != Some(user.as_str()) {
        if s.cache.opening.as_deref() == Some(user.as_str()) {
            return Task::none();
        }
        if s.cache.failed_for.as_deref() == Some(user.as_str()) {
            return send_queued(s);
        }
        s.cache = ServerCache {
            opening: Some(user.clone()),
            ..Default::default()
        };
        let dir = server_dir(base, &s.id);
        let preload = last_room.map(TimelineKey::room);
        let sid = s.id.clone();
        return Task::perform(CacheHandle::open(dir, user.clone(), preload), move |r| {
            wrap(Msg::Opened(
                sid.clone(),
                user.clone(),
                Box::new(r.map_err(|e| e.to_string())),
            ))
        });
    }
    let Some(handle) = s.cache.handle.clone() else {
        return Task::none();
    };
    if handle.take_write_failed() {
        // A dropped batch: everything is written again.
        s.cache.write_all_again();
    }
    let mut batch = Batch::default();
    if live(&s.workspace) {
        let full = std::mem::take(&mut s.cache.needs_full);
        collect_documents(&s.workspace, full, &mut batch);
        collect_timelines(&s.workspace, &mut s.cache, full, &mut batch);
        collect_threads(s, full, Instant::now(), &mut batch);
        collect_later(s, full, Instant::now(), &mut batch);
    }
    collect_outbox(s, &mut batch);
    handle.write(batch);
    let seeds = ask_seeds(s, &handle);
    let sends = send_queued(s);
    Task::batch([seeds, sends])
}

/// The sidebar's thread data (once the workspace is live) when it changed,
/// at most every [`THREADS_WRITE_INTERVAL`] (right away with a full write).
/// Changes are seen by revision: they may be marked after this pass took
/// the workspace's pending changes (the thread config is applied later).
fn collect_threads(s: &mut ServerState, full: bool, now: Instant, batch: &mut Batch) {
    if !live(&s.workspace) {
        return;
    }
    let rev = s.workspace.revision(Dirty::THREAD_INDEX);
    let changed = s.cache.threads_rev != Some(rev);
    let due = s
        .cache
        .threads_written
        .is_none_or(|t| now.duration_since(t) >= THREADS_WRITE_INTERVAL);
    if !full && !(changed && due) {
        return;
    }
    s.cache.threads_rev = Some(rev);
    s.cache.threads_written = Some(now);
    batch.threads = Some(crate::workspace::threads::cached_threads(
        &s.workspace,
        &s.threads,
    ));
}

/// The Later view's data ([`cached_later`]) once the workspace is live,
/// when it changed, at most every [`THREADS_WRITE_INTERVAL`] (right away
/// with a full write).
fn collect_later(s: &mut ServerState, full: bool, now: Instant, batch: &mut Batch) {
    if !live(&s.workspace) {
        return;
    }
    let rev = s.workspace.revision(Dirty::LATER);
    let changed = s.cache.later_rev != Some(rev);
    let due = s
        .cache
        .later_written
        .is_none_or(|t| now.duration_since(t) >= THREADS_WRITE_INTERVAL);
    if !full && !(changed && due) {
        return;
    }
    s.cache.later_rev = Some(rev);
    s.cache.later_written = Some(now);
    batch.later = Some(later_write(&s.workspace, &mut s.cache.later_keys, full));
}

/// The Later write: everything (`full`, or nothing written yet), else
/// the messages that changed since the last write and the ones gone.
fn later_write(
    ws: &Workspace,
    written: &mut HashMap<String, u64>,
    full: bool,
) -> rustket_core::cache::CachedLater {
    let idx = ws.later_index();
    let keys: HashMap<String, u64> = idx
        .starred()
        .chain(idx.copies())
        .map(|m| (m.id.clone(), crate::room::render::message_key(m)))
        .collect();
    if full || written.is_empty() {
        *written = keys;
        return cached_later(ws);
    }
    let mut l = cached_later_scans(ws);
    l.delta = true;
    l.messages = idx
        .starred()
        .chain(idx.copies())
        .filter(|m| written.get(&m.id) != keys.get(&m.id))
        .cloned()
        .collect();
    l.removed = written
        .keys()
        .filter(|id| !keys.contains_key(*id))
        .cloned()
        .collect();
    *written = keys;
    l
}

/// The rooms' Later scans, no messages.
fn cached_later_scans(ws: &Workspace) -> rustket_core::cache::CachedLater {
    rustket_core::cache::CachedLater {
        scans: ws
            .later_index()
            .scans()
            .map(|(rid, s)| rustket_core::cache::CachedLaterScan {
                rid: rid.to_owned(),
                scanned_at: s.scanned_at,
                scanned_lm: s.lm,
            })
            .collect(),
        ..Default::default()
    }
}

/// What the Later index holds: starred messages, reminder copies and the
/// rooms' scans (messages pass `sanitize` when written).
pub fn cached_later(ws: &Workspace) -> rustket_core::cache::CachedLater {
    let idx = ws.later_index();
    rustket_core::cache::CachedLater {
        messages: idx.starred().chain(idx.copies()).cloned().collect(),
        ..cached_later_scans(ws)
    }
}

fn full_state(ws: &Workspace) -> CachedState {
    CachedState {
        me: ws.me().cloned(),
        server_info: ws.server_info().cloned(),
        settings: settings(ws),
        permissions: permissions(ws),
        roles: ws.roles().values().cloned().collect(),
        subscriptions: ws.rooms().values().filter_map(|e| e.sub.clone()).collect(),
        rooms: ws
            .rooms()
            .values()
            .filter(|e| e.sub.is_some())
            .filter_map(|e| e.room.clone())
            .collect(),
        custom_emoji: ws.emoji().custom().cloned().collect(),
        custom_statuses: ws.custom_statuses().cloned().collect(),
        saved_at: None,
        // Written separately (`collect_threads`, `collect_later`).
        threads: Default::default(),
        later: Default::default(),
    }
}

fn permissions(ws: &Workspace) -> Vec<Permission> {
    ws.permissions()
        .iter()
        .map(|(id, roles)| Permission {
            id: id.clone(),
            roles: roles.clone(),
            updated_at: None,
        })
        .collect()
}

/// Documents of the changed areas (all of them after an initial load).
fn collect_documents(ws: &Workspace, full: bool, batch: &mut Batch) {
    let ch: &Changes = ws.pending_changes();
    if full || ch.areas.contains(Dirty::ALL) {
        batch.full = Some(Box::new(full_state(ws)));
        return;
    }
    let a = ch.areas;
    if a.contains(Dirty::ME) {
        batch.me = ws.me().cloned();
    }
    if a.contains(Dirty::SERVER_INFO) {
        batch.server_info = ws.server_info().cloned();
    }
    if a.contains(Dirty::SETTINGS) {
        batch.settings = Some(settings(ws));
    }
    if a.contains(Dirty::PERMISSIONS) {
        batch.permissions = Some(permissions(ws));
    }
    if a.contains(Dirty::ROLES) {
        batch.roles = Some(ws.roles().values().cloned().collect());
    }
    if a.contains(Dirty::EMOJI) {
        batch.custom_emoji = Some(ws.emoji().custom().cloned().collect());
    }
    if a.contains(Dirty::CUSTOM_STATUSES) {
        batch.custom_statuses = Some(ws.custom_statuses().cloned().collect());
    }
    if a.contains(Dirty::ROOMS) {
        for rid in &ch.rooms {
            // Previewed rooms (no subscription) are not kept.
            let entry = ws.room(rid).filter(|e| e.sub.is_some());
            batch.rooms.push((
                rid.clone(),
                entry.and_then(|e| e.sub.clone()),
                entry.and_then(|e| e.room.clone()),
            ));
        }
    }
}

/// Public settings only: the workspace never takes private ones
/// (`Workspace::apply_setting_event`).
fn settings(ws: &Workspace) -> Vec<Setting> {
    ws.settings()
        .iter()
        .map(|(id, value)| Setting {
            id: id.to_owned(),
            value: value.clone(),
            ..Default::default()
        })
        .collect()
}

/// The newest messages of a loaded timeline (oldest first).
fn newest(t: &Timeline) -> Vec<RcMessage> {
    let mut out: Vec<RcMessage> = t
        .iter()
        .rev()
        .filter(|i| !i.is_pending())
        .take(MESSAGES_PER_TIMELINE)
        .map(|i| i.message.clone())
        .collect();
    out.reverse();
    out
}

/// Loaded timelines of joined rooms that changed since they were last
/// written.
fn collect_timelines(ws: &Workspace, cache: &mut ServerCache, full: bool, batch: &mut Batch) {
    let ch = ws.pending_changes();
    let joined = |rid: &str| ws.room(rid).is_some_and(|e| e.sub.is_some());
    let rooms: Vec<String> = if full {
        ws.rooms_with_timelines().into_iter().collect()
    } else {
        ch.timelines.iter().cloned().collect()
    };
    for rid in rooms {
        let Some(t) = ws.timeline(&rid) else { continue };
        if !t.pagination.loaded || !joined(&rid) {
            continue;
        }
        let key = TimelineKey::room(rid.as_str());
        let sig = (t.epoch(), t.version());
        if cache.captured.get(&key) == Some(&sig) {
            continue;
        }
        cache.captured.insert(key.clone(), sig);
        batch.timelines.push(CachedTimeline {
            key,
            parent: None,
            messages: newest(t),
        });
    }
    for tmid in &ch.threads {
        let Some(t) = ws.thread(tmid) else { continue };
        if !t.pagination.loaded || !joined(t.rid()) {
            continue;
        }
        let key = TimelineKey::thread(t.rid(), tmid.as_str());
        let sig = (t.epoch(), t.version());
        if cache.captured.get(&key) == Some(&sig) {
            continue;
        }
        cache.captured.insert(key.clone(), sig);
        batch.timelines.push(CachedTimeline {
            key,
            parent: t.parent().cloned(),
            messages: newest(t),
        });
    }
}

/// Persists the outbox when it changed. Messages that appear while the
/// server is not reachable are queued for sending.
fn collect_outbox(s: &mut ServerState, batch: &mut Batch) {
    if !s.cache.outbox_restored {
        return;
    }
    let up = online(s);
    let ws = &s.workspace;
    let c = &mut s.cache;
    let mut items: Vec<_> = ws.outbox_items().collect();
    items.sort_by(|a, b| (a.message.ts, &a.message.id).cmp(&(b.message.ts, &b.message.id)));
    let ids: HashSet<&str> = items.iter().map(|i| i.message.id.as_str()).collect();
    c.known.retain(|id| ids.contains(id.as_str()));
    c.queued.retain(|id| ids.contains(id.as_str()));
    for i in &items {
        if c.known.insert(i.message.id.clone()) && !up {
            c.queued.insert(i.message.id.clone());
        }
    }
    c.unconfirmed.retain(|id| ids.contains(id.as_str()));
    let scheduled = ws.scheduled().sorted();
    let ids: HashSet<&str> = ids
        .into_iter()
        .chain(scheduled.iter().map(|s| s.id()))
        .collect();
    c.restored.retain(|id| ids.contains(id.as_str()));
    let mut entries: Vec<OutboxEntry> = items
        .iter()
        .filter(|i| persistable(ws, &i.message, c.restored.contains(&i.message.id)))
        .map(|i| {
            let sending = i.state == DeliveryState::Sending;
            OutboxEntry {
                message: i.message.clone(),
                queued: c.queued.contains(&i.message.id) || sending,
                maybe_sent: sending || c.unconfirmed.contains(&i.message.id),
                error: match &i.state {
                    DeliveryState::Failed { error } => Some(error.clone()),
                    _ => None,
                },
                send_at: None,
                scheduled: c.scheduled.contains(&i.message.id),
                reactions: crate::room::composer::poll_reactions_peek(&s.id, &i.message.id),
            }
        })
        .collect();
    // Scheduled messages: only those of rooms known to be unencrypted, like
    // pending ones (the text is never logged nor synced).
    entries.extend(
        scheduled
            .iter()
            .filter(|e| persistable(ws, &e.message, c.restored.contains(e.id())))
            .map(|e| OutboxEntry {
                message: e.message.clone(),
                queued: false,
                maybe_sent: false,
                error: None,
                send_at: Some(e.send_at),
                scheduled: false,
                reactions: crate::room::composer::poll_reactions_peek(&s.id, e.id()),
            }),
    );
    let sig: Vec<OutboxSig> = entries
        .iter()
        .map(|e| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            e.message.msg.hash(&mut h);
            (
                e.message.id.clone(),
                e.queued,
                e.maybe_sent,
                e.error.clone(),
                e.send_at,
                h.finish(),
                e.reactions.clone(),
            )
        })
        .collect();
    if c.outbox_sig.as_ref() != Some(&sig) {
        c.outbox_sig = Some(sig);
        batch.outbox = Some(entries);
    }
}

/// While the server is not reachable (or the cache is shown), timelines
/// opened without their first page get their cached messages.
fn ask_seeds(s: &mut ServerState, handle: &CacheHandle) -> Task<Message> {
    let ws = &s.workspace;
    if !(ws.from_cache() || *ws.connection() != ConnectionState::Connected) {
        return Task::none();
    }
    let ch = ws.pending_changes();
    let mut wanted: Vec<(TimelineKey, Epoch)> = Vec::new();
    let unloaded = |t: &Timeline| !t.pagination.loaded && !t.has_seeded();
    for rid in &ch.timelines {
        if let Some(t) = ws.timeline(rid).filter(|t| unloaded(t)) {
            wanted.push((TimelineKey::room(rid.as_str()), t.epoch()));
        }
    }
    for tmid in &ch.threads {
        if let Some(t) = ws.thread(tmid).filter(|t| unloaded(t)) {
            wanted.push((TimelineKey::thread(t.rid(), tmid.as_str()), t.epoch()));
        }
    }
    let mut tasks = Vec::new();
    for (key, epoch) in wanted {
        if !s.cache.seed_asked.insert((key.clone(), epoch)) {
            continue;
        }
        let (h, sid) = (handle.clone(), s.id.clone());
        let k = key.clone();
        tasks.push(Task::perform(
            async move { h.timeline(k).await },
            move |t| {
                wrap(Msg::Timeline(
                    sid.clone(),
                    key.clone(),
                    epoch,
                    t.map(Box::new),
                ))
            },
        ));
    }
    Task::batch(tasks)
}

/// Sends queued messages that are waiting (failed) once the server answers:
/// one sequence at a time, oldest first. Messages of rooms that need
/// encryption wait until the E2E manager can encrypt for them.
fn send_queued(s: &mut ServerState) -> Task<Message> {
    if s.cache.queued.is_empty() || !s.cache.sending.is_empty() || !online(s) {
        return Task::none();
    }
    if s.cache
        .retry_at
        .is_some_and(|at| at > chrono::Utc::now().timestamp_millis())
    {
        return Task::none();
    }
    let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
        return Task::none();
    };
    let mut waiting: Vec<(RcMessage, Option<String>)> = s
        .workspace
        .outbox_items()
        .filter(|i| s.cache.queued.contains(&i.message.id))
        .filter_map(|i| match &i.state {
            DeliveryState::Failed { error } => Some((i.message.clone(), Some(error.clone()))),
            _ => None,
        })
        .collect();
    waiting.sort_by(|(a, _), (b, _)| (a.ts, &a.id).cmp(&(b.ts, &b.id)));
    let mut held: HashSet<String> = HashSet::new();
    let mut ready = Vec::new();
    for (m, error) in waiting {
        if held.contains(&m.rid) {
            continue;
        }
        match crate::e2e::outgoing::mode(&s.workspace, &m.rid) {
            Mode::Plain | Mode::Encrypt(_) => {}
            // Not ready to encrypt (keys loading, room key missing, room
            // state unknown): the room's messages wait, never downgraded.
            Mode::Blocked | Mode::Unencrypted => {
                held.insert(m.rid.clone());
                let why = crate::e2e::outgoing::blocked_text(&s.workspace, &m.rid);
                if error.as_deref() != Some(why.as_str()) {
                    s.workspace
                        .mark_pending_failed(&m.rid, m.tmid.as_deref(), &m.id, &why);
                }
                continue;
            }
        }
        let enc = crate::e2e::outgoing::prepare_send(&s.workspace, &m);
        ready.push(QueuedSend {
            check_deleted: s.cache.unconfirmed.contains(&m.id),
            message: m,
            enc,
        });
    }
    if ready.is_empty() {
        return Task::none();
    }
    for q in &ready {
        let m = &q.message;
        s.cache.sending.insert(m.id.clone());
        s.workspace
            .mark_pending_sending(&m.rid, m.tmid.as_deref(), &m.id);
        // The composer must not take it for a lost send.
        crate::room::composer::outbox_start(&s.id, &m.rid, &m.id);
    }
    let (sid, generation) = (s.id.clone(), s.generation);
    let (task, handle) = Task::run(send_in_order(rest, ready), move |r| {
        wrap(Msg::Sent(sid.clone(), generation, Box::new(r)))
    })
    .abortable();
    s.cache.send_task = Some(handle);
    task
}

/// Sends `items` one after the other (each waits for the previous answer,
/// so the server keeps their order); after a failure the rest of that
/// room is skipped. Yields one result per item.
pub fn send_in_order(rest: RestClient, items: Vec<QueuedSend>) -> impl Stream<Item = Sent> {
    iced::stream::channel(8, async move |mut out| {
        let mut failed: HashMap<String, Failure> = HashMap::new();
        for q in items {
            let (id, rid, tmid) = (
                q.message.id.clone(),
                q.message.rid.clone(),
                q.message.tmid.clone(),
            );
            let outcome = match failed.get(&rid) {
                Some(e) => Outcome::Skipped(e.clone()),
                None => send_one(&rest, q).await,
            };
            if let Outcome::Failed(e) = &outcome {
                failed.insert(rid.clone(), e.clone());
            }
            let _ = out
                .send(Sent {
                    id,
                    rid,
                    tmid,
                    outcome,
                })
                .await;
        }
    })
}

async fn send_one(rest: &RestClient, q: QueuedSend) -> Outcome {
    let QueuedSend {
        message: m,
        enc,
        check_deleted,
    } = q;
    let enc = match enc {
        Some(Err(e)) => return Outcome::Failed(e.into()),
        other => other,
    };
    if check_deleted {
        // Possibly delivered before the app quit: sending the id again
        // after a deletion would bring the message back.
        if let Ok(found) = rest.get_message(&m.id).await
            && found.rid == m.rid
        {
            return Outcome::Sent(Box::new(found));
        }
        let since =
            m.ts.map(|t| t.timestamp_millis() - DELETED_LOOKBACK_MS)
                .and_then(RcDate::from_millis)
                .or_else(|| RcDate::from_millis(0));
        if let Some(since) = since {
            match rest.chat_sync_messages(&m.rid, since).await {
                Ok(sync) if sync.deleted.iter().any(|d| d.id == m.id) => return Outcome::Deleted,
                Ok(_) => {}
                Err(e) => return Outcome::Failed(e.into()),
            }
        }
    }
    let (id, rid) = (m.id.clone(), m.rid.clone());
    let out = OutgoingMessage {
        id: Some(m.id),
        rid: m.rid,
        msg: m.msg,
        tmid: m.tmid,
        tshow: m.tshow,
        ..Default::default()
    };
    match crate::e2e::outgoing::send_outgoing(rest.clone(), out, enc).await {
        Ok(m) => Outcome::Sent(Box::new(m)),
        // Sent before (the app quit before it heard back): the server
        // keeps the first message with this id.
        Err(e) => match rest.get_message(&id).await {
            Ok(found) if found.rid == rid => Outcome::Sent(Box::new(found)),
            _ => Outcome::Failed(e.into()),
        },
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Opened(sid, user, r) => on_opened(app, sid, user, *r),
        Msg::Timeline(sid, key, epoch, t) => {
            let Some(t) = t else {
                return Task::none();
            };
            let Some(s) = app.servers.get_mut(&sid) else {
                return Task::none();
            };
            let ws = &mut s.workspace;
            let current = match &key.tmid {
                None => ws.timeline(&key.rid).map(Timeline::epoch),
                Some(tmid) => ws.thread(tmid).map(Timeline::epoch),
            };
            if current == Some(epoch) {
                let CachedTimeline {
                    parent, messages, ..
                } = *t;
                ws.seed_cached_messages(&key.rid, key.tmid.as_deref(), parent, messages);
            }
            Task::none()
        }
        Msg::Sent(sid, generation, r) => {
            crate::room::composer::outbox_forget(&sid, &r.id);
            let Some(s) = app.servers.get_mut(&sid) else {
                return Task::none();
            };
            s.cache.sending.remove(&r.id);
            if s.generation != generation {
                return Task::none();
            }
            let Sent {
                id,
                rid,
                tmid,
                outcome,
            } = *r;
            let tmid = tmid.as_deref();
            let tried = matches!(outcome, Outcome::Failed(_));
            match outcome {
                Outcome::Sent(m) => {
                    s.cache.forget_queued(&id);
                    s.cache.unconfirmed.remove(&id);
                    s.workspace.apply_message(*m);
                    // A poll queued offline: its option reactions now.
                    if let Some(list) = crate::room::composer::poll_reactions_take(&sid, &id)
                        && let Some(rest) = s.session.as_ref().map(|h| h.rest().clone())
                    {
                        return Task::perform(
                            crate::room::composer::add_reactions(rest, id, list),
                            |r| {
                                if let Err(e) = r {
                                    tracing::warn!(error = %e, "poll reactions not added");
                                }
                            },
                        )
                        .discard();
                    }
                }
                Outcome::Deleted => {
                    s.cache.forget_queued(&id);
                    s.workspace.discard_pending(&rid, tmid, &id);
                }
                Outcome::Failed(e) | Outcome::Skipped(e) => {
                    tracing::warn!(error = %e, "queued message not sent");
                    let tries = s.cache.tries.get(&id).copied().unwrap_or(0);
                    if *s.workspace.connection() != ConnectionState::Connected {
                        // The connection went away meanwhile: it waits.
                        s.workspace
                            .mark_pending_failed(&rid, tmid, &id, &t!("cache.waiting"));
                    } else if retries(e.error(), tried, tries) {
                        // A blip (network, server overloaded): tried again
                        // later, as it goes out with nobody watching.
                        if tried {
                            s.cache.tries.insert(id.clone(), tries + 1);
                            let at = chrono::Utc::now().timestamp_millis()
                                + retry_delay_ms(tries + 1, e.error());
                            s.cache.retry_at = Some(s.cache.retry_at.map_or(at, |r| r.max(at)));
                        }
                        s.workspace
                            .mark_pending_failed(&rid, tmid, &id, &t!("cache.waiting"));
                    } else {
                        let scheduled = s.cache.scheduled.contains(&id);
                        s.cache.forget_queued(&id);
                        let error = e.user_text();
                        let pending = scheduled
                            .then(|| {
                                s.workspace
                                    .outbox_items()
                                    .find(|i| i.message.id == id)
                                    .map(|i| i.message.clone())
                            })
                            .flatten();
                        let Some(m) = pending else {
                            s.workspace.mark_pending_failed(&rid, tmid, &id, &error);
                            return Task::none();
                        };
                        // Nobody may be watching it go out: it goes back to
                        // Drafts & scheduled as a missed entry (kept on disk,
                        // listed by the "not sent" dialog until the user
                        // decides), not a failed row in a room.
                        s.workspace.return_failed_scheduled(m, &error);
                        let room = s
                            .workspace
                            .room(&rid)
                            .map(|r| r.fname().unwrap_or(r.name()).to_owned())
                            .unwrap_or_default();
                        app.missed_scheduled();
                        return app.toast(
                            BannerKind::Error,
                            t!("scheduled.failed_title"),
                            t!("scheduled.failed_body", room = room, error = error),
                        );
                    }
                }
            }
            Task::none()
        }
        Msg::Size(n) => {
            app.offline.size = Some(n);
            Task::none()
        }
        Msg::Cleared(r) => {
            app.offline.clearing = false;
            // What is shown is written again in full (the outbox too), after
            // the clear on each writer's queue; also after a partial failure.
            for s in app.servers.values_mut() {
                if s.cache.handle.is_some() {
                    s.cache.write_all_again();
                }
            }
            let toast = match r {
                Ok(()) => app.toast(BannerKind::Success, t!("cache.cleared"), String::new()),
                Err(e) => app.toast(BannerKind::Error, t!("cache.clear_failed"), e),
            };
            Task::batch([toast, size_task(app)])
        }
    }
}

fn on_opened(
    app: &mut App,
    sid: ServerId,
    user: String,
    r: Result<Opened, String>,
) -> Task<Message> {
    let last_room = app.config.server(&sid).and_then(|e| e.last_room.clone());
    let active = app.active.as_ref() == Some(&sid);
    let current = app.servers.get(&sid).is_some_and(|s| {
        s.cache.opening.as_deref() == Some(user.as_str())
            && s.credentials.as_ref().map(|c| c.user_id.as_str()) == Some(user.as_str())
    });
    if !current {
        // Signed out, removed or another account meanwhile: the handle is
        // dropped; a cache purged while it was opening is deleted again
        // (the open may have recreated it).
        let dir = app.offline.base.as_ref().map(|b| server_dir(b, &sid));
        if let (Some(dir), Ok(o)) = (dir, r)
            && app.offline.purged.remove(&dir)
        {
            return Task::perform(o.handle.destroy(), |r| {
                if let Err(e) = r {
                    tracing::warn!(error = %e, "cannot delete the offline cache");
                }
                Message::Noop
            });
        }
        return Task::none();
    }
    let Some(s) = app.servers.get_mut(&sid) else {
        return Task::none();
    };
    s.cache.opening = None;
    let o = match r {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(server = %sid, error = %e, "offline cache unavailable");
            s.cache.failed_for = Some(user);
            return Task::none();
        }
    };
    if let Some(path) = &o.report.moved_aside {
        tracing::warn!(path = %path.display(), "unusable offline cache moved aside");
    }
    let Opened {
        handle,
        state,
        outbox,
        interrupted_uploads,
        preloaded,
        report: _,
    } = o;
    s.cache.handle = Some(handle);
    s.cache.user = Some(user);
    s.cache.needs_full = true;
    s.cache.saved_at = state.saved_at;
    let shown = !state.is_empty() && s.workspace.apply_cached(state);
    restore_outbox(s, outbox);
    let mut tasks = Vec::new();
    if !interrupted_uploads.is_empty() {
        let names = interrupted_uploads.join(", ");
        tasks.push(app.toast(
            BannerKind::Warning,
            t!("cache.uploads_interrupted"),
            t!("cache.uploads_interrupted_body", names = names),
        ));
    }
    if !shown {
        return Task::batch(tasks);
    }
    let Some(s) = app.servers.get(&sid) else {
        return Task::batch(tasks);
    };
    if active
        && s.active_room.is_none()
        && s.pending_room.is_none()
        && let Some(rid) = last_room.filter(|r| s.workspace.room(r).is_some())
    {
        tasks.push(app.open_room(&sid, &rid));
        if let Some(t) = preloaded.filter(|t| t.key.rid == rid && t.key.tmid.is_none())
            && let Some(s) = app.servers.get_mut(&sid)
        {
            s.workspace
                .seed_cached_messages(&rid, None, None, t.messages);
        }
    }
    Task::batch(tasks)
}

/// Puts the persisted outbox back: every message waits as failed ("waiting
/// for the connection" for queued ones, which are sent automatically).
fn restore_outbox(s: &mut ServerState, outbox: Vec<OutboxEntry>) {
    let present: HashSet<String> = s
        .workspace
        .outbox_items()
        .map(|i| i.message.id.clone())
        .collect();
    for e in outbox {
        let m = e.message;
        if present.contains(&m.id) {
            continue;
        }
        // A poll queued offline adds its option reactions once sent, after
        // a restart too.
        crate::room::composer::poll_reactions_restore(&s.id, &m.id, &e.reactions);
        if let Some(at) = e.send_at {
            s.cache.restored.insert(m.id.clone());
            s.workspace
                .restore_scheduled(rustket_core::scheduled::Scheduled::new(m, at));
            continue;
        }
        let (id, rid, tmid) = (m.id.clone(), m.rid.clone(), m.tmid.clone());
        s.workspace.add_pending(m);
        let error = match (e.queued, e.error) {
            (true, _) | (false, None) => t!("cache.waiting").into_owned(),
            (false, Some(err)) => err,
        };
        s.workspace
            .mark_pending_failed(&rid, tmid.as_deref(), &id, &error);
        s.cache.known.insert(id.clone());
        s.cache.restored.insert(id.clone());
        if e.queued && e.maybe_sent {
            s.cache.unconfirmed.insert(id.clone());
        }
        if e.queued && e.scheduled {
            s.cache.scheduled.insert(id.clone());
        }
        if e.queued {
            s.cache.queued.insert(id);
        }
    }
    s.cache.outbox_restored = true;
}

/// Puts a scheduled message whose time came (or that the user sends now)
/// into the outbox as a queued entry: it goes out like a message written
/// offline (`send_queued`), and a failure leaves a failed entry to retry or
/// discard.
pub fn enqueue_scheduled(s: &mut ServerState, m: RcMessage) {
    let (id, rid, tmid) = (m.id.clone(), m.rid.clone(), m.tmid.clone());
    s.workspace.add_pending(m);
    s.workspace
        .mark_pending_failed(&rid, tmid.as_deref(), &id, &t!("cache.waiting"));
    s.cache.known.insert(id.clone());
    s.cache.scheduled.insert(id.clone());
    s.cache.queued.insert(id);
}

/// Scheduled messages of server `s` whose time came go into the outbox
/// while the server answers; returns the ones that turned out missed (their
/// time passed while they could not be sent).
pub fn tick_scheduled(s: &mut ServerState, now_ms: i64) -> Vec<String> {
    if s.workspace.scheduled().is_empty() || !s.is_logged_in() {
        return Vec::new();
    }
    let up = online(s);
    let due = s.workspace.take_due_scheduled(now_ms, up);
    for e in due.send {
        enqueue_scheduled(s, e.message);
    }
    due.missed
}

/// Deletes a server's cache (logout, rejected token, server removal).
pub fn purge(app: &mut App, sid: &ServerId) -> Task<Message> {
    let Some(base) = app.offline.base.clone() else {
        return Task::none();
    };
    let dir = server_dir(&base, sid);
    let cache = app
        .servers
        .get_mut(sid)
        .map(|s| std::mem::take(&mut s.cache))
        .unwrap_or_default();
    if cache.handle.is_none() && cache.opening.is_some() {
        app.offline.purged.insert(dir.clone());
    }
    let handle = cache.handle;
    Task::perform(
        async move {
            match handle {
                Some(h) => h.destroy().await,
                None => tokio::task::spawn_blocking(move || rustket_core::cache::destroy(&dir))
                    .await
                    .unwrap_or_else(|e| Err(std::io::Error::other(e))),
            }
        },
        |r| {
            if let Err(e) = r {
                tracing::warn!(error = %e, "cannot delete the offline cache");
            }
            Message::Noop
        },
    )
}

/// Settings → Storage → "Clear": every server's cached data (unsent
/// messages stay). What is shown stays shown and is written again as it
/// changes.
pub fn clear_all(app: &mut App) -> Task<Message> {
    let Some(base) = app.offline.base.clone() else {
        return Task::none();
    };
    app.offline.clearing = true;
    let mut handles = Vec::new();
    let mut closed = Vec::new();
    for s in app.servers.values_mut() {
        match s.cache.handle.clone() {
            // Written again in full once cleared (`Msg::Cleared`): a full
            // write queued now would land before the clear.
            Some(h) => handles.push(h),
            None if s.cache.opening.is_none() => closed.push(server_dir(&base, &s.id)),
            None => {}
        }
    }
    Task::perform(
        async move {
            let mut first: Result<(), String> = Ok(());
            for h in handles {
                if let Err(e) = h.clear().await {
                    tracing::warn!(error = %e, "cannot clear the offline cache");
                    first = first.and(Err(e.to_string()));
                }
            }
            let closed = tokio::task::spawn_blocking(move || {
                closed
                    .iter()
                    .map(|dir| rustket_core::cache::destroy(dir))
                    .find_map(Result::err)
            })
            .await;
            match closed {
                Ok(Some(e)) => first.and(Err(e.to_string())),
                Err(e) => first.and(Err(e.to_string())),
                Ok(None) => first,
            }
        },
        |r| wrap(Msg::Cleared(r)),
    )
}

/// Bytes on disk of every server's cache.
pub fn size_task(app: &App) -> Task<Message> {
    let Some(base) = app.offline.base.clone() else {
        return Task::none();
    };
    let dirs: Vec<PathBuf> = app
        .servers
        .keys()
        .map(|sid| server_dir(&base, sid))
        .collect();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                dirs.iter().map(|d| rustket_core::cache::dir_size(d)).sum()
            })
            .await
            .unwrap_or(0)
        },
        |n| wrap(Msg::Size(n)),
    )
}

/// What quitting writes: running uploads and the latest thread data (data
/// kept current live is current now).
fn quit_batch(s: &ServerState) -> Batch {
    let names = crate::room::upload::running_names(s.id.as_str());
    let threads = live(&s.workspace)
        .then(|| crate::workspace::threads::cached_threads(&s.workspace, &s.threads));
    let later = live(&s.workspace).then(|| cached_later(&s.workspace));
    Batch {
        interrupted_uploads: (!names.is_empty()).then_some(names),
        threads,
        later,
        ..Default::default()
    }
}

/// What a suspension writes: what quitting writes, without the running
/// uploads (the app names them at once; they were not cut by a quit).
fn suspend_batch(s: &ServerState) -> Batch {
    Batch {
        interrupted_uploads: None,
        ..quit_batch(s)
    }
}

/// Closes a server's cache without deleting it (the policy suspended the
/// server): what quitting writes, minus the running uploads, is queued, and
/// the worker stops after its queued writes once the handle is dropped
/// here. The next start of the account opens it again, outbox included.
pub fn close(s: &mut ServerState) {
    let mut cache = std::mem::take(&mut s.cache);
    cache.abort_sends();
    if let Some(h) = &cache.handle {
        h.write(suspend_batch(s));
    }
}

/// The app quits: running uploads are recorded (reported on the next
/// start), the thread data is written and queued writes are flushed
/// (bounded wait).
pub fn on_quit(app: &App) {
    for s in app.servers.values() {
        let Some(h) = &s.cache.handle else { continue };
        h.write(quit_batch(s));
        if !h.flush_blocking(QUIT_FLUSH) {
            tracing::warn!(server = %s.id, "offline cache not flushed before quitting");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use iced::futures::StreamExt;
    use rocketchat::ServerUrl;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A blip at the due time of an unattended message (scheduled, written
    /// offline) is tried again with a growing wait; a refusal is not.
    #[test]
    fn transient_send_failures_are_tried_again() {
        let blip = rocketchat::Error::Timeout("chat.sendMessage");
        let refused = rocketchat::Error::Api {
            status: 400,
            error: Some("error-not-allowed".into()),
            error_type: None,
            message: None,
            details: None,
        };
        assert!(retries(&blip, true, 0));
        assert!(retries(&blip, true, MAX_SEND_TRIES - 1));
        assert!(!retries(&blip, true, MAX_SEND_TRIES), "given up");
        assert!(retries(&blip, false, MAX_SEND_TRIES), "skipped: not a try");
        assert!(!retries(&refused, true, 0));
        assert!(!retries(&refused, false, 0));
        assert_eq!(retry_delay_ms(1, &blip), 15_000);
        assert_eq!(retry_delay_ms(2, &blip), 30_000);
        assert_eq!(retry_delay_ms(30, &blip), 300_000);
        let limited = rocketchat::Error::RateLimited {
            retry_after: Some(Duration::from_secs(600)),
            message: None,
        };
        assert_eq!(retry_delay_ms(1, &limited), 600_000);
    }

    type Log = Arc<Mutex<Vec<(String, String)>>>;

    /// A local HTTP server answering with `answer(path, body)` (status,
    /// JSON) and logging every request in order.
    async fn stub(answer: fn(&str, &str) -> (u16, String)) -> (ServerUrl, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Log = Arc::default();
        let log2 = log.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let log = log2.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let path = head
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
                    let (status, json) = answer(&path, &body);
                    log.lock().unwrap().push((path, body));
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (
            ServerUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            log,
        )
    }

    fn answer(path: &str, body: &str) -> (u16, String) {
        let fail = (400, r#"{"success":false,"error":"boom"}"#.to_owned());
        if path.contains("chat.sendMessage") {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let m = &v["message"];
            if m["_id"] == "bad" {
                return fail;
            }
            let msg = serde_json::json!({"_id": m["_id"], "rid": m["rid"], "msg": m["msg"]});
            return (
                200,
                serde_json::json!({"success": true, "message": msg}).to_string(),
            );
        }
        if path.contains("chat.syncMessages") {
            return (
                200,
                r#"{"success":true,"result":{"updated":[],"deleted":[{"_id":"gone"}]}}"#.to_owned(),
            );
        }
        fail
    }

    fn queued(id: &str, rid: &str, ms: i64, check_deleted: bool) -> QueuedSend {
        QueuedSend {
            message: RcMessage {
                id: id.into(),
                rid: rid.into(),
                msg: format!("text {id}"),
                ts: RcDate::from_millis(ms),
                ..Default::default()
            },
            enc: None,
            check_deleted,
        }
    }

    /// Closing the cache (suspension by the policy) aborts the send of
    /// queued messages: no further queued message is posted.
    #[test]
    fn close_aborts_the_queued_send() {
        let mut s = thread_server();
        let (_task, handle) = Task::<Message>::future(std::future::pending()).abortable();
        s.cache.send_task = Some(handle.clone());
        close(&mut s);
        assert!(handle.is_aborted());
        assert!(s.cache.send_task.is_none());
    }

    fn thread_server() -> ServerState {
        let entry = rustket_core::config::ServerEntry {
            id: rustket_core::ServerId("s-cache-threads".into()),
            url: "http://localhost:3000".into(),
            ..Default::default()
        };
        let mut s = ServerState::from_entry(&entry, &Default::default()).unwrap();
        s.workspace
            .set_thread_config(rustket_core::config::ThreadsMode::UnreadKept, [("a", "k")]);
        s
    }

    fn go_live(s: &mut ServerState) {
        s.workspace.apply_initial(rustket_core::sync::InitialData {
            me: rocketchat::models::User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![rocketchat::models::Subscription {
                id: "sub-a".into(),
                rid: "a".into(),
                name: "a".into(),
                open: true,
                lr: RcDate::from_millis(10),
                ..Default::default()
            }],
            next_since: RcDate::from_millis(1),
            ..Default::default()
        });
    }

    fn thread_parent(id: &str, tcount: u64) -> RcMessage {
        RcMessage {
            id: id.into(),
            rid: "a".into(),
            ts: RcDate::from_millis(1),
            tlm: RcDate::from_millis(10),
            tcount: Some(tcount),
            replies: Some(vec!["me".into()]),
            updated_at: RcDate::from_millis(tcount as i64),
            ..Default::default()
        }
    }

    fn collect(
        s: &mut ServerState,
        full: bool,
        at: Instant,
    ) -> Option<rustket_core::cache::CachedThreads> {
        let mut batch = Batch::default();
        collect_threads(s, full, at, &mut batch);
        batch.threads
    }

    /// A signed-in server with unencrypted room `a`, its outbox restored.
    fn room_a_server() -> ServerState {
        let mut s = thread_server();
        s.workspace.apply_initial(rustket_core::sync::InitialData {
            me: rocketchat::models::User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![rocketchat::models::Subscription {
                id: "sub-a".into(),
                rid: "a".into(),
                name: "a".into(),
                open: true,
                ..Default::default()
            }],
            rooms: vec![rocketchat::models::Room {
                id: "a".into(),
                name: Some("a".into()),
                ..Default::default()
            }],
            next_since: RcDate::from_millis(1),
            ..Default::default()
        });
        s.cache.outbox_restored = true;
        s
    }

    #[test]
    fn poll_reactions_survive_a_restart_with_the_outbox() {
        use crate::room::composer::{poll_reactions_restore, poll_reactions_take};
        let mut s = room_a_server();
        let m = s.workspace.new_local_message("a", "poll", None, false);
        let id = m.id.clone();
        s.workspace.add_pending(m);
        poll_reactions_restore(&s.id, &id, &[":one:".into(), ":two:".into()]);
        let mut batch = Batch::default();
        collect_outbox(&mut s, &mut batch);
        let entries = batch.outbox.expect("written");
        assert_eq!(entries[0].reactions, [":one:", ":two:"]);
        // The process ends: the in-memory registry is gone.
        assert!(poll_reactions_take(&s.id, &id).is_some());
        let mut s2 = thread_server();
        let mut stored = entries;
        // Only option reactions come back from the file.
        stored[0].reactions.push(":skull:".into());
        restore_outbox(&mut s2, stored);
        assert_eq!(
            poll_reactions_take(&s2.id, &id),
            Some(vec![":one:", ":two:"])
        );
    }

    #[test]
    fn scheduled_messages_are_persisted_restored_and_queued_when_due() {
        use rustket_core::scheduled::MISSED_GRACE_MS;
        let mut s = room_a_server();
        let m = s
            .workspace
            .new_local_message("a", "later text", None, false);
        let id = m.id.clone();
        s.workspace.schedule_message(m, 5_000);
        let mut batch = Batch::default();
        collect_outbox(&mut s, &mut batch);
        let entries = batch.outbox.expect("written");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].send_at, Some(5_000));
        assert!(!entries[0].queued);
        // Unchanged: not written again; an edited text is.
        let mut batch = Batch::default();
        collect_outbox(&mut s, &mut batch);
        assert!(batch.outbox.is_none());
        s.workspace.set_scheduled_text(&id, "edited");
        let mut batch = Batch::default();
        collect_outbox(&mut s, &mut batch);
        assert_eq!(batch.outbox.unwrap()[0].message.msg, "edited");
        // A restart puts it back in the queue, not in the timeline.
        let mut s2 = thread_server();
        restore_outbox(&mut s2, entries);
        assert_eq!(s2.workspace.scheduled().len(), 1);
        assert_eq!(s2.workspace.outbox_items().count(), 0);
        assert!(!s2.cache.is_queued(&id));
        // Not connected: within the grace time it waits, later it is missed.
        assert!(tick_scheduled(&mut s2, 5_000).is_empty());
        assert_eq!(s2.workspace.scheduled().len(), 1);
        assert_eq!(
            tick_scheduled(&mut s2, 5_001 + MISSED_GRACE_MS),
            Vec::<String>::new(),
            "signed out: nothing happens"
        );
        // Sent now: a queued outbox entry, waiting for the connection.
        let e = s2.workspace.take_scheduled_now(&id, 9_000).unwrap();
        enqueue_scheduled(&mut s2, e.message);
        assert!(s2.cache.is_queued(&id));
        assert!(s2.workspace.scheduled().is_empty());
        let item = s2.workspace.outbox_items().next().unwrap();
        assert!(matches!(item.state, DeliveryState::Failed { .. }));
        // Queued, it stays known as a scheduled one across a restart (its
        // final failure goes back to the scheduled ones).
        s2.cache.outbox_restored = true;
        let mut batch = Batch::default();
        collect_outbox(&mut s2, &mut batch);
        let entries = batch.outbox.expect("written");
        assert!(entries[0].queued && entries[0].scheduled && entries[0].send_at.is_none());
        let mut s3 = thread_server();
        restore_outbox(&mut s3, entries);
        assert!(s3.cache.is_queued(&id) && s3.cache.scheduled.contains(&id));
    }

    #[test]
    fn thread_data_is_written_live_and_throttled() {
        let mut s = thread_server();
        let t0 = Instant::now();
        let min = |m: u64| t0 + Duration::from_secs(m * 60);
        assert!(collect(&mut s, true, t0).is_none(), "not live yet");
        go_live(&mut s);
        assert!(collect(&mut s, false, t0).is_some(), "first live pass");
        assert!(collect(&mut s, false, min(5)).is_none(), "nothing changed");
        s.workspace
            .apply_thread_parents(vec![thread_parent("k", 1)]);
        let at = min(5);
        assert_eq!(collect(&mut s, false, at).unwrap().parents.len(), 1);
        s.workspace
            .apply_thread_parents(vec![thread_parent("k", 2)]);
        assert!(collect(&mut s, false, at).is_none(), "throttled");
        assert!(collect(&mut s, true, at).is_some(), "a full write");
        // A change marked after this pass took the pending changes (the
        // thread config is applied after the cache pass) is still seen.
        s.workspace.take_changes();
        s.workspace.set_thread_config(
            rustket_core::config::ThreadsMode::UnreadKept,
            [("a", "k"), ("a", "k2")],
        );
        s.workspace.take_changes();
        assert!(collect(&mut s, false, min(7)).is_some());
        assert!(collect(&mut s, false, min(9)).is_none());
    }

    #[test]
    fn writing_all_again_rewrites_later_in_full() {
        let mut s = thread_server();
        go_live(&mut s);
        s.cache.later_keys.insert("gone".into(), 1);
        s.cache.captured.insert(TimelineKey::room("a"), ((1, 1), 1));
        s.cache.write_all_again();
        assert!(s.cache.needs_full && s.cache.captured.is_empty());
        let l = later_write(&s.workspace, &mut s.cache.later_keys, false);
        assert!(!l.delta, "a full Later write, not a delta");
        assert!(l.removed.is_empty());
    }

    #[test]
    fn quitting_writes_the_thread_data_once_live() {
        let mut s = thread_server();
        assert!(quit_batch(&s).threads.is_none());
        go_live(&mut s);
        s.workspace
            .apply_thread_parents(vec![thread_parent("k", 1)]);
        let b = quit_batch(&s);
        let t = b.threads.unwrap();
        assert_eq!(t.parents.len(), 1);
        assert!(b.interrupted_uploads.is_none());
    }

    /// Quitting records the running uploads (reported at the next start as
    /// cut by the quit); a suspension does not: the app names them at once.
    #[test]
    fn a_suspension_does_not_record_running_uploads() {
        let entry = rustket_core::config::ServerEntry {
            id: rustket_core::ServerId("s-cache-suspend-uploads".into()),
            url: "http://localhost:3000".into(),
            ..Default::default()
        };
        let s = ServerState::from_entry(&entry, &Default::default()).unwrap();
        let handle = crate::room::upload::register_pending(s.id.as_str(), "report.pdf");
        assert_eq!(
            quit_batch(&s).interrupted_uploads,
            Some(vec!["report.pdf".to_owned()])
        );
        assert!(suspend_batch(&s).interrupted_uploads.is_none());
        crate::room::upload::cancel_server(s.id.as_str());
        assert!(handle.is_aborted());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_messages_go_out_in_order_one_at_a_time() {
        let (server, log) = stub(answer).await;
        let tls = crate::session::tls_for(&[]).unwrap();
        let rest = RestClient::new(server, &tls).unwrap();
        let items = vec![
            queued("a", "R1", 1_000, false),
            queued("bad", "R1", 2_000, false),
            queued("c", "R1", 3_000, false),
            queued("d", "R2", 4_000, false),
            queued("gone", "R3", 5_000_000, true),
        ];
        let results: Vec<Sent> = send_in_order(rest, items).collect().await;
        let got: Vec<(&str, &str)> = results
            .iter()
            .map(|r| {
                let o = match &r.outcome {
                    Outcome::Sent(_) => "sent",
                    Outcome::Deleted => "deleted",
                    Outcome::Failed(_) => "failed",
                    Outcome::Skipped(_) => "skipped",
                };
                (r.id.as_str(), o)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", "sent"),
                ("bad", "failed"),
                ("c", "skipped"),
                ("d", "sent"),
                ("gone", "deleted"),
            ]
        );
        let calls: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|(path, body)| {
                let endpoint = path.split('?').next().unwrap_or_default().to_owned();
                match serde_json::from_str::<serde_json::Value>(body) {
                    Ok(v) if v["message"]["_id"].is_string() => {
                        format!("{endpoint} {}", v["message"]["_id"].as_str().unwrap())
                    }
                    _ => endpoint,
                }
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                "/api/v1/chat.sendMessage a",
                "/api/v1/chat.sendMessage bad",
                // A failed send checks whether the message exists.
                "/api/v1/chat.getMessage",
                "/api/v1/chat.sendMessage d",
                // Possibly delivered before quitting: looked up, then the
                // deletions; never sent again.
                "/api/v1/chat.getMessage",
                "/api/v1/chat.syncMessages",
            ]
        );
    }
}
