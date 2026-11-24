//! System tray: the app icon (with a dot while something is unread: red for
//! mentions, orange for `@all`, a quiet one otherwise; see [`TrayLevel`]; on
//! macOS a monochrome template glyph with a dot for mentions and `@all` and a
//! ring otherwise), a tooltip with the counts (and a notification pause) and
//! a menu: open, status, pause notifications, quit. Clicking the icon
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
use rustket_core::indicators::{IndicatorScheme, RcSignal};
use rustket_core::room_list::{BadgeVariant, WorkspaceSignal};
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
    Pause(rustket_core::quiet::PausePreset),
    Resume,
    Quit,
}

/// The dot on the tray icon, weakest first. The macOS menu bar glyph is
/// monochrome: a ring for [`TrayLevel::Unread`], a dot for the mention levels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrayLevel {
    /// The plain icon: nothing counted is unread.
    #[default]
    None,
    /// A quiet dot (the unread colour; a ring on macOS): plain or thread
    /// unread only.
    Unread,
    /// An orange dot (a dot on macOS): `@all` / `@here`, no mentions.
    GroupMention,
    /// A red dot (a dot on macOS): mentions (a DM included).
    Mention,
}

impl TrayLevel {
    /// The dot for the unread signal over all accounts.
    pub fn of(signal: &WorkspaceSignal) -> Self {
        match signal.strongest {
            _ if signal.mentions > 0 => TrayLevel::Mention,
            Some(BadgeVariant::Mention) => TrayLevel::Mention,
            Some(BadgeVariant::GroupMention) => TrayLevel::GroupMention,
            Some(_) => TrayLevel::Unread,
            None => TrayLevel::None,
        }
    }
}

/// The tray icon's art: Rustket's dot of a [`TrayLevel`], or the Rocket.Chat
/// desktop app's counter (`Badge.tsx`) on Rustket's logo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TrayArt {
    /// The plain icon.
    #[default]
    Plain,
    /// Rustket: the dot of the level (never [`TrayLevel::None`]).
    Level(TrayLevel),
    /// Rocket.Chat: 1…9 unread.
    RcCount(u8),
    /// Rocket.Chat: more than 9 unread ("+9").
    RcPlus,
    /// Rocket.Chat: the bullet (alert, nothing counted).
    RcDot,
}

impl TrayArt {
    /// The art for the unread signals over all accounts under `scheme`.
    pub fn of(signal: &WorkspaceSignal, rc: &RcSignal, scheme: IndicatorScheme) -> Self {
        match scheme {
            IndicatorScheme::Rustket => match TrayLevel::of(signal) {
                TrayLevel::None => TrayArt::Plain,
                level => TrayArt::Level(level),
            },
            IndicatorScheme::RocketChat => match rc.unread {
                0 if rc.bullet => TrayArt::RcDot,
                0 => TrayArt::Plain,
                n @ 1..=9 => TrayArt::RcCount(n as u8),
                _ => TrayArt::RcPlus,
            },
        }
    }

    /// The Rocket.Chat badge glyph, if any.
    pub fn glyph(self) -> Option<super::badge_art::Glyph> {
        use super::badge_art::Glyph;
        match self {
            TrayArt::RcCount(n) => Some(Glyph::Digit(n)),
            TrayArt::RcPlus => Some(Glyph::Plus),
            TrayArt::RcDot => Some(Glyph::Dot),
            TrayArt::Plain | TrayArt::Level(_) => None,
        }
    }

    /// The Rustket level drawn (`None` for the plain icon; the Rocket.Chat
    /// art is not a level).
    pub fn level(self) -> TrayLevel {
        match self {
            TrayArt::Level(l) => l,
            _ => TrayLevel::None,
        }
    }
}

/// What the tray shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    /// Counted unread rooms over all accounts.
    pub unread: u64,
    pub mentions: u64,
    /// Rustket's level (drawn only under the Rustket scheme; see `art`).
    pub level: TrayLevel,
    /// The icon's art ([`TrayArt::of`]).
    pub art: TrayArt,
    /// The indicator scheme the tooltip and art follow.
    pub scheme: IndicatorScheme,
    /// Rocket.Chat's unread signal over all accounts (Rocket.Chat scheme).
    pub rc: RcSignal,
    /// The user's status on the active server (`None`: not logged in).
    pub status: Option<UserStatus>,
    /// Statuses the active server allows, in menu order.
    pub offered: Vec<UserStatus>,
    /// Notifications are paused: the line saying until when.
    pub paused: Option<String>,
    /// Labels of the pause presets (`PausePreset::ALL` order).
    pub pause_labels: Vec<String>,
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

