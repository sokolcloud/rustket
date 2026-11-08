//! Omnichannel (livechat) models for the agent side (`docs/omnichannel.md`).
//!
//! Everything is tolerant: unknown fields are ignored, nested values that fail to
//! parse are dropped instead of rejecting the document.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use super::{Message, Room, RoomType, Subscription, User, UserRef, UserStatus, lenient};
use crate::RcDate;

/// Agent availability (`statusLivechat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AgentStatus {
    Available,
    #[default]
    NotAvailable,
    /// Anything else: re-read `me`.
    Unknown,
}

impl AgentStatus {
    /// Lenient parse: the server sometimes sends `"available "` (trailing space,
    /// business-hours login path).
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "available" => Self::Available,
            "not-available" => Self::NotAvailable,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotAvailable => "not-available",
            Self::Unknown => "",
        }
    }

    pub fn is_available(&self) -> bool {
        *self == Self::Available
    }

    /// `statusLivechat` from a `{uid}/userData` payload, if it changed.
    /// `unset: {statusLivechat}` counts as not available.
    pub fn from_user_data(v: &Value) -> Option<Self> {
        for key in ["diff", "data"] {
            if let Some(s) = v.get(key).and_then(|d| d.get("statusLivechat")) {
                return Some(s.as_str().map_or(Self::Unknown, Self::parse));
            }
        }
        v.get("unset")
            .and_then(|u| u.get("statusLivechat"))
            .map(|_| Self::NotAvailable)
    }
}

impl Serialize for AgentStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::String(s) => Self::parse(&s),
            _ => Self::Unknown,
        })
    }
}

/// `GET livechat/config/routing` → `config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoutingConfig {
    pub preview_room: bool,
    pub show_connecting: bool,
    pub show_queue: bool,
    pub show_queue_link: bool,
    pub return_queue: bool,
    pub enable_trigger_action: bool,
    pub auto_assign_agent: bool,
}

/// Inquiry `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InquiryStatus {
    Verifying,
    Ready,
    Queued,
    Taken,
    /// Legacy.
    Open,
    #[default]
    #[serde(other)]
    Unknown,
}

/// `{agentId, username}` (an inquiry's `defaultAgent`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SelectedAgent {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// The visitor as embedded in rooms, subscriptions and inquiries (`v`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VisitorRef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Visitor token (present for agents in `rooms.info`; stripped by some endpoints).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub status: Option<UserStatus>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub phone: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_message_ts: Option<RcDate>,
    /// Months (`YYYY-MM`) the contact was active in (MAC accounting).
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub activity: Vec<String>,
}

impl VisitorRef {
    /// `name`, else `username`.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.username.as_deref())
            .unwrap_or_default()
    }
}

/// `servedBy` of an omnichannel room.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServedBy {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
}

/// `responseBy` of an omnichannel room (removed while `waitingResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResponseBy {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub first_response_ts: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_message_ts: Option<RcDate>,
}

/// `source.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Widget,
    Email,
    Sms,
    App,
    Api,
    Other,
    #[default]
    #[serde(other)]
    Unknown,
}

/// `IOmnichannelSource`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OmnichannelSource {
    #[serde(rename = "type", deserialize_with = "lenient::or_default")]
    pub kind: SourceType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

/// `ILivechatInquiryRecord`: the routing/queue object of an omnichannel room.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Inquiry {
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    #[serde(deserialize_with = "super::null_default")]
    pub name: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub status: InquiryStatus,
    #[serde(deserialize_with = "lenient::or_default")]
    pub t: RoomType,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub queued_at: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub taken_at: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub message: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub v: VisitorRef,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub department: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub default_agent: Option<SelectedAgent>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub source: Option<OmnichannelSource>,
    /// 1..5, 99 = not specified.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub priority_weight: Option<i64>,
    /// Minutes; 9999999 = no SLA.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub estimated_waiting_time_queue: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_message: Option<Box<Message>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub locked: Option<bool>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

/// `type` of an inquiry queue stream event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InquiryEventKind {
    Added,
    Changed,
    Removed,
}

impl InquiryEventKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "added" | "inserted" => Some(Self::Added),
            "changed" | "updated" => Some(Self::Changed),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VisitorEmail {
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VisitorPhone {
    pub phone_number: String,
}

/// `{_id, ts}` (`lastChat`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LastChat {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
}

