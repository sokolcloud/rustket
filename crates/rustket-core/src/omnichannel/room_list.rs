//! Omnichannel sidebar groups (`docs/omnichannel.md` §6): "Queued chats",
//! "Chats in progress" and "On hold". Additive to [`crate::room_list`]: the UI
//! replaces the generic `SectionKind::Omnichannel` section with these.

use rocketchat::RcDate;
use rocketchat::models::{Inquiry, RoomType, SourceType, UserStatus};

use super::OmnichannelState;
use super::room::room_over_mac;
use crate::room_list::{
    Preview, RoomListItem, RoomListOptions, Section, SectionKind, item, preview,
};
use crate::workspace::{RoomEntry, Workspace};

/// Omnichannel sidebar group, in display order (placed first, hidden when empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OmnichannelSectionKind {
    /// Manual-selection queue (`Incoming_Livechats`).
    Queued,
    /// `Open_Livechats`.
    Open,
    /// `On_Hold_Chats`.
    OnHold,
}

impl OmnichannelSectionKind {
    /// Locale keys (to be defined by the UI's locale file).
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Queued => "sidebar.omnichannel_queued",
            Self::Open => "sidebar.omnichannel_open",
            Self::OnHold => "sidebar.omnichannel_on_hold",
        }
    }
}

/// Row decorations of an omnichannel room or queue item.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OmnichannelDecor {
    /// Icon: widget = livechat bubble, email = mail, sms, else headset.
    pub source: SourceType,
    /// Icon colour.
    pub visitor_status: Option<UserStatus>,
    /// 1..5 when set (shown only with the EE priorities feature).
    pub priority: Option<i64>,
    /// MAC limit warning icon.
    pub over_mac: bool,
}

/// A queued inquiry as a sidebar row (always shown with an alert).
#[derive(Debug, Clone, PartialEq)]
pub struct QueueRow {
    pub inquiry_id: String,
    pub rid: String,
    pub name: String,
    pub preview: Option<Preview>,
    pub ts: Option<RcDate>,
    pub decor: OmnichannelDecor,
}

/// One omnichannel group.
#[derive(Debug, Clone, PartialEq)]
pub struct OmnichannelSection {
    pub kind: OmnichannelSectionKind,
    /// Queue rows (`Queued` only).
    pub queue: Vec<QueueRow>,
    /// Subscribed rooms (`Open` / `OnHold`), in the order of the source section.
    pub rooms: Vec<RoomListItem>,
}

fn priority_of(w: Option<i64>) -> Option<i64> {
    w.filter(|w| (1..=5).contains(w))
}

/// Decorations of a room entry.
pub fn decor(e: &RoomEntry, over_mac: bool, month: &str) -> OmnichannelDecor {
    let Some(room) = e.room.as_ref() else {
        return OmnichannelDecor {
            visitor_status: e
                .sub
                .as_ref()
                .and_then(|s| s.v.as_ref())
                .and_then(|v| v.status),
            ..Default::default()
        };
    };
    OmnichannelDecor {
        source: room
            .omnichannel_source()
            .map(|s| s.kind)
            .unwrap_or_default(),
        visitor_status: room.visitor().and_then(|v| v.status).or_else(|| {
            e.sub
                .as_ref()
                .and_then(|s| s.v.as_ref())
                .and_then(|v| v.status)
        }),
        priority: priority_of(room.priority_weight),
        over_mac: room_over_mac(room, over_mac, month),
    }
}

fn queue_row(ws: &Workspace, inq: &Inquiry, use_real_names: bool) -> QueueRow {
    QueueRow {
        inquiry_id: inq.id.clone(),
        rid: inq.rid.clone(),
        name: if inq.name.is_empty() {
            inq.v.display_name().to_owned()
        } else {
            inq.name.clone()
        },
        preview: inq
            .last_message
            .as_deref()
            .map(|m| preview(ws, m, RoomType::Livechat, false, use_real_names)),
        ts: inq.ts,
        decor: OmnichannelDecor {
            source: inq.source.as_ref().map(|s| s.kind).unwrap_or_default(),
            visitor_status: inq.v.status,
            priority: priority_of(inq.priority_weight),
            over_mac: false,
        },
    }
}

fn is_on_hold(e: &RoomEntry) -> bool {
    e.sub.as_ref().is_some_and(|s| s.is_on_hold())
        || e.room.as_ref().is_some_and(|r| r.on_hold == Some(true))
}

/// The "Queued chats" group: only with the manual queue enabled, filtered by
/// `filter` (case-insensitive name substring); `None` when empty.
pub fn queue_section(
    ws: &Workspace,
    omni: &OmnichannelState,
    opts: &RoomListOptions,
    filter: &str,
) -> Option<OmnichannelSection> {
    if !omni.manual_queue {
        return None;
    }
    let q = filter.trim().to_lowercase();
    let queue: Vec<QueueRow> = omni
        .queue
        .items()
        .into_iter()
        // Rooms the agent already has a subscription for are listed as rooms.
        .filter(|i| ws.subscription(&i.rid).is_none())
        .map(|i| queue_row(ws, i, opts.use_real_names))
        .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q))
        .collect();
    (!queue.is_empty()).then_some(OmnichannelSection {
        kind: OmnichannelSectionKind::Queued,
        queue,
        rooms: Vec::new(),
    })
}