/// Tooltip text for the counts (and the pause).
pub fn tooltip(state: &TrayState) -> String {
    let counts = match (state.scheme, state.unread, state.mentions) {
        (IndicatorScheme::RocketChat, ..) => match state.rc {
            RcSignal {
                unread: n @ 1.., ..
            } => t!("desktop.tray_rc_unread", count = n),
            RcSignal { bullet: true, .. } => t!("desktop.tray_rc_alert"),
            _ => t!("desktop.tray_no_unread"),
        }
        .into_owned(),
        (_, 0, 0) => t!("desktop.tray_no_unread").into_owned(),
        (_, u, 0) => t!("desktop.tray_unread", count = u).into_owned(),
        (_, u, m) => t!("desktop.tray_unread_mentions", mentions = m, count = u).into_owned(),
    };
    match &state.paused {
        Some(p) => format!("{counts}\n{p}"),
        None => counts,
    }
}

/// What a `tray-icon` backend redraws when the state goes from `shown` to
/// `state`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repaint {
    pub icon: bool,
    pub tooltip: bool,
    pub menu: bool,
    /// The macOS menu bar title ([`menubar_title`]).
    pub title: bool,
}

impl Repaint {
    pub fn of(shown: &TrayState, state: &TrayState) -> Self {
        let relabel = state.revision != shown.revision;
        let counts = |s: &TrayState| (s.unread, s.mentions, s.scheme, s.rc);
        Self {
            icon: state.art != shown.art,
            tooltip: relabel || counts(state) != counts(shown) || state.paused != shown.paused,
            menu: relabel
                || state.status != shown.status
                || state.offered != shown.offered
                || state.paused != shown.paused
                || state.pause_labels != shown.pause_labels,
            title: menubar_title(state) != menubar_title(shown),
        }
    }
}

/// The macOS menu bar title: Rocket.Chat's unread count next to the glyph
/// (Rocket.Chat scheme only).
pub fn menubar_title(state: &TrayState) -> Option<String> {
    (state.scheme == IndicatorScheme::RocketChat && state.rc.unread > 0)
        .then(|| state.rc.unread.to_string())
}

