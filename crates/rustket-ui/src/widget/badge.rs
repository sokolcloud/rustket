//! Unread/mention counters.

use iced::widget::{container, space, text};
use iced::{Color, Element, Theme, border};

use crate::theme::{TEXT_MICRO, Tokens, font_bold, tokens};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BadgeKind {
    /// Plain unread messages.
    #[default]
    Unread,
    /// The user was mentioned directly (or a DM).
    Mention,
    /// `@all` / `@here` mention.
    GroupMention,
    /// Unread thread replies.
    Thread,
    /// Muted room.
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

    /// Picks the badge kind for a room the way Rocket.Chat does: direct mentions win
    /// over group mentions, then thread-only unread; muted rooms are grey.
    pub fn for_room(
        user_mentions: u32,
        group_mentions: u32,
        unread: u32,
        thread_unread: bool,
        muted: bool,
    ) -> Option<Self> {
        if user_mentions > 0 {
            Some(BadgeKind::Mention)
        } else if group_mentions > 0 {
            Some(BadgeKind::GroupMention)
        } else if muted && (unread > 0 || thread_unread) {
            Some(BadgeKind::Muted)
        } else if unread > 0 {
            Some(BadgeKind::Unread)
        } else if thread_unread {
            Some(BadgeKind::Thread)
        } else {
            None
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

/// A small dot badge without a count (e.g. unread threads in condensed mode).
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

    #[test]
    fn room_badge_priority() {
        assert_eq!(
            BadgeKind::for_room(1, 1, 5, true, true),
            Some(BadgeKind::Mention)
        );
        assert_eq!(
            BadgeKind::for_room(0, 2, 5, false, false),
            Some(BadgeKind::GroupMention)
        );
        assert_eq!(
            BadgeKind::for_room(0, 0, 5, false, true),
            Some(BadgeKind::Muted)
        );
        assert_eq!(
            BadgeKind::for_room(0, 0, 5, false, false),
            Some(BadgeKind::Unread)
        );
        assert_eq!(
            BadgeKind::for_room(0, 0, 0, true, false),
            Some(BadgeKind::Thread)
        );
        assert_eq!(BadgeKind::for_room(0, 0, 0, false, false), None);
    }
}
