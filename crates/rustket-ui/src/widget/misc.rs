//! Small decorations: tooltips, chips/tags, keyboard hints, dividers.

use iced::widget::{Column, column, container, text, tooltip as iced_tooltip};
use iced::{Element, Length, Theme, border};

use crate::style;
use crate::theme::{FONT_SEMIBOLD, MONOSPACE_FONT, RADIUS_SM, TEXT_CAPTION, TEXT_MICRO, tokens};

pub use iced::widget::tooltip::Position as TooltipPosition;

/// Wraps `content` with a consistently styled text tooltip.
pub fn tooltip<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    label: impl Into<String>,
    position: TooltipPosition,
) -> Element<'a, Message> {
    iced_tooltip(
        content,
        container(text(label.into()).size(TEXT_CAPTION))
            .padding([4, 8])
            .style(style::container_tooltip),
        position,
    )
    .gap(6)
    .snap_within_viewport(true)
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChipKind {
    #[default]
    Neutral,
    Primary,
    Danger,
    Warning,
    Success,
}

/// Small rounded tag, e.g. user roles (Admin, Owner, Bot).
pub fn chip<'a, Message: 'a>(label: impl Into<String>, kind: ChipKind) -> Element<'a, Message> {
    container(
        text(label.into())
            .size(TEXT_MICRO + 1.0)
            .font(FONT_SEMIBOLD)
            .wrapping(text::Wrapping::None),
    )
    .padding([1, 6])
    .style(move |theme: &Theme| {
        let t = tokens(theme);
        let (bg, fg) = match kind {
            ChipKind::Neutral => (t.surface_sunken, t.text_secondary),
            ChipKind::Primary => (t.info_bg, t.info_fg),
            ChipKind::Danger => (t.error_bg, t.error_fg),
            ChipKind::Warning => (t.warning_bg, t.warning_fg),
            ChipKind::Success => (t.success_bg, t.success_fg),
        };
        container::Style {
            text_color: Some(fg),
            background: Some(bg.into()),
            border: border::rounded(RADIUS_SM).width(1).color(t.border),
            ..container::Style::default()
        }
    })
    .into()
}

/// Keyboard key hint (`Ctrl`, `K`, `Enter`).
pub fn kbd<'a, Message: 'a>(key: impl Into<String>) -> Element<'a, Message> {
    container(
        text(key.into())
            .size(TEXT_MICRO + 1.0)
            .font(MONOSPACE_FONT)
            .wrapping(text::Wrapping::None),
    )
    .padding([1, 5])
    .style(|theme: &Theme| {
        let t = tokens(theme);
        container::Style {
            text_color: Some(t.text_secondary),
            background: Some(t.surface_sunken.into()),
            border: border::rounded(RADIUS_SM).width(1).color(t.border_strong),
            ..container::Style::default()
        }
    })
    .into()
}

/// 1px horizontal line in the border colour.
pub fn divider<'a, Message: 'a>() -> Element<'a, Message> {
    container(iced::widget::space().height(1))
        .width(Length::Fill)
        .height(1)
        .style(style::container_divider)
        .into()
}

/// 1px vertical line in the border colour.
pub fn vertical_divider<'a, Message: 'a>() -> Element<'a, Message> {
    container(iced::widget::space().width(1))
        .height(Length::Fill)
        .width(1)
        .style(style::container_divider)
        .into()
}

/// `top` with `gap` below it, or an empty placeholder: always one tree node,
/// so the widgets after it keep their positions (and their widget state:
/// focus, cursor, scroll offset) when `top` comes and goes.
pub fn slot<'a, Message: 'a>(top: Option<Element<'a, Message>>, gap: f32) -> Element<'a, Message> {
    match top {
        Some(top) => container(top)
            .padding(iced::Padding {
                bottom: gap,
                ..iced::Padding::ZERO
            })
            .into(),
        None => iced::widget::space().into(),
    }
}

/// `body` below an optional `top` (an error banner, a notice) separated by
/// `gap`. `body` keeps its widget state when `top` appears or disappears,
/// which it would not as the next child of a column (iced diffs children by
/// position).
pub fn above<'a, Message: 'a>(
    top: Option<Element<'a, Message>>,
    gap: f32,
    body: impl Into<Element<'a, Message>>,
) -> Column<'a, Message> {
    column![slot(top, gap), body.into()]
}
