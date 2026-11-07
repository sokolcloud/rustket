//! Public client handles.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::config::DdpConfig;
use crate::driver::{CallRequest, Command, Driver, LAG_TAKEN, SubItem, SubRequest};
use crate::error::{CallError, ConnectError, SubscribeError};
use crate::message::{DataMessage, MeteorError};

/// Connection state of a [`DdpClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Opening the WebSocket, performing the DDP handshake or running the
    /// `on_connected` hook.
    Connecting,
    /// Handshake and hook done; calls and subscriptions flow.
    Connected {
        /// DDP session id from the server.
        session: String,
    },
    /// The connection was lost or could not be established; the next attempt
    /// starts after `retry_in`.
    Reconnecting {
        /// 1-based number of the upcoming attempt since the last stable connection.
        attempt: u32,
        /// Delay before the next attempt.
        retry_in: Duration,
        /// Why the previous connection ended.
        reason: String,
    },
    /// Terminal failure (hook reported a fatal error, or the server rejected the
    /// protocol version). No reconnects happen until [`DdpClient::reconnect_now`].
    Failed {
        /// Why the client failed.
        reason: String,
    },
    /// The client was closed.
    Closed,
}

impl ConnectionState {
    /// Whether calls are currently sent immediately.
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// Server-side state of a [`Subscription`].
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionState {
    /// Waiting for `ready` (initially and after every reconnect).
    Pending,
    /// The server sent `ready`.
    Ready,
    /// The server stopped the subscription with `nosub`; it is not renewed.
    Stopped(Option<MeteorError>),
    /// The client was closed.
    Closed,
}

/// How data messages of a subscription's collection are matched to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Routing {
    /// If the first param is a non-empty string, messages carrying
    /// `fields.eventName` must have that event name (messages without one are
    /// delivered); otherwise every message of the collection is delivered.
    /// An empty first param (e.g. `stream-user-presence` with `["", {..}]`)
    /// means collection-only routing.
    #[default]
    Auto,
    /// Every message whose `collection` equals the subscription name.
    Collection,
    /// Only messages whose `fields.eventName` equals this value.
    EventName(String),
    /// Only messages whose `fields.eventName` starts with this value.
    EventNamePrefix(String),
}

/// Options of [`DdpClient::subscribe_with`].
#[derive(Debug, Clone, Default)]
pub struct SubscribeOptions {
    /// Routing of data messages to this subscription.
    pub routing: Routing,
    /// Queue length; `None` uses [`DdpConfig::subscription_buffer`]. `Some(0)`
    /// makes a keep-alive-only subscription that receives no data messages (read
    /// them from [`DdpClient::data_messages`] instead). When the queue is full,
    /// new messages are dropped and reported as [`RecvError::Lagged`].
    pub buffer: Option<usize>,
    /// Timeout for `ready`; `None` uses [`DdpConfig::subscribe_timeout`].
    pub timeout: Option<Duration>,
}

/// Error of [`Subscription::recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    /// This many messages were dropped because the queue was full; the consumer
    /// should resynchronise. Further messages follow.
    #[error("subscription lagged by {0} messages")]
    Lagged(u64),
    /// The subscription stopped (`nosub`) or the client closed.
    #[error("subscription closed")]
    Closed,
}

/// Item of the [`Subscription`] stream when messages were dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("subscription lagged by {0} messages")]
pub struct Lagged(pub u64);

/// Upper bound for subscription queue lengths.
const MAX_SUBSCRIPTION_BUFFER: usize = 1 << 20;
/// Upper bound for the broadcast channel capacity.
const MAX_BROADCAST_CAPACITY: usize = 1 << 16;

pub(crate) struct Shared {
    pub(crate) cmd_tx: mpsc::UnboundedSender<Command>,
    pub(crate) data_tx: broadcast::Sender<Arc<DataMessage>>,
    state_rx: watch::Receiver<ConnectionState>,
    next_id: AtomicU64,
    call_timeout: Duration,
    subscribe_timeout: Duration,
    subscription_buffer: usize,
}

/// Tells the driver to forget a call whose caller stopped waiting.
struct CancelOnDrop<'a> {
    cmd_tx: &'a mpsc::UnboundedSender<Command>,
    id: Option<String>,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.cmd_tx.send(Command::CancelCall(id));
        }
    }
}

impl Shared {
    pub(crate) fn next_id(&self) -> String {
        self.next_raw_id().to_string()
    }

    fn next_raw_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn call(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Duration,
        generation: Option<u64>,
    ) -> Result<Value, CallError> {
        let (reply, rx) = oneshot::channel();
        let id = self.next_id();
        let request = CallRequest {
            id: id.clone(),
            method: method.to_owned(),
            params,
            reply,
            generation,
        };
        self.cmd_tx
            .send(Command::Call(request))
            .map_err(|_| CallError::Closed)?;
        let mut cancel = CancelOnDrop {
            cmd_tx: &self.cmd_tx,
            id: Some(id),
        };
        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CallError::Closed),
            Err(_) => return Err(CallError::Timeout),
        };
        cancel.id = None;
        result
    }
}

