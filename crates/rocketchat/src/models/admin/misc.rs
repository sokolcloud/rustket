//! Invites, custom sounds, OAuth apps, e-mail inboxes, mailer, roles, banners
//! (docs/admin.md §6, §8, §11–13, §16, §19).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::models::{UserRef, lenient};

/// `IInvite`. `url` is empty in stored invites (`listInvites`); rebuild it with
/// `rustket_core::admin::invites::invite_url`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Invite {
    /// Also the token.
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    pub user_id: Option<String>,
    /// 0 = never expires.
    #[serde(deserialize_with = "lenient::or_default")]
    pub days: u32,
    /// 0 = unlimited.
    #[serde(deserialize_with = "lenient::or_default")]
    pub max_uses: u32,
    #[serde(deserialize_with = "lenient::or_default")]
    pub uses: u32,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    /// `null` when the invite never expires.
    #[serde(deserialize_with = "lenient::opt")]
    pub expires: Option<RcDate>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub url: String,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

impl Invite {
    /// Allowed `days` of `findOrCreateInvite`.
    pub const DAYS: [u32; 5] = [0, 1, 7, 15, 30];
    /// Allowed `maxUses` of `findOrCreateInvite`.
    pub const MAX_USES: [u32; 7] = [0, 1, 5, 10, 25, 50, 100];

    /// `None` = unlimited.
    pub fn uses_left(&self) -> Option<u32> {
        (self.max_uses > 0).then(|| self.max_uses.saturating_sub(self.uses))
    }

    pub fn is_expired(&self, now: RcDate) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}

/// A custom sound (`custom-sounds.list` / `getOne`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomSound {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub extension: String,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

impl CustomSound {
    /// Server-relative file path `/custom-sounds/{_id}.{extension}`.
    pub fn path(&self) -> String {
        format!("/custom-sounds/{}.{}", self.id, self.extension)
    }
}

/// An OAuth2 client app (Rocket.Chat as provider). `clientSecret` is redacted in
/// `Debug`.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OAuthApp {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub active: bool,
    /// Comma-separated redirect URIs.
    pub redirect_uri: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    #[serde(rename = "_createdAt", deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_createdBy", deserialize_with = "lenient::opt")]
    pub created_by: Option<UserRef>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

impl fmt::Debug for OAuthApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthApp")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("active", &self.active)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// SMTP or IMAP part of an e-mail inbox. `password` is redacted in `Debug`.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EmailInboxServer {
    pub server: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub port: u16,
    pub username: String,
    pub password: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub secure: bool,
    /// IMAP only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
}

impl fmt::Debug for EmailInboxServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailInboxServer")
            .field("server", &self.server)
            .field("port", &self.port)
            .field("secure", &self.secure)
            .finish_non_exhaustive()
    }
}

/// An e-mail inbox (`email-inbox.list`, `email-inbox/:_id`); also the body of
/// `POST email-inbox` (create without `_id`, update with it).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EmailInbox {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub email: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    pub smtp: EmailInboxServer,
    pub imap: EmailInboxServer,
    #[serde(
        rename = "_createdAt",
        deserialize_with = "lenient::opt",
        skip_serializing
    )]
    pub created_at: Option<RcDate>,
    #[serde(
        rename = "_updatedAt",
        deserialize_with = "lenient::opt",
        skip_serializing
    )]
    pub updated_at: Option<RcDate>,
}

/// `POST mailer` body. `query` is an EJSON Mongo filter string on users.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct MailerRequest {
    pub from: String,
    pub subject: String,
    /// HTML body; must contain `[unsubscribe]`.
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dryrun: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

impl MailerRequest {
    /// The placeholder the body must contain.
    pub const UNSUBSCRIBE: &'static str = "[unsubscribe]";

    pub fn has_unsubscribe(&self) -> bool {
        self.body.contains(Self::UNSUBSCRIBE)
    }
}

/// `roles.create` / `roles.update` body. For protected roles always send the current
/// `name`, `scope` and `description` (docs/admin.md §13.3).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mandatory2fa: Option<bool>,
}

impl RoleParams {
    /// Update parameters that repeat every current field of `role`.
    pub fn update_of(role: &crate::models::Role) -> Self {
        Self {
            role_id: Some(role.id.clone()),
            name: role.name.clone(),
            scope: Some(role.scope.clone().unwrap_or_else(|| "Users".into())),
            description: Some(role.description.clone().unwrap_or_default()),
            mandatory2fa: role.mandatory2fa,
        }
    }
}

/// A setting permission (`level: "settings"`) or an ordinary permission with all
/// fields of `permissions.listAll`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PermissionDef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "lenient::vec")]
    pub roles: Vec<String>,
    /// `settings` for setting permissions.
    pub level: Option<String>,
    pub setting_id: Option<String>,
    pub group: Option<String>,
    pub group_permission_id: Option<String>,
    pub section: Option<String>,
    pub section_permission_id: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub sorter: Option<i64>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

impl PermissionDef {
    pub fn is_setting_permission(&self) -> bool {
        self.level.as_deref() == Some("settings")
    }
}

/// `{message}` answer of LDAP tests and sync (`LDAP_Connection_successful`, …): an
/// i18n key.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct MessageResult {
    pub message: String,
}

/// A banner (`GET banners`); its UiKit view is rendered by Phase 10.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Banner {
    #[serde(rename = "_id")]
    pub id: String,
    pub platform: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub expire_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub start_at: Option<RcDate>,
    pub view: Option<Value>,
    #[serde(deserialize_with = "lenient::opt")]
    pub active: Option<bool>,
}