/// `ILivechatVisitor` (`livechat/visitors.info`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Visitor {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub visitor_emails: Vec<VisitorEmail>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub phone: Vec<VisitorPhone>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub status: Option<UserStatus>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub user_agent: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ip: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub host: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_chat: Option<LastChat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_agent: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livechat_data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_manager: Option<Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub department: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub activity: Vec<String>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

impl Visitor {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.username.as_deref())
            .unwrap_or(&self.id)
    }

    pub fn email(&self) -> Option<&str> {
        self.visitor_emails
            .first()
            .map(|e| e.address.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn phone_number(&self) -> Option<&str> {
        self.phone
            .first()
            .map(|p| p.phone_number.as_str())
            .filter(|s| !s.is_empty())
    }
}

/// `{visitorId, source}` of a contact channel.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContactVisitor {
    pub visitor_id: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub source: OmnichannelSource,
}

/// `ILivechatContactChannel`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContactChannel {
    pub name: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub visitor: ContactVisitor,
    #[serde(deserialize_with = "lenient::or_default")]
    pub blocked: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub verified: bool,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub details: Option<OmnichannelSource>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_chat: Option<LastChat>,
}

/// `ILivechatContact` (`omnichannel/contacts.get`, `contacts.search`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Contact {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub emails: Vec<VisitorEmail>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub phones: Vec<VisitorPhone>,
    /// A user id in `contacts.get`, `{_id, name, username}` in `contacts.search`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_manager: Option<Value>,
    /// Auto-created, not curated.
    #[serde(deserialize_with = "lenient::or_default")]
    pub unknown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub channels: Vec<ContactChannel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicting_fields: Option<Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_chat: Option<LastChat>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub activity: Vec<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub pre_registration: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub created_at: Option<RcDate>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

impl Contact {
    /// The contact manager's user id, whatever the shape.
    pub fn contact_manager_id(&self) -> Option<&str> {
        match self.contact_manager.as_ref()? {
            Value::String(s) => Some(s.as_str()),
            v => v.get("_id").and_then(Value::as_str),
        }
        .filter(|s| !s.is_empty())
    }
}

/// `ILivechatDepartment`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Department {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub name: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub enabled: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub show_on_registration: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub show_on_offline_form: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub request_tag_before_closing_chat: bool,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub chat_closing_tags: Vec<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub allow_receive_forward_offline: bool,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub fallback_forward_department: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub departments_allowed_to_forward: Vec<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub num_agents: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub parent_id: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub ancestors: Vec<String>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

impl Department {
    /// Whether forwarding a chat from this department to `target` is allowed
    /// (EE rule; an empty list allows everything).
    pub fn allows_forward_to(&self, target: &str) -> bool {
        self.departments_allowed_to_forward.is_empty()
            || self
                .departments_allowed_to_forward
                .iter()
                .any(|d| d == target)
    }
}

/// `{_id, name}` (`department.autocomplete`, transfer data).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DepartmentRef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub name: String,
}

/// `ILivechatDepartmentAgents` as returned by `livechat/agents/{uid}/departments`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentDepartment {
    #[serde(rename = "_id")]
    pub id: String,
    pub agent_id: String,
    pub department_id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub department_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub count: i64,
    #[serde(deserialize_with = "lenient::or_default")]
    pub order: i64,
    #[serde(deserialize_with = "lenient::or_default")]
    pub department_enabled: bool,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

/// An agent in `livechat/users/agent` (forward picker).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LivechatAgent {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub status: Option<UserStatus>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub status_livechat: Option<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emails: Option<Vec<super::Email>>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub departments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livechat: Option<Value>,
}

impl LivechatAgent {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.username.as_deref())
            .unwrap_or(&self.id)
    }
}

/// `ILivechatCustomField` (`livechat/custom-fields`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomFieldDef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub label: String,
    /// `room` or `visitor`.
    #[serde(deserialize_with = "super::null_default")]
    pub scope: String,
    /// `visible` or `hidden`.
    #[serde(deserialize_with = "super::null_default")]
    pub visibility: String,
    /// `input` or `select`.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub regexp: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub required: bool,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub default_value: Option<String>,
    /// Comma-separated options of a `select` field.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub options: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub public: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub searchable: bool,
}

