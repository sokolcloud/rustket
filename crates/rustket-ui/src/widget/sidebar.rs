//! Sidebar building blocks: room rows, section headers, workspace rail items.

use iced::widget::{button, column, container, row, space, stack, text};
use iced::{Alignment, Color, Element, Font, Length, Theme, border};

use super::avatar::{AvatarShape, AvatarSource, avatar, face};
use super::badge::{
    BadgeCap, BadgeKind, BadgePlace, WorkspaceAt, WorkspaceBadge, badge, badge_in, dot,
    workspace_badge,
};
use super::ellipsis::ellipsis;
use super::flag::{FlagMark, flag_mark, flag_swatch};
use super::focus_ring::focus_ring;
use super::misc::{TooltipPosition, tooltip};
use super::presence::Presence;
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{
    IndicatorLook, RADIUS_PILL, SPACE_1, SPACE_2, TEXT_BODY, TEXT_CAPTION, TEXT_MICRO, Tokens,
    font_bold, font_medium, font_regular, font_semibold, indicator_look, radius_lg, tokens,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    /// Two lines: title and last message preview, 36px avatar.
    #[default]
    Extended,
    /// One line, 20px avatar.
    Condensed,
}

/// Data for one sidebar room row.
#[derive(Debug, Clone)]
pub struct RoomListItemProps<Message> {
    pub avatar: AvatarSource,
    pub title: String,
    /// Last message preview (extended mode only).
    pub subtitle: Option<String>,
    /// Formatted time of the last message.
    pub time: Option<String>,
    /// The badge drawn (`None`: no badge); its kind is decided by the
    /// caller (`rustket_core::room_list::badge` for room rows).
    pub badge: Option<BadgeKind>,
    /// The number on the badge; 0 draws a dot.
    pub count: u32,
    /// How the number is printed (Rocket.Chat rows: uncapped).
    pub cap: BadgeCap,
    /// Hover text of the badge.
    pub badge_tip: Option<String>,
    /// Bold title (unread rows).
    pub bold: bool,
    pub is_muted: bool,
    pub is_selected: bool,
    /// Presence of the DM peer.
    pub presence: Option<Presence>,
    /// Room type icon (Hash, Lock, Users for teams, MessagesSquare for discussions…).
    pub icon: Option<Icon>,
    /// Someone is typing: replaces the preview with `typing_label`.
    pub typing: bool,
    pub typing_label: String,
    pub density: Density,
    /// Flag mark after the title.
    pub flag: Option<FlagMark>,
    pub on_press: Message,
}

impl<Message> RoomListItemProps<Message> {
    pub fn new(avatar: AvatarSource, title: impl Into<String>, on_press: Message) -> Self {
        Self {
            avatar,
            title: title.into(),
            subtitle: None,
            time: None,
            badge: None,
            count: 0,
            cap: BadgeCap::At99,
            badge_tip: None,
            bold: false,
            is_muted: false,
            is_selected: false,
            presence: None,
            icon: None,
            typing: false,
            typing_label: String::new(),
            density: Density::Extended,
            flag: None,
            on_press,
        }
    }

    pub fn badge_kind(&self) -> Option<BadgeKind> {
        self.badge
    }

    /// The number the badge shows; 0 draws a dot (when [`Self::badge_kind`]
    /// gives one).
    pub fn badge_count(&self) -> u32 {
        if self.badge.is_some() { self.count } else { 0 }
    }

    pub fn is_bold(&self) -> bool {
        self.bold
    }
}

fn room_badge<'a, Message: 'a>(p: &RoomListItemProps<Message>) -> Option<Element<'a, Message>> {
    let kind = p.badge_kind()?;
    let count = p.badge_count();
    let b = if count == 0 {
        dot(kind, 8.0)
    } else if p.cap == BadgeCap::At99 {
        badge(count, kind)
    } else {
        badge_in(u64::from(count), kind, BadgePlace::Sidebar, p.cap)
    };
    Some(match &p.badge_tip {
        Some(tip) => tooltip(b, tip.clone(), TooltipPosition::Bottom),
        None => b,
    })
}

