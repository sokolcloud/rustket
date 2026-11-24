//! Unread/mention counters.

use iced::widget::{container, space, text};
use iced::{Color, Element, Shadow, Theme, Vector, border};

use crate::theme::{IndicatorLook, TEXT_MICRO, Tokens, font_bold, indicator_look, tokens};

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

/// Where a badge is drawn: the Rocket.Chat look picks its palette by it
/// (the Rustket look ignores it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BadgePlace {
    /// Room and thread rows, section headers (RC's always-dark sidebar
    /// palette, `rc_sidebar_*`).
    #[default]
    Sidebar,
    /// The room header, timeline, thread panel, inbox views and quick
    /// switcher (`rc_danger` … `rc_secondary`, per mode).
    Content,
    /// The workspace badge on the rail and title bar: the desktop app's
    /// grey "ghost" badge (`rc_ghost`), whatever the kind.
    Shell,
}

/// How a counter is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BadgeCap {
    /// `99+` above 99.
    #[default]
    At99,
    /// The full number (Rocket.Chat's room rows and collapsed headers).
    None,
}

/// The ghost badge's shadow (`0 0 1px 1px rgba(0,0,0,.2)`; iced has no
/// spread, so blur and spread are one 2 px blur).
pub const GHOST_SHADOW: Shadow = Shadow {
    color: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.2,
    },
    offset: Vector::new(0.0, 0.0),
    blur_radius: 2.0,
};

impl BadgeKind {
    /// Background and text colours under the active look, sidebar palette.
    pub fn colors(self, t: &Tokens) -> (Color, Color) {
        self.colors_in(t, indicator_look(), BadgePlace::Sidebar)
    }

    /// Background and text colours under `look` at `place`. Rustket: the
    /// `badge_*` tokens wherever it is drawn. Rocket.Chat: only `rc_*`
    /// tokens; Unread and Muted are both "secondary".
    pub fn colors_in(self, t: &Tokens, look: IndicatorLook, place: BadgePlace) -> (Color, Color) {
        match (look, place) {
            (IndicatorLook::Rustket, _) => self.rustket_colors(t),
            (IndicatorLook::RocketChat, BadgePlace::Shell) => (t.rc_ghost, t.rc_ghost_fg),
            (IndicatorLook::RocketChat, BadgePlace::Sidebar) => {
                let bg = match self {
                    BadgeKind::Mention => t.rc_sidebar_danger,
                    BadgeKind::GroupMention => t.rc_sidebar_warning,
                    BadgeKind::Thread => t.rc_sidebar_primary,
                    BadgeKind::Unread | BadgeKind::Muted => t.rc_sidebar_secondary,
                };
                (bg, t.rc_badge_fg)
            }
            (IndicatorLook::RocketChat, BadgePlace::Content) => {
                let bg = match self {
                    BadgeKind::Mention => t.rc_danger,
                    BadgeKind::GroupMention => t.rc_warning,
                    BadgeKind::Thread => t.rc_primary,
                    BadgeKind::Unread | BadgeKind::Muted => t.rc_secondary,
                };
                (bg, t.rc_badge_fg)
            }
        }
    }

    fn rustket_colors(self, t: &Tokens) -> (Color, Color) {
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
    badge_label_in(u64::from(count), BadgeCap::At99)
}

/// Counter text under `cap`.
pub fn badge_label_in(count: u64, cap: BadgeCap) -> String {
    match cap {
        BadgeCap::At99 if count > 99 => "99+".to_owned(),
        _ => count.to_string(),
    }
}

pub const BADGE_HEIGHT: f32 = 18.0;

/// A pill with a count (`99+` cap), sidebar palette of the active look.
pub fn badge<'a, Message: 'a>(count: u32, kind: BadgeKind) -> Element<'a, Message> {
    badge_in(u64::from(count), kind, BadgePlace::Sidebar, BadgeCap::At99)
}

/// The container style of a badge or dot.
fn pill_style(
    t: &Tokens,
    kind: BadgeKind,
    look: IndicatorLook,
    place: BadgePlace,
    radius: f32,
) -> container::Style {
    let (bg, fg) = kind.colors_in(t, look, place);
    let ghost = look == IndicatorLook::RocketChat && place == BadgePlace::Shell;
    container::Style {
        text_color: Some(fg),
        background: Some(bg.into()),
        border: border::rounded(radius),
        shadow: if ghost {
            GHOST_SHADOW
        } else {
            Shadow::default()
        },
        ..container::Style::default()
    }
}

