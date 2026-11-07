//! Supervised stream subscriptions and the connection/health monitor.
//!
//! meteor-ddp renews subscriptions after reconnects, but a subscription the
//! server stops (`nosub`, e.g. `too-many-requests` from Rocket.Chat's DDP rate
//! limiter) is gone for good. Every stream therefore has a supervisor task that
//! mirrors its state into a [`Registry`] and subscribes again with per-stream
//! backoff (honouring `details.timeToReset`). The monitor task turns registry and
//! connection changes into `Connection`, `Degraded` and `Resync` events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use meteor_ddp::{
    DdpClient, MeteorError, SubscribeError, SubscribeOptions, Subscription, SubscriptionState,
};
use serde_json::Value;
use tokio::sync::watch;
use tokio::time::Instant;

use super::Core;
use super::events::{ConnectionState, ResyncReason, SessionEvent};
use super::stream_params;

/// State of one supervised stream.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum StreamStatus {
    /// First subscribe attempt in flight.
    Starting,
    /// Waiting for `ready` (after a reconnect or a retry).
    Pending,
    Ready,
    /// Stopped or refused by the server (or timed out); retried.
    Down(Option<MeteorError>),
    /// An optional stream the server does not allow for this user; not retried.
    Refused,
}

struct Entry {
    name: String,
    status: StreamStatus,
    /// Counted in `down(None)`, `all_ready` and restorations. Room streams become
    /// visible only once `watch_room` succeeded.
    visible: bool,
}

#[derive(Default)]
struct Entries {
    next: u64,
    /// Down -> Ready transitions of visible streams.
    restored: u64,
    map: HashMap<u64, Entry>,
}

/// Status of every supervised stream, with change notification.
pub(super) struct Registry {
    entries: Mutex<Entries>,
    changed: watch::Sender<u64>,
}

