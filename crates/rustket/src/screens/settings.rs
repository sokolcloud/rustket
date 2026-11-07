//! App preferences. Owner: desktop agent. Strings: `locales/settings.yml`.
//! STUB: theme, language and the `rocketchat://` handler entry; notification,
//! tray, autostart, proxy, zoom … preferences are to be added.

use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Length, Subscription};
use rust_i18n::t;
use rustket_core::config::{AppConfig, Language, Theme as ThemePref};
use rustket_core::secrets::SecretBackend;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, banner, secondary_button};

use crate::component::{Action, Effect};
use crate::ui_state::UiState;

#[derive(Debug, Clone)]
pub enum Message {
    SetTheme(ThemePref),
    SetLanguage(Language),
    RegisterHandler,
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
}

#[derive(Debug, Default)]
pub struct Settings;

impl Settings {
    pub fn new() -> Self {
        Self
    }

    pub fn update(&mut self, msg: Message, ctx: &mut SettingsCtx<'_>) -> Action<Message> {
        match msg {
            Message::SetTheme(t) => {
                ctx.config.ui.theme = t;
                Action::effect(Effect::SaveConfig)
            }
            Message::SetLanguage(l) => {
                ctx.config.ui.language = l;
                crate::i18n::apply(l);
                Action::effect(Effect::SaveConfig)
            }
            Message::RegisterHandler => Action::effect(Effect::RegisterUrlHandler),
            Message::Close => Action::effect(Effect::Close),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, v: &SettingsView<'a>) -> Element<'a, Message> {
        fn choice<'a, T: Clone + PartialEq + 'a>(
            label: String,
            current: &T,
            value: T,
            to_msg: fn(T) -> Message,
        ) -> Element<'a, Message> {
            let selected = current == &value;
            button(text(label).size(TEXT_BODY))
                .padding([6, 12])
                .style(if selected {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(to_msg(value))
                .into()
        }
        let ui = &v.config.ui;
        let mut body = column![
            text(t!("settings.theme"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            row![
                choice(
                    t!("settings.theme_system").into_owned(),
                    &ui.theme,
                    ThemePref::System,
                    Message::SetTheme
                ),
                choice(
                    t!("settings.theme_light").into_owned(),
                    &ui.theme,
                    ThemePref::Light,
                    Message::SetTheme
                ),
                choice(
                    t!("settings.theme_dark").into_owned(),
                    &ui.theme,
                    ThemePref::Dark,
                    Message::SetTheme
                ),
            ]
            .spacing(SPACE_2),
            text(t!("settings.language"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            row![
                choice(
                    t!("settings.language_system").into_owned(),
                    &ui.language,
                    Language::System,
                    Message::SetLanguage
                ),
                choice(
                    "English".to_owned(),
                    &ui.language,
                    Language::En,
                    Message::SetLanguage
                ),
                choice(
                    "Русский".to_owned(),
                    &ui.language,
                    Language::Ru,
                    Message::SetLanguage
                ),
            ]
            .spacing(SPACE_2),
            text(t!("settings.links"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            row![
                text(if v.handler_registered {
                    t!("settings.handler_on")
                } else {
                    t!("settings.handler_off")
                })
                .size(TEXT_BODY)
                .width(Length::Fill),
                secondary_button(
                    t!("settings.handler_register"),
                    Some(Message::RegisterHandler)
                ),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        ]
        .spacing(SPACE_2)
        .max_width(640);
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
        body = body.push(super::page::placeholder(
            t!("settings.more_soon").into_owned(),
        ));
        super::page::page(
            t!("settings.title").into_owned(),
            body.into(),
            Message::Close,
        )
    }
}
