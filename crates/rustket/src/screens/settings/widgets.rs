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
pub(super) fn section<'a, M: 'a>(title: String, rows: Vec<Element<'a, M>>) -> Element<'a, M> {
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
    switch_with(label, help, value, on_toggle, None)
}

/// [`switch`] for a preference the enterprise policy can force: `managed`
/// is the "Managed by …" text while it does (docs/policy.md section 9.7),
/// shown as a visible line under the label; the switch is then disabled
/// and shows the forced value (`value`, the effective preference).
pub(super) fn switch_with<'a>(
    label: String,
    help: Option<String>,
    value: bool,
    on_toggle: Option<fn(bool) -> Message>,
    managed: Option<String>,
) -> Element<'a, Message> {
    let locked = managed.is_some();
    let on_toggle = if locked { None } else { on_toggle };
    // Disabled: the label is dimmed (the track alone barely shows it).
    let label_style = if on_toggle.is_some() {
        style::text_primary
    } else {
        style::text_hint
    };
    let mut col = column![text(label).size(TEXT_BODY).style(label_style)].spacing(SPACE_1);
    if let Some(h) = help {
        col = col.push(text(h).size(TEXT_CAPTION).style(style::text_secondary));
    }
    if let Some(m) = managed {
        col = col.push(managed_line(m));
    }
    row![
        col.width(Length::Fill),
        focus_ring(
            toggler(value)
                .size(20)
                .style(if locked {
                    toggler_managed
                } else {
                    style::toggler
                })
                .on_toggle_maybe(on_toggle),
            on_toggle.map(|f| f(!value)),
        )
        .radius(rustket_ui::theme::RADIUS_PILL),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center)
    .into()
}

/// A forced switch: disabled, but its track keeps the on or off colour
/// (faded), so the forced value reads at a glance; the shared disabled
/// style paints both states alike.
fn toggler_managed(theme: &iced::Theme, status: toggler::Status) -> toggler::Style {
    let t = rustket_ui::theme::tokens(theme);
    let on = match status {
        toggler::Status::Active { is_toggled }
        | toggler::Status::Hovered { is_toggled }
        | toggler::Status::Disabled { is_toggled } => is_toggled,
    };
    let track = if on { t.primary } else { t.border_strong };
    toggler::Style {
        background: iced::Background::Color(track.scale_alpha(0.55)),
        ..style::toggler(theme, status)
    }
}

/// The lock icon and the "Managed by …" text of a forced setting.
pub(super) fn managed_line<'a>(label: String) -> Element<'a, Message> {
    row![
        rustket_ui::icon::sized(rustket_ui::icon::Icon::Lock, TEXT_CAPTION)
            .style(style::text_secondary),
        text(label)
            .size(TEXT_CAPTION)
            .style(style::text_secondary)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
    ]
    .spacing(SPACE_1)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A forced switch shows on and off apart: on keeps the primary colour.
    #[test]
    fn managed_switch_tracks_differ_by_value() {
        let theme = rustket_ui::theme(rustket_ui::theme::Mode::Light);
        let t = rustket_ui::theme::tokens(&theme);
        let track =
            |is_toggled| match toggler_managed(&theme, toggler::Status::Disabled { is_toggled })
                .background
            {
                iced::Background::Color(c) => c,
                other => panic!("{other:?}"),
            };
        let (on, off) = (track(true), track(false));
        assert_ne!(on, off);
        assert_eq!((on.r, on.g, on.b), (t.primary.r, t.primary.g, t.primary.b));
        assert!(on.a < t.primary.a, "faded");
    }
}
