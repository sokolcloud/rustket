//! Error type and mapping of Rocket.Chat's error envelopes.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::date::RcDate;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Second factor requested by the server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TwoFactorMethod {
    Totp,
    Email,
    Password,
    #[serde(other)]
    Unknown,
}

impl TwoFactorMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Totp => "totp",
            Self::Email => "email",
            Self::Password => "password",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "totp" => Self::Totp,
            "email" => Self::Email,
            "password" => Self::Password,
            _ => Self::Unknown,
        }
    }
}

/// `details` of a `totp-required` / `totp-invalid` error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwoFactorChallenge {
    pub method: TwoFactorMethod,
    pub available_methods: Vec<TwoFactorMethod>,
    pub email_or_username: Option<String>,
    /// Whether the server just generated (and e-mailed) a new code. `false` with
    /// `code_expires` set means a previously sent code is still valid.
    pub code_generated: Option<bool>,
    pub code_expires: Option<RcDate>,
}

impl TwoFactorChallenge {
    pub fn from_details(details: Option<&Value>) -> Self {
        let get = |k: &str| details.and_then(|d| d.get(k));
        let method = get("method")
            .and_then(Value::as_str)
            .map_or(TwoFactorMethod::Unknown, TwoFactorMethod::parse);
        let available_methods = get("availableMethods")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(TwoFactorMethod::parse)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            method,
            available_methods,
            email_or_username: get("emailOrUsername")
                .and_then(Value::as_str)
                .map(str::to_owned),
            code_generated: get("codeGenerated").and_then(Value::as_bool),
            code_expires: get("codeExpires").and_then(|v| serde_json::from_value(v.clone()).ok()),
        }
    }
}

/// Error object of a DDP method result (also returned by `method.call`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MethodError {
    /// Meteor error code: a string like `error-not-allowed` or a number like 404.
    pub error: Option<Value>,
    pub reason: Option<String>,
    pub message: Option<String>,
    #[serde(rename = "errorType")]
    pub error_type: Option<String>,
    pub details: Option<Value>,
}

