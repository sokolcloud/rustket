//! Desktop integration: notifications, tray, window behaviour (close /
//! minimize to tray, start minimized), idle → away, sounds, autostart,
//! keyboard shortcuts. Owner: desktop agent. Strings: `locales/desktop.yml`
//! (`desktop.*`).
//!
//! The app calls the hooks below; everything else is internal.
//! [`Desktop::new`] is inert (no tray, no notifications, no sounds, no idle
//! polling): tests and snapshots use it. The binary uses [`Desktop::system`].

pub mod autostart;
mod dbus;
pub mod icon;
pub mod idle;
pub mod notifications;
pub mod shortcuts;
pub mod sound;
pub mod tray;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iced::{Subscription, Task};
use rocketchat::models::UserStatus;
use rocketchat::session::DesktopNotification;
use rustket_core::config::UiPrefs;
use rustket_core::workspace::ConnectionState;
use rustket_core::{ServerId, Workspace};

pub use notifications::NotifyEnv;

#[derive(Debug, Clone)]
pub enum Message {
    Notifications(notifications::Message),
    Tray(tray::Message),
    Idle(idle::Message),
    /// The window was hidden to the tray (`true`) or not.
    WindowHidden(bool),
    /// Alt(+Shift)+Up/Down.
    RoomStep(shortcuts::RoomStep),
}

/// What the desktop layer asks the app to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Open a room (notification click); the window is raised separately.
    OpenRoom { server_id: ServerId, rid: String },
    /// Open a thread of a room (notification of a thread reply).
    OpenThread {
        server_id: ServerId,
        rid: String,
        tmid: String,
    },
    /// Show / raise the window.
    ShowWindow,
    /// Quit the app (tray menu).
    Quit,
    /// Session idle time, sampled every [`idle::POLL_INTERVAL`]: the app runs
    /// [`Desktop::idle_calls`] and sends the presence calls.
    IdleTime(Duration),
    /// Set this status on every logged-in server (tray menu).
    SetStatus(UserStatus),
    /// Open the previous/next (unread) room in sidebar order.
    RoomStep(shortcuts::RoomStep),
}

/// A value of `me.settings.preferences`.
pub(crate) fn user_pref<'a>(ws: &'a Workspace, key: &str) -> Option<&'a serde_json::Value> {
    ws.me()
        .and_then(|m| m.settings.as_ref())
        .and_then(|s| s.preferences.as_ref())
        .and_then(|p| p.get(key))
}

/// Whether winit will use Wayland (it prefers Wayland whenever a Wayland
/// display is set), where windows can be neither hidden nor queried for
/// being minimized.
pub fn wayland_session() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// What the window can do with the tray (settings screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraySupport {
    /// A tray host shows our icon.
    pub available: bool,
    /// The window can be hidden (not on Wayland: it is minimized instead).
    pub can_hide: bool,
}

#[derive(Debug, Default)]
pub struct Desktop {
    pub notifications: notifications::Notifications,
    pub tray: tray::Tray,
    pub idle: idle::Idle,
    pub sound: sound::Sound,
    /// Real desktop integration (not in tests).
    enabled: bool,
    /// The window is hidden to the tray.
    pub hidden: bool,
    pub focused: bool,
    /// Start hidden/minimized once the tray answered.
    start_minimized: bool,
    locale: String,
    /// Wayland session: "hiding" minimizes.
    pub wayland: bool,
    /// Last seen connection state per server (`true`: connected), to notice
    /// reconnects.
    connected: HashMap<ServerId, bool>,
}

impl Desktop {
    /// Inert desktop layer (tests, snapshots).
    pub fn new() -> Self {
        Self {
            focused: true,
            ..Self::default()
        }
    }