/// The text colours of a room row under `look`. Rustket: a dimmed title for
/// a muted read row, the time in the link colour and the preview in the
/// primary colour while bold. Rocket.Chat (`highlightUnread`): the title, its
/// icon and the preview bright while highlighted, else secondary; the time
/// always a hint; muted rooms look like the others.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowColors {
    pub title: Color,
    pub icon: Color,
    pub time: Color,
    pub preview: Color,
}

/// [`RowColors`] of a row that is `bold` (highlighted) and `muted`.
pub fn row_colors(t: &Tokens, look: IndicatorLook, bold: bool, muted: bool) -> RowColors {
    match look {
        IndicatorLook::Rustket => {
            let dim = muted && !bold;
            RowColors {
                title: if dim { t.text_hint } else { t.text_primary },
                icon: t.text_secondary,
                time: if bold && !dim { t.link } else { t.text_hint },
                preview: if bold && !dim {
                    t.text_primary
                } else {
                    t.text_secondary
                },
            }
        }
        IndicatorLook::RocketChat => {
            let c = if bold {
                t.text_primary
            } else {
                t.text_secondary
            };
            RowColors {
                title: c,
                icon: c,
                time: t.text_hint,
                preview: c,
            }
        }
    }
}

/// The title font and the preview font (`None`: the default) of a row that
/// is `bold` under `look`: Rustket's bold title, Rocket.Chat's weight 500
/// for the title and the preview.
pub fn row_fonts(look: IndicatorLook, bold: bool) -> (Font, Option<Font>) {
    match (look, bold) {
        (IndicatorLook::Rustket, true) => (font_bold(), None),
        (IndicatorLook::Rustket, false) => (font_regular(), None),
        (IndicatorLook::RocketChat, true) => (font_medium(), Some(font_medium())),
        (IndicatorLook::RocketChat, false) => (font_regular(), Some(font_regular())),
    }
}

/// A sidebar row (Rocket.Chat density). Hover/selected backgrounds come from the style.
pub fn room_list_item<'a, Message: Clone + 'a>(
    p: RoomListItemProps<Message>,
) -> Element<'a, Message> {
    let bold = p.is_bold();
    let muted = p.is_muted;
    let look = indicator_look();
    let colors = move |theme: &Theme| row_colors(tokens(theme), look, bold, muted);
    let (title_font, preview_font) = row_fonts(look, bold);
    let condensed = p.density == Density::Condensed;
    let avatar_size = if condensed { 20.0 } else { 36.0 };
    let selected = p.is_selected;

    let face_el: Element<'a, Message> = avatar(p.avatar.clone(), avatar_size, AvatarShape::Rounded)
        .presence(if condensed { None } else { p.presence })
        .ring_with(move |t| {
            if selected {
                t.sidebar_selected_bg
            } else {
                t.surface_sidebar
            }
        })
        .into();

    let mut title_row = row![].spacing(SPACE_1).align_y(Alignment::Center);
    if condensed && let Some(presence) = p.presence {
        title_row = title_row.push(super::presence::presence_dot(presence, 8.0));
    }
    if let Some(i) = p.icon {
        title_row = title_row.push(
            icon::sized(i, 14.0).style(move |theme: &Theme| text::Style {
                color: Some(colors(theme).icon),
            }),
        );
    }
    title_row = title_row.push(
        ellipsis(p.title.clone())
            .size(TEXT_BODY)
            .font(title_font)
            .style(move |theme: &Theme| Some(colors(theme).title)),
    );
    if let Some(mark) = &p.flag {
        title_row = title_row.push(flag_mark(mark, 12.0));
    }
    if p.is_muted {
        title_row = title_row.push(icon::sized(Icon::BellOff, 12.0).style(style::text_hint));
    }

    let content: Element<'a, Message> = if condensed {
        if let Some(b) = room_badge(&p) {
            title_row = title_row.push(b);
        }
        row![face_el, title_row]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
    } else {
        if let Some(time) = &p.time {
            title_row = title_row.push(
                text(time.clone())
                    .size(TEXT_CAPTION)
                    .wrapping(text::Wrapping::None)
                    .style(move |theme: &Theme| text::Style {
                        color: Some(colors(theme).time),
                    }),
            );
        }
        let preview: Element<'a, Message> = if p.typing {
            ellipsis(p.typing_label.clone())
                .size(TEXT_CAPTION + 1.0)
                .style(|theme: &Theme| Some(tokens(theme).link))
                .into()
        } else {
            let preview = ellipsis(p.subtitle.clone().unwrap_or_default())
                .size(TEXT_CAPTION + 1.0)
                .style(move |theme: &Theme| Some(colors(theme).preview));
            match preview_font {
                Some(f) => preview.font(f).into(),
                None => preview.into(),
            }
        };
        let mut sub_row = row![preview].spacing(SPACE_1).align_y(Alignment::Center);
        if let Some(b) = room_badge(&p) {
            sub_row = sub_row.push(b);
        }
        row![
            face_el,
            column![title_row, sub_row].spacing(2).width(Length::Fill)
        ]
        .spacing(SPACE_2 + 2.0)
        .align_y(Alignment::Center)
        .into()
    };

    let on_press = p.on_press;
    let row = button(container(content).center_y(Length::Fill))
        .width(Length::Fill)
        .height(if condensed { 32.0 } else { 56.0 })
        .padding([0.0, SPACE_2])
        .style(style::button_list_item(p.is_selected))
        .on_press(on_press.clone());
    focus_ring(row, Some(on_press)).into()
}

