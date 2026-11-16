//! App preferences (local, not per server) in tabs: General, Appearance,
//! Notifications, Sidebar, Mentions, Privacy & storage, About. Each tab's cards are
//! built in `settings/<tab>.rs`; this file keeps the state, the messages, the
//! update and the tab frame. Strings: `locales/settings.yml` (`settings.*`).

use iced::widget::{Column, container};
use iced::{Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::config::{
    AppConfig, Density, Language, SidebarSort, Theme as ThemePref, ThreadsMode, UiPrefs, ZOOM_RANGE,
};
use rustket_core::secrets::SecretBackend;
use rustket_ui::icon::Icon;
use rustket_ui::theme::SPACE_6;
use rustket_ui::widget::BannerKind;

use crate::component::{Action, Effect};
use crate::desktop::TraySupport;
use crate::desktop::autostart::{self, Autostart};
use crate::ui_state::UiState;

mod about;
mod appearance;
mod general;
mod mentions;
mod notifications;
mod pickers;
mod privacy;
mod sidebar;
mod widgets;

pub use mentions::{
    CheckReport as MentionCheckReport, GroupCheck as MentionGroupCheck, Issue as MentionIssue,
    MentionAccount, Message as MentionsMessage, Suggestion as MentionSuggestion,
};
pub use privacy::size_label;
pub use sidebar::{FlagAccount, GroupDraft, Message as SidebarMessage};

/// A settings tab (left nav, or a wrapping row in narrow windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Tab {
    #[default]
    General,
    Appearance,
    Notifications,
    Sidebar,
    Mentions,
    Privacy,
    About,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::General,
        Tab::Appearance,
        Tab::Notifications,
        Tab::Sidebar,
        Tab::Mentions,
        Tab::Privacy,
        Tab::About,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Tab::General => "settings.tab_general",
            Tab::Appearance => "settings.appearance",
            Tab::Notifications => "settings.notifications",
            Tab::Sidebar => "settings.sidebar",
            Tab::Mentions => "settings.tab_mentions",
            Tab::Privacy => "settings.tab_privacy",
            Tab::About => "settings.about",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tab::General => Icon::Settings,
            Tab::Appearance => Icon::Palette,
            Tab::Notifications => Icon::Bell,
            Tab::Sidebar => Icon::PanelLeft,
            Tab::Mentions => Icon::AtSign,
            Tab::Privacy => Icon::ShieldCheck,
            Tab::About => Icon::Info,
        }
    }
}

/// The first card of the Sidebar tab (scrolled to by the sidebar's
/// "Sidebar settings…").
pub const SIDEBAR_CARD: &str = "settings-sidebar";
/// The Storage card (Privacy & storage tab).
pub const STORAGE_CARD: &str = "settings-storage";
/// The secret store card (Privacy & storage tab).
pub const SECRET_STORE_CARD: &str = "settings-secret-store";

/// Container id of a group's row in the Sidebar tab's section list.
pub fn sidebar_group_row_id(group: &str) -> String {
    format!("settings-sidebar-group-{group}")
}

/// Id of a flag's name field in the Flags block (per account).
pub fn flag_name_id(sid: &rustket_core::ServerId, color: rustket_core::flags::FlagColor) -> String {
    format!("settings-flag-{}-{}", sid.as_str(), color.as_str())
}

/// Container id of a flag's row in the Flags block.
pub fn flag_row_id(sid: &rustket_core::ServerId, color: rustket_core::flags::FlagColor) -> String {
    format!("settings-flag-row-{}-{}", sid.as_str(), color.as_str())
}

fn reveal_container<M: Send + 'static>(id: iced::widget::Id) -> Task<M> {
    crate::room::timeline::list::reveal(iced::widget::Id::new(super::page::SCROLL_ID), id, false)
}

/// Scrolls the open settings page to the Sidebar tab's first card (the
/// Sidebar tab must be open: [`Settings::with_tab`]).
pub fn reveal_sidebar<M: Send + 'static>() -> Task<M> {
    reveal_container(iced::widget::Id::new(SIDEBAR_CARD))
}

/// Scrolls the open settings page to a group's row in the Sidebar tab
/// (where its deletion preset by "Ungroup…" is confirmed).
pub fn reveal_sidebar_group<M: Send + 'static>(group: &str) -> Task<M> {
    reveal_container(iced::widget::Id::from(sidebar_group_row_id(group)))
}

/// Scrolls the open settings page to a flag's name field and focuses it
/// ("Rename flag…" of a flag section).
pub fn reveal_flag<M: Send + 'static>(
    sid: &rustket_core::ServerId,
    color: rustket_core::flags::FlagColor,
) -> Task<M> {
    Task::batch([
        reveal_container(iced::widget::Id::from(flag_row_id(sid, color))),
        iced::widget::operation::focus(iced::widget::Id::from(flag_name_id(sid, color))),
    ])
}

/// Scrolls the open settings page to the secret store card (the Privacy &
/// storage tab must be open).
pub fn reveal_secret_store<M: Send + 'static>() -> Task<M> {
    reveal_container(iced::widget::Id::new(SECRET_STORE_CARD))
}

/// Scrolls the open settings page to the Storage card (the Privacy &
/// storage tab must be open).
pub fn reveal_storage<M: Send + 'static>() -> Task<M> {
    reveal_container(iced::widget::Id::new(STORAGE_CARD))
}

/// Zoom step of the −/+ buttons.
pub const ZOOM_STEP: f32 = 0.1;
pub const WEBSITE: &str = "https://rocket.chat";
pub const DOCS: &str = "https://docs.rocket.chat";
/// Choices of the image disk cache cap (MiB).
pub const IMAGE_CACHE_CHOICES: [u32; 5] = [100, 250, 500, 1000, 2000];