    /// The real desktop: notification server, tray, idle polling, sounds.
    pub fn system(prefs: &UiPrefs) -> Self {
        Self {
            notifications: notifications::Notifications::with_backend(Arc::new(
                notifications::ZbusNotifier,
            )),
            tray: tray::Tray::system(),
            idle: idle::Idle::system(),
            sound: sound::Sound::system(),
            enabled: true,
            hidden: false,
            focused: true,
            start_minimized: prefs.start_minimized,
            locale: rust_i18n::locale().to_string(),
            wayland: wayland_session(),
            connected: HashMap::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn tray_support(&self) -> TraySupport {
        TraySupport {
            available: self.tray.available,
            can_hide: !self.wayland,
        }
    }

    /// Returns follow-up work and requests for the app.
    pub fn update(&mut self, msg: Message) -> (Task<Message>, Vec<Request>) {
        match msg {
            Message::Notifications(m) => {
                let (task, requests, play) = self.notifications.update(m);
                if play {
                    self.sound.play(None);
                }
                (task, requests)
            }
            Message::Tray(tray::Message::Activate) => {
                if self.tray.available && !self.hidden && self.focused {
                    (self.hide_window(), Vec::new())
                } else {
                    (Task::none(), vec![Request::ShowWindow])
                }
            }
            Message::Tray(tray::Message::Ready(ok)) => {
                let (task, requests) = self.tray.update(tray::Message::Ready(ok));
                if !ok && self.hidden {
                    // The tray host went away: nothing could bring it back.
                    return (Task::batch([task, self.show_window()]), requests);
                }
                if !std::mem::take(&mut self.start_minimized) {
                    return (task, requests);
                }
                let start = if ok {
                    self.hide_window()
                } else {
                    window::minimize()
                };
                (Task::batch([task, start]), requests)
            }
            Message::Tray(m) => self.tray.update(m),
            Message::Idle(m) => self.idle.update(m),
            Message::WindowHidden(hidden) => {
                if hidden {
                    self.hidden = true;
                }
                (Task::none(), Vec::new())
            }
            Message::RoomStep(step) => (Task::none(), vec![Request::RoomStep(step)]),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Alt+arrows also while a widget (the composer) captured the key.
        let shortcuts = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(ev) => shortcuts::from_event(&ev).map(Message::RoomStep),
            _ => None,
        });
        Subscription::batch([
            shortcuts,
            self.tray.subscription().map(Message::Tray),
            self.idle.subscription().map(Message::Idle),
            self.notifications
                .subscription()
                .map(Message::Notifications),
        ])
    }

    /// Shows and raises the window.
    pub fn show_window(&mut self) -> Task<Message> {
        self.hidden = false;
        if !self.enabled {
            return Task::none();
        }
        window::show()
    }

    /// Hides the window to the tray; on Wayland, where a window cannot be
    /// hidden, it is minimized instead.
    fn hide_window(&mut self) -> Task<Message> {
        if self.wayland {
            return window::minimize();
        }
        self.hidden = true;
        window::hide()
    }

    /// The window asked to close. `Some(task)`: it was hidden to the tray
    /// instead; `None`: quit (the app flushes the config and closes).
    pub fn on_close_requested(&mut self, prefs: &UiPrefs) -> Option<Task<Message>> {
        (self.enabled && prefs.close_to_tray && self.tray.available).then(|| self.hide_window())
    }

    /// The window gained or lost focus: a minimized window goes to the tray
    /// when "minimize to tray" is on.
    pub fn on_focus_changed(&mut self, focused: bool, prefs: &UiPrefs) -> Task<Message> {
        self.focused = focused;
        if focused {
            self.hidden = false;
            return Task::none();
        }
        if self.enabled && prefs.minimize_to_tray && self.tray.available && !self.wayland {
            return window::hide_if_minimized().map(Message::WindowHidden);
        }
        Task::none()
    }

    /// A `{uid}/notification` event arrived for `server_id` (after it was
    /// applied to `workspace`).
    pub fn on_notification(
        &mut self,
        server_id: &ServerId,
        server_name: &str,
        notification: &DesktopNotification,
        workspace: &Workspace,
        prefs: &UiPrefs,
        env: &NotifyEnv<'_>,
    ) -> Task<Message> {
        let env = NotifyEnv {
            window_active: env.window_active && self.focused && !self.hidden,
            ..*env
        };
        self.notifications
            .notify(server_id, server_name, notification, workspace, prefs, &env)
    }

    /// The total unread count (rooms) or mention count over all servers changed.
    pub fn on_unread_changed(&mut self, unread: u64, mentions: u64) -> Task<Message> {
        self.tray.set_unread(unread, mentions);
        Task::none()
    }

    /// Called after every update with the logged-in servers: closes the
    /// notifications of rooms that were read (or of servers that are gone)
    /// and keeps the tray status in sync with the active server.
    pub fn on_state<'a>(
        &mut self,
        servers: &[(&'a ServerId, &'a Workspace)],
        active: Option<&ServerId>,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        // A reconnect resets the connection status to online: auto-away is
        // sent again. Servers that are gone are forgotten.
        self.connected
            .retain(|sid, _| servers.iter().any(|(s, _)| *s == sid));
        for (sid, ws) in servers {
            let now = *ws.connection() == ConnectionState::Connected;
            let before = self.connected.insert((*sid).clone(), now);
            if now && before == Some(false) {
                self.idle.reconnected(sid);
            }
        }
        self.idle
            .retain(|sid| servers.iter().any(|(s, _)| *s == sid));
        if !self.notifications.is_empty() {
            for (sid, ws) in servers {
                tasks.push(self.notifications.sync_read(sid, ws));
            }
            let alive: Vec<&ServerId> = servers.iter().map(|(s, _)| *s).collect();
            tasks.push(self.notifications.retain_servers(&alive));
        }
        let active_ws = servers
            .iter()
            .find(|(s, _)| Some(*s) == active)
            .filter(|(_, ws)| ws.user_id().is_some())
            .map(|(_, ws)| *ws);
        self.tray.set_status(
            active_ws.map(|ws| (ws.my_status(), crate::workspace::status_menu::offered(ws))),
        );
        if self.enabled {
            let locale = rust_i18n::locale();
            if *locale != *self.locale {
                self.locale = locale.to_string();
                self.tray.refresh();
            }
        }
        Task::batch(tasks)
    }