/// Collapsible sidebar group header ("Favorites", "Channels"…).
pub fn section_header<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    on_toggle: Message,
) -> Element<'a, Message> {
    section_header_with(title, collapsed, count, None, on_toggle)
}

/// [`section_header`] with a trailing status (e.g. a loading indicator) at
/// its right end.
pub fn section_header_with<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    status: Option<Element<'a, Message>>,
    on_toggle: Message,
) -> Element<'a, Message> {
    section_header_full(title, collapsed, count, None, status, on_toggle)
}

/// [`section_header_with`] with an optional flag swatch (palette index of
/// [`Tokens::flag`](crate::theme::Tokens::flag)) before the title.
pub fn section_header_full<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    swatch: Option<usize>,
    status: Option<Element<'a, Message>>,
    on_toggle: Message,
) -> Element<'a, Message> {
    header_impl(title, collapsed, count, swatch, status, false, on_toggle)
}

/// A section header in the Rocket.Chat look: no row count unless `count` is
/// given (search results); `emphasis` (a collapsed section with something
/// unread) draws the title bright and bold.
pub fn section_header_rc<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    swatch: Option<usize>,
    status: Option<Element<'a, Message>>,
    emphasis: bool,
    on_toggle: Message,
) -> Element<'a, Message> {
    header_impl(title, collapsed, count, swatch, status, emphasis, on_toggle)
}

fn header_impl<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    swatch: Option<usize>,
    status: Option<Element<'a, Message>>,
    emphasis: bool,
    on_toggle: Message,
) -> Element<'a, Message> {
    let mut content = row![icon::sized(
        if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        },
        14.0
    ),]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    if let Some(index) = swatch {
        content = content.push(flag_swatch(index, 8.0));
    }
    let title = text(title.into().to_uppercase())
        .size(TEXT_MICRO + 1.0)
        .wrapping(text::Wrapping::None);
    content = content.push(if emphasis {
        title.font(font_bold()).style(style::text_primary)
    } else {
        title.font(font_semibold())
    });
    if let Some(count) = count {
        content = content.push(
            text(count.to_string())
                .size(TEXT_MICRO + 1.0)
                .style(style::text_hint),
        );
    }
    if let Some(status) = status {
        content = content.push(space().width(Length::Fill)).push(status);
    }
    let header = button(content)
        .width(Length::Fill)
        .padding([6.0, SPACE_2])
        .style(|theme: &Theme, status| {
            let t = tokens(theme);
            let mut s = style::button_ghost(theme, status);
            s.text_color = match status {
                button::Status::Hovered | button::Status::Pressed => t.text_primary,
                _ => t.text_secondary,
            };
            s.background = None;
            s
        })
        .on_press(on_toggle.clone());
    focus_ring(header, Some(on_toggle)).into()
}

