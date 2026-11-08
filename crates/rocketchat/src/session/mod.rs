//! Realtime session: one logged-in account on one server (REST + DDP).
//!
//! [`Session::start`] connects the DDP socket (sharing the REST client's TLS
//! context), logs in with the resume token after every (re)connect, subscribes the
//! global user streams and turns their data messages into typed
//! [`SessionEvent`]s on a single bounded [`SessionEvents`] stream.
//!
//! - All stream subscriptions are keep-alive handles; one task decodes the
//!   client-wide data feed, so events keep the server's order (a `Message` is
//!   never overtaken by its `MessageDeleted`).
//! - Every subscription is supervised: one the server stops (`nosub`, e.g. rate
//!   limiting) is retried with backoff, reported through
//!   [`SessionEvent::Degraded`], and followed by a `Resync` once it is back.
//! - Dropping (or closing) the [`Session`] closes the socket and ends every task;
//!   the terminal event (`Connection(Closed)` or `LoggedOut`) is the last one.
//! - `userData` events are never trusted (other members of a shared room can
//!   forge them): they only trigger a rate-limited `/me` refresh, delivered as
//!   [`SessionEvent::MeChanged`].
//!
//! **Logging**: meteor-ddp redacts credentials in its own logs, but tungstenite
//! logs raw WebSocket frames (including the resume token) at TRACE. Never enable
//! `tungstenite=trace`; append [`LOG_FILTER_PINS`] to any user-supplied filter
//! (or use [`RECOMMENDED_LOG_FILTER`]).

mod events;
mod omnichannel;
mod pump;
mod supervisor;

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::Stream;
use meteor_ddp::http::{HeaderValue, header};
use meteor_ddp::{CallError, ConnectError, Connector, DdpClient, DdpConfig, DdpHandle, HookError};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

pub use crate::models::md::MAX_MD_DEPTH;
pub use events::{
    BulkDelete, ChangeKind, ConnectionState, CustomSound, DesktopNotification, NotificationAction,
    NotificationMessage, NotificationPayload, ResyncReason, RoleAssignment, SessionEvent,
    TimeRange, decode_event, status_from_code, streams,
};
pub use meteor_ddp::{BackoffConfig, DataKind, DataMessage, HeartbeatConfig};
pub use omnichannel::{
    OmnichannelEvent, StreamWatch, canned_responses_subscription, decode_omnichannel,
    inquiry_queue_subscriptions, room_data_subscription,
};

use self::pump::Ctl;
use self::supervisor::{Registry, RetryPolicy, StreamSpec, StreamStatus};
use crate::error::{Error, MethodError, Result};
use crate::models::User;
use crate::rest::{Credentials, RestClient};
use crate::server_url::ServerUrl;
use crate::tls::TlsContext;

/// Log filter directives that keep transport libraries from logging raw frames
/// and headers (tungstenite prints the DDP resume token at TRACE). Append them to
/// whatever filter the user configures: in `tracing_subscriber::EnvFilter` the
/// more specific target wins, so `trace,<pins>` still caps these crates.
pub const LOG_FILTER_PINS: &str = "tungstenite=info,tokio_tungstenite=info,hyper=info,hyper_util=info,h2=info,reqwest=info,rustls=info";

/// A safe default `EnvFilter` for the app: `info` plus [`LOG_FILTER_PINS`].
pub const RECOMMENDED_LOG_FILTER: &str = "info,tungstenite=info,tokio_tungstenite=info,hyper=info,hyper_util=info,h2=info,reqwest=info,rustls=info";

/// `stream-notify-user` events subscribed for `{uid}/<event>` (Rocket.Chat 8.9).
pub const NOTIFY_USER_EVENTS: &[&str] = &[
    "message",
    "notification",
    "rooms-changed",
    "subscriptions-changed",
    "uiInteraction",
    "e2ekeyRequest",
    "userData",
    "video-conference",
    "force_logout",
    "calendar",
    "banners",
    "webdav",
    "departmentAgentData",
];

/// `stream-notify-logged` events. `private-settings-changed` is refused with
/// `not-allowed` unless the user may view privileged settings.
pub const NOTIFY_LOGGED_EVENTS: &[&str] = &[
    "user-status",
    "Users:NameChanged",
    "Users:Deleted",
    "updateAvatar",
    "updateEmojiCustom",
    "deleteEmojiCustom",
    "roles-change",
    "permissions-changed",
    "private-settings-changed",
    "updateCustomUserStatus",
    "deleteCustomUserStatus",
    "banner-changed",
    "new-banner",
    "omnichannel.priority-changed",
];

