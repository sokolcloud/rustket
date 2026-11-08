//! Left rail: one icon per server with unread badges, add-server and settings
//! buttons. Strings: `locales/rail.yml`.

use iced::widget::{Column, column, container, scrollable, space, stack};
use iced::{Alignment, Element, Length, Theme, border};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{RAIL_WIDTH, SPACE_2, SPACE_3, tokens};
use rustket_ui::widget::{
    AvatarSource, BadgeKind, RailIcon, add_server_button, icon_button, workspace_rail_item,
};

#[derive(Debug, Clone)]
pub enum Message {
    Select(ServerId),
    Add,
    Settings,
}

pub struct RailEntry {
    pub id: ServerId,
    pub name: String,
    pub avatar: AvatarSource,
    /// (unread rooms, mentions)
    pub unread: (u64, u64),
    pub selected: bool,
    /// Logged out (token rejected, signed out by the server, never signed
    /// in): nothing arrives from this server until the user signs in.
    pub signed_out: bool,
}

pub fn badge(unread: (u64, u64)) -> Option<(u32, BadgeKind)> {
    let (rooms, mentions) = unread;
    if mentions > 0 {
        Some((crate::format::count_u32(mentions), BadgeKind::Mention))
    } else if rooms > 0 {
        Some((0, BadgeKind::Unread))
    } else {
        None
    }
}

/// A small "signed out" emblem on the bottom-right corner of the face, or
/// an empty placeholder when `shown` is false.
fn signed_out_marker(item: Element<'_, Message>, shown: bool) -> Element<'_, Message> {
    if !shown {
        return stack![item, iced::widget::space()].clip(false).into();
    }
    let emblem = container(icon::sized(Icon::LogOut, 10.0))
        .center(16)
        .style(|theme: &Theme| {
            let t = tokens(theme);
            container::Style {
                text_color: Some(t.on_danger),
                background: Some(t.danger.into()),
                border: border::rounded(8).width(2).color(t.surface_rail),
                ..container::Style::default()
            }
        });
    stack![
        item,
        container(emblem)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::End)
            .padding(iced::Padding::ZERO.right(6.0).bottom(-2.0)),
    ]
    .clip(false)
    .into()
}

pub fn view<'a>(entries: Vec<RailEntry>) -> Element<'a, Message> {
    let items = entries.into_iter().map(|e| {
        let name = if e.signed_out {
            t!("rail.signed_out", name = e.name).into_owned()
        } else {
            e.name
        };
        let item = workspace_rail_item(
            RailIcon::Avatar(e.avatar),
            name,
            badge(e.unread),
            e.selected,
            Message::Select(e.id),
        );
        // Always wrapped, so the item keeps its widget state when the
        // server signs out or back in.
        signed_out_marker(item, e.signed_out)
    });
    let list = Column::with_children(items)
        .push(add_server_button(t!("rail.add_server"), Message::Add))
        .spacing(SPACE_3)
        .align_x(Alignment::Start)
        // Inside the scrollable so the badges drawn above the first item
        // (offset -4 px) are not clipped by its viewport.
        .padding(iced::Padding::ZERO.top(SPACE_2));
    container(
        column![
            scrollable(list)
                .height(Length::Fill)
                .style(style::scrollable),
            container(icon_button(
                Icon::Settings,
                Some(Message::Settings),
                t!("rail.settings")
            ))
            .center_x(Length::Fill),
            space().height(SPACE_2),
        ]
        .spacing(SPACE_2)
        .padding(iced::Padding::ZERO.top(SPACE_3 - SPACE_2).bottom(SPACE_3)),
    )
    .width(RAIL_WIDTH)
    .height(Length::Fill)
    .style(style::container_rail)
    .into()
}
