//! Cloneable error wrapper for messages and user-facing error texts.

use std::fmt;
use std::sync::Arc;

use rust_i18n::t;

/// A `rocketchat::Error` that can travel inside a (cloneable) iced message.
#[derive(Clone)]
pub struct Failure(pub Arc<rocketchat::Error>);

impl Failure {
    pub fn error(&self) -> &rocketchat::Error {
        &self.0
    }

    /// Localised, user-facing text.
    pub fn user_text(&self) -> String {
        user_text(&self.0)
    }
}

impl From<rocketchat::Error> for Failure {
    fn from(e: rocketchat::Error) -> Self {
        Self(Arc::new(e))
    }
}

impl fmt::Debug for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

pub type Res<T> = Result<T, Failure>;

/// Maps a protocol error to a short localised sentence.
pub fn user_text(e: &rocketchat::Error) -> String {
    use rocketchat::Error as E;
    match e {
        E::Unauthorized { code, .. } => match code.as_deref() {
            Some("error-login-blocked-for-user") | Some("error-login-blocked-for-ip") => {
                t!("errors.login_blocked").into_owned()
            }
            _ => t!("errors.unauthorized").into_owned(),
        },
        E::TwoFactorInvalid(_) => t!("errors.two_factor_invalid").into_owned(),
        E::TwoFactorRequired(_) => t!("errors.two_factor_required").into_owned(),
        E::Certificate { host, .. } => {
            t!("errors.certificate", host = host.as_deref().unwrap_or("?")).into_owned()
        }
        E::Tls(m) => t!("errors.tls", reason = m).into_owned(),
        E::Timeout(_) => t!("errors.timeout").into_owned(),
        E::RateLimited { .. } => t!("errors.rate_limited").into_owned(),
        E::Url(m) => t!("errors.bad_url", reason = m).into_owned(),
        E::Http(_) if e.is_transient() => t!("errors.network").into_owned(),
        E::Status { status: 404, .. } => t!("errors.not_rocketchat").into_owned(),
        E::Api { message, error, .. } => message
            .clone()
            .or_else(|| error.clone())
            .unwrap_or_else(|| e.to_string()),
        _ => e.to_string(),
    }
}