/// The pause entries of the tray menus: `(label, message)`; "Resume" while
/// paused.
pub fn pause_entries(state: &TrayState) -> Vec<(String, Message)> {
    let mut v: Vec<(String, Message)> = rustket_core::quiet::PausePreset::ALL
        .iter()
        .zip(&state.pause_labels)
        .map(|(p, l)| (l.clone(), Message::Pause(*p)))
        .collect();
    if state.paused.is_some() {
        v.push((t!("quiet.resume").into_owned(), Message::Resume));
    }
    v
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

    /// The unread signal over all accounts: the counts and the icon's dot.
    pub fn set_unread(&mut self, signal: &WorkspaceSignal) {
        self.set_signals(signal, RcSignal::default(), IndicatorScheme::Rustket);
    }

    /// The unread signals over all accounts under the Rocket.Chat scheme:
    /// the desktop app's counter art and tooltip.
    pub fn set_unread_rc(&mut self, signal: &WorkspaceSignal, rc: RcSignal) {
        self.set_signals(signal, rc, IndicatorScheme::RocketChat);
    }

    fn set_signals(&mut self, signal: &WorkspaceSignal, rc: RcSignal, scheme: IndicatorScheme) {
        let (unread, mentions, level) = (signal.rooms, signal.mentions, TrayLevel::of(signal));
        let art = TrayArt::of(signal, &rc, scheme);
        self.state.send_if_modified(|s| {
            let changed = (s.unread, s.mentions, s.level, s.art, s.scheme, s.rc)
                != (unread, mentions, level, art, scheme, rc);
            s.unread = unread;
            s.mentions = mentions;
            s.level = level;
            s.art = art;
            s.scheme = scheme;
            s.rc = rc;
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

    /// The pause line (`None`: not paused) and the preset labels.
    pub fn set_pause(&mut self, paused: Option<String>, labels: Vec<String>) {
        self.state.send_if_modified(|s| {
            let changed = s.paused != paused || s.pause_labels != labels;
            s.paused = paused;
            s.pause_labels = labels;
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
            Message::Pause(p) => (Task::none(), vec![Request::Pause(p)]),
            Message::Resume => (Task::none(), vec![Request::Resume]),
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
        tray.set_unread(&WorkspaceSignal {
            mentions: 1,
            strongest: Some(BadgeVariant::Mention),
            rooms: 3,
        });
        tray.set_status(Some((
            UserStatus::Busy,
            vec![UserStatus::Online, UserStatus::Busy],
        )));
        let s = tray.state();
        assert_eq!(
            (s.unread, s.mentions, s.level, s.status),
            (3, 1, TrayLevel::Mention, Some(UserStatus::Busy))
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
        assert_eq!(tooltip(&s), "Mentions: 1 · unread rooms: 2");
        s.paused = Some("Notifications paused until 14:30".into());
        assert_eq!(
            tooltip(&s),
            "Mentions: 1 · unread rooms: 2\nNotifications paused until 14:30"
        );
    }

    /// Items 3/4/7: red for mentions, orange for `@all` without mentions, a
    /// quiet dot for any other counted unread, nothing otherwise.
    #[test]
    fn tray_levels() {
        let sig = |mentions, strongest, rooms| WorkspaceSignal {
            mentions,
            strongest,
            rooms,
        };
        use BadgeVariant::*;
        assert_eq!(TrayLevel::of(&sig(0, None, 0)), TrayLevel::None);
        assert_eq!(TrayLevel::of(&sig(2, Some(Mention), 1)), TrayLevel::Mention);
        assert_eq!(
            TrayLevel::of(&sig(0, Some(GroupMention), 2)),
            TrayLevel::GroupMention
        );
        assert_eq!(TrayLevel::of(&sig(0, Some(Unread), 1)), TrayLevel::Unread);
        assert_eq!(TrayLevel::of(&sig(0, Some(Thread), 1)), TrayLevel::Unread);
        let mut tray = Tray::default();
        tray.set_unread(&sig(0, Some(GroupMention), 4));
        let s = tray.state();
        assert_eq!(
            (s.unread, s.mentions, s.level),
            (4, 0, TrayLevel::GroupMention)
        );
        tray.set_unread(&sig(0, Some(Thread), 4));
        assert_eq!(tray.state().level, TrayLevel::Unread);
    }

    #[test]
    fn pause_entries_and_requests() {
        rust_i18n::set_locale("en");
        let mut tray = Tray::default();
        tray.set_pause(None, vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        let entries = pause_entries(&tray.state());
        assert_eq!(entries.len(), 4);
        tray.set_pause(Some("paused".into()), tray.state().pause_labels);
        let entries = pause_entries(&tray.state());
        assert!(matches!(entries.last(), Some((_, Message::Resume))));
        let (_, r) = tray.update(Message::Pause(rustket_core::quiet::PausePreset::Hour1));
        assert!(matches!(
            r.as_slice(),
            [Request::Pause(rustket_core::quiet::PausePreset::Hour1)]
        ));
        let (_, r) = tray.update(Message::Resume);
        assert!(matches!(r.as_slice(), [Request::Resume]));
    }

    fn rc(unread: u64, bullet: bool) -> RcSignal {
        RcSignal { unread, bullet }
    }

    /// Tray art: 1–9, "+9" above 9, the bullet's dot, else plain; the
    /// Rustket scheme keeps its levels.
    #[test]
    fn tray_art_per_scheme() {
        use IndicatorScheme::{RocketChat, Rustket};
        let none = WorkspaceSignal::default();
        let loud = WorkspaceSignal {
            mentions: 2,
            strongest: Some(BadgeVariant::Mention),
            rooms: 3,
        };
        assert_eq!(
            TrayArt::of(&none, &rc(0, false), RocketChat),
            TrayArt::Plain
        );
        for n in 1..=9u8 {
            assert_eq!(
                TrayArt::of(&none, &rc(u64::from(n), false), RocketChat),
                TrayArt::RcCount(n)
            );
        }
        assert_eq!(
            TrayArt::of(&none, &rc(10, true), RocketChat),
            TrayArt::RcPlus
        );
        assert_eq!(
            TrayArt::of(&none, &rc(5000, false), RocketChat),
            TrayArt::RcPlus
        );
        assert_eq!(TrayArt::of(&none, &rc(0, true), RocketChat), TrayArt::RcDot);
        // Rocket.Chat ignores Rustket's mentions, and Rustket ignores `rc`.
        assert_eq!(
            TrayArt::of(&loud, &rc(0, false), RocketChat),
            TrayArt::Plain
        );
        assert_eq!(TrayArt::of(&none, &rc(4, true), Rustket), TrayArt::Plain);
        assert_eq!(
            TrayArt::of(&loud, &rc(0, false), Rustket),
            TrayArt::Level(TrayLevel::Mention)
        );
        assert_eq!(TrayArt::RcCount(3).level(), TrayLevel::None);
        assert_eq!(TrayArt::Level(TrayLevel::Unread).level(), TrayLevel::Unread);
        assert_eq!(TrayArt::Level(TrayLevel::Unread).glyph(), None);
    }

    /// The Rocket.Chat state, its tooltip and the macOS menu bar title; the
    /// Rustket path puts everything back.
    #[test]
    fn rc_state_and_tooltip() {
        rust_i18n::set_locale("en");
        let sig = WorkspaceSignal {
            mentions: 1,
            strongest: Some(BadgeVariant::Mention),
            rooms: 2,
        };
        let mut tray = Tray::default();
        tray.set_unread_rc(&sig, rc(12, false));
        let s = tray.state();
        assert_eq!(
            (s.art, s.scheme, s.rc, s.unread, s.mentions),
            (
                TrayArt::RcPlus,
                IndicatorScheme::RocketChat,
                rc(12, false),
                2,
                1
            )
        );
        assert_eq!(tooltip(&s), "Unread messages: 12");
        assert_eq!(menubar_title(&s).as_deref(), Some("12"));
        tray.set_unread_rc(&sig, rc(0, true));
        let s = tray.state();
        assert_eq!(s.art, TrayArt::RcDot);
        assert_eq!(tooltip(&s), "Unread messages");
        assert_eq!(menubar_title(&s), None);
        tray.set_unread_rc(&WorkspaceSignal::default(), rc(0, false));
        assert_eq!(tooltip(&tray.state()), "No unread messages");
        tray.set_pause(Some("Paused".into()), Vec::new());
        tray.set_unread_rc(&sig, rc(3, false));
        assert_eq!(tooltip(&tray.state()), "Unread messages: 3\nPaused");
        tray.set_unread(&sig);
        let s = tray.state();
        assert_eq!(
            (s.art, s.scheme, s.rc),
            (
                TrayArt::Level(TrayLevel::Mention),
                IndicatorScheme::Rustket,
                RcSignal::default()
            )
        );
        assert_eq!(tooltip(&s), "Mentions: 1 · unread rooms: 2\nPaused");
        assert_eq!(menubar_title(&s), None);
    }

    /// The `tray-icon` backends repaint the icon on 1 → 2, the tooltip on any
    /// count, and the title only with the Rocket.Chat count; a scheme switch
    /// alone repaints the icon and tooltip.
    #[test]
    fn repaint_on_rc_changes() {
        let sig = WorkspaceSignal::default();
        let state = |n: u64| {
            let mut t = Tray::default();
            t.set_unread_rc(&sig, rc(n, false));
            t.state()
        };
        let r = Repaint::of(&state(1), &state(2));
        assert!(r.icon && r.tooltip && r.title && !r.menu, "{r:?}");
        assert_eq!(Repaint::of(&state(2), &state(2)), Repaint::default());
        let r = Repaint::of(&state(10), &state(11));
        assert!(!r.icon && r.tooltip && r.title, "{r:?}");
        let mut t = Tray::default();
        t.set_unread(&sig);
        let r = Repaint::of(&t.state(), &state(0));
        assert!(!r.icon && r.tooltip && !r.title, "{r:?}");
        let r = Repaint::of(&t.state(), &state(1));
        assert!(r.icon && r.tooltip && r.title, "{r:?}");
        // Rustket: as before, the icon follows the level.
        let mut a = Tray::default();
        a.set_unread(&WorkspaceSignal {
            mentions: 0,
            strongest: Some(BadgeVariant::Unread),
            rooms: 1,
        });
        let mut b = Tray::default();
        b.set_unread(&WorkspaceSignal {
            mentions: 0,
            strongest: Some(BadgeVariant::Unread),
            rooms: 2,
        });
        let r = Repaint::of(&a.state(), &b.state());
        assert!(!r.icon && r.tooltip && !r.title && !r.menu, "{r:?}");
    }
}