impl CustomFieldDef {
    /// A room field shown in the room info panel.
    pub fn is_visible_room_field(&self) -> bool {
        self.scope == "room" && self.visibility == "visible"
    }

    /// Options of a `select` field.
    pub fn option_list(&self) -> Vec<&str> {
        self.options
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Canned response scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CannedScope {
    Global,
    Department,
    User,
    #[default]
    #[serde(other)]
    Unknown,
}

impl CannedScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Department => "department",
            Self::User => "user",
            Self::Unknown => "",
        }
    }
}

/// `IOmnichannelCannedResponse` (EE).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CannedResponse {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub shortcut: String,
    #[serde(deserialize_with = "super::null_default")]
    pub text: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub scope: CannedScope,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub tags: Vec<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub user_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub department_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub created_by: Option<UserRef>,
    #[serde(
        rename = "_createdAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub created_at: Option<RcDate>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

/// Body of `POST canned-responses` (create when `id` is `None`).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCannedResponse {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub shortcut: String,
    pub text: String,
    pub scope: CannedScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// `ILivechatTag` (EE).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tag {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient::vec"
    )]
    pub departments: Vec<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub num_departments: Option<u64>,
}

/// `ILivechatPriority` (EE). Display `name` when `dirty`, else translate `i18n`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Priority {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i18n: Option<String>,
    /// 1..5.
    #[serde(deserialize_with = "lenient::or_default")]
    pub sort_item: i64,
    #[serde(deserialize_with = "lenient::or_default")]
    pub dirty: bool,
}

impl Priority {
    /// The admin-edited name to show verbatim (`dirty`); `None` means translate
    /// [`Self::i18n`].
    pub fn custom_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .filter(|n| self.dirty && !n.trim().is_empty())
    }
}

/// `IOmnichannelServiceLevelAgreements` (EE).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Sla {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "super::null_default")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub due_time_in_minutes: i64,
}

/// `transferData.scope`. Unknown scopes must be rendered generically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferScope {
    Agent,
    Department,
    Queue,
    AutoTransferUnansweredChatsToAgent,
    AutoTransferUnansweredChatsToQueue,
    #[default]
    #[serde(other)]
    Unknown,
}

/// `transferredBy` / `transferredTo`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TransferActor {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `agent`, `user` or `visitor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,
}

impl TransferActor {
    /// `name || username` (the web renderer's `from`).
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.username.as_deref())
            .unwrap_or_default()
    }
}

/// `transferData` of `livechat_transfer_history` messages and `transfer.history` entries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TransferData {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub transferred_by: Option<TransferActor>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub transferred_to: Option<TransferActor>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub scope: TransferScope,
    /// Free text; the duration in seconds for the auto-transfer scopes.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub comment: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub next_department: Option<DepartmentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_department: Option<Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
}

/// `action` of a `{uid}/departmentAgentData` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepartmentAgentAction {
    Inserted,
    Updated,
    Removed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// `{uid}/departmentAgentData`: the user's department membership changed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DepartmentAgentChange {
    #[serde(deserialize_with = "lenient::or_default")]
    pub action: DepartmentAgentAction,
    #[serde(rename = "_id")]
    pub id: String,
    pub agent_id: String,
    pub department_id: String,
}

/// `transcriptEmail` of `room.closeByUser`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEmail {
    pub send_to_visitor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_data: Option<TranscriptRequestData>,
}

