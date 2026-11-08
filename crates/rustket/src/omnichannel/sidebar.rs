//! Omnichannel groups of the sidebar (`docs/omnichannel.md` §6): "Queued
//! chats" (manual selection queue, rows are inquiries, not subscriptions),
//! "Chats in progress" and "On hold" (the generic omnichannel section split),
//! and the Contact Center button in the sidebar header.

use iced::Element;
use iced::widget::Column;
use rust_i18n::t;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::omnichannel::room_list::{OmnichannelSectionKind, QueueRow, queue_section};
use rustket_core::permissions::has_permission;
use rustket_core::room_list::{RoomListItem, RoomListOptions, Section, SectionKind};
use rustket_ui::icon::Icon;
use rustket_ui::widget::{
    AvatarSource, RoomListItemProps, icon_button, room_list_item, section_header,
};

use crate::workspace::sidebar::{Message, SidebarCtx, preview_text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Collapse/expand a group (persisted key).
    Toggle(String),
    /// A queued chat: preview it (manual selection).
    OpenQueued {
        inquiry_id: String,
        rid: String,
    },
    ContactCenter,
}

/// Persisted collapse key of a group.
pub fn section_key(kind: OmnichannelSectionKind) -> &'static str {
    match kind {
        OmnichannelSectionKind::Queued => "omnichannel.section_queued",
        OmnichannelSectionKind::Open => "omnichannel.section_open",
        OmnichannelSectionKind::OnHold => "omnichannel.section_on_hold",
    }
}

/// One omnichannel group: queue rows (owned) or room rows (borrowed from
/// the sidebar's sections).
pub struct Group<'a> {
    pub kind: OmnichannelSectionKind,
    pub queue: Vec<QueueRow>,
    pub rooms: Vec<&'a RoomListItem>,
}

/// The "Queued chats" group, placed first; `None` without the manual queue
/// (or when it is empty).
pub fn queue_group(ctx: &SidebarCtx<'_>, filter: &str) -> Option<Group<'static>> {
    let omni = super::shared::get(ctx.server_id)?;
    if !omni.enabled {
        return None;
    }
    let opts = RoomListOptions::from_prefs(ctx.prefs, ctx.workspace);
    queue_section(ctx.workspace, &omni, &opts, filter).map(|s| Group {
        kind: s.kind,
        queue: s.queue,
        rooms: Vec::new(),
    })
}

fn on_hold(ws: &rustket_core::Workspace, rid: &str) -> bool {
    ws.room(rid).is_some_and(|e| {
        e.sub.as_ref().is_some_and(|s| s.is_on_hold())
            || e.room.as_ref().is_some_and(|r| r.on_hold == Some(true))
    })
}

/// Splits the generic omnichannel section into "Chats in progress" and "On
/// hold" (empty groups omitted).
pub fn split_groups<'a>(ctx: &SidebarCtx<'_>, section: &'a Section) -> Vec<Group<'a>> {
    let (hold, open): (Vec<&RoomListItem>, Vec<&RoomListItem>) = section
        .items
        .iter()
        .partition(|i| on_hold(ctx.workspace, &i.rid));
    [
        (OmnichannelSectionKind::Open, open),
        (OmnichannelSectionKind::OnHold, hold),
    ]
    .into_iter()
    .filter(|(_, rooms)| !rooms.is_empty())
    .map(|(kind, rooms)| Group {
        kind,
        queue: Vec::new(),
        rooms,
    })
    .collect()
}

fn header<'a>(
    ctx: &SidebarCtx<'a>,
    kind: OmnichannelSectionKind,
    count: usize,
    filter: &str,
) -> (Element<'a, Message>, bool) {
    let key = section_key(kind);
    let collapsed = filter.trim().is_empty() && ctx.collapsed.contains(key);
    (
        section_header(
            t!(key).into_owned(),
            collapsed,
            Some(u32::try_from(count).unwrap_or(u32::MAX)),
            Message::Omni(Msg::Toggle(key.to_owned())),
        ),
        collapsed,
    )
}

/// Appends `group` to the sidebar list; room rows are drawn by `row`.
pub fn push_group<'a>(
    mut list: Column<'a, Message>,
    group: Group<'a>,
    ctx: &SidebarCtx<'a>,
    filter: &str,
    row: &dyn Fn(&'a RoomListItem) -> Element<'a, Message>,
) -> Column<'a, Message> {
    let count = group.queue.len() + group.rooms.len();
    let (head, collapsed) = header(ctx, group.kind, count, filter);
    list = list.push(head);
    if collapsed {
        return list;
    }
    for q in group.queue {
        let mut p = RoomListItemProps::new(
            AvatarSource::initials(&q.name, &q.rid),
            q.name.clone(),
            Message::Omni(Msg::OpenQueued {
                inquiry_id: q.inquiry_id.clone(),
                rid: q.rid.clone(),
            }),
        );
        p.subtitle = q.preview.as_ref().map(preview_text);
        p.time =
            q.ts.map(|ts| crate::format::short_time(ts, chrono::Local::now(), ctx.prefs.clock_24h));
        p.thread_unread = true;
        p.is_selected = ctx.active_room == Some(q.rid.as_str());
        p.icon = Some(super::panel::info::source_icon(q.decor.source));
        p.presence = q
            .decor
            .visitor_status
            .map(crate::workspace::sidebar::presence);
        list = list.push(room_list_item(p));
    }
    for item in group.rooms {
        list = list.push(row(item));
    }
    list
}

/// Appends the queue group and the split omnichannel section of `sections`.
pub fn push_groups<'a>(
    mut list: Column<'a, Message>,
    queue: Option<Group<'static>>,
    sections: &'a [Section],
    ctx: &SidebarCtx<'a>,
    filter: &str,
    row: &dyn Fn(&'a RoomListItem) -> Element<'a, Message>,
) -> Column<'a, Message> {
    if let Some(q) = queue {
        list = push_group(list, q, ctx, filter, row);
    }
    for s in sections
        .iter()
        .filter(|s| s.kind == SectionKind::Omnichannel)
    {
        for g in split_groups(ctx, s) {
            list = push_group(list, g, ctx, filter, row);
        }
    }
    list
}

/// The Contact Center button of the sidebar header.
pub fn header_button<'a>(ctx: &SidebarCtx<'a>) -> Option<Element<'a, Message>> {
    let omni = super::shared::get(ctx.server_id)?;
    (omni.enabled && has_permission(ctx.workspace, perm::VIEW_OMNICHANNEL_CONTACT_CENTER, None))
        .then(|| {
            icon_button(
                Icon::Contact,
                Some(Message::Omni(Msg::ContactCenter)),
                t!("omnichannel.contact_center"),
            )
        })
}
