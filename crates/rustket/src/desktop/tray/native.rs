//! `tray-icon` backend (Windows notification area, macOS menu bar).
//!
//! The icon and its menu must live on the thread running the platform event
//! loop (strictly the main thread on macOS). iced runs `update` there (the
//! winit event loop polls the application future), so the icon is created
//! and changed from `update` and kept in a thread local (`TrayIcon` is not
//! `Send`). Clicks and menu picks arrive through `tray-icon`'s global event
//! handlers and reach the tray subscription over a channel; the subscription
//! starts with [`Message::Init`] so the icon is created once the event loop
//! runs.

use std::cell::RefCell;
use std::sync::{LazyLock, Mutex};

use futures::StreamExt;
use futures::channel::mpsc;
use rocketchat::models::UserStatus;
use rust_i18n::t;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::{Message, TrayState, status_label, tooltip};

const ID_OPEN: &str = "open";
const ID_QUIT: &str = "quit";
const STATUS_PREFIX: &str = "status:";

/// Icon size in pixels (the menu bar scales it to its height).
#[cfg(windows)]
const ICON_SIZE: u32 = 32;
#[cfg(target_os = "macos")]
const ICON_SIZE: u32 = 64;

struct Native {
    icon: TrayIcon,
    shown: TrayState,
}

thread_local! {
    static ICON: RefCell<Option<Native>> = const { RefCell::new(None) };
}

struct Channel {
    tx: mpsc::UnboundedSender<Message>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
}

static CHANNEL: LazyLock<Channel> = LazyLock::new(|| {
    let (tx, rx) = mpsc::unbounded();
    Channel {
        tx,
        rx: Mutex::new(Some(rx)),
    }
});

fn send(m: Message) {
    let _ = CHANNEL.tx.unbounded_send(m);
}

/// The tray subscription: [`Message::Init`], then the icon's events.
pub fn events() -> impl futures::Stream<Item = Message> {
    let rx = match CHANNEL.rx.lock().ok().and_then(|mut g| g.take()) {
        Some(rx) => rx.left_stream(),
        None => futures::stream::empty().right_stream(),
    };
    futures::stream::once(async { Message::Init }).chain(rx)
}

fn unread(state: &TrayState) -> bool {
    state.unread > 0 || state.mentions > 0
}

fn icon(unread: bool) -> Option<Icon> {
    let (w, h, px) = super::super::icon::rgba(ICON_SIZE, unread)?;
    Icon::from_rgba(px, w, h)
        .inspect_err(|e| tracing::debug!(error = %e, "bad tray icon"))
        .ok()
}

fn status_from_id(id: &str) -> Option<UserStatus> {
    let s = id.strip_prefix(STATUS_PREFIX)?;
    [
        UserStatus::Online,
        UserStatus::Away,
        UserStatus::Busy,
        UserStatus::Offline,
    ]
    .into_iter()
    .find(|st| st.as_str() == s)
}

fn menu(state: &TrayState) -> Menu {
    let menu = Menu::new();
    let mut items: Vec<Box<dyn tray_icon::menu::IsMenuItem>> = vec![Box::new(MenuItem::with_id(
        ID_OPEN,
        t!("desktop.tray_open"),
        true,
        None,
    ))];
    if let Some(status) = state.status
        && !state.offered.is_empty()
    {
        let sub = Submenu::new(t!("desktop.tray_status"), true);
        for s in &state.offered {
            let item = CheckMenuItem::with_id(
                format!("{STATUS_PREFIX}{}", s.as_str()),
                status_label(*s),
                true,
                *s == status,
                None,
            );
            if let Err(e) = sub.append(&item) {
                tracing::debug!(error = %e, "tray menu");
            }
        }
        items.push(Box::new(sub));
    }
    items.push(Box::new(PredefinedMenuItem::separator()));
    items.push(Box::new(MenuItem::with_id(
        ID_QUIT,
        t!("desktop.tray_quit"),
        true,
        None,
    )));
    for item in &items {
        if let Err(e) = menu.append(item.as_ref()) {
            tracing::debug!(error = %e, "tray menu");
        }
    }
    menu
}

fn tooltip_text(state: &TrayState) -> String {
    format!("{}\n{}", t!("desktop.app_name"), tooltip(state))
}

fn set_handlers() {
    TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            send(Message::Activate);
        }
    }));
    MenuEvent::set_event_handler(Some(|e: MenuEvent| {
        let id = e.id.0.as_str();
        let m = match id {
            ID_OPEN => Some(Message::Show),
            ID_QUIT => Some(Message::Quit),
            _ => status_from_id(id).map(Message::SetStatus),
        };
        if let Some(m) = m {
            send(m);
        }
    }));
}

/// Drops the icon of the current thread (Windows would otherwise leave a
/// stale icon in the notification area after exit).
#[derive(Debug)]
pub struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = ICON.try_with(|c| c.try_borrow_mut().map(|mut i| i.take()));
    }
}

/// Creates the icon (once); returns whether it exists. Event loop thread only.
pub fn create(state: &TrayState) -> bool {
    ICON.with_borrow_mut(|slot| {
        if slot.is_some() {
            return true;
        }
        set_handlers();
        let mut builder = TrayIconBuilder::new()
            .with_id("rustket")
            .with_tooltip(tooltip_text(state))
            .with_menu(Box::new(menu(state)))
            .with_menu_on_left_click(false);
        if let Some(i) = icon(unread(state)) {
            builder = builder.with_icon(i);
        }
        match builder.build() {
            Ok(icon) => {
                *slot = Some(Native {
                    icon,
                    shown: state.clone(),
                });
                true
            }
            Err(e) => {
                tracing::info!(error = %e, "no system tray");
                false
            }
        }
    })
}

/// Shows `state` on the icon, if it exists. Event loop thread only.
pub fn apply(state: &TrayState) {
    ICON.with_borrow_mut(|slot| {
        let Some(n) = slot else {
            return;
        };
        if unread(state) != unread(&n.shown)
            && let Err(e) = n.icon.set_icon(icon(unread(state)))
        {
            tracing::debug!(error = %e, "tray icon");
        }
        let relabel = state.revision != n.shown.revision;
        if (relabel || (state.unread, state.mentions) != (n.shown.unread, n.shown.mentions))
            && let Err(e) = n.icon.set_tooltip(Some(tooltip_text(state)))
        {
            tracing::debug!(error = %e, "tray tooltip");
        }
        if relabel || state.status != n.shown.status || state.offered != n.shown.offered {
            n.icon.set_menu(Some(Box::new(menu(state))));
        }
        n.shown = state.clone();
    });
}
