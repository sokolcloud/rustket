//! Omnichannel models and stream decoding. Fixtures in `fixtures/omnichannel/`
//! were captured from the 8.9 dev server (throwaway agent, department, visitor and
//! chat); `ddp/synthetic_*` follow the sources for paths that need Enterprise or
//! `Manual_Selection` routing.
#![allow(clippy::unwrap_used)]

use rocketchat::models::{
    AgentDepartment, AgentStatus, CannedScope, Contact, ContactChannel, CustomFieldDef, Department,
    DepartmentAgentAction, DepartmentRef, Inquiry, InquiryEventKind, InquiryStatus, LicenseInfo,
    LivechatAgent, Message, Priority, Room, RoomType, RoutingConfig, Sla, SourceType, Subscription,
    Tag, TransferData, TransferScope, User, UserStatus, Visitor, lenient,
};
use rocketchat::session::{
    DataKind, DataMessage, OmnichannelEvent, SessionEvent, decode_event, decode_omnichannel,
    streams,
};
use rocketchat::{Error, RcDate};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/omnichannel/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn take<T: serde::de::DeserializeOwned>(name: &str, key: &str) -> T {
    let mut v = fixture(name);
    serde_json::from_value(v[key].take()).unwrap()
}

fn list<T: serde::de::DeserializeOwned>(name: &str, key: &str) -> Vec<T> {
    let mut v = fixture(name);
    let c = lenient::counted::<T>(v[key].take());
    assert_eq!(c.dropped, 0, "{name}: dropped elements");
    c.items
}

fn decode(name: &str) -> SessionEvent {
    let v = fixture(&format!("ddp/{name}"));
    decode_event(&DataMessage {
        kind: DataKind::Changed,
        collection: v["collection"].as_str().unwrap().to_owned(),
        id: "id".into(),
        fields: v["fields"].as_object().cloned(),
        cleared: Vec::new(),
        before: None,
    })
}

fn omni(name: &str) -> OmnichannelEvent {
    decode(name)
        .omnichannel()
        .unwrap_or_else(|| panic!("{name}: not an omnichannel event"))
}

const AGENT: &str = "ufKsmnbWJrX2vfR3i";
const RID: &str = "CS2PFviwDBkDoQDWP";
const DEPT: &str = "6aefb685ff2338a3f2a2f259";

#[test]
fn routing_and_status() {
    let c: RoutingConfig = take("config_routing", "config");
    assert!(c.auto_assign_agent && c.show_queue_link && c.enable_trigger_action);
    assert!(!c.show_queue && !c.return_queue && !c.preview_room);
    let s: AgentStatus = take("agent_status_response", "status");
    assert_eq!(s, AgentStatus::NotAvailable);
    let me: User = serde_json::from_value(fixture("me_agent")).unwrap();
    assert!(me.is_livechat_agent());
    assert_eq!(me.agent_status(), AgentStatus::Available);
    // `livechatStatusSystemModified: null` and `livechat: null` are tolerated.
    assert_eq!(me.livechat_status_system_modified, None);
}

#[test]
fn inquiries() {
    let v = fixture("inquiries_queued_for_user");
    assert_eq!(v["total"], 1);
    let q: Vec<Inquiry> = list("inquiries_queued_for_user", "inquiries");
    let i = &q[0];
    assert_eq!(i.rid, RID);
    assert_eq!(i.status, InquiryStatus::Queued);
    assert_eq!(i.department.as_deref(), Some(DEPT));
    assert_eq!(i.t, RoomType::Livechat);
    assert_eq!(i.priority_weight, Some(99));
    assert_eq!(i.estimated_waiting_time_queue, Some(9_999_999));
    assert!(i.queued_at.is_some() && i.updated_at.is_some());
    assert_eq!(i.v.username.as_deref(), Some("guest-8"));
    assert_eq!(i.v.status, Some(UserStatus::Online));
    assert_eq!(i.source.as_ref().unwrap().kind, SourceType::Api);
    let lm = i.last_message.as_deref().unwrap();
    assert_eq!(lm.t.as_deref(), Some("livechat_transfer_history"));
    let td = lm.transfer_data.as_ref().unwrap();
    assert_eq!(td.scope, TransferScope::Department);
    assert_eq!(td.next_department.as_ref().unwrap().id, DEPT);
    assert_eq!(td.transferred_by.as_ref().unwrap().display_name(), "Admin");

    let taken: Option<Inquiry> = take("inquiries_get_one_taken", "inquiry");
    let taken = taken.unwrap();
    assert_eq!(taken.status, InquiryStatus::Taken);
    assert!(taken.taken_at.is_some() && taken.queued_at.is_none());
}

