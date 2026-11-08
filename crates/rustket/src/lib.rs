//! Rustket application crate: the iced shell around `rocketchat` and
//! `rustket-core`.
//!
//! The binary (`main.rs`) only sets up logging, the single-instance socket and
//! runs [`app::App`]. Everything else lives here so examples and tests can drive
//! the real `App::update` paths headlessly.

rust_i18n::i18n!("locales", fallback = "en");

pub mod app;
pub mod component;
pub mod config_writer;
pub mod deeplink;
pub mod desktop;
pub mod dialogs;
pub mod error;
pub mod format;
pub mod i18n;
pub mod images;
pub mod message;
pub mod renderer;
pub mod room;
pub mod screens;
pub mod session;
pub mod single_instance;
#[cfg(test)]
mod tree_probe;
pub mod ui_state;
pub mod url_handler;
pub mod workspace;

pub use app::App;
pub use message::Message;
