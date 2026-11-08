//! User administration (docs/admin.md §4).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::error::Error;
use crate::models::{Email, UserStatus, lenient};

/// A user row of `users.listByStatus`, or `users.info` with `includeUserRooms`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AdminUser {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub emails: Vec<Email>,
    #[serde(deserialize_with = "lenient::vec")]
    pub roles: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub status: Option<UserStatus>,
    pub status_text: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub active: Option<bool>,
    #[serde(rename = "avatarETag")]
    pub avatar_etag: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub last_login: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    /// `user`, `bot`, `app`, ….
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Registration reason (pending approval).
    pub reason: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub federated: Option<bool>,
    pub free_switch_extension: Option<String>,
    pub nickname: Option<String>,
    pub bio: Option<String>,
    pub custom_fields: Option<Value>,
    #[serde(deserialize_with = "lenient::opt")]
    pub require_password_change: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub can_view_all_info: Option<bool>,
    /// `users.info?includeUserRooms=true`: partial subscriptions.
    #[serde(deserialize_with = "lenient::vec")]
    pub rooms: Vec<AdminUserRoom>,
}

impl AdminUser {
    pub fn primary_email(&self) -> Option<&Email> {
        self.emails.first()
    }

    pub fn is_active(&self) -> bool {
        self.active.unwrap_or(true)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminUserRoom {
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    pub name: Option<String>,
    pub t: Option<String>,
    pub unread: Option<u64>,
    #[serde(deserialize_with = "lenient::vec")]
    pub roles: Vec<String>,
}

/// `status` filter of `users.listByStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserActiveFilter {
    Active,
    Deactivated,
}

impl UserActiveFilter {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deactivated => "deactivated",
        }
    }
}

/// Filters of `users.listByStatus` (docs/admin.md §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsersFilter {
    pub status: Option<UserActiveFilter>,
    /// `deactivated`, `pending_approval`, `idle_too_long`.
    pub inactive_reason: Vec<String>,
    /// `user`, `bot`, ….
    pub kind: Option<String>,
    /// Empty = no filter.
    pub roles: Vec<String>,
    pub search_term: Option<String>,
    pub has_logged_in: Option<bool>,
}

impl UsersFilter {
    /// Tab "All".
    pub fn all() -> Self {
        Self::default()
    }

    /// Tab "Pending".
    pub fn pending() -> Self {
        Self {
            kind: Some("user".into()),
            status: Some(UserActiveFilter::Deactivated),
            inactive_reason: vec!["pending_approval".into()],
            ..Self::default()
        }
    }

    /// Tab "Active".
    pub fn active() -> Self {
        Self {
            status: Some(UserActiveFilter::Active),
            ..Self::default()
        }
    }

    /// Tab "Deactivated".
    pub fn deactivated() -> Self {
        Self {
            status: Some(UserActiveFilter::Deactivated),
            inactive_reason: vec!["deactivated".into(), "idle_too_long".into()],
            ..Self::default()
        }
    }
}

/// `users.create` body. `password` is redacted in `Debug`.
#[derive(Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUser {
    pub email: String,
    pub name: String,
    /// Required by the schema even with `set_random_password`.
    pub password: String,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    /// The server joins default channels when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_default_channels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_password_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_random_password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_welcome_email: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_switch_extension: Option<String>,
}

impl fmt::Debug for NewUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewUser")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// `users.update` `data`: send only changed fields (docs/admin.md §4.3). `roles`
/// replaces the user's global roles. `password` is redacted in `Debug`.
#[derive(Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_password_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_random_password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_welcome_email: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_switch_extension: Option<String>,
}

impl fmt::Debug for UserUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserUpdate")
            .field("username", &self.username)
            .field("roles", &self.roles)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl UserUpdate {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `details` of a `user-last-owner` error: room names (`fname || name`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LastOwnerDetails {
    #[serde(deserialize_with = "lenient::vec")]
    pub should_change_owner: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub should_be_removed: Vec<String>,
}

impl LastOwnerDetails {
    /// The details when `e` is a `user-last-owner` error: repeat the call with
    /// `confirm_relinquish = true` after showing both lists.
    pub fn from_error(e: &Error) -> Option<Self> {
        match e {
            Error::Api {
                error_type: Some(t),
                details,
                ..
            } if t == "user-last-owner" => Some(
                details
                    .clone()
                    .and_then(lenient::try_convert)
                    .unwrap_or_default(),
            ),
            _ => None,
        }
    }
}

/// `users.setActiveStatus` → `user`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ActiveStatusResult {
    #[serde(rename = "_id")]
    pub id: String,
    pub active: bool,
}

/// User of `roles.getUsersInRole`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserInRole {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: Option<String>,
    pub username: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub emails: Vec<Email>,
    #[serde(rename = "avatarETag")]
    pub avatar_etag: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
}