#[test]
fn rooms() {
    let r: Room = take("rooms_info_served", "room");
    assert_eq!(r.t, RoomType::Livechat);
    assert!(r.is_omnichannel_open());
    assert!(r.u.is_none());
    let v = r.visitor().unwrap();
    assert_eq!(v.username.as_deref(), Some("guest-8"));
    assert_eq!(v.token.as_deref(), Some("omnicap-vt-1794094719964"));
    assert_eq!(r.served_by_agent().unwrap().id, AGENT);
    assert_eq!(r.omnichannel_source().unwrap().kind, SourceType::Api);
    assert_eq!(r.department_id.as_deref(), Some(DEPT));
    assert!(r.contact_id.is_some());
    assert_eq!(r.priority_weight, Some(99));

    let q: Room = take("rooms_info_queued", "room");
    assert!(q.served_by_agent().is_none());
    assert!(q.queued_at.is_some());

    let c: Room = take("rooms_info_closed", "room");
    assert!(!c.is_omnichannel_open(), "closed: `open` unset");
    assert_eq!(c.open, None);
    assert!(c.closed_at.is_some());
    assert_eq!(c.closer.as_deref(), Some("user"));
    assert_eq!(c.closed_by.as_ref().unwrap().id, AGENT);
    assert_eq!(
        c.tags.as_deref().unwrap(),
        ["closing", "alpha", "beta", "dept-tag"]
    );
    assert_eq!(c.topic.as_deref(), Some("Omnicap topic"));
    let rb = c.response_by.as_ref().unwrap();
    assert!(rb.first_response_ts.is_some());
    assert_eq!(
        c.metrics.as_ref().unwrap()["response"]["avg"].as_f64(),
        Some(8.47)
    );
    assert_eq!(c.visitor().unwrap().activity, ["2026-11"]);
    assert_eq!(
        c.last_message.as_ref().unwrap().transcript_requested,
        Some(false)
    );

    let hist: Vec<Room> = list("contacts_history", "history");
    assert_eq!(hist.len(), 1);
    assert!(hist[0].closed_at.is_some());
    let rooms: Vec<Room> = list("livechat_rooms", "rooms");
    assert_eq!(rooms[0].id, RID);
    let ch: Vec<Room> = list("visitors_chat_history", "history");
    assert_eq!(ch[0].id, RID);
    let sc: Vec<Room> = list("visitors_search_chats", "history");
    assert_eq!(sc[0].id, RID);
}

#[test]
fn subscription() {
    let s: Subscription = take("subscriptions_get_one_livechat", "subscription");
    assert_eq!(s.t, RoomType::Livechat);
    assert_eq!(s.department.as_deref(), Some(DEPT));
    assert_eq!(s.v.as_ref().unwrap().username.as_deref(), Some("guest-8"));
    assert!(s.alert && s.unread == 1 && s.user_mentions == 1);
    assert!(!s.is_on_hold());
}