impl TranscriptEmail {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn to_visitor(email: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            send_to_visitor: true,
            request_data: Some(TranscriptRequestData {
                email: email.into(),
                subject: subject.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct TranscriptRequestData {
    pub email: String,
    pub subject: String,
}

/// Body of `POST livechat/room.closeByUser`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseChat {
    pub rid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// EE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate_transcript_pdf: Option<bool>,
    /// Defaults to `{sendToVisitor: false}` like the web client.
    pub transcript_email: TranscriptEmail,
    /// Only honoured with `Omnichannel_allow_force_close_conversations`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_close: Option<bool>,
}

impl CloseChat {
    pub fn new(rid: impl Into<String>, comment: Option<String>) -> Self {
        Self {
            rid: rid.into(),
            comment: comment.filter(|c| !c.trim().is_empty()),
            ..Default::default()
        }
    }
}

/// Target of `room.forward`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardTarget {
    Agent(String),
    Department(String),
    /// A department with a preferred (online, member) agent.
    DepartmentAgent {
        department_id: String,
        agent_id: String,
    },
}

/// Body of `POST livechat/room.forward`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardChat {
    pub room_id: String,
    pub target: ForwardTarget,
    pub comment: Option<String>,
}

impl ForwardChat {
    pub fn to_body(&self) -> Value {
        let mut body = json!({ "roomId": self.room_id, "clientAction": true });
        match &self.target {
            ForwardTarget::Agent(uid) => body["userId"] = json!(uid),
            ForwardTarget::Department(d) => body["departmentId"] = json!(d),
            ForwardTarget::DepartmentAgent {
                department_id,
                agent_id,
            } => {
                body["departmentId"] = json!(department_id);
                body["userId"] = json!(agent_id);
            }
        }
        if let Some(c) = self.comment.as_deref().filter(|c| !c.trim().is_empty()) {
            body["comment"] = json!(c);
        }
        body
    }
}

/// `guestData` of `room.saveInfo`. Blank strings are ignored by the server.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestData {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Only saved with `edit-livechat-room-customfields`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livechat_data: Option<BTreeMap<String, String>>,
}

/// `roomData` of `room.saveInfo`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomData {
    #[serde(rename = "_id")]
    pub id: String,
    /// `None` keeps the topic, `Some("")` removes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Always sent: the server clears the tags when they are omitted.
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livechat_data: Option<BTreeMap<String, String>>,
    /// EE; `""` removes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_id: Option<String>,
    /// EE; `""` removes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sla_id: Option<String>,
}

/// Body of `POST livechat/room.saveInfo`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct SaveRoomInfo {
    #[serde(rename = "guestData")]
    pub guest: GuestData,
    #[serde(rename = "roomData")]
    pub room: RoomData,
}

impl SaveRoomInfo {
    /// Room data initialised from `room` (current topic and full tag list) and
    /// guest data with only the visitor id, like the web form.
    pub fn from_room(room: &Room) -> Self {
        Self {
            guest: GuestData {
                id: room.visitor().map(|v| v.id).unwrap_or_default(),
                ..Default::default()
            },
            room: RoomData {
                id: room.id.clone(),
                topic: room.topic.clone(),
                tags: room.tags.clone().unwrap_or_default(),
                ..Default::default()
            },
        }
    }
}

/// `GET licenses.info` → `license`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LicenseInfo {
    #[serde(deserialize_with = "lenient::vec")]
    pub active_modules: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub external_modules: Vec<Value>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub prevented_actions: BTreeMap<String, bool>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub has_valid_license: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub trial: bool,
}

impl LicenseInfo {
    pub const LIVECHAT_ENTERPRISE: &'static str = "livechat-enterprise";
    pub const CANNED_RESPONSES: &'static str = "canned-responses";
    pub const CONTACT_ID_VERIFICATION: &'static str = "contact-id-verification";

    pub fn has_module(&self, module: &str) -> bool {
        self.active_modules.iter().any(|m| m == module)
    }

    /// `preventedActions.monthlyActiveContacts`: the MAC limit is reached.
    pub fn over_mac_limit(&self) -> bool {
        self.prevented_actions
            .get("monthlyActiveContacts")
            .copied()
            .unwrap_or(false)
    }
}

// --- typed views of the loose omnichannel fields of existing models ----------

impl Room {
    /// `v` as a typed visitor reference.
    pub fn visitor(&self) -> Option<VisitorRef> {
        self.v.clone().and_then(lenient::try_convert)
    }

    /// `servedBy` as a typed value (`None` while queued).
    pub fn served_by_agent(&self) -> Option<ServedBy> {
        self.served_by
            .clone()
            .and_then(lenient::try_convert::<ServedBy>)
            .filter(|s| !s.id.is_empty())
    }

    /// `source` as a typed value.
    pub fn omnichannel_source(&self) -> Option<OmnichannelSource> {
        self.source.clone().and_then(lenient::try_convert)
    }

    pub fn is_omnichannel(&self) -> bool {
        matches!(self.t, RoomType::Livechat | RoomType::Voip)
    }

