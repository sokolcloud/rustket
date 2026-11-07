//! Background task owning the WebSocket.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use futures::future::BoxFuture;
use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, trace, warn};

use crate::client::{ConnectionState, DdpHandle, Routing, Shared, SubscriptionState};
use crate::config::DdpConfig;
use crate::error::{CallError, ConnectError, HookError, SubscribeError};
use crate::message::{
    ClientMessage, DDP_SUPPORT, DDP_VERSION, DataKind, DataMessage, ServerMessage,
};

type Ws = WebSocketStream<MaybeTlsStream<Tracked<TcpStream>>>;
type HookFuture = BoxFuture<'static, Result<(), HookError>>;

/// Longest frame excerpt written to debug logs.
const LOG_FRAME_LIMIT: usize = 2000;
/// How long to wait for a graceful WebSocket close.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) enum Command {
    Call(CallRequest),
    CancelCall(String),
    Subscribe(SubRequest),
    Unsubscribe(u64),
    ReconnectNow,
    Close,
}

pub(crate) struct CallRequest {
    pub(crate) id: String,
    pub(crate) method: String,
    pub(crate) params: Vec<Value>,
    pub(crate) reply: oneshot::Sender<Result<Value, CallError>>,
    /// `Some` for hook calls: only valid on that connection generation.
    pub(crate) generation: Option<u64>,
}

pub(crate) struct SubRequest {
    pub(crate) handle: u64,
    pub(crate) name: String,
    pub(crate) params: Vec<Value>,
    pub(crate) routing: Routing,
    pub(crate) data: mpsc::Sender<SubItem>,
    pub(crate) deliver: bool,
    pub(crate) ready: oneshot::Sender<Result<String, SubscribeError>>,
    pub(crate) state: watch::Sender<SubscriptionState>,
}

/// Item of a subscription queue. The queue has one slot more than the data
/// buffer, reserved for the `Lagged` marker.
pub(crate) enum SubItem {
    Data(Arc<DataMessage>),
    /// Messages after the preceding items were dropped. The driver keeps adding
    /// to the count until the consumer takes it (swapping in [`LAG_TAKEN`]).
    Lagged(Arc<AtomicU64>),
}

/// Value of a lag counter once the consumer has read it.
pub(crate) const LAG_TAKEN: u64 = u64::MAX;

/// Resolved routing filter of one handle.
enum Filter {
    All,
    /// Auto routing: messages without an event name pass.
    EventLoose(String),
    Event(String),
    Prefix(String),
}

impl Filter {
    fn new(routing: Routing, params: &[Value]) -> Self {
        match routing {
            Routing::Auto => match params.first().and_then(Value::as_str) {
                Some(event) if !event.is_empty() => Filter::EventLoose(event.to_owned()),
                _ => Filter::All,
            },
            Routing::Collection => Filter::All,
            Routing::EventName(event) => Filter::Event(event),
            Routing::EventNamePrefix(prefix) => Filter::Prefix(prefix),
        }
    }

    fn matches(&self, event: Option<&str>) -> bool {
        match self {
            Filter::All => true,
            Filter::EventLoose(e) => event.is_none_or(|ev| ev == e),
            Filter::Event(e) => event == Some(e.as_str()),
            Filter::Prefix(p) => event.is_some_and(|ev| ev.starts_with(p.as_str())),
        }
    }
}

/// One `Subscription` handle attached to a server-side subscription.
struct Handle {
    filter: Filter,
    data: mpsc::Sender<SubItem>,
    deliver: bool,
    /// Dropped messages not yet in a queued marker.
    pending_lag: u64,
    /// The last queued lag marker.
    marker: Option<Arc<AtomicU64>>,
    /// Whether data was queued after `marker`.
    data_after_marker: bool,
    ready: Option<oneshot::Sender<Result<String, SubscribeError>>>,
    state: watch::Sender<SubscriptionState>,
}

/// A server-side subscription shared by identical handles.
struct ServerSub {
    name: String,
    params: Vec<Value>,
    key: String,
    ready: bool,
    handles: HashMap<u64, Handle>,
}

struct PendingCall {
    reply: oneshot::Sender<Result<Value, CallError>>,
}

enum Offline<T> {
    Done(T),
    Reconnect,
    Close,
}