/// What a rail item shows.
#[derive(Debug, Clone)]
pub enum RailIcon {
    Avatar(AvatarSource),
    Icon(Icon),
}

const RAIL_ITEM: f32 = 40.0;

/// A workspace (server) in the left rail: selection indicator, tooltip with the
/// name and an optional unread badge.
pub fn workspace_rail_item<'a, Message: Clone + 'a>(
    icon_or_initials: RailIcon,
    name: impl Into<String>,
    unread: Option<(u32, BadgeKind)>,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message> {
    workspace_rail_item_with(icon_or_initials, name, unread.into(), selected, on_press)
}

/// [`workspace_rail_item`] with any [`WorkspaceBadge`] (the Rocket.Chat
/// ghost badges included).
pub fn workspace_rail_item_with<'a, Message: Clone + 'a>(
    icon_or_initials: RailIcon,
    name: impl Into<String>,
    unread: WorkspaceBadge,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let face_el: Element<'a, Message> = match &icon_or_initials {
        RailIcon::Avatar(source) => face(source, RAIL_ITEM, AvatarShape::Rounded),
        RailIcon::Icon(i) => container(icon::sized(*i, 20.0))
            .center(RAIL_ITEM)
            .style(|theme: &Theme| {
                let t = tokens(theme);
                container::Style {
                    text_color: Some(t.text_secondary),
                    background: Some(t.surface_raised.into()),
                    border: border::rounded(radius_lg()).width(1).color(t.border),
                    ..container::Style::default()
                }
            })
            .into(),
    };
    // Selection ring drawn around the face.
    let ringed = container(face_el).padding(3).style(move |theme: &Theme| {
        let t = tokens(theme);
        container::Style {
            border: border::rounded(radius_lg() + 3.0)
                .width(2)
                .color(if selected {
                    t.primary
                } else {
                    iced::Color::TRANSPARENT
                }),
            ..container::Style::default()
        }
    });
    let mut layers = stack![ringed].clip(false);
    if let Some(b) = workspace_badge(unread, WorkspaceAt::Rail) {
        layers = layers.push(
            container(b)
                .width(Length::Fill)
                .align_x(iced::alignment::Horizontal::Right)
                .padding(iced::Padding::ZERO.right(-4.0).top(-4.0)),
        );
    }
    let indicator =
        container(space().width(4).height(if selected { 32 } else { 0 })).style(|theme: &Theme| {
            container::Style {
                background: Some(tokens(theme).text_primary.into()),
                border: border::rounded(
                    border::Radius::default()
                        .top_right(RADIUS_PILL)
                        .bottom_right(RADIUS_PILL),
                ),
                ..container::Style::default()
            }
        });
    let item = button(layers)
        .padding(0)
        .style(style::button_bare)
        .on_press(on_press.clone());
    let item = focus_ring(item, Some(on_press)).radius(radius_lg() + 3.0);
    let content =
        row![indicator, space().width(6), item, space().width(10)].align_y(Alignment::Center);
    tooltip(content, name, TooltipPosition::Right)
}