#[test]
fn visitor_and_contact() {
    let v: Visitor = take("visitors_info", "visitor");
    assert_eq!(v.username.as_deref(), Some("guest-8"));
    assert_eq!(v.email(), Some("omnicap-v-1794094719964@example.com"));
    assert_eq!(v.phone_number(), Some("+15554719964"));
    assert_eq!(v.ip, None, "`ip: null`");
    assert_eq!(v.last_chat.as_ref().unwrap().id, RID);
    assert_eq!(v.display_name(), "Omnicap Visitor 1794094719964");

    let c: Contact = take("contacts_get", "contact");
    assert!(c.unknown);
    assert_eq!(c.channels.len(), 1);
    assert_eq!(c.channels[0].visitor.source.kind, SourceType::Api);
    assert_eq!(c.contact_manager_id(), None);
    let chans: Vec<ContactChannel> = list("contacts_channels", "channels");
    assert_eq!(chans[0].visitor.visitor_id, v.id);
    let search = fixture("contacts_search");
    assert_eq!(search["count"], 10, "count echoes the request");
    let m: Contact = serde_json::from_value(
        json!({"_id":"c","contactManager":{"_id":"u1","name":"M","username":"m"}}),
    )
    .unwrap();
    assert_eq!(m.contact_manager_id(), Some("u1"));
}

#[test]
fn departments_agents_fields() {
    let d: Option<Department> = take("department_get", "department");
    let d = d.unwrap();
    assert_eq!(d.id, DEPT);
    assert!(d.enabled && d.allow_receive_forward_offline);
    assert_eq!(d.chat_closing_tags, ["dept-tag"]);
    assert_eq!(d.num_agents, Some(1));
    assert!(d.allows_forward_to("any"));
    let missing: Option<Department> = take("department_get_missing", "department");
    assert!(missing.is_none());
    let all: Vec<Department> = list("department_list", "departments");
    assert_eq!(all.len(), 1);
    let ac: Vec<DepartmentRef> = list("department_autocomplete", "items");
    assert_eq!(ac[0].id, DEPT);
    let mine: Vec<AgentDepartment> = list("agent_departments", "departments");
    assert_eq!(mine[0].department_id, DEPT);
    assert!(mine[0].department_enabled);
    let agents: Vec<LivechatAgent> = list("users_agent_list", "users");
    assert_eq!(agents[0].id, AGENT);
    assert_eq!(agents[0].status_livechat, Some(AgentStatus::Available));
    assert_eq!(agents[0].departments, [DEPT]);
    let fields: Vec<CustomFieldDef> = list("custom_fields", "customFields");
    assert!(fields.is_empty());
    let f: CustomFieldDef =
        serde_json::from_value(json!({"_id":"cf","label":"Plan","scope":"room",
        "visibility":"visible","type":"select","options":"a, b,,c","required":false,"regexp":""}))
        .unwrap();
    assert!(f.is_visible_room_field());
    assert_eq!(f.option_list(), ["a", "b", "c"]);
    let hist: Vec<TransferData> = list("transfer_history", "history");
    assert_eq!(hist[0].comment.as_deref(), Some("to queue"));
}

#[test]
fn messages() {
    let msgs: Vec<Message> = list("livechat_room_messages", "messages");
    let visitor = msgs.iter().find(|m| m.is_from_visitor()).unwrap();
    assert!(visitor.alias.is_some());
    assert!(msgs.iter().all(|m| m.rid == RID));
}

#[test]
fn errors_and_license() {
    let err = |name: &str, status: u16| {
        Error::from_response(status, None, fixture(name).to_string().as_bytes())
    };
    assert!(err("error_enterprise_tags", 400).is_enterprise_only());
    assert_eq!(
        err("error_close_comment_required", 400).livechat_error_code(),
        Some("error-comment-is-required")
    );
    assert_eq!(
        err("error_inquiry_taken", 400).livechat_error_code(),
        Some("error-inquiry-taken")
    );
    assert_eq!(
        err("error_inquiry_take_offline", 400).livechat_error_code(),
        Some("error-agent-status-service-offline")
    );
    assert_eq!(
        err("error_room_already_closed", 400).livechat_error_code(),
        Some("error-room-already-closed")
    );
    assert_eq!(
        err("error_agent_not_found", 404).livechat_error_code(),
        Some("Agent not found")
    );
    assert_eq!(
        err("error_forward_unauthorized", 403).livechat_error_code(),
        Some("error-unauthorized")
    );
    assert!(!err("error_canned_responses_unauthorized", 403).is_enterprise_only());

    let l: LicenseInfo = serde_json::from_value(json!({"activeModules":[],"externalModules":[],
        "preventedActions":{"privateApps":true,"monthlyActiveContacts":false},"hasValidLicense":false}))
    .unwrap();
    assert!(!l.has_module(LicenseInfo::LIVECHAT_ENTERPRISE));
    assert!(!l.over_mac_limit());
}

