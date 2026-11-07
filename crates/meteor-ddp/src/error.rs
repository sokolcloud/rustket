//! Error types.

use std::fmt;

use crate::message::MeteorError;

/// Error of a method call.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CallError {
    /// The server answered with an error.
    #[error("server error: {0}")]
    Server(Box<MeteorError>),
    /// The connection dropped after the call was sent and before its result arrived.
    /// The call may or may not have been executed by the server; it is not retried.
    /// Also returned when the client is in the terminal [`Failed`](crate::ConnectionState::Failed)
    /// state.
    #[error("connection lost before the method result arrived")]
    Disconnected,
    /// No result within the call timeout (including time spent waiting for a connection).
    #[error("method call timed out")]
    Timeout,
    /// The client was closed.
    #[error("client closed")]
    Closed,
}

/// Error of a subscription request.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SubscribeError {
    /// The server refused the subscription with `nosub`.
    #[error("subscription refused: {}", .0.as_ref().map_or_else(|| "nosub".to_owned(), ToString::to_string))]
    Server(Option<Box<MeteorError>>),
    /// The client is in the terminal [`Failed`](crate::ConnectionState::Failed) state.
    #[error("client is disconnected (failed state)")]
    Disconnected,
    /// No `ready` within the subscribe timeout.
    #[error("subscription timed out")]
    Timeout,
    /// The client was closed.
    #[error("client closed")]
    Closed,
}

/// Error of [`DdpClient::connect`](crate::DdpClient::connect) (first connection attempt).
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The URL has no host or port.
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    /// TCP connect failed.
    #[error("tcp connect failed: {0}")]
    Io(#[source] std::io::Error),
    /// The WebSocket connection could not be established.
    #[error("websocket error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    /// Timed out while connecting or waiting for the DDP handshake.
    #[error("connection timed out")]
    Timeout,
    /// The connection was established but lost before the client became ready.
    #[error("connection lost: {0}")]
    Lost(String),
    /// The server rejected the DDP handshake (`failed`).
    #[error("server rejected the DDP handshake (server suggests version {0:?})")]
    Handshake(Option<String>),
    /// The `on_connected` hook failed.
    #[error("on_connected hook failed: {0}")]
    Hook(HookError),
    /// The client was closed before it connected.
    #[error("client closed")]
    Closed,
}

/// Whether an [`HookError`] should stop reconnecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookErrorKind {
    /// Terminal (e.g. authentication rejected): the client enters
    /// [`ConnectionState::Failed`](crate::ConnectionState::Failed) and stops reconnecting.
    Fatal,
    /// Transient: the connection is dropped and retried with backoff.
    Retry,
}

/// Error returned by the `on_connected` hook.
#[derive(Debug, Clone)]
pub struct HookError {
    kind: HookErrorKind,
    message: String,
    server: Option<Box<MeteorError>>,
}

impl HookError {
    /// A terminal error: stop reconnecting.
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            kind: HookErrorKind::Fatal,
            message: message.into(),
            server: None,
        }
    }

    /// A transient error: reconnect with backoff.
    pub fn retry(message: impl Into<String>) -> Self {
        Self {
            kind: HookErrorKind::Retry,
            message: message.into(),
            server: None,
        }
    }

    /// Attach the server error that caused this hook error.
    pub fn with_server_error(mut self, error: impl Into<Box<MeteorError>>) -> Self {
        self.server = Some(error.into());
        self
    }

    /// Fatal or retryable.
    pub fn kind(&self) -> HookErrorKind {
        self.kind
    }

    /// Whether this error is terminal.
    pub fn is_fatal(&self) -> bool {
        self.kind == HookErrorKind::Fatal
    }

    /// Human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The server error behind this hook error, if any.
    pub fn server_error(&self) -> Option<&MeteorError> {
        self.server.as_deref()
    }
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HookError {}

/// Server error codes that make a hook error fatal by default: authentication
/// failures that will not go away by retrying.
pub const FATAL_HOOK_ERROR_CODES: &[&str] = &[
    "401",
    "403",
    "totp-required",
    "totp-invalid",
    "error-app-user-is-not-allowed-to-login",
    "error-user-is-not-activated",
];

/// Default classification: server errors with a code in
/// [`FATAL_HOOK_ERROR_CODES`] (e.g. 401 "User not found", 403 "You've been logged
/// out by the server") are fatal; everything else (other server errors such as
/// `500`, the time-limited `error-login-blocked-for-ip`/`-user`,
/// `too-many-requests`, timeouts, disconnects) is retried with backoff. Use
/// [`HookError::fatal`] / [`HookError::retry`] for explicit control.
impl From<CallError> for HookError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Server(e) if FATAL_HOOK_ERROR_CODES.iter().any(|c| e.is(c)) => {
                HookError::fatal(e.to_string()).with_server_error(e)
            }
            CallError::Server(e) => HookError::retry(e.to_string()).with_server_error(e),
            other => HookError::retry(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(error: serde_json::Value) -> CallError {
        CallError::Server(Box::new(MeteorError {
            error,
            reason: None,
            message: None,
            error_type: None,
            details: None,
        }))
    }

    #[test]
    fn hook_error_classification() {
        assert!(HookError::from(server(json!(403))).is_fatal());
        assert!(HookError::from(server(json!(401))).is_fatal());
        assert!(HookError::from(server(json!("403"))).is_fatal());
        assert!(HookError::from(server(json!("totp-required"))).is_fatal());
        assert!(HookError::from(server(json!("error-user-is-not-activated"))).is_fatal());
        for code in [
            json!(500),
            json!("error-login-blocked-for-ip"),
            json!("error-login-blocked-for-user"),
            json!("too-many-requests"),
        ] {
            assert!(!HookError::from(server(code)).is_fatal());
        }
        assert!(!HookError::from(CallError::Timeout).is_fatal());
        assert!(!HookError::from(CallError::Disconnected).is_fatal());
        let e = HookError::from(server(json!(403)));
        assert_eq!(
            e.server_error().map(MeteorError::code).as_deref(),
            Some("403")
        );
    }
}
