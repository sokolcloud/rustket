//! Rustket application crate: the iced shell around `rocketchat` and
//! `rustket-core`.
//!
//! The binary (`main.rs`) only sets up logging, the single-instance socket and
//! runs [`app::App`]. Everything else lives here so examples and tests can drive
//! the real `App::update` paths headlessly.

// App Store guideline 2.4.5(vii): the store build must not contain an updater.
#[cfg(all(feature = "mas", feature = "self-update"))]
compile_error!(
    "the `mas` (Mac App Store) and `self-update` features exclude each other: \
     store builds update only through the App Store (docs/app-store.md)"
);

// External locale files override the bundled strings (`i18n::Overrides`).
rust_i18n::i18n!("locales", fallback = "en", backend = crate::i18n::Overrides);

/// The window's application id (X11 `WM_CLASS`, Wayland `app_id`); the packaged
/// desktop file, icon and AppStream id use the same name (`packaging/linux`).
pub const APP_ID: &str = "rustket";

/// The macOS `CFBundleIdentifier` and the Windows AppUserModelID; the app
/// bundle (`Info.plist`) and the Windows installer use the same id.
pub const BUNDLE_ID: &str = "io.github.sokolcloud.rustket";

pub mod app;
pub mod cache;
pub mod cli;
pub mod component;
pub mod config_writer;
pub mod deeplink;
pub mod desktop;
pub mod dialogs;
pub mod drafts;
pub mod e2e;
pub mod error;
pub mod focus;
pub mod format;
pub mod help;
pub mod i18n;
pub mod images;
pub mod import;
pub mod inbox;
pub mod keymap;
pub mod later;
pub mod legal;
pub mod locales;
pub mod logging;
#[cfg(target_os = "macos")]
pub mod macos_events;
pub mod message;
pub mod omnichannel;
pub mod os_events;
pub mod policy;
pub mod quiet;
pub mod reads;
pub mod renderer;
pub mod room;
pub mod screens;
pub mod search;
pub mod session;
pub mod single_instance;
pub mod teams;
pub mod themes;
#[cfg(test)]
mod tree_probe;
pub mod ui_state;
pub mod uikit;
#[cfg(feature = "self-update")]
pub mod updater;
pub mod url_handler;
pub mod videoconf;
pub mod workspace;

pub use app::App;
pub use message::Message;
