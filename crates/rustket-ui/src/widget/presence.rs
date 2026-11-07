//! User presence indicator.

use iced::widget::{container, space};
use iced::{Color, Element, Theme, border};

use crate::theme::{Tokens, tokens};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Presence {
    Online,
    Away,
    Busy,
    Offline,
    #[default]
    Unknown,
}

impl Presence {
    pub fn color(self, t: &Tokens) -> Color {
        match self {
            Presence::Online => t.presence_online,
            Presence::Away => t.presence_away,
            Presence::Busy => t.presence_busy,
            Presence::Offline | Presence::Unknown => t.presence_offline,
        }
    }
}

/// A filled circle in the presence colour. Offline/unknown are drawn as a ring.
pub fn presence_dot<'a, Message: 'a>(status: Presence, size: f32) -> Element<'a, Message> {
    container(space().width(size).height(size))
        .style(move |theme: &Theme| {
            let t = tokens(theme);
            let color = status.color(t);
            let hollow = matches!(status, Presence::Offline | Presence::Unknown);
            let (ring_width, ring) = if hollow {
                ((size / 4.0).max(1.5), color)
            } else if status == Presence::Away && t.presence_away_ring != color {
                (1.0, t.presence_away_ring)
            } else {
                (0.0, color)
            };
            container::Style {
                background: Some(if hollow { Color::TRANSPARENT } else { color }.into()),
                border: border::rounded(size).width(ring_width).color(ring),
                ..container::Style::default()
            }
        })
        .into()
}
