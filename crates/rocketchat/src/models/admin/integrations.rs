//! Incoming / outgoing webhooks (docs/admin.md §10). Tokens and scripts are
//! secrets: they are redacted in `Debug`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::models::{UserRef, lenient};

/// `type` of an integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum IntegrationType {
    #[default]
    #[serde(rename = "webhook-incoming")]
    Incoming,
    #[serde(rename = "webhook-outgoing")]
    Outgoing,
    #[serde(other)]
    Unknown,
}

impl IntegrationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Incoming => "webhook-incoming",
            Self::Outgoing => "webhook-outgoing",
            Self::Unknown => "",
        }
    }
}

/// Outgoing webhook trigger (`event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutgoingEvent {
    SendMessage,
    FileUploaded,
    RoomArchived,
    RoomCreated,
    RoomJoined,
    RoomLeft,
    UserCreated,
    #[serde(other)]
    Unknown,
}

impl OutgoingEvent {
    pub const ALL: [Self; 7] = [
        Self::SendMessage,
        Self::FileUploaded,
        Self::RoomArchived,
        Self::RoomCreated,
        Self::RoomJoined,
        Self::RoomLeft,
        Self::UserCreated,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SendMessage => "sendMessage",
            Self::FileUploaded => "fileUploaded",
            Self::RoomArchived => "roomArchived",
            Self::RoomCreated => "roomCreated",
            Self::RoomJoined => "roomJoined",
            Self::RoomLeft => "roomLeft",
            Self::UserCreated => "userCreated",
            Self::Unknown => "",
        }
    }

    /// Server i18n key of the label.
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::SendMessage => "Integrations_Outgoing_Type_SendMessage",
            Self::FileUploaded => "Integrations_Outgoing_Type_FileUploaded",
            Self::RoomArchived => "Integrations_Outgoing_Type_RoomArchived",
            Self::RoomCreated => "Integrations_Outgoing_Type_RoomCreated",
            Self::RoomJoined => "Integrations_Outgoing_Type_RoomJoined",
            Self::RoomLeft => "Integrations_Outgoing_Type_RoomLeft",
            Self::UserCreated => "Integrations_Outgoing_Type_UserCreated",
            Self::Unknown => "",
        }
    }

    pub fn uses_channel(&self) -> bool {
        matches!(
            self,
            Self::SendMessage | Self::FileUploaded | Self::RoomJoined | Self::RoomLeft
        )
    }

    pub fn uses_trigger_words(&self) -> bool {
        matches!(self, Self::SendMessage)
    }

    pub fn uses_target_room(&self) -> bool {
        matches!(self, Self::UserCreated)
    }
}

/// `retryDelay` of outgoing webhooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetryDelay {
    PowersOfTen,
    PowersOfTwo,
    IncrementsOfTwo,
    #[serde(other)]
    Unknown,
}

/// `scriptError` of an integration.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptError {
    pub name: Option<String>,
    pub message: Option<String>,
    pub stack: Option<String>,
}

/// An incoming or outgoing integration. Incoming-only and outgoing-only fields are
/// optional.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Integration {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type", deserialize_with = "lenient::or_default")]
    pub kind: IntegrationType,
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub enabled: bool,
    /// Stored as a list (`#channel`, `@user`).
    #[serde(deserialize_with = "channels")]
    pub channel: Vec<String>,
    pub username: Option<String>,
    pub user_id: Option<String>,
    pub alias: Option<String>,
    pub avatar: Option<String>,
    pub emoji: Option<String>,
    pub token: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub script_enabled: bool,
    pub script_engine: Option<String>,
    pub script: Option<String>,
    pub script_compiled: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub script_error: Option<ScriptError>,
    #[serde(deserialize_with = "lenient::opt")]
    pub override_destination_channel_enabled: Option<bool>,
    // Outgoing.
    #[serde(deserialize_with = "lenient::opt")]
    pub event: Option<OutgoingEvent>,
    #[serde(deserialize_with = "lenient::vec")]
    pub urls: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub trigger_words: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub trigger_word_anywhere: Option<bool>,
    pub target_room: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub impersonate_user: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub run_on_edits: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub retry_failed_calls: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub retry_count: Option<u32>,
    #[serde(deserialize_with = "lenient::opt")]
    pub retry_delay: Option<RetryDelay>,
    #[serde(rename = "_createdBy", deserialize_with = "lenient::opt")]
    pub created_by: Option<UserRef>,
    #[serde(rename = "_createdAt", deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

