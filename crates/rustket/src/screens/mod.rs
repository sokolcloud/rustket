//! Full-window screens shown before a workspace is available.

pub mod account;
pub mod add_server;
pub mod cert;
pub mod directory;
pub mod login;
pub mod page;
pub mod settings;

use iced::widget::{column, container, text};
use iced::{Alignment, Element, Length};
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_6, TEXT_BODY, TEXT_HEADLINE};

/// Centred card used by the add-server and login screens.
pub fn card<'a, M: 'a>(
    title: String,
    subtitle: Option<String>,
    body: Element<'a, M>,
) -> Element<'a, M> {
    let mut head = column![
        text(title)
            .size(TEXT_HEADLINE)
            .font(FONT_SEMIBOLD)
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
    ]
    .spacing(SPACE_2)
    .width(Length::Fill)
    .align_x(Alignment::Center);
    if let Some(s) = subtitle {
        head = head.push(
            text(s)
                .size(TEXT_BODY)
                .style(style::text_secondary)
                .width(Length::Fill)
                .align_x(iced::alignment::Horizontal::Center),
        );
    }
    let card = container(column![head, body].spacing(SPACE_6).width(Length::Fill))
        .padding(32)
        .max_width(440)
        .style(style::container_card);
    container(iced::widget::scrollable(
        container(card).center_x(Length::Fill).padding(SPACE_6),
    ))
    .center(Length::Fill)
    .style(style::container_room)
    .into()
}
