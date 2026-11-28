//! About tab: updates (with the `self-update` feature), the version, the
//! developer, support and legal links (`crate::legal`).

use iced::Element;
use iced::widget::{button, row, text};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_4, TEXT_BODY};

use super::widgets::{caption, link, section};
use super::{Message, SettingsView};
use crate::legal;

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let mut cards = Vec::new();
    #[cfg(feature = "self-update")]
    cards.push(updates_card(v));
    #[cfg(not(feature = "self-update"))]
    let _ = v;
    let mut secondary = row![].spacing(SPACE_4);
    if legal::third_party_notices().is_some() {
        secondary = secondary.push(
            button(
                text(t!("legal.third_party").into_owned())
                    .size(TEXT_BODY)
                    .style(style::text_link),
            )
            .padding(0)
            .style(style::button_link)
            .on_press(Message::OpenNotices),
        );
    }
    secondary = secondary.push(link(
        t!("legal.rocket_chat_docs").into_owned(),
        legal::ROCKET_CHAT_DOCS_URL,
    ));
    cards.push(section(
        t!("settings.about").into_owned(),
        vec![
            text(t!("settings.version", version = env!("CARGO_PKG_VERSION")))
                .size(TEXT_BODY)
                .into(),
            caption(t!("legal.by_entity", entity = legal::LEGAL_ENTITY).into_owned()).into(),
            caption(t!("legal.about_text").into_owned()).into(),
            row![
                link(t!("legal.support").into_owned(), legal::SUPPORT_URL),
                link(t!("legal.privacy").into_owned(), legal::PRIVACY_URL),
                link(t!("legal.terms").into_owned(), legal::TERMS_URL),
            ]
            .spacing(SPACE_4)
            .into(),
            secondary.into(),
            caption(t!("legal.disclaimer").into_owned()).into(),
            caption(legal::COPYRIGHT.to_owned()).into(),
        ],
    ));
    cards
}

/// Contact and legal links (Help tab): Rustket's own support, and who to
/// ask about a workspace.
pub(super) fn support_card<'a>() -> Element<'a, Message> {
    section(
        t!("legal.support_title").into_owned(),
        vec![
            caption(t!("legal.support_help").into_owned()).into(),
            row![
                link(t!("legal.support").into_owned(), legal::SUPPORT_URL),
                link(t!("legal.privacy").into_owned(), legal::PRIVACY_URL),
                link(t!("legal.terms").into_owned(), legal::TERMS_URL),
            ]
            .spacing(SPACE_4)
            .into(),
        ],
    )
}

/// The "Updates" card: automatic checks, pre-releases, how this copy is
/// updated, the status and "Check now" (+ "Install", or the release page in
/// the update mode `notify`). In the mode `off` a line says who manages
/// updates and nothing checks.
#[cfg(feature = "self-update")]
fn updates_card<'a>(v: &SettingsView<'a>) -> Element<'a, Message> {
    use super::widgets::switch_with;
    use crate::updater::app::Msg;
    use crate::updater::state::CheckOrigin;
    use iced::widget::column;
    use iced::{Alignment, Length};
    use rustket_core::policy::LockedPref;
    use rustket_ui::theme::{SPACE_1, SPACE_3};
    use rustket_ui::widget::secondary_button;
    let (ui, d) = (&v.config.ui, &v.updater);
    let mut status = column![].spacing(SPACE_1).width(Length::Fill);
    if !d.status.is_empty() {
        status = status.push(text(d.status.clone()).size(TEXT_BODY));
    }
    if let Some(managed) = &d.managed {
        status = status.push(text(managed.clone()).size(TEXT_BODY));
    } else {
        status = status.push(caption(d.last_checked.clone()));
    }
    let mut actions = row![status].spacing(SPACE_3).align_y(Alignment::Center);
    if d.check {
        actions = actions.push(secondary_button(
            t!("updater.check_now"),
            (!d.busy).then_some(Message::Updater(Msg::Check(CheckOrigin::Manual))),
        ));
    }
    if d.release_page {
        actions = actions.push(
            secondary_button(
                t!("updater.release_page"),
                Some(Message::Updater(Msg::OpenReleasePage)),
            )
            .icon(rustket_ui::icon::Icon::ExternalLink),
        );
    }
    if let Some(label) = &d.install {
        actions = actions.push(rustket_ui::widget::primary_button(
            label.clone(),
            Some(Message::Updater(Msg::Install)),
        ));
    }
    section(
        t!("updater.title").into_owned(),
        vec![
            switch_with(
                t!("updater.auto_check").into_owned(),
                Some(t!("updater.auto_check_help").into_owned()),
                ui.check_updates,
                Some(Message::SetCheckUpdates),
                v.managed(LockedPref::CheckUpdates),
            ),
            switch_with(
                t!("updater.prereleases").into_owned(),
                Some(t!("updater.prereleases_help").into_owned()),
                ui.update_prereleases,
                Some(Message::SetUpdatePrereleases),
                v.managed(LockedPref::UpdatePrereleases),
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
