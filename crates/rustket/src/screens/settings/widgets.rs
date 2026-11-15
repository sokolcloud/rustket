//! Building blocks shared by the settings tabs: titled cards, labelled
//! controls, switches, segmented choices and links.

use std::fmt;

use iced::widget::{button, column, container, row, text, toggler};
use iced::{Alignment, Element, Length};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SUBTITLE, font_semibold,
};
use rustket_ui::widget::{divider, focus_ring, secondary_button};

use super::Message;

/// A pick list entry: value plus its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Choice<T> {
    pub(super) value: T,
    pub(super) label: String,
}

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// A titled card with rows separated by dividers.
pub(super) fn section<'a>(title: String, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![].spacing(SPACE_3);
    let n = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        col = col.push(r);
        if i + 1 < n {
            col = col.push(divider());
        }
    }
    column![
        text(title).size(TEXT_SUBTITLE).font(font_semibold()),
        container(col)
            .padding(SPACE_4)
            .width(Length::Fill)
            .style(style::container_card),
    ]
    .spacing(SPACE_2)
    .into()
}

/// Label (+ help) above a control.
pub(super) fn field<'a>(
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

/// Label (+ help) with a switch on the right.
pub(super) fn switch<'a>(
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

/// Label above a caption, with a button on the right.
pub(super) fn action_row<'a>(
    label: String,
    caption: Element<'a, Message>,
    button_label: String,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    row![
        column![text(label).size(TEXT_BODY), caption]
            .spacing(SPACE_1)
            .width(Length::Fill),
        secondary_button(button_label, on_press),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center)
    .into()
}

/// A secondary caption line.
pub(super) fn caption<'a>(s: String) -> iced::widget::Text<'a> {
    text(s).size(TEXT_CAPTION).style(style::text_secondary)
}

/// A row of mutually exclusive buttons (wrapping in narrow windows).
pub(super) fn segmented<'a, T: Clone + PartialEq + 'a>(
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
    r.wrap().vertical_spacing(SPACE_2).into()
}

pub(super) fn link<'a>(label: String, url: &'static str) -> Element<'a, Message> {
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
