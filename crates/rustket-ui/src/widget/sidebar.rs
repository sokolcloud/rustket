//! Sidebar building blocks: room rows, section headers, workspace rail items.

use iced::widget::{button, column, container, row, space, stack, text};
use iced::{Alignment, Element, Length, Theme, border};

use super::avatar::{AvatarShape, AvatarSource, avatar, face};
use super::badge::{BadgeKind, badge, dot};
use super::ellipsis::ellipsis;
use super::misc::{TooltipPosition, tooltip};
use super::presence::Presence;
use crate::icon::{self, Icon};
use crate::style;
use crate::theme::{
    FONT_BOLD, FONT_SEMIBOLD, RADIUS_LG, RADIUS_PILL, SPACE_1, SPACE_2, TEXT_BODY, TEXT_CAPTION,
    TEXT_MICRO, tokens,
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
    pub unread: u32,
    pub user_mentions: u32,
    pub group_mentions: u32,
    pub thread_unread: bool,
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
    pub on_press: Message,
}

impl<Message> RoomListItemProps<Message> {
    pub fn new(avatar: AvatarSource, title: impl Into<String>, on_press: Message) -> Self {
        Self {
            avatar,
            title: title.into(),
            subtitle: None,
            time: None,
            unread: 0,
            user_mentions: 0,
            group_mentions: 0,
            thread_unread: false,
            is_muted: false,
            is_selected: false,
            presence: None,
            icon: None,
            typing: false,
            typing_label: String::new(),
            density: Density::Extended,
            on_press,
        }
    }

    pub fn badge_kind(&self) -> Option<BadgeKind> {
        BadgeKind::for_room(
            self.user_mentions,
            self.group_mentions,
            self.unread,
            self.thread_unread,
            self.is_muted,
        )
    }
}

fn room_badge<'a, Message: 'a>(p: &RoomListItemProps<Message>) -> Option<Element<'a, Message>> {
    let kind = p.badge_kind()?;
    let count = if p.unread > 0 {
        p.unread
    } else {
        p.user_mentions.max(p.group_mentions)
    };
    Some(if count == 0 {
        dot(kind, 8.0)
    } else {
        badge(count, kind)
    })
}

/// A sidebar row (Rocket.Chat density). Hover/selected backgrounds come from the style.
pub fn room_list_item<'a, Message: Clone + 'a>(
    p: RoomListItemProps<Message>,
) -> Element<'a, Message> {
    let bold = p.unread > 0 || p.user_mentions > 0 || p.group_mentions > 0;
    let dim = p.is_muted && !bold;
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

    let title_font = if bold {
        FONT_BOLD
    } else {
        crate::theme::DEFAULT_FONT
    };
    let mut title_row = row![].spacing(SPACE_1).align_y(Alignment::Center);
    if condensed && let Some(presence) = p.presence {
        title_row = title_row.push(super::presence::presence_dot(presence, 8.0));
    }
    if let Some(i) = p.icon {
        title_row = title_row.push(icon::sized(i, 14.0).style(style::text_secondary));
    }
    title_row = title_row.push(
        ellipsis(p.title.clone())
            .size(TEXT_BODY)
            .font(title_font)
            .style(move |theme: &Theme| {
                let t = tokens(theme);
                Some(if dim { t.text_hint } else { t.text_primary })
            }),
    );
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
                    .style(move |theme: &Theme| {
                        let t = tokens(theme);
                        text::Style {
                            color: Some(if bold && !dim { t.link } else { t.text_hint }),
                        }
                    }),
            );
        }
        let preview: Element<'a, Message> = if p.typing {
            ellipsis(p.typing_label.clone())
                .size(TEXT_CAPTION + 1.0)
                .style(|theme: &Theme| Some(tokens(theme).link))
                .into()
        } else {
            ellipsis(p.subtitle.clone().unwrap_or_default())
                .size(TEXT_CAPTION + 1.0)
                .style(move |theme: &Theme| {
                    let t = tokens(theme);
                    Some(if bold && !dim {
                        t.text_primary
                    } else {
                        t.text_secondary
                    })
                })
                .into()
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

    button(container(content).center_y(Length::Fill))
        .width(Length::Fill)
        .height(if condensed { 32.0 } else { 56.0 })
        .padding([0.0, SPACE_2])
        .style(style::button_list_item(p.is_selected))
        .on_press(p.on_press)
        .into()
}

/// Collapsible sidebar group header ("Favorites", "Channels"…).
pub fn section_header<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    collapsed: bool,
    count: Option<u32>,
    on_toggle: Message,
) -> Element<'a, Message> {
    let mut content = row![
        icon::sized(
            if collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            },
            14.0
        ),
        text(title.into().to_uppercase())
            .size(TEXT_MICRO + 1.0)
            .font(FONT_SEMIBOLD)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center);
    if let Some(count) = count {
        content = content.push(
            text(count.to_string())
                .size(TEXT_MICRO + 1.0)
                .style(style::text_hint),
        );
    }
    button(content)
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
        .on_press(on_toggle)
        .into()
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
    let face_el: Element<'a, Message> = match &icon_or_initials {
        RailIcon::Avatar(source) => face(source, RAIL_ITEM, AvatarShape::Rounded),
        RailIcon::Icon(i) => container(icon::sized(*i, 20.0))
            .center(RAIL_ITEM)
            .style(|theme: &Theme| {
                let t = tokens(theme);
                container::Style {
                    text_color: Some(t.text_secondary),
                    background: Some(t.surface_raised.into()),
                    border: border::rounded(RADIUS_LG).width(1).color(t.border),
                    ..container::Style::default()
                }
            })
            .into(),
    };
    // Selection ring drawn around the face.
    let ringed = container(face_el).padding(3).style(move |theme: &Theme| {
        let t = tokens(theme);
        container::Style {
            border: border::rounded(RADIUS_LG + 3.0)
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
    if let Some((count, kind)) = unread {
        let b = if count == 0 {
            dot(kind, 10.0)
        } else {
            badge(count, kind)
        };
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
        .on_press(on_press);
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
                border: border::rounded(RADIUS_LG).width(1).color(t.border_strong),
                ..button::Style::default()
            }
        })
        .on_press(on_press);
    let content = row![space().width(13), tile, space().width(13)];
    tooltip(content, label, TooltipPosition::Right)
}