/// A pill with a count at `place`, printed under `cap`, in the look active
/// when it is built (Rustket's 18 px geometry in both looks).
pub fn badge_in<'a, Message: 'a>(
    count: u64,
    kind: BadgeKind,
    place: BadgePlace,
    cap: BadgeCap,
) -> Element<'a, Message> {
    pill(badge_label_in(count, cap), kind, place)
}

fn pill<'a, Message: 'a>(
    label: String,
    kind: BadgeKind,
    place: BadgePlace,
) -> Element<'a, Message> {
    let look = indicator_look();
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
    .style(move |theme: &Theme| pill_style(tokens(theme), kind, look, place, BADGE_HEIGHT / 2.0))
    .into()
}

/// A small dot badge without a count (e.g. a thread row, or `alert` alone),
/// sidebar palette of the active look.
pub fn dot<'a, Message: 'a>(kind: BadgeKind, size: f32) -> Element<'a, Message> {
    dot_in(kind, size, BadgePlace::Sidebar)
}

/// A dot at `place` in the look active when it is built.
pub fn dot_in<'a, Message: 'a>(
    kind: BadgeKind,
    size: f32,
    place: BadgePlace,
) -> Element<'a, Message> {
    let look = indicator_look();
    container(space().width(size).height(size))
        .style(move |theme: &Theme| pill_style(tokens(theme), kind, look, place, size))
        .into()
}

/// Size of [`bullet_in`] (Fuselage's 1rem badge minimum).
pub const BULLET_SIZE: f32 = 16.0;

/// Electron's vertical-tab "unread, no count" badge at `place`: a
/// [`BULLET_SIZE`] pill with a centred 2 px dot in the text colour, in the
/// look active when it is built.
pub fn bullet_in<'a, Message: 'a>(kind: BadgeKind, place: BadgePlace) -> Element<'a, Message> {
    let look = indicator_look();
    let dot = container(space().width(2.0).height(2.0)).style(move |theme: &Theme| {
        let (_, fg) = kind.colors_in(tokens(theme), look, place);
        container::Style {
            background: Some(fg.into()),
            border: border::rounded(1.0),
            ..container::Style::default()
        }
    });
    container(dot)
        .center(BULLET_SIZE)
        .style(move |theme: &Theme| pill_style(tokens(theme), kind, look, place, BULLET_SIZE / 2.0))
        .into()
}

/// The badge of a workspace item on the rail or in the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WorkspaceBadge {
    #[default]
    None,
    /// Rustket: a counter of the kind (`99+`), or a dot of the kind when the
    /// count is 0.
    Kind(u32, BadgeKind),
    /// Rocket.Chat desktop app: the ghost counter (`99+`).
    Ghost(u32),
    /// Rocket.Chat desktop app: unread without a number, a ghost ball in the
    /// title bar (horizontal tabs) or a ghost pill with a dot on the rail
    /// (vertical tabs).
    GhostBullet,
}

impl From<Option<(u32, BadgeKind)>> for WorkspaceBadge {
    fn from(b: Option<(u32, BadgeKind)>) -> Self {
        b.map_or(Self::None, |(n, k)| Self::Kind(n, k))
    }
}

/// Where a workspace badge is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceAt {
    /// The left rail (Electron's vertical tabs).
    Rail,
    /// The custom title bar (Electron's horizontal tab strip).
    TitleBar,
}

/// What a [`WorkspaceBadge`] draws.
#[derive(Debug, Clone, PartialEq)]
pub enum BadgeShape {
    /// A counter: label, kind, place.
    Pill(String, BadgeKind, BadgePlace),
    /// A dot: size, kind, place.
    Dot(f32, BadgeKind, BadgePlace),
    /// A [`bullet_in`] pill: kind, place.
    Bullet(BadgeKind, BadgePlace),
}

