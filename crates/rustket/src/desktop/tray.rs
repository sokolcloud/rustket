//! System tray (StatusNotifierItem via ksni): the app icon (with a red dot
//! while something is unread), a tooltip with the counts and a menu: open,
//! status, quit. Clicking the icon toggles the window.
//!
//! The tray service runs inside a subscription; the app's state reaches it
//! through a `watch` channel, its events come back as [`Message`]s.

use std::sync::Arc;

use iced::{Subscription, Task};
use rocketchat::models::UserStatus;
use rust_i18n::t;
use tokio::sync::watch;

use super::Request;

#[derive(Debug, Clone)]
pub enum Message {
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
#[derive(Clone)]
struct Slot(Arc<watch::Receiver<TrayState>>);

impl std::hash::Hash for Slot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "rustket-tray".hash(state);
    }
}

#[derive(Debug)]
pub struct Tray {
    enabled: bool,
    state: watch::Sender<TrayState>,
    rx: Arc<watch::Receiver<TrayState>>,
    /// A tray host shows our icon.
    pub available: bool,
}

impl Default for Tray {
    fn default() -> Self {
        let (state, rx) = watch::channel(TrayState::default());
        Self {
            enabled: false,
            state,
            rx: Arc::new(rx),
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
    }

    /// Rebuilds the labels (after a language change).
    pub fn refresh(&mut self) {
        self.state.send_modify(|s| s.revision += 1);
    }

    /// Handles everything except `Activate`, which needs the window state.
    pub fn update(&mut self, msg: Message) -> (Task<super::Message>, Vec<Request>) {
        match msg {
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
        Subscription::run_with(Slot(self.rx.clone()), run)
    }
}

struct RustketTray {
    state: TrayState,
    tx: futures::channel::mpsc::UnboundedSender<Message>,
}

impl RustketTray {
    fn send(&self, m: Message) {
        let _ = self.tx.unbounded_send(m);
    }
}

impl ksni::Tray for RustketTray {
    fn id(&self) -> String {
        "rustket".into()
    }

    fn title(&self) -> String {
        t!("desktop.app_name").into_owned()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        super::icon::tray_pixmaps(self.state.unread > 0 || self.state.mentions > 0)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: t!("desktop.app_name").into_owned(),
            description: tooltip(&self.state),
            icon_pixmap: Vec::new(),
            icon_name: String::new(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Message::Activate);
    }

    fn watcher_online(&self) {
        self.send(Message::Ready(true));
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // Keep the service: the host may come back (shell restart).
        self.send(Message::Ready(false));
        true
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: t!("desktop.tray_open").into_owned(),
                activate: Box::new(|t: &mut Self| t.send(Message::Show)),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(status) = self.state.status
            && !self.state.offered.is_empty()
        {
            let offered = self.state.offered.clone();
            items.push(
                SubMenu {
                    label: t!("desktop.tray_status").into_owned(),
                    submenu: vec![
                        RadioGroup {
                            selected: offered.iter().position(|s| *s == status).unwrap_or(0),
                            select: Box::new(|t: &mut Self, i| {
                                if let Some(s) = t.state.offered.get(i) {
                                    t.send(Message::SetStatus(*s));
                                }
                            }),
                            options: offered
                                .iter()
                                .map(|s| RadioItem {
                                    label: status_label(*s),
                                    ..Default::default()
                                })
                                .collect(),
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(ksni::MenuItem::Separator);
        items.push(
            StandardItem {
                label: t!("desktop.tray_quit").into_owned(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Message::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn run(slot: &Slot) -> impl futures::Stream<Item = Message> + use<> {
    let mut rx = (*slot.0).clone();
    iced::stream::channel(
        16,
        async move |mut out: futures::channel::mpsc::Sender<Message>| {
            use futures::{SinkExt, StreamExt};
            use ksni::TrayMethods;
            let (tx, mut events) = futures::channel::mpsc::unbounded();
            let tray = RustketTray {
                state: rx.borrow_and_update().clone(),
                tx,
            };
            let handle = match tray.spawn().await {
                Ok(h) => h,
                Err(e) => {
                    tracing::info!(error = %e, "no system tray");
                    let _ = out.send(Message::Ready(false)).await;
                    return;
                }
            };
            let _ = out.send(Message::Ready(true)).await;
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let state = rx.borrow_and_update().clone();
                        if handle.update(|t| t.state = state).await.is_none() {
                            // The service died: no icon any more.
                            let _ = out.send(Message::Ready(false)).await;
                            return;
                        }
                    }
                    ev = events.next() => match ev {
                        Some(m) => {
                            if out.send(m).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            handle.shutdown().await;
        },
    )
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