impl MethodError {
    pub fn code(&self) -> Option<String> {
        match &self.error {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }
}

impl std::fmt::Display for MethodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = self
            .reason
            .as_deref()
            .or(self.message.as_deref())
            .unwrap_or("method error");
        match self.code() {
            Some(c) => write!(f, "{text} [{c}]"),
            None => f.write_str(text),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[source] reqwest::Error),
    #[error("unexpected HTTP status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("server error ({status}): {}", api_text(.error, .message, .error_type))]
    Api {
        status: u16,
        error: Option<String>,
        error_type: Option<String>,
        message: Option<String>,
        details: Option<Value>,
    },
    #[error("two-factor authentication required ({})", .0.method.as_str())]
    TwoFactorRequired(TwoFactorChallenge),
    #[error("invalid two-factor code ({})", .0.method.as_str())]
    TwoFactorInvalid(TwoFactorChallenge),
    #[error("unauthorized{}", .message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Unauthorized {
        message: Option<String>,
        /// Original error code (`errorType`/`error`), e.g. `error-login-blocked-for-user`.
        code: Option<String>,
    },
    /// A redirect was not followed: to another origin (refused so credentials never
    /// leave the server), or a same-origin loop / too many hops / a redirect that
    /// would need to replay a streamed body.
    #[error("{} to {location}", if *.cross_origin { "refused cross-origin redirect" } else { "redirect not followed (loop, too many hops or non-replayable body)" })]
    Redirect {
        location: String,
        cross_origin: bool,
    },
    /// No progress (upload stalled) or no response within the configured time.
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("rate limited, retry after {retry_after:?}")]
    RateLimited {
        retry_after: Option<Duration>,
        message: Option<String>,
    },
    #[error("method error: {0}")]
    Method(Box<MethodError>),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid URL: {0}")]
    Url(String),
    #[error("TLS error: {0}")]
    Tls(String),
    /// The server certificate was rejected. The presented chain is available from
    /// [`crate::TlsContext::take_rejected_chain`] for `host`.
    #[error("certificate not trusted{}: {reason}", .host.as_deref().map(|h| format!(" for {h}")).unwrap_or_default())]
    Certificate {
        host: Option<String>,
        reason: String,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

fn api_text(
    error: &Option<String>,
    message: &Option<String>,
    error_type: &Option<String>,
) -> String {
    let text = message
        .as_deref()
        .or(error.as_deref())
        .unwrap_or("unknown error");
    match error_type {
        Some(t) if !text.contains(t.as_str()) => format!("{text} [{t}]"),
        _ => text.to_owned(),
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if let Some(tls) = find_rustls_error(&e) {
            return match tls {
                rustls::Error::InvalidCertificate(c) => Error::Certificate {
                    host: e.url().and_then(|u| u.host_str()).map(str::to_owned),
                    reason: format!("{c:?}"),
                },
                other => Error::Tls(other.to_string()),
            };
        }
        Error::Http(e)
    }
}

fn find_rustls_error<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a rustls::Error> {
    if let Some(r) = e.downcast_ref::<rustls::Error>() {
        return Some(r);
    }
    // io::Error::source() skips the wrapped error itself, so descend into it.
    if let Some(io) = e.downcast_ref::<std::io::Error>()
        && let Some(inner) = io.get_ref()
        && let Some(r) = find_rustls_error(inner)
    {
        return Some(r);
    }
    e.source().and_then(find_rustls_error)
}

impl Error {
    /// The Rocket.Chat error code (`error` or `errorType`), if any.
    pub fn api_error_code(&self) -> Option<&str> {
        match self {
            Error::Api {
                error_type: Some(t),
                ..
            } => Some(t),
            Error::Api { error: Some(e), .. } => Some(e),
            _ => None,
        }
    }

    /// A room with that name already exists (`channels.create` /
    /// `groups.create`), possibly archived.
    pub fn is_duplicate_room_name(&self) -> bool {
        matches!(
            self.api_error_code(),
            Some("error-duplicate-channel-name" | "error-archived-duplicate-name")
        )
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Unauthorized { .. })
    }

    pub fn is_certificate_error(&self) -> bool {
        matches!(self, Error::Certificate { .. })
    }

    /// Network-level failure worth retrying (timeout, connection refused, …).
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(e) => {
                find_rustls_error(e).is_none()
                    && (e.is_timeout() || e.is_connect() || e.is_request())
            }
            Error::RateLimited { .. } | Error::Timeout(_) => true,
            Error::Status { status, .. } | Error::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// Maps a DDP/`method.call` error, recognising 2FA and auth failures.
    pub fn from_method_error(e: MethodError) -> Self {
        match e.code().as_deref() {
            Some("totp-required") => {
                Error::TwoFactorRequired(TwoFactorChallenge::from_details(e.details.as_ref()))
            }
            Some("totp-invalid") => {
                Error::TwoFactorInvalid(TwoFactorChallenge::from_details(e.details.as_ref()))
            }
            Some("too-many-requests" | "error-too-many-requests") => Error::RateLimited {
                retry_after: e
                    .details
                    .as_ref()
                    .and_then(|d| d.get("timeToReset"))
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis),
                message: e.reason.clone().or(e.message.clone()),
            },
            _ => Error::Method(Box::new(e)),
        }
    }

    /// Maps an HTTP error response (or a 200 response with `success: false`).
    ///
    /// Handles `{"success":false,"error":..,"errorType":..,"details":..}`,
    /// `{"status":"error","message":..}` and `{"status":"error","error":..,"message":..}`.
    /// `ratelimit_reset_ms` is the `x-ratelimit-reset` header (epoch milliseconds).
    pub fn from_response(status: u16, ratelimit_reset_ms: Option<i64>, body: &[u8]) -> Self {
        let parsed: Option<ErrorBody> = serde_json::from_slice(body).ok();
        let retry_after = || {
            ratelimit_reset_ms.map(|reset| {
                let now = chrono::Utc::now().timestamp_millis();
                Duration::from_millis(u64::try_from(reset.saturating_sub(now)).unwrap_or(0))
            })
        };
        let Some(b) = parsed else {
            let text = String::from_utf8_lossy(body)
                .chars()
                .take(512)
                .collect::<String>();
            return match status {
                401 => Error::Unauthorized {
                    message: (!text.is_empty()).then_some(text),
                    code: None,
                },
                429 => Error::RateLimited {
                    retry_after: retry_after(),
                    message: (!text.is_empty()).then_some(text),
                },
                _ => Error::Status { status, body: text },
            };
        };
        let error = match &b.error {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let code = b.error_type.as_deref().or(error.as_deref());
        let message = b.message.clone().or_else(|| b.reason.clone());
        match code {
            Some("totp-required") => {
                return Error::TwoFactorRequired(TwoFactorChallenge::from_details(
                    b.details.as_ref(),
                ));
            }
            Some("totp-invalid") => {
                return Error::TwoFactorInvalid(TwoFactorChallenge::from_details(
                    b.details.as_ref(),
                ));
            }
            _ => {}
        }
        let too_many = code == Some("error-too-many-requests")
            || error
                .as_deref()
                .is_some_and(|e| e.contains("[error-too-many-requests]"));
        if status == 429 || too_many {
            return Error::RateLimited {
                retry_after: retry_after(),
                message: message.or(error),
            };
        }
        let not_logged_in = |s: &Option<String>| {
            s.as_deref()
                .is_some_and(|m| m.starts_with("You must be logged in"))
        };
        if status == 401 || not_logged_in(&error) || not_logged_in(&message) {
            return Error::Unauthorized {
                code: b.error_type.clone().or_else(|| error.clone()),
                message: message.or(error),
            };
        }
        Error::Api {
            status,
            error,
            error_type: b.error_type,
            message,
            details: b.details,
        }
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default, rename = "errorType")]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    details: Option<Value>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn totp_required_email() {
        let body = br#"{"success":false,"error":"totp-required","status":"error","details":{"method":"email","emailOrUsername":"carol","codeGenerated":false,"codeExpires":"2026-11-07T16:59:48.818Z","availableMethods":["email"]},"message":"TOTP Required"}"#;
        match Error::from_response(401, None, body) {
            Error::TwoFactorRequired(c) => {
                assert_eq!(c.method, TwoFactorMethod::Email);
                assert_eq!(c.available_methods, vec![TwoFactorMethod::Email]);
                assert_eq!(c.email_or_username.as_deref(), Some("carol"));
                assert_eq!(c.code_generated, Some(false));
                assert!(c.code_expires.is_some());
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn totp_invalid() {
        let body = br#"{"success":false,"error":"totp-invalid","status":"error","details":{"method":"totp"},"message":"TOTP Invalid"}"#;
        assert!(matches!(
            Error::from_response(401, None, body),
            Error::TwoFactorInvalid(TwoFactorChallenge {
                method: TwoFactorMethod::Totp,
                ..
            })
        ));
    }

    #[test]
    fn unauthorized_variants() {
        let b1 = br#"{"success":false,"error":"Unauthorized","status":"error","message":"Unauthorized"}"#;
        assert!(Error::from_response(401, None, b1).is_unauthorized());
        let b2 = br#"{"success":false,"error":"You must be logged in to do this.","status":"error","message":"You must be logged in to do this."}"#;
        assert!(Error::from_response(401, None, b2).is_unauthorized());
        assert!(Error::from_response(200, None, b2).is_unauthorized());
        assert!(Error::from_response(401, None, b"").is_unauthorized());
        let b3 = br#"{"success":false,"error":"error-login-blocked-for-user","message":"Blocked"}"#;
        match Error::from_response(401, None, b3) {
            Error::Unauthorized { code, message } => {
                assert_eq!(code.as_deref(), Some("error-login-blocked-for-user"));
                assert_eq!(message.as_deref(), Some("Blocked"));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn api_error() {
        let b = br#"{"success":false,"error":"The required \"roomId\" param provided does not match any channel [error-room-not-found]","errorType":"error-room-not-found"}"#;
        let e = Error::from_response(400, None, b);
        assert_eq!(e.api_error_code(), Some("error-room-not-found"));
        assert!(!e.is_duplicate_room_name());
        // groups.create with a taken name (8.9.1).
        let b = br#"{"success":false,"error":"A channel with name 'x' exists [error-duplicate-channel-name]","errorType":"error-duplicate-channel-name","details":{"function":"RocketChat.getValidRoomName","channel_name":"x"}}"#;
        assert!(Error::from_response(400, None, b).is_duplicate_room_name());
        let b = br#"{"status":"error","message":"Something broke"}"#;
        match Error::from_response(500, None, b) {
            Error::Api { message, error, .. } => {
                assert_eq!(message.as_deref(), Some("Something broke"));
                assert!(error.is_none());
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn rate_limited() {
        let reset = chrono::Utc::now().timestamp_millis() + 30_000;
        let b = br#"{"success":false,"error":"Error, too many requests. Please slow down. You must wait 30 seconds before trying this endpoint again. [error-too-many-requests]"}"#;
        match Error::from_response(429, Some(reset), b) {
            Error::RateLimited { retry_after, .. } => {
                let s = retry_after.unwrap().as_secs();
                assert!((25..=30).contains(&s), "{s}");
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            Error::from_response(429, None, b"<html>"),
            Error::RateLimited {
                retry_after: None,
                ..
            }
        ));
    }

    #[test]
    fn non_json() {
        assert!(matches!(
            Error::from_response(502, None, b"Bad gateway"),
            Error::Status { status: 502, .. }
        ));
    }

    #[test]
    fn method_errors() {
        let e: MethodError = serde_json::from_str(
            r#"{"isClientSafe":true,"error":404,"reason":"Method 'x' not found","message":"Method 'x' not found [404]","errorType":"Meteor.Error"}"#,
        )
        .unwrap();
        assert_eq!(e.code().as_deref(), Some("404"));
        assert!(matches!(Error::from_method_error(e), Error::Method(_)));
        let e: MethodError = serde_json::from_str(
            r#"{"error":"totp-required","reason":"TOTP Required","details":{"method":"password","availableMethods":["password"]}}"#,
        )
        .unwrap();
        assert!(matches!(
            Error::from_method_error(e),
            Error::TwoFactorRequired(TwoFactorChallenge {
                method: TwoFactorMethod::Password,
                ..
            })
        ));
        let e: MethodError = serde_json::from_str(
            r#"{"error":"too-many-requests","reason":"Error, too many requests. Please slow down.","details":{"timeToReset":4321}}"#,
        )
        .unwrap();
        assert!(matches!(
            Error::from_method_error(e),
            Error::RateLimited { retry_after: Some(d), .. } if d.as_millis() == 4321
        ));
    }
}
