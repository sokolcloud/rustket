//! Appearance tab: theme, mode, density, zoom, names, avatars, clock,
//! renderer and title bar.

use iced::widget::{button, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::config::{Density, Theme as ThemePref, ZOOM_RANGE};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, TEXT_BODY, TEXT_SUBTITLE};
use rustket_ui::widget::{focus_ring, secondary_button};

use super::pickers::theme_picker;
use super::widgets::{field, section, segmented, switch};
use super::{Message, SettingsView};
use crate::renderer::Renderer;

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let ui = &v.config.ui;
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

    vec![section(
        t!("settings.appearance").into_owned(),
        vec![
            theme_picker(ui, v.themes),
            field(
                t!("settings.mode").into_owned(),
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
    )]
}

fn renderer_label(r: Renderer) -> std::borrow::Cow<'static, str> {
    match r {
        Renderer::Gpu => t!("settings.renderer_gpu"),
        Renderer::Cpu => t!("settings.renderer_cpu"),
    }
}
