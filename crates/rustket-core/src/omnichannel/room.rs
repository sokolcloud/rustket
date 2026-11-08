//! Pure gating of an omnichannel room view (`docs/omnichannel.md` §7, §8): the
//! composer state, header quick actions and the room info panel fields.

use chrono::{DateTime, Datelike, Local, Utc};
use rocketchat::RcDate;
use rocketchat::models::{
    AgentStatus, CustomFieldDef, LicenseInfo, OmnichannelSource, Room, RoomType, SourceType,
    Subscription, UserStatus,
};
use serde_json::Value;

use super::OmnichannelState;
use super::permissions as perm;
use super::permissions::settings;
use crate::permissions::has_permission;
use crate::workspace::Workspace;

/// What replaces (or is) the composer, first match wins (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerState {
    /// "This conversation is already closed."
    Closed,
    /// The workspace exceeded the monthly active contacts limit.
    OverMac,
    /// "This chat is on-hold due to inactivity" + Resume.
    OnHold,
    /// Queued and not served: "You are in preview mode" + Take it!
    Preview,
    /// Served by someone else and not subscribed: read only + Join.
    Join,
    Normal,
}

/// Why "Take it!" is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeBlock {
    /// "You cannot take new conversations because you're offline".
    Offline,
    /// "… because you're unavailable".
    Unavailable,
}

/// `YYYY-MM` of the local month (MAC activity key).
pub fn month_key(now: DateTime<Local>) -> String {
    format!("{:04}-{:02}", now.year(), now.month())
}

/// `useIsRoomOverMacLimit`: the workspace is over the limit, the room is open
/// and its contact was not active in `month` (`YYYY-MM`).
pub fn room_over_mac(room: &Room, over_mac: bool, month: &str) -> bool {
    over_mac
        && room.t == RoomType::Livechat
        && room.closed_at.is_none()
        && !room
            .visitor()
            .is_some_and(|v| v.activity.iter().any(|m| m == month))
}

fn on_hold(room: &Room, sub: Option<&Subscription>) -> bool {
    room.on_hold == Some(true) || sub.is_some_and(Subscription::is_on_hold)
}

/// The composer state of an omnichannel room (§7.1). `room_over_mac` from
/// [`room_over_mac`].
pub fn composer_state(
    room: &Room,
    sub: Option<&Subscription>,
    me: &str,
    room_over_mac: bool,
) -> ComposerState {
    if room.t == RoomType::Voip {
        // Removed room type: read-only.
        return ComposerState::Closed;
    }
    if room.open != Some(true) {
        return ComposerState::Closed;
    }
    if room_over_mac {
        return ComposerState::OverMac;
    }
    if on_hold(room, sub) {
        return ComposerState::OnHold;
    }
    let served = room.served_by_agent();
    if served.is_none() && room.queued_at.is_some() {
        return ComposerState::Preview;
    }
    if sub.is_none() && served.as_ref().is_none_or(|s| s.id != me) {
        return ComposerState::Join;
    }
    ComposerState::Normal
}

/// Whether "Take it!" must be disabled, and why.
pub fn take_block(my_status: UserStatus, agent_status: AgentStatus) -> Option<TakeBlock> {
    // Unknown presence (not loaded yet) does not block, like the web client.
    if my_status == UserStatus::Offline {
        Some(TakeBlock::Offline)
    } else if !agent_status.is_available() {
        Some(TakeBlock::Unavailable)
    } else {
        None
    }
}

/// A header quick action (§7.2), in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAction {
    MoveToQueue,
    Forward,
    /// Send transcript via e-mail.
    TranscriptEmail,
    /// Export transcript as PDF (EE); only enabled for closed rooms.
    TranscriptPdf {
        enabled: bool,
    },
    PlaceOnHold,
    /// End conversation (danger).
    Close,
}

