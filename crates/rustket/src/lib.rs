//! Rustket application crate: the iced shell around `rocketchat` and
//! `rustket-core`.
//!
//! The binary (`main.rs`) only sets up logging, the single-instance socket and
//! runs [`app::App`]. Everything else lives here so examples and tests can drive
//! the real `App::update` paths headlessly.

rust_i18n::i18n!("locales", fallback = "en");

/// The window's application id (X11 `WM_CLASS`, Wayland `app_id`); the packaged
/// desktop file, icon and AppStream id use the same name (`packaging/linux`).
pub const APP_ID: &str = "rustket";
pub const BUNDLE_ID: &str = "io.github.sokolcloud.rustket";

pub mod app;
pub mod cache;
pub mod cli;
pub mod component;
pub mod config_writer;
pub mod deeplink;
pub mod desktop;
pub mod dialogs;
pub mod e2e;
pub mod error;
pub mod focus;
pub mod format;
pub mod i18n;
pub mod images;
pub mod message;
pub mod omnichannel;
pub mod renderer;
pub mod room;
pub mod screens;
pub mod session;
pub mod single_instance;
pub mod teams;
#[cfg(test)]
mod tree_probe;
pub mod ui_state;
pub mod uikit;
pub mod url_handler;
pub mod videoconf;
pub mod workspace;

pub use app::App;
pub use message::Message;