/// `stream-notify-all` events.
pub const NOTIFY_ALL_EVENTS: &[&str] = &[
    "public-settings-changed",
    "updateCustomSound",
    "deleteCustomSound",
];

/// Global streams without which the session is not usable: a failure at start
/// fails [`Session::start`]. Other streams are optional (a `not-allowed` refusal
/// is accepted silently); every stream is retried when the server stops it.
/// Entries are `(stream, event)` with `{uid}` standing for the user id.
pub const REQUIRED_STREAMS: &[(&str, &str)] = &[
    (streams::ROOM_MESSAGES, streams::MY_MESSAGES),
    (streams::NOTIFY_USER, "{uid}/message"),
    (streams::NOTIFY_USER, "{uid}/notification"),
    (streams::NOTIFY_USER, "{uid}/rooms-changed"),
    (streams::NOTIFY_USER, "{uid}/subscriptions-changed"),
    (streams::NOTIFY_USER, "{uid}/force_logout"),
    (streams::NOTIFY_LOGGED, "user-status"),
];

/// `stream-notify-room` events subscribed by [`Session::watch_room`] for `{rid}/<event>`.
pub const NOTIFY_ROOM_EVENTS: &[&str] = &[
    "deleteMessage",
    "deleteMessageBulk",
    "user-activity",
    "messagesRead",
    "videoconf",
    "messagesImported",
    // Decoded by `crate::e2e::RoomE2eKeyRequest::from_event` (arrives as `Unknown`).
    "e2e.keyRequest",
];

/// Activity name of "typing" in [`Session::set_user_activity`].
/// At most one `/me` refresh per this interval after `userData` hints.
pub const ME_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
/// Retries of a failed `/me` re-read (the change it should pick up, such as
/// `statusLivechat`, sends no second `userData`).
pub const ME_REFRESH_RETRIES: u32 = 6;

/// Wait before retry `failure` (1-based) of a failed `/me` re-read: the
/// server's rate-limit reset when known, else 2 s doubling; at most 60 s.
/// `None` once the retries are used up or for errors a retry cannot fix.
fn me_retry_delay(e: &crate::Error, failure: u32) -> Option<Duration> {
    if failure > ME_REFRESH_RETRIES || e.is_unauthorized() {
        return None;
    }
    let backoff = ME_REFRESH_INTERVAL.saturating_mul(1 << (failure - 1).min(5));
    let wait = match e {
        crate::Error::RateLimited {
            retry_after: Some(after),
            ..
        } => (*after).max(ME_REFRESH_INTERVAL),
        _ => backoff,
    };
    Some(wait.min(Duration::from_secs(60)))
}
/// Hints arriving within this delay share one `/me` refresh.
const ME_REFRESH_DEBOUNCE: Duration = Duration::from_millis(250);

pub const ACTIVITY_TYPING: &str = "user-typing";
pub const ACTIVITY_RECORDING: &str = "user-recording";
pub const ACTIVITY_UPLOADING: &str = "user-uploading";

/// Subscription params of a Rocket.Chat stream, as the web client sends them
/// (`[eventName, {useCollection: false, args: []}]`). Every subscription goes
/// through this helper so identical logical subscriptions are byte-identical and
/// share one server-side subscription.
pub fn stream_params(event_name: &str) -> Vec<Value> {
    vec![
        Value::String(event_name.to_owned()),
        json!({"useCollection": false, "args": []}),
    ]
}

/// Every global `(stream, event)` the session subscribes for user `uid`.
pub fn global_subscriptions(uid: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![(streams::ROOM_MESSAGES, streams::MY_MESSAGES.to_owned())];
    out.extend(
        NOTIFY_USER_EVENTS
            .iter()
            .map(|e| (streams::NOTIFY_USER, format!("{uid}/{e}"))),
    );
    out.extend(
        NOTIFY_LOGGED_EVENTS
            .iter()
            .map(|e| (streams::NOTIFY_LOGGED, (*e).to_owned())),
    );
    out.extend(
        NOTIFY_ALL_EVENTS
            .iter()
            .map(|e| (streams::NOTIFY_ALL, (*e).to_owned())),
    );
    out.push((streams::ROLES, "roles".to_owned()));
    out.push((streams::APPS, "apps".to_owned()));
    out
}

/// Every `(stream, event)` [`Session::watch_room`] subscribes for `rid`.
pub fn room_subscriptions(rid: &str, legacy_typing: bool) -> Vec<(&'static str, String)> {
    let mut out: Vec<_> = NOTIFY_ROOM_EVENTS
        .iter()
        .map(|e| (streams::NOTIFY_ROOM, format!("{rid}/{e}")))
        .collect();
    if legacy_typing {
        out.push((streams::NOTIFY_ROOM, format!("{rid}/typing")));
    }
    out
}

