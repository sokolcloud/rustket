//! The macOS main menu (App, View, Window and Help menus) through
//! `tray_icon::menu` (muda). Strings: `locales/menu.yml` (`menu.*`).
//!
//! winit installs a default menu when the app finishes launching, so ours is
//! installed afterwards: the subscription starts with [`Message::Init`],
//! which runs once the event loop is up. The menu is rebuilt when the UI
//! language changes ([`refresh`]).
//!
//! Items that act on the app are custom items: their picks arrive through
//! muda's global event handler (shared with the tray, see
//! `tray::native::install_menu_handler`) and become [`Message::Item`]. Quit
//! is one of them so ⌘Q takes the app's quit path (config flush, cache on
//! quit, the unsaved admin settings question) instead of `terminate:`.
//! Hide, Services, Minimize, Zoom, full screen and Bring All to Front are
//! AppKit's own actions.
//!
//! There is no Edit menu: muda menus do not auto-enable items, so a
//! predefined Copy/Paste/Cut/Select All/Undo item is always enabled and its
//! key equivalent (⌘C, ⌘V, …) is consumed by `NSMenu performKeyEquivalent:`
//! before the key reaches winit's view (`keyDown:`); the action then goes
//! up the responder chain, where winit's view implements no `copy:`/`paste:`
//! (only text input and `doCommandBySelector:`). iced's text inputs and
//! selectable text would lose those shortcuts. Items without key
//! equivalents could not trigger iced's clipboard handling either.

#[cfg(target_os = "macos")]
use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::sync::{LazyLock, Mutex};

use iced::Subscription;

use super::Request;
use crate::component::Nav;

/// The custom items (everything else is a predefined AppKit item).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    /// About Rustket: the Settings About tab.
    About,
    /// Settings… ⌘,
    Settings,
    /// Quit Rustket ⌘Q.
    Quit,
    /// Close ⌘W: hides the window (the Dock icon, ⌘0 and the tray bring it
    /// back).
    Close,
    /// "Rustket" ⌘0: shows the main window.
    MainWindow,
    /// Rustket Help: the Settings Help tab.
    Help,
    /// Support: the support page in the browser.
    Support,
    /// Privacy Policy: the privacy policy in the browser.
    Privacy,
    /// Terms of Use: the terms in the browser.
    Terms,
}

impl Item {
    pub const ALL: [Item; 9] = [
        Item::About,
        Item::Settings,
        Item::Quit,
        Item::Close,
        Item::MainWindow,
        Item::Help,
        Item::Support,
        Item::Privacy,
        Item::Terms,
    ];

    /// The muda menu id (distinct from the tray menu's ids).
    pub fn id(self) -> &'static str {
        match self {
            Item::About => "menubar.about",
            Item::Settings => "menubar.settings",
            Item::Quit => "menubar.quit",
            Item::Close => "menubar.close",
            Item::MainWindow => "menubar.main_window",
            Item::Help => "menubar.help",
            Item::Support => "menubar.support",
            Item::Privacy => "menubar.privacy",
            Item::Terms => "menubar.terms",
        }
    }

    pub fn from_id(id: &str) -> Option<Item> {
        Item::ALL.into_iter().find(|i| i.id() == id)
    }
}

/// What a pick asks of the app; `None` for [`Item::Close`], which the
/// desktop layer handles itself (it hides the window).
pub fn request(item: Item) -> Option<Request> {
    Some(match item {
        Item::About => Request::Navigate(Nav::AboutSettings),
        Item::Settings => Request::Navigate(Nav::Settings),
        Item::Help => Request::Navigate(Nav::HelpSettings),
        Item::Support => Request::OpenUrl(crate::legal::help_links().support),
        Item::Privacy => Request::OpenUrl(crate::legal::help_links().privacy),
        Item::Terms => Request::OpenUrl(crate::legal::help_links().terms),
        Item::Quit => Request::Quit,
        Item::MainWindow => Request::ShowWindow,
        Item::Close => return None,
    })
}

#[derive(Debug, Clone)]
pub enum Message {
    /// The event loop runs: install the menu.
    Init,
    Item(Item),
}

#[cfg(target_os = "macos")]
struct Channel {
    tx: futures::channel::mpsc::UnboundedSender<Message>,
    rx: Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Message>>>,
}

#[cfg(target_os = "macos")]
static CHANNEL: LazyLock<Channel> = LazyLock::new(|| {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    Channel {
        tx,
        rx: Mutex::new(Some(rx)),
    }
});

/// Reports a pick of menu id `id`; `false` when it is not one of ours.
#[cfg(target_os = "macos")]
pub(crate) fn dispatch(id: &str) -> bool {
    let Some(item) = Item::from_id(id) else {
        return false;
    };
    let _ = CHANNEL.tx.unbounded_send(Message::Item(item));
    true
}

#[cfg(target_os = "macos")]
fn events() -> impl futures::Stream<Item = Message> {
    use futures::StreamExt;
    let rx = match CHANNEL.rx.lock().ok().and_then(|mut g| g.take()) {
        Some(rx) => rx.left_stream(),
        None => futures::stream::empty().right_stream(),
    };
    futures::stream::once(async { Message::Init }).chain(rx)
}