enum Outcome {
    /// Connection lost or not established; retry with backoff.
    Lost {
        reason: String,
        error: ConnectError,
        stable: bool,
    },
    /// Terminal failure; stop reconnecting.
    Fatal { reason: String, error: ConnectError },
    /// `reconnect_now` while connected.
    Restart,
    /// `close`.
    Closed,
}

fn lost(reason: impl Into<String>) -> Outcome {
    let reason = reason.into();
    Outcome::Lost {
        error: ConnectError::Lost(reason.clone()),
        reason,
        stable: false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Handshake,
    Hook,
    Ready,
}

struct Conn {
    phase: Phase,
    session: String,
    liveness: Liveness,
    ping_sent: Option<Instant>,
    ping_counter: u64,
    ready_at: Option<Instant>,
}

pub(crate) struct Driver {
    config: Arc<DdpConfig>,
    shared: Arc<Shared>,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    state_tx: watch::Sender<ConnectionState>,
    first: Option<oneshot::Sender<Result<(), ConnectError>>>,
    pending: HashMap<String, PendingCall>,
    queued: VecDeque<CallRequest>,
    /// Server-side subscriptions by DDP id (ordered for deterministic re-sends).
    subs: BTreeMap<String, ServerSub>,
    sub_by_key: HashMap<String, String>,
    sub_by_handle: HashMap<u64, String>,
    /// Ids of sent calls to redacted methods whose result has not arrived on the
    /// current connection (kept even when the caller gave up).
    redacted_ids: HashSet<String>,
    generation: u64,
    failed: bool,
}

impl Driver {
    pub(crate) fn new(
        config: DdpConfig,
        shared: Arc<Shared>,
        cmd_rx: mpsc::UnboundedReceiver<Command>,
        state_tx: watch::Sender<ConnectionState>,
        first: Option<oneshot::Sender<Result<(), ConnectError>>>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            shared,
            cmd_rx,
            state_tx,
            first,
            pending: HashMap::new(),
            queued: VecDeque::new(),
            subs: BTreeMap::new(),
            sub_by_key: HashMap::new(),
            sub_by_handle: HashMap::new(),
            redacted_ids: HashSet::new(),
            generation: 0,
            failed: false,
        }
    }

    pub(crate) async fn run(mut self) {
        let mut attempt: u32 = 0;
        loop {
            self.set_state(ConnectionState::Connecting);
            let deadline = Instant::now().checked_add(self.config.connect_timeout);
            let connect = with_deadline(deadline, connect_ws(Arc::clone(&self.config)));
            let outcome = match self.offline(connect).await {
                Offline::Close => break,
                Offline::Reconnect => {
                    attempt = 0;
                    continue;
                }
                Offline::Done(Err(error)) => Outcome::Lost {
                    reason: error.to_string(),
                    error,
                    stable: false,
                },
                Offline::Done(Ok((ws, liveness))) => {
                    self.run_connection(ws, liveness, deadline).await
                }
            };
            match outcome {
                Outcome::Closed => break,
                Outcome::Restart => {
                    info!("DDP reconnect requested");
                    attempt = 0;
                }
                Outcome::Fatal { reason, error } => {
                    if let Some(first) = self.first.take() {
                        let _ = first.send(Err(error));
                        break;
                    }
                    warn!(%reason, "DDP connection failed permanently");
                    self.enter_failed(reason);
                    match self.offline(std::future::pending::<()>()).await {
                        Offline::Close => break,
                        Offline::Reconnect | Offline::Done(()) => {
                            self.failed = false;
                            attempt = 0;
                        }
                    }
                }
                Outcome::Lost {
                    reason,
                    error,
                    stable,
                } => {
                    if let Some(first) = self.first.take() {
                        let _ = first.send(Err(error));
                        break;
                    }
                    if stable {
                        attempt = 0;
                    }
                    attempt = attempt.saturating_add(1);
                    let retry_in = self.config.backoff.delay(attempt);
                    info!(%reason, attempt, ?retry_in, "DDP connection lost; reconnecting");
                    self.set_state(ConnectionState::Reconnecting {
                        attempt,
                        retry_in,
                        reason,
                    });
                    match self.offline(tokio::time::sleep(retry_in)).await {
                        Offline::Close => break,
                        Offline::Reconnect => attempt = 0,
                        Offline::Done(()) => {}
                    }
                }
            }
        }
        self.shutdown();
    }

    fn set_state(&self, state: ConnectionState) {
        self.state_tx.send_replace(state);
    }

    fn is_redacted(&self, method: &str) -> bool {
        self.config.redacted_methods.iter().any(|m| m == method)
    }

    /// Run `fut` while no connection is usable, handling commands meanwhile.
    async fn offline<F: Future>(&mut self, fut: F) -> Offline<F::Output> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                out = &mut fut => return Offline::Done(out),
                cmd = self.cmd_rx.recv() => match cmd {
                    None | Some(Command::Close) => return Offline::Close,
                    Some(Command::ReconnectNow) => return Offline::Reconnect,
                    Some(Command::Call(req)) => {
                        if req.generation.is_some() || self.failed {
                            let _ = req.reply.send(Err(CallError::Disconnected));
                        } else {
                            self.enqueue(req);
                        }
                    }
                    Some(Command::CancelCall(id)) => self.cancel_call(&id),
                    Some(Command::Subscribe(req)) => {
                        if self.failed {
                            let _ = req.ready.send(Err(SubscribeError::Disconnected));
                        } else {
                            self.add_sub(req);
                        }
                    }
                    Some(Command::Unsubscribe(handle)) => {
                        self.remove_handle(handle);
                    }
                },
            }
        }
    }

    fn enqueue(&mut self, req: CallRequest) {
        self.queued.retain(|c| !c.reply.is_closed());
        self.queued.push_back(req);
    }

    fn cancel_call(&mut self, id: &str) {
        self.pending.remove(id);
        self.queued.retain(|c| c.id != id);
    }

    /// Attach a handle; returns the id of a newly created server-side subscription.
    fn add_sub(&mut self, req: SubRequest) -> Option<String> {
        let key = sub_key(&req.name, &req.params);
        let mut handle = Handle {
            filter: Filter::new(req.routing, &req.params),
            data: req.data,
            deliver: req.deliver,
            pending_lag: 0,
            marker: None,
            data_after_marker: false,
            ready: Some(req.ready),
            state: req.state,
        };
        if let Some(id) = self.sub_by_key.get(&key)
            && let Some(sub) = self.subs.get_mut(id)
        {
            if sub.ready {
                handle.state.send_replace(SubscriptionState::Ready);
                if let Some(ready) = handle.ready.take() {
                    let _ = ready.send(Ok(id.clone()));
                }
            }
            sub.handles.insert(req.handle, handle);
            self.sub_by_handle.insert(req.handle, id.clone());
            return None;
        }
        let id = self.shared.next_id();
        self.sub_by_key.insert(key.clone(), id.clone());
        self.sub_by_handle.insert(req.handle, id.clone());
        self.subs.insert(
            id.clone(),
            ServerSub {
                name: req.name,
                params: req.params,
                key,
                ready: false,
                handles: HashMap::from([(req.handle, handle)]),
            },
        );
        Some(id)
    }

    /// Detach a handle; returns the server-side id to `unsub` if it was the last one.
    fn remove_handle(&mut self, handle: u64) -> Option<String> {
        let id = self.sub_by_handle.remove(&handle)?;
        let sub = self.subs.get_mut(&id)?;
        sub.handles.remove(&handle);
        if !sub.handles.is_empty() {
            return None;
        }
        if let Some(sub) = self.subs.remove(&id) {
            self.sub_by_key.remove(&sub.key);
        }
        Some(id)
    }

    fn enter_failed(&mut self, reason: String) {
        self.failed = true;
        for req in self.queued.drain(..) {
            let _ = req.reply.send(Err(CallError::Disconnected));
        }
        for sub in self.subs.values_mut() {
            for handle in sub.handles.values_mut() {
                if let Some(ready) = handle.ready.take() {
                    let _ = ready.send(Err(SubscribeError::Disconnected));
                }
            }
        }
        self.set_state(ConnectionState::Failed { reason });
    }

    fn shutdown(&mut self) {
        self.cmd_rx.close();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                Command::Call(req) => self.queued.push_back(req),
                Command::Subscribe(req) => {
                    let _ = req.ready.send(Err(SubscribeError::Closed));
                    req.state.send_replace(SubscriptionState::Closed);
                }
                Command::CancelCall(_)
                | Command::Unsubscribe(_)
                | Command::ReconnectNow
                | Command::Close => {}
            }
        }
        for (_, call) in self.pending.drain() {
            let _ = call.reply.send(Err(CallError::Closed));
        }
        for req in self.queued.drain(..) {
            let _ = req.reply.send(Err(CallError::Closed));
        }
        for (_, sub) in std::mem::take(&mut self.subs) {
            for (_, handle) in sub.handles {
                if let Some(ready) = handle.ready {
                    let _ = ready.send(Err(SubscribeError::Closed));
                }
                handle.state.send_replace(SubscriptionState::Closed);
            }
        }
        self.sub_by_key.clear();
        self.sub_by_handle.clear();
        if let Some(first) = self.first.take() {
            let _ = first.send(Err(ConnectError::Closed));
        }
        info!("DDP client closed");
        self.set_state(ConnectionState::Closed);
    }

    async fn run_connection(
        &mut self,
        mut ws: Ws,
        liveness: Liveness,
        handshake_deadline: Option<Instant>,
    ) -> Outcome {
        self.generation = self.generation.wrapping_add(1);
        let mut conn = Conn {
            phase: Phase::Handshake,
            session: String::new(),
            liveness,
            ping_sent: None,
            ping_counter: 0,
            ready_at: None,
        };
        let mut outcome = self
            .connection_loop(&mut ws, &mut conn, handshake_deadline)
            .await;
        if let Outcome::Lost { stable, .. } = &mut outcome {
            *stable = conn
                .ready_at
                .is_some_and(|t| t.elapsed() >= self.config.backoff.stable_after);
        }

        let call_error = if matches!(outcome, Outcome::Closed) {
            CallError::Closed
        } else {
            CallError::Disconnected
        };
        for (_, call) in self.pending.drain() {
            let _ = call.reply.send(Err(call_error.clone()));
        }
        self.redacted_ids.clear();
        for sub in self.subs.values_mut() {
            sub.ready = false;
            for handle in sub.handles.values() {
                handle.state.send_if_modified(|s| {
                    let changed = *s == SubscriptionState::Ready;
                    if changed {
                        *s = SubscriptionState::Pending;
                    }
                    changed
                });
            }
        }
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, ws.close(None)).await;
        outcome
    }

    async fn connection_loop(
        &mut self,
        ws: &mut Ws,
        conn: &mut Conn,
        handshake_deadline: Option<Instant>,
    ) -> Outcome {
        let hello = ClientMessage::Connect {
            version: DDP_VERSION,
            support: DDP_SUPPORT,
        };
        if let Err(outcome) = self.send(ws, &hello).await {
            return outcome;
        }
        let heartbeat = self.config.heartbeat;
        let mut hook: Option<HookFuture> = None;
        loop {
            let last = conn.liveness.last();
            if conn.ping_sent.is_some_and(|sent| last >= sent) {
                conn.ping_sent = None;
            }
            let deadline = match (conn.phase, conn.ping_sent) {
                (Phase::Handshake, _) => handshake_deadline,
                (_, Some(sent)) => sent.checked_add(heartbeat.timeout),
                (_, None) => last.checked_add(heartbeat.interval),
            };
            // Commands and hook completion are low-volume and go first so that a
            // flood of inbound frames cannot starve them; inbound frames come
            // before the timer so that a buffered pong beats the heartbeat timeout.
            tokio::select! {
                biased;
                cmd = self.cmd_rx.recv() => match cmd {
                    None | Some(Command::Close) => return Outcome::Closed,
                    Some(Command::ReconnectNow) => return Outcome::Restart,
                    Some(cmd) => {
                        if let Err(outcome) = self.handle_command(ws, conn, cmd).await {
                            return outcome;
                        }
                    }
                },
                result = poll_hook(&mut hook) => {
                    hook = None;
                    match result {
                        Ok(()) => {
                            if let Err(outcome) = self.become_ready(ws, conn).await {
                                return outcome;
                            }
                        }
                        Err(e) if e.is_fatal() => {
                            return Outcome::Fatal {
                                reason: format!("on_connected hook failed: {e}"),
                                error: ConnectError::Hook(e),
                            };
                        }
                        Err(e) => {
                            return Outcome::Lost {
                                reason: format!("on_connected hook failed: {e}"),
                                error: ConnectError::Hook(e),
                                stable: false,
                            };
                        }
                    }
                }
                frame = ws.next() => {
                    let msg = match frame {
                        None => return lost("connection closed by server"),
                        Some(Err(e)) => return lost(format!("websocket error: {e}")),
                        Some(Ok(msg)) => msg,
                    };
                    match msg {
                        Message::Text(text) => {
                            if let Some(outcome) =
                                self.handle_text(ws, conn, &mut hook, text.as_str()).await
                            {
                                return outcome;
                            }
                        }
                        Message::Close(frame) => {
                            let reason = frame.map_or_else(
                                || "server closed the connection".to_owned(),
                                |f| format!("server closed the connection ({} {})", f.code, f.reason),
                            );
                            return lost(reason);
                        }
                        Message::Binary(_) => debug!("ignoring binary DDP frame"),
                        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
                () = sleep_until_opt(deadline) => {
                    if conn.phase == Phase::Handshake {
                        return Outcome::Lost {
                            reason: "DDP handshake timed out".to_owned(),
                            error: ConnectError::Timeout,
                            stable: false,
                        };
                    }
                    // Bytes may have arrived since the deadline was computed.
                    let now = Instant::now();
                    let last = conn.liveness.last();
                    match conn.ping_sent {
                        Some(sent) if last < sent => return lost("heartbeat timeout"),
                        Some(_) => {}
                        None => {
                            if last.checked_add(heartbeat.interval).is_some_and(|d| d <= now) {
                                conn.ping_counter += 1;
                                let id = conn.ping_counter.to_string();
                                let ping = ClientMessage::Ping { id: &id };
                                if let Err(outcome) = self.send(ws, &ping).await {
                                    return outcome;
                                }
                                conn.ping_sent = Some(Instant::now());
                            }
                        }
                    }
                }
            }
        }
    }

    async fn handle_command(
        &mut self,
        ws: &mut Ws,
        conn: &Conn,
        cmd: Command,
    ) -> Result<(), Outcome> {
        let ready = conn.phase == Phase::Ready;
        match cmd {
            Command::Call(req) => {
                let sendable = match req.generation {
                    Some(g) => g == self.generation && conn.phase != Phase::Handshake,
                    None => ready,
                };
                if sendable {
                    self.send_call(ws, req).await?;
                } else if req.generation.is_some() {
                    let _ = req.reply.send(Err(CallError::Disconnected));
                } else {
                    self.enqueue(req);
                }
            }
            Command::CancelCall(id) => self.cancel_call(&id),
            Command::Subscribe(req) => {
                if let Some(id) = self.add_sub(req)
                    && ready
                    && let Some(sub) = self.subs.get(&id)
                {
                    let msg = ClientMessage::Sub {
                        id: &id,
                        name: &sub.name,
                        params: &sub.params,
                    };
                    self.send(ws, &msg).await?;
                }
            }
            Command::Unsubscribe(handle) => {
                if let Some(id) = self.remove_handle(handle)
                    && ready
                {
                    self.send(ws, &ClientMessage::Unsub { id: &id }).await?;
                }
            }
            Command::ReconnectNow | Command::Close => {}
        }
        Ok(())
    }

    /// Send a method call. If the write fails, a user call goes back to the front
    /// of the queue (it never reached the server); a hook call fails.
    async fn send_call(&mut self, ws: &mut Ws, req: CallRequest) -> Result<(), Outcome> {
        if req.reply.is_closed() {
            return Ok(());
        }
        let msg = ClientMessage::Method {
            method: &req.method,
            params: &req.params,
            id: &req.id,
        };
        if let Err(outcome) = self.send(ws, &msg).await {
            if req.generation.is_some() {
                let _ = req.reply.send(Err(CallError::Disconnected));
            } else {
                self.queued.push_front(req);
            }
            return Err(outcome);
        }
        if self.is_redacted(&req.method) {
            self.redacted_ids.insert(req.id.clone());
        }
        self.pending
            .insert(req.id, PendingCall { reply: req.reply });
        Ok(())
    }

    async fn become_ready(&mut self, ws: &mut Ws, conn: &mut Conn) -> Result<(), Outcome> {
        conn.phase = Phase::Ready;
        conn.ready_at = Some(Instant::now());
        for (id, sub) in &self.subs {
            let msg = ClientMessage::Sub {
                id,
                name: &sub.name,
                params: &sub.params,
            };
            self.send(ws, &msg).await?;
        }
        while let Some(req) = self.queued.pop_front() {
            self.send_call(ws, req).await?;
        }
        info!("DDP connection ready");
        self.set_state(ConnectionState::Connected {
            session: conn.session.clone(),
        });
        if let Some(first) = self.first.take() {
            let _ = first.send(Ok(()));
        }
        Ok(())
    }

    async fn send(&self, ws: &mut Ws, msg: &ClientMessage<'_>) -> Result<(), Outcome> {
        let text = serde_json::to_string(msg)
            .map_err(|e| lost(format!("failed to encode DDP message: {e}")))?;
        match msg {
            ClientMessage::Method { method, id, .. } if self.is_redacted(method) => {
                debug!(%id, %method, "DDP -> method (params redacted)");
            }
            _ => debug!(frame = %excerpt(&text), "DDP ->"),
        }
        match tokio::time::timeout(self.config.write_timeout, ws.send(Message::text(text))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(lost(format!("websocket write failed: {e}"))),
            Err(_) => Err(lost("websocket write timed out")),
        }
    }

    fn start_hook(&self, session: String) -> Option<HookFuture> {
        let on_connected = self.config.on_connected.as_ref()?;
        let handle = DdpHandle::new(Arc::clone(&self.shared), self.generation, session);
        let fut = match std::panic::catch_unwind(AssertUnwindSafe(|| on_connected(handle))) {
            Ok(fut) => fut,
            Err(_) => {
                error!("on_connected hook panicked");
                return Some(futures::future::ready(Err(hook_panicked())).boxed());
            }
        };
        Some(
            AssertUnwindSafe(fut)
                .catch_unwind()
                .map(|result| {
                    result.unwrap_or_else(|_| {
                        error!("on_connected hook panicked");
                        Err(hook_panicked())
                    })
                })
                .boxed(),
        )
    }

    async fn handle_text(
        &mut self,
        ws: &mut Ws,
        conn: &mut Conn,
        hook: &mut Option<HookFuture>,
        text: &str,
    ) -> Option<Outcome> {
        let msg: ServerMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                log_unparsed(text, &e);
                return None;
            }
        };
        let redact = match &msg {
            ServerMessage::Result { id, .. } => self.redacted_ids.remove(id),
            _ => false,
        };
        if redact {
            debug!("DDP <- result (redacted)");
        } else {
            debug!(frame = %excerpt(text), "DDP <-");
        }

        match msg {
            ServerMessage::Connected { session } => {
                if conn.phase != Phase::Handshake {
                    debug!("ignoring duplicate DDP connected message");
                    return None;
                }
                info!("DDP handshake done");
                conn.session = session.clone();
                match self.start_hook(session) {
                    Some(fut) => {
                        conn.phase = Phase::Hook;
                        *hook = Some(fut);
                    }
                    None => return self.become_ready(ws, conn).await.err(),
                }
            }
            ServerMessage::Failed { version } => {
                return Some(Outcome::Fatal {
                    reason: format!(
                        "server rejected DDP version {DDP_VERSION} (suggests {version:?})"
                    ),
                    error: ConnectError::Handshake(version),
                });
            }
            ServerMessage::Ping { id } => {
                let pong = ClientMessage::Pong { id: id.as_deref() };
                return self.send(ws, &pong).await.err();
            }
            ServerMessage::Pong { .. } | ServerMessage::Updated { .. } => {}
            ServerMessage::Result { id, result, error } => match self.pending.remove(&id) {
                Some(call) => {
                    let outcome = match error {
                        Some(e) => Err(CallError::Server(Box::new(e))),
                        None => Ok(result.unwrap_or(Value::Null)),
                    };
                    let _ = call.reply.send(outcome);
                }
                None => debug!(%id, "ignoring result for unknown method call"),
            },
            ServerMessage::Ready { subs } => {
                for id in subs {
                    let Some(sub) = self.subs.get_mut(&id) else {
                        continue;
                    };
                    sub.ready = true;
                    for handle in sub.handles.values_mut() {
                        handle.state.send_replace(SubscriptionState::Ready);
                        if let Some(ready) = handle.ready.take() {
                            let _ = ready.send(Ok(id.clone()));
                        }
                    }
                }
            }
            ServerMessage::Nosub { id, error } => {
                if let Some(sub) = self.subs.remove(&id) {
                    self.sub_by_key.remove(&sub.key);
                    info!(%id, name = %sub.name, "server stopped subscription");
                    for (handle_id, handle) in sub.handles {
                        self.sub_by_handle.remove(&handle_id);
                        if let Some(ready) = handle.ready {
                            let error = error.clone().map(Box::new);
                            let _ = ready.send(Err(SubscribeError::Server(error)));
                        }
                        handle
                            .state
                            .send_replace(SubscriptionState::Stopped(error.clone()));
                    }
                }
            }
            ServerMessage::Added(raw) => self.route(raw.into_data(DataKind::Added)),
            ServerMessage::Changed(raw) => self.route(raw.into_data(DataKind::Changed)),
            ServerMessage::Removed(raw) => self.route(raw.into_data(DataKind::Removed)),
            ServerMessage::AddedBefore(raw) => self.route(raw.into_data(DataKind::AddedBefore)),
            ServerMessage::MovedBefore(raw) => self.route(raw.into_data(DataKind::MovedBefore)),
            ServerMessage::Error { reason } => {
                warn!(
                    reason = reason.as_deref().unwrap_or(""),
                    "DDP error message from server"
                );
            }
        }
        None
    }

    fn route(&mut self, data: DataMessage) {
        let data = Arc::new(data);
        let event = data.event_name();
        for sub in self.subs.values_mut() {
            if sub.name != data.collection {
                continue;
            }
            for handle in sub.handles.values_mut() {
                if handle.deliver && handle.filter.matches(event) {
                    handle.deliver(&data);
                }
            }
        }
        let _ = self.shared.data_tx.send(data);
    }
}

