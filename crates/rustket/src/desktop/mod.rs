//! Desktop integration: notifications, tray, Dock badge (macOS), window
//! behaviour (close / minimize to tray, start minimized), idle → away, sounds,
//! autostart, keyboard shortcuts. Owner: desktop agent. Strings: `locales/desktop.yml`
//! (`desktop.*`).
//!
//! The app calls the hooks below; everything else is internal.
//! [`Desktop::new`] is inert (no tray, no notifications, no sounds, no idle
//! polling): tests and snapshots use it. The binary uses [`Desktop::system`].

pub mod autostart;
pub mod badge_art;
#[cfg(all(unix, not(target_os = "macos")))]
mod dbus;
pub mod dock;
pub mod icon;
pub mod idle;
pub mod menu_bar;
pub mod notifications;
pub mod open;
pub mod sandbox;
pub mod shortcuts;
pub mod sound;
pub mod tray;
pub mod window;

use std::collections::HashMap;
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
    /// The macOS main menu.
    MenuBar(menu_bar::Message),
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
    /// Show the window on this page (macOS main menu).
    Navigate(crate::component::Nav),
    /// Open an http(s) URL in the browser (macOS Help menu).
    OpenUrl(&'static str),
    /// Quit the app (tray menu, macOS main menu).
    Quit,
    /// Session idle time, sampled every [`idle::POLL_INTERVAL`]: the app runs
    /// [`Desktop::idle_calls`] and sends the presence calls.
    IdleTime(Duration),
    /// Set this status on every logged-in server (tray menu).
    SetStatus(UserStatus),
    /// Pause notifications (tray menu).
    Pause(rustket_core::quiet::PausePreset),
    /// Resume paused notifications (tray menu).
    Resume,
    /// Open the previous/next (unread) room in sidebar order.
    RoomStep(shortcuts::RoomStep),
    /// A notification server signal for a notification shown by another
    /// component (call notifications, `crate::videoconf`).
    ForeignSignal(u32, notifications::Signal),
    /// "Join" on a server conference notification (`crate::videoconf`).
    JoinCall {
        server_id: ServerId,
        call_id: String,
    },
}

/// A value of `me.settings.preferences`.
pub(crate) fn user_pref<'a>(ws: &'a Workspace, key: &str) -> Option<&'a serde_json::Value> {
    ws.me()
        .and_then(|m| m.settings.as_ref())
        .and_then(|s| s.preferences.as_ref())
        .and_then(|p| p.get(key))
}

/// The account's sound preferences (`me.settings.preferences`; `None`: not
/// set).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountSound {
    /// `newMessageNotification` (a sound name, `none` = off).
    pub new_message: Option<String>,
    /// `newRoomNotification` (a sound name, `none` = off).
    pub new_room: Option<String>,
    pub notifications_volume: Option<f64>,
    pub master_volume: Option<f64>,
}

impl AccountSound {
    pub fn of(ws: &Workspace) -> Self {
        let text = |k: &str| user_pref(ws, k).and_then(|v| v.as_str()).map(str::to_owned);
        let num = |k: &str| user_pref(ws, k).and_then(|v| v.as_f64());
        Self {
            new_message: text("newMessageNotification"),
            new_room: text("newRoomNotification"),
            notifications_volume: num("notificationsSoundVolume"),
            master_volume: num("masterVolume"),
        }
    }

    /// The volume of notification sounds in percent, as Rocket.Chat web
    /// plays them: `notificationsSoundVolume` × `masterVolume` (each 0..=100,
    /// 100 when not set).
    pub fn volume(&self) -> u8 {
        let pct = |v: Option<f64>| {
            v.filter(|v| v.is_finite())
                .unwrap_or(100.0)
                .clamp(0.0, 100.0)
        };
        (pct(self.notifications_volume) * pct(self.master_volume) / 100.0).round() as u8
    }

    /// The notification or master volume is zero: no sound.
    pub fn volume_muted(&self) -> bool {
        self.volume() == 0
    }

    /// The new-message sound is "None".
    pub fn new_message_off(&self) -> bool {
        self.new_message.as_deref() == Some("none")
    }

    /// The account's new-message sound (`chime` when not set); `None` when
    /// it is "None".
    pub fn message_sound(&self) -> Option<&str> {
        match sound_choice(self.new_message.as_deref()) {
            Some("none") => None,
            Some(name) => Some(name),
            None => Some(DEFAULT_MESSAGE_SOUND),
        }
    }