/// Splits the generic omnichannel section of [`crate::room_list::compute`] into
/// "Chats in progress" and "On hold" (empty groups omitted). Other sections are
/// ignored.
pub fn split_section(ws: &Workspace, section: &Section) -> Vec<OmnichannelSection> {
    if !section.is(SectionKind::Omnichannel) {
        return Vec::new();
    }
    let (mut open, mut hold) = (Vec::new(), Vec::new());
    for it in &section.items {
        if ws.room(&it.rid).is_some_and(is_on_hold) {
            hold.push(it.clone());
        } else {
            open.push(it.clone());
        }
    }
    [
        (OmnichannelSectionKind::Open, open),
        (OmnichannelSectionKind::OnHold, hold),
    ]
    .into_iter()
    .filter(|(_, rooms)| !rooms.is_empty())
    .map(|(kind, rooms)| OmnichannelSection {
        kind,
        queue: Vec::new(),
        rooms,
    })
    .collect()
}

/// All omnichannel groups in display order: the queue, then the split of the
/// omnichannel section of `sections` (from [`crate::room_list::compute`]).
pub fn sections(
    ws: &Workspace,
    omni: &OmnichannelState,
    opts: &RoomListOptions,
    filter: &str,
    sections: &[Section],
) -> Vec<OmnichannelSection> {
    let mut out: Vec<OmnichannelSection> =
        queue_section(ws, omni, opts, filter).into_iter().collect();
    for s in sections {
        out.extend(split_section(ws, s));
    }
    out
}

/// The list item of an omnichannel room (whatever its group), for search results.
pub fn room_item(ws: &Workspace, rid: &str, opts: &RoomListOptions) -> Option<RoomListItem> {
    ws.room(rid).map(|e| item(ws, e, opts))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::omnichannel::queue::QueueContext;
    use crate::room_list::compute;
    use crate::test_util::{me, room, sub, ts};
    use crate::workspace::Change;
    use rocketchat::models::{InquiryEventKind, InquiryStatus, RoutingConfig};
    use serde_json::json;

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        for (rid, hold) in [("l1", false), ("l2", true)] {
            let mut s = sub(rid, &format!("Visitor {rid}"), RoomType::Livechat);
            s.on_hold = Some(hold).filter(|h| *h);
            ws.apply_subscription(Change::Inserted, s);
            let mut r = room(rid, &format!("Visitor {rid}"), RoomType::Livechat);
            r.open = Some(true);
            r.source = Some(json!({"type":"widget"}));
            r.v = Some(json!({"_id":"v","status":"away"}));
            r.priority_weight = Some(if hold { 2 } else { 99 });
            ws.apply_room(Change::Inserted, r);
        }
        ws.apply_subscription(Change::Inserted, sub("c1", "general", RoomType::Channel));
        ws
    }

    fn manual(omni: &mut OmnichannelState) {
        omni.manual_queue = true;
        omni.routing = Some(RoutingConfig {
            show_queue: true,
            ..Default::default()
        });
    }

    #[test]
    fn split_and_queue() {
        let ws = ws();
        let opts = RoomListOptions::default();
        let base = compute(&ws, &opts, "");
        let mut omni = OmnichannelState {
            queue: crate::omnichannel::queue::InquiryQueue::new(QueueContext {
                me: "me".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let inq = |id: &str, rid: &str| Inquiry {
            id: id.into(),
            rid: rid.into(),
            name: format!("Queued {id}"),
            status: InquiryStatus::Queued,
            ts: Some(ts(5)),
            ..Default::default()
        };
        omni.queue.apply(InquiryEventKind::Added, inq("i1", "q1"));
        // Already subscribed (just taken): not shown as a queue row.
        omni.queue.apply(InquiryEventKind::Added, inq("i2", "l1"));
        let groups = sections(&ws, &omni, &opts, "", &base);
        let kinds: Vec<_> = groups.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [OmnichannelSectionKind::Open, OmnichannelSectionKind::OnHold],
            "no queue group without the manual queue"
        );
        manual(&mut omni);
        let groups = sections(&ws, &omni, &opts, "", &base);
        assert_eq!(groups[0].kind, OmnichannelSectionKind::Queued);
        assert_eq!(groups[0].queue.len(), 1);
        assert_eq!(groups[0].queue[0].rid, "q1");
        assert_eq!(groups[1].rooms[0].rid, "l1");
        assert_eq!(groups[2].rooms[0].rid, "l2");
        assert!(
            sections(&ws, &omni, &opts, "nomatch", &base)[0].kind != OmnichannelSectionKind::Queued
        );
        // Decorations.
        let d = decor(ws.room("l2").unwrap(), false, "2026-11");
        assert_eq!(d.source, SourceType::Widget);
        assert_eq!(d.visitor_status, Some(UserStatus::Away));
        assert_eq!(d.priority, Some(2));
        assert_eq!(
            decor(ws.room("l1").unwrap(), false, "2026-11").priority,
            None
        );
        assert!(decor(ws.room("l1").unwrap(), true, "2026-11").over_mac);
        // The existing grouping is untouched.
        assert!(base.iter().any(|s| s.is(SectionKind::Omnichannel)));
        assert_eq!(
            OmnichannelSectionKind::OnHold.i18n_key(),
            "sidebar.omnichannel_on_hold"
        );
    }
}
