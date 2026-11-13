//! Buttons: primary/secondary/danger/ghost with optional icon and loading state,
//! plus square icon buttons with a tooltip.

use iced::widget::{Button, Space, button, row, text};
use iced::{Alignment, Element, Length, Padding, Theme};

use super::focus_ring::focus_ring;
use super::misc::tooltip;
use super::spinner::spinner;
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{SPACE_2, TEXT_BODY, font_medium};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    #[default]
    Primary,
    Secondary,
    Danger,
    Ghost,
}

impl ButtonKind {
    fn style(self) -> fn(&Theme, button::Status) -> button::Style {
        match self {
            ButtonKind::Primary => style::button_primary,
            ButtonKind::Secondary => style::button_secondary,
            ButtonKind::Danger => style::button_danger,
            ButtonKind::Ghost => style::button_ghost,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// Text button builder. Disabled when `on_press` is `None` or while loading.
pub struct ActionButton<'a, Message> {
    kind: ButtonKind,
    size: ButtonSize,
    label: String,
    icon: Option<Icon>,
    loading: bool,
    width: Length,
    on_press: Option<Message>,
    _marker: std::marker::PhantomData<&'a ()>,
}

pub fn action_button<'a, Message>(
    kind: ButtonKind,
    label: impl Into<String>,
    on_press: Option<Message>,
) -> ActionButton<'a, Message> {
    ActionButton {
        kind,
        size: ButtonSize::Medium,
        label: label.into(),
        icon: None,
        loading: false,
        width: Length::Shrink,
        on_press,
        _marker: std::marker::PhantomData,
    }
}

pub fn primary_button<'a, Message>(
    label: impl Into<String>,
    on_press: Option<Message>,
) -> ActionButton<'a, Message> {
    action_button(ButtonKind::Primary, label, on_press)
}

pub fn secondary_button<'a, Message>(
    label: impl Into<String>,
    on_press: Option<Message>,
) -> ActionButton<'a, Message> {
    action_button(ButtonKind::Secondary, label, on_press)
}

pub fn danger_button<'a, Message>(
    label: impl Into<String>,
    on_press: Option<Message>,
) -> ActionButton<'a, Message> {
    action_button(ButtonKind::Danger, label, on_press)
}

pub fn ghost_button<'a, Message>(
    label: impl Into<String>,
    on_press: Option<Message>,
) -> ActionButton<'a, Message> {
    action_button(ButtonKind::Ghost, label, on_press)
}

impl<'a, Message> ActionButton<'a, Message> {
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Shows a spinner in place of the icon and disables the button.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

impl<'a, Message: Clone + 'a> From<ActionButton<'a, Message>> for Element<'a, Message> {
    fn from(b: ActionButton<'a, Message>) -> Self {
        let (font_size, padding, height) = match b.size {
            ButtonSize::Small => (TEXT_BODY - 1.0, Padding::from([0.0, 10.0]), 28.0),
            ButtonSize::Medium => (TEXT_BODY, Padding::from([0.0, 14.0]), 36.0),
            ButtonSize::Large => (TEXT_BODY + 2.0, Padding::from([0.0, 18.0]), 44.0),
        };
        let lead: Option<Element<'a, Message>> = if b.loading {
            Some(spinner(font_size + 2.0))
        } else {
            b.icon.map(|i| icon::sized(i, font_size + 2.0).into())
        };
        let mut content = row![].spacing(SPACE_2).align_y(Alignment::Center);
        if b.width != Length::Shrink {
            content = content.push(Space::new().width(Length::Fill));
        }
        if let Some(lead) = lead {
            content = content.push(lead);
        }
        content = content.push(
            text(b.label)
                .size(font_size)
                .font(font_medium())
                .wrapping(text::Wrapping::None),
        );
        if b.width != Length::Shrink {
            content = content.push(Space::new().width(Length::Fill));
        }
        let on_press = if b.loading { None } else { b.on_press };
        let loading = b.loading;
        let kind_style = b.kind.style();
        let activate = on_press.clone();
        let btn = button(iced::widget::container(content).center_y(Length::Fill))
            .padding(padding)
            .height(height)
            .width(b.width)
            .style(move |theme: &Theme, status| {
                // Keep the enabled look while loading.
                let status = if loading && status == button::Status::Disabled {
                    button::Status::Active
                } else {
                    status
                };
                kind_style(theme, status)
            })
            .on_press_maybe(on_press);
        focus_ring(btn, activate).into()
    }
}

/// Square icon button. `tooltip` is shown on hover when non-empty.
pub fn icon_button<'a, Message: Clone + 'a>(
    icon: Icon,
    on_press: Option<Message>,
    tooltip_text: impl Into<String>,
) -> Element<'a, Message> {
    icon_button_with(icon, on_press, tooltip_text, false, 32.0)
}

/// Icon button with explicit active state and size.
pub fn icon_button_with<'a, Message: Clone + 'a>(
    icon: Icon,
    on_press: Option<Message>,
    tooltip_text: impl Into<String>,
    active: bool,
    size: f32,
) -> Element<'a, Message> {
    let glyph = icon::sized(icon, (size * 0.56).round());
    let b: Button<'a, Message> = button(iced::widget::container(glyph).center(Length::Fill))
        .width(size)
        .height(size)
        .padding(0)
        .style(if active {
            style::button_icon_active
        } else {
            style::button_icon
        })
        .on_press_maybe(on_press.clone());
    let b = focus_ring(b, on_press);
    let label = tooltip_text.into();
    if label.is_empty() {
        b.into()
    } else {
        tooltip(b, label, iced::widget::tooltip::Position::Bottom)
    }
}