/// Sends `Close` to the driver when the last [`DdpClient`] clone is dropped.
struct CloseGuard(mpsc::UnboundedSender<Command>);

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Command::Close);
    }
}

/// A DDP client. Cheap to clone; the connection is owned by a background task
/// which stops when [`close`](Self::close) is called or the last clone is dropped.
#[derive(Clone)]
pub struct DdpClient {
    shared: Arc<Shared>,
    _guard: Arc<CloseGuard>,
}

impl std::fmt::Debug for DdpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdpClient")
            .field("state", &*self.shared.state_rx.borrow())
            .finish_non_exhaustive()
    }
}

impl DdpClient {
    /// Connect and wait until the first handshake and `on_connected` hook succeed.
    /// Any failure of this first attempt is returned and the client is discarded;
    /// later connection losses are handled by reconnecting.
    pub async fn connect(config: DdpConfig) -> Result<Self, ConnectError> {
        let (first_tx, first_rx) = oneshot::channel();
        let client = Self::start(config, Some(first_tx));
        match first_rx.await {
            Ok(Ok(())) => Ok(client),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ConnectError::Closed),
        }
    }

    /// Start a client that connects in the background and keeps reconnecting.
    /// Observe progress through [`watch_state`](Self::watch_state).
    ///
    /// Must be called from within a Tokio runtime.
    pub fn spawn(config: DdpConfig) -> Self {
        Self::start(config, None)
    }

    fn start(config: DdpConfig, first: Option<oneshot::Sender<Result<(), ConnectError>>>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(ConnectionState::Connecting);
        let (data_tx, _) =
            broadcast::channel(config.broadcast_capacity.clamp(1, MAX_BROADCAST_CAPACITY));
        let shared = Arc::new(Shared {
            cmd_tx: cmd_tx.clone(),
            data_tx,
            state_rx,
            next_id: AtomicU64::new(1),
            call_timeout: config.call_timeout,
            subscribe_timeout: config.subscribe_timeout,
            subscription_buffer: config.subscription_buffer,
        });
        let driver = Driver::new(config, Arc::clone(&shared), cmd_rx, state_tx, first);
        tokio::spawn(driver.run());
        Self {
            shared,
            _guard: Arc::new(CloseGuard(cmd_tx)),
        }
    }

    /// Call a method with the default timeout.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, CallError> {
        self.call_with_timeout(method, params, self.shared.call_timeout)
            .await
    }

    /// Call a method. If the client is not connected, the call waits for the
    /// connection (bounded by `timeout`). A missing `result` is returned as `null`.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        self.shared.call(method, params, timeout, None).await
    }

    /// Subscribe with default options; resolves on `ready`.
    pub async fn subscribe(
        &self,
        name: &str,
        params: Vec<Value>,
    ) -> Result<Subscription, SubscribeError> {
        self.subscribe_with(name, params, SubscribeOptions::default())
            .await
    }

    /// Subscribe with default options and the given timeout.
    pub async fn subscribe_with_timeout(
        &self,
        name: &str,
        params: Vec<Value>,
        timeout: Duration,
    ) -> Result<Subscription, SubscribeError> {
        let options = SubscribeOptions {
            timeout: Some(timeout),
            ..SubscribeOptions::default()
        };
        self.subscribe_with(name, params, options).await
    }

    /// Subscribe; resolves on `ready`, fails on `nosub`. The subscription is
    /// renewed after every reconnect until the last handle is dropped.
    ///
    /// Identical subscriptions (same name and params) share one server-side
    /// subscription: joining a ready one resolves immediately, `unsub` is sent
    /// when the last handle goes away, and every handle receives each matching
    /// data message exactly once. Only object key order is normalised: callers
    /// must build identical params for the same logical subscription (e.g.
    /// Rocket.Chat accepts both `{"useCollection":false,"args":[]}` and `false` as
    /// the second stream param; mixing them creates two server-side
    /// subscriptions and therefore duplicate events).
    ///
    /// Rocket.Chat answers a second, different `stream-user-presence`
    /// subscription with an immediate `nosub` (it merges `added` users into the
    /// first one): keep a single presence subscription per connection.
    pub async fn subscribe_with(
        &self,
        name: &str,
        params: Vec<Value>,
        options: SubscribeOptions,
    ) -> Result<Subscription, SubscribeError> {
        let handle = self.shared.next_raw_id();
        let buffer = options
            .buffer
            .unwrap_or(self.shared.subscription_buffer)
            .min(MAX_SUBSCRIPTION_BUFFER);
        let (ready_tx, ready_rx) = oneshot::channel();
        // One extra slot is reserved for the lag marker.
        let (data_tx, data_rx) = mpsc::channel(buffer.max(1) + 1);
        let (state_tx, state_rx) = watch::channel(SubscriptionState::Pending);
        let request = SubRequest {
            handle,
            name: name.to_owned(),
            params,
            routing: options.routing,
            data: data_tx,
            deliver: buffer > 0,
            ready: ready_tx,
            state: state_tx,
        };
        self.shared
            .cmd_tx
            .send(Command::Subscribe(request))
            .map_err(|_| SubscribeError::Closed)?;
        // Created before waiting so that a timeout or a dropped future unsubscribes.
        let mut subscription = Subscription {
            handle,
            id: String::new(),
            name: name.to_owned(),
            rx: data_rx,
            state: state_rx,
            cmd_tx: self.shared.cmd_tx.clone(),
        };
        let timeout = options.timeout.unwrap_or(self.shared.subscribe_timeout);
        match tokio::time::timeout(timeout, ready_rx).await {
            Ok(Ok(Ok(id))) => {
                subscription.id = id;
                Ok(subscription)
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(SubscribeError::Closed),
            Err(_) => Err(SubscribeError::Timeout),
        }
    }

    /// Current connection state.
    pub fn state(&self) -> ConnectionState {
        self.shared.state_rx.borrow().clone()
    }

    /// Receiver of connection state changes.
    pub fn watch_state(&self) -> watch::Receiver<ConnectionState> {
        self.shared.state_rx.clone()
    }

    /// Client-wide stream of every data message (`added`, `changed`, …), for
    /// consumers that route messages themselves. Slow receivers lag (see
    /// [`broadcast::error::RecvError::Lagged`]).
    pub fn data_messages(&self) -> broadcast::Receiver<Arc<DataMessage>> {
        self.shared.data_tx.subscribe()
    }

    /// Reconnect immediately: skips a pending backoff delay, drops a live
    /// connection (in-flight calls fail with [`CallError::Disconnected`]) or
    /// leaves the [`Failed`](ConnectionState::Failed) state.
    pub fn reconnect_now(&self) {
        let _ = self.shared.cmd_tx.send(Command::ReconnectNow);
    }

    /// Close the connection and stop the background task. Pending calls fail with
    /// [`CallError::Closed`] and subscription streams end. Waits until the task
    /// has finished.
    pub async fn close(&self) {
        let _ = self.shared.cmd_tx.send(Command::Close);
        let mut state = self.shared.state_rx.clone();
        let _ = state
            .wait_for(|s| matches!(s, ConnectionState::Closed))
            .await;
    }
}

