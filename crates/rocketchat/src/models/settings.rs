use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;

/// `GET /api/info` (unauthenticated: short version only; authenticated: `info.version`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerInfo {
    /// `8.9` without authentication.
    pub version: Option<String>,
    pub info: Option<ServerBuildInfo>,
    pub workspace_url: Option<String>,
    pub hashed_workspace_url: Option<String>,
    pub minimum_client_versions: Option<MinimumClientVersions>,
    /// Signed JWT (`supportedVersions.signed`).
    pub supported_versions: Option<Value>,
    pub cloud_workspace_id: Option<String>,
}

impl ServerInfo {
    /// The most precise version string available.
    pub fn best_version(&self) -> Option<&str> {
        self.info
            .as_ref()
            .and_then(|i| i.version.as_deref())
            .or(self.version.as_deref())
    }

    /// `(major, minor, patch)` of [`Self::best_version`].
    pub fn version_tuple(&self) -> Option<(u32, u32, u32)> {
        parse_version(self.best_version()?)
    }
}

pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>());
    let major = it.next()?.ok()?;
    let minor = it.next().and_then(Result::ok).unwrap_or(0);
    let patch = it.next().and_then(Result::ok).unwrap_or(0);
    Some((major, minor, patch))
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerBuildInfo {
    pub version: Option<String>,
    pub build: Option<Value>,
    pub commit: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MinimumClientVersions {
    pub desktop: Option<String>,
    pub mobile: Option<String>,
}

/// ISetting as returned by `settings.public` / the `public-settings` stream.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Setting {
    #[serde(rename = "_id")]
    pub id: String,
    pub value: Value,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enterprise: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modules: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i18n_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Value>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}

impl Setting {
    pub fn as_bool(&self) -> Option<bool> {
        self.value.as_bool()
    }

    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.value.as_i64()
    }
}

/// Entry of `settings.oauth` / `service.configurations`.
///
/// Built-in OAuth providers carry `name` (`google`, `github`, `gitlab`, …); custom
/// OAuth, SAML and CAS carry `service`. Use [`Self::service_name`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginServiceConfig {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Facebook uses `appId`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Twitter uses `consumerKey`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<bool>,
    #[serde(rename = "serverURL", skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorize_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `popup`, `redirect` or empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_label_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button_label_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_button: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_button_on_mobile: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    // SAML
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_point: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_config: Option<ClientConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    // CAS
    #[serde(rename = "login_url", skip_serializing_if = "Option::is_none")]
    pub login_url: Option<String>,
    #[serde(rename = "base_url", skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoclose: Option<bool>,
}

/// SAML `clientConfig` (`provider`), LinkedIn `requestPermissions`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClientConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_permissions: Option<Vec<String>>,
}

/// Kind of login service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginServiceKind {
    OAuth,
    CustomOAuth,
    Saml,
    Cas,
}

impl LoginServiceConfig {
    /// `service` or `name` (lowercase service id used in `/_oauth/{service}`).
    pub fn service_name(&self) -> &str {
        self.service
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or_default()
    }

    pub fn kind(&self) -> LoginServiceKind {
        match self.service_name() {
            "saml" => LoginServiceKind::Saml,
            "cas" => LoginServiceKind::Cas,
            _ if self.custom == Some(true) => LoginServiceKind::CustomOAuth,
            _ => LoginServiceKind::OAuth,
        }
    }

    /// `clientId`, `appId` (Facebook) or `consumerKey` (Twitter).
    pub fn client_id(&self) -> Option<&str> {
        self.client_id
            .as_deref()
            .or(self.app_id.as_deref())
            .or(self.consumer_key.as_deref())
            .filter(|s| !s.is_empty())
    }

    /// Label for a login button.
    pub fn button_label(&self) -> String {
        if let Some(l) = self.button_label_text.as_deref().filter(|s| !s.is_empty()) {
            return l.to_owned();
        }
        let n = self.service_name();
        let mut c = n.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    }

    /// Whether a button should be shown (custom OAuth honours `showButton`).
    pub fn is_visible(&self) -> bool {
        self.show_button != Some(false) && self.enabled != Some(false)
    }
}