/// Everything [`quick_actions_for`] needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuickActionInput {
    pub room_open: bool,
    pub subscribed: bool,
    pub manager_or_monitor: bool,
    pub last_message_is_close: bool,
    pub last_message_from_visitor: bool,
    pub on_hold: bool,
    pub over_mac: bool,
    pub return_queue: bool,
    pub livechat_enterprise: bool,
    pub can_transfer: bool,
    pub can_send_transcript: bool,
    pub can_request_pdf: bool,
    pub can_close: bool,
    pub allow_manual_on_hold: bool,
    pub on_hold_engagement_only: bool,
}

/// `useQuickActions` (§7.2).
pub fn quick_actions_for(i: &QuickActionInput) -> Vec<QuickAction> {
    let room_open =
        i.room_open && (i.subscribed || i.manager_or_monitor) && !i.last_message_is_close;
    let mut out = Vec::new();
    if !i.over_mac && room_open && i.return_queue && i.subscribed {
        out.push(QuickAction::MoveToQueue);
    }
    if !i.over_mac && room_open && i.can_transfer {
        out.push(QuickAction::Forward);
    }
    if !i.over_mac {
        if i.can_send_transcript {
            out.push(QuickAction::TranscriptEmail);
        }
        if i.livechat_enterprise && i.can_request_pdf {
            out.push(QuickAction::TranscriptPdf {
                enabled: !i.room_open,
            });
        }
    }
    if i.livechat_enterprise
        && room_open
        && i.allow_manual_on_hold
        && !i.on_hold
        && i.subscribed
        && (!i.on_hold_engagement_only || !i.last_message_from_visitor)
    {
        out.push(QuickAction::PlaceOnHold);
    }
    if room_open && i.can_close {
        out.push(QuickAction::Close);
    }
    out
}

fn license_has(omni: &OmnichannelState, module: &str) -> bool {
    omni.license.as_ref().is_some_and(|l| l.has_module(module))
}

/// The quick actions of `room` for the current user. `room` may come from the
/// workspace or from `rooms.info` (closed rooms and unsubscribed chats a
/// manager opens are not in the workspace); `sub` is the user's subscription.
pub fn quick_actions(
    ws: &Workspace,
    omni: &OmnichannelState,
    room: &Room,
    sub: Option<&Subscription>,
    month: &str,
) -> Vec<QuickAction> {
    if room.t != RoomType::Livechat {
        return Vec::new();
    }
    let rid = room.id.as_str();
    let me = ws.me();
    let lm = room.last_message.as_deref();
    let has = |p: &str| has_permission(ws, p, Some(rid));
    let input = QuickActionInput {
        room_open: room.open == Some(true),
        subscribed: sub.is_some(),
        manager_or_monitor: me
            .is_some_and(|m| m.has_role(perm::ROLE_MANAGER) || m.has_role(perm::ROLE_MONITOR)),
        last_message_is_close: lm.and_then(|m| m.t.as_deref()) == Some("livechat-close"),
        last_message_from_visitor: lm.is_some_and(|m| m.is_from_visitor()),
        on_hold: on_hold(room, sub),
        over_mac: room_over_mac(room, omni.over_mac, month),
        return_queue: omni.routing.is_some_and(|r| r.return_queue),
        livechat_enterprise: license_has(omni, LicenseInfo::LIVECHAT_ENTERPRISE),
        can_transfer: has(perm::TRANSFER_LIVECHAT_GUEST),
        can_send_transcript: has(perm::SEND_OMNICHANNEL_CHAT_TRANSCRIPT),
        can_request_pdf: has(perm::REQUEST_PDF_TRANSCRIPT),
        can_close: has(perm::CLOSE_LIVECHAT_ROOM) || has(perm::CLOSE_OTHERS_LIVECHAT_ROOM),
        allow_manual_on_hold: ws.settings().bool_or(settings::ALLOW_MANUAL_ON_HOLD, false),
        on_hold_engagement_only: ws
            .settings()
            .bool_or(settings::MANUAL_ON_HOLD_ENGAGEMENT_ONLY, true),
    };
    quick_actions_for(&input)
}

/// The server's rule for `room.saveInfo`: served by me or
/// `save-others-livechat-room-info` (being subscribed is not enough).
pub fn can_edit_room_info(ws: &Workspace, room: &Room) -> bool {
    let me = ws.user_id().unwrap_or_default();
    room.served_by_agent()
        .is_some_and(|s| !me.is_empty() && s.id == me)
        || has_permission(ws, perm::SAVE_OTHERS_LIVECHAT_ROOM_INFO, Some(&room.id))
}