    /// Presence calls for an idle-time sample: `(server, call)` for every
    /// connected server in `servers`.
    pub fn idle_calls<'a>(
        &mut self,
        idle: Duration,
        servers: impl IntoIterator<Item = (&'a ServerId, &'a Workspace)>,
    ) -> Vec<(ServerId, idle::PresenceCall)> {
        servers
            .into_iter()
            .filter_map(|(sid, ws)| {
                let call = self
                    .idle
                    .decide(sid, idle, idle::AwayPrefs::of(ws), ws.my_status())?;
                Some((sid.clone(), call))
            })
            .collect()
    }

    /// "Send test notification" (settings).
    pub fn test_notification(&mut self, prefs: &UiPrefs) -> Task<Message> {
        self.notifications.test(prefs)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn inert_desktop_never_hides_the_window() {
        let mut d = Desktop::new();
        let prefs = UiPrefs {
            close_to_tray: true,
            minimize_to_tray: true,
            ..UiPrefs::default()
        };
        assert!(d.on_close_requested(&prefs).is_none(), "quits");
        d.tray.available = true;
        assert!(d.on_close_requested(&prefs).is_none(), "still inert");
        let _ = d.on_focus_changed(false, &prefs);
        assert!(!d.focused && !d.hidden);
    }

    #[test]
    fn close_to_tray_and_tray_toggle() {
        let mut d = Desktop::new();
        d.enabled = true;
        let mut prefs = UiPrefs::default();
        // No tray host: closing quits.
        assert!(d.on_close_requested(&prefs).is_none());
        let _ = d.update(Message::Tray(tray::Message::Ready(true)));
        assert!(d.on_close_requested(&prefs).is_some());
        assert!(d.hidden);
        // Clicking the tray icon shows it again.
        let (_, r) = d.update(Message::Tray(tray::Message::Activate));
        assert!(matches!(r.as_slice(), [Request::ShowWindow]));
        let _ = d.show_window();
        let _ = d.on_focus_changed(true, &prefs);
        assert!(!d.hidden);
        // Visible and focused: the click hides it.
        let (_, r) = d.update(Message::Tray(tray::Message::Activate));
        assert!(r.is_empty() && d.hidden);
        prefs.close_to_tray = false;
        let _ = d.show_window();
        assert!(d.on_close_requested(&prefs).is_none());
    }

    #[test]
    fn start_minimized_hides_once_the_tray_is_up() {
        let mut d = Desktop::new();
        d.enabled = true;
        d.start_minimized = true;
        let _ = d.update(Message::Tray(tray::Message::Ready(true)));
        assert!(d.hidden);
        let _ = d.show_window();
        let _ = d.update(Message::Tray(tray::Message::Ready(true)));
        assert!(!d.hidden, "only at startup");
    }

    #[test]
    fn tray_status_follows_the_active_server() {
        let mut d = Desktop::new();
        let mut ws = Workspace::new();
        let sid = ServerId("a".into());
        let _ = d.on_state(&[(&sid, &ws)], Some(&sid));
        assert_eq!(d.tray.state().status, None, "not logged in");
        ws.apply_me(
            serde_json::from_value(
                serde_json::json!({"_id": "me", "username": "me", "status": "busy"}),
            )
            .unwrap(),
        );
        let _ = d.on_state(&[(&sid, &ws)], Some(&sid));
        assert_eq!(d.tray.state().status, Some(UserStatus::Busy));
        let _ = d.on_unread_changed(4, 2);
        assert_eq!((d.tray.state().unread, d.tray.state().mentions), (4, 2));
    }
}
