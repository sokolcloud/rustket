//! Private settings editor backend, assets, OAuth/SAML helpers and server i18n
//! (docs/admin.md §14). Setting values are never logged.

use std::collections::HashMap;

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::AdminUpload;
use crate::RcDate;
use crate::auth::TwoFactorCode;
use crate::error::{Error, Result};
use crate::models::UpdatedSince;
use crate::models::admin::{
    ActionResult, LookupOption, SamlMetadata, SettingChange, SettingDef, parse_private_settings,
};
use crate::rest::{Query, RestClient};

#[derive(Deserialize, Default)]
#[serde(default)]
struct LookupData {
    data: Vec<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SingleSetting {
    value: Value,
}

impl RestClient {
    /// All settings the caller may manage, with full metadata (`private-settings/get`
    /// over `method.call`). Users without any settings permission get `[]`.
    pub async fn private_settings(&self) -> Result<Vec<SettingDef>> {
        let v = self
            .method_call_raw("private-settings/get", json!([]), false)
            .await?;
        Ok(parse_private_settings(v))
    }

    /// Settings changed (`update`) or removed (`remove`) after `since`.
    pub async fn private_settings_since(&self, since: RcDate) -> Result<UpdatedSince<SettingDef>> {
        self.method_call("private-settings/get", json!([since.ddp()]))
            .await
    }

    /// Bulk save (`POST settings`). 2FA is asked on **every** call. All checks run
    /// before the first write, so a rejected save changes nothing.
    pub async fn save_settings(
        &self,
        changes: &[SettingChange],
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let ids: Vec<&str> = changes.iter().map(|c| c.id.as_str()).collect();
        tracing::debug!(?ids, "saving settings");
        let _: Value = self
            .post_with("settings", &json!({ "settings": changes }), two_factor)
            .await?;
        Ok(())
    }

    /// `GET settings/:_id` → the stored value (`view-privileged-setting`).
    pub async fn setting_value(&self, id: &str) -> Result<Value> {
        let s: SingleSetting = self
            .get(
                &format!("settings/{}", crate::server_url::encode_segment(id)),
                &Query::new(),
            )
            .await?;
        Ok(s.value)
    }

    /// Runs an `action` setting through `POST settings/:_id {execute: true}`
    /// (`edit-privileged-setting` + 2FA). Prefer [`Self::setting_action_method`].
    pub async fn setting_execute(
        &self,
        id: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<Value> {
        self.post_with(
            &format!("settings/{}", crate::server_url::encode_segment(id)),
            &json!({ "execute": true }),
            two_factor,
        )
        .await
    }

    /// Runs the method form of an `action` setting (a Meteor method without params).
    pub async fn setting_action_method(&self, method: &str) -> Result<ActionResult> {
        let v = self.method_call_raw(method, json!([]), false).await?;
        Ok(serde_json::from_value(v).unwrap_or_default())
    }

    /// Runs the endpoint form of an `action` setting: `{method, path}` relative to
    /// `/api` (e.g. `POST /v1/push.test`) with an empty body.
    pub async fn setting_action_endpoint(&self, method: &str, path: &str) -> Result<ActionResult> {
        let m = Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::Protocol(format!("invalid action method {method}")))?;
        let url = self.server().api_root(path);
        let rb = self.request(m.clone(), &url, None);
        let rb = if m == Method::GET {
            rb
        } else {
            rb.json(&json!({}))
        };
        let v: Value = self.send(rb).await?;
        Ok(serde_json::from_value(v).unwrap_or_default())
    }

    /// Options of a `lookup` setting: `GET /api/<lookupEndpoint>`.
    pub async fn settings_lookup(&self, endpoint: &str) -> Result<Vec<LookupOption>> {
        let url = self.server().api_root(endpoint);
        let d: LookupData = self.send(self.request(Method::GET, &url, None)).await?;
        Ok(crate::models::lenient::vec_from_value(Value::Array(d.data)))
    }

    /// Uploads an asset (`assets.setAsset`, multipart `asset` + `assetName`).
    pub async fn assets_set(
        &self,
        asset_name: &str,
        file: AdminUpload,
        refresh_all_clients: bool,
    ) -> Result<()> {
        let mut fields = vec![("assetName", asset_name.to_owned())];
        if refresh_all_clients {
            fields.push(("refreshAllClients", "true".to_owned()));
        }
        self.upload_form("assets.setAsset", fields, Some(("asset", file)), None)
            .await?;
        Ok(())
    }

    pub async fn assets_unset(&self, asset_name: &str, refresh_all_clients: bool) -> Result<()> {
        self.post_ok(
            "assets.unsetAsset",
            &json!({ "assetName": asset_name, "refreshAllClients": refresh_all_clients }),
        )
        .await
    }

    /// `settings.addCustomOAuth` (2FA).
    pub async fn settings_add_custom_oauth(
        &self,
        name: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with(
                "settings.addCustomOAuth",
                &json!({ "name": name }),
                two_factor,
            )
            .await?;
        Ok(())
    }

    /// `settings.removeCustomOAuth` (2FA); `name` is lower-cased as the server expects.
    pub async fn settings_remove_custom_oauth(
        &self,
        name: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with(
                "settings.removeCustomOAuth",
                &json!({ "name": name.to_lowercase() }),
                two_factor,
            )
            .await?;
        Ok(())
    }

    /// `settings.refreshOAuthServices` (2FA).
    pub async fn settings_refresh_oauth_services(
        &self,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<()> {
        let _: Value = self
            .post_with("settings.refreshOAuthServices", &json!({}), two_factor)
            .await?;
        Ok(())
    }

    /// `saml.parseMetadata {url}`: the server fetches the IdP metadata URL.
    pub async fn saml_parse_metadata(&self, url: &str) -> Result<SamlMetadata> {
        self.post("saml.parseMetadata", &json!({ "url": url }))
            .await
    }

    /// Server translations `GET /i18n/{lang}.json` (no login needed). The body is cut
    /// short without compression (server bug, docs/admin.md §14.12); reqwest asks for
    /// gzip. A parse error means "translations unavailable".
    pub async fn server_translations(&self, lang: &str) -> Result<HashMap<String, String>> {
        if lang.is_empty()
            || !lang
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::Protocol(format!("invalid language code {lang:?}")));
        }
        let url = self.server().join(&format!("i18n/{lang}.json"));
        let resp = self
            .http()
            .get(&url)
            .timeout(self.config().api_timeout)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, None, &body));
        }
        server_translations_from_bytes(&body)
    }
}

/// Parses a server translation file (flat `{key: text}`); non-string values are
/// skipped.
pub fn server_translations_from_bytes(body: &[u8]) -> Result<HashMap<String, String>> {
    let v: serde_json::Map<String, Value> = serde_json::from_slice(body)
        .map_err(|e| Error::Protocol(format!("translations unavailable: {e}")))?;
    Ok(v.into_iter()
        .filter_map(|(k, v)| match v {
            Value::String(s) => Some((k, s)),
            _ => None,
        })
        .collect())
}
