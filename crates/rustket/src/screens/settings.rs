//! App preferences (local, not per server): appearance, language,
//! notifications, window behaviour, integration, storage, about. Owner:
//! desktop agent. Strings: `locales/settings.yml` (`settings.*`).

use iced::widget::{button, column, container, row, text, toggler};
use iced::{Alignment, Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::config::{
    AppConfig, Density, Language, SidebarSort, Theme as ThemePref, ThreadsMode, UiPrefs, ZOOM_RANGE,
};
use rustket_core::secrets::SecretBackend;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_CAPTION,
    TEXT_SUBTITLE,
};
use rustket_ui::widget::{BannerKind, banner, divider, focus_ring, secondary_button};

use crate::component::{Action, Effect};
use crate::desktop::TraySupport;
use crate::desktop::autostart::{self, Autostart};
use crate::renderer::Renderer;
use crate::ui_state::UiState;

mod sidebar;

pub use sidebar::{GroupDraft, Message as SidebarMessage};

/// The Sidebar card (scrolled to by the sidebar's "Sidebar settings…").
pub const SIDEBAR_CARD: &str = "settings-sidebar";
/// The Storage card.
pub const STORAGE_CARD: &str = "settings-storage";

/// Container id of a group's row in the Sidebar card's section list.
pub fn sidebar_group_row_id(group: &str) -> String {
    format!("settings-sidebar-group-{group}")
}

fn reveal_container<M: Send + 'static>(id: iced::widget::Id) -> Task<M> {
    crate::room::timeline::list::reveal(iced::widget::Id::new(super::page::SCROLL_ID), id, false)
}

/// Scrolls the open settings page to the Sidebar card.
pub fn reveal_sidebar<M: Send + 'static>() -> Task<M> {
    reveal_container(iced::widget::Id::new(SIDEBAR_CARD))
}

/// Scrolls the open settings page to a group's row in the Sidebar card
/// (where its deletion preset by "Ungroup…" is confirmed).
pub fn reveal_sidebar_group<M: Send + 'static>(group: &str) -> Task<M> {
    reveal_container(iced::widget::Id::from(sidebar_group_row_id(group)))
}

/// Scrolls the open settings page to the Storage card.
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
    SetDensity(Density),
    /// Zoom in (`1`), out (`-1`) or reset (`0`).
    Zoom(i8),
    SetShowAvatars(bool),
    SetUnreadOnTop(bool),
    SetClock24(bool),
    /// `None`: the server's `UI_Use_Real_Name`.
    SetRealNames(Option<bool>),
    SetLanguage(Language),
    SetNotifications(bool),
    SetPreview(bool),
    SetSound(bool),
    TestNotification,
    SetCloseToTray(bool),
    SetMinimizeToTray(bool),
    SetStartMinimized(bool),
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
    // Sidebar card (`settings/sidebar.rs`).
    SetGroupByType(bool),
    SetSidebarSort(SidebarSort),
    SetThreadsMode(ThreadsMode),
    /// The section list, the group editor and the layout reset.
    Sidebar(SidebarMessage),
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
    pub handler_registered: bool,
    pub secret_backend: Option<SecretBackend>,
    /// Secrets are kept in memory because nothing else is usable (not in tests).
    pub secrets_in_memory: bool,
    /// Whether a tray exists and the window can be hidden to it.
    pub tray: TraySupport,
    /// Offline cache (`crate::cache`): `Some((bytes on disk, clearing))`
    /// when it is enabled.
    pub offline_cache: Option<(Option<u64>, bool)>,
    #[cfg(feature = "self-update")]
    pub updater: crate::updater::view::SettingsData,
}

