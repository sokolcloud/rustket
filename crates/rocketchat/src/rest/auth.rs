//! Login, logout, `me`, 2FA helpers.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Credentials, Query, RestClient, field};
use crate::auth::{TwoFactorCode, password_digest};
use crate::error::Result;
use crate::models::{LoginServiceConfig, ServerInfo, User};

/// `data` of a successful `POST /api/v1/login`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    pub user_id: String,
    pub auth_token: String,
    #[serde(default, deserialize_with = "crate::models::lenient::opt")]
    pub me: Option<User>,
}

impl fmt::Debug for LoginResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginResult")
            .field("user_id", &self.user_id)
            .field("auth_token", &"<redacted>")
            .field("me", &self.me.as_ref().map(|m| &m.username))
            .finish()
    }
}

impl LoginResult {
    pub fn credentials(&self) -> Credentials {
        Credentials::new(&self.user_id, &self.auth_token)
    }
}

/// LDAP login parameters.
#[derive(Clone)]
pub struct LdapLogin {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for LdapLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LdapLogin")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Any kind of `POST /api/v1/login`.
#[derive(Clone)]
pub enum LoginRequest {
    /// Username or e-mail; the password is sent as a SHA-256 digest.
    Password {
        user: String,
        password: String,
    },
    Ldap(LdapLogin),
    Resume {
        token: String,
    },
    /// Legacy OAuth deep-link credentials.
    OAuth {
        credential_token: String,
        credential_secret: String,
    },
    Saml {
        credential_token: String,
    },
    Cas {
        credential_token: String,
    },
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Password { user, .. } => return write!(f, "LoginRequest::Password({user})"),
            Self::Ldap(l) => return write!(f, "LoginRequest::Ldap({})", l.username),
            Self::Resume { .. } => "Resume",
            Self::OAuth { .. } => "OAuth",
            Self::Saml { .. } => "Saml",
            Self::Cas { .. } => "Cas",
        };
        write!(f, "LoginRequest::{kind}(<redacted>)")
    }
}

impl LoginRequest {
    /// The login body without a second factor.
    pub fn body(&self) -> Value {
        match self {
            Self::Password { user, password } => {
                json!({ "user": user, "password": password_digest(password) })
            }
            Self::Ldap(l) => json!({
                "ldap": true,
                "username": l.username,
                "ldapPass": l.password,
                "ldapOptions": {},
            }),
            Self::Resume { token } => json!({ "resume": token }),
            Self::OAuth {
                credential_token,
                credential_secret,
            } => json!({
                "oauth": { "credentialToken": credential_token, "credentialSecret": credential_secret }
            }),
            Self::Saml { credential_token } => {
                json!({ "saml": true, "credentialToken": credential_token })
            }
            Self::Cas { credential_token } => {
                json!({ "cas": { "credentialToken": credential_token } })
            }
        }
    }

    /// The body with a 2FA code: top-level `code` for password logins, otherwise
    /// `{"totp": {"login": <body>, "code": ..}}` (the only form the server honours
    /// for LDAP/OAuth/SAML/CAS).
    pub fn body_with_code(&self, code: Option<&TwoFactorCode>) -> Value {
        let mut body = self.body();
        let Some(c) = code else { return body };
        match self {
            Self::Password { .. } => {
                body["code"] = Value::String(c.code.clone());
                body
            }
            _ => json!({ "totp": { "login": body, "code": c.code } }),
        }
    }
}

#[derive(Deserialize)]
struct LoginEnvelope {
    data: LoginResult,
}

impl RestClient {
    /// `GET /api/info`.
    pub async fn info(&self) -> Result<ServerInfo> {
        let url = self.server().api_root("info");
        self.send(self.request(reqwest::Method::GET, &url, None))
            .await
    }

    /// `GET /api/v1/settings.oauth` — enabled login services (public).
    pub async fn settings_oauth(&self) -> Result<Vec<LoginServiceConfig>> {
        let v: Value = self.get("settings.oauth", &Query::new()).await?;
        field(v, "services")
    }

    /// `GET /api/v1/service.configurations` (public; same entries as `settings.oauth`).
    pub async fn service_configurations(&self) -> Result<Vec<LoginServiceConfig>> {
        let v: Value = self.get("service.configurations", &Query::new()).await?;
        field(v, "configurations")
    }