#[derive(Debug, Clone)]
pub enum Message {
    SetTheme(ThemePref),
    /// External theme id (`None`: built-in Rustket).
    SetThemeName(Option<String>),
    ReloadThemes,
    OpenThemesFolder,
    SetDensity(Density),
    /// Zoom in (`1`), out (`-1`) or reset (`0`).
    Zoom(i8),
    SetShowAvatars(bool),
    SetUnreadOnTop(bool),
    SetClock24(bool),
    /// `None`: the server's `UI_Use_Real_Name`.
    SetRealNames(Option<bool>),
    /// `UiPrefs::language`: `"system"` or a locale code.
    SetLanguage(Language),
    ReloadLocales,
    OpenLocalesFolder,
    SetNotifications(bool),
    SetPreview(bool),
    SetSound(bool),
    TestNotification,
    SetReminderNotifications(bool),
    SetReminderSound(bool),
    SetReminderMorningHour(u8),
    /// Shows a tab (scrolled to its top).
    Tab(Tab),
    SetCloseToTray(bool),
    SetMinimizeToTray(bool),
    SetStartMinimized(bool),
    /// `UiPrefs::escape_marks_read`.
    SetEscapeMarksRead(bool),
    /// `UiPrefs::confirm_broad_mentions`.
    SetConfirmBroadMentions(bool),
    /// `UiPrefs::room_bookmarks`.
    SetRoomBookmarks(bool),
    /// `UiPrefs::share_utc_offset`.
    SetShareUtcOffset(bool),
    /// Open the keyboard shortcuts cheat sheet.
    ShowShortcuts,
    SetHardwareAcceleration(bool),
    SetSystemTitleBar(bool),
    SetAutostart(bool),
    /// Whether the autostart entry exists (`None`: unknown).
    AutostartLoaded(Option<bool>),
    AutostartSet(bool, Result<(), String>),
    RegisterHandler,
    ClearImageCache,
    /// Disk cache cap in MiB (`UiPrefs::image_cache_mb`).
    SetImageCacheMb(u32),
    ClearOfflineCache,
    /// `UiPrefs::secrets_memory_only` (the app asks before turning it on).
    SetSecretsMemoryOnly(bool),
    // Sidebar tab (`settings/sidebar.rs`).
    SetGroupByType(bool),
    SetSidebarSort(SidebarSort),
    SetThreadsMode(ThreadsMode),
    SetStarredThreads(bool),
    /// `UiPrefs::sidebar_pinned_views`.
    SetPinnedViews(bool),
    /// The section list, the group editor and the layout reset.
    Sidebar(SidebarMessage),
    /// The Mentions tab: mention groups and their editor.
    Mentions(MentionsMessage),
    OpenUrl(String),
    Close,
    // Updates card (`crate::updater`).
    #[cfg(feature = "self-update")]
    SetCheckUpdates(bool),
    #[cfg(feature = "self-update")]
    SetUpdatePrereleases(bool),
    #[cfg(feature = "self-update")]
    Updater(crate::updater::app::Msg),
}

impl From<SidebarMessage> for Message {
    fn from(m: SidebarMessage) -> Self {
        Self::Sidebar(m)
    }
}

/// What the settings screen may read and change.
pub struct SettingsCtx<'a> {
    pub config: &'a mut AppConfig,
    pub ui_state: &'a mut UiState,
}

pub struct SettingsView<'a> {
    pub config: &'a AppConfig,
    pub themes: &'a crate::themes::Themes,
    pub locales: &'a crate::locales::Locales,
    pub handler_registered: bool,
    pub secret_backend: Option<SecretBackend>,
    /// The secret store is the system one (keyring / file / memory fallback),
    /// not a test's memory store.
    pub secrets_system: bool,
    /// The secret store is switching backends (the memory-only switch waits).
    pub secrets_switching: bool,
    /// Whether a tray exists and the window can be hidden to it.
    pub tray: TraySupport,
    /// Offline cache (`crate::cache`): `Some((bytes on disk, clearing))`
    /// when it is enabled.
    pub offline_cache: Option<(Option<u64>, bool)>,
    /// Flags of the signed-in accounts, the active server's first (its
    /// names and colors also label the flag sections).
    pub flags: Vec<FlagAccount<'a>>,
    /// "Move kept threads to Favorites" is running.
    pub star_kept_busy: bool,
    /// Mention groups of the signed-in accounts, the active server's first.
    pub mention_groups: Vec<MentionAccount<'a>>,
    #[cfg(feature = "self-update")]
    pub updater: crate::updater::view::SettingsData,
}

#[derive(Debug)]
pub struct Settings {
    /// The tab shown.
    pub tab: Tab,
    /// Where the launch-at-login entry lives (a temporary dir in tests).
    autostart_at: Option<Autostart>,
    /// Launch at login (`None` until read).
    pub autostart: Option<bool>,
    pub autostart_busy: bool,
    /// Id of the sidebar group whose deletion awaits confirmation.
    pub confirm_delete: Option<String>,
    /// The sidebar group being created or edited.
    pub group_draft: Option<GroupDraft>,
    /// The sidebar layout reset awaits confirmation.
    pub confirm_reset: bool,
    /// Flag names being edited, per account and color.
    pub flag_drafts: std::collections::BTreeMap<
        (rustket_core::ServerId, rustket_core::flags::FlagColor),
        String,
    >,
    /// "Remove from all" of this account's flag awaits confirmation.
    pub confirm_flag_clear: Option<(rustket_core::ServerId, rustket_core::flags::FlagColor)>,
    /// "Move kept threads to Favorites" awaits confirmation.
    pub confirm_star_kept: bool,
    /// The mention group being created or edited.
    pub mention_draft: Option<mentions::Draft>,
    /// The deletion of this account's mention group awaits confirmation.
    pub confirm_mention_delete: Option<(rustket_core::ServerId, String)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// Next zoom level for a −/+/reset step, clamped and rounded to the step.
pub fn zoom_step(current: f32, dir: i8) -> f32 {
    if dir == 0 {
        return 1.0;
    }
    let next = current + f32::from(dir.signum()) * ZOOM_STEP;
    ((next / ZOOM_STEP).round() * ZOOM_STEP).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end())
}

impl Settings {
    pub fn new() -> Self {
        Self::with_autostart(Autostart::system())
    }

    pub fn with_autostart(at: Option<Autostart>) -> Self {
        Self {
            tab: Tab::default(),
            autostart_at: at,
            autostart: None,
            autostart_busy: false,
            confirm_delete: None,
            group_draft: None,
            confirm_reset: false,
            flag_drafts: Default::default(),
            confirm_flag_clear: None,
            confirm_star_kept: false,
            mention_draft: None,
            confirm_mention_delete: None,
        }
    }

    /// The same screen showing `tab` first (deep links reveal cards in it).
    pub fn with_tab(mut self, tab: Tab) -> Self {
        self.tab = tab;
        self
    }

