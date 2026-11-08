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
//!   layer keeps E2E messages as ciphertext only.
//! - Offline reading: while the server is unreachable (or the cache is
//!   shown), an opened room or thread without its first page gets its
//!   cached messages (`Workspace::seed_cached_messages`); the first page from
//!   the server replaces them.
//! - Outbox: pending text messages are persisted with their client ids
//!   (not those of encrypted rooms, see `docs/e2e.md`). Messages written
//!   offline, and those still sending when the app quit, are sent once the
//!   server is back (the server rejects a second message with the same id;
//!   a failed resend checks whether the message exists). Failed ones stay
//!   for retry or discard. Uploads are not persisted: one interrupted by
//!   quitting is reported on the next start.
//! - Cleanup: logout, a rejected token and server removal delete the
//!   server's cache; Settings → Storage clears every cache (unsent messages
//!   stay).

pub mod view;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use iced::Task;
use rocketchat::models::{Message as RcMessage, OutgoingMessage, Permission, Setting};
use rust_i18n::t;
use rustket_core::cache::{
    Batch, CacheHandle, CachedState, CachedTimeline, MESSAGES_PER_TIMELINE, Opened, OutboxEntry,
    TimelineKey, server_dir,
};
use rustket_core::timeline::{DeliveryState, Timeline};
use rustket_core::workspace::{Changes, ConnectionState};
use rustket_core::{Dirty, ServerId, Workspace};
use rustket_ui::widget::BannerKind;

use crate::app::App;
use crate::error::{Failure, Res};
use crate::message::Message;
use crate::workspace::ServerState;

/// Longest wait for queued writes when the app quits.
const QUIT_FLUSH: Duration = Duration::from_secs(2);

/// A timeline instance (see `Timeline::epoch`).
type Epoch = (u64, u64);

/// App-wide cache state.
#[derive(Debug, Default)]
pub struct Caches {
    /// Base directory (`None`: caching is off, e.g. tests and snapshots).
    base: Option<PathBuf>,
    /// Bytes on disk of every server's cache (Settings → Storage).
    pub size: Option<u64>,
    pub clearing: bool,
}

