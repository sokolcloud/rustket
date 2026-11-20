//! Who plays a message or reminder sound. When a notification is shown by
//! a backend that can play sounds, the system plays it with the
//! notification, so its Do Not Disturb or Focus mode applies: a
//! freedesktop server with the `sound` capability (GNOME) gets the
//! Rocket.Chat sound's file, a bundled macOS app a copy of it in
//! `~/Library/Sounds`, a Windows toast the Windows message sound
//! (unpackaged apps cannot play their own files in toasts). Otherwise
//! Rustket plays the sound itself; on Linux it first asks the desktop
//! whether Do Not Disturb is on (KDE Plasma's `Inhibited`, GNOME's
//! `show-banners`). Windows and macOS offer no safe way to ask, so there
//! a sound without a notification always plays. Call rings do not go
//! through here.

use std::path::{Path, PathBuf};

use super::RcSound;

/// The desktop family the app runs on (passed as a value so the texts can
/// be tested for every platform).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux and the BSDs (freedesktop notifications).
    Linux,
    Windows,
    MacOs,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }

    /// What silences Rustket's sounds besides its own pause (Settings →
    /// Notifications, Help Q1).
    pub fn dnd_text(self) -> String {
        match self {
            Platform::Linux => rust_i18n::t!("settings.sound_dnd_linux"),
            Platform::Windows => rust_i18n::t!("settings.sound_dnd_windows"),
            Platform::MacOs => rust_i18n::t!("settings.sound_dnd_macos"),
        }
        .into_owned()
    }
}

/// The notification shown with the sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toast {
    /// None: notifications off, suppressed, failed or no notification
    /// server.
    None,
    /// Shown; `plays_sound`: its backend can play a sound with it.
    Shown { plays_sound: bool },
}

/// A sound the system plays with its notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsSound {
    /// The Rocket.Chat sound's file: the freedesktop `sound-file` hint; on
    /// macOS the copy in `~/Library/Sounds`, named by its stem.
    File(PathBuf),
    /// The system's message sound: the freedesktop `sound-name`
    /// `message-new-instant`, the toast's `Notification.IM`, macOS's
    /// default notification sound.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Silent,
    /// The notification plays it.
    Os(OsSound),
    /// Rustket plays it; `quiet_check`: unless the desktop's Do Not Disturb
    /// is on.
    Local {
        quiet_check: bool,
    },
}

impl Route {
    /// The notification plays the sound.
    pub fn by_os(&self) -> bool {
        matches!(self, Route::Os(_))
    }
}

/// Who plays `sound` on `platform`. `file`: the Rocket.Chat sound
/// prepared for the notification (the WAV on Linux, its copy in
/// `~/Library/Sounds` on macOS; `None` when preparing failed or was not
/// tried). Windows toasts always play the Windows message sound.
pub fn route(
    platform: Platform,
    toast: Toast,
    sound: Option<&RcSound>,
    file: Option<&Path>,
) -> Route {
    if sound.is_none_or(|s| s.volume == 0) {
        return Route::Silent;
    }
    match toast {
        Toast::Shown { plays_sound: true } => Route::Os(match (platform, file) {
            (Platform::Windows, _) | (_, None) => OsSound::Default,
            (_, Some(f)) => OsSound::File(f.to_owned()),
        }),
        Toast::Shown { plays_sound: false } | Toast::None => Route::Local {
            quiet_check: platform == Platform::Linux,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chime() -> RcSound {
        RcSound {
            name: "chime".into(),
            volume: 80,
        }
    }

    #[test]
    fn the_os_plays_the_sound_of_its_notification() {
        let s = chime();
        let f = Path::new("/c/v1-chime-80.wav");
        let shown = Toast::Shown { plays_sound: true };
        assert_eq!(
            route(Platform::Linux, shown, Some(&s), Some(f)),
            Route::Os(OsSound::File(f.to_owned()))
        );
        assert_eq!(
            route(Platform::Linux, shown, Some(&s), None),
            Route::Os(OsSound::Default)
        );
        assert_eq!(
            route(Platform::MacOs, shown, Some(&s), Some(f)),
            Route::Os(OsSound::File(f.to_owned()))
        );
        assert_eq!(
            route(Platform::MacOs, shown, Some(&s), None),
            Route::Os(OsSound::Default)
        );
        // Unpackaged Windows apps cannot play their own files in toasts.
        assert_eq!(
            route(Platform::Windows, shown, Some(&s), Some(f)),
            Route::Os(OsSound::Default)
        );
        assert!(route(Platform::Windows, shown, Some(&s), None).by_os());
    }

    #[test]
    fn rustket_plays_it_without_a_sounding_notification() {
        let s = chime();
        for toast in [Toast::None, Toast::Shown { plays_sound: false }] {
            assert_eq!(
                route(Platform::Linux, toast, Some(&s), None),
                Route::Local { quiet_check: true }
            );
            for p in [Platform::Windows, Platform::MacOs] {
                assert_eq!(
                    route(p, toast, Some(&s), None),
                    Route::Local { quiet_check: false }
                );
            }
        }
    }

    #[test]
    fn no_sound_is_silent() {
        let shown = Toast::Shown { plays_sound: true };
        assert_eq!(route(Platform::Linux, shown, None, None), Route::Silent);
        let zero = RcSound {
            volume: 0,
            ..chime()
        };
        assert_eq!(
            route(Platform::Windows, Toast::None, Some(&zero), None),
            Route::Silent
        );
        assert!(!Route::Silent.by_os());
    }
}
