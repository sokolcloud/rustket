//! Desktop integration: notifications, tray, idle → away, sounds, autostart.
//! Owner: desktop agent. Strings: `locales/desktop.yml` (`desktop.*`).
//!
//! The app calls the hooks below; everything else is internal. STUB: the
//! hooks do nothing yet.

pub mod autostart;
pub mod idle;
pub mod notifications;
pub mod sound;
pub mod tray;

use iced::{Subscription, Task};
use rocketchat::session::DesktopNotification;
use rustket_core::config::UiPrefs;
use rustket_core::{ServerId, Workspace};

#[derive(Debug, Clone)]
pub enum Message {
    Notifications(notifications::Message),
    Tray(tray::Message),
    Idle(idle::Message),
}

/// What the desktop layer asks the app to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Show the window and open a room (notification or tray click).
    OpenRoom { server_id: ServerId, rid: String },
    /// Show / raise the window.
    ShowWindow,
    /// Quit the app (tray menu).
    Quit,
    /// The user became idle / active again (set away / back).
    Idle(bool),
}

#[derive(Debug, Default)]
pub struct Desktop {
    pub notifications: notifications::Notifications,
    pub tray: tray::Tray,
    pub idle: idle::Idle,
    pub sound: sound::Sound,
    pub autostart: autostart::Autostart,
}

impl Desktop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns follow-up work and requests for the app.
    pub fn update(&mut self, msg: Message) -> (Task<Message>, Vec<Request>) {
        match msg {
            Message::Notifications(m) => self.notifications.update(m),
            Message::Tray(m) => self.tray.update(m),
            Message::Idle(m) => self.idle.update(m),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            self.tray.subscription().map(Message::Tray),
            self.idle.subscription().map(Message::Idle),
        ])
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
        focused: bool,
    ) -> Task<Message> {
        self.notifications.notify(
            server_id,
            server_name,
            notification,
            workspace,
            prefs,
            focused,
        )
    }

    /// The total unread count (rooms) or mention count over all servers changed.
    pub fn on_unread_changed(&mut self, unread: u64, mentions: u64) -> Task<Message> {
        self.tray.set_unread(unread, mentions);
        Task::none()
    }
}
