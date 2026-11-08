//! REST client (`/api/v1`) with Rocket.Chat envelope handling.
//!
//! Endpoint groups live in submodules as `impl RestClient` blocks; add new endpoints
//! next to related ones using [`RestClient::get`], [`RestClient::post`] and friends.

mod apps;
mod auth;
mod e2e;
mod files;
mod messages;
mod misc;
mod rooms;
mod users;

pub use apps::UI_INTERACTION_TIMEOUT;
pub use auth::LoginRequest;
pub use auth::{LdapLogin, LoginResult};
/// Byte buffer type of `UploadSource` (re-exported so callers need no `bytes` dependency).
pub use bytes::Bytes;
pub use e2e::{
    ENCRYPTED_UPLOAD_MIME, EncryptedMediaConfirm, MyKeys, SetKeyIdOutcome, SuggestedKey,
    SuggestedOldKey, UserPublicKeyEntry,
};
pub use files::{Download, ProgressFn, UploadRequest, UploadSource};
pub use messages::{HistoryCursor, HistoryOptions};
pub use rooms::{CreateRoom, RoomMembersOptions};
pub use users::UserLookup;

use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::{Method, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::auth::TwoFactorCode;
use crate::error::{Error, MethodError, Result};
use crate::server_url::ServerUrl;
use crate::tls::TlsContext;

/// `X-User-Id` / `X-Auth-Token` pair (login token or personal access token).
#[derive(Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Credentials {
    pub user_id: String,
    pub auth_token: String,
}

impl Credentials {
    pub fn new(user_id: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            auth_token: auth_token.into(),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user_id", &self.user_id)
            .field("auth_token", &"<redacted>")
            .finish()
    }
}

/// Cheap-to-clone REST client for one server. Clones share credentials.
#[derive(Clone)]
pub struct RestClient {
    inner: Arc<Inner>,
}

struct Inner {
    /// API calls: same-origin redirects only, total timeout per request.
    http: reqwest::Client,
    /// Downloads from the server: same-origin redirects only, read timeout.
    download_http: reqwest::Client,
    /// Uploads: same-origin redirects only, no read timeout (the response only
    /// starts after the whole body is sent).
    upload_http: reqwest::Client,
    /// Downloads redirected to foreign storage: never sees credentials.
    foreign_http: reqwest::Client,
    server: ServerUrl,
    config: RestConfig,
    credentials: RwLock<Option<Credentials>>,
}

/// Timeouts of the REST client.
#[derive(Debug, Clone, Copy)]
pub struct RestConfig {
    /// Total time limit of one API call (not applied to uploads/downloads).
    pub api_timeout: Duration,
    /// Downloads: maximum wait for the response and between body reads (no total
    /// limit). Not used for API calls (they have `api_timeout`) or uploads.
    pub read_timeout: Duration,
    /// Uploads: abort when no body bytes could be sent for this long.
    pub upload_stall_timeout: Duration,
    /// Uploads: maximum wait for the response after the whole body was sent.
    pub upload_response_timeout: Duration,
    /// Maximum redirect hops followed for downloads from foreign storage.
    pub max_foreign_redirects: usize,
}

impl Default for RestConfig {
    fn default() -> Self {
        Self {
            api_timeout: Duration::from_secs(60),
            read_timeout: Duration::from_secs(30),
            upload_stall_timeout: Duration::from_secs(60),
            upload_response_timeout: Duration::from_secs(120),
            max_foreign_redirects: 5,
        }
    }
}