/// [`Message::Init`], then the picks (macOS only).
pub fn subscription() -> Subscription<Message> {
    #[cfg(target_os = "macos")]
    return Subscription::run(events);
    #[cfg(not(target_os = "macos"))]
    Subscription::none()
}

#[cfg(target_os = "macos")]
thread_local! {
    /// The installed menu (muda keeps the `NSMenu` alive through it).
    static MENU: RefCell<Option<tray_icon::menu::Menu>> = const { RefCell::new(None) };
}

#[cfg(target_os = "macos")]
fn build() -> Option<tray_icon::menu::Menu> {
    use rust_i18n::t;
    use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
    use tray_icon::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let cmd = |code| Some(Accelerator::new(Modifiers::META, code));
    let item = |i: Item, label: &str, accel| MenuItem::with_id(i.id(), label, true, accel);
    let sep = PredefinedMenuItem::separator;
    let submenu = |title: &str, items: &[&dyn IsMenuItem]| {
        let s = Submenu::new(title, true);
        s.append_items(items).map(|()| s)
    };

    let app = submenu(
        &t!("desktop.app_name"),
        &[
            &item(Item::About, &t!("menu.about"), None),
            &sep(),
            &item(Item::Settings, &t!("menu.settings"), cmd(Code::Comma)),
            &sep(),
            &PredefinedMenuItem::services(Some(&t!("menu.services"))),
            &sep(),
            &PredefinedMenuItem::hide(Some(&t!("menu.hide"))),
            &PredefinedMenuItem::hide_others(Some(&t!("menu.hide_others"))),
            &PredefinedMenuItem::show_all(Some(&t!("menu.show_all"))),
            &sep(),
            &item(Item::Quit, &t!("menu.quit"), cmd(Code::KeyQ)),
        ],
    );
    let view = submenu(
        &t!("menu.view"),
        &[&PredefinedMenuItem::fullscreen(Some(&t!(
            "menu.full_screen"
        )))],
    );
    let window = submenu(
        &t!("menu.window"),
        &[
            &PredefinedMenuItem::minimize(Some(&t!("menu.minimize"))),
            &PredefinedMenuItem::maximize(Some(&t!("menu.zoom"))),
            &item(Item::Close, &t!("menu.close"), cmd(Code::KeyW)),
            &sep(),
            &item(Item::MainWindow, &t!("menu.main_window"), cmd(Code::Digit0)),
            &sep(),
            &PredefinedMenuItem::bring_all_to_front(Some(&t!("menu.bring_all_to_front"))),
        ],
    );
    let help = submenu(
        &t!("menu.help"),
        &[
            &item(Item::Help, &t!("menu.app_help"), None),
            &sep(),
            &item(Item::Support, &t!("legal.support"), None),
            &item(Item::Privacy, &t!("legal.privacy"), None),
            &item(Item::Terms, &t!("legal.terms"), None),
        ],
    );
    let (app, view, window, help) = match (app, view, window, help) {
        (Ok(a), Ok(v), Ok(w), Ok(h)) => (a, v, w, h),
        (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => {
            tracing::warn!(error = %e, "cannot build the main menu");
            return None;
        }
    };
    let menu = Menu::new();
    if let Err(e) = menu.append_items(&[&app, &view, &window, &help]) {
        tracing::warn!(error = %e, "cannot build the main menu");
        return None;
    }
    menu.init_for_nsapp();
    // Only after `init_for_nsapp`.
    window.set_as_windows_menu_for_nsapp();
    help.set_as_help_menu_for_nsapp();
    Some(menu)
}

/// Installs the main menu (again). Main thread only: iced runs `update`
/// there.
#[cfg(target_os = "macos")]
pub fn install() {
    super::tray::install_menu_handler();
    MENU.with_borrow_mut(|slot| {
        if let Some(menu) = build() {
            *slot = Some(menu);
        }
    });
}

/// Rebuilds the installed menu's labels (language change).
#[cfg(target_os = "macos")]
pub fn refresh() {
    if MENU.with_borrow(Option::is_some) {
        install();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_differ_from_the_tray() {
        for i in Item::ALL {
            assert_eq!(Item::from_id(i.id()), Some(i));
            assert!(i.id().starts_with("menubar."));
        }
        assert_eq!(Item::from_id("quit"), None, "the tray's quit");
        assert_eq!(Item::from_id("menubar.nope"), None);
    }

    #[test]
    fn items_map_to_app_requests() {
        let nav = |i| match request(i) {
            Some(Request::Navigate(n)) => Some(n),
            _ => None,
        };
        assert!(matches!(nav(Item::About), Some(Nav::AboutSettings)));
        assert!(matches!(nav(Item::Settings), Some(Nav::Settings)));
        assert!(matches!(nav(Item::Help), Some(Nav::HelpSettings)));
        assert!(matches!(request(Item::Quit), Some(Request::Quit)));
        let links = crate::legal::help_links();
        assert!(matches!(request(Item::Support), Some(Request::OpenUrl(u)) if u == links.support));
        assert!(matches!(request(Item::Privacy), Some(Request::OpenUrl(u)) if u == links.privacy));
        assert!(matches!(request(Item::Terms), Some(Request::OpenUrl(u)) if u == links.terms));
        assert!(matches!(
            request(Item::MainWindow),
            Some(Request::ShowWindow)
        ));
        assert!(
            request(Item::Close).is_none(),
            "hidden by the desktop layer"
        );
    }
}
