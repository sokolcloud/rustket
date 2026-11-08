use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;

/// Presence status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Online,
    Away,
    Busy,
    #[default]
    Offline,
    #[serde(other)]
    Unknown,
}

impl UserStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Away => "away",
            Self::Busy => "busy",
            Self::Offline | Self::Unknown => "offline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Email {
    pub address: String,
    pub verified: bool,
}

/// IUser as returned by `users.info`, `me`, login (`data.me`), presence lists, ….
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
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
    pub status_connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emails: Option<Vec<Email>>,
    /// `me` only: primary e-mail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(rename = "avatarETag", skip_serializing_if = "Option::is_none")]
    pub avatar_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<UserSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<UserServices>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_password_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_oauth_user: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<RcDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login: Option<RcDate>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
    /// Public E2E key (`e2e.getUsersOfRoomWithoutKey` etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2e: Option<Value>,
    /// Omnichannel agent availability (`me` of a `livechat-agent`).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::lenient::opt"
    )]
    pub status_livechat: Option<super::AgentStatus>,
    /// The availability was last changed by the server (logout, business hours).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::lenient::opt"
    )]
    pub livechat_status_system_modified: Option<bool>,
}

/// The logged-in user (`/api/v1/me`).
pub type Me = User;

impl User {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.username.as_deref())
            .unwrap_or(&self.id)
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.roles
            .as_ref()
            .is_some_and(|r| r.iter().any(|x| x == role))
    }

    pub fn preferences(&self) -> Option<&serde_json::Map<String, Value>> {
        self.settings.as_ref()?.preferences.as_ref()
    }

    pub fn totp_enabled(&self) -> bool {
        self.services
            .as_ref()
            .and_then(|s| s.totp.as_ref())
            .is_some_and(|t| t.enabled)
    }

    pub fn email_2fa_enabled(&self) -> bool {
        self.services
            .as_ref()
            .and_then(|s| s.email2fa.as_ref())
            .is_some_and(|t| t.enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Value>,
    /// Preferences map (`sendOnEnter`, `desktopNotifications`, `sidebarSortby`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferences: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserServices {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totp: Option<Enabled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email2fa: Option<Enabled>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Enabled {
    pub enabled: bool,
}