#[derive(Debug)]
pub struct Settings {
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
            autostart_at: at,
            autostart: None,
            autostart_busy: false,
            confirm_delete: None,
            group_draft: None,
            confirm_reset: false,
        }
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
            Message::SetDensity(v) => Self::pref(ctx, |p| p.density = v),
            Message::Zoom(dir) => Self::pref(ctx, |p| p.zoom = zoom_step(p.zoom, dir)),
            Message::SetShowAvatars(v) => Self::pref(ctx, |p| p.show_avatars = v),
            Message::SetUnreadOnTop(v) => Self::pref(ctx, |p| p.unread_on_top = v),
            Message::SetClock24(v) => Self::pref(ctx, |p| p.clock_24h = v),
            Message::SetRealNames(v) => Self::pref(ctx, |p| p.use_real_names = v),
            Message::SetLanguage(l) => {
                crate::i18n::apply(l);
                Self::pref(ctx, |p| p.language = l)
            }
            Message::SetNotifications(v) => Self::pref(ctx, |p| p.desktop_notifications = v),
            Message::SetPreview(v) => Self::pref(ctx, |p| p.notification_preview = v),
            Message::SetSound(v) => Self::pref(ctx, |p| p.sound = v),
            Message::TestNotification => Action::effect(Effect::TestNotification),
            Message::SetCloseToTray(v) => Self::pref(ctx, |p| p.close_to_tray = v),
            Message::SetMinimizeToTray(v) => Self::pref(ctx, |p| p.minimize_to_tray = v),
            Message::SetStartMinimized(v) => Self::pref(ctx, |p| p.start_minimized = v),
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
            Message::OpenUrl(url) => Action::effect(Effect::OpenUrl(url)),
            Message::Close => Action::effect(Effect::Close),
            Message::SetGroupByType(v) => Self::pref(ctx, |p| p.group_by_type = v),
            Message::SetSidebarSort(s) => Self::pref(ctx, |p| p.sidebar_sort = s),
            Message::SetThreadsMode(m) => Self::pref(ctx, |p| p.sidebar_threads = m),
            Message::Sidebar(m) => self.update_sidebar(m, ctx),
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

    pub fn view<'a>(&'a self, v: &SettingsView<'a>) -> Element<'a, Message> {
        let ui = &v.config.ui;
        let mut body = column![].spacing(SPACE_6).max_width(720);
        match v.secret_backend {
            Some(SecretBackend::File) => {
                body = body.push(banner(BannerKind::Warning, t!("app.keyring_file"), None));
            }
            Some(SecretBackend::Memory) if v.secrets_in_memory => {
                body = body.push(banner(
                    BannerKind::Warning,
                    t!("settings.secrets_memory"),
                    None,
                ));
            }
            _ => {}
        }

        let step = |glyph: &'static str, msg: Option<Message>| {
            let b = button(
                text(glyph)
                    .size(TEXT_SUBTITLE)
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
            )
            .width(36)
            .padding([2, 0])
            .style(style::button_secondary)
            .on_press_maybe(msg.clone());
            focus_ring(b, msg)
        };
        let zoom = row![
            step(
                "\u{2212}",
                (ui.zoom > *ZOOM_RANGE.start() + 0.01).then_some(Message::Zoom(-1))
            ),
            text(format!("{:.0} %", ui.zoom * 100.0))
                .size(TEXT_BODY)
                .width(64)
                .align_x(Alignment::Center),
            step(
                "+",
                (ui.zoom < *ZOOM_RANGE.end() - 0.01).then_some(Message::Zoom(1))
            ),
            secondary_button(
                t!("settings.zoom_reset"),
                ((ui.zoom - 1.0).abs() > 0.01).then_some(Message::Zoom(0))
            ),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);

        body = body.push(section(
            t!("settings.appearance").into_owned(),
            vec![
                field(
                    t!("settings.theme").into_owned(),
                    None,
                    segmented(
                        &ui.theme,
                        vec![
                            (ThemePref::System, t!("settings.theme_system").into_owned()),
                            (ThemePref::Light, t!("settings.theme_light").into_owned()),
                            (ThemePref::Dark, t!("settings.theme_dark").into_owned()),
                        ],
                        Message::SetTheme,
                    ),
                ),
                field(
                    t!("settings.density").into_owned(),
                    None,
                    segmented(
                        &ui.density,
                        vec![
                            (
                                Density::Extended,
                                t!("settings.density_extended").into_owned(),
                            ),
                            (
                                Density::Condensed,
                                t!("settings.density_condensed").into_owned(),
                            ),
                        ],
                        Message::SetDensity,
                    ),
                ),
                field(t!("settings.zoom").into_owned(), None, zoom.into()),
                field(
                    t!("settings.names").into_owned(),
                    Some(t!("settings.names_help").into_owned()),
                    segmented(
                        &ui.use_real_names,
                        vec![
                            (None, t!("settings.names_server").into_owned()),
                            (Some(true), t!("settings.names_real").into_owned()),
                            (Some(false), t!("settings.names_usernames").into_owned()),
                        ],
                        Message::SetRealNames,
                    ),
                ),
                switch(
                    t!("settings.show_avatars").into_owned(),
                    None,
                    ui.show_avatars,
                    Some(Message::SetShowAvatars),
                ),
                switch(
                    t!("settings.clock_24h").into_owned(),
                    None,
                    ui.clock_24h,
                    Some(Message::SetClock24),
                ),
                switch(
                    t!("settings.hardware_acceleration").into_owned(),
                    Some(format!(
                        "{}\n{}",
                        t!("settings.hardware_acceleration_help"),
                        t!(
                            "settings.renderer_now",
                            renderer = renderer_label(crate::renderer::active())
                        )
                    )),
                    ui.hardware_acceleration,
                    Some(Message::SetHardwareAcceleration),
                ),
                switch(
                    t!("titlebar.setting").into_owned(),
                    Some(t!("titlebar.setting_help").into_owned()),
                    ui.system_title_bar,
                    Some(Message::SetSystemTitleBar),
                ),
            ],
        ));

        body = body.push(self.sidebar_card(ui));

        body = body.push(section(
            t!("settings.language").into_owned(),
            vec![segmented(
                &ui.language,
                vec![
                    (
                        Language::System,
                        t!("settings.language_system").into_owned(),
                    ),
                    (Language::En, "English".to_owned()),
                    (Language::Ru, "Русский".to_owned()),
                ],
                Message::SetLanguage,
            )],
        ));

        let notify_on = ui.desktop_notifications;
        body = body.push(section(
            t!("settings.notifications").into_owned(),
            vec![
                switch(
                    t!("settings.notifications_enable").into_owned(),
                    Some(t!("settings.notifications_help").into_owned()),
                    notify_on,
                    Some(Message::SetNotifications),
                ),
                switch(
                    t!("settings.notifications_preview").into_owned(),
                    None,
                    ui.notification_preview,
                    notify_on.then_some(Message::SetPreview),
                ),
                switch(
                    t!("settings.notifications_sound").into_owned(),
                    None,
                    ui.sound,
                    notify_on.then_some(Message::SetSound),
                ),
                row![secondary_button(
                    t!("settings.notifications_test"),
                    notify_on.then_some(Message::TestNotification)
                )]
                .into(),
            ],
        ));

        let autostart: Element<'a, Message> = match self.autostart {
            Some(on) => switch(
                t!("settings.autostart").into_owned(),
                None,
                on,
                (!self.autostart_busy && self.autostart_at.is_some())
                    .then_some(Message::SetAutostart),
            ),
            // Still loading, or no config directory at all.
            None => switch(
                t!("settings.autostart").into_owned(),
                Some(if self.autostart_at.is_none() {
                    t!("settings.autostart_unknown").into_owned()
                } else {
                    t!("settings.autostart_loading").into_owned()
                }),
                false,
                None,
            ),
        };
        let tray = v.tray;
        let close_help = if !tray.available {
            t!("settings.no_tray")
        } else if !tray.can_hide {
            t!("settings.tray_help_wayland")
        } else {
            t!("settings.tray_help")
        };
        body = body.push(section(
            t!("settings.behaviour").into_owned(),
            vec![
                switch(
                    t!("settings.close_to_tray").into_owned(),
                    Some(close_help.into_owned()),
                    ui.close_to_tray && tray.available,
                    tray.available.then_some(Message::SetCloseToTray),
                ),
                switch(
                    t!("settings.minimize_to_tray").into_owned(),
                    (tray.available && !tray.can_hide)
                        .then(|| t!("settings.minimize_wayland").into_owned()),
                    ui.minimize_to_tray && tray.can_hide,
                    (tray.available && tray.can_hide).then_some(Message::SetMinimizeToTray),
                ),
                switch(
                    t!("settings.start_minimized").into_owned(),
                    None,
                    ui.start_minimized,
                    Some(Message::SetStartMinimized),
                ),
                autostart,
            ],
        ));

        // macOS: the app bundle declares the scheme; nothing to register.
        let links = if crate::url_handler::MANAGED_BY_BUNDLE {
            column![
                text(t!("settings.links")).size(TEXT_BODY),
                text(t!("settings.handler_bundle"))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            ]
            .spacing(SPACE_1)
            .width(Length::Fill)
            .into()
        } else {
            row![
                column![
                    text(t!("settings.links")).size(TEXT_BODY),
                    text(if v.handler_registered {
                        t!("settings.handler_on")
                    } else {
                        t!("settings.handler_off")
                    })
                    .size(TEXT_CAPTION)
                    .style(if v.handler_registered {
                        style::text_success
                    } else {
                        style::text_secondary
                    }),
                ]
                .spacing(SPACE_1)
                .width(Length::Fill),
                secondary_button(
                    t!("settings.handler_register"),
                    Some(Message::RegisterHandler)
                ),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .into()
        };
        body = body.push(section(
            t!("settings.integration").into_owned(),
            vec![links],
        ));

        let with_cache_row = |rows| {
            crate::cache::view::with_storage_row(v.offline_cache, Message::ClearOfflineCache, rows)
        };
        body = body.push(
            container(section(
                t!("settings.storage").into_owned(),
                with_cache_row(vec![
                    row![
                        column![
                            text(t!("settings.image_cache")).size(TEXT_BODY),
                            text(t!("settings.image_cache_help"))
                                .size(TEXT_CAPTION)
                                .style(style::text_secondary),
                        ]
                        .spacing(SPACE_1)
                        .width(Length::Fill),
                        secondary_button(
                            t!("settings.clear_cache"),
                            Some(Message::ClearImageCache)
                        ),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center)
                    .into(),
                    field(
                        t!("settings.cache_limit").into_owned(),
                        Some(cache_limit_help(ui.image_cache_mb)),
                        segmented(
                            &ui.image_cache_mb,
                            cache_choices(ui.image_cache_mb),
                            Message::SetImageCacheMb,
                        ),
                    ),
                ]),
            ))
            .id(STORAGE_CARD),
        );

        #[cfg(feature = "self-update")]
        {
            body = body.push(updates_card(ui, &v.updater));
        }

        body = body.push(section(
            t!("settings.about").into_owned(),
            vec![
                text(t!("settings.version", version = env!("CARGO_PKG_VERSION")))
                    .size(TEXT_BODY)
                    .into(),
                text(t!("settings.about_text"))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .into(),
                row![
                    link(t!("settings.website").into_owned(), WEBSITE),
                    link(t!("settings.docs").into_owned(), DOCS),
                ]
                .spacing(SPACE_4)
                .into(),
            ],
        ));

        super::page::page(
            t!("settings.title").into_owned(),
            container(body).center_x(Length::Fill).into(),
            Message::Close,
        )
    }
}