    /// An open livechat room (closed rooms have `open` unset, not `false`).
    pub fn is_omnichannel_open(&self) -> bool {
        self.t == RoomType::Livechat && self.open == Some(true)
    }
}

impl Subscription {
    pub fn is_on_hold(&self) -> bool {
        self.on_hold == Some(true)
    }
}

impl User {
    pub fn is_livechat_agent(&self) -> bool {
        self.has_role("livechat-agent")
    }

    pub fn agent_status(&self) -> AgentStatus {
        self.status_livechat.unwrap_or_default()
    }
}

impl Message {
    /// Sent by an omnichannel visitor (`token` present).
    pub fn is_from_visitor(&self) -> bool {
        self.token.is_some()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn agent_status_trims_and_tolerates() {
        let s: AgentStatus = serde_json::from_value(json!("available ")).unwrap();
        assert_eq!(s, AgentStatus::Available);
        let s: AgentStatus = serde_json::from_value(json!("not-available")).unwrap();
        assert_eq!(s, AgentStatus::NotAvailable);
        let s: AgentStatus = serde_json::from_value(json!("weird")).unwrap();
        assert_eq!(s, AgentStatus::Unknown);
        let s: AgentStatus = serde_json::from_value(json!(5)).unwrap();
        assert_eq!(s, AgentStatus::Unknown);
        assert_eq!(
            serde_json::to_value(AgentStatus::NotAvailable).unwrap(),
            json!("not-available")
        );
    }

    #[test]
    fn agent_status_from_user_data() {
        let v = json!({"type":"updated","diff":{"statusLivechat":"available ","livechatStatusSystemModified":true},"unset":{}});
        assert_eq!(
            AgentStatus::from_user_data(&v),
            Some(AgentStatus::Available)
        );
        let v = json!({"type":"updated","diff":{"name":"x"},"unset":{"statusLivechat":1}});
        assert_eq!(
            AgentStatus::from_user_data(&v),
            Some(AgentStatus::NotAvailable)
        );
        let v = json!({"type":"updated","diff":{"name":"x"},"unset":{}});
        assert_eq!(AgentStatus::from_user_data(&v), None);
    }

    #[test]
    fn forward_body() {
        let f = ForwardChat {
            room_id: "r".into(),
            target: ForwardTarget::Department("d".into()),
            comment: Some(" ".into()),
        };
        assert_eq!(
            f.to_body(),
            json!({"roomId":"r","departmentId":"d","clientAction":true})
        );
        let f = ForwardChat {
            room_id: "r".into(),
            target: ForwardTarget::Agent("u".into()),
            comment: Some("hi".into()),
        };
        assert_eq!(
            f.to_body(),
            json!({"roomId":"r","userId":"u","comment":"hi","clientAction":true})
        );
    }

    #[test]
    fn close_and_save_bodies() {
        let c = CloseChat::new("r", Some("  ".into()));
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"rid":"r","transcriptEmail":{"sendToVisitor":false}})
        );
        let c = CloseChat {
            tags: vec!["a".into()],
            transcript_email: TranscriptEmail::to_visitor("v@x", "S"),
            ..CloseChat::new("r", Some("done".into()))
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"rid":"r","comment":"done","tags":["a"],
                   "transcriptEmail":{"sendToVisitor":true,"requestData":{"email":"v@x","subject":"S"}}})
        );
        let s = SaveRoomInfo {
            guest: GuestData {
                id: "v".into(),
                ..Default::default()
            },
            room: RoomData {
                id: "r".into(),
                topic: Some("t".into()),
                ..Default::default()
            },
        };
        // Tags are always sent (omitting them clears the room's tags).
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"guestData":{"_id":"v"},"roomData":{"_id":"r","topic":"t","tags":[]}})
        );
    }

    #[test]
    fn unknown_enums() {
        let s: OmnichannelSource = serde_json::from_value(json!({"type":"telegram"})).unwrap();
        assert_eq!(s.kind, SourceType::Unknown);
        let t: TransferData =
            serde_json::from_value(json!({"scope":"weird","comment":null})).unwrap();
        assert_eq!(t.scope, TransferScope::Unknown);
        let i: Inquiry =
            serde_json::from_value(json!({"_id":"i","rid":"r","status":"pending","v":"bad"}))
                .unwrap();
        assert_eq!(i.status, InquiryStatus::Unknown);
        assert_eq!(i.v, VisitorRef::default());
    }
}
