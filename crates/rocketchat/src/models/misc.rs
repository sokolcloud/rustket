use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RoomType, UserStatus};
use crate::RcDate;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Permission {
    #[serde(rename = "_id")]
    pub id: String,
    pub roles: Vec<String>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Role {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `Users` or `Subscriptions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mandatory2fa: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEmoji {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub extension: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomUserStatus {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub status_type: UserStatus,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SlashCommand {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<Value>,
    pub client_only: bool,
    pub provides_preview: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

/// User entry in `spotlight`, `users.autocomplete`, `users.presence`, room members.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserSummary {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<UserStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<f64>,
    #[serde(rename = "avatarETag", skip_serializing_if = "Option::is_none")]
    pub avatar_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outside: Option<bool>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}

/// Room entry in `spotlight` / `rooms.autocomplete.*`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoomSummary {
    #[serde(rename = "_id")]
    pub id: String,
    pub t: RoomType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fname: Option<String>,
    #[serde(rename = "avatarETag", skip_serializing_if = "Option::is_none")]
    pub avatar_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_main: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub users_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpotlightResult {
    pub users: Vec<UserSummary>,
    pub rooms: Vec<RoomSummary>,
}

/// `users.presence`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Presence {
    pub users: Vec<UserSummary>,
    /// `true` when the list is complete (no `from` filter).
    pub full: bool,
}

/// `rooms.media` result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadedFile {
    #[serde(rename = "_id")]
    pub id: String,
    pub url: String,
}

/// `loadHistory` method result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "RawHistory", rename_all = "camelCase")]
pub struct History {
    /// Newest first; invalid messages are skipped (see `dropped`).
    pub messages: Vec<super::Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_unread: Option<Box<super::Message>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread_not_loaded: Option<u64>,
    /// Messages the server returned, including dropped ones: compare this (not
    /// `messages.len()`) with the requested limit to detect the end of history.
    #[serde(skip)]
    pub raw_len: usize,
    #[serde(skip)]
    pub dropped: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc(hidden)]
pub struct RawHistory {
    #[serde(default)]
    messages: Value,
    #[serde(default, deserialize_with = "super::lenient::opt")]
    first_unread: Option<Box<super::Message>>,
    #[serde(default, deserialize_with = "super::lenient::opt")]
    unread_not_loaded: Option<u64>,
}

impl From<RawHistory> for History {
    fn from(raw: RawHistory) -> Self {
        let c = super::lenient::counted(raw.messages);
        Self {
            messages: c.items,
            first_unread: raw.first_unread,
            unread_not_loaded: raw.unread_not_loaded,
            raw_len: c.raw_len,
            dropped: c.dropped,
        }
    }
}

/// `chat.syncMessages` result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "RawSyncMessages")]
pub struct SyncMessages {
    pub updated: Vec<super::Message>,
    pub deleted: Vec<super::Removed>,
    /// Entries skipped because they failed to parse.
    #[serde(skip)]
    pub dropped: usize,
}

#[derive(Deserialize)]
#[doc(hidden)]
pub struct RawSyncMessages {
    #[serde(default)]
    updated: Value,
    #[serde(default)]
    deleted: Value,
}

impl From<RawSyncMessages> for SyncMessages {
    fn from(raw: RawSyncMessages) -> Self {
        let u = super::lenient::counted(raw.updated);
        let d = super::lenient::counted(raw.deleted);
        Self {
            dropped: u.dropped + d.dropped,
            updated: u.items,
            deleted: d.items,
        }
    }
}