fn is_required(uid: &str, stream: &str, event: &str) -> bool {
    REQUIRED_STREAMS
        .iter()
        .any(|(s, e)| *s == stream && e.replace("{uid}", uid) == event)
}

/// Tuning of a [`Session`].
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// Capacity of the [`SessionEvents`] channel. When it is full, stream events
    /// are dropped and a [`SessionEvent::Resync`] is queued instead.
    pub event_capacity: usize,
    /// Capacity of the DDP data message feed (clamped by meteor-ddp to 65536).
    pub feed_capacity: usize,
    pub heartbeat: HeartbeatConfig,
    pub backoff: BackoffConfig,
    pub connect_timeout: Duration,
    pub call_timeout: Duration,
    pub subscribe_timeout: Duration,
    /// After a reconnect, how long to wait for the renewed subscriptions before
    /// emitting [`ResyncReason::Reconnected`] anyway (another follows once they
    /// are ready).
    pub resync_wait: Duration,
    /// First retry delay of a stream the server stopped (doubles per attempt,
    /// jittered, never before the server's `timeToReset`).
    pub stream_retry_min: Duration,
    /// Upper bound of the stream retry delay.
    pub stream_retry_max: Duration,
    /// Largest accepted DDP frame. Rocket.Chat stream payloads are small.
    pub max_message_size: usize,
    /// Also subscribe the legacy `{rid}/typing` room event. 8.9 accepts the
    /// subscription, but no current client publishes to it (and a publish with the
    /// legacy shape is not relayed), so it is off by default.
    pub legacy_typing: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            event_capacity: 10_000,
            feed_capacity: 16_384,
            heartbeat: HeartbeatConfig::default(),
            backoff: BackoffConfig::default(),
            connect_timeout: Duration::from_secs(20),
            call_timeout: Duration::from_secs(60),
            subscribe_timeout: Duration::from_secs(30),
            resync_wait: Duration::from_secs(15),
            stream_retry_min: Duration::from_secs(2),
            stream_retry_max: Duration::from_secs(300),
            max_message_size: 16 << 20,
            legacy_typing: false,
        }
    }
}

impl SessionOptions {
    fn retry(&self) -> RetryPolicy {
        RetryPolicy {
            min: self.stream_retry_min,
            max: self.stream_retry_max.max(self.stream_retry_min),
        }
    }
}

/// The consumer side of a session's events. Ends after the terminal event
/// (session closed, dropped or logged out).
#[derive(Debug)]
pub struct SessionEvents {
    rx: mpsc::Receiver<SessionEvent>,
}

impl SessionEvents {
    /// Next event; `None` once the session has stopped and all events were read.
    pub async fn recv(&mut self) -> Option<SessionEvent> {
        self.rx.recv().await
    }

    /// Next event if one is queued.
    pub fn try_recv(&mut self) -> Option<SessionEvent> {
        self.rx.try_recv().ok()
    }
}

impl Stream for SessionEvents {
    type Item = SessionEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// State shared by the session, its tasks and the DDP `on_connected` hook. It must
/// never own the `DdpClient` (the driver owns the hook, so that would keep the
/// connection alive forever).
struct Core {
    credentials: Credentials,
    rest: RestClient,
    me: RwLock<Option<User>>,
    /// Public setting `UI_Use_Real_Name`: user activity is published under the
    /// real name instead of the username.
    use_real_name: AtomicBool,
    ctl: mpsc::UnboundedSender<Ctl>,
    shutdown: watch::Sender<bool>,
    /// The first login (including `me`) finished.
    first_done: AtomicBool,
    /// Why the server rejected the token, once it did.
    logged_out: Mutex<Option<String>>,
    registry: Arc<Registry>,
    me_refresh: Mutex<MeRefresh>,
}

/// State of the `/me` refreshes triggered by `userData` hints.
#[derive(Debug, Default)]
struct MeRefresh {
    /// A refresh is waiting to run (later hints join it).
    scheduled: bool,
    /// When the last refresh started.
    last: Option<Instant>,
}

impl Core {
    fn emit(&self, event: SessionEvent) {
        if !self.is_terminated() {
            let _ = self.ctl.send(Ctl::Event(event));
        }
    }

