//! `rocketchat://` and `https://go.rocket.chat/` deep links.

use std::collections::HashMap;

use crate::error::Result;
use crate::server_url::ServerUrl;

#[derive(Clone, PartialEq, Eq)]
pub enum AuthDeepLink {
    /// Legacy OAuth: finish with `login_oauth(credential_token, credential_secret)`.
    OAuth {
        host: String,
        credential_token: String,
        credential_secret: String,
    },
    /// Modern OAuth / "sign in with browser": a ready login token (resume it).
    Token {
        host: String,
        user_id: String,
        token: String,
    },
    /// SAML: finish with `login_saml(credential_token)`.
    Saml {
        host: String,
        credential_token: String,
    },
    Error {
        host: String,
        error: String,
    },
}

impl std::fmt::Debug for AuthDeepLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth { host, .. } => write!(f, "AuthDeepLink::OAuth({host}, <redacted>)"),
            Self::Token { host, user_id, .. } => {
                write!(f, "AuthDeepLink::Token({host}, {user_id}, <redacted>)")
            }
            Self::Saml { host, .. } => write!(f, "AuthDeepLink::Saml({host}, <redacted>)"),
            Self::Error { host, error } => write!(f, "AuthDeepLink::Error({host}, {error})"),
        }
    }
}

impl AuthDeepLink {
    pub fn host(&self) -> &str {
        match self {
            Self::OAuth { host, .. }
            | Self::Token { host, .. }
            | Self::Saml { host, .. }
            | Self::Error { host, .. } => host,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDeepLink {
    pub host: String,
    pub rid: Option<String>,
    /// e.g. `channel/general`, `direct/bob`, `group/x?msg=ID`.
    pub path: Option<String>,
}

impl RoomDeepLink {
    /// `msg` query parameter inside `path` (permalink to a message).
    pub fn message_id(&self) -> Option<String> {
        let path = self.path.as_deref()?;
        let (_, q) = path.split_once('?')?;
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "msg")
            .map(|(_, v)| v.into_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLink {
    Auth(AuthDeepLink),
    Room(RoomDeepLink),
    /// `path` like `invite/TOKEN`.
    Invite {
        host: String,
        path: String,
    },
}

impl DeepLink {
    pub fn host(&self) -> &str {
        match self {
            Self::Auth(a) => a.host(),
            Self::Room(r) => &r.host,
            Self::Invite { host, .. } => host,
        }
    }

    /// Whether the link's `host` is exactly `server` (scheme, host, port; see
    /// [`ServerUrl::matches_link_host`]). After a match, keep using the caller's
    /// stored `ServerUrl`, never one built from the link.
    pub fn matches(&self, server: &ServerUrl) -> bool {
        server.matches_link_host(self.host())
    }

    /// Server URL built from the link's `host` (no scheme means https). The link is
    /// attacker-controlled: only use this to offer "add this server?", never to
    /// pick an existing account (use [`Self::matches`]).
    pub fn untrusted_server_url(&self) -> Result<ServerUrl> {
        ServerUrl::parse(self.host())
    }
}

/// Parses `rocketchat://{auth|room|invite}?…` or `https://go.rocket.chat/{…}?…`.
pub fn parse_deep_link(input: &str) -> Option<DeepLink> {
    let s = input.trim();
    let s = s.split_once('#').map_or(s, |(before, _)| before);
    let rest = s
        .strip_prefix("rocketchat://")
        .or_else(|| s.strip_prefix("https://go.rocket.chat/"))?;
    let (kind, query) = rest.split_once('?')?;
    let kind = kind.trim_end_matches('/');
    let q: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let get = |k: &str| q.get(k).filter(|v| !v.is_empty()).cloned();
    let host = get("host")?;
    match kind {
        "auth" => {
            if let Some(error) = get("error") {
                return Some(DeepLink::Auth(AuthDeepLink::Error { host, error }));
            }
            match get("type").as_deref() {
                Some("oauth") => Some(DeepLink::Auth(AuthDeepLink::OAuth {
                    host,
                    credential_token: get("credentialToken")?,
                    credential_secret: get("credentialSecret")?,
                })),
                Some("saml") => Some(DeepLink::Auth(AuthDeepLink::Saml {
                    host,
                    credential_token: get("credentialToken")?,
                })),
                _ => Some(DeepLink::Auth(AuthDeepLink::Token {
                    host,
                    user_id: get("userId")?,
                    token: get("token")?,
                })),
            }
        }
        "room" => {
            let rid = get("rid");
            let path = get("path");
            if rid.is_none() && path.is_none() {
                return None;
            }
            Some(DeepLink::Room(RoomDeepLink { host, rid, path }))
        }
        "invite" => Some(DeepLink::Invite {
            host,
            path: get("path")?,
        }),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn oauth_link() {
        let l = parse_deep_link(
            "rocketchat://auth?host=http://localhost:3000/&type=oauth&credentialToken=abc&credentialSecret=def",
        )
        .unwrap();
        assert_eq!(
            l,
            DeepLink::Auth(AuthDeepLink::OAuth {
                host: "http://localhost:3000/".into(),
                credential_token: "abc".into(),
                credential_secret: "def".into(),
            })
        );
        assert_eq!(
            l.untrusted_server_url().unwrap().as_str(),
            "http://localhost:3000"
        );
        let srv = ServerUrl::parse("http://localhost:3000").unwrap();
        assert!(l.matches(&srv));
        assert!(!l.matches(&ServerUrl::parse("https://localhost:3000").unwrap()));
        assert!(!format!("{l:?}").contains("def"));
        let down = parse_deep_link(
            "rocketchat://auth?host=http://chat.corp.com:80/&type=oauth&credentialToken=a&credentialSecret=b",
        )
        .unwrap();
        assert!(!down.matches(&ServerUrl::parse("https://chat.corp.com").unwrap()));
    }

    #[test]
    fn token_and_saml_links() {
        let l = parse_deep_link("rocketchat://auth?host=https://open.rocket.chat&token=T&userId=U")
            .unwrap();
        assert!(matches!(
            l,
            DeepLink::Auth(AuthDeepLink::Token { ref user_id, ref token, .. }) if user_id == "U" && token == "T"
        ));
        let l = parse_deep_link(
            "rocketchat://auth?type=saml&host=https%3A%2F%2Fchat.example.com&credentialToken=ct",
        )
        .unwrap();
        assert_eq!(
            l,
            DeepLink::Auth(AuthDeepLink::Saml {
                host: "https://chat.example.com".into(),
                credential_token: "ct".into()
            })
        );
        let l = parse_deep_link("https://go.rocket.chat/auth?host=x.org&error=denied").unwrap();
        assert!(matches!(l, DeepLink::Auth(AuthDeepLink::Error { .. })));
        assert_eq!(l.untrusted_server_url().unwrap().as_str(), "https://x.org");
        assert!(parse_deep_link("rocketchat://auth?host=x&type=oauth&credentialToken=a").is_none());
        assert!(parse_deep_link("rocketchat://auth?type=oauth").is_none());
    }

    #[test]
    fn room_and_invite_links() {
        let l = parse_deep_link(
            "rocketchat://room?host=open.rocket.chat&rid=GENERAL&path=channel/general",
        )
        .unwrap();
        let DeepLink::Room(r) = &l else { panic!() };
        assert_eq!(r.rid.as_deref(), Some("GENERAL"));
        assert_eq!(r.path.as_deref(), Some("channel/general"));
        assert_eq!(
            l.untrusted_server_url().unwrap().as_str(),
            "https://open.rocket.chat"
        );
        let l = parse_deep_link(
            "https://go.rocket.chat/room?host=localhost:3000&path=channel%2Fgeneral%3Fmsg%3DabcID",
        )
        .unwrap();
        let DeepLink::Room(r) = &l else { panic!() };
        assert_eq!(r.message_id().as_deref(), Some("abcID"));
        assert_eq!(
            l.untrusted_server_url().unwrap().as_str(),
            "https://localhost:3000"
        );
        let frag = parse_deep_link("rocketchat://room?host=a.org&rid=R#frag").unwrap();
        let DeepLink::Room(r) = frag else { panic!() };
        assert_eq!(r.rid.as_deref(), Some("R"));
        assert_eq!(
            parse_deep_link("rocketchat://invite?host=open.rocket.chat&path=invite/token123"),
            Some(DeepLink::Invite {
                host: "open.rocket.chat".into(),
                path: "invite/token123".into()
            })
        );
        assert!(parse_deep_link("https://example.com/some/path").is_none());
        assert!(parse_deep_link("").is_none());
        assert!(parse_deep_link("rocketchat://shareextension?text=hi").is_none());
        assert!(parse_deep_link("rocketchat://room?host=x").is_none());
    }
}