impl Registry {
    pub(super) fn new() -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            changed: watch::channel(0).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self) {
        self.changed.send_modify(|g| *g = g.wrapping_add(1));
    }

    pub(super) fn add(&self, name: String, visible: bool) -> u64 {
        let id = {
            let mut guard = self.lock();
            guard.next += 1;
            let id = guard.next;
            guard.map.insert(
                id,
                Entry {
                    name,
                    status: StreamStatus::Starting,
                    visible,
                },
            );
            id
        };
        self.notify();
        id
    }

    /// Makes entries visible (see [`Entry::visible`]).
    pub(super) fn commit(&self, ids: &[u64]) {
        {
            let mut guard = self.lock();
            for id in ids {
                if let Some(e) = guard.map.get_mut(id) {
                    e.visible = true;
                }
            }
        }
        self.notify();
    }

    pub(super) fn set(&self, id: u64, status: StreamStatus) {
        let changed = {
            let mut guard = self.lock();
            let mut restored = false;
            let changed = match guard.map.get_mut(&id) {
                Some(entry) if entry.status != status => {
                    restored = entry.visible
                        && matches!(entry.status, StreamStatus::Down(_))
                        && status == StreamStatus::Ready;
                    entry.status = status;
                    true
                }
                _ => false,
            };
            if restored {
                guard.restored += 1;
            }
            changed
        };
        if changed {
            self.notify();
        }
    }

    pub(super) fn remove(&self, ids: &[u64]) {
        {
            let mut guard = self.lock();
            for id in ids {
                guard.map.remove(id);
            }
        }
        self.notify();
    }

    pub(super) fn status(&self, id: u64) -> Option<StreamStatus> {
        self.lock().map.get(&id).map(|e| e.status.clone())
    }

    /// Number of Down -> Ready transitions so far.
    pub(super) fn restored(&self) -> u64 {
        self.lock().restored
    }

    fn names(&self, ids: Option<&[u64]>, pred: impl Fn(&StreamStatus) -> bool) -> Vec<String> {
        let guard = self.lock();
        let mut names: Vec<String> = guard
            .map
            .iter()
            .filter(|(id, e)| match ids {
                Some(ids) => ids.contains(id),
                None => e.visible,
            })
            .filter(|(_, e)| pred(&e.status))
            .map(|(_, e)| e.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Sorted, deduplicated names of streams that are down (all visible streams,
    /// or the given ones).
    pub(super) fn down(&self, ids: Option<&[u64]>) -> Vec<String> {
        self.names(ids, |s| matches!(s, StreamStatus::Down(_)))
    }

    /// Names of the given streams that are not subscribed and ready.
    pub(super) fn not_ready(&self, ids: &[u64]) -> Vec<String> {
        self.names(Some(ids), |s| {
            !matches!(s, StreamStatus::Ready | StreamStatus::Refused)
        })
    }

    /// Whether every visible stream is ready (or permanently refused).
    pub(super) fn all_ready(&self) -> bool {
        self.lock()
            .map
            .values()
            .filter(|e| e.visible)
            .all(|e| matches!(e.status, StreamStatus::Ready | StreamStatus::Refused))
    }

    /// Whether none of `ids` is still on its first attempt.
    pub(super) fn settled(&self, ids: &[u64]) -> bool {
        let guard = self.lock();
        ids.iter().all(|id| {
            guard
                .map
                .get(id)
                .is_none_or(|e| e.status != StreamStatus::Starting)
        })
    }

    pub(super) fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
}

/// One stream to supervise.
#[derive(Debug, Clone)]
pub(super) struct StreamSpec {
    pub stream: &'static str,
    pub event: String,
    /// A `not-allowed` refusal makes it [`StreamStatus::Refused`] instead of down.
    pub optional: bool,
}

impl StreamSpec {
    pub(super) fn name(&self) -> String {
        format!("{}:{}", self.stream, self.event)
    }
}

/// Retry delays of a stopped stream.
#[derive(Debug, Clone, Copy)]
pub(super) struct RetryPolicy {
    pub min: Duration,
    pub max: Duration,
}

impl RetryPolicy {
    /// `min * 2^attempt` capped at `max`, jittered to 50-100 %, but never before
    /// the server's `details.timeToReset` (plus up to 2 s of jitter).
    fn delay(&self, attempt: u32, error: Option<&MeteorError>) -> Duration {
        let base = self
            .min
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max);
        let mut delay = base.mul_f64(rand::random_range(0.5..=1.0));
        let reset = error
            .and_then(|e| e.details.as_ref())
            .and_then(|d| d.get("timeToReset"))
            .and_then(Value::as_u64);
        if let Some(ms) = reset {
            let reset =
                Duration::from_millis(ms) + Duration::from_millis(rand::random_range(0..=2000));
            delay = delay.max(reset.min(Duration::from_secs(3600)));
        }
        delay
    }
}

/// Resolves when the session terminates or `extra` (a room watch) is dropped.
async fn cancelled(stop: &mut watch::Receiver<bool>, extra: &mut Option<watch::Receiver<()>>) {
    let room = async {
        match extra {
            Some(rx) => {
                // Only ends when the sender (the RoomWatch) is dropped.
                while rx.changed().await.is_ok() {}
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = stop.wait_for(|s| *s) => {}
        () = room => {}
    }
}

async fn keep_alive(ddp: &DdpClient, spec: &StreamSpec) -> Result<Subscription, SubscribeError> {
    let options = SubscribeOptions {
        buffer: Some(0),
        ..SubscribeOptions::default()
    };
    ddp.subscribe_with(spec.stream, stream_params(&spec.event), options)
        .await
}

/// Waits for a live connection; returns its DDP session id (`None`: closed).
async fn wait_connected(conn: &mut watch::Receiver<meteor_ddp::ConnectionState>) -> Option<String> {
    let state = conn
        .wait_for(|s| {
            matches!(
                s,
                meteor_ddp::ConnectionState::Connected { .. } | meteor_ddp::ConnectionState::Closed
            )
        })
        .await
        .ok()?
        .clone();
    match state {
        meteor_ddp::ConnectionState::Connected { session } => Some(session),
        _ => None,
    }
}

fn same_connection(ddp: &DdpClient, session: &str) -> bool {
    matches!(ddp.state(), meteor_ddp::ConnectionState::Connected { session: s } if s == session)
}

fn is_not_allowed(e: &MeteorError) -> bool {
    e.is("not-allowed") || e.is("error-not-allowed")
}

/// Keeps `spec` subscribed until cancelled. The `DdpClient` clone is released on
/// cancellation, so the connection still closes when the session is dropped.
pub(super) async fn supervise(
    ddp: DdpClient,
    spec: StreamSpec,
    id: u64,
    registry: Arc<Registry>,
    retry: RetryPolicy,
    mut stop: watch::Receiver<bool>,
    mut extra: Option<watch::Receiver<()>>,
) {
    let name = spec.name();
    let mut attempt: u32 = 0;
    let mut conn = ddp.watch_state();
    loop {
        // Only subscribe on a live connection: a subscribe timeout that ran while
        // offline drops the handle, and meteor-ddp would not renew it.
        let session = tokio::select! {
            () = cancelled(&mut stop, &mut extra) => return,
            s = wait_connected(&mut conn) => match s {
                Some(s) => s,
                None => return,
            },
        };
        let result = tokio::select! {
            () = cancelled(&mut stop, &mut extra) => return,
            r = keep_alive(&ddp, &spec) => r,
        };
        let error = match result {
            Ok(sub) => {
                if attempt > 0 {
                    tracing::info!(stream = %name, "stream subscription restored");
                }
                let mut state = sub.watch_state();
                let stopped = loop {
                    let current = state.borrow_and_update().clone();
                    match current {
                        SubscriptionState::Pending => registry.set(id, StreamStatus::Pending),
                        SubscriptionState::Ready => {
                            attempt = 0;
                            registry.set(id, StreamStatus::Ready);
                        }
                        SubscriptionState::Stopped(e) => break e,
                        SubscriptionState::Closed => return,
                    }
                    tokio::select! {
                        () = cancelled(&mut stop, &mut extra) => return,
                        changed = state.changed() => if changed.is_err() { return },
                    }
                };
                tracing::warn!(
                    stream = %name,
                    error = %stopped.as_ref().map(MeteorError::code).unwrap_or_default(),
                    "server stopped a stream subscription; retrying"
                );
                stopped
            }
            Err(SubscribeError::Server(e)) => {
                let e = e.map(|e| *e);
                if spec.optional && e.as_ref().is_some_and(is_not_allowed) {
                    tracing::info!(stream = %name, "optional stream not allowed for this user");
                    registry.set(id, StreamStatus::Refused);
                    cancelled(&mut stop, &mut extra).await;
                    return;
                }
                tracing::warn!(
                    stream = %name,
                    error = %e.as_ref().map(MeteorError::code).unwrap_or_default(),
                    "stream subscription refused; retrying"
                );
                e
            }
            Err(SubscribeError::Closed) => return,
            Err(SubscribeError::Timeout | SubscribeError::Disconnected)
                if !same_connection(&ddp, &session) =>
            {
                // The connection dropped meanwhile: not the stream's fault. Keep
                // the status, wait for the next connection and retry at once.
                continue;
            }
            Err(e) => {
                tracing::warn!(stream = %name, error = %e, "stream subscription failed; retrying");
                None
            }
        };
        registry.set(id, StreamStatus::Down(error.clone()));
        let delay = retry.delay(attempt, error.as_ref());
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = cancelled(&mut stop, &mut extra) => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Forwards connection changes, `Degraded` and `Resync` events.
pub(super) async fn monitor(
    core: Weak<Core>,
    mut state: watch::Receiver<meteor_ddp::ConnectionState>,
    initial: meteor_ddp::ConnectionState,
    resync_wait: Duration,
) {
    let (mut shutdown, mut reg, mut last_restored) = match core.upgrade() {
        Some(core) => (
            core.shutdown.subscribe(),
            core.registry.watch(),
            core.registry.restored(),
        ),
        None => return,
    };
    let mut last_raw = initial;
    let mut last_down: Vec<String> = Vec::new();
    let mut need_resync: Option<ResyncReason> = None;
    let mut deadline: Option<Instant> = None;
    state.mark_changed();
    reg.mark_changed();
    loop {
        let mut timed_out = false;
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|s| *s) => return,
            changed = state.changed() => if changed.is_err() { return },
            changed = reg.changed() => if changed.is_err() { return },
            () = sleep_until(deadline) => {
                deadline = None;
                timed_out = true;
            }
        }
        let Some(core) = core.upgrade() else { return };
        if core.is_terminated() {
            return;
        }
        let raw = state.borrow_and_update().clone();
        reg.borrow_and_update();
        if raw != last_raw {
            let mapped = ConnectionState::from(&raw);
            let was = ConnectionState::from(&last_raw);
            if was.is_connected() && mapped.is_connected() {
                // A whole disconnect/reconnect cycle happened between two wake-ups.
                core.emit(SessionEvent::Connection(ConnectionState::Connecting));
            }
            if mapped != was || mapped.is_connected() {
                core.emit(SessionEvent::Connection(mapped.clone()));
            }
            last_raw = raw;
            match mapped {
                ConnectionState::Closed => {
                    core.terminate(Some(SessionEvent::Connection(ConnectionState::Closed)));
                    return;
                }
                ConnectionState::Connected => {
                    need_resync = Some(ResyncReason::Reconnected);
                    deadline = Some(Instant::now() + resync_wait);
                    // Changes made while offline were not streamed.
                    tokio::spawn(Arc::clone(&core).refresh_identity());
                }
                _ => deadline = None,
            }
        }
        let connected = last_raw.is_connected();

        let down = core.registry.down(None);
        if down != last_down {
            core.emit(SessionEvent::Degraded {
                streams: down.clone(),
            });
            last_down = down;
        }
        // Only a real Down -> Ready transition means events were missed.
        let restored = core.registry.restored();
        if restored != last_restored {
            last_restored = restored;
            need_resync.get_or_insert(ResyncReason::StreamsRestored);
            if connected && deadline.is_none() {
                deadline = Some(Instant::now() + resync_wait);
            }
        }

        if connected && let Some(reason) = need_resync.clone() {
            if core.registry.all_ready() {
                core.emit(SessionEvent::Resync { reason });
                need_resync = None;
                deadline = None;
            } else if timed_out {
                tracing::warn!("subscriptions not renewed in time; resyncing now and again later");
                core.emit(SessionEvent::Resync { reason });
                deadline = None;
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}
