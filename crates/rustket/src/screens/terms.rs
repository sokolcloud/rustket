//! The terms of use screen: shown before the first server is added and
//! again whenever `crate::legal::TERMS_VERSION` is bumped. It covers the
//! whole window until the user agrees (or quits).

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_TITLE, font_semibold};
use rustket_ui::widget::{primary_button, secondary_button};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Accept,
    /// Open the full terms (hosted page) in the browser.
    OpenTerms,
    OpenPrivacy,
    Quit,
}

fn point<'a>(i: Icon, body: String) -> Element<'a, Message> {
    row![
        icon::sized(i, 18.0).style(style::text_secondary),
        text(body).size(TEXT_BODY).width(Length::Fill),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Start)
    .into()
}

fn link<'a>(label: String, msg: Message) -> Element<'a, Message> {
    iced::widget::button(text(label).size(TEXT_BODY).style(style::text_link))
        .padding(0)
        .style(style::button_link)
        .on_press(msg)
        .into()
}

pub fn view<'a>() -> Element<'a, Message> {
    let body = column![
        text(t!("legal.terms_title").into_owned())
            .size(TEXT_TITLE)
            .font(font_semibold()),
        text(t!("legal.terms_intro").into_owned())
            .size(TEXT_BODY)
            .style(style::text_secondary),
        point(Icon::Ban, t!("legal.terms_point_content").into_owned()),
        point(Icon::Flag, t!("legal.terms_point_moderation").into_owned()),
        point(Icon::EyeOff, t!("legal.terms_point_tools").into_owned()),
        point(
            Icon::ShieldCheck,
            t!("legal.terms_point_privacy").into_owned()
        ),
        row![
            link(t!("legal.terms_full").into_owned(), Message::OpenTerms),
            link(t!("legal.privacy").into_owned(), Message::OpenPrivacy),
        ]
        .spacing(SPACE_4),
        row![
            iced::widget::space().width(Length::Fill),
            secondary_button(t!("legal.terms_quit").into_owned(), Some(Message::Quit)),
            primary_button(t!("legal.terms_accept").into_owned(), Some(Message::Accept)),
        ]
        .spacing(SPACE_2),
    ]
    .spacing(SPACE_4)
    .max_width(560);
    container(scrollable(
        container(
            container(body)
                .padding(SPACE_6)
                .style(style::container_card),
        )
        .padding(SPACE_6)
        .center_x(Length::Fill),
    ))
    .center(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen builds in every bundled language without a missing key.
    #[test]
    fn builds() {
        let _ = view();
        for k in [
            "legal.terms_title",
            "legal.terms_intro",
            "legal.terms_point_content",
            "legal.terms_point_moderation",
            "legal.terms_accept",
        ] {
            assert_ne!(t!(k, locale = "en"), k);
        }
    }
}