/// Dashed "+" tile at the end of the rail.
pub fn add_server_button<'a, Message: Clone + 'a>(
    label: impl Into<String>,
    on_press: Message,
) -> Element<'a, Message> {
    let tile = button(container(icon::sized(Icon::Plus, 20.0)).center(RAIL_ITEM))
        .padding(0)
        .style(|theme: &Theme, status| {
            let t = tokens(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(
                    if hovered {
                        t.hover_bg
                    } else {
                        iced::Color::TRANSPARENT
                    }
                    .into(),
                ),
                text_color: if hovered { t.primary } else { t.text_secondary },
                border: border::rounded(radius_lg()).width(1).color(t.border_strong),
                ..button::Style::default()
            }
        })
        .on_press(on_press.clone());
    let tile = focus_ring(tile, Some(on_press)).radius(radius_lg());
    let content = row![space().width(13), tile, space().width(13)];
    tooltip(content, label, TooltipPosition::Right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props() -> RoomListItemProps<()> {
        RoomListItemProps::new(AvatarSource::initials("Room", "room"), "room", ())
    }

    #[test]
    fn badge_and_bold_are_drawn_as_given() {
        let p = props();
        assert!(!p.is_bold());
        assert_eq!((p.badge_kind(), p.badge_count()), (None, 0));
        // A count without a kind draws nothing.
        let mut p = props();
        p.count = 3;
        assert_eq!((p.badge_kind(), p.badge_count()), (None, 0));
        // A dot: a kind with no count.
        p.count = 0;
        p.badge = Some(BadgeKind::Unread);
        p.bold = true;
        assert!(p.is_bold());
        assert_eq!(
            (p.badge_kind(), p.badge_count()),
            (Some(BadgeKind::Unread), 0)
        );
        p.count = 4;
        p.badge = Some(BadgeKind::Mention);
        assert_eq!(
            (p.badge_kind(), p.badge_count()),
            (Some(BadgeKind::Mention), 4)
        );
    }
}

#[cfg(test)]
mod rc_tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    /// The Rustket look keeps today's row: a dimmed muted read title, the
    /// link-coloured time and bright preview while bold, a bold title.
    #[test]
    fn rustket_row_look_is_unchanged() {
        let l = IndicatorLook::Rustket;
        for t in [&LIGHT, &DARK] {
            let bold = row_colors(t, l, true, false);
            assert_eq!(
                (bold.title, bold.icon, bold.time, bold.preview),
                (t.text_primary, t.text_secondary, t.link, t.text_primary)
            );
            let read = row_colors(t, l, false, false);
            assert_eq!(
                (read.title, read.icon, read.time, read.preview),
                (
                    t.text_primary,
                    t.text_secondary,
                    t.text_hint,
                    t.text_secondary
                )
            );
            let muted = row_colors(t, l, false, true);
            assert_eq!((muted.title, muted.time), (t.text_hint, t.text_hint));
            assert_eq!(row_colors(t, l, true, true), bold);
        }
        assert_eq!(row_fonts(l, true), (font_bold(), None));
        assert_eq!(row_fonts(l, false), (font_regular(), None));
    }

    /// Rocket.Chat: highlighted rows bright at weight 500 (title, icon and
    /// preview), read rows secondary at 400, the time always a hint, muted
    /// rooms never dimmed.
    #[test]
    fn rocketchat_row_look_follows_highlight_only() {
        let l = IndicatorLook::RocketChat;
        for t in [&LIGHT, &DARK] {
            for muted in [false, true] {
                let hi = row_colors(t, l, true, muted);
                assert_eq!(
                    (hi.title, hi.icon, hi.time, hi.preview),
                    (t.text_primary, t.text_primary, t.text_hint, t.text_primary)
                );
                let read = row_colors(t, l, false, muted);
                assert_eq!(
                    (read.title, read.icon, read.time, read.preview),
                    (
                        t.text_secondary,
                        t.text_secondary,
                        t.text_hint,
                        t.text_secondary
                    )
                );
            }
        }
        assert_eq!(row_fonts(l, true), (font_medium(), Some(font_medium())));
        assert_eq!(row_fonts(l, false), (font_regular(), Some(font_regular())));
    }

    /// A row with an uncapped counter and a tooltip builds in both looks.
    #[test]
    fn uncapped_badge_with_tooltip_builds() {
        let mut p = RoomListItemProps::new(AvatarSource::initials("Room", "room"), "room", ());
        p.badge = Some(BadgeKind::Thread);
        p.count = 137;
        p.cap = BadgeCap::None;
        p.badge_tip = Some("137 unread threaded messages".into());
        p.bold = true;
        assert_eq!(p.badge_count(), 137);
        for look in [IndicatorLook::Rustket, IndicatorLook::RocketChat] {
            crate::theme::with_indicator_look(look, || {
                let _: Element<'_, ()> = room_list_item(p.clone());
                let _: Element<'_, ()> =
                    section_header_rc("Channels", true, None, None, None, true, ());
            });
        }
    }
}