    fn is_terminated(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Stops all tasks; `last` becomes the final event of the stream. Idempotent.
    fn terminate(&self, last: Option<SessionEvent>) {
        let first = self.shutdown.send_if_modified(|stopped| {
            let first = !*stopped;
            *stopped = true;
            first
        });
        if first {
            let _ = self.ctl.send(Ctl::Terminate(last));
        }
    }

    /// Reads `UI_Use_Real_Name`; `false` if the read failed.
    async fn read_real_name(&self) -> bool {
        match self
            .rest
            .settings_public_by_ids(&["UI_Use_Real_Name"])
            .await
        {
            Ok(settings) => {
                let on = settings
                    .iter()
                    .find(|s| s.id == "UI_Use_Real_Name")
                    .and_then(|s| s.as_bool())
                    .unwrap_or(false);
                self.use_real_name.store(on, Ordering::Release);
                true
            }
            Err(e) => {
                tracing::debug!(error = %e, "could not read UI_Use_Real_Name");
                false
            }
        }
    }

    /// Re-reads the user and `UI_Use_Real_Name` (after a reconnect, changes are
    /// not streamed), retrying a few times.
    async fn refresh_identity(self: Arc<Self>) {
        let mut shutdown = self.shutdown.subscribe();
        for attempt in 0..5u32 {
            if self.is_terminated() {
                return;
            }
            let me = self.rest.me().await;
            let setting = self.read_real_name().await;
            if let Ok(me) = &me {
                *self.me.write().unwrap_or_else(PoisonError::into_inner) = Some(me.clone());
            }
            if me.is_ok() && setting {
                return;
            }
            let delay = Duration::from_secs(2u64 << attempt);
            tokio::select! {
                _ = shutdown.wait_for(|s| *s) => return,
                () = tokio::time::sleep(delay) => {}
            }
        }
        tracing::warn!("could not refresh the user after reconnecting");
    }

    fn logged_out(&self) -> Option<String> {
        self.logged_out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update_me(&self, f: impl FnOnce(&mut User)) {
        if let Some(me) = self
            .me
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            f(me);
        }
    }

    /// A `userData` event arrived: re-read `/me` (debounced, at most once per
    /// [`ME_REFRESH_INTERVAL`]) and send [`SessionEvent::MeChanged`] when the
    /// user differs from the cached one. The event's own fields are never used.
    fn hint_me_changed(self: &Arc<Self>) {
        let delay = {
            let mut r = self
                .me_refresh
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if r.scheduled {
                return;
            }
            r.scheduled = true;
            let wait = r.last.map_or(Duration::ZERO, |t| {
                ME_REFRESH_INTERVAL.saturating_sub(t.elapsed())
            });
            wait.max(ME_REFRESH_DEBOUNCE)
        };
        let core = Arc::downgrade(self);
        let mut shutdown = self.shutdown.subscribe();
        tokio::spawn(async move {
            let mut delay = delay;
            let mut failures = 0u32;
            loop {
                tokio::select! {
                    _ = shutdown.wait_for(|s| *s) => return,
                    () = tokio::time::sleep(delay) => {}
                }
                let Some(core) = core.upgrade() else {
                    return;
                };
                {
                    let mut r = core
                        .me_refresh
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    r.scheduled = false;
                    r.last = Some(Instant::now());
                }
                match core.rest.me().await {
                    Ok(me) => {
                        let changed = {
                            let mut cached =
                                core.me.write().unwrap_or_else(PoisonError::into_inner);
                            let changed = cached.as_ref() != Some(&me);
                            if changed {
                                *cached = Some(me.clone());
                            }
                            changed
                        };
                        if changed {
                            core.emit(SessionEvent::MeChanged(Box::new(me)));
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "refreshing the user after userData failed");
                        failures += 1;
                        let Some(wait) = me_retry_delay(&e, failures) else {
                            return;
                        };
                        // A newer `userData` already scheduled a re-read.
                        let mut r = core
                            .me_refresh
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        if r.scheduled {
                            return;
                        }
                        r.scheduled = true;
                        delay = wait;
                    }
                }
            }
        });
    }

    /// Keeps the cached user and settings current; returns whether `event` is
    /// delivered (`userData` is not: see [`SessionEvent::UserData`]).
    fn observe(self: &Arc<Self>, event: &SessionEvent) -> bool {
        let uid = self.credentials.user_id.as_str();
        match event {
            SessionEvent::UserData(_) => {
                self.hint_me_changed();
                return false;
            }
            SessionEvent::UserNameChanged {
                uid: changed,
                username,
                name,
            } if changed == uid => self.update_me(|me| {
                if username.is_some() {
                    me.username.clone_from(username);
                }
                if name.is_some() {
                    me.name.clone_from(name);
                }
            }),
            SessionEvent::SettingChanged { setting, .. } if setting.id == "UI_Use_Real_Name" => {
                if let Some(v) = setting.as_bool() {
                    self.use_real_name.store(v, Ordering::Release);
                }
            }
            _ => {}
        }
        true
    }

    async fn on_connected(&self, ddp: DdpHandle) -> std::result::Result<(), HookError> {
        let first = !self.first_done.load(Ordering::Acquire);
        let result = ddp
            .call(
                "login",
                vec![json!({ "resume": self.credentials.auth_token })],
            )
            .await;
        let fatal = |e: HookError| {
            if e.is_fatal() && !first {
                tracing::warn!(reason = %e, "resume login rejected; session logged out");
                *self
                    .logged_out
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
                self.terminate(Some(SessionEvent::LoggedOut {
                    reason: e.to_string(),
                }));
            }
            e
        };
        let result = result.map_err(|e| fatal(HookError::from(e)))?;
        let id = result.get("id").and_then(Value::as_str);
        if id != Some(self.credentials.user_id.as_str()) {
            return Err(fatal(HookError::fatal(
                "resume login returned a different user",
            )));
        }
        if first {
            let me = self.rest.me().await.map_err(|e| {
                if e.is_unauthorized() {
                    HookError::fatal(format!("me: {e}"))
                } else {
                    HookError::retry(format!("me: {e}"))
                }
            })?;
            *self.me.write().unwrap_or_else(PoisonError::into_inner) = Some(me);
            self.first_done.store(true, Ordering::Release);
        }
        tracing::debug!("DDP resume login done");
        Ok(())
    }
}

/// A logged-in account on one server. Dropping it closes the connection.
pub struct Session {
    core: Arc<Core>,
    ddp: DdpClient,
    options: SessionOptions,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("server", self.core.rest.server())
            .field("user_id", &self.core.credentials.user_id)
            .field("state", &self.connection_state())
            .finish_non_exhaustive()
    }
}

/// Error of a stream that is down.
fn stream_error(status: Option<StreamStatus>) -> Error {
    match status {
        Some(StreamStatus::Down(Some(e))) => Error::from_method_error(meteor_to_method_error(&e)),
        _ => Error::Timeout("stream subscription"),
    }
}

impl Session {
    /// Connects and logs in with `credentials`. Fails if the first connection or
    /// login fails ([`Error::Unauthorized`] for a rejected token) or a required
    /// stream (see [`REQUIRED_STREAMS`]) cannot be subscribed.
    pub async fn start(
        server: ServerUrl,
        tls: &TlsContext,
        credentials: Credentials,
        options: SessionOptions,
    ) -> Result<(Session, SessionEvents)> {
        let rest = RestClient::new(server, tls)?;
        rest.set_credentials(Some(credentials));
        Self::start_with_rest(rest, tls, options).await
    }

