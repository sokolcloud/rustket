//! Left rail: one icon per server with unread badges, add-server and settings
//! buttons. Strings: `locales/rail.yml`.

use iced::widget::{Column, column, container, scrollable, space};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{RAIL_WIDTH, SPACE_2, SPACE_3};
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

pub fn view<'a>(entries: Vec<RailEntry>) -> Element<'a, Message> {
    let items = entries.into_iter().map(|e| {
        workspace_rail_item(
            RailIcon::Avatar(e.avatar),
            e.name,
            badge(e.unread),
            e.selected,
            Message::Select(e.id),
        )
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