impl Caches {
    pub fn new(base: Option<PathBuf>) -> Self {
        Self {
            base,
            size: None,
            clearing: false,
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
    outbox_sig: Option<Vec<(String, bool, Option<String>)>>,
    /// Outbox ids seen so far.
    known: HashSet<String>,
    /// Outbox ids to send once the server is reachable.
    queued: HashSet<String>,
    /// Outbox ids being sent by this module.
    sending: HashSet<String>,
}

impl ServerCache {
    pub fn is_open(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether a message waits to be sent automatically.
    pub fn is_queued(&self, id: &str) -> bool {
        self.queued.contains(id)
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Opened(ServerId, String, Box<Result<Opened, String>>),
    Timeline(ServerId, TimelineKey, Epoch, Option<Box<CachedTimeline>>),
    /// A queued message was sent: server, session generation, id, room,
    /// thread, result.
    Sent(
        ServerId,
        u64,
        String,
        String,
        Option<String>,
        Res<Box<RcMessage>>,
    ),
    Size(u64),
    Cleared,
}

fn wrap(m: Msg) -> Message {
    Message::Cache(m)
}

/// The server answers (realtime up, initial load in).
fn online(s: &ServerState) -> bool {
    s.session.is_some()
        && s.workspace.last_sync().is_some()
        && !s.workspace.from_cache()
        && *s.workspace.connection() == ConnectionState::Connected
}

/// Data in the workspace came from the server in this session.
fn live(ws: &Workspace) -> bool {
    ws.last_sync().is_some() && !ws.from_cache()
}

/// Pending messages of encrypted rooms (or of rooms not known) are never
/// persisted: they hold the plaintext until they are encrypted at send time.
fn persistable(ws: &Workspace, m: &RcMessage) -> bool {
    !m.is_encrypted()
        && m.e2e.is_none()
        && ws
            .room(&m.rid)
            .is_some_and(|e| e.sub.is_some() && !e.is_encrypted())
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
        return Task::none();
    };
    let Some(user) = s.credentials.as_ref().map(|c| c.user_id.clone()) else {
        return Task::none();
    };
    if s.cache.user.as_deref() != Some(user.as_str()) {
        if s.cache.opening.as_deref() == Some(user.as_str())
            || s.cache.failed_for.as_deref() == Some(user.as_str())
        {
            return Task::none();
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
    let mut batch = Batch::default();
    if live(&s.workspace) {
        let full = std::mem::take(&mut s.cache.needs_full);
        collect_documents(&s.workspace, full, &mut batch);
        collect_timelines(&s.workspace, &mut s.cache, full, &mut batch);
    }
    collect_outbox(s, &mut batch);
    handle.write(batch);
    let seeds = ask_seeds(s, &handle);
    let sends = send_queued(s);
    Task::batch([seeds, sends])
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
    let entries: Vec<OutboxEntry> = items
        .iter()
        .filter(|i| persistable(ws, &i.message))
        .map(|i| OutboxEntry {
            message: i.message.clone(),
            queued: c.queued.contains(&i.message.id) || i.state == DeliveryState::Sending,
            error: match &i.state {
                DeliveryState::Failed { error } => Some(error.clone()),
                _ => None,
            },
        })
        .collect();
    let sig: Vec<(String, bool, Option<String>)> = entries
        .iter()
        .map(|e| (e.message.id.clone(), e.queued, e.error.clone()))
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

/// Sends queued messages that are waiting (failed) once the server answers.
fn send_queued(s: &mut ServerState) -> Task<Message> {
    if s.cache.queued.is_empty() || !online(s) {
        return Task::none();
    }
    let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
        return Task::none();
    };
    let ready: Vec<RcMessage> = s
        .workspace
        .outbox_items()
        .filter(|i| matches!(i.state, DeliveryState::Failed { .. }))
        .filter(|i| s.cache.queued.contains(&i.message.id))
        .filter(|i| !s.cache.sending.contains(&i.message.id))
        .map(|i| i.message.clone())
        .collect();
    let mut tasks = Vec::new();
    for m in ready {
        let (id, rid, tmid) = (m.id.clone(), m.rid.clone(), m.tmid.clone());
        s.cache.sending.insert(id.clone());
        s.workspace.mark_pending_sending(&rid, tmid.as_deref(), &id);
        // Encrypted rooms: encrypted now with the current room key (or
        // refused, never sent as plaintext).
        let enc = crate::e2e::outgoing::prepare_send(&s.workspace, &m);
        let out = OutgoingMessage {
            id: Some(m.id),
            rid: m.rid,
            msg: m.msg,
            tmid: m.tmid,
            tshow: m.tshow,
            ..Default::default()
        };
        let rest = rest.clone();
        let (sid, generation) = (s.id.clone(), s.generation);
        let (id2, rid2) = (id.clone(), rid.clone());
        tasks.push(Task::perform(
            async move {
                match crate::e2e::outgoing::send_outgoing(rest.clone(), out, enc).await {
                    Ok(m) => Ok(Box::new(m)),
                    // Sent before (e.g. the app quit before it heard back):
                    // the server keeps the first message with this id.
                    Err(e) => match rest.get_message(&id2).await {
                        Ok(found) if found.rid == rid2 => Ok(Box::new(found)),
                        _ => Err(Failure::from(e)),
                    },
                }
            },
            move |r| {
                wrap(Msg::Sent(
                    sid.clone(),
                    generation,
                    id.clone(),
                    rid.clone(),
                    tmid.clone(),
                    r,
                ))
            },
        ));
    }
    Task::batch(tasks)
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
        Msg::Sent(sid, generation, id, rid, tmid, r) => {
            let Some(s) = app.servers.get_mut(&sid) else {
                return Task::none();
            };
            s.cache.sending.remove(&id);
            if s.generation != generation {
                return Task::none();
            }
            s.cache.queued.remove(&id);
            match r {
                Ok(m) => s.workspace.apply_message(*m),
                Err(e) => {
                    tracing::warn!(error = %e, "queued message not sent");
                    s.workspace
                        .mark_pending_failed(&rid, tmid.as_deref(), &id, &e.user_text());
                }
            }
            Task::none()
        }
        Msg::Size(n) => {
            app.offline.size = Some(n);
            Task::none()
        }
        Msg::Cleared => {
            app.offline.clearing = false;
            size_task(app)
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
    let Some(s) = app.servers.get_mut(&sid) else {
        return Task::none();
    };
    let current_user = s.credentials.as_ref().map(|c| c.user_id.as_str());
    if s.cache.opening.as_deref() != Some(user.as_str()) || current_user != Some(user.as_str()) {
        // Signed out or another account meanwhile: the handle is dropped.
        return Task::none();
    }
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
        let (id, rid, tmid) = (m.id.clone(), m.rid.clone(), m.tmid.clone());
        s.workspace.add_pending(m);
        let error = match (e.queued, e.error) {
            (true, _) | (false, None) => t!("cache.waiting").into_owned(),
            (false, Some(err)) => err,
        };
        s.workspace
            .mark_pending_failed(&rid, tmid.as_deref(), &id, &error);
        s.cache.known.insert(id.clone());
        if e.queued {
            s.cache.queued.insert(id);
        }
    }
    s.cache.outbox_restored = true;
}

/// Deletes a server's cache (logout, rejected token, server removal).
pub fn purge(app: &mut App, sid: &ServerId) -> Task<Message> {
    let Some(base) = app.offline.base.clone() else {
        return Task::none();
    };
    let handle = app
        .servers
        .get_mut(sid)
        .and_then(|s| std::mem::take(&mut s.cache).handle);
    let dir = server_dir(&base, sid);
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
            Some(h) => {
                s.cache.captured.clear();
                handles.push(h);
            }
            None if s.cache.opening.is_none() => closed.push(server_dir(&base, &s.id)),
            None => {}
        }
    }
    Task::perform(
        async move {
            for h in handles {
                if let Err(e) = h.clear().await {
                    tracing::warn!(error = %e, "cannot clear the offline cache");
                }
            }
            let _ = tokio::task::spawn_blocking(move || {
                for dir in closed {
                    let _ = rustket_core::cache::destroy(&dir);
                }
            })
            .await;
        },
        |()| wrap(Msg::Cleared),
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

/// The app quits: running uploads are recorded (reported on the next
/// start) and queued writes are flushed (bounded wait).
pub fn on_quit(app: &App) {
    for s in app.servers.values() {
        let Some(h) = &s.cache.handle else { continue };
        let names = crate::room::upload::running_names(s.id.as_str());
        if !names.is_empty() {
            h.write(Batch {
                interrupted_uploads: Some(names),
                ..Default::default()
            });
        }
        if !h.flush_blocking(QUIT_FLUSH) {
            tracing::warn!(server = %s.id, "offline cache not flushed before quitting");
        }
    }
}