#[test]
fn ee_tags_priorities_sla() {
    // Synthetic, from core-typings (the dev server is CE: these endpoints answer
    // `error-action-not-allowed`).
    let tags: Vec<Tag> = list("synthetic_ee_tags", "tags");
    assert_eq!(tags.len(), 3);
    assert_eq!(tags[0].departments, ["dep1"]);
    assert_eq!(tags[0].num_departments, Some(1));
    assert_eq!(tags[0].description.as_deref(), Some("Billing questions"));
    assert!(tags[2].name.is_empty() && tags[2].departments.is_empty());

    let prios: Vec<Priority> = list("synthetic_ee_priorities", "priorities");
    assert_eq!(prios.len(), 4);
    assert_eq!(prios[0].custom_name(), None);
    assert_eq!(prios[0].i18n.as_deref(), Some("Highest"));
    assert_eq!(
        prios[1].custom_name(),
        Some("Urgent!"),
        "dirty: show the name"
    );
    assert_eq!(prios[2].custom_name(), None, "not dirty: translate i18n");
    assert_eq!(prios[1].sort_item, 2);

    let slas: Vec<Sla> = list("synthetic_ee_sla", "sla");
    assert_eq!(slas[0].name, "Gold");
    assert_eq!(slas[0].due_time_in_minutes, 15);
    assert_eq!(slas[1].description, None);
}

#[tokio::test]
async fn agent_status_unknown_is_rejected_locally() {
    // Sending `{}` would toggle the availability; nothing must reach the network.
    let tls = rocketchat::TlsConfig::new().build().unwrap();
    let server = rocketchat::ServerUrl::parse("http://127.0.0.1:9").unwrap();
    let rest = rocketchat::RestClient::new(server, &tls).unwrap();
    let e = rest
        .livechat_agent_status(Some(AgentStatus::Unknown))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Protocol(_)), "{e:?}");
}

#[test]
fn ddp_inquiry_events() {
    let OmnichannelEvent::InquiryQueue { key, kind, inquiry } = omni("inquiry_changed_queued")
    else {
        panic!()
    };
    assert_eq!(key, format!("department/{DEPT}"));
    assert_eq!(kind, InquiryEventKind::Changed);
    assert_eq!(inquiry.status, InquiryStatus::Queued);
    assert_eq!(inquiry.rid, RID);
    assert!(inquiry.updated_at.is_some());
    let OmnichannelEvent::InquiryQueue { kind, inquiry, .. } = omni("inquiry_changed_taken") else {
        panic!()
    };
    assert_eq!(
        (kind, inquiry.status),
        (InquiryEventKind::Changed, InquiryStatus::Taken)
    );
    let OmnichannelEvent::InquiryQueue { kind, .. } = omni("inquiry_removed") else {
        panic!()
    };
    assert_eq!(kind, InquiryEventKind::Removed);
    let OmnichannelEvent::InquiryQueue { key, kind, inquiry } =
        omni("synthetic_inquiry_added_no_department")
    else {
        panic!()
    };
    assert_eq!((key.as_str(), kind), ("public", InquiryEventKind::Added));
    assert!(inquiry.department.is_none());
    assert_eq!(inquiry.source.unwrap().kind, SourceType::Widget);
    // Garbage on the queue stream stays undecoded.
    assert!(
        decode_omnichannel(
            streams::INQUIRY_QUEUE,
            "public",
            &[json!({"type":"changed"})]
        )
        .is_none()
    );
    assert!(
        decode_omnichannel(
            streams::INQUIRY_QUEUE,
            "public",
            &[json!({"type":"x","_id":"i"})]
        )
        .is_none()
    );
}

