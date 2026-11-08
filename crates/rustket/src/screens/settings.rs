//! App preferences (local, not per server): appearance, language,
//! notifications, window behaviour, integration, storage, about. Owner:
//! desktop agent. Strings: `locales/settings.yml` (`settings.*`).

use std::path::PathBuf;

use iced::widget::{button, column, container, row, text, toggler};
use iced::{Alignment, Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::config::{AppConfig, Density, Language, Theme as ThemePref, UiPrefs, ZOOM_RANGE};
use rustket_core::secrets::SecretBackend;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_CAPTION,
    TEXT_SUBTITLE,
};
use rustket_ui::widget::{BannerKind, banner, divider, focus_ring, secondary_button};

use crate::component::{Action, Effect};
use crate::desktop::{TraySupport, autostart};
use crate::renderer::Renderer;
use crate::ui_state::UiState;

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
    SetAutostart(bool),
    /// Whether the autostart entry exists (`None`: unknown).
    AutostartLoaded(Option<bool>),
    AutostartSet(bool, Result<(), String>),
    RegisterHandler,
    ClearImageCache,
    /// Disk cache cap in MiB (`UiPrefs::image_cache_mb`).
    SetImageCacheMb(u32),
    OpenUrl(String),
    Close,
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
}

#[derive(Debug)]
pub struct Settings {
    /// `$XDG_CONFIG_HOME` holding `autostart/` (a temporary dir in tests).
    autostart_home: Option<PathBuf>,
    /// Launch at login (`None` until read).
    pub autostart: Option<bool>,
    pub autostart_busy: bool,
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
        Self::with_autostart_home(autostart::config_home())
    }

    pub fn with_autostart_home(home: Option<PathBuf>) -> Self {
        Self {
            autostart_home: home,
            autostart: None,
            autostart_busy: false,
        }
    }

    /// Reads the launch-at-login state (run when the screen opens).
    pub fn load(&self) -> Task<Message> {
        let Some(home) = self.autostart_home.clone() else {
            return Task::done(Message::AutostartLoaded(None));
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || autostart::is_enabled(&home))
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
            Message::SetAutostart(on) => {
                let Some(home) = self.autostart_home.clone() else {
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
                            autostart::set_enabled(&home, on, &exe).map_err(|e| e.to_string())
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
            Message::OpenUrl(url) => Action::effect(Effect::OpenUrl(url)),
            Message::Close => Action::effect(Effect::Close),
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
            ],
        ));

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
                (!self.autostart_busy && self.autostart_home.is_some())
                    .then_some(Message::SetAutostart),
            ),
            // Still loading, or no config directory at all.
            None => switch(
                t!("settings.autostart").into_owned(),
                Some(if self.autostart_home.is_none() {
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

        body = body.push(section(
            t!("settings.integration").into_owned(),
            vec![
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
                .into(),
            ],
        ));

        body = body.push(section(
            t!("settings.storage").into_owned(),
            vec![
                row![
                    column![
                        text(t!("settings.image_cache")).size(TEXT_BODY),
                        text(t!("settings.image_cache_help"))
                            .size(TEXT_CAPTION)
                            .style(style::text_secondary),
                    ]
                    .spacing(SPACE_1)
                    .width(Length::Fill),
                    secondary_button(t!("settings.clear_cache"), Some(Message::ClearImageCache)),
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
            ],
        ));

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

/// "100 MB", "1 GB" …
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
                used = size_label(bytes.div_ceil(1024 * 1024)),
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

    fn run(s: &mut Settings, cfg: &mut AppConfig, msg: Message) -> Action<Message> {
        let mut ui_state = UiState::default();
        let mut ctx = SettingsCtx {
            config: cfg,
            ui_state: &mut ui_state,
        };
        s.update(msg, &mut ctx)
    }

    #[test]
    fn prefs_change_and_are_saved() {
        let mut s = Settings::with_autostart_home(None);
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
            (Message::SetImageCacheMb(1000), |c| c.image_cache_mb == 1000),
        ] {
            let a = run(&mut s, &mut cfg, msg.clone());
            assert!(check(&cfg.ui), "{msg:?}");
            assert!(
                matches!(a.effects.as_slice(), [Effect::SaveConfig]),
                "{msg:?}"
            );
        }
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
        let mut s = Settings::with_autostart_home(None);
        let mut cfg = AppConfig::default();
        run(&mut s, &mut cfg, Message::Zoom(1));
        run(&mut s, &mut cfg, Message::Zoom(1));
        assert!((cfg.ui.zoom - 1.2).abs() < 1e-4);
    }

    #[test]
    fn language_is_saved_and_applied() {
        // Only English here: other tests in this binary assert English text
        // and the rust-i18n locale is process-wide.
        let mut s = Settings::with_autostart_home(None);
        let mut cfg = AppConfig::default();
        cfg.ui.language = Language::Ru;
        let a = run(&mut s, &mut cfg, Message::SetLanguage(Language::En));
        assert_eq!(cfg.ui.language, Language::En);
        assert!(matches!(a.effects.as_slice(), [Effect::SaveConfig]));
        assert_eq!(&*rust_i18n::locale(), "en");
    }

    #[test]
    fn effects_for_actions() {
        let mut s = Settings::with_autostart_home(None);
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
    }

    #[test]
    fn autostart_toggles_in_a_temporary_config_home() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Settings::with_autostart_home(Some(d.path().to_owned()));
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
        assert!(autostart::is_enabled(d.path()));
        let a = run(&mut s, &mut cfg, Message::SetAutostart(false));
        drive(&mut s, &mut cfg, a.task);
        assert_eq!(s.autostart, Some(false));
        assert!(!autostart::path(d.path()).exists());
    }
}