    /// The account's new-room sound (`door` when not set); `None` when it is
    /// "None".
    pub fn room_sound(&self) -> Option<&str> {
        match sound_choice(self.new_room.as_deref()) {
            Some("none") => None,
            Some(name) => Some(name),
            None => Some(DEFAULT_ROOM_SOUND),
        }
    }
}

/// Rocket.Chat's default `newMessageNotification`.
pub const DEFAULT_MESSAGE_SOUND: &str = "chime";
/// Rocket.Chat's default `newRoomNotification`.
pub const DEFAULT_ROOM_SOUND: &str = "door";

/// A sound preference's own choice: `None` for unset, empty and "default".
pub fn sound_choice(v: Option<&str>) -> Option<&str> {
    v.filter(|v| !v.is_empty() && *v != "default")
}

/// Whether winit will use Wayland (it prefers Wayland whenever a Wayland
/// display is set), where windows can be neither hidden nor queried for
/// being minimized. Never on Windows and macOS.
pub fn wayland_session() -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        return false;
    }
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
    /// The Dock badge last set (macOS).
    dock_badge: Option<String>,
    /// The art of the window icon last set (Linux, Windows; the Rocket.Chat
    /// scheme badges it).
    window_art: tray::TrayArt,
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
            notifications: notifications::Notifications::with_backend(
                notifications::system_backend(),
            ),
            tray: tray::Tray::system(),
            idle: idle::Idle::system(),
            sound: sound::Sound::system(),
            enabled: true,
            hidden: false,
            focused: true,
            start_minimized: prefs.start_minimized,
            locale: crate::i18n::cache_tag(),
            wayland: wayland_session(),
            connected: HashMap::new(),
            dock_badge: None,
            window_art: tray::TrayArt::Plain,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The window icon can carry the Rocket.Chat badge: the conditions of
    /// `set_window_art` (not on Wayland, which ignores window icons, nor on
    /// macOS, which uses the Dock).
    pub fn window_icon_badges(&self) -> bool {
        self.enabled && !self.wayland && !cfg!(target_os = "macos")
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
                let (task, requests, sound) = self.notifications.update(m);
                if let Some(sound) = sound {
                    self.sound.play_alone(&sound);
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
            Message::MenuBar(menu_bar::Message::Init) => {
                #[cfg(target_os = "macos")]
                if self.enabled {
                    menu_bar::install();
                }
                (Task::none(), Vec::new())
            }
            Message::MenuBar(menu_bar::Message::Item(item)) => match menu_bar::request(item) {
                Some(r) => (Task::none(), vec![r]),
                None => (self.hide_window(), Vec::new()),
            },
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Alt+arrows also while a widget (the composer) captured the key.
        let shortcuts = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(ev) => shortcuts::from_event(&ev).map(Message::RoomStep),
            _ => None,
        });
        let menu_bar = if self.enabled {
            menu_bar::subscription().map(Message::MenuBar)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            shortcuts,
            menu_bar,
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
        let (task, sound) =
            self.notifications
                .notify(server_id, server_name, notification, workspace, prefs, &env);
        if let Some(sound) = sound {
            self.sound.play_alone(&sound);
        }
        task
    }

    /// The unread signal over all servers changed.
    pub fn on_unread_changed(
        &mut self,
        signal: rustket_core::room_list::WorkspaceSignal,
    ) -> Task<Message> {
        self.tray.set_unread(&signal);
        let badge = dock::badge_label(&signal);
        if self.enabled && badge != self.dock_badge {
            #[cfg(target_os = "macos")]
            dock::apply(badge.as_deref());
            self.dock_badge = badge;
        }
        self.set_window_art(tray::TrayArt::Plain)
    }

    /// The unread signals over all servers changed under the Rocket.Chat
    /// scheme: the tray, Dock and window icon show the desktop app's total
    /// unread (`rc`), or its bullet.
    pub fn on_unread_changed_rc(
        &mut self,
        signal: rustket_core::room_list::WorkspaceSignal,
        rc: rustket_core::indicators::RcSignal,
    ) -> Task<Message> {
        self.tray.set_unread_rc(&signal, rc);
        let badge = Some(dock::badge_label_rc(rc)).filter(|b| !b.is_empty());
        if self.enabled && badge != self.dock_badge {
            #[cfg(target_os = "macos")]
            dock::apply(badge.as_deref());
            self.dock_badge = badge;
        }
        self.set_window_art(tray::TrayArt::of(
            &signal,
            &rc,
            rustket_core::indicators::IndicatorScheme::RocketChat,
        ))
    }

    /// The art of the window icon now (see [`Self::on_unread_changed_rc`]).
    pub fn window_art(&self) -> tray::TrayArt {
        self.window_art
    }

    /// Badges the window icon with `art` when it changed (the plain icon
    /// again for [`tray::TrayArt::Plain`]). macOS keeps the Dock instead.
    fn set_window_art(&mut self, art: tray::TrayArt) -> Task<Message> {
        if art == self.window_art || cfg!(target_os = "macos") {
            return Task::none();
        }
        self.window_art = art;
        // winit ignores window icons on Wayland.
        if !self.enabled || self.wayland {
            return Task::none();
        }
        let icon = match art.glyph() {
            Some(_) => icon::window_icon_rc(art),
            None => icon::window_icon(),
        };
        icon.map_or_else(Task::none, window::set_icon)
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
            let locale = crate::i18n::cache_tag();
            if locale != self.locale {
                self.locale = locale;
                self.tray.refresh();
                #[cfg(target_os = "macos")]
                menu_bar::refresh();
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

    /// "Send test notification" (settings), with the message sound of
    /// `account` (the active account's preferences).
    pub fn test_notification(&mut self, prefs: &UiPrefs, account: &AccountSound) -> Task<Message> {
        let sound = notifications::message_sound(prefs.sound, None, account).ok();
        if !prefs.desktop_notifications {
            if let Some(sound) = sound {
                self.sound.play_alone(&sound);
            }
            return Task::none();
        }
        self.notifications.test(sound)
    }

    /// Plays Rocket.Chat sound `name` at the volume of `account` (a sound
    /// picked in Account → Preferences).
    pub fn preview_sound(&self, name: &str, account: &AccountSound) {
        let volume = account.volume();
        if volume > 0 && name != "none" {
            self.sound.play_rc(
                &sound::RcSound {
                    name: name.to_owned(),
                    volume,
                },
                false,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn account_sound_volume_and_names() {
        let a = AccountSound::default();
        assert_eq!(a.volume(), 100);
        assert_eq!(a.message_sound(), Some("chime"));
        assert_eq!(a.room_sound(), Some("door"));
        let a = AccountSound {
            new_message: Some("default".into()),
            new_room: Some("none".into()),
            notifications_volume: Some(70.0),
            master_volume: Some(50.0),
        };
        assert_eq!(a.volume(), 35);
        assert!(!a.volume_muted());
        assert_eq!(a.message_sound(), Some("chime"));
        assert_eq!(a.room_sound(), None);
        let loud = AccountSound {
            notifications_volume: Some(250.0),
            master_volume: Some(f64::NAN),
            ..AccountSound::default()
        };
        assert_eq!(loud.volume(), 100);
        let off = AccountSound {
            notifications_volume: Some(-5.0),
            ..AccountSound::default()
        };
        assert!(off.volume_muted());
        assert_eq!(sound_choice(Some("")), None);
        assert_eq!(sound_choice(Some("ding")), Some("ding"));
    }

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
        let _ = d.on_unread_changed(rustket_core::room_list::WorkspaceSignal {
            mentions: 2,
            strongest: Some(rustket_core::room_list::BadgeVariant::Mention),
            rooms: 4,
        });
        assert_eq!((d.tray.state().unread, d.tray.state().mentions), (4, 2));
    }

    /// The Rocket.Chat hook fills the tray with the desktop app's art and
    /// badges the window icon; the Rustket hook restores the plain icon.
    #[test]
    fn rc_unread_hook_and_window_icon() {
        use rustket_core::indicators::RcSignal;
        use rustket_core::room_list::WorkspaceSignal;
        // macOS badges the Dock instead.
        let badged = |a| {
            if cfg!(target_os = "macos") {
                tray::TrayArt::Plain
            } else {
                a
            }
        };
        let mut d = Desktop::new();
        let sig = WorkspaceSignal::default();
        assert_eq!(d.window_art(), tray::TrayArt::Plain);
        let _ = d.on_unread_changed_rc(
            sig,
            RcSignal {
                unread: 4,
                bullet: false,
            },
        );
        assert_eq!(d.tray.state().art, tray::TrayArt::RcCount(4));
        assert_eq!(d.window_art(), badged(tray::TrayArt::RcCount(4)));
        let _ = d.on_unread_changed_rc(
            sig,
            RcSignal {
                unread: 0,
                bullet: true,
            },
        );
        assert_eq!(d.window_art(), badged(tray::TrayArt::RcDot));
        let _ = d.on_unread_changed(sig);
        assert_eq!(d.window_art(), tray::TrayArt::Plain);
        assert_eq!(
            d.tray.state().scheme,
            rustket_core::indicators::IndicatorScheme::Rustket
        );
    }
}