/// The "Updates" card: automatic checks, pre-releases, how this copy is
/// updated, the status and "Check now" (+ "Install").
#[cfg(feature = "self-update")]
fn updates_card<'a>(ui: &UiPrefs, d: &crate::updater::view::SettingsData) -> Element<'a, Message> {
    use crate::updater::app::Msg;
    use crate::updater::state::CheckOrigin;
    let mut status = column![].spacing(SPACE_1).width(Length::Fill);
    if !d.status.is_empty() {
        status = status.push(text(d.status.clone()).size(TEXT_BODY));
    }
    status = status.push(
        text(d.last_checked.clone())
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
    );
    let mut actions = row![
        status,
        secondary_button(
            t!("updater.check_now"),
            (!d.busy).then_some(Message::Updater(Msg::Check(CheckOrigin::Manual))),
        )
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center);
    if let Some(label) = &d.install {
        actions = actions.push(rustket_ui::widget::primary_button(
            label.clone(),
            Some(Message::Updater(Msg::Install)),
        ));
    }
    section(
        t!("updater.title").into_owned(),
        vec![
            switch(
                t!("updater.auto_check").into_owned(),
                Some(t!("updater.auto_check_help").into_owned()),
                ui.check_updates,
                Some(Message::SetCheckUpdates),
            ),
            switch(
                t!("updater.prereleases").into_owned(),
                Some(t!("updater.prereleases_help").into_owned()),
                ui.update_prereleases,
                Some(Message::SetUpdatePrereleases),
            ),
            column![
                text(t!("updater.method")).size(TEXT_BODY),
                text(d.method.clone())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
            ]
            .spacing(SPACE_1)
            .into(),
            actions.into(),
        ],
    )
}

