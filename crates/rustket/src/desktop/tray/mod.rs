//! System tray: the app icon (with a red dot while something is unread), a
//! tooltip with the counts and a menu: open, status, quit. Clicking the icon
//! toggles the window.
//!
//! Backends: a StatusNotifierItem (ksni) on Linux and the BSDs, `tray-icon`
//! on Windows and macOS. The app's state is kept in a `watch` channel; the
//! icon's events come back as [`Message`]s from the tray subscription.

#[cfg(any(windows, target_os = "macos"))]
mod native;
#[cfg(all(unix, not(target_os = "macos")))]
mod sni;

#[cfg(all(unix, not(target_os = "macos")))]
use std::sync::Arc;

use iced::{Subscription, Task};
use rocketchat::models::UserStatus;
use rust_i18n::t;
use tokio::sync::watch;

use super::Request;

#[derive(Debug, Clone)]
pub enum Message {
    /// The platform event loop runs: create the icon (`tray-icon` backend).
    Init,
    /// The tray service started (`true`) or no tray host is available.
    Ready(bool),
    /// The icon was clicked.
    Activate,
    Show,
    SetStatus(UserStatus),
    Quit,
}

/// What the tray shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    pub unread: u64,
    pub mentions: u64,
    /// The user's status on the active server (`None`: not logged in).
    pub status: Option<UserStatus>,
    /// Statuses the active server allows, in menu order.
    pub offered: Vec<UserStatus>,
    /// Bumped to rebuild labels (language change).
    pub revision: u64,
}

pub fn status_label(s: UserStatus) -> String {
    match s {
        UserStatus::Online => t!("desktop.status_online"),
        UserStatus::Away => t!("desktop.status_away"),
        UserStatus::Busy => t!("desktop.status_busy"),
        _ => t!("desktop.status_invisible"),
    }
    .into_owned()
}

/// Tooltip text for the counts.
pub fn tooltip(state: &TrayState) -> String {
    match (state.unread, state.mentions) {
        (0, 0) => t!("desktop.tray_no_unread").into_owned(),
        (u, 0) => t!("desktop.tray_unread", count = u).into_owned(),
        (u, m) => t!("desktop.tray_unread_mentions", count = u, mentions = m).into_owned(),
    }
}

/// The subscription's identity: one tray per app.
#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone)]
struct Slot(Arc<watch::Receiver<TrayState>>);

#[cfg(all(unix, not(target_os = "macos")))]
impl std::hash::Hash for Slot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "rustket-tray".hash(state);
    }
}

#[derive(Debug)]
pub struct Tray {
    enabled: bool,
    state: watch::Sender<TrayState>,
    #[cfg(all(unix, not(target_os = "macos")))]
    rx: Arc<watch::Receiver<TrayState>>,
    /// Removes the `tray-icon` icon when the app state is dropped.
    #[cfg(any(windows, target_os = "macos"))]
    _icon: native::Guard,
    /// A tray host shows our icon.
    pub available: bool,
}

impl Default for Tray {
    fn default() -> Self {
        let (state, rx) = watch::channel(TrayState::default());
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        drop(rx);
        Self {
            enabled: false,
            state,
            #[cfg(all(unix, not(target_os = "macos")))]
            rx: Arc::new(rx),
            #[cfg(any(windows, target_os = "macos"))]
            _icon: native::Guard,
            available: false,
        }
    }
}

impl Tray {
    pub fn system() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    pub fn state(&self) -> TrayState {
        self.state.borrow().clone()
    }

    pub fn set_unread(&mut self, unread: u64, mentions: u64) {
        self.state.send_if_modified(|s| {
            let changed = (s.unread, s.mentions) != (unread, mentions);
            s.unread = unread;
            s.mentions = mentions;
            changed
        });
        self.apply();
    }

    /// The status on the active server and the statuses it offers.
    pub fn set_status(&mut self, status: Option<(UserStatus, Vec<UserStatus>)>) {
        let (status, offered) = match status {
            Some((s, o)) => (Some(s), o),
            None => (None, Vec::new()),
        };
        self.state.send_if_modified(|s| {
            let changed = s.status != status || s.offered != offered;
            s.status = status;
            s.offered = offered;
            changed
        });
        self.apply();
    }

    /// Rebuilds the labels (after a language change).
    pub fn refresh(&mut self) {
        self.state.send_modify(|s| s.revision += 1);
        self.apply();
    }

    /// Shows the state on the `tray-icon` icon (the ksni service watches it).
    fn apply(&self) {
        #[cfg(any(windows, target_os = "macos"))]
        if self.enabled {
            native::apply(&self.state.borrow());
        }
    }

    /// Handles everything except `Activate`, which needs the window state.
    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
            Message::Init => {
                #[cfg(any(windows, target_os = "macos"))]
                if self.enabled {
                    let ok = native::create(&self.state.borrow());
                    return (
                        Task::done(super::Message::Tray(Message::Ready(ok))),
                        Vec::new(),
                    );
                }
                (Task::none(), Vec::new())
            }
            Message::Ready(ok) => {
                self.available = ok;
                (Task::none(), Vec::new())
            }
            Message::Activate | Message::Show => (Task::none(), vec![Request::ShowWindow]),
            Message::SetStatus(s) => (Task::none(), vec![Request::SetStatus(s)]),
            Message::Quit => (Task::none(), vec![Request::Quit]),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if !self.enabled {
            return Subscription::none();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        return Subscription::run_with(Slot(self.rx.clone()), sni::run);
        #[cfg(any(windows, target_os = "macos"))]
        return Subscription::run(native::events);
        #[cfg(not(any(unix, windows)))]
        return Subscription::none();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn state_updates_and_requests() {
        let mut tray = Tray::default();
        assert!(!tray.enabled);
        tray.set_unread(3, 1);
        tray.set_status(Some((
            UserStatus::Busy,
            vec![UserStatus::Online, UserStatus::Busy],
        )));
        let s = tray.state();
        assert_eq!(
            (s.unread, s.mentions, s.status),
            (3, 1, Some(UserStatus::Busy))
        );
        let (_, r) = tray.update(Message::SetStatus(UserStatus::Away));
        assert!(matches!(
            r.as_slice(),
            [Request::SetStatus(UserStatus::Away)]
        ));
        let (_, r) = tray.update(Message::Quit);
        assert!(matches!(r.as_slice(), [Request::Quit]));
        let _ = tray.update(Message::Ready(true));
        assert!(tray.available);
    }

    #[test]
    fn tooltip_texts() {
        rust_i18n::set_locale("en");
        let mut s = TrayState::default();
        assert_eq!(tooltip(&s), "No unread messages");
        s.unread = 2;
        assert_eq!(tooltip(&s), "Unread rooms: 2");
        s.mentions = 1;
        assert_eq!(tooltip(&s), "Unread rooms: 2, mentions: 1");
    }
}