#[test]
fn ddp_department_agent_data() {
    let OmnichannelEvent::DepartmentAgentChanged(c) = omni("department_agent_data_inserted") else {
        panic!()
    };
    assert_eq!(c.action, DepartmentAgentAction::Inserted);
    assert_eq!(
        (c.agent_id.as_str(), c.department_id.as_str()),
        (AGENT, DEPT)
    );
    let OmnichannelEvent::DepartmentAgentChanged(c) = omni("department_agent_data_removed") else {
        panic!()
    };
    assert_eq!(c.action, DepartmentAgentAction::Removed);
}

#[test]
fn ddp_agent_status() {
    assert_eq!(
        decode("user_data_status_livechat").agent_status(),
        Some(AgentStatus::Available)
    );
    assert_eq!(
        decode("synthetic_user_data_status_livechat_space").agent_status(),
        Some(AgentStatus::Available)
    );
    assert_eq!(
        decode("synthetic_user_data_status_livechat_system").agent_status(),
        Some(AgentStatus::NotAvailable)
    );
    assert!(decode("user_data_status_livechat").omnichannel().is_none());
}

#[test]
fn ddp_livechat_session_events() {
    let SessionEvent::SubscriptionChanged { sub, .. } =
        decode("subscriptions_changed_inserted_livechat")
    else {
        panic!()
    };
    assert_eq!(sub.t, RoomType::Livechat);
    assert!(sub.v.is_some());
    let SessionEvent::RoomChanged { room, .. } = decode("rooms_changed_updated_livechat") else {
        panic!()
    };
    assert!(room.visitor().is_some());
    let SessionEvent::Notification(n) = decode("notification_livechat") else {
        panic!()
    };
    assert_eq!(n.payload.room_type, Some(RoomType::Livechat));
    assert_eq!(n.payload.room_id(), Some(RID));
    assert!(!n.payload.is_queued_inquiry());
    let SessionEvent::Notification(n) = decode("synthetic_notification_queued_inquiry") else {
        panic!()
    };
    assert_eq!(n.payload.room_id(), None, "never open room \"\"");
    assert!(n.payload.is_queued_inquiry());

    let SessionEvent::Message { message, .. } = decode("my_messages_visitor") else {
        panic!()
    };
    assert!(message.is_from_visitor());
    let SessionEvent::Message { message, .. } = decode("my_messages_livechat_transfer_history")
    else {
        panic!()
    };
    assert_eq!(
        message.transfer_data.as_ref().unwrap().scope,
        TransferScope::Department
    );
    let SessionEvent::Message {
        message,
        room_participant,
    } = decode("my_messages_livechat_close")
    else {
        panic!()
    };
    assert_eq!(message.t.as_deref(), Some("livechat-close"));
    assert!(!room_participant, "the subscription is already gone");
    let SessionEvent::Message { message, .. } = decode("my_messages_command_connected") else {
        panic!()
    };
    assert_eq!(message.t.as_deref(), Some("command"));
}

#[test]
fn ddp_canned_priority_room_data() {
    let OmnichannelEvent::CannedResponseChanged { response, agents } =
        omni("synthetic_canned_response_changed")
    else {
        panic!()
    };
    assert_eq!(response.scope, CannedScope::Department);
    assert_eq!(response.department_id.as_deref(), Some("d1"));
    assert_eq!(agents.unwrap(), [AGENT]);
    assert!(response.created_at.is_some());
    assert!(matches!(
        omni("synthetic_canned_response_removed"),
        OmnichannelEvent::CannedResponseRemoved { .. }
    ));
    assert!(matches!(
        omni("synthetic_priority_changed"),
        OmnichannelEvent::PriorityChanged {
            client_action: true,
            ..
        }
    ));
    let mut room = fixture("rooms_info_served")["room"].take();
    room["ts"] = json!({"$date": 1794094724328i64});
    let ev = decode_omnichannel(streams::ROOM_DATA, RID, &[room]).unwrap();
    let OmnichannelEvent::RoomData(r) = ev else {
        panic!()
    };
    assert_eq!(r.id, RID);
    assert_eq!(r.ts, RcDate::from_millis(1794094724328));
}
