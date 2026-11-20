//! Unread/mention counters.

use iced::widget::{container, space, text};
use iced::{Color, Element, Theme, border};

use crate::theme::{TEXT_MICRO, Tokens, font_bold, tokens};

/// The colour of a badge. Room and thread rows get their kind from
/// `rustket_core::room_list` (`BadgeVariant`, the one ranking: Mention >
/// GroupMention > Unread > Thread > Muted); this type only draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BadgeKind {
    /// Muted blue: unread messages in the main timeline, including the
    /// `alert`-only dot of a plain channel message.
    #[default]
    Unread,
    /// Red: the user was mentioned directly (or by a highlight word), or a
    /// direct message is unread.
    Mention,
    /// Orange: `@all` / `@here`, unless the room mutes group mentions.
    GroupMention,
    /// Slate: unread thread replies (rows, headers, dots in timelines and
    /// thread lists); also the waiting dot of a queued omnichannel chat.
    Thread,
    /// Grey: a non-mention badge of a room with notifications off, or a
    /// quiet counter (scheduled messages).
    Muted,
}

impl BadgeKind {
    pub fn colors(self, t: &Tokens) -> (Color, Color) {
        match self {
            BadgeKind::Unread => (t.badge_unread, t.badge_fg),
            BadgeKind::Mention => (t.badge_mention, t.badge_fg),
            BadgeKind::GroupMention => (t.badge_group_mention, t.badge_fg),
            BadgeKind::Thread => (t.badge_thread, t.badge_fg),
            BadgeKind::Muted => (t.badge_muted, t.badge_muted_fg),
        }
    }
}

/// Counter text capped at `99+`.
pub fn badge_label(count: u32) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

pub const BADGE_HEIGHT: f32 = 18.0;

/// A pill with a count (`99+` cap).
pub fn badge<'a, Message: 'a>(count: u32, kind: BadgeKind) -> Element<'a, Message> {
    let label = badge_label(count);
    let narrow = label.len() == 1;
    container(
        text(label)
            .size(TEXT_MICRO + 1.0)
            .font(font_bold())
            .wrapping(text::Wrapping::None),
    )
    .height(BADGE_HEIGHT)
    .width(if narrow {
        iced::Length::Fixed(BADGE_HEIGHT)
    } else {
        iced::Length::Shrink
    })
    .padding([0.0, if narrow { 0.0 } else { 5.0 }])
    .center_y(BADGE_HEIGHT)
    .align_x(iced::alignment::Horizontal::Center)
    .style(move |theme: &Theme| {
        let (bg, fg) = kind.colors(tokens(theme));
        container::Style {
            text_color: Some(fg),
            background: Some(bg.into()),
            border: border::rounded(BADGE_HEIGHT / 2.0),
            ..container::Style::default()
        }
    })
    .into()
}

/// A small dot badge without a count (e.g. a thread row, or `alert` alone).
pub fn dot<'a, Message: 'a>(kind: BadgeKind, size: f32) -> Element<'a, Message> {
    container(space().width(size).height(size))
        .style(move |theme: &Theme| container::Style {
            background: Some(kind.colors(tokens(theme)).0.into()),
            border: border::rounded(size),
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capping() {
        assert_eq!(badge_label(0), "0");
        assert_eq!(badge_label(7), "7");
        assert_eq!(badge_label(99), "99");
        assert_eq!(badge_label(100), "99+");
        assert_eq!(badge_label(u32::MAX), "99+");
    }
}
