//! Apps-engine endpoints under `/api/apps/` (not `/api/v1`), UIKit
//! interactions and banners (docs/uikit.md §4.1, §5.7, §6, §8).

use std::time::Duration;

use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{Query, RestClient, field, redirect_error};
use crate::error::{Error, Result};
use crate::models::{
    ActionButton, AppInfo, AppLanguages, AppLogs, AppLogsDistinct, AppLogsQuery, AppSetting,
    AppStatus, AppStatusInfo, AppsCount, Banner, LanguageMap, language_map, lenient,
};
use crate::server_url::encode_segment;
use crate::uikit::{InteractionResponse, UserInteraction};

/// Client-side time limit of one UIKit interaction (`ActionManager` uses 5 s).
pub const UI_INTERACTION_TIMEOUT: Duration = Duration::from_secs(5);

/// An app id for a URL path segment. App ids in blocks are untrusted, so ids
/// with path or query characters are refused; the rest is percent-encoded.
fn app_segment(app_id: &str) -> Result<String> {
    if app_id.is_empty()
        || app_id.contains(['/', '?', '#', '\\'])
        || app_id == "."
        || app_id == ".."
    {
        return Err(Error::Url(format!("invalid app id {app_id:?}")));
    }
    Ok(encode_segment(app_id))
}

/// Rate-limit reset (Unix ms) of an error answer. The apps router uses
/// express-rate-limit 5: `Retry-After` in seconds on 429, `X-RateLimit-Reset`
/// in Unix seconds; the REST API sends `x-ratelimit-reset` in Unix ms.
fn ratelimit_reset_ms(headers: &reqwest::header::HeaderMap, now_ms: i64) -> Option<i64> {
    let num = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<i64>().ok())
    };
    if let Some(secs) = num("retry-after") {
        return Some(now_ms.saturating_add(secs.max(0).saturating_mul(1000)));
    }
    num("x-ratelimit-reset").map(|r| {
        if r < 100_000_000_000 {
            r.saturating_mul(1000)
        } else {
            r
        }
    })
}

/// `settings` of `GET /api/apps/{id}/settings` in the order the server sent
/// (an object keyed by id in the app's declaration order, or an array).
#[derive(Default)]
struct OrderedSettings(Vec<AppSetting>);

impl<'de> serde::Deserialize<'de> for OrderedSettings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = OrderedSettings;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object or array of settings")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some((_, v)) = map.next_entry::<serde::de::IgnoredAny, Value>()? {
                    out.extend(lenient::try_convert(v));
                }
                Ok(OrderedSettings(out))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = seq.next_element::<Value>()? {
                    out.extend(lenient::try_convert(v));
                }
                Ok(OrderedSettings(out))
            }
            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(OrderedSettings::default())
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(serde::Deserialize)]
struct SettingsBody {
    #[serde(default)]
    settings: OrderedSettings,
}

/// Parses a settings answer body, keeping the server's order.
fn parse_settings(body: &[u8]) -> Result<Vec<AppSetting>> {
    Ok(serde_json::from_slice::<SettingsBody>(body)?.settings.0)
}

fn logs_query(q: &AppLogsQuery) -> Query {
    Query::new()
        .opt("count", q.count)
        .opt("offset", q.offset)
        .opt("logLevel", q.log_level)
        .opt("method", q.method.as_deref())
        .opt("instanceId", q.instance_id.as_deref())
        .opt("startDate", q.start_date.as_deref())
        .opt("endDate", q.end_date.as_deref())
        .opt("sort", q.sort.as_deref())
}

impl RestClient {
    /// `GET /api/{path}{query}` with the Rocket.Chat envelope handling.
    async fn apps_get<T: DeserializeOwned>(&self, path: &str, query: &Query) -> Result<T> {
        let url = self.inner.server.api_root(&query.apply(path));
        self.send(self.request(Method::GET, &url, None)).await
    }