fn channels<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match <Value as serde::Deserialize>::deserialize(d)? {
        Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        v => lenient::vec_from_value(v),
    })
}

impl fmt::Debug for Integration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Integration")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Integration {
    /// Incoming webhook URL `{site_url}/hooks/{_id}/{token}`.
    pub fn webhook_url(&self, site_url: &str) -> Option<String> {
        let token = self.token.as_deref().filter(|t| !t.is_empty())?;
        Some(format!(
            "{}/hooks/{}/{}",
            site_url.trim_end_matches('/'),
            self.id,
            token
        ))
    }
}

/// Body of `integrations.create` / `integrations.update` (docs/admin.md §10.2).
/// `channel` is a string (comma-separated) and must be sent even when unused (`""`).
/// For outgoing updates `token` must be non-empty.
#[derive(Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationParams {
    #[serde(rename = "type")]
    pub kind: IntegrationType,
    /// Required by `integrations.update`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_id: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub channel: String,
    pub username: String,
    pub script_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_destination_channel_enabled: Option<bool>,
    // Outgoing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<OutgoingEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_words: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_word_anywhere: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impersonate_user: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_on_edits: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_failed_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_delay: Option<RetryDelay>,
}

impl fmt::Debug for IntegrationParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntegrationParams")
            .field("kind", &self.kind)
            .field("integration_id", &self.integration_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl IntegrationParams {
    /// A new incoming webhook posting as `username` into `channel`.
    pub fn incoming(name: &str, channel: &str, username: &str) -> Self {
        Self {
            kind: IntegrationType::Incoming,
            name: name.into(),
            enabled: true,
            channel: channel.into(),
            username: username.into(),
            ..Self::default()
        }
    }

    /// A new outgoing webhook with a random token (docs/admin.md §10.2: the token is
    /// required to update it later).
    pub fn outgoing(name: &str, event: OutgoingEvent, urls: Vec<String>, username: &str) -> Self {
        Self {
            kind: IntegrationType::Outgoing,
            name: name.into(),
            enabled: true,
            event: Some(event),
            urls: Some(urls),
            username: username.into(),
            token: Some(crate::ids::random_secret(24)),
            ..Self::default()
        }
    }

    /// Parameters to update `integration` unchanged (edit the result before sending).
    pub fn from_integration(i: &Integration) -> Self {
        Self {
            kind: i.kind,
            integration_id: Some(i.id.clone()),
            name: i.name.clone().unwrap_or_default(),
            enabled: i.enabled,
            channel: i.channel.join(", "),
            username: i.username.clone().unwrap_or_default(),
            script_enabled: i.script_enabled,
            script_engine: i.script_engine.clone(),
            script: i.script.clone(),
            alias: i.alias.clone(),
            avatar: i.avatar.clone(),
            emoji: i.emoji.clone(),
            override_destination_channel_enabled: i.override_destination_channel_enabled,
            event: i.event,
            urls: (i.kind == IntegrationType::Outgoing).then(|| i.urls.clone()),
            trigger_words: (i.kind == IntegrationType::Outgoing).then(|| i.trigger_words.clone()),
            trigger_word_anywhere: i.trigger_word_anywhere,
            target_room: i.target_room.clone(),
            impersonate_user: i.impersonate_user,
            token: (i.kind == IntegrationType::Outgoing)
                .then(|| i.token.clone())
                .flatten(),
            run_on_edits: i.run_on_edits,
            retry_failed_calls: i.retry_failed_calls,
            retry_count: i.retry_count,
            retry_delay: i.retry_delay,
        }
    }
}

/// Entry of `integrations.history` (outgoing webhooks). The payloads carry the
/// webhook token, so `Debug` omits them.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntegrationHistory {
    #[serde(rename = "_id")]
    pub id: String,
    pub step: Option<String>,
    pub event: Option<String>,
    pub url: Option<String>,
    pub trigger_word: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub finished: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ran_prepare_script: Option<bool>,
    pub http_call_data: Option<Value>,
    pub http_error: Option<Value>,
    pub http_result: Option<Value>,
    pub error: Option<Value>,
    pub error_stack: Option<Value>,
    pub data: Option<Value>,
    #[serde(rename = "_createdAt", deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

impl fmt::Debug for IntegrationHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntegrationHistory")
            .field("id", &self.id)
            .field("step", &self.step)
            .field("event", &self.event)
            .field("finished", &self.finished)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}
