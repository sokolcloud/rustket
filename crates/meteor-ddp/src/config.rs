//! Client configuration.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::http::HeaderMap;
use url::Url;

use crate::client::DdpHandle;
use crate::error::HookError;

/// Async hook run after every successful DDP handshake, before subscriptions are
/// re-sent and queued calls proceed.
pub type OnConnected =
    Arc<dyn Fn(DdpHandle) -> BoxFuture<'static, Result<(), HookError>> + Send + Sync>;

/// Methods whose params and results are redacted from logs by default.
pub const DEFAULT_REDACTED_METHODS: &[&str] = &[
    "login",
    "saveUserProfile",
    "registerUser",
    "setUserPassword",
    "resetPassword",
    "personalAccessTokens:generateToken",
    "personalAccessTokens:regenerateToken",
    "e2e.fetchMyKeys",
    "e2e.setUserPublicAndPrivateKeys",
    "2fa:enable",
    "2fa:validateTempToken",
    "2fa:regenerateCodes",
    "2fa:checkCodesRemaining",
];

/// Client-side heartbeat settings. `Duration::MAX` disables the respective timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatConfig {
    /// Send a `ping` after this long without inbound traffic.
    pub interval: Duration,
    /// Consider the connection dead if nothing arrives this long after our `ping`.
    pub timeout: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(25),
            timeout: Duration::from_secs(15),
        }
    }
}

/// Reconnect backoff settings: `initial * factor^(attempt-1)`, capped at `max`,
/// multiplied by a random jitter factor in `[1 - jitter, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackoffConfig {
    /// Delay before the first reconnect attempt.
    pub initial: Duration,
    /// Upper bound of the delay.
    pub max: Duration,
    /// Growth factor per attempt.
    pub factor: f64,
    /// Jitter fraction in `[0, 1]`.
    pub jitter: f64,
    /// A connection that stayed ready this long resets the attempt counter.
    pub stable_after: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
            factor: 2.0,
            jitter: 0.5,
            stable_after: Duration::from_secs(30),
        }
    }
}

impl BackoffConfig {
    /// Delay before reconnect attempt `attempt` (1-based).
    pub fn delay(&self, attempt: u32) -> Duration {
        let exp = attempt.saturating_sub(1).min(64) as i32;
        let base = self.initial.as_secs_f64() * self.factor.max(1.0).powi(exp);
        let capped = base.min(self.max.as_secs_f64()).max(0.0);
        let jitter = self.jitter.clamp(0.0, 1.0);
        let scale = if jitter > 0.0 {
            rand::random_range((1.0 - jitter)..=1.0)
        } else {
            1.0
        };
        Duration::try_from_secs_f64(capped * scale).unwrap_or(self.max)
    }
}

/// Configuration of a [`DdpClient`](crate::DdpClient).
#[derive(Clone)]
pub struct DdpConfig {
    /// WebSocket endpoint, e.g. `wss://chat.example.com/websocket`.
    pub url: Url,
    /// TLS connector; `None` uses tokio-tungstenite defaults.
    pub connector: Option<Connector>,
    /// Extra HTTP headers for the upgrade request (e.g. `User-Agent`).
    pub headers: HeaderMap,
    /// Client heartbeat.
    pub heartbeat: HeartbeatConfig,
    /// Reconnect backoff.
    pub backoff: BackoffConfig,
    /// Total timeout of one connection attempt: TCP, TLS, WebSocket upgrade and
    /// DDP handshake together. `Duration::MAX` disables it.
    pub connect_timeout: Duration,
    /// Timeout of a single WebSocket write.
    pub write_timeout: Duration,
    /// Default timeout of [`DdpClient::call`](crate::DdpClient::call).
    pub call_timeout: Duration,
    /// Default timeout of [`DdpClient::subscribe`](crate::DdpClient::subscribe).
    pub subscribe_timeout: Duration,
    /// Maximum size of an inbound WebSocket message.
    pub max_message_size: usize,
    /// Capacity of the client-wide data message broadcast channel (clamped to
    /// `1..=65536`).
    pub broadcast_capacity: usize,
    /// Default per-subscription queue length (see
    /// [`SubscribeOptions::buffer`](crate::SubscribeOptions::buffer)).
    pub subscription_buffer: usize,
    /// Methods whose params and results are never logged (default:
    /// [`DEFAULT_REDACTED_METHODS`]).
    pub redacted_methods: Vec<String>,
    /// Hook run after every successful handshake.
    pub on_connected: Option<OnConnected>,
}

impl DdpConfig {
    /// Configuration with defaults for the given endpoint.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            connector: None,
            headers: HeaderMap::new(),
            heartbeat: HeartbeatConfig::default(),
            backoff: BackoffConfig::default(),
            connect_timeout: Duration::from_secs(20),
            write_timeout: Duration::from_secs(60),
            call_timeout: Duration::from_secs(60),
            subscribe_timeout: Duration::from_secs(60),
            max_message_size: 128 << 20,
            broadcast_capacity: 4096,
            subscription_buffer: 4096,
            redacted_methods: DEFAULT_REDACTED_METHODS
                .iter()
                .map(|m| (*m).to_owned())
                .collect(),
            on_connected: None,
        }
    }

    /// Set the TLS connector.
    pub fn connector(mut self, connector: Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// Set the heartbeat settings.
    pub fn heartbeat(mut self, heartbeat: HeartbeatConfig) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    /// Set the backoff settings.
    pub fn backoff(mut self, backoff: BackoffConfig) -> Self {
        self.backoff = backoff;
        self
    }

    /// Set the `on_connected` hook from an async closure.
    pub fn on_connected<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(DdpHandle) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HookError>> + Send + 'static,
    {
        self.on_connected = Some(Arc::new(move |handle| hook(handle).boxed()));
        self
    }
}

impl fmt::Debug for DdpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DdpConfig")
            .field("url", &self.url.as_str())
            .field("connector", &self.connector.as_ref().map(|_| ".."))
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("heartbeat", &self.heartbeat)
            .field("backoff", &self.backoff)
            .field("connect_timeout", &self.connect_timeout)
            .field("write_timeout", &self.write_timeout)
            .field("subscription_buffer", &self.subscription_buffer)
            .field("call_timeout", &self.call_timeout)
            .field("subscribe_timeout", &self.subscribe_timeout)
            .field("max_message_size", &self.max_message_size)
            .field("broadcast_capacity", &self.broadcast_capacity)
            .field("redacted_methods", &self.redacted_methods)
            .field("on_connected", &self.on_connected.is_some())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let b = BackoffConfig {
            jitter: 0.0,
            ..BackoffConfig::default()
        };
        assert_eq!(b.delay(1), Duration::from_millis(500));
        assert_eq!(b.delay(2), Duration::from_secs(1));
        assert_eq!(b.delay(4), Duration::from_secs(4));
        assert_eq!(b.delay(20), Duration::from_secs(30));
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(30));
        let j = BackoffConfig::default();
        for attempt in 1..10 {
            let d = j.delay(attempt);
            let full = b.delay(attempt);
            assert!(d <= full && d >= full / 2, "{d:?} vs {full:?}");
        }
    }
}