    async fn apps_send<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let url = self.inner.server.api_root(path);
        let mut rb = self.request(method, &url, None);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        self.send(rb).await
    }

    /// URL of `GET /api/apps/{id}/icon` (public; image bytes, possibly SVG).
    pub fn app_icon_url(&self, app_id: &str) -> Result<String> {
        Ok(self
            .inner
            .server
            .api_root(&format!("apps/{}/icon", app_segment(app_id)?)))
    }

    /// `POST /api/apps/ui.interaction/{app_id}`. Any 2xx body is returned (an
    /// `errors` answer is `success: false` with HTTP 200): an empty body is
    /// [`InteractionResponse::Empty`], bodies without `type` are `Ack`. Non-2xx
    /// answers (`{error}` bodies, the HTML 413) map to errors; after
    /// [`UI_INTERACTION_TIMEOUT`] the call fails with [`Error::Timeout`].
    pub async fn ui_interaction(
        &self,
        app_id: &str,
        interaction: &UserInteraction,
    ) -> Result<InteractionResponse> {
        self.ui_interaction_with_timeout(app_id, interaction, UI_INTERACTION_TIMEOUT)
            .await
    }

    /// [`Self::ui_interaction`] with a custom time limit.
    pub async fn ui_interaction_with_timeout(
        &self,
        app_id: &str,
        interaction: &UserInteraction,
        timeout: Duration,
    ) -> Result<InteractionResponse> {
        let url = self
            .inner
            .server
            .api_root(&format!("apps/ui.interaction/{}", app_segment(app_id)?));
        let rb = self
            .request(Method::POST, &url, None)
            .timeout(timeout)
            .json(interaction);
        let map_timeout = |e: reqwest::Error| {
            if e.is_timeout() {
                Error::Timeout("app interaction")
            } else {
                Error::from(e)
            }
        };
        let resp = rb.send().await.map_err(map_timeout)?;
        if resp.status().is_redirection() {
            return Err(redirect_error(&resp));
        }
        let status = resp.status().as_u16();
        let reset = ratelimit_reset_ms(resp.headers(), chrono::Utc::now().timestamp_millis());
        let body = resp.bytes().await.map_err(map_timeout)?;
        if !(200..300).contains(&status) {
            tracing::debug!(
                status,
                app_id,
                kind = interaction.type_name(),
                "ui.interaction failed"
            );
            return Err(Error::from_response(status, reset, &body));
        }
        InteractionResponse::from_body(&body)
    }

    /// `GET /api/apps/actionButtons`: a bare array (no envelope; no auth needed).
    /// Invalid entries are skipped.
    pub async fn action_buttons(&self) -> Result<Vec<ActionButton>> {
        let v: Value = self.apps_get("apps/actionButtons", &Query::new()).await?;
        Ok(lenient::vec_from_value(v))
    }

    /// `GET /api/apps/languages`: every app's translations.
    pub async fn apps_languages(&self) -> Result<Vec<AppLanguages>> {
        let v: Value = self.apps_get("apps/languages", &Query::new()).await?;
        Ok(v.get("apps")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(AppLanguages::from_value).collect())
            .unwrap_or_default())
    }

    /// `GET /api/apps/{id}/languages`.
    pub async fn app_languages(&self, app_id: &str) -> Result<LanguageMap> {
        let v: Value = self
            .apps_get(
                &format!("apps/{}/languages", app_segment(app_id)?),
                &Query::new(),
            )
            .await?;
        Ok(v.get("languages").map(language_map).unwrap_or_default())
    }

    /// `GET /api/apps/count` (public).
    pub async fn apps_count(&self) -> Result<AppsCount> {
        self.apps_get("apps/count", &Query::new()).await
    }

    /// `GET /api/apps/installed?includeClusterStatus=true` (any logged-in user).
    pub async fn installed_apps(&self) -> Result<Vec<AppInfo>> {
        let v: Value = self
            .apps_get(
                "apps/installed",
                &Query::new().push("includeClusterStatus", "true"),
            )
            .await?;
        Ok(lenient::vec_from_value(
            v.get("apps").cloned().unwrap_or_default(),
        ))
    }

    /// `GET /api/apps/{id}` (`manage-apps`).
    pub async fn app_info(&self, app_id: &str) -> Result<AppInfo> {
        let v: Value = self
            .apps_get(&format!("apps/{}", app_segment(app_id)?), &Query::new())
            .await?;
        field(v, "app")
    }

    /// `GET /api/apps/{id}/status` (`manage-apps`).
    pub async fn app_status(&self, app_id: &str) -> Result<AppStatusInfo> {
        self.apps_get(
            &format!("apps/{}/status", app_segment(app_id)?),
            &Query::new(),
        )
        .await
    }

    /// `POST /api/apps/{id}/status` with `manually_enabled` / `manually_disabled`;
    /// returns the new status.
    pub async fn set_app_status(&self, app_id: &str, enabled: bool) -> Result<AppStatus> {
        let status = if enabled {
            AppStatus::ManuallyEnabled
        } else {
            AppStatus::ManuallyDisabled
        };
        let v: Value = self
            .apps_send(
                Method::POST,
                &format!("apps/{}/status", app_segment(app_id)?),
                Some(&json!({ "status": status })),
            )
            .await?;
        Ok(v.get("status")
            .cloned()
            .and_then(|s| serde_json::from_value(s).ok())
            .unwrap_or_default())
    }

    /// `GET /api/apps/{id}/settings`: an object keyed by setting id (the typings
    /// say array); returned as a list in the app's declaration order (the
    /// object's key order). Hidden settings are removed by the server.
    pub async fn app_settings(&self, app_id: &str) -> Result<Vec<AppSetting>> {
        // Parsed from the bytes: a `Value` (sorted map) would lose the order.
        let url = self
            .inner
            .server
            .api_root(&format!("apps/{}/settings", app_segment(app_id)?));
        let resp = self.request(Method::GET, &url, None).send().await?;
        if resp.status().is_redirection() {
            return Err(redirect_error(&resp));
        }
        let status = resp.status().as_u16();
        let reset = ratelimit_reset_ms(resp.headers(), chrono::Utc::now().timestamp_millis());
        let body = resp.bytes().await?;
        let failed = !(200..300).contains(&status)
            || serde_json::from_slice::<Value>(&body).is_ok_and(|v| {
                v.get("success").and_then(Value::as_bool) == Some(false)
                    || v.get("status").and_then(Value::as_str) == Some("error")
            });
        if failed {
            return Err(Error::from_response(status, reset, &body));
        }
        parse_settings(&body)
    }

    /// `POST /api/apps/{id}/settings` with an array (only entries whose value
    /// changed are applied); returns `updated`.
    pub async fn set_app_settings(
        &self,
        app_id: &str,
        settings: &[AppSetting],
    ) -> Result<Vec<AppSetting>> {
        let v: Value = self
            .apps_send(
                Method::POST,
                &format!("apps/{}/settings", app_segment(app_id)?),
                Some(&json!({ "settings": settings })),
            )
            .await?;
        Ok(lenient::vec_from_value(
            v.get("updated").cloned().unwrap_or_default(),
        ))
    }

    /// `GET /api/apps/{id}/logs` (`manage-apps`).
    pub async fn app_logs(&self, app_id: &str, query: &AppLogsQuery) -> Result<AppLogs> {
        self.apps_get(
            &format!("apps/{}/logs", app_segment(app_id)?),
            &logs_query(query),
        )
        .await
    }

    /// `GET /api/apps/logs?appId=…` (all apps when `app_id` is `None`).
    pub async fn apps_logs(&self, app_id: Option<&str>, query: &AppLogsQuery) -> Result<AppLogs> {
        self.apps_get("apps/logs", &logs_query(query).opt("appId", app_id))
            .await
    }

    /// `GET /api/apps/{id}/logs/distinctValues`.
    pub async fn app_logs_distinct(&self, app_id: &str) -> Result<AppLogsDistinct> {
        self.apps_get(
            &format!("apps/{}/logs/distinctValues", app_segment(app_id)?),
            &Query::new(),
        )
        .await
    }

    /// `DELETE /api/apps/{id}` (uninstall); returns the app with `status: disabled`.
    pub async fn uninstall_app(&self, app_id: &str) -> Result<AppInfo> {
        let v: Value = self
            .apps_send::<Value, Value>(
                Method::DELETE,
                &format!("apps/{}", app_segment(app_id)?),
                None,
            )
            .await?;
        field(v, "app")
    }

    /// `GET /api/v1/banners?platform=web` (UIKit banners for the user).
    pub async fn banners(&self) -> Result<Vec<Banner>> {
        let v: Value = self
            .get("banners", &Query::new().push("platform", "web"))
            .await?;
        Ok(lenient::vec_from_value(
            v.get("banners").cloned().unwrap_or_default(),
        ))
    }

    /// `POST /api/v1/banners.dismiss`.
    pub async fn dismiss_banner(&self, banner_id: &str) -> Result<()> {
        self.post_ok("banners.dismiss", &json!({ "bannerId": banner_id }))
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn settings_keep_the_server_order() {
        let body = br#"{"settings":{"zeta":{"id":"zeta","type":"string","packageValue":"","required":false,"public":false,"i18nLabel":"Z"},
            "alpha":{"id":"alpha","type":"boolean","packageValue":false,"required":false,"public":false,"i18nLabel":"A"},
            "mid":{"id":"mid","type":"int","packageValue":1,"required":false,"public":false,"i18nLabel":"M"}},"success":true}"#;
        let ids: Vec<String> = parse_settings(body)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["zeta", "alpha", "mid"]);
        let arr = br#"{"settings":[{"id":"b"},{"id":"a"}]}"#;
        let ids: Vec<String> = parse_settings(arr)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(parse_settings(br#"{"settings":null}"#).unwrap().is_empty());
        assert!(parse_settings(br#"{}"#).unwrap().is_empty());
    }

    #[test]
    fn ratelimit_reset_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let now = 1_700_000_000_000;
        let mut h = HeaderMap::new();
        assert_eq!(ratelimit_reset_ms(&h, now), None);
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1700000060"));
        assert_eq!(ratelimit_reset_ms(&h, now), Some(1_700_000_060_000));
        h.insert(
            "x-ratelimit-reset",
            HeaderValue::from_static("1700000030000"),
        );
        assert_eq!(ratelimit_reset_ms(&h, now), Some(1_700_000_030_000));
        h.insert("retry-after", HeaderValue::from_static("12"));
        assert_eq!(ratelimit_reset_ms(&h, now), Some(now + 12_000));
        let e = Error::from_response(429, ratelimit_reset_ms(&h, now), b"Too many requests");
        assert!(matches!(e, Error::RateLimited { .. }));
        let e = Error::from_response(
            401,
            None,
            br#"{"status":"error","message":"You must be logged in to do this."}"#,
        );
        assert!(matches!(e, Error::Unauthorized { .. }));
    }

    #[test]
    fn app_ids_in_paths() {
        assert_eq!(app_segment("videoconf-core").unwrap(), "videoconf-core");
        assert_eq!(app_segment("a b").unwrap(), "a%20b");
        for bad in ["", "a/b", "a?b", "a#b", "..", "a\\b"] {
            assert!(app_segment(bad).is_err(), "{bad}");
        }
    }
}