/// Same scheme, host and effective port.
pub(crate) fn same_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(|h| h.trim_end_matches('.'))
            == b.host_str().map(|h| h.trim_end_matches('.'))
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Follows redirects only within the origin of the original request.
fn same_origin_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let Some(first) = attempt.previous().first() else {
            return attempt.stop();
        };
        if attempt.previous().len() > 10 || !same_origin(first, attempt.url()) {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

/// Follows redirects between foreign hosts but never from https to http.
fn no_downgrade_policy(max: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        let downgrade = attempt.url().scheme() != "https"
            && attempt.previous().iter().any(|u| u.scheme() == "https");
        if attempt.previous().len() > max || downgrade {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

impl fmt::Debug for RestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestClient")
            .field("server", &self.inner.server)
            .field("credentials", &self.credentials())
            .finish()
    }
}

/// Query string builder: `Query::new().push("roomId", rid).opt("count", count)`.
#[derive(Debug, Clone, Default)]
pub struct Query(Vec<(String, String)>);

impl Query {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(mut self, key: &str, value: impl ToString) -> Self {
        self.0.push((key.to_owned(), value.to_string()));
        self
    }

    pub fn opt<V: ToString>(self, key: &str, value: Option<V>) -> Self {
        match value {
            Some(v) => self.push(key, v),
            None => self,
        }
    }

    /// Appends the query to `path` (which may already contain `?`).
    pub fn apply(&self, path: &str) -> String {
        if self.0.is_empty() {
            return path.to_owned();
        }
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &self.0 {
            ser.append_pair(k, v);
        }
        let sep = if path.contains('?') { '&' } else { '?' };
        format!("{path}{sep}{}", ser.finish())
    }
}

impl RestClient {
    /// Creates a client using the shared TLS context and default timeouts.
    pub fn new(server: ServerUrl, tls: &TlsContext) -> Result<Self> {
        Self::with_config(server, tls, RestConfig::default())
    }

    pub fn with_config(server: ServerUrl, tls: &TlsContext, config: RestConfig) -> Result<Self> {
        // API calls: only the per-request total timeout (a read timeout would also
        // cap the time to the first byte below `api_timeout`).
        let http = tls
            .reqwest_builder()
            .redirect(same_origin_policy())
            .build()?;
        let download_http = tls
            .reqwest_builder()
            .redirect(same_origin_policy())
            .read_timeout(config.read_timeout)
            .build()?;
        let upload_http = tls
            .reqwest_builder()
            .redirect(same_origin_policy())
            .build()?;
        let foreign_http = tls
            .reqwest_builder()
            .redirect(no_downgrade_policy(config.max_foreign_redirects))
            .read_timeout(config.read_timeout)
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                upload_http,
                download_http,
                foreign_http,
                server,
                config,
                credentials: RwLock::new(None),
            }),
        })
    }

    pub fn config(&self) -> &RestConfig {
        &self.inner.config
    }

    pub(crate) fn download_http(&self) -> &reqwest::Client {
        &self.inner.download_http
    }

    pub(crate) fn upload_http(&self) -> &reqwest::Client {
        &self.inner.upload_http
    }

    pub(crate) fn foreign_http(&self) -> &reqwest::Client {
        &self.inner.foreign_http
    }

    pub fn server(&self) -> &ServerUrl {
        &self.inner.server
    }

    /// The client used for API calls (same-origin redirects only).
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub fn credentials(&self) -> Option<Credentials> {
        self.inner.credentials.read().ok().and_then(|g| g.clone())
    }

    pub fn set_credentials(&self, credentials: Option<Credentials>) {
        if let Ok(mut g) = self.inner.credentials.write() {
            *g = credentials;
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.credentials().is_some()
    }

    pub fn user_id(&self) -> Option<String> {
        self.credentials().map(|c| c.user_id)
    }

    /// API request builder with auth headers, optional 2FA headers and the total
    /// API timeout.
    pub fn request(
        &self,
        method: Method,
        url: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> RequestBuilder {
        self.authed(&self.inner.http, method, url, two_factor)
            .timeout(self.inner.config.api_timeout)
    }

    /// Adds auth/2FA headers to a request on `client` (no total timeout).
    pub(crate) fn authed(
        &self,
        client: &reqwest::Client,
        method: Method,
        url: &str,
        two_factor: Option<&TwoFactorCode>,
    ) -> RequestBuilder {
        let mut rb = client.request(method, url);
        if let Some(c) = self.credentials() {
            rb = rb
                .header("X-User-Id", c.user_id)
                .header("X-Auth-Token", c.auth_token);
        }
        if let Some(tf) = two_factor {
            rb = rb
                .header("x-2fa-code", tf.code.as_str())
                .header("x-2fa-method", tf.method.as_str());
        }
        rb
    }

    /// Sends a request and unwraps the Rocket.Chat envelope into `T`.
    pub async fn send<T: DeserializeOwned>(&self, rb: RequestBuilder) -> Result<T> {
        let value = self.send_value(rb).await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn send_value(&self, rb: RequestBuilder) -> Result<Value> {
        let resp = rb.send().await?;
        let status = resp.status().as_u16();
        let reset = resp
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<i64>().ok());
        if resp.status().is_redirection() {
            return Err(redirect_error(&resp));
        }
        let body = resp.bytes().await?;
        if !(200..300).contains(&status) {
            tracing::debug!(status, "REST error response");
            return Err(Error::from_response(status, reset, &body));
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(Default::default()));
        }
        let value: Value = serde_json::from_slice(&body).map_err(|e| {
            Error::Protocol(format!(
                "invalid JSON ({e}): {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(200)
                    .collect::<String>()
            ))
        })?;
        let failed = value.get("success").and_then(Value::as_bool) == Some(false)
            || value.get("status").and_then(Value::as_str) == Some("error");
        if failed {
            return Err(Error::from_response(status, reset, &body));
        }
        Ok(value)
    }

    /// `GET /api/v1/{path}{query}`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str, query: &Query) -> Result<T> {
        let url = self.inner.server.api(&query.apply(path));
        self.send(self.request(Method::GET, &url, None)).await
    }

    /// `POST /api/v1/{path}` with a JSON body.
    pub async fn post<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.post_with(path, body, None).await
    }

    /// `POST` with optional 2FA headers (for endpoints that demand re-authentication).
    pub async fn post_with<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
        two_factor: Option<&TwoFactorCode>,
    ) -> Result<T> {
        let url = self.inner.server.api(path);
        self.send(self.request(Method::POST, &url, two_factor).json(body))
            .await
    }

    /// `DELETE /api/v1/{path}{query}`.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str, query: &Query) -> Result<T> {
        let url = self.inner.server.api(&query.apply(path));
        self.send(self.request(Method::DELETE, &url, None)).await
    }

    /// `POST` whose response is only `{success: true}`.
    pub async fn post_ok<B: Serialize + ?Sized>(&self, path: &str, body: &B) -> Result<()> {
        let _: Value = self.post(path, body).await?;
        Ok(())
    }

    /// Calls a Meteor method over REST (`POST /api/v1/method.call/{name}`), the same
    /// as a DDP `method` frame. Use `anonymous` for `method.callAnon`.
    pub async fn method_call_raw(
        &self,
        name: &str,
        params: Value,
        anonymous: bool,
    ) -> Result<Value> {
        let frame = serde_json::json!({
            "msg": "method",
            "id": crate::ids::random_id_len(8),
            "method": name,
            "params": params,
        });
        let endpoint = if anonymous {
            "method.callAnon"
        } else {
            "method.call"
        };
        let url = self.inner.server.api(&format!(
            "{endpoint}/{}",
            crate::server_url::encode_segment(name)
        ));
        let body = serde_json::json!({ "message": frame.to_string() });
        let resp = self
            .request(Method::POST, &url, None)
            .json(&body)
            .send()
            .await?;
        if resp.status().is_redirection() {
            return Err(redirect_error(&resp));
        }
        let status = resp.status().as_u16();
        let ok_status = resp.status().is_success();
        let bytes = resp.bytes().await?;
        let outer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        // The DDP frame is stringified into `message`; anything else is an HTTP error.
        let inner: Option<Value> = outer
            .get("message")
            .and_then(Value::as_str)
            .and_then(|m| serde_json::from_str::<Value>(m).ok())
            .filter(Value::is_object);
        let Some(mut inner) = inner else {
            return Err(Error::from_response(
                if ok_status { 500 } else { status },
                None,
                &bytes,
            ));
        };
        if let Some(err) = inner.get_mut("error").map(Value::take) {
            let me: MethodError = serde_json::from_value(err).unwrap_or_default();
            return Err(Error::from_method_error(me));
        }
        if !ok_status {
            return Err(Error::from_response(status, None, &bytes));
        }
        Ok(inner
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    /// Typed [`Self::method_call_raw`] (authenticated).
    pub async fn method_call<T: DeserializeOwned>(&self, name: &str, params: Value) -> Result<T> {
        let v = self.method_call_raw(name, params, false).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Typed `method.callAnon`.
    pub async fn method_call_anon<T: DeserializeOwned>(
        &self,
        name: &str,
        params: Value,
    ) -> Result<T> {
        let v = self.method_call_raw(name, params, true).await?;
        Ok(serde_json::from_value(v)?)
    }
}

/// Extracts `value[key]` as `T` (for responses like `{"message": {...}}`).
pub(crate) fn field<T: DeserializeOwned>(mut value: Value, key: &str) -> Result<T> {
    let v = value
        .get_mut(key)
        .map(Value::take)
        .ok_or_else(|| Error::Protocol(format!("missing `{key}` in response")))?;
    Ok(serde_json::from_value(v)?)
}

/// Extracts a paginated list stored under `key`.
pub(crate) fn paginated<T: DeserializeOwned>(
    mut value: Value,
    key: &str,
) -> Result<crate::models::Paginated<T>> {
    let num = |k: &str| value.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (count, offset, total) = (num("count"), num("offset"), num("total"));
    let items = value
        .get_mut(key)
        .map(Value::take)
        .ok_or_else(|| Error::Protocol(format!("missing `{key}` in response")))?;
    let c = crate::models::lenient::counted(items);
    Ok(crate::models::Paginated {
        items: c.items,
        count,
        offset,
        total,
        raw_len: c.raw_len,
        dropped: c.dropped,
    })
}

/// Error for a 3xx the client did not follow: cross-origin (refused on purpose), or
/// same-origin but a loop / too many hops / a non-replayable body.
pub(crate) fn redirect_error(resp: &reqwest::Response) -> Error {
    let raw = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let resolved = resp.url().join(raw).ok();
    let cross_origin = resolved
        .as_ref()
        .is_none_or(|u| !same_origin(u, resp.url()));
    let location = resolved.map_or_else(|| raw.to_owned(), |u| u.to_string());
    if cross_origin {
        tracing::warn!(%location, "refusing cross-origin redirect");
    }
    Error::Redirect {
        location,
        cross_origin,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn query_building() {
        let q = Query::new()
            .push("roomId", "a b&c")
            .opt("count", Some(5))
            .opt::<u32>("offset", None);
        assert_eq!(q.apply("x"), "x?roomId=a+b%26c&count=5");
        assert_eq!(Query::new().apply("x"), "x");
        assert_eq!(Query::new().push("a", 1).apply("x?b=2"), "x?b=2&a=1");
    }

    #[test]
    fn credentials_debug_redacts() {
        let c = Credentials::new("u", "secret-token");
        assert!(!format!("{c:?}").contains("secret-token"));
    }
}