    /// Like [`start`](Self::start) with an existing REST client that already has
    /// credentials. `tls` must be the context the client was built with.
    pub async fn start_with_rest(
        rest: RestClient,
        tls: &TlsContext,
        options: SessionOptions,
    ) -> Result<(Session, SessionEvents)> {
        let credentials = rest.credentials().ok_or(Error::Unauthorized {
            message: Some("no credentials".into()),
            code: None,
        })?;
        let url = url::Url::parse(&rest.server().websocket())
            .map_err(|e| Error::Url(format!("websocket url: {e}")))?;
        let (tx, rx) = mpsc::channel(options.event_capacity.max(16));
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let core = Arc::new(Core {
            credentials,
            rest,
            me: RwLock::new(None),
            use_real_name: AtomicBool::new(false),
            ctl: ctl_tx,
            shutdown: watch::channel(false).0,
            first_done: AtomicBool::new(false),
            logged_out: Mutex::new(None),
            registry: Arc::new(Registry::new()),
            me_refresh: Mutex::new(MeRefresh::default()),
        });

        let mut config = DdpConfig::new(url)
            .connector(Connector::Rustls(tls.client_config()))
            .heartbeat(options.heartbeat)
            .backoff(options.backoff);
        config.headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );
        config.connect_timeout = options.connect_timeout;
        config.call_timeout = options.call_timeout;
        config.subscribe_timeout = options.subscribe_timeout;
        config.broadcast_capacity = options.feed_capacity;
        config.max_message_size = options.max_message_size;
        let hook_core = Arc::downgrade(&core);
        let config = config.on_connected(move |handle| {
            let core = hook_core.upgrade();
            async move {
                match core {
                    Some(core) => core.on_connected(handle).await,
                    None => Err(HookError::fatal("session dropped")),
                }
            }
        });

        let ddp = DdpClient::connect(config)
            .await
            .map_err(map_connect_error)?;
        let initial_state = ddp.state();
        // Taken before any stream is subscribed so no event is missed.
        let feed = ddp.data_messages();
        let session = Session { core, ddp, options };

