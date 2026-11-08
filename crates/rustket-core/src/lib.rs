//! UI-agnostic client state for Rustket: configuration, secrets, the per-server
//! [`Workspace`](workspace::Workspace) store and its reducers, derived views (room
//! list, timelines, typing labels), permission checks, emoji tables, notification
//! filtering and REST-based sync.
//!
//! Everything except [`sync`] and [`secrets::open_default`] is synchronous, pure
//! state. Realtime events are applied through fine-grained `Workspace::apply_*`
//! methods that take `rocketchat` model types.

pub mod admin;
pub mod cache;
pub mod config;
pub mod e2e;
pub mod emoji;
pub mod notify;
pub mod omnichannel;
pub mod permissions;
pub mod presence;
pub mod room_list;
pub mod secrets;
pub mod settings;
pub mod sync;
pub mod teams;
pub mod timeline;
pub mod typing;
pub mod uikit;
pub mod videoconf;
pub mod workspace;

pub use config::{AppConfig, ServerId};
pub use workspace::{Change, Dirty, Workspace};

#[cfg(test)]
pub(crate) mod test_util;