/// A titled card with rows separated by dividers.
fn section<'a>(title: String, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![].spacing(SPACE_3);
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        col = col.push(r);
        if i + 1 < n {
            col = col.push(divider());
        }
    }
    column![
        text(title).size(TEXT_SUBTITLE).font(FONT_SEMIBOLD),
        container(col)
            .padding(SPACE_4)
            .width(Length::Fill)
            .style(style::container_card),
    ]
    .spacing(SPACE_2)
    .into()
}

/// Label (+ help) above a control.
fn field<'a>(
    label: String,
    help: Option<String>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut col = column![text(label).size(TEXT_BODY)].spacing(SPACE_1);
    if let Some(h) = help {
        col = col.push(text(h).size(TEXT_CAPTION).style(style::text_secondary));
    }
    column![col, control].spacing(SPACE_2).into()
}

fn renderer_label(r: Renderer) -> std::borrow::Cow<'static, str> {
    match r {
        Renderer::Gpu => t!("settings.renderer_gpu"),
        Renderer::Cpu => t!("settings.renderer_cpu"),
    }
}

/// Label (+ help) with a switch on the right.
fn switch<'a>(
    label: String,
    help: Option<String>,
    value: bool,
    on_toggle: Option<fn(bool) -> Message>,
) -> Element<'a, Message> {
    let mut col = column![text(label).size(TEXT_BODY)].spacing(SPACE_1);
    if let Some(h) = help {
        col = col.push(text(h).size(TEXT_CAPTION).style(style::text_secondary));
    }
    row![
        col.width(Length::Fill),
        focus_ring(
            toggler(value)
                .size(20)
                .style(style::toggler)
                .on_toggle_maybe(on_toggle),
            on_toggle.map(|f| f(!value)),
        )
        .radius(rustket_ui::theme::RADIUS_PILL),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center)
    .into()
}

