//! Authentication helpers: 2FA codes, password digests, browser-based login URLs
//! (OAuth legacy/modern, SAML, CAS, "sign in with browser") and deep links.

mod deeplink;
mod oauth;

pub use deeplink::{AuthDeepLink, DeepLink, RoomDeepLink, parse_deep_link};
pub use oauth::{
    BrowserLogin, OAuthOptions, browser_login_url, cas_login, decode_oauth_state, modern_oauth_url,
    oauth_login, oauth_state, saml_login,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::TwoFactorMethod;

/// A second-factor code to send with a login or a protected request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoFactorCode {
    pub code: String,
    pub method: TwoFactorMethod,
}

impl std::fmt::Debug for TwoFactorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwoFactorCode")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl TwoFactorCode {
    /// A TOTP or e-mail code; spaces and dashes (`435-478` in e-mails) are removed.
    pub fn new(code: &str, method: TwoFactorMethod) -> Self {
        Self {
            code: code
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .collect(),
            method,
        }
    }

    pub fn totp(code: &str) -> Self {
        Self::new(code, TwoFactorMethod::Totp)
    }

    pub fn email(code: &str) -> Self {
        Self::new(code, TwoFactorMethod::Email)
    }

    /// Password fallback for `x-2fa-method: password`: the code is the SHA-256 hex
    /// digest of the password.
    pub fn password(password: &str) -> Self {
        Self {
            code: sha256_hex(password),
            method: TwoFactorMethod::Password,
        }
    }
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Meteor password digest `{"digest": sha256_hex(password), "algorithm": "sha-256"}`,
/// accepted by REST `/login` and DDP `login`.
pub fn password_digest(password: &str) -> serde_json::Value {
    serde_json::json!({ "digest": sha256_hex(password), "algorithm": "sha-256" })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn digest() {
        assert_eq!(
            password_digest("alicepass")["digest"],
            sha256_hex("alicepass")
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(password_digest("x")["algorithm"], "sha-256");
    }

    #[test]
    fn code_normalisation() {
        assert_eq!(TwoFactorCode::email(" 435-478 ").code, "435478");
        assert_eq!(TwoFactorCode::password("abc").code, sha256_hex("abc"));
        assert!(!format!("{:?}", TwoFactorCode::totp("123456")).contains("123456"));
    }
}
