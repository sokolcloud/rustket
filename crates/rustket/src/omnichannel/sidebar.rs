//! Omnichannel groups of the sidebar (`docs/omnichannel.md` §6): "Queued
//! chats" (manual selection queue, rows are inquiries, not subscriptions),
//! "Chats in progress" and "On hold" (the generic omnichannel section split).

use iced::Element;
use iced::widget::Column;
use rust_i18n::t;
use std::collections::BTreeSet;

use rustket_core::omnichannel::room::month_key;
use rustket_core::omnichannel::room_list::{
    OmnichannelSectionKind, QueueRow, decor, queue_section,
};
use rustket_core::room_list::{RoomKind, RoomListItem, RoomListOptions, Section, SectionKind};
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::widget::{AvatarSource, RoomListItemProps, room_list_item, section_header};

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

/// Whether omnichannel rooms are shown at all: `Livechat_enabled` and
/// `view-l-room` (the web drops them from the room list otherwise). Shown
/// while the routing config is still loading.
pub fn shown(sid: &ServerId) -> bool {
    super::shared::get(sid).is_some_and(|o| o.accessible)
}

/// Splits the generic omnichannel section into "Chats in progress" and "On
/// hold" (empty groups omitted).
pub fn split_groups<'a>(ws: &Workspace, section: &'a Section) -> Vec<Group<'a>> {
    let (hold, open): (Vec<&RoomListItem>, Vec<&RoomListItem>) =
        section.items.iter().partition(|i| on_hold(ws, &i.rid));
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
    if !shown(ctx.server_id) {
        return list;
    }
    for s in sections
        .iter()
        .filter(|s| s.kind == SectionKind::Omnichannel)
    {
        for g in split_groups(ctx.workspace, s) {
            list = push_group(list, g, ctx, filter, row);
        }
    }
    list
}

/// The subscribed rooms in the order the sidebar draws them, without
/// collapsed groups (Alt+Up/Down): the omnichannel groups first, then the
/// other sections.
pub fn step_order<'a>(
    sid: &ServerId,
    ws: &Workspace,
    sections: &'a [Section],
    collapsed: &BTreeSet<String>,
) -> Vec<&'a RoomListItem> {
    let mut out = Vec::new();
    let (omni, rest): (Vec<&Section>, Vec<&Section>) = sections
        .iter()
        .partition(|s| s.kind == SectionKind::Omnichannel);
    if shown(sid) {
        for s in omni {
            for g in split_groups(ws, s) {
                if !collapsed.contains(section_key(g.kind)) {
                    out.extend(g.rooms);
                }
            }
        }
    }
    for s in rest {
        if !collapsed.contains(s.kind.i18n_key()) {
            out.extend(s.items.iter());
        }
    }
    out
}

/// Row decorations of a livechat room (§6): source icon (a warning while
/// over the MAC limit) and the visitor's status.
pub fn decorate<M>(p: &mut RoomListItemProps<M>, item: &RoomListItem, ctx: &SidebarCtx<'_>) {
    if item.kind != RoomKind::Omnichannel {
        return;
    }
    let Some(e) = ctx.workspace.room(&item.rid) else {
        return;
    };
    let over = super::shared::get(ctx.server_id).is_some_and(|o| o.over_mac);
    let month = month_key(chrono::Local::now());
    let d = decor(e, over, &month);
    p.icon = Some(if d.over_mac {
        Icon::TriangleAlert
    } else {
        super::panel::info::source_icon(d.source)
    });
    if let Some(st) = d.visitor_status {
        p.presence = Some(crate::workspace::sidebar::presence(st));
    }
}