    /// Raw `POST /api/v1/login`. On success the returned credentials are stored in
    /// the client.
    pub async fn login_raw(&self, body: Value) -> Result<LoginResult> {
        let env: LoginEnvelope = self.post("login", &body).await?;
        self.set_credentials(Some(env.data.credentials()));
        Ok(env.data)
    }

    /// Logs in. On [`crate::Error::TwoFactorRequired`] repeat the same request with
    /// the code (for OAuth the same `credentialToken`/`credentialSecret` stay valid).
    pub async fn login(
        &self,
        req: &LoginRequest,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login_raw(req.body_with_code(code)).await
    }

    /// Username/e-mail + password login (password sent as a SHA-256 digest).
    pub async fn login_password(
        &self,
        user: &str,
        password: &str,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login(
            &LoginRequest::Password {
                user: user.to_owned(),
                password: password.to_owned(),
            },
            code,
        )
        .await
    }

    /// LDAP login (plain password, as LDAP needs it).
    pub async fn login_ldap(
        &self,
        ldap: &LdapLogin,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login(&LoginRequest::Ldap(ldap.clone()), code).await
    }

    /// Resume a session with a stored login token (8.9 returns the same token;
    /// persist whatever comes back).
    pub async fn login_resume(&self, token: &str) -> Result<LoginResult> {
        self.login(
            &LoginRequest::Resume {
                token: token.to_owned(),
            },
            None,
        )
        .await
    }

    /// Finish the legacy OAuth flow with the deep-link credentials.
    pub async fn login_oauth(
        &self,
        credential_token: &str,
        credential_secret: &str,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login(
            &LoginRequest::OAuth {
                credential_token: credential_token.to_owned(),
                credential_secret: credential_secret.to_owned(),
            },
            code,
        )
        .await
    }

    /// Finish a SAML login (`credentialToken` from the authorize URL / deep link).
    pub async fn login_saml(
        &self,
        credential_token: &str,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login(
            &LoginRequest::Saml {
                credential_token: credential_token.to_owned(),
            },
            code,
        )
        .await
    }

    /// Finish a CAS login.
    pub async fn login_cas(
        &self,
        credential_token: &str,
        code: Option<&TwoFactorCode>,
    ) -> Result<LoginResult> {
        self.login(
            &LoginRequest::Cas {
                credential_token: credential_token.to_owned(),
            },
            code,
        )
        .await
    }

    /// `POST /api/v1/logout`; clears the stored credentials even if the call fails.
    pub async fn logout(&self) -> Result<()> {
        let r = self.post_ok("logout", &json!({})).await;
        self.set_credentials(None);
        r
    }

    /// `GET /api/v1/me`.
    pub async fn me(&self) -> Result<User> {
        self.get("me", &Query::new()).await
    }

    /// `POST /api/v1/users.2fa.sendEmailCode` (public) — e-mails a new 2FA code.
    pub async fn send_email_2fa_code(&self, email_or_username: &str) -> Result<()> {
        self.post_ok(
            "users.2fa.sendEmailCode",
            &json!({ "emailOrUsername": email_or_username }),
        )
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bodies_with_code() {
        let code = TwoFactorCode::email("123-456");
        let p = LoginRequest::Password {
            user: "u".into(),
            password: "pw".into(),
        };
        let b = p.body_with_code(Some(&code));
        assert_eq!(b["code"], "123456");
        assert_eq!(b["password"]["algorithm"], "sha-256");
        let o = LoginRequest::OAuth {
            credential_token: "t".into(),
            credential_secret: "s".into(),
        };
        assert_eq!(
            o.body_with_code(Some(&code)),
            json!({"totp": {"login": {"oauth": {"credentialToken": "t", "credentialSecret": "s"}}, "code": "123456"}})
        );
        assert_eq!(o.body_with_code(None), o.body());
        let l = LoginRequest::Ldap(LdapLogin {
            username: "u".into(),
            password: "SECRET".into(),
        });
        assert_eq!(l.body_with_code(Some(&code))["totp"]["login"]["ldap"], true);
        assert!(!format!("{l:?} {o:?} {p:?}").contains("SECRET"));
        assert!(!format!("{o:?}").contains("\"s\""));
    }
}