/// Visitor custom fields may be edited too (`edit-livechat-room-customfields`).
pub fn can_edit_custom_fields(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, perm::EDIT_LIVECHAT_ROOM_CUSTOMFIELDS, Some(rid))
}

/// `t: l` rooms cannot be left (`rooms.leave` → `error-not-allowed`); hiding works.
pub fn can_leave(room_type: RoomType) -> bool {
    !matches!(room_type, RoomType::Livechat | RoomType::Voip)
}

/// Label of a room's channel (`useOmnichannelSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelLabel {
    Livechat,
    Email,
    Sms,
    /// `source.alias` of an app/api source.
    Alias(String),
    CustomApp,
    CustomApi,
    CustomIntegration,
    /// Unknown type: `alias || id || type`.
    Raw(String),
}

/// Detail line of the channel field (`SourceField`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelDetail {
    None,
    Text(String),
    /// "External".
    External,
}

pub fn channel_label(source: &OmnichannelSource) -> ChannelLabel {
    let alias = source.alias.clone().filter(|a| !a.is_empty());
    match source.kind {
        SourceType::Widget => ChannelLabel::Livechat,
        SourceType::Email => ChannelLabel::Email,
        SourceType::Sms => ChannelLabel::Sms,
        SourceType::App => alias.map_or(ChannelLabel::CustomApp, ChannelLabel::Alias),
        SourceType::Api => alias.map_or(ChannelLabel::CustomApi, ChannelLabel::Alias),
        SourceType::Other => ChannelLabel::CustomIntegration,
        SourceType::Unknown => ChannelLabel::Raw(alias.or(source.id.clone()).unwrap_or_default()),
    }
}

pub fn channel_detail(source: &OmnichannelSource) -> ChannelDetail {
    match source.kind {
        SourceType::Widget => ChannelDetail::None,
        SourceType::Email => source
            .id
            .clone()
            .filter(|s| !s.is_empty())
            .map_or(ChannelDetail::None, ChannelDetail::Text),
        SourceType::App | SourceType::Api => source
            .label
            .clone()
            .filter(|s| !s.is_empty())
            .map_or(ChannelDetail::External, ChannelDetail::Text),
        _ => ChannelDetail::External,
    }
}

fn secs_between(from: RcDate, to: RcDate) -> i64 {
    (to.timestamp_millis() - from.timestamp_millis()).max(0) / 1000
}

/// `formatQueuedAt`: seconds from `queuedAt || ts` to `servedBy.ts`, or to now
/// while open and unserved, or to `closedAt`.
pub fn queue_time_secs(room: &Room, now: RcDate) -> Option<i64> {
    let start = room.queued_at.or(room.ts)?;
    if let Some(served) = room.served_by_agent().and_then(|s| s.ts) {
        return Some(secs_between(start, served));
    }
    if room.open == Some(true) {
        return Some(secs_between(start, now));
    }
    room.closed_at.map(|c| secs_between(start, c))
}

/// One row of the room info panel (§8), in display order.
#[derive(Debug, Clone, PartialEq)]
pub enum InfoField {
    Channel {
        kind: SourceType,
        label: ChannelLabel,
        detail: ChannelDetail,
    },
    /// Visitor details come from `livechat/visitors.info`.
    Contact {
        visitor_id: String,
    },
    /// Serving agent (name/avatar via `users.info`).
    Agent {
        user_id: String,
        username: Option<String>,
    },
    /// Name via `livechat/department/{id}`.
    Department {
        id: String,
    },
    Tags(Vec<String>),
    /// Markdown, inline.
    Topic(String),
    QueueTime {
        secs: i64,
    },
    ChatDuration {
        secs: i64,
    },
    CreatedAt(RcDate),
    ClosedAt(RcDate),
    TakenAt(RcDate),
    AvgResponseTime {
        secs: f64,
    },
    InactivityTime {
        secs: i64,
    },
    /// `(label, value)` of visible room custom fields with a value.
    CustomFields(Vec<(String, String)>),
    /// EE.
    Sla {
        id: String,
    },
    /// EE.
    Priority {
        id: String,
    },
}

