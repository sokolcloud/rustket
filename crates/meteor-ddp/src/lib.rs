//! Generic Meteor DDP client over WebSocket (tokio + tokio-tungstenite + rustls).
//!
//! A background task owns the socket: it performs the DDP handshake, answers and
//! sends heartbeats, reconnects with exponential backoff and jitter, runs an
//! optional `on_connected` hook after every handshake (e.g. to log in with a
//! resume token), then renews subscriptions and sends queued method calls.
//!
//! Data messages are delivered to every [`Subscription`] whose name equals the
//! message `collection` and whose [`Routing`] matches (by default, for
//! Rocket.Chat-style streams, a non-empty first string param must equal
//! `fields.eventName`), and to the client-wide [`DdpClient::data_messages`]
//! broadcast. Identical subscriptions share one server-side subscription.
//! Subscription queues are bounded; overflow is reported as [`RecvError::Lagged`].
//!
//! ```no_run
//! use meteor_ddp::{DdpClient, DdpConfig, HookError};
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let url = url::Url::parse("wss://chat.example.com/websocket")?;
//! let config = DdpConfig::new(url).on_connected(|ddp| async move {
//!     ddp.call("login", vec![json!({"resume": "<token>"})]).await?;
//!     Ok::<(), HookError>(())
//! });
//! let client = DdpClient::connect(config).await?;
//!
//! let mut sub = client
//!     .subscribe(
//!         "stream-room-messages",
//!         vec![json!("GENERAL"), json!({"useCollection": false, "args": []})],
//!     )
//!     .await?;
//! let user = client.call("getUserRoles", vec![]).await?;
//! println!("{user}");
//! loop {
//!     match sub.recv().await {
//!         Ok(event) => println!("{:?}", event.args()),
//!         Err(meteor_ddp::RecvError::Lagged(n)) => println!("missed {n} events; resync"),
//!         Err(meteor_ddp::RecvError::Closed) => break,
//!     }
//! }
//! client.close().await;
//! # Ok(())
//! # }
//! ```

mod client;
mod config;
mod driver;
mod error;
mod message;

pub use client::{
    ConnectionState, DdpClient, DdpHandle, Lagged, RecvError, Routing, SubscribeOptions,
    Subscription, SubscriptionState,
};
pub use config::{
    BackoffConfig, DEFAULT_REDACTED_METHODS, DdpConfig, HeartbeatConfig, OnConnected,
};
pub use error::{
    CallError, ConnectError, FATAL_HOOK_ERROR_CODES, HookError, HookErrorKind, SubscribeError,
};
pub use message::{DataKind, DataMessage, MeteorError};

/// Re-exported for [`DdpConfig::connector`] and [`DdpConfig::headers`].
pub use tokio_tungstenite::Connector;
pub use tokio_tungstenite::tungstenite::http;