impl Handle {
    /// Queue a message without blocking. A full queue drops the message; drops
    /// are reported by a `Lagged` marker placed after the older messages.
    fn deliver(&mut self, data: &Arc<DataMessage>) {
        if self.data.is_closed() {
            return;
        }
        self.flush_lag();
        // One slot stays free for the marker.
        if self.pending_lag == 0
            && self.data.capacity() > 1
            && self.data.try_send(SubItem::Data(Arc::clone(data))).is_ok()
        {
            self.data_after_marker = true;
            return;
        }
        self.record_drop();
    }

    fn record_drop(&mut self) {
        if !self.data_after_marker
            && let Some(marker) = &self.marker
            && add_lag(marker)
        {
            return;
        }
        self.pending_lag += 1;
        self.flush_lag();
    }

    fn flush_lag(&mut self) {
        if self.pending_lag == 0 {
            return;
        }
        let marker = Arc::new(AtomicU64::new(self.pending_lag));
        if self
            .data
            .try_send(SubItem::Lagged(Arc::clone(&marker)))
            .is_ok()
        {
            self.pending_lag = 0;
            self.marker = Some(marker);
            self.data_after_marker = false;
        }
    }
}

/// Add one drop to a queued marker unless the consumer already took it.
fn add_lag(marker: &AtomicU64) -> bool {
    let mut current = marker.load(Ordering::Acquire);
    while current != LAG_TAKEN {
        match marker.compare_exchange_weak(
            current,
            current.saturating_add(1).min(LAG_TAKEN - 1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
    false
}

fn hook_panicked() -> HookError {
    HookError::retry("on_connected hook panicked")
}

fn log_unparsed(text: &str, error: &serde_json::Error) {
    match serde_json::from_str::<Value>(text) {
        // Syntax errors only carry a position, never content.
        Err(_) => warn!(%error, len = text.len(), "ignoring malformed DDP frame"),
        // Data errors may quote values (e.g. a result field), so no details.
        Ok(value) => match value.get("msg").and_then(Value::as_str) {
            None => trace!(len = text.len(), "ignoring DDP frame without msg"),
            Some(kind) => debug!(msg = %kind, "ignoring unknown or malformed DDP message"),
        },
    }
}

/// Identity of a subscription: name plus params with object keys sorted.
fn sub_key(name: &str, params: &[Value]) -> String {
    fn canonical(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                out.push('{');
                for (i, (k, v)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    canonical(v, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(v, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut key = Value::String(name.to_owned()).to_string();
    canonical(&Value::Array(params.to_vec()), &mut key);
    key
}

async fn poll_hook(hook: &mut Option<HookFuture>) -> Result<(), HookError> {
    match hook {
        Some(fut) => fut.await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn with_deadline<T>(
    deadline: Option<Instant>,
    fut: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, ConnectError> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, fut)
            .await
            .unwrap_or(Err(ConnectError::Timeout)),
        None => fut.await,
    }
}

async fn connect_ws(config: Arc<DdpConfig>) -> Result<(Ws, Liveness), ConnectError> {
    let url = &config.url;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| ConnectError::WebSocket(Box::new(e)))?;
    for (name, value) in &config.headers {
        request.headers_mut().insert(name.clone(), value.clone());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ConnectError::InvalidUrl(format!("no port in {url}")))?;
    debug!(%url, "DDP connecting");
    let tcp = match url.host() {
        Some(url::Host::Domain(host)) => TcpStream::connect((host, port)).await,
        Some(url::Host::Ipv4(ip)) => TcpStream::connect((ip, port)).await,
        Some(url::Host::Ipv6(ip)) => TcpStream::connect((ip, port)).await,
        None => return Err(ConnectError::InvalidUrl(format!("no host in {url}"))),
    }
    .map_err(ConnectError::Io)?;
    let _ = tcp.set_nodelay(true);
    let liveness = Liveness::new();
    let stream = Tracked {
        inner: tcp,
        liveness: liveness.clone(),
    };
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(config.max_message_size))
        .max_frame_size(Some(config.max_message_size));
    let (ws, _response) = tokio_tungstenite::client_async_tls_with_config(
        request,
        stream,
        Some(ws_config),
        config.connector.clone(),
    )
    .await
    .map_err(|e| ConnectError::WebSocket(Box::new(e)))?;
    Ok((ws, liveness))
}

/// Time of the last byte received on a connection.
#[derive(Clone)]
struct Liveness {
    base: Instant,
    nanos: Arc<AtomicU64>,
}

impl Liveness {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            nanos: Arc::new(AtomicU64::new(0)),
        }
    }

    fn stamp(&self) {
        let nanos = u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.nanos.store(nanos, Ordering::Relaxed);
    }

    fn last(&self) -> Instant {
        let offset = std::time::Duration::from_nanos(self.nanos.load(Ordering::Relaxed));
        self.base.checked_add(offset).unwrap_or(self.base)
    }
}

/// Stream adapter that stamps [`Liveness`] on every successful read.
struct Tracked<S> {
    inner: S,
    liveness: Liveness,
}

impl<S: AsyncRead + Unpin> AsyncRead for Tracked<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.liveness.stamp();
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Tracked<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

fn excerpt(text: &str) -> &str {
    match text.char_indices().nth(LOG_FRAME_LIMIT) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sub_key_ignores_object_key_order() {
        let a = sub_key(
            "s",
            &[json!("x"), json!({"a": 1, "b": [ {"c": 1, "d": 2} ]})],
        );
        let b = sub_key(
            "s",
            &[json!("x"), json!({"b": [ {"d": 2, "c": 1} ], "a": 1})],
        );
        assert_eq!(a, b);
        assert_ne!(a, sub_key("t", &[json!("x"), json!({"a": 1, "b": []})]));
        assert_ne!(sub_key("s", &[json!("1")]), sub_key("s", &[json!(1)]));
    }

    #[test]
    fn filters() {
        let auto = Filter::new(Routing::Auto, &[json!("room")]);
        assert!(auto.matches(Some("room")) && auto.matches(None) && !auto.matches(Some("x")));
        let presence = Filter::new(Routing::Auto, &[json!(""), json!({})]);
        assert!(presence.matches(Some("uid")));
        let exact = Filter::new(Routing::EventName("a".into()), &[]);
        assert!(exact.matches(Some("a")) && !exact.matches(None));
        let prefix = Filter::new(Routing::EventNamePrefix("u1/".into()), &[]);
        assert!(prefix.matches(Some("u1/message")) && !prefix.matches(Some("u2/message")));
    }
}