/// The info panel rows of `room`. `custom_fields` are the definitions
/// (`livechat/custom-fields`); pass `can_view_custom_fields` from
/// `view-livechat-room-customfields`.
pub fn info_fields(
    room: &Room,
    now: RcDate,
    custom_fields: &[CustomFieldDef],
    can_view_custom_fields: bool,
) -> Vec<InfoField> {
    let mut out = Vec::new();
    if let Some(src) = room.omnichannel_source() {
        out.push(InfoField::Channel {
            kind: src.kind,
            label: channel_label(&src),
            detail: channel_detail(&src),
        });
    }
    if let Some(v) = room.visitor().filter(|v| !v.id.is_empty()) {
        out.push(InfoField::Contact { visitor_id: v.id });
    }
    let served = room.served_by_agent();
    if let Some(s) = &served {
        out.push(InfoField::Agent {
            user_id: s.id.clone(),
            username: s.username.clone(),
        });
    }
    if let Some(d) = room.department_id.clone().filter(|d| !d.is_empty()) {
        out.push(InfoField::Department { id: d });
    }
    if let Some(tags) = room.tags.clone().filter(|t| !t.is_empty()) {
        out.push(InfoField::Tags(tags));
    }
    if let Some(t) = room.topic.clone().filter(|t| !t.trim().is_empty()) {
        out.push(InfoField::Topic(t));
    }
    if let Some(secs) = queue_time_secs(room, now) {
        out.push(InfoField::QueueTime { secs });
    }
    if let (Some(ts), Some(closed)) = (room.ts, room.closed_at) {
        out.push(InfoField::ChatDuration {
            secs: secs_between(ts, closed),
        });
    }
    if let Some(ts) = room.ts {
        out.push(InfoField::CreatedAt(ts));
    }
    if let Some(c) = room.closed_at {
        out.push(InfoField::ClosedAt(c));
    }
    if let Some(t) = served.and_then(|s| s.ts) {
        out.push(InfoField::TakenAt(t));
    }
    if let Some(avg) = room
        .metrics
        .as_ref()
        .and_then(|m| m.get("response"))
        .and_then(|r| r.get("avg"))
        .and_then(Value::as_f64)
    {
        out.push(InfoField::AvgResponseTime { secs: avg });
    }
    if room.waiting_response != Some(true)
        && let Some(last) = room.response_by.as_ref().and_then(|r| r.last_message_ts)
    {
        out.push(InfoField::InactivityTime {
            secs: secs_between(last, now),
        });
    }
    if can_view_custom_fields {
        let values = room.livechat_data.as_ref().and_then(Value::as_object);
        let fields: Vec<(String, String)> = custom_fields
            .iter()
            .filter(|f| f.is_visible_room_field())
            .filter_map(|f| {
                let v = values?.get(&f.id)?;
                let text = match v {
                    Value::String(s) => s.trim().to_owned(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                (!text.is_empty()).then(|| (f.label.clone(), text))
            })
            .collect();
        if !fields.is_empty() {
            out.push(InfoField::CustomFields(fields));
        }
    }
    if let Some(id) = room.sla_id.clone().filter(|s| !s.is_empty()) {
        out.push(InfoField::Sla { id });
    }
    if let Some(id) = room.priority_id.clone().filter(|s| !s.is_empty()) {
        out.push(InfoField::Priority { id });
    }
    out
}

/// Current time as [`RcDate`] (for callers without one).
pub fn now() -> RcDate {
    RcDate::from(Utc::now())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{fixture, ts};
    use serde_json::json;

    fn served_room() -> Room {
        serde_json::from_value(fixture("omnichannel/rooms_info_served.json")["room"].clone())
            .unwrap()
    }

    fn closed_room() -> Room {
        serde_json::from_value(fixture("omnichannel/rooms_info_closed.json")["room"].clone())
            .unwrap()
    }

    fn sub_of(room: &Room) -> Subscription {
        Subscription {
            rid: room.id.clone(),
            t: RoomType::Livechat,
            open: true,
            ..Default::default()
        }
    }

    const AGENT: &str = "ufKsmnbWJrX2vfR3i";

    #[test]
    fn composer_states() {
        let r = served_room();
        let s = sub_of(&r);
        assert_eq!(
            composer_state(&r, Some(&s), AGENT, false),
            ComposerState::Normal
        );
        assert_eq!(
            composer_state(&r, None, AGENT, false),
            ComposerState::Normal,
            "serving agent"
        );
        assert_eq!(
            composer_state(&r, None, "manager", false),
            ComposerState::Join
        );
        assert_eq!(
            composer_state(&r, Some(&s), AGENT, true),
            ComposerState::OverMac
        );
        let mut hold = r.clone();
        hold.on_hold = Some(true);
        assert_eq!(
            composer_state(&hold, Some(&s), AGENT, true),
            ComposerState::OverMac,
            "MAC first"
        );
        assert_eq!(
            composer_state(&hold, Some(&s), AGENT, false),
            ComposerState::OnHold
        );
        let mut sub_hold = s.clone();
        sub_hold.on_hold = Some(true);
        assert_eq!(
            composer_state(&r, Some(&sub_hold), AGENT, false),
            ComposerState::OnHold
        );
        let queued: Room =
            serde_json::from_value(fixture("omnichannel/rooms_info_queued.json")["room"].clone())
                .unwrap();
        assert_eq!(
            composer_state(&queued, None, AGENT, false),
            ComposerState::Preview
        );
        let c = closed_room();
        assert_eq!(
            composer_state(&c, None, AGENT, false),
            ComposerState::Closed
        );
        assert_eq!(
            composer_state(&c, None, AGENT, true),
            ComposerState::Closed,
            "closed first"
        );
        let mut voip = r.clone();
        voip.t = RoomType::Voip;
        assert_eq!(
            composer_state(&voip, Some(&s), AGENT, false),
            ComposerState::Closed
        );
    }

    #[test]
    fn mac_and_take() {
        let r = served_room();
        let mut active = r.clone();
        active.v = Some(json!({"_id":"v","activity":["2026-11"]}));
        assert!(!room_over_mac(&active, true, "2026-11"));
        assert!(room_over_mac(&active, true, "2026-12"));
        assert!(!room_over_mac(&active, false, "2026-12"));
        assert!(
            !room_over_mac(&closed_room(), true, "2030-01"),
            "closed rooms are exempt"
        );
        assert_eq!(
            take_block(UserStatus::Offline, AgentStatus::Available),
            Some(TakeBlock::Offline)
        );
        assert_eq!(
            take_block(UserStatus::Busy, AgentStatus::NotAvailable),
            Some(TakeBlock::Unavailable)
        );
        assert_eq!(take_block(UserStatus::Away, AgentStatus::Available), None);
        assert_eq!(
            take_block(UserStatus::Unknown, AgentStatus::Available),
            None
        );
        let d = chrono::TimeZone::with_ymd_and_hms(&Local, 2026, 3, 5, 12, 0, 0).unwrap();
        assert_eq!(month_key(d), "2026-03");
    }

    fn base() -> QuickActionInput {
        QuickActionInput {
            room_open: true,
            subscribed: true,
            can_close: true,
            on_hold_engagement_only: true,
            ..Default::default()
        }
    }

    #[test]
    fn quick_action_rules() {
        // A plain agent on CE: only close.
        assert_eq!(quick_actions_for(&base()), [QuickAction::Close]);
        let all = QuickActionInput {
            return_queue: true,
            can_transfer: true,
            can_send_transcript: true,
            can_request_pdf: true,
            livechat_enterprise: true,
            allow_manual_on_hold: true,
            ..base()
        };
        assert_eq!(
            quick_actions_for(&all),
            [
                QuickAction::MoveToQueue,
                QuickAction::Forward,
                QuickAction::TranscriptEmail,
                QuickAction::TranscriptPdf { enabled: false },
                QuickAction::PlaceOnHold,
                QuickAction::Close
            ]
        );
        // Engagement-only: no on-hold right after a visitor message.
        let v = QuickActionInput {
            last_message_from_visitor: true,
            ..all
        };
        assert!(!quick_actions_for(&v).contains(&QuickAction::PlaceOnHold));
        // Over MAC: transcript/forward/queue gone, close stays.
        let mac = QuickActionInput {
            over_mac: true,
            ..all
        };
        assert_eq!(
            quick_actions_for(&mac),
            [QuickAction::PlaceOnHold, QuickAction::Close]
        );
        // Closed room: only transcripts (PDF enabled).
        let closed = QuickActionInput {
            room_open: false,
            ..all
        };
        assert_eq!(
            quick_actions_for(&closed),
            [
                QuickAction::TranscriptEmail,
                QuickAction::TranscriptPdf { enabled: true }
            ]
        );
        // Last message is the close message: treated as closed.
        let lm = QuickActionInput {
            last_message_is_close: true,
            ..all
        };
        assert!(!quick_actions_for(&lm).contains(&QuickAction::Close));
        // Not subscribed: a manager may still forward/close, not move to queue.
        let mgr = QuickActionInput {
            subscribed: false,
            manager_or_monitor: true,
            ..all
        };
        assert_eq!(
            quick_actions_for(&mgr),
            [
                QuickAction::Forward,
                QuickAction::TranscriptEmail,
                QuickAction::TranscriptPdf { enabled: false },
                QuickAction::Close
            ]
        );
        let nobody = QuickActionInput {
            subscribed: false,
            ..base()
        };
        assert!(quick_actions_for(&nobody).is_empty());
    }

    #[test]
    fn quick_actions_without_workspace_entry() {
        use crate::workspace::Change;
        use rocketchat::models::Permission;
        let mut ws = Workspace::new();
        let mut me = crate::test_util::me("mgr", "mgr");
        me.roles = Some(vec!["livechat-manager".into()]);
        ws.apply_me(me);
        for p in [
            perm::SEND_OMNICHANNEL_CHAT_TRANSCRIPT,
            perm::TRANSFER_LIVECHAT_GUEST,
            perm::CLOSE_OTHERS_LIVECHAT_ROOM,
        ] {
            let p: Permission =
                serde_json::from_value(json!({"_id": p, "roles": ["livechat-manager"]})).unwrap();
            ws.apply_permission(Change::Inserted, p);
        }
        let omni = OmnichannelState::new();
        // Closed room from `rooms.info`: never in the workspace.
        let closed = closed_room();
        assert!(ws.room(&closed.id).is_none());
        let a = quick_actions(&ws, &omni, &closed, None, "2026-11");
        assert!(a.contains(&QuickAction::TranscriptEmail), "{a:?}");
        assert!(!a.contains(&QuickAction::Close));
        // Open chat a manager views without a subscription.
        let open = served_room();
        let a = quick_actions(&ws, &omni, &open, None, "2026-11");
        assert!(a.contains(&QuickAction::Forward), "{a:?}");
        assert!(a.contains(&QuickAction::Close), "{a:?}");
    }

    #[test]
    fn channel_labels() {
        let s = |v: Value| -> OmnichannelSource { serde_json::from_value(v).unwrap() };
        assert_eq!(
            channel_label(&s(json!({"type":"widget"}))),
            ChannelLabel::Livechat
        );
        assert_eq!(
            channel_detail(&s(json!({"type":"widget"}))),
            ChannelDetail::None
        );
        assert_eq!(
            channel_label(&s(json!({"type":"api"}))),
            ChannelLabel::CustomApi
        );
        assert_eq!(
            channel_detail(&s(json!({"type":"api"}))),
            ChannelDetail::External
        );
        assert_eq!(
            channel_label(&s(json!({"type":"app","alias":"WhatsApp","label":"Shop"}))),
            ChannelLabel::Alias("WhatsApp".into())
        );
        assert_eq!(
            channel_detail(&s(json!({"type":"app","label":"Shop"}))),
            ChannelDetail::Text("Shop".into())
        );
        assert_eq!(
            channel_detail(&s(json!({"type":"email","id":"inbox@x"}))),
            ChannelDetail::Text("inbox@x".into())
        );
        assert_eq!(
            channel_detail(&s(json!({"type":"sms"}))),
            ChannelDetail::External
        );
        assert_eq!(
            channel_label(&s(json!({"type":"other"}))),
            ChannelLabel::CustomIntegration
        );
        assert_eq!(
            channel_label(&s(json!({"type":"telegram","id":"tg"}))),
            ChannelLabel::Raw("tg".into())
        );
    }

    #[test]
    fn info_panel() {
        let c = closed_room();
        let now = ts(1_794_094_800_000);
        let defs: Vec<CustomFieldDef> = vec![
            serde_json::from_value(
                json!({"_id":"plan","label":"Plan","scope":"room","visibility":"visible"}),
            )
            .unwrap(),
            serde_json::from_value(
                json!({"_id":"secret","label":"S","scope":"room","visibility":"hidden"}),
            )
            .unwrap(),
            serde_json::from_value(
                json!({"_id":"empty","label":"E","scope":"room","visibility":"visible"}),
            )
            .unwrap(),
        ];
        let mut room = c.clone();
        room.livechat_data = Some(json!({"plan":" gold ","secret":"x","empty":""}));
        let f = info_fields(&room, now, &defs, true);
        let kinds: Vec<&str> = f
            .iter()
            .map(|f| match f {
                InfoField::Channel { .. } => "channel",
                InfoField::Contact { .. } => "contact",
                InfoField::Agent { .. } => "agent",
                InfoField::Department { .. } => "department",
                InfoField::Tags(_) => "tags",
                InfoField::Topic(_) => "topic",
                InfoField::QueueTime { .. } => "queue",
                InfoField::ChatDuration { .. } => "duration",
                InfoField::CreatedAt(_) => "created",
                InfoField::ClosedAt(_) => "closed",
                InfoField::TakenAt(_) => "taken",
                InfoField::AvgResponseTime { .. } => "avg",
                InfoField::InactivityTime { .. } => "inactivity",
                InfoField::CustomFields(_) => "custom",
                InfoField::Sla { .. } => "sla",
                InfoField::Priority { .. } => "priority",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "channel",
                "contact",
                "agent",
                "department",
                "tags",
                "topic",
                "queue",
                "duration",
                "created",
                "closed",
                "taken",
                "avg",
                "inactivity",
                "custom"
            ]
        );
        assert!(f.contains(&InfoField::CustomFields(vec![(
            "Plan".into(),
            "gold".into()
        )])));
        // queuedAt 23:38:48.133 -> servedBy.ts 23:38:49.951
        assert!(f.contains(&InfoField::QueueTime { secs: 1 }));
        assert!(f.contains(&InfoField::AvgResponseTime { secs: 8.47 }));
        // No custom fields without the permission.
        assert!(
            !info_fields(&room, now, &defs, false)
                .iter()
                .any(|f| matches!(f, InfoField::CustomFields(_)))
        );
    }

    #[test]
    fn queue_time_cases() {
        let mut r = Room {
            t: RoomType::Livechat,
            ts: Some(ts(0)),
            open: Some(true),
            ..Default::default()
        };
        assert_eq!(
            queue_time_secs(&r, ts(10_000)),
            Some(10),
            "open, unserved: until now"
        );
        r.queued_at = Some(ts(4_000));
        r.served_by = Some(json!({"_id":"a","ts":{"$date":7000}}));
        assert_eq!(queue_time_secs(&r, ts(99_000)), Some(3));
        r.served_by = None;
        r.open = None;
        r.closed_at = Some(ts(9_000));
        assert_eq!(queue_time_secs(&r, ts(99_000)), Some(5));
        r.closed_at = None;
        assert_eq!(queue_time_secs(&r, ts(99_000)), None);
    }

    #[test]
    fn leave_rule() {
        assert!(!can_leave(RoomType::Livechat));
        assert!(can_leave(RoomType::Channel));
    }
}