/// Handle passed to the `on_connected` hook. Its calls are sent right away on the
/// connection that triggered the hook (ahead of queued user calls) and fail with
/// [`CallError::Disconnected`] if that connection is gone.
#[derive(Clone)]
pub struct DdpHandle {
    shared: Arc<Shared>,
    generation: u64,
    session: String,
}

impl std::fmt::Debug for DdpHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdpHandle")
            .field("generation", &self.generation)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl DdpHandle {
    pub(crate) fn new(shared: Arc<Shared>, generation: u64, session: String) -> Self {
        Self {
            shared,
            generation,
            session,
        }
    }

    /// DDP session id of this connection.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Call a method on this connection with the default timeout.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, CallError> {
        self.call_with_timeout(method, params, self.shared.call_timeout)
            .await
    }

    /// Call a method on this connection.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        self.shared
            .call(method, params, timeout, Some(self.generation))
            .await
    }
}

/// A subscription handle. Receives the data messages whose `collection` equals
/// the subscription name and that match its [`Routing`].
///
/// Dropping it (or calling [`unsubscribe`](Self::unsubscribe)) releases it; `unsub`
/// is sent when no other handle shares the server-side subscription. Receiving
/// ends when the server stops the subscription or the client closes.
#[derive(Debug)]
pub struct Subscription {
    handle: u64,
    id: String,
    name: String,
    rx: mpsc::Receiver<SubItem>,
    state: watch::Receiver<SubscriptionState>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl Subscription {
    /// DDP id of the (possibly shared) server-side subscription.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Subscription (publication) name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Next data message. [`RecvError::Lagged`] reports messages dropped because
    /// the queue was full; it comes after the older queued messages and before
    /// the ones received after the gap. [`RecvError::Closed`] means the
    /// subscription ended.
    pub async fn recv(&mut self) -> Result<Arc<DataMessage>, RecvError> {
        match self.rx.recv().await {
            Some(SubItem::Data(message)) => Ok(message),
            Some(SubItem::Lagged(n)) => Err(RecvError::Lagged(n.swap(LAG_TAKEN, Ordering::AcqRel))),
            None => Err(RecvError::Closed),
        }
    }

    /// Current server-side state.
    pub fn state(&self) -> SubscriptionState {
        self.state.borrow().clone()
    }

    /// Receiver of server-side state changes (e.g. a later `nosub`).
    pub fn watch_state(&self) -> watch::Receiver<SubscriptionState> {
        self.state.clone()
    }

    /// Release this handle (sends `unsub` if it was the last one).
    pub fn unsubscribe(self) {
        drop(self);
    }
}

/// Yields `Err(Lagged(n))` when messages were dropped; ends when the subscription does.
impl Stream for Subscription {
    type Item = Result<Arc<DataMessage>, Lagged>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|item| {
            item.map(|item| match item {
                SubItem::Data(message) => Ok(message),
                SubItem::Lagged(n) => Err(Lagged(n.swap(LAG_TAKEN, Ordering::AcqRel))),
            })
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Unsubscribe(self.handle));
    }
}