        let uid = session.core.credentials.user_id.clone();
        let specs: Vec<(StreamSpec, bool)> = global_subscriptions(&uid)
            .into_iter()
            .map(|(stream, event)| {
                let required = is_required(&uid, stream, &event);
                let spec = StreamSpec {
                    stream,
                    event,
                    optional: !required,
                };
                (spec, required)
            })
            .collect();
        let ids: Vec<u64> = specs
            .iter()
            .map(|(spec, _)| session.spawn_supervisor(spec.clone(), None))
            .collect();
        let (settled, real_name) =
            tokio::join!(session.wait_settled(&ids), session.core.read_real_name());
        settled?;
        for ((spec, required), id) in specs.iter().zip(&ids) {
            let status = session.core.registry.status(*id);
            if !*required {
                continue;
            }
            match status {
                Some(StreamStatus::Ready) => {}
                Some(StreamStatus::Down(_)) => {
                    tracing::warn!(stream = %spec.name(), "required stream unavailable");
                    return Err(stream_error(status));
                }
                _ => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "connection lost while subscribing",
                    )));
                }
            }
        }
        if !real_name {
            // Retry in the background; set_user_activity uses the username meanwhile.
            tokio::spawn(Arc::clone(&session.core).refresh_identity());
        }

        let me = session
            .me()
            .ok_or_else(|| Error::Protocol("login finished without user".into()))?;
        let core = &session.core;
        core.emit(SessionEvent::Connection(ConnectionState::Connected));
        core.emit(SessionEvent::Ready { me: Box::new(me) });
        let observer = Arc::downgrade(core);
        tokio::spawn(pump::run(tx, ctl_rx, Some(feed), move |event| {
            observer.upgrade().is_none_or(|core| core.observe(event))
        }));
        tokio::spawn(supervisor::monitor(
            Arc::downgrade(core),
            session.ddp.watch_state(),
            initial_state,
            session.options.resync_wait,
        ));
        Ok((session, SessionEvents { rx }))
    }

    fn spawn_supervisor(&self, spec: StreamSpec, extra: Option<watch::Receiver<()>>) -> u64 {
        // Room streams stay invisible to Degraded/Resync until watch_room succeeds.
        let id = self.core.registry.add(spec.name(), extra.is_none());
        tokio::spawn(supervisor::supervise(
            self.ddp.clone(),
            spec,
            id,
            Arc::clone(&self.core.registry),
            self.options.retry(),
            self.core.shutdown.subscribe(),
            extra,
        ));
        id
    }

    /// Waits until every stream in `ids` had its first answer.
    async fn wait_settled(&self, ids: &[u64]) -> Result<()> {
        let mut changes = self.core.registry.watch();
        let mut shutdown = self.core.shutdown.subscribe();
        let limit = self.options.subscribe_timeout + Duration::from_secs(5);
        let wait = async {
            loop {
                if self.core.is_terminated() {
                    return Err(match self.core.logged_out() {
                        Some(reason) => Error::Unauthorized {
                            message: Some(reason),
                            code: None,
                        },
                        None => Error::Protocol("session stopped".into()),
                    });
                }
                if self.core.registry.settled(ids) {
                    return Ok(());
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() {
                        return Err(Error::Protocol("session stopped".into()));
                    },
                    _ = shutdown.wait_for(|s| *s) => {}
                }
            }
        };
        tokio::time::timeout(limit, wait)
            .await
            .map_err(|_| Error::Timeout("stream subscriptions"))?
    }

    pub fn rest(&self) -> &RestClient {
        &self.core.rest
    }

    pub fn server(&self) -> &ServerUrl {
        self.core.rest.server()
    }

    pub fn user_id(&self) -> &str {
        &self.core.credentials.user_id
    }

    /// The current user: fetched at the first login, then kept current from
    /// `/me` refreshes (after `userData` hints and reconnects) and
    /// `Users:NameChanged` (username and name).
    pub fn me(&self) -> Option<User> {
        self.core
            .me
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Streams currently down (retried in the background), as in
    /// [`SessionEvent::Degraded`].
    pub fn degraded_streams(&self) -> Vec<String> {
        self.core.registry.down(None)
    }

    /// Calls a DDP method on the session's connection.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        self.ddp.call(method, params).await.map_err(map_call_error)
    }

    pub fn connection_state(&self) -> ConnectionState {
        if self.core.is_terminated() {
            return ConnectionState::Closed;
        }
        ConnectionState::from(&self.ddp.state())
    }

    /// Raw DDP connection state changes (ends with `Closed` when the session closes).
    pub fn watch_connection(&self) -> watch::Receiver<meteor_ddp::ConnectionState> {
        self.ddp.watch_state()
    }

    /// Drops the current connection (or skips the backoff delay) and reconnects.
    pub fn reconnect_now(&self) {
        if !self.core.is_terminated() {
            self.ddp.reconnect_now();
        }
    }

    /// Subscribes the per-room events (deletes, typing, read receipts, video
    /// conferences, imports) of `rid`; they arrive on the session's event stream
    /// until the returned guard is dropped. New and edited messages need no watch:
    /// they come through `__my_messages__` for every accessible room (with
    /// `room_participant: false` for public channels the user has not joined).
    ///
    /// Fails if none of the room streams could be subscribed. If only some
    /// failed, the watch is returned, [`RoomWatch::missing`] lists them, and they
    /// are retried in the background (also reported through `Degraded`).
    /// Watching a room twice is fine: the server-side subscriptions are shared
    /// and each event is delivered once.
    pub async fn watch_room(&self, rid: &str) -> Result<RoomWatch> {
        if rid.is_empty() || rid.contains('/') {
            return Err(Error::Protocol(format!("invalid room id {rid:?}")));
        }
        let (stop, stop_rx) = watch::channel(());
        let ids: Vec<u64> = room_subscriptions(rid, self.options.legacy_typing)
            .into_iter()
            .map(|(stream, event)| {
                let spec = StreamSpec {
                    stream,
                    event,
                    optional: false,
                };
                self.spawn_supervisor(spec, Some(stop_rx.clone()))
            })
            .collect();
        let watch = RoomWatch {
            rid: rid.to_owned(),
            ids,
            registry: Arc::clone(&self.core.registry),
            _stop: stop,
        };
        // Offline (or going offline meanwhile): the streams subscribe once the
        // connection is back; until then they are listed by `missing()`.
        let mut conn = self.ddp.watch_state();
        let offline = async {
            let _ = conn.wait_for(|s| !s.is_connected()).await;
        };
        tokio::select! {
            r = self.wait_settled(&watch.ids) => r?,
            () = offline => {
                self.core.registry.commit(&watch.ids);
                return Ok(watch);
            }
        }
        let down = self.core.registry.down(Some(&watch.ids));
        if down.len() == watch.ids.len() {
            let status = watch
                .ids
                .first()
                .and_then(|id| self.core.registry.status(*id));
            return Err(stream_error(status));
        }
        if !down.is_empty() {
            tracing::warn!(rid, missing = ?down, "some room streams unavailable; retrying");
        }
        self.core.registry.commit(&watch.ids);
        Ok(watch)
    }

    /// Broadcasts the user's activity in a room (or thread `tmid`), as the web
    /// client does: `["user-typing"]` while typing, an empty list to stop. The web
    /// client repeats an ongoing activity every 5 s; receivers expire it after 15 s.
    ///
    /// The server relays the publish only if the sender identity matches the user
    /// (the real name when `UI_Use_Real_Name` is on, else the username); the
    /// session tracks both. A mismatch is dropped silently by the server: the
    /// call still returns `Ok`.
    pub async fn set_user_activity(
        &self,
        rid: &str,
        tmid: Option<&str>,
        activities: &[&str],
    ) -> Result<()> {
        let me = self.me();
        let identity = if self.core.use_real_name.load(Ordering::Acquire) {
            me.and_then(|me| me.name.or(me.username))
        } else {
            me.and_then(|me| me.username)
        }
        .ok_or_else(|| Error::Protocol("user has no username".into()))?;
        let extras = match tmid {
            Some(tmid) => json!({ "tmid": tmid }),
            None => json!({}),
        };
        self.call(
            "stream-notify-room",
            vec![
                json!(format!("{rid}/user-activity")),
                json!(identity),
                json!(activities),
                extras,
            ],
        )
        .await
        .map(drop)
    }

    /// Publishes a direct-call signal to `target_uid`
    /// (`stream-notify-user ["{target}/video-conference", {action, params:{callId, uid: me, rid}}]`).
    /// The server relays it only when both users can access `rid` and the call
    /// is live (docs/teams-videoconf.md §7.5); a refused relay is not an error.
    pub async fn notify_video_conference(
        &self,
        target_uid: &str,
        action: crate::models::VideoConfAction,
        call_id: &str,
        rid: &str,
    ) -> Result<()> {
        if target_uid.is_empty() || target_uid.contains('/') {
            return Err(Error::Protocol(format!("invalid user id {target_uid:?}")));
        }
        let signal = crate::models::VideoConfSignal {
            action,
            call_id: call_id.to_owned(),
            uid: self.user_id().to_owned(),
            rid: rid.to_owned(),
        };
        self.call(
            "stream-notify-user",
            vec![
                json!(format!("{target_uid}/video-conference")),
                signal.to_value(),
            ],
        )
        .await
        .map(drop)
    }

    /// Closes the connection, then invalidates the token on the server. The event
    /// stream ends either way.
    pub async fn logout(&self) -> Result<()> {
        self.close().await;
        self.core.rest.logout().await
    }

    /// Closes the connection and ends the event stream (`Connection(Closed)` is
    /// the last event).
    pub async fn close(&self) {
        self.core
            .terminate(Some(SessionEvent::Connection(ConnectionState::Closed)));
        self.ddp.close().await;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.core
            .terminate(Some(SessionEvent::Connection(ConnectionState::Closed)));
    }
}

