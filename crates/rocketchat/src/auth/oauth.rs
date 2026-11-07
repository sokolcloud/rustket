//! URLs for browser-based logins.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::ids::random_secret;
use crate::models::{LoginServiceConfig, LoginServiceKind};
use crate::server_url::{ServerUrl, encode_query};

/// Deep link the server redirects to at the end of the legacy OAuth flow.
pub const AUTH_REDIRECT_URL: &str = "rocketchat://auth";

/// A URL to open in the browser plus the token identifying the pending login.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowserLogin {
    pub url: String,
    /// `credentialToken` to match the returning deep link / to poll with.
    /// `None` for flows that return a resume token instead.
    pub credential_token: Option<String>,
}

impl std::fmt::Debug for BrowserLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The URL embeds the credential token (state / path), so it is redacted too.
        f.debug_struct("BrowserLogin").finish_non_exhaustive()
    }
}

/// Extra inputs some providers need.
#[derive(Debug, Clone, Default)]
pub struct OAuthOptions {
    /// Base URL of a self-hosted GitLab (public setting `API_Gitlab_URL`).
    pub gitlab_url: Option<String>,
}

/// `state` for the legacy flow: unpadded base64url of
/// `{"loginStyle":"redirect","credentialToken":..,"isCordova":true,"redirectUrl":"rocketchat://auth"}`
/// (js-base64 `encodeURI`, as the React Native client does).
pub fn oauth_state(credential_token: &str) -> String {
    let obj = json!({
        "loginStyle": "redirect",
        "credentialToken": credential_token,
        "isCordova": true,
        "redirectUrl": AUTH_REDIRECT_URL,
    });
    URL_SAFE_NO_PAD.encode(obj.to_string())
}

/// Decodes a `state` (base64url or standard, padded or not).
pub fn decode_oauth_state(state: &str) -> Option<Value> {
    let trimmed = state.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .ok()
        .or_else(|| STANDARD.decode(state).ok())?;
    serde_json::from_slice(&bytes).ok()
}

fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode_query(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Builds the provider authorize URL for the legacy Meteor OAuth flow
/// (`Accounts_OAuth_Use_Modern_Flow = false`). After the provider login the server
/// redirects to `rocketchat://auth?host=..&type=oauth&credentialToken=..&credentialSecret=..`;
/// finish with [`crate::RestClient::login_oauth`].
pub fn oauth_login(
    service: &LoginServiceConfig,
    server: &ServerUrl,
    opts: &OAuthOptions,
) -> Result<BrowserLogin> {
    let name = service.service_name();
    if matches!(
        service.kind(),
        LoginServiceKind::Saml | LoginServiceKind::Cas
    ) {
        return Err(Error::Protocol(format!("{name} is not an OAuth service")));
    }
    let token = random_secret(43);
    let state = oauth_state(&token);
    let client_id = || {
        service
            .client_id()
            .ok_or_else(|| Error::Protocol(format!("login service {name} has no client id")))
    };
    let redirect_close = format!("{}/_oauth/{name}?close", server.as_str());
    let url = match (service.kind(), name) {
        (LoginServiceKind::CustomOAuth, _) => {
            let server_url = service
                .server_url
                .as_deref()
                .unwrap_or_default()
                .trim_end_matches('/');
            let path = service
                .authorize_path
                .as_deref()
                .unwrap_or("/oauth/authorize");
            let base = if path.starts_with("http://") || path.starts_with("https://") {
                path.to_owned()
            } else if path.starts_with('/') {
                format!("{server_url}{path}")
            } else {
                format!("{server_url}/{path}")
            };
            let sep = if base.contains('?') { '&' } else { '?' };
            let redirect = format!("{}/_oauth/{name}", server.as_str());
            format!(
                "{base}{sep}{}",
                query(&[
                    ("client_id", client_id()?),
                    ("redirect_uri", &redirect),
                    ("response_type", "code"),
                    ("state", &state),
                    ("scope", service.scope.as_deref().unwrap_or_default()),
                ])
            )
        }
        (_, "google") => format!(
            "https://accounts.google.com/o/oauth2/auth?{}",
            query(&[
                ("client_id", client_id()?),
                ("redirect_uri", &redirect_close),
                ("scope", "profile email"),
                ("state", &state),
                ("response_type", "code"),
                ("prompt", "select_account"),
            ])
        ),
        (_, "github") => format!(
            "https://github.com/login/oauth/authorize?{}",
            query(&[
                ("client_id", client_id()?),
                ("redirect_uri", &redirect_close),
                ("scope", "user:email"),
                ("state", &state),
            ])
        ),
        (_, "gitlab") => {
            let base = opts
                .gitlab_url
                .as_deref()
                .map(|u| u.trim().trim_end_matches('/'))
                .filter(|u| !u.is_empty())
                .unwrap_or("https://gitlab.com");
            format!(
                "{base}/oauth/authorize?{}",
                query(&[
                    ("client_id", client_id()?),
                    ("redirect_uri", &redirect_close),
                    ("scope", "read_user"),
                    ("state", &state),
                    ("response_type", "code"),
                ])
            )
        }
        (_, "facebook") => format!(
            "https://www.facebook.com/v2.9/dialog/oauth?{}",
            query(&[
                ("client_id", client_id()?),
                ("redirect_uri", &redirect_close),
                ("scope", "email"),
                ("state", &state),
            ])
        ),
        (_, "linkedin") => format!(
            "https://www.linkedin.com/oauth/v2/authorization?{}",
            query(&[
                ("client_id", client_id()?),
                ("redirect_uri", &redirect_close),
                ("scope", "r_liteprofile,r_emailaddress"),
                ("state", &state),
                ("response_type", "code"),
            ])
        ),
        (_, "meteor-developer") => format!(
            "https://www.meteor.com/oauth2/authorize?{}",
            query(&[
                ("client_id", client_id()?),
                (
                    "redirect_uri",
                    &format!("{}/_oauth/meteor-developer", server.as_str())
                ),
                ("state", &state),
                ("response_type", "code"),
            ])
        ),
        (_, "twitter") => format!(
            "{}/_oauth/twitter/?{}",
            server.as_str(),
            query(&[("requestTokenAndRedirect", "true"), ("state", &state)])
        ),
        (_, "wordpress") => {
            let base = service
                .server_url
                .as_deref()
                .unwrap_or("https://public-api.wordpress.com")
                .trim_end_matches('/');
            format!(
                "{base}/oauth/authorize?{}",
                query(&[
                    ("client_id", client_id()?),
                    ("redirect_uri", &redirect_close),
                    ("scope", "openid"),
                    ("state", &state),
                    ("response_type", "code"),
                ])
            )
        }
        (_, other) => {
            return Err(Error::Protocol(format!(
                "login service {other} is not supported in the legacy flow; use sign in with browser"
            )));
        }
    };
    Ok(BrowserLogin {
        url,
        credential_token: Some(token),
    })
}

/// Modern passport flow (`Accounts_OAuth_Use_Modern_Flow = true`): ends with
/// `rocketchat://auth?host=..&token=..&userId=..` (2FA handled by the web pages).
pub fn modern_oauth_url(server: &ServerUrl, service: &str) -> String {
    format!(
        "{}/oauth/{}?loginClient=desktop",
        server.as_str(),
        crate::server_url::encode_segment(service)
    )
}

/// SAML: open `{server}/_saml/authorize/{provider}/{credentialToken}?loginClient=desktop`,
/// then `login_saml(credentialToken)` (after the `type=saml` deep link or by polling).
pub fn saml_login(server: &ServerUrl, provider: &str) -> BrowserLogin {
    let token = random_secret(43);
    BrowserLogin {
        url: format!(
            "{}/_saml/authorize/{}/{}?loginClient=desktop",
            server.as_str(),
            crate::server_url::encode_segment(provider),
            token
        ),
        credential_token: Some(token),
    }
}

/// CAS: open `{cas_login_url}?service={server}/_cas/{credentialToken}` and poll
/// `login_cas(credentialToken)`.
pub fn cas_login(server: &ServerUrl, cas_login_url: &str) -> BrowserLogin {
    let token = random_secret(43);
    let service = format!("{}/_cas/{token}", server.as_str());
    let sep = if cas_login_url.contains('?') {
        '&'
    } else {
        '?'
    };
    BrowserLogin {
        url: format!("{cas_login_url}{sep}service={}", encode_query(&service)),
        credential_token: Some(token),
    }
}

/// Universal fallback: the web app logs in by any method and then redirects to
/// `rocketchat://auth?host=..&token=..&userId=..`.
pub fn browser_login_url(server: &ServerUrl) -> String {
    format!("{}/home?loginClient=desktop", server.as_str())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn server() -> ServerUrl {
        ServerUrl::parse("https://chat.example.com").unwrap()
    }

    fn param(url: &str, key: &str) -> Option<String> {
        let u = url::Url::parse(url).unwrap();
        u.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    fn svc(json: &str) -> LoginServiceConfig {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn state_round_trip() {
        let s = oauth_state("tok");
        assert!(!s.contains('=') && !s.contains('+') && !s.contains('/'));
        let v = decode_oauth_state(&s).unwrap();
        assert_eq!(
            v,
            json!({"loginStyle":"redirect","credentialToken":"tok","isCordova":true,"redirectUrl":"rocketchat://auth"})
        );
        let padded = STANDARD.encode(br#"{"a":1}"#);
        assert_eq!(decode_oauth_state(&padded).unwrap(), json!({"a":1}));
    }

    #[test]
    fn custom_oauth() {
        let s = svc(
            r#"{"service":"keycloak","clientId":"rocketchat","custom":true,"serverURL":"http://localhost:8081/realms/rustket/protocol/openid-connect","authorizePath":"/auth","scope":"openid email profile"}"#,
        );
        let srv = ServerUrl::parse("http://localhost:3000").unwrap();
        let l = oauth_login(&s, &srv, &OAuthOptions::default()).unwrap();
        assert!(
            l.url
                .starts_with("http://localhost:8081/realms/rustket/protocol/openid-connect/auth?")
        );
        assert_eq!(param(&l.url, "client_id").unwrap(), "rocketchat");
        assert_eq!(
            param(&l.url, "redirect_uri").unwrap(),
            "http://localhost:3000/_oauth/keycloak"
        );
        assert_eq!(param(&l.url, "response_type").unwrap(), "code");
        assert_eq!(param(&l.url, "scope").unwrap(), "openid email profile");
        let state = decode_oauth_state(&param(&l.url, "state").unwrap()).unwrap();
        let token = l.credential_token.unwrap();
        assert_eq!(token.len(), 43);
        assert_eq!(state["credentialToken"], token.as_str());
        assert_eq!(state["redirectUrl"], "rocketchat://auth");
        assert_eq!(state["loginStyle"], "redirect");
        assert_eq!(state["isCordova"], true);

        let abs = svc(
            r#"{"service":"corp","clientId":"c","custom":true,"serverURL":"https://sso.corp","authorizePath":"https://sso.corp/authorize?tenant=1","scope":"openid"}"#,
        );
        let l = oauth_login(&abs, &server(), &OAuthOptions::default()).unwrap();
        assert!(
            l.url
                .starts_with("https://sso.corp/authorize?tenant=1&client_id=c&")
        );
    }

    #[test]
    fn builtin_providers() {
        let g = svc(r#"{"name":"google","clientId":"gid","custom":false}"#);
        let l = oauth_login(&g, &server(), &OAuthOptions::default()).unwrap();
        assert!(
            l.url
                .starts_with("https://accounts.google.com/o/oauth2/auth?")
        );
        assert_eq!(
            param(&l.url, "redirect_uri").unwrap(),
            "https://chat.example.com/_oauth/google?close"
        );
        assert_eq!(param(&l.url, "scope").unwrap(), "profile email");

        let gl = svc(r#"{"name":"gitlab","clientId":"x"}"#);
        let l = oauth_login(
            &gl,
            &server(),
            &OAuthOptions {
                gitlab_url: Some("https://git.corp/".into()),
            },
        )
        .unwrap();
        assert!(l.url.starts_with("https://git.corp/oauth/authorize?"));

        let fb = svc(r#"{"service":"facebook","appId":"fbid"}"#);
        let l = oauth_login(&fb, &server(), &OAuthOptions::default()).unwrap();
        assert_eq!(param(&l.url, "client_id").unwrap(), "fbid");

        let tw = svc(r#"{"service":"twitter","consumerKey":"k"}"#);
        let l = oauth_login(&tw, &server(), &OAuthOptions::default()).unwrap();
        assert!(l.url.starts_with(
            "https://chat.example.com/_oauth/twitter/?requestTokenAndRedirect=true&state="
        ));

        let none = svc(r#"{"name":"github"}"#);
        assert!(oauth_login(&none, &server(), &OAuthOptions::default()).is_err());
        let apple = svc(r#"{"name":"apple","clientId":"a"}"#);
        assert!(oauth_login(&apple, &server(), &OAuthOptions::default()).is_err());
    }

    #[test]
    fn other_flows() {
        let s = ServerUrl::parse("https://example.com/chat").unwrap();
        assert_eq!(
            modern_oauth_url(&s, "keycloak"),
            "https://example.com/chat/oauth/keycloak?loginClient=desktop"
        );
        assert_eq!(
            browser_login_url(&s),
            "https://example.com/chat/home?loginClient=desktop"
        );
        let saml = saml_login(&s, "rustket-saml");
        let tok = saml.credential_token.unwrap();
        assert_eq!(
            saml.url,
            format!(
                "https://example.com/chat/_saml/authorize/rustket-saml/{tok}?loginClient=desktop"
            )
        );
        let cas = cas_login(&s, "https://cas.example.com/cas/login");
        let tok = cas.credential_token.unwrap();
        assert_eq!(
            param(&cas.url, "service").unwrap(),
            format!("https://example.com/chat/_cas/{tok}")
        );
        assert!(
            cas.url
                .starts_with("https://cas.example.com/cas/login?service=https%3A%2F%2F")
        );
        let saml_cfg = svc(r#"{"service":"saml","clientConfig":{"provider":"p"}}"#);
        assert!(oauth_login(&saml_cfg, &s, &OAuthOptions::default()).is_err());
    }
}
