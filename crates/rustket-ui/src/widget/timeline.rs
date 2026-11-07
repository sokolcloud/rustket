//! Timeline pieces: separators, reactions, emoji.

use iced::widget::{Text, button, container, row, text};
use iced::{Alignment, Element, Length, Theme};

use super::misc::divider;
use crate::style;
use crate::theme::{FONT_BOLD, FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_CAPTION, tokens};

/// Centred date label between two lines ("Today", "12 March 2026").
pub fn date_separator<'a, Message: 'a>(label: impl Into<String>) -> Element<'a, Message> {
    row![
        divider(),
        container(
            text(label.into())
                .size(TEXT_CAPTION)
                .font(FONT_SEMIBOLD)
                .wrapping(text::Wrapping::None)
                .style(style::text_secondary)
        )
        .padding([0.0, SPACE_2]),
        divider(),
    ]
    .align_y(Alignment::Center)
    .padding([SPACE_2, SPACE_3])
    .into()
}

/// Red "Unread messages" line with the label on the right.
pub fn unread_separator<'a, Message: 'a>(label: impl Into<String>) -> Element<'a, Message> {
    let line = container(iced::widget::space().height(1))
        .width(Length::Fill)
        .height(1)
        .style(|theme: &Theme| container::Style {
            background: Some(tokens(theme).danger.into()),
            ..container::Style::default()
        });
    row![
        line,
        container(
            text(label.into())
                .size(TEXT_CAPTION)
                .font(FONT_BOLD)
                .wrapping(text::Wrapping::None)
                .style(style::text_danger)
        )
        .padding([0.0, SPACE_2]),
    ]
    .align_y(Alignment::Center)
    .padding([SPACE_1, SPACE_3])
    .into()
}

/// Reaction pill: emoji + count; highlighted when the user reacted.
pub fn reaction_pill<'a, Message: Clone + 'a>(
    emoji: impl Into<Element<'a, Message>>,
    count: u32,
    reacted_by_me: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(
        row![
            emoji.into(),
            text(count.to_string())
                .size(TEXT_CAPTION)
                .font(FONT_SEMIBOLD)
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center),
    )
    .padding([2, 8])
    .style(style::button_reaction(reacted_by_me))
    .on_press_maybe(on_press)
    .into()
}

/// Unicode emoji text at `size` (16 for reactions, 32 for big-emoji messages).
/// Colour emoji come from the system emoji font through fallback.
pub fn emoji_text<'a>(emoji: impl text::IntoFragment<'a>, size: f32) -> Text<'a> {
    text(emoji)
        .font(crate::emoji::EMOJI_FONT)
        .size(size)
        .shaping(text::Shaping::Advanced)
        .line_height(text::LineHeight::Relative(1.2))
        .wrapping(text::Wrapping::None)
}
