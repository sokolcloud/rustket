//! Custom title bar and subscriptions.

use std::time::{Duration, Instant};

use iced::{Subscription, Task, window};
use rustket_core::config::Theme as ThemePref;

use super::{App, Modal, Screen};
use crate::images;
use crate::message::Message;
use crate::session;
use crate::single_instance;
use crate::url_handler;

impl App {
    // --- custom title bar ------------------------------------------------------

    /// Whether the app draws its own title bar and window frame.
    pub fn custom_chrome(&self) -> bool {
        !self.decorated
    }

    /// Applies `UiPrefs::system_title_bar` to the window. iced can only flip
    /// decorations, so the current state is tracked and only a difference
    /// issues a toggle.
    pub(super) fn sync_decorations(&mut self) -> Task<Message> {
        if self.config.ui.system_title_bar == self.decorated {
            return Task::none();
        }
        self.decorated = self.config.ui.system_title_bar;
        let toggle = window::latest().and_then(window::toggle_decorations);
        if self.decorated {
            return toggle;
        }
        // Maximize changes made under the system frame went unnoticed.
        toggle.chain(query_maximized())
    }

    pub(super) fn on_title_bar(&mut self, m: crate::workspace::titlebar::Message) -> Task<Message> {
        use crate::workspace::titlebar::{Outcome, WindowOp};
        match self.titlebar.update(m, Instant::now()) {
            Outcome::None => Task::none(),
            Outcome::Rail(m) => Task::done(Message::Rail(m)),
            Outcome::QueryMaximized => query_maximized(),
            Outcome::Window(op) => window::latest().and_then(move |id| match op {
                WindowOp::Drag => window::drag(id),
                WindowOp::ToggleMaximize => window::toggle_maximize(id),
                WindowOp::Minimize => window::minimize(id, true),
                WindowOp::Resize(d) => window::drag_resize(id, d),
                WindowOp::Close => Task::done(Message::CloseRequested(id)),
            }),
        }
    }

    // --- subscriptions -----------------------------------------------------------

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = Vec::new();
        for s in self.servers.values() {
            if let Some(spec) = s.spec() {
                subs.push(
                    session::subscription(spec)
                        .map(|(sid, generation, ev)| Message::Server(sid, generation, ev)),
                );
            }
            subs.push(crate::e2e::subscription(s));
            for (rid, rv) in &s.rooms {
                subs.push(
                    rv.subscription()
                        .with((s.id.clone(), s.generation, rid.clone()))
                        .map(|((sid, epoch, rid), m)| Message::Room(sid, epoch, rid, m)),
                );
            }
        }
        if let Some(slot) = &self.listener {
            subs.push(single_instance::subscription(slot.clone()).map(Message::DeepLinks));
        }
        // Links and Dock clicks arrive as Apple Events (`macos_events`).
        #[cfg(target_os = "macos")]
        subs.push(crate::os_events::subscription().map(|e| match e {
            crate::os_events::OsEvent::Url(url) => Message::DeepLinks(vec![url]),
            crate::os_events::OsEvent::Reopen => Message::DeepLinks(Vec::new()),
        }));
        if self.config.ui.theme == ThemePref::System {
            subs.push(iced::system::theme_changes().map(Message::SystemTheme));
        }
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::Focused) => Some(Message::WindowFocused(true)),
                iced::Event::Window(window::Event::Unfocused) => {
                    Some(Message::WindowFocused(false))
                }
                _ => None,
            },
        ));
        // Every message redraws the window; typing labels are not visible while
        // it is hidden to the tray (expired entries go at the next tick).
        if !self.desktop.hidden
            && self
                .servers
                .values()
                .any(|s| s.workspace.typing().next_expiry().is_some())
        {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
        }
        #[cfg(feature = "self-update")]
        subs.extend(crate::updater::app::subscription(self));
        // A long-running session expires opened media too, not only startup.
        subs.push(
            iced::time::every(crate::room::timeline::transfer::MEDIA_PURGE_INTERVAL)
                .map(|_| Message::PurgeStaleMedia),
        );
        // Image misses recorded while drawing a frame are fetched right after it.
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::RedrawRequested(_))
                    if images::VIEW_MISSES.swap(false, std::sync::atomic::Ordering::Relaxed) =>
                {
                    Some(Message::FlushImages)
                }
                _ => None,
            },
        ));
        subs.push(window::close_requests().map(Message::CloseRequested));
        if self.custom_chrome() {
            subs.push(
                window::resize_events()
                    .map(|_| Message::TitleBar(crate::workspace::titlebar::Message::Resized)),
            );
            subs.push(self.titlebar.subscription().map(Message::TitleBar));
        }
        // Files dropped onto the window go to the active composer.
        subs.push(iced::event::listen_with(
            |event, _status, _id| match event {
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Message::FileDropped(path))
                }
                _ => None,
            },
        ));
        // Ctrl+K: quick switcher. Tab: focus traversal (modal dialogs keep Tab).
        subs.push(iced::keyboard::listen().filter_map(|ev| match ev {
            iced::keyboard::Event::KeyPressed {
                key,
                modifiers,
                physical_key,
                ..
            } if modifiers.command() && key.to_latin(physical_key) == Some('k') => {
                Some(Message::OpenQuickSwitcher)
            }
            iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                modifiers,
                ..
            } if !modifiers.command() && !modifiers.alt() => {
                Some(Message::FocusNext(!modifiers.shift()))
            }
            iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::F6),
                modifiers,
                ..
            } if !modifiers.command() && !modifiers.alt() => {
                Some(Message::FocusRegion(!modifiers.shift()))
            }
            iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers,
                ..
            } if modifiers.is_empty() => Some(Message::Escape),
            _ => None,
        }));
        subs.push(self.desktop.subscription().map(Message::Desktop));
        subs.push(self.calls.subscription().map(Message::Calls));
        match &self.screen {
            Screen::Settings(c) => subs.push(c.subscription().map(Message::Settings)),
            Screen::Account(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Account(sid, m)),
            ),
            Screen::Directory(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Directory(sid, m)),
            ),
            Screen::Admin(sid, c) => subs.push(
                c.subscription()
                    .with(sid.clone())
                    .map(|(sid, m)| Message::Admin(sid, m)),
            ),
            _ => {}
        }
        for m in &self.modals {
            match m {
                Modal::CreateRoom(sid, c) => subs.push(
                    c.subscription()
                        .with(sid.clone())
                        .map(|(sid, m)| Message::CreateRoom(sid, m)),
                ),
                Modal::QuickSwitcher(sid, c) => subs.push(
                    c.subscription()
                        .with(sid.clone())
                        .map(|(sid, m)| Message::QuickSwitcher(sid, m)),
                ),
                _ => {}
            }
        }
        for s in self.servers.values() {
            subs.push(
                s.status_menu
                    .subscription()
                    .with(s.id.clone())
                    .map(|(sid, m)| Message::StatusMenu(sid, m)),
            );
        }
        Subscription::batch(subs)
    }
}

/// Reads the window's maximize state into the custom title bar.
pub(super) fn query_maximized() -> Task<Message> {
    use crate::workspace::titlebar::Message as Tm;
    window::latest()
        .and_then(|id| window::is_maximized(id).map(|m| Message::TitleBar(Tm::Maximized(m))))
}

pub(super) fn query_handler_task() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                let q = url_handler::query();
                (q.current, q.registered, q.check)
            })
            .await
            .unwrap_or_else(|e| (None, false, Err(e.to_string())))
        },
        |(c, reg, check)| Message::HandlerQueried(c, reg, check),
    )
}