    /// Reads the launch-at-login state (run when the screen opens).
    pub fn load(&self) -> Task<Message> {
        let Some(at) = self.autostart_at.clone() else {
            return Task::done(Message::AutostartLoaded(None));
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || at.is_enabled())
                    .await
                    .ok()
            },
            Message::AutostartLoaded,
        )
    }

    fn pref(ctx: &mut SettingsCtx<'_>, f: impl FnOnce(&mut UiPrefs)) -> Action<Message> {
        f(&mut ctx.config.ui);
        Action::effect(Effect::SaveConfig)
    }

    pub fn update(&mut self, msg: Message, ctx: &mut SettingsCtx<'_>) -> Action<Message> {
        match msg {
            Message::SetTheme(v) => Self::pref(ctx, |p| p.theme = v),
            Message::SetThemeName(v) => {
                Self::pref(ctx, |p| p.theme_name = v).with(Effect::ApplyTheme { rescan: false })
            }
            Message::ReloadThemes => Action::effect(Effect::ApplyTheme { rescan: true }),
            Message::OpenThemesFolder => Action::effect(Effect::OpenThemesFolder),
            Message::SetDensity(v) => Self::pref(ctx, |p| p.density = v),
            Message::Zoom(dir) => Self::pref(ctx, |p| p.zoom = zoom_step(p.zoom, dir)),
            Message::SetShowAvatars(v) => Self::pref(ctx, |p| p.show_avatars = v),
            Message::SetUnreadOnTop(v) => Self::pref(ctx, |p| p.unread_on_top = v),
            Message::SetClock24(v) => Self::pref(ctx, |p| p.clock_24h = v),
            Message::SetRealNames(v) => Self::pref(ctx, |p| p.use_real_names = v),
            Message::SetLanguage(l) => {
                crate::i18n::apply(&l);
                Self::pref(ctx, |p| p.language = l)
            }
            Message::ReloadLocales => Action::effect(Effect::ReloadLocales),
            Message::OpenLocalesFolder => Action::effect(Effect::OpenLocalesFolder),
            Message::SetNotifications(v) => Self::pref(ctx, |p| p.desktop_notifications = v),
            Message::SetPreview(v) => Self::pref(ctx, |p| p.notification_preview = v),
            Message::SetSound(v) => Self::pref(ctx, |p| p.sound = v),
            Message::SetReminderNotifications(v) => {
                Self::pref(ctx, |p| p.reminder_notifications = v)
            }
            Message::SetReminderSound(v) => Self::pref(ctx, |p| p.reminder_sound = v),
            Message::SetReminderMorningHour(h) => Self::pref(ctx, |p| p.reminder_morning_hour = h),
            Message::TestNotification => Action::effect(Effect::TestNotification),
            Message::Tab(tab) => {
                self.tab = tab;
                Action::task(iced::widget::operation::snap_to(
                    iced::widget::Id::new(super::page::SCROLL_ID),
                    iced::widget::scrollable::RelativeOffset::START,
                ))
            }
            Message::SetCloseToTray(v) => Self::pref(ctx, |p| p.close_to_tray = v),
            Message::SetMinimizeToTray(v) => Self::pref(ctx, |p| p.minimize_to_tray = v),
            Message::SetStartMinimized(v) => Self::pref(ctx, |p| p.start_minimized = v),
            Message::SetEscapeMarksRead(v) => Self::pref(ctx, |p| p.escape_marks_read = v),
            Message::SetRoomBookmarks(v) => Self::pref(ctx, |p| p.room_bookmarks = v),
            Message::SetConfirmBroadMentions(v) => {
                Self::pref(ctx, |p| p.confirm_broad_mentions = v)
            }
            Message::SetShareUtcOffset(v) => Self::pref(ctx, |p| p.share_utc_offset = v),
            Message::ShowShortcuts => {
                Action::effect(Effect::Navigate(crate::component::Nav::Shortcuts))
            }
            Message::SetHardwareAcceleration(v) => Self::pref(ctx, |p| p.hardware_acceleration = v),
            Message::SetSystemTitleBar(v) => Self::pref(ctx, |p| p.system_title_bar = v),
            Message::SetAutostart(on) => {
                let Some(at) = self.autostart_at.clone() else {
                    return Action::none();
                };
                if self.autostart_busy {
                    return Action::none();
                }
                self.autostart_busy = true;
                Action::task(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let exe = autostart::executable().map_err(|e| e.to_string())?;
                            at.set_enabled(on, &exe).map_err(|e| e.to_string())
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    move |r| Message::AutostartSet(on, r),
                ))
            }
            Message::AutostartLoaded(v) => {
                self.autostart = v;
                Action::none()
            }
            Message::AutostartSet(on, r) => {
                self.autostart_busy = false;
                match r {
                    Ok(()) => {
                        self.autostart = Some(on);
                        Action::none()
                    }
                    Err(e) => Action::effect(Effect::Toast(
                        BannerKind::Error,
                        t!("settings.autostart_failed").into_owned(),
                        e,
                    )),
                }
            }
            Message::RegisterHandler => Action::effect(Effect::RegisterUrlHandler),
            Message::ClearImageCache => {
                Action::effect(Effect::ClearImageCache).with(Effect::Toast(
                    BannerKind::Success,
                    t!("settings.cache_cleared").into_owned(),
                    String::new(),
                ))
            }
            Message::SetImageCacheMb(mb) => Self::pref(ctx, |p| p.image_cache_mb = mb),
            // The result is toasted by `crate::cache` once it is done.
            Message::ClearOfflineCache => Action::effect(Effect::ClearOfflineCache),
            // The app stores the preference once the store follows it.
            Message::SetSecretsMemoryOnly(on) => Action::effect(Effect::SetSecretsMemoryOnly(on)),
            Message::OpenUrl(url) => Action::effect(Effect::OpenUrl(url)),
            Message::Close => Action::effect(Effect::Close),
            Message::SetGroupByType(v) => Self::pref(ctx, |p| p.group_by_type = v),
            Message::SetSidebarSort(s) => Self::pref(ctx, |p| p.sidebar_sort = s),
            Message::SetThreadsMode(m) => Self::pref(ctx, |p| p.sidebar_threads = m),
            Message::SetStarredThreads(v) => Self::pref(ctx, |p| p.sidebar_starred_threads = v),
            Message::SetPinnedViews(v) => Self::pref(ctx, |p| p.sidebar_pinned_views = v),
            Message::Sidebar(m) => self.update_sidebar(m, ctx),
            Message::Mentions(m) => self.update_mentions(m),
            #[cfg(feature = "self-update")]
            Message::SetCheckUpdates(v) => Self::pref(ctx, |p| p.check_updates = v),
            #[cfg(feature = "self-update")]
            Message::SetUpdatePrereleases(v) => {
                let a = Self::pref(ctx, |p| p.update_prereleases = v);
                if v {
                    a
                } else {
                    a.with(Effect::Updater(crate::updater::app::Msg::PrereleasesOff))
                }
            }
            #[cfg(feature = "self-update")]
            Message::Updater(m) => Action::effect(Effect::Updater(m)),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    /// The page: tab navigation and the current tab's cards. Takes the view
    /// data by value: the body is rebuilt whenever the layout width changes.
    pub fn view<'a>(&'a self, v: SettingsView<'a>) -> Element<'a, Message> {
        let nav = Tab::ALL
            .into_iter()
            .map(|tab| super::page::NavEntry {
                icon: tab.icon(),
                label: t!(tab.i18n_key()).into_owned(),
                selected: self.tab == tab,
                on_press: Message::Tab(tab),
            })
            .collect();
        super::page::page_with_nav(
            t!("settings.title").into_owned(),
            nav,
            move || self.tab_body(&v),
            Message::Close,
        )
    }

    fn tab_body<'a>(&'a self, v: &SettingsView<'a>) -> Element<'a, Message> {
        let cards = match self.tab {
            Tab::General => general::view(self, v),
            Tab::Appearance => appearance::view(v),
            Tab::Notifications => notifications::view(v),
            Tab::Sidebar => self.sidebar_cards(&v.config.ui, &v.flags, v.star_kept_busy),
            Tab::Mentions => mentions::view(self, v),
            Tab::Privacy => privacy::view(v),
            Tab::About => about::view(v),
        };
        container(Column::with_children(cards).spacing(SPACE_6).max_width(720))
            .center_x(Length::Fill)
            .into()
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::privacy::cache_choices;
    use super::*;
    use rustket_core::config::{GroupType, SectionId};
    use rustket_core::room_list::SectionKind;
    use rustket_core::sidebar_layout::LayoutEdit;

    fn run(s: &mut Settings, cfg: &mut AppConfig, msg: impl Into<Message>) -> Action<Message> {
        let mut ui_state = UiState::default();
        let mut ctx = SettingsCtx {
            config: cfg,
            ui_state: &mut ui_state,
        };
        s.update(msg.into(), &mut ctx)
    }

    #[test]
    fn prefs_change_and_are_saved() {
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        for (msg, check) in [
            (
                Message::SetTheme(ThemePref::Dark),
                (|c: &UiPrefs| c.theme == ThemePref::Dark) as fn(&UiPrefs) -> bool,
            ),
            (Message::SetDensity(Density::Condensed), |c| {
                c.density == Density::Condensed
            }),
            (Message::SetShowAvatars(false), |c| !c.show_avatars),
            (Message::SetUnreadOnTop(true), |c| c.unread_on_top),
            (Message::SetUnreadOnTop(false), |c| !c.unread_on_top),
            (Message::SetClock24(false), |c| !c.clock_24h),
            (Message::SetRealNames(Some(false)), |c| {
                c.use_real_names == Some(false)
            }),
            (Message::SetNotifications(false), |c| {
                !c.desktop_notifications
            }),
            (Message::SetPreview(false), |c| !c.notification_preview),
            (Message::SetSound(false), |c| !c.sound),
            (Message::SetReminderNotifications(false), |c| {
                !c.reminder_notifications
            }),
            (Message::SetReminderSound(false), |c| !c.reminder_sound),
            (Message::SetReminderMorningHour(7), |c| {
                c.reminder_morning_hour == 7
            }),
            (Message::SetCloseToTray(false), |c| !c.close_to_tray),
            (Message::SetMinimizeToTray(true), |c| c.minimize_to_tray),
            (Message::SetStartMinimized(true), |c| c.start_minimized),
            (Message::SetEscapeMarksRead(false), |c| !c.escape_marks_read),
            (Message::SetEscapeMarksRead(true), |c| c.escape_marks_read),
            (Message::SetRoomBookmarks(false), |c| !c.room_bookmarks),
            (Message::SetConfirmBroadMentions(false), |c| {
                !c.confirm_broad_mentions
            }),
            (Message::SetShareUtcOffset(false), |c| !c.share_utc_offset),
            (Message::SetHardwareAcceleration(false), |c| {
                !c.hardware_acceleration
            }),
            (Message::SetHardwareAcceleration(true), |c| {
                c.hardware_acceleration
            }),
            (Message::SetSystemTitleBar(true), |c| c.system_title_bar),
            (Message::SetSystemTitleBar(false), |c| !c.system_title_bar),
            (Message::SetImageCacheMb(1000), |c| c.image_cache_mb == 1000),
            (Message::SetGroupByType(false), |c| !c.group_by_type),
            (Message::SetSidebarSort(SidebarSort::ActivityAsc), |c| {
                c.sidebar_sort == SidebarSort::ActivityAsc
            }),
            (Message::SetThreadsMode(ThreadsMode::Following), |c| {
                c.sidebar_threads == ThreadsMode::Following
            }),
            (Message::SetStarredThreads(false), |c| {
                !c.sidebar_starred_threads
            }),
            (Message::SetPinnedViews(false), |c| !c.sidebar_pinned_views),
            (
                Message::Sidebar(SidebarMessage::Layout(LayoutEdit::SetThreadsLimit(50))),
                |c| c.sidebar_threads_limit == 50,
            ),
            (
                Message::Sidebar(SidebarMessage::Layout(LayoutEdit::SetThreadsLimit(0))),
                |c| c.sidebar_threads_limit == 0,
            ),
            #[cfg(feature = "self-update")]
            (Message::SetCheckUpdates(false), |c| !c.check_updates),
            #[cfg(feature = "self-update")]
            (Message::SetUpdatePrereleases(true), |c| {
                c.update_prereleases
            }),
            (
                Message::Sidebar(SidebarMessage::Layout(LayoutEdit::Move {
                    id: SectionId::Builtin(SectionKind::Channels),
                    delta: -1,
                })),
                |c| {
                    let order = rustket_core::sidebar_layout::effective_order(c);
                    let pos = |k| order.iter().position(|x| x == &SectionId::Builtin(k));
                    pos(SectionKind::Channels) < pos(SectionKind::Discussions)
                },
            ),
        ] {
            let a = run(&mut s, &mut cfg, msg.clone());
            assert!(check(&cfg.ui), "{msg:?}");
            assert!(
                matches!(a.effects.as_slice(), [Effect::SaveConfig]),
                "{msg:?}"
            );
        }
    }

    /// The deletion preset by the sidebar's "Ungroup…" happens only on
    /// confirmation, and drops the group's collapse state.
    #[test]
    fn preset_group_deletion_is_confirmed() {
        use rustket_core::sidebar_layout::{GroupType, LayoutEdit, apply};
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels],
            },
        );
        let gid = cfg.ui.sidebar_groups[0].id.clone();
        s.confirm_delete = Some(gid.clone());
        let a = run(&mut s, &mut cfg, SidebarMessage::GroupDeleteCancel);
        assert!(a.effects.is_empty() && s.confirm_delete.is_none());
        assert_eq!(cfg.ui.sidebar_groups.len(), 1);
        let a = run(&mut s, &mut cfg, SidebarMessage::GroupDeleteConfirm);
        assert!(a.effects.is_empty(), "nothing to confirm");

        s.confirm_delete = Some(gid.clone());
        let mut ui_state = UiState::default();
        let key = format!("sidebar.group.{gid}");
        ui_state.collapsed_sections.insert(key.clone());
        let mut ctx = SettingsCtx {
            config: &mut cfg,
            ui_state: &mut ui_state,
        };
        let a = s.update(SidebarMessage::GroupDeleteConfirm.into(), &mut ctx);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert!(cfg.ui.sidebar_groups.is_empty() && s.confirm_delete.is_none());
        assert!(!ui_state.collapsed_sections.contains(&key));
    }

    /// New → name → types → Save adds the group before its earliest type
    /// section; Save is refused while the draft is invalid; Edit renames and
    /// changes the types; Delete → Confirm removes it and its collapse state.
    #[test]
    fn sidebar_group_flow() {
        use rustket_core::sidebar_layout::effective_order;
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        let mut ui_state = UiState::default();
        let send = |s: &mut Settings, cfg: &mut AppConfig, ui: &mut UiState, m: SidebarMessage| {
            let mut ctx = SettingsCtx {
                config: cfg,
                ui_state: ui,
            };
            s.update(m.into(), &mut ctx)
        };

        send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupNew);
        assert_eq!(s.group_draft, Some(GroupDraft::default()));
        let a = send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupSave);
        assert!(a.effects.is_empty() && s.group_draft.is_some(), "no name");
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupName("  Work ".into()),
        );
        let a = send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupSave);
        assert!(a.effects.is_empty() && s.group_draft.is_some(), "no type");
        assert!(cfg.ui.sidebar_groups.is_empty());
        for ty in [
            GroupType::Channels,
            GroupType::PrivateGroups,
            GroupType::Teams,
        ] {
            send(
                &mut s,
                &mut cfg,
                &mut ui_state,
                SidebarMessage::GroupType(ty, true),
            );
        }
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupType(GroupType::Teams, false),
        );
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupName("x".repeat(100)),
        );
        assert_eq!(
            s.group_draft.as_ref().unwrap().name.chars().count(),
            rustket_core::sidebar_layout::MAX_GROUP_NAME_CHARS
        );
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupName("  Work ".into()),
        );
        let a = send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupSave);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert!(s.group_draft.is_none());
        let g = cfg.ui.sidebar_groups[0].clone();
        assert_eq!(g.name, "Work");
        assert_eq!(g.types, [GroupType::Channels, GroupType::PrivateGroups]);
        let order = effective_order(&cfg.ui);
        let gpos = order.iter().position(|x| x == &SectionId::group(&g.id));
        let cpos = order
            .iter()
            .position(|x| x == &SectionId::Builtin(SectionKind::Channels));
        assert_eq!(gpos.map(|p| p + 1), cpos, "{order:?}");

        // Edit: rename and move the types; an edited group may end up empty.
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupEdit(g.id.clone()),
        );
        let d = s.group_draft.clone().unwrap();
        assert_eq!(d.editing.as_deref(), Some(g.id.as_str()));
        assert_eq!(d.types.len(), 2);
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupName("Job".into()),
        );
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupType(GroupType::Channels, false),
        );
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupType(GroupType::PrivateGroups, false),
        );
        let a = send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupSave);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(cfg.ui.sidebar_groups[0].name, "Job");
        assert!(cfg.ui.sidebar_groups[0].types.is_empty());
        // Saving an unchanged draft writes nothing.
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupEdit(g.id.clone()),
        );
        let a = send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupSave);
        assert!(a.effects.is_empty());
        // Cancel drops the draft.
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupEdit(g.id.clone()),
        );
        send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupName("Nope".into()),
        );
        send(&mut s, &mut cfg, &mut ui_state, SidebarMessage::GroupCancel);
        assert!(s.group_draft.is_none());
        assert_eq!(cfg.ui.sidebar_groups[0].name, "Job");

        // Delete asks first, then removes the group and its collapse state.
        let key = format!("sidebar.group.{}", g.id);
        ui_state.collapsed_sections.insert(key.clone());
        let a = send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupDelete(g.id.clone()),
        );
        assert!(a.effects.is_empty() && cfg.ui.sidebar_groups.len() == 1);
        assert_eq!(s.confirm_delete.as_deref(), Some(g.id.as_str()));
        let a = send(
            &mut s,
            &mut cfg,
            &mut ui_state,
            SidebarMessage::GroupDeleteConfirm,
        );
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert!(cfg.ui.sidebar_groups.is_empty());
        assert!(!effective_order(&cfg.ui).contains(&SectionId::group(&g.id)));
        assert!(!ui_state.collapsed_sections.contains(&key));
    }

    fn flags_of(edits: &[rustket_core::flags::FlagEdit]) -> rustket_core::flags::Flags {
        let mut blob = rustket_core::flags::FlagBlob::default();
        let stored = rustket_core::flags::FlagBlob::default();
        for e in edits {
            blob.edit(&stored, e, 1_000, "dev").unwrap();
        }
        blob.view()
    }

    /// "Move kept threads to Favorites" is offered with the starred-threads
    /// switch on and kept threads to move, and asks first.
    #[test]
    fn star_kept_asks_first() {
        rust_i18n::set_locale("en");
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        assert!(
            run(&mut s, &mut cfg, SidebarMessage::StarKept)
                .effects
                .is_empty()
        );
        assert!(s.confirm_star_kept);
        let flags = flags_of(&[]);
        let account = FlagAccount {
            sid: rustket_core::ServerId("s".into()),
            label: "me on s".into(),
            flags: &flags,
            usage: Default::default(),
            status: crate::workspace::flags::Status::Synced,
            kept_unstarred: 3,
        };
        cfg.ui.sidebar_starred_threads = true;
        let _ = s.sidebar_cards(&cfg.ui, std::slice::from_ref(&account), false);
        let _ = s.sidebar_cards(&cfg.ui, std::slice::from_ref(&account), true);
        run(&mut s, &mut cfg, SidebarMessage::StarKeptCancel);
        assert!(!s.confirm_star_kept);
        run(&mut s, &mut cfg, SidebarMessage::StarKept);
        let a = run(&mut s, &mut cfg, SidebarMessage::StarKeptConfirm);
        assert!(matches!(a.effects.as_slice(), [Effect::StarKeptThreads]));
        assert!(!s.confirm_star_kept);
    }

    /// A flag name is edited in place and stored on Enter or "Save" (blank:
    /// the default name), for its account.
    #[test]
    fn flag_rename_flow() {
        use rustket_core::flags::{FlagColor, FlagEdit};
        rust_i18n::set_locale("en");
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        let sid = rustket_core::ServerId("s".into());
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagNameSave(sid.clone(), FlagColor::Red),
        );
        assert!(a.effects.is_empty(), "nothing edited");
        let long = "x".repeat(60);
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagName(sid.clone(), FlagColor::Red, long),
        );
        assert_eq!(
            s.flag_drafts[&(sid.clone(), FlagColor::Red)]
                .chars()
                .count(),
            rustket_core::flags::MAX_FLAG_NAME_CHARS
        );
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagName(sid.clone(), FlagColor::Red, "  Urgent ".into()),
        );
        let flags = flags_of(&[]);
        let account = FlagAccount {
            sid: sid.clone(),
            label: "me on s".into(),
            flags: &flags,
            usage: Default::default(),
            status: crate::workspace::flags::Status::Pending,
            kept_unstarred: 0,
        };
        let _ = s.sidebar_cards(&cfg.ui, std::slice::from_ref(&account), false);
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagNameSave(sid.clone(), FlagColor::Red),
        );
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::SetFlag(id, FlagEdit::Rename { color: FlagColor::Red, name: Some(n) })]
                if id == &sid && n == "Urgent"
        ));
        assert!(s.flag_drafts.is_empty());
        // Blank: back to the default name.
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagName(sid.clone(), FlagColor::Blue, " ".into()),
        );
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagNameSave(sid.clone(), FlagColor::Blue),
        );
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::SetFlag(
                _,
                FlagEdit::Rename {
                    color: FlagColor::Blue,
                    name: None
                }
            )]
        ));
    }

    /// "Remove from all" asks first, then clears that color for its account.
    #[test]
    fn flag_clear_is_confirmed() {
        use rustket_core::flags::{FlagColor, FlagEdit};
        rust_i18n::set_locale("en");
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        let sid = rustket_core::ServerId("s".into());
        let a = run(&mut s, &mut cfg, SidebarMessage::FlagClearConfirm);
        assert!(a.effects.is_empty(), "not asked yet");
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagClear(sid.clone(), FlagColor::Green),
        );
        assert_eq!(s.confirm_flag_clear, Some((sid.clone(), FlagColor::Green)));
        let flags = flags_of(&[FlagEdit::Room {
            rid: "r".into(),
            color: Some(FlagColor::Green),
        }]);
        let account = FlagAccount {
            sid: sid.clone(),
            label: "me on s".into(),
            flags: &flags,
            usage: [(FlagColor::Green, (1, 2))].into(),
            status: crate::workspace::flags::Status::Failed("offline".into()),
            kept_unstarred: 0,
        };
        let _ = s.sidebar_cards(&cfg.ui, std::slice::from_ref(&account), false);
        run(&mut s, &mut cfg, SidebarMessage::FlagClearCancel);
        assert!(s.confirm_flag_clear.is_none());
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::FlagClear(sid.clone(), FlagColor::Green),
        );
        let a = run(&mut s, &mut cfg, SidebarMessage::FlagClearConfirm);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::SetFlag(id, FlagEdit::ClearColor(FlagColor::Green))] if id == &sid
        ));
        assert!(s.confirm_flag_clear.is_none());
        assert_eq!(
            t!("settings.flag_clear_confirm", name = "Green"),
            "Remove the “Green” flag from all rooms and threads on all your devices?"
        );
    }

    /// Flag sections are listed while the active account uses their color,
    /// under its name; moves skip the unlisted ones.
    #[test]
    fn flag_sections_are_listed_when_used() {
        use rustket_core::flags::{FlagColor, FlagEdit};
        use rustket_core::sidebar_layout::Layout;
        rust_i18n::set_locale("en");
        let cfg = AppConfig::default();
        let layout = Layout::from_prefs(&cfg.ui);
        let flag_ids = |flags: Option<&rustket_core::flags::Flags>| -> Vec<SectionId> {
            sidebar::listed_sections(&layout, flags)
                .into_iter()
                .filter(|id| id.flag().is_some())
                .cloned()
                .collect()
        };
        assert!(flag_ids(None).is_empty());
        let flags = flags_of(&[
            FlagEdit::Room {
                rid: "r".into(),
                color: Some(FlagColor::Blue),
            },
            FlagEdit::Rename {
                color: FlagColor::Blue,
                name: Some("Waiting".into()),
            },
            FlagEdit::Rename {
                color: FlagColor::Red,
                name: Some("Unused".into()),
            },
        ]);
        assert_eq!(flag_ids(Some(&flags)), [SectionId::Flag(FlagColor::Blue)]);
        // Blue sits right after Favorites: its neighbours are listed ones.
        let ids = sidebar::listed_sections(&layout, Some(&flags));
        let i = ids
            .iter()
            .position(|id| **id == SectionId::Flag(FlagColor::Blue))
            .unwrap();
        assert_eq!(*ids[i - 1], SectionId::Builtin(SectionKind::Favorites));
        assert!(ids[i + 1].flag().is_none());
        let s = Settings::with_autostart(None);
        let _ = s.sidebar_cards(&cfg.ui, &[], false);
    }

    /// Reset asks first; it drops groups, order, section prefs and the
    /// groups' collapse state.
    #[test]
    fn sidebar_reset_is_confirmed() {
        use rustket_core::sidebar_layout::apply;
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels],
            },
        );
        apply(
            &mut cfg.ui,
            LayoutEdit::SetSort {
                id: SectionId::Builtin(SectionKind::Teams),
                sort: Some(SidebarSort::Alphabetical),
            },
        );
        let key = format!("sidebar.group.{}", cfg.ui.sidebar_groups[0].id);
        let a = run(&mut s, &mut cfg, SidebarMessage::ResetConfirm);
        assert!(a.effects.is_empty(), "not asked yet");
        assert_eq!(cfg.ui.sidebar_groups.len(), 1);
        run(&mut s, &mut cfg, SidebarMessage::Reset);
        assert!(s.confirm_reset);
        run(&mut s, &mut cfg, SidebarMessage::ResetCancel);
        assert!(!s.confirm_reset && cfg.ui.sidebar_groups.len() == 1);

        run(&mut s, &mut cfg, SidebarMessage::Reset);
        let mut ui_state = UiState::default();
        ui_state.collapsed_sections.insert(key.clone());
        let mut ctx = SettingsCtx {
            config: &mut cfg,
            ui_state: &mut ui_state,
        };
        let a = s.update(SidebarMessage::ResetConfirm.into(), &mut ctx);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert!(!s.confirm_reset);
        assert!(cfg.ui.sidebar_groups.is_empty() && cfg.ui.sidebar_section_prefs.is_empty());
        assert!(!ui_state.collapsed_sections.contains(&key));
    }

    /// Per-section sort and unread first go through the reducer; setting
    /// unread first back to its default stores no override.
    #[test]
    fn section_sort_and_unread_first() {
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        let threads = SectionId::Builtin(SectionKind::Threads);
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::Layout(LayoutEdit::SetSort {
                id: threads.clone(),
                sort: Some(SidebarSort::Alphabetical),
            }),
        );
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(
            cfg.ui.sidebar_section_prefs["threads"].sort,
            Some(SidebarSort::Alphabetical)
        );
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::Layout(LayoutEdit::SetSort {
                id: threads.clone(),
                sort: Some(SidebarSort::Alphabetical),
            }),
        );
        assert!(a.effects.is_empty(), "unchanged");
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::Layout(LayoutEdit::SetSort {
                id: threads.clone(),
                sort: None,
            }),
        );
        assert!(cfg.ui.sidebar_section_prefs.is_empty());

        // Threads default to unread first: off stores `false`, on again
        // drops the override.
        let unread_first = |cfg: &AppConfig, key: &str| {
            cfg.ui
                .sidebar_section_prefs
                .get(key)
                .and_then(|p| p.unread_first)
        };
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::SetUnreadFirst(threads.clone(), false),
        );
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(unread_first(&cfg, "threads"), Some(false));
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::SetUnreadFirst(threads.clone(), true),
        );
        assert_eq!(unread_first(&cfg, "threads"), None);
        assert!(cfg.ui.sidebar_section_prefs.is_empty());
        // Other sections default to off.
        let channels = SectionId::Builtin(SectionKind::Channels);
        run(
            &mut s,
            &mut cfg,
            SidebarMessage::SetUnreadFirst(channels.clone(), true),
        );
        assert_eq!(unread_first(&cfg, "channels"), Some(true));
        let a = run(
            &mut s,
            &mut cfg,
            SidebarMessage::SetUnreadFirst(channels.clone(), false),
        );
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert!(cfg.ui.sidebar_section_prefs.is_empty());
    }

    /// The Sidebar card's captions: claimed built-ins, unused sections,
    /// type-less groups; Unread has no "Unread first".
    #[test]
    fn sidebar_card_captions() {
        use rustket_core::sidebar_layout::{Layout, apply};
        rust_i18n::set_locale("en");
        let mut cfg = AppConfig::default();
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels],
            },
        );
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "Empty".into(),
                types: vec![GroupType::Teams],
            },
        );
        let empty = cfg.ui.sidebar_groups[1].id.clone();
        apply(
            &mut cfg.ui,
            LayoutEdit::SetGroupType {
                id: empty.clone(),
                ty: GroupType::Teams,
                on: false,
            },
        );
        let ui = &cfg.ui;
        let layout = Layout::from_prefs(ui);
        let cap = |id: SectionId| sidebar::section_caption(&id, ui, &layout);
        assert_eq!(
            cap(SectionId::Builtin(SectionKind::Channels)),
            (Some("In “Work”".to_owned()), true)
        );
        assert_eq!(cap(SectionId::Builtin(SectionKind::Teams)), (None, false));
        assert_eq!(
            cap(SectionId::Builtin(SectionKind::Conversations))
                .0
                .as_deref(),
            Some("Used only while grouping by type is off")
        );
        assert_eq!(
            cap(SectionId::group(&empty)).0.as_deref(),
            Some("No room types")
        );
        assert_eq!(
            cap(SectionId::Builtin(SectionKind::Threads)).0.as_deref(),
            Some("Off")
        );
        assert!(!sidebar::has_unread_first(&SectionId::Builtin(
            SectionKind::Unread
        )));
        assert!(sidebar::has_unread_first(&SectionId::Builtin(
            SectionKind::Threads
        )));
        assert!(sidebar::has_unread_first(&SectionId::group(&empty)));
        let s = Settings::with_autostart(None);
        let _ = s.sidebar_cards(ui, &[], false);
    }

    /// Conversations can never have rooms once groups take every room type.
    #[test]
    fn conversations_claimed_when_groups_take_every_type() {
        use rustket_core::sidebar_layout::{Layout, apply};
        rust_i18n::set_locale("en");
        let conv = SectionId::Builtin(SectionKind::Conversations);
        let mut cfg = AppConfig::default();
        cfg.ui.group_by_type = false;
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "All".into(),
                types: GroupType::ALL[..4].to_vec(),
            },
        );
        let cap = |ui: &UiPrefs| sidebar::section_caption(&conv, ui, &Layout::from_prefs(ui));
        assert_eq!(cap(&cfg.ui), (None, false));
        let all = cfg.ui.sidebar_groups[0].id.clone();
        apply(
            &mut cfg.ui,
            LayoutEdit::SetGroupType {
                id: all.clone(),
                ty: GroupType::DirectMessages,
                on: true,
            },
        );
        assert_eq!(cap(&cfg.ui), (Some("In “All”".to_owned()), true));
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "People".into(),
                types: vec![GroupType::DirectMessages],
            },
        );
        assert_eq!(cap(&cfg.ui), (Some("Taken by groups".to_owned()), true));
    }

    /// The group editor names the group a checked type moves from, or the
    /// one holding an unchecked type; the edited group's own types have none.
    #[test]
    fn group_editor_type_captions() {
        use rustket_core::sidebar_layout::{Layout, apply};
        rust_i18n::set_locale("en");
        let mut cfg = AppConfig::default();
        apply(
            &mut cfg.ui,
            LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels, GroupType::Teams],
            },
        );
        let work = cfg.ui.sidebar_groups[0].id.clone();
        let layout = Layout::from_prefs(&cfg.ui);
        let mut d = GroupDraft {
            types: [GroupType::Channels].into(),
            ..GroupDraft::default()
        };
        let cap = |d: &GroupDraft, ty| sidebar::type_caption(&layout, d, ty);
        assert_eq!(
            cap(&d, GroupType::Channels).as_deref(),
            Some("moves from “Work”")
        );
        assert_eq!(cap(&d, GroupType::Teams).as_deref(), Some("In “Work”"));
        assert_eq!(cap(&d, GroupType::DirectMessages), None);
        d.editing = Some(work);
        assert_eq!(cap(&d, GroupType::Channels), None);
        assert_eq!(cap(&d, GroupType::Teams), None);
    }

    /// Storage: the disk cache cap choices keep a value set in the file.
    #[test]
    fn tab_switches_and_scrolls_to_top() {
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        assert_eq!(s.tab, Tab::General);
        let a = run(&mut s, &mut cfg, Message::Tab(Tab::Privacy));
        assert_eq!(s.tab, Tab::Privacy);
        assert!(a.effects.is_empty());
        assert_eq!(a.task.units(), 1, "scrolls the page to its top");
        assert_eq!(
            Settings::with_autostart(None).with_tab(Tab::Sidebar).tab,
            Tab::Sidebar
        );
    }

    #[test]
    fn every_tab_is_translated() {
        let entries = crate::i18n::tests::locale_entries();
        let mut keys = std::collections::BTreeSet::new();
        for tab in Tab::ALL {
            let key = tab.i18n_key();
            assert!(keys.insert(key), "{key} used twice");
            let texts = entries.get(key).unwrap_or_else(|| panic!("{key} missing"));
            for loc in crate::i18n::tests::BUNDLED_COMPLETE {
                assert!(
                    texts.get(*loc).is_some_and(|t| !t.is_empty()),
                    "{key}: missing {loc}"
                );
            }
        }
    }

    #[test]
    fn cache_limit_choices() {
        rust_i18n::set_locale("en");
        let labels = |mb| -> Vec<String> { cache_choices(mb).into_iter().map(|c| c.1).collect() };
        assert_eq!(labels(500), ["100 MB", "250 MB", "500 MB", "1 GB", "2 GB"]);
        assert_eq!(cache_choices(750).len(), 6);
        assert_eq!(cache_choices(750)[3].0, 750);
        assert_eq!(size_label(1500), "1500 MB");
        rust_i18n::set_locale("ru");
        assert_eq!(size_label(2000), "2 ГБ");
        rust_i18n::set_locale("en");
    }

    #[test]
    fn zoom_steps_are_clamped() {
        assert!((zoom_step(1.0, 1) - 1.1).abs() < 1e-4);
        assert!((zoom_step(1.0, -1) - 0.9).abs() < 1e-4);
        assert!((zoom_step(2.0, 1) - 2.1).abs() < 1e-4);
        assert!((zoom_step(2.5, -1) - 2.4).abs() < 1e-4);
        assert!((zoom_step(3.0, 1) - 3.0).abs() < 1e-4);
        assert!((zoom_step(0.5, -1) - 0.5).abs() < 1e-4);
        assert!((zoom_step(1.37, 0) - 1.0).abs() < 1e-4);
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        run(&mut s, &mut cfg, Message::Zoom(1));
        run(&mut s, &mut cfg, Message::Zoom(1));
        assert!((cfg.ui.zoom - 1.2).abs() < 1e-4);
    }

    #[test]
    fn language_is_saved_and_applied() {
        // Only English here: other tests in this binary assert English text
        // and the rust-i18n locale is process-wide.
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        cfg.ui.language = Language::new("ru").unwrap();
        let en = Language::new("en").unwrap();
        let a = run(&mut s, &mut cfg, Message::SetLanguage(en.clone()));
        assert_eq!(cfg.ui.language, en);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(&*rust_i18n::locale(), "en");
        let a = run(&mut s, &mut cfg, Message::ReloadLocales);
        assert!(matches!(a.effects.as_slice(), [Effect::ReloadLocales]));
        let a = run(&mut s, &mut cfg, Message::OpenLocalesFolder);
        assert!(matches!(a.effects.as_slice(), [Effect::OpenLocalesFolder]));
    }

    #[test]
    fn effects_for_actions() {
        let mut s = Settings::with_autostart(None);
        let mut cfg = AppConfig::default();
        let a = run(&mut s, &mut cfg, Message::TestNotification);
        assert!(matches!(a.effects.as_slice(), [Effect::TestNotification]));
        let a = run(&mut s, &mut cfg, Message::ClearImageCache);
        assert!(matches!(a.effects[0], Effect::ClearImageCache));
        let a = run(&mut s, &mut cfg, Message::RegisterHandler);
        assert!(matches!(a.effects.as_slice(), [Effect::RegisterUrlHandler]));
        let a = run(&mut s, &mut cfg, Message::OpenUrl(DOCS.into()));
        assert!(matches!(a.effects.as_slice(), [Effect::OpenUrl(u)] if u == DOCS));
        // No autostart directory: the switch does nothing.
        let a = run(&mut s, &mut cfg, Message::SetAutostart(true));
        assert!(a.effects.is_empty() && !s.autostart_busy);
        #[cfg(feature = "self-update")]
        {
            use crate::updater::app::Msg;
            let a = run(&mut s, &mut cfg, Message::Updater(Msg::Install));
            assert!(matches!(
                a.effects.as_slice(),
                [Effect::Updater(Msg::Install)]
            ));
            // Switching pre-releases off withdraws an offered one.
            let a = run(&mut s, &mut cfg, Message::SetUpdatePrereleases(false));
            assert!(!cfg.ui.update_prereleases);
            assert!(matches!(
                a.effects.as_slice(),
                [Effect::SaveConfig, Effect::Updater(Msg::PrereleasesOff)]
            ));
        }
    }

    #[test]
    fn autostart_toggles_in_a_temporary_config_home() {
        let d = tempfile::tempdir().unwrap();
        let at = Autostart::LaunchAgent {
            home: d.path().to_owned(),
        };
        let mut s = Settings::with_autostart(Some(at.clone()));
        let mut cfg = AppConfig::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let drive = |s: &mut Settings, cfg: &mut AppConfig, task: Task<Message>| {
            let Some(stream) = iced_runtime::task::into_stream(task) else {
                return;
            };
            use futures::StreamExt;
            for a in rt.block_on(stream.collect::<Vec<_>>()) {
                if let iced_runtime::Action::Output(m) = a {
                    run(s, cfg, m);
                }
            }
        };
        let t = s.load();
        drive(&mut s, &mut cfg, t);
        assert_eq!(s.autostart, Some(false));
        let a = run(&mut s, &mut cfg, Message::SetAutostart(true));
        assert!(s.autostart_busy);
        drive(&mut s, &mut cfg, a.task);
        assert_eq!(s.autostart, Some(true));
        assert!(at.is_enabled());
        let a = run(&mut s, &mut cfg, Message::SetAutostart(false));
        drive(&mut s, &mut cfg, a.task);
        assert_eq!(s.autostart, Some(false));
        assert!(!at.path().unwrap().exists());
    }
}