/// Keeps the per-room subscriptions of [`Session::watch_room`] alive; dropping it
/// unsubscribes.
#[derive(Debug)]
pub struct RoomWatch {
    rid: String,
    ids: Vec<u64>,
    registry: Arc<Registry>,
    _stop: watch::Sender<()>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

impl RoomWatch {
    pub fn rid(&self) -> &str {
        &self.rid
    }

    /// Room streams that are not currently subscribed: refused or stopped by the
    /// server (retried), or waiting for the connection.
    pub fn missing(&self) -> Vec<String> {
        self.registry.not_ready(&self.ids)
    }
}

impl Drop for RoomWatch {
    fn drop(&mut self) {
        self.registry.remove(&self.ids);
    }
}

fn map_connect_error(e: ConnectError) -> Error {
    match e {
        ConnectError::Hook(h) if h.is_fatal() => Error::Unauthorized {
            code: h.server_error().map(|e| e.code()),
            message: Some(h.message().to_owned()),
        },
        ConnectError::Hook(h) => match h.server_error() {
            Some(e) => Error::from_method_error(meteor_to_method_error(e)),
            None => Error::Protocol(format!("login failed: {h}")),
        },
        ConnectError::InvalidUrl(u) => Error::Url(u),
        ConnectError::Io(e) => Error::Io(e),
        ConnectError::WebSocket(e) => map_ws_error(*e),
        ConnectError::Timeout => Error::Timeout("DDP connect"),
        ConnectError::Lost(reason) => Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            reason,
        )),
        ConnectError::Handshake(v) => Error::Protocol(format!(
            "server rejected the DDP handshake (suggested version {v:?})"
        )),
        ConnectError::Closed => Error::Protocol("session closed while connecting".into()),
    }
}

