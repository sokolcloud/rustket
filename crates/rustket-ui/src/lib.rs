//! Rustket design system: theme, icons and reusable iced widgets.
//!
//! Startup wiring:
//!
//! ```ignore
//! iced::application(App::new, App::update, App::view)
//!     .settings(rustket_ui::settings()) // fonts (lucide), default font, 14px text
//!     .theme(|app: &App| rustket_ui::theme(app.mode))
//!     .run()
//! ```
//!
//! This crate knows nothing about Rocket.Chat protocol types: widgets take strings,
//! numbers, image handles and messages.

pub mod emoji;
pub mod fonts;
pub mod icon;
pub mod palette;
pub mod style;
pub mod theme;
pub mod theme_file;
pub mod widget;

pub use icon::{Icon, icon};
pub use theme::{Mode, Tokens, fonts, settings, theme, tokens};
pub use theme_file::{ThemeFile, ThemeFileError, ThemeWarning};
