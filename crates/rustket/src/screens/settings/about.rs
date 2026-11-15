//! About tab: updates (with the `self-update` feature) and the version.

use iced::Element;
use iced::widget::{row, text};
use rust_i18n::t;
use rustket_ui::theme::{SPACE_4, TEXT_BODY};

use super::widgets::{caption, link, section};
use super::{DOCS, Message, SettingsView, WEBSITE};

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let mut cards = Vec::new();
    #[cfg(feature = "self-update")]
    cards.push(updates_card(&v.config.ui, &v.updater));
    #[cfg(not(feature = "self-update"))]
    let _ = v;
    cards.push(section(
        t!("settings.about").into_owned(),
        vec![
            text(t!("settings.version", version = env!("CARGO_PKG_VERSION")))
                .size(TEXT_BODY)
                .into(),
            caption(t!("settings.about_text").into_owned()).into(),
            row![
                link(t!("settings.website").into_owned(), WEBSITE),
                link(t!("settings.docs").into_owned(), DOCS),
            ]
            .spacing(SPACE_4)
            .into(),
        ],
    ));
    cards
}

/// The "Updates" card: automatic checks, pre-releases, how this copy is
/// updated, the status and "Check now" (+ "Install").
#[cfg(feature = "self-update")]
fn updates_card<'a>(
    ui: &rustket_core::config::UiPrefs,
    d: &crate::updater::view::SettingsData,
) -> Element<'a, Message> {
    use super::widgets::switch;
    use crate::updater::app::Msg;
    use crate::updater::state::CheckOrigin;
    use iced::widget::column;
    use iced::{Alignment, Length};
    use rustket_ui::theme::{SPACE_1, SPACE_3};
    use rustket_ui::widget::secondary_button;
    let mut status = column![].spacing(SPACE_1).width(Length::Fill);
    if !d.status.is_empty() {
        status = status.push(text(d.status.clone()).size(TEXT_BODY));
    }
    status = status.push(caption(d.last_checked.clone()));
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
                caption(d.method.clone()),
            ]
            .spacing(SPACE_1)
            .into(),
            actions.into(),
        ],
    )
}
