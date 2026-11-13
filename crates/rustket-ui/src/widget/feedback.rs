//! Banners (connection state, certificate warnings…).

use iced::widget::{button, container, row, text};
use iced::{Alignment, Color, Element, Length, Theme, border};

use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{SPACE_2, SPACE_3, TEXT_BODY, Tokens, font_semibold, radius_sm, tokens};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BannerKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl BannerKind {
    pub fn colors(self, t: &Tokens) -> (Color, Color) {
        match self {
            BannerKind::Info => (t.info_bg, t.info_fg),
            BannerKind::Success => (t.success_bg, t.success_fg),
            BannerKind::Warning => (t.warning_bg, t.warning_fg),
            BannerKind::Error => (t.error_bg, t.error_fg),
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            BannerKind::Info => Icon::Info,
            BannerKind::Success => Icon::CircleCheck,
            BannerKind::Warning => Icon::TriangleAlert,
            BannerKind::Error => Icon::CircleAlert,
        }
    }

    pub(crate) fn container_style(self) -> fn(&Theme) -> container::Style {
        match self {
            BannerKind::Info => style::container_banner_info,
            BannerKind::Success => style::container_banner_success,
            BannerKind::Warning => style::container_banner_warning,
            BannerKind::Error => style::container_banner_error,
        }
    }
}

fn action_button<'a, Message: Clone + 'a>(
    kind: BannerKind,
    label: String,
    message: Message,
) -> Element<'a, Message> {
    button(text(label).size(TEXT_BODY).font(font_semibold()))
        .padding([2, 8])
        .style(move |theme: &Theme, status| {
            let t = tokens(theme);
            let (_, fg) = kind.colors(t);
            let background = match status {
                button::Status::Hovered => Some(fg.scale_alpha(0.12).into()),
                button::Status::Pressed => Some(fg.scale_alpha(0.2).into()),
                _ => None,
            };
            button::Style {
                background,
                text_color: fg,
                border: border::rounded(radius_sm())
                    .width(1)
                    .color(fg.scale_alpha(0.5)),
                ..button::Style::default()
            }
        })
        .on_press(message)
        .into()
}

/// Rounded inline banner with icon, text and an optional action.
pub fn banner<'a, Message: Clone + 'a>(
    kind: BannerKind,
    message: impl Into<String>,
    action: Option<(String, Message)>,
) -> Element<'a, Message> {
    let mut content = row![
        icon::sized(kind.icon(), 16.0),
        text(message.into()).size(TEXT_BODY).width(Length::Fill),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some((label, msg)) = action {
        content = content.push(action_button(kind, label, msg));
    }
    container(content)
        .padding([SPACE_2, SPACE_3])
        .width(Length::Fill)
        .style(kind.container_style())
        .into()
}

/// Full-width flat strip (no border/radius), e.g. "Reconnecting…" above the timeline.
pub fn status_strip<'a, Message: Clone + 'a>(
    kind: BannerKind,
    message: impl Into<String>,
    leading: Option<Element<'a, Message>>,
    action: Option<(String, Message)>,
) -> Element<'a, Message> {
    let lead = leading.unwrap_or_else(|| icon::sized(kind.icon(), 14.0).into());
    let mut content = row![
        lead,
        text(message.into())
            .size(TEXT_BODY - 1.0)
            .font(font_semibold())
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if let Some((label, msg)) = action {
        content = content.push(action_button(kind, label, msg));
    }
    container(content)
        .padding([4, 12])
        .center_x(Length::Fill)
        .style(style::container_strip(kind))
        .into()
}