impl WorkspaceBadge {
    /// The shape drawn at `at`.
    pub fn shape(self, at: WorkspaceAt) -> Option<BadgeShape> {
        let ghost = BadgeKind::Unread;
        Some(match (self, at) {
            (Self::None, _) => return None,
            (Self::Kind(0, k), WorkspaceAt::Rail) => BadgeShape::Dot(10.0, k, BadgePlace::Sidebar),
            (Self::Kind(0, k), WorkspaceAt::TitleBar) => {
                BadgeShape::Dot(9.0, k, BadgePlace::Sidebar)
            }
            (Self::Kind(n, k), _) => BadgeShape::Pill(badge_label(n), k, BadgePlace::Sidebar),
            (Self::Ghost(n), _) => BadgeShape::Pill(badge_label(n), ghost, BadgePlace::Shell),
            (Self::GhostBullet, WorkspaceAt::Rail) => BadgeShape::Bullet(ghost, BadgePlace::Shell),
            (Self::GhostBullet, WorkspaceAt::TitleBar) => {
                BadgeShape::Dot(8.0, ghost, BadgePlace::Shell)
            }
        })
    }
}

/// The element of a workspace badge at `at` (`None`: nothing drawn), in the
/// look active when it is built.
pub fn workspace_badge<'a, Message: 'a>(
    b: WorkspaceBadge,
    at: WorkspaceAt,
) -> Option<Element<'a, Message>> {
    Some(match b.shape(at)? {
        BadgeShape::Pill(label, kind, place) => pill(label, kind, place),
        BadgeShape::Dot(size, kind, place) => dot_in(kind, size, place),
        BadgeShape::Bullet(kind, place) => bullet_in(kind, place),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    #[test]
    fn capping() {
        assert_eq!(badge_label(0), "0");
        assert_eq!(badge_label(7), "7");
        assert_eq!(badge_label(99), "99");
        assert_eq!(badge_label(100), "99+");
        assert_eq!(badge_label(u32::MAX), "99+");
    }

    #[test]
    fn capping_in() {
        assert_eq!(badge_label_in(0, BadgeCap::At99), "0");
        assert_eq!(badge_label_in(1, BadgeCap::At99), "1");
        assert_eq!(badge_label_in(100, BadgeCap::At99), "99+");
        assert_eq!(badge_label_in(100, BadgeCap::None), "100");
        assert_eq!(badge_label_in(1234, BadgeCap::None), "1234");
    }

    const KINDS: [BadgeKind; 5] = [
        BadgeKind::Unread,
        BadgeKind::Mention,
        BadgeKind::GroupMention,
        BadgeKind::Thread,
        BadgeKind::Muted,
    ];
    const PLACES: [BadgePlace; 3] = [BadgePlace::Sidebar, BadgePlace::Content, BadgePlace::Shell];

    #[test]
    fn rustket_look_keeps_todays_colours_everywhere() {
        for t in [&LIGHT, &DARK] {
            let today = |k| match k {
                BadgeKind::Unread => (t.badge_unread, t.badge_fg),
                BadgeKind::Mention => (t.badge_mention, t.badge_fg),
                BadgeKind::GroupMention => (t.badge_group_mention, t.badge_fg),
                BadgeKind::Thread => (t.badge_thread, t.badge_fg),
                BadgeKind::Muted => (t.badge_muted, t.badge_muted_fg),
            };
            for k in KINDS {
                assert_eq!(k.colors(t), today(k), "{k:?}");
                for p in PLACES {
                    assert_eq!(k.colors_in(t, IndicatorLook::Rustket, p), today(k));
                }
            }
        }
    }

    #[test]
    fn rocketchat_look_uses_only_rc_tokens() {
        let rc = IndicatorLook::RocketChat;
        for t in [&LIGHT, &DARK] {
            let side = |k: BadgeKind| k.colors_in(t, rc, BadgePlace::Sidebar);
            assert_eq!(
                side(BadgeKind::Mention),
                (t.rc_sidebar_danger, t.rc_badge_fg)
            );
            assert_eq!(
                side(BadgeKind::GroupMention),
                (t.rc_sidebar_warning, t.rc_badge_fg)
            );
            assert_eq!(
                side(BadgeKind::Thread),
                (t.rc_sidebar_primary, t.rc_badge_fg)
            );
            assert_eq!(
                side(BadgeKind::Unread),
                (t.rc_sidebar_secondary, t.rc_badge_fg)
            );
            assert_eq!(side(BadgeKind::Muted), side(BadgeKind::Unread));
            let content = |k: BadgeKind| k.colors_in(t, rc, BadgePlace::Content);
            assert_eq!(content(BadgeKind::Mention), (t.rc_danger, t.rc_badge_fg));
            assert_eq!(
                content(BadgeKind::GroupMention),
                (t.rc_warning, t.rc_badge_fg)
            );
            assert_eq!(content(BadgeKind::Thread), (t.rc_primary, t.rc_badge_fg));
            assert_eq!(content(BadgeKind::Unread), (t.rc_secondary, t.rc_badge_fg));
            assert_eq!(content(BadgeKind::Muted), content(BadgeKind::Unread));
            for k in KINDS {
                assert_eq!(
                    k.colors_in(t, rc, BadgePlace::Shell),
                    (t.rc_ghost, t.rc_ghost_fg)
                );
            }
        }
        // The sidebar palette is the same in both modes.
        assert_eq!(
            BadgeKind::Mention.colors_in(&LIGHT, rc, BadgePlace::Sidebar),
            BadgeKind::Mention.colors_in(&DARK, rc, BadgePlace::Sidebar)
        );
    }

    #[test]
    fn colors_follow_the_thread_local_look() {
        let t = &LIGHT;
        let before = crate::theme::generation();
        crate::theme::with_indicator_look(IndicatorLook::RocketChat, || {
            assert_eq!(
                BadgeKind::Mention.colors(t),
                (t.rc_sidebar_danger, t.rc_badge_fg)
            );
        });
        assert_eq!(BadgeKind::Mention.colors(t), (t.badge_mention, t.badge_fg));
        assert_eq!(crate::theme::generation(), before);
    }

    #[test]
    fn only_the_rc_shell_badge_has_a_shadow() {
        let t = &LIGHT;
        for look in [IndicatorLook::Rustket, IndicatorLook::RocketChat] {
            for p in PLACES {
                let s = pill_style(t, BadgeKind::Mention, look, p, 9.0);
                let ghost = look == IndicatorLook::RocketChat && p == BadgePlace::Shell;
                assert_eq!(s.shadow == GHOST_SHADOW, ghost, "{look:?} {p:?}");
            }
        }
    }

    /// Rustket workspace badges keep today's shapes; Rocket.Chat's are
    /// ghost pills (`99+`), an 8 px ghost ball in the title bar and a ghost
    /// bullet on the rail, all at the shell place (ghost colours, shadow).
    #[test]
    fn workspace_badge_shapes() {
        use BadgePlace::{Shell, Sidebar};
        use WorkspaceAt::{Rail, TitleBar};
        let k = BadgeKind::GroupMention;
        assert_eq!(WorkspaceBadge::from(None), WorkspaceBadge::None);
        assert_eq!(
            WorkspaceBadge::from(Some((3, k))),
            WorkspaceBadge::Kind(3, k)
        );
        for at in [Rail, TitleBar] {
            assert_eq!(WorkspaceBadge::None.shape(at), None);
            assert_eq!(
                WorkspaceBadge::Kind(150, k).shape(at),
                Some(BadgeShape::Pill("99+".into(), k, Sidebar))
            );
            assert_eq!(
                WorkspaceBadge::Ghost(7).shape(at),
                Some(BadgeShape::Pill("7".into(), BadgeKind::Unread, Shell))
            );
            assert_eq!(
                WorkspaceBadge::Ghost(100).shape(at),
                Some(BadgeShape::Pill("99+".into(), BadgeKind::Unread, Shell))
            );
            assert!(workspace_badge::<()>(WorkspaceBadge::None, at).is_none());
            assert!(workspace_badge::<()>(WorkspaceBadge::GhostBullet, at).is_some());
        }
        assert_eq!(
            WorkspaceBadge::Kind(0, k).shape(Rail),
            Some(BadgeShape::Dot(10.0, k, Sidebar))
        );
        assert_eq!(
            WorkspaceBadge::Kind(0, k).shape(TitleBar),
            Some(BadgeShape::Dot(9.0, k, Sidebar))
        );
        assert_eq!(
            WorkspaceBadge::GhostBullet.shape(Rail),
            Some(BadgeShape::Bullet(BadgeKind::Unread, Shell))
        );
        assert_eq!(
            WorkspaceBadge::GhostBullet.shape(TitleBar),
            Some(BadgeShape::Dot(8.0, BadgeKind::Unread, Shell))
        );
        let t = &LIGHT;
        let rc = IndicatorLook::RocketChat;
        assert_eq!(
            BadgeKind::Unread.colors_in(t, rc, Shell),
            (t.rc_ghost, t.rc_ghost_fg)
        );
        assert_eq!(
            pill_style(t, BadgeKind::Unread, rc, Shell, 8.0).shadow,
            GHOST_SHADOW
        );
    }
}