fn map_ws_error(e: tokio_tungstenite::tungstenite::Error) -> Error {
    use tokio_tungstenite::tungstenite::Error as W;
    use tokio_tungstenite::tungstenite::error::TlsError;
    match e {
        W::Io(e) => Error::Io(e),
        W::Tls(TlsError::Rustls(e)) => match *e {
            rustls::Error::InvalidCertificate(c) => Error::Certificate {
                host: None,
                reason: format!("{c:?}"),
            },
            other => Error::Tls(other.to_string()),
        },
        W::Tls(t) => Error::Tls(t.to_string()),
        W::Http(response) => Error::Status {
            status: response.status().as_u16(),
            body: String::new(),
        },
        other => Error::Protocol(format!("websocket: {other}")),
    }
}

fn meteor_to_method_error(e: &meteor_ddp::MeteorError) -> MethodError {
    MethodError {
        error: Some(e.error.clone()),
        reason: e.reason.clone(),
        message: e.message.clone(),
        error_type: e.error_type.clone(),
        details: e.details.clone(),
    }
}

fn map_call_error(e: CallError) -> Error {
    match e {
        CallError::Server(e) => Error::from_method_error(meteor_to_method_error(&e)),
        CallError::Disconnected => Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "connection lost before the method result arrived",
        )),
        CallError::Timeout => Error::Timeout("DDP method call"),
        CallError::Closed => Error::Protocol("session closed".into()),
    }
}

#[cfg(test)]
mod me_retry_tests {
    use super::*;

    #[test]
    fn me_re_read_retries_back_off() {
        let net = crate::Error::Timeout("me");
        assert_eq!(me_retry_delay(&net, 1), Some(Duration::from_secs(2)));
        assert_eq!(me_retry_delay(&net, 3), Some(Duration::from_secs(8)));
        assert_eq!(
            me_retry_delay(&net, ME_REFRESH_RETRIES),
            Some(Duration::from_secs(60))
        );
        assert_eq!(me_retry_delay(&net, ME_REFRESH_RETRIES + 1), None);
        let limited = crate::Error::RateLimited {
            retry_after: Some(Duration::from_secs(40)),
            message: None,
        };
        assert_eq!(me_retry_delay(&limited, 1), Some(Duration::from_secs(40)));
        let gone = crate::Error::Unauthorized {
            message: None,
            code: None,
        };
        assert_eq!(me_retry_delay(&gone, 1), None);
    }
}