/// "100 MB", "1 GB" … (decimal units, like the cap: [`crate::images::MB`]).
pub fn size_label(mb: u64) -> String {
    if mb >= 1000 && mb.is_multiple_of(1000) {
        t!("settings.size_gb", n = mb / 1000).into_owned()
    } else {
        t!("settings.size_mb", n = mb).into_owned()
    }
}

/// The cap choices, with a configured value outside them kept as its own.
fn cache_choices(current: u32) -> Vec<(u32, String)> {
    let mut v: Vec<u32> = IMAGE_CACHE_CHOICES.to_vec();
    if !v.contains(&current) {
        v.push(current);
        v.sort_unstable();
    }
    v.into_iter()
        .map(|mb| (mb, size_label(u64::from(mb))))
        .collect()
}

fn cache_limit_help(cap_mb: u32) -> String {
    let help = t!("settings.cache_limit_help").into_owned();
    match crate::images::disk_usage() {
        Some(bytes) => format!(
            "{help}\n{}",
            t!(
                "settings.cache_used",
                used = size_label(bytes.div_ceil(crate::images::MB)),
                limit = size_label(u64::from(cap_mb))
            )
        ),
        None => help,
    }
}

/// A row of mutually exclusive buttons.
fn segmented<'a, T: Clone + PartialEq + 'a>(
    current: &T,
    options: Vec<(T, String)>,
    to_msg: fn(T) -> Message,
) -> Element<'a, Message> {
    let mut r = row![].spacing(SPACE_2);
    for (value, label) in options {
        let selected = *current == value;
        let msg = to_msg(value);
        r = r.push(focus_ring(
            button(text(label).size(TEXT_BODY))
                .padding([6, 12])
                .style(if selected {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(msg.clone()),
            Some(msg),
        ));
    }
    r.into()
}

fn link<'a>(label: String, url: &'static str) -> Element<'a, Message> {
    let msg = Message::OpenUrl(url.to_owned());
    focus_ring(
        button(text(label).size(TEXT_BODY).style(style::text_link))
            .padding(0)
            .style(style::button_link)
            .on_press(msg.clone()),
        Some(msg),
    )
    .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
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
            (Message::SetCloseToTray(false), |c| !c.close_to_tray),
            (Message::SetMinimizeToTray(true), |c| c.minimize_to_tray),
            (Message::SetStartMinimized(true), |c| c.start_minimized),
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
        let _ = s.sidebar_card(ui);
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
        cfg.ui.language = Language::Ru;
        let a = run(&mut s, &mut cfg, Message::SetLanguage(Language::En));
        assert_eq!(cfg.ui.language, Language::En);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(&*rust_i18n::locale(), "en");
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
