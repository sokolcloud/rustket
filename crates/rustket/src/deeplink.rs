//! Acceptance rules for `rocketchat://` links.
//!
//! Auth links are attacker-controllable (any web page can open one), so they are
//! only acted upon when they match a login this app started:
//! - OAuth / SAML links must match a pending login of the same kind for the
//!   exact server (`DeepLink::matches`) and its `credentialToken`;
//! - Token links (modern OAuth, "sign in with browser") carry no nonce: they are
//!   accepted only while a browser/modern login for that server is pending, and
//!   even then the user has to confirm the account first (login CSRF);
//!   One pending login accepts exactly one Token link; a second one is treated
//!   as an attack and cancels the login.
//! - Error links are shown (as a notice, without cancelling anything) only for
//!   a pending login of that server.
//!
//! Room and invite links switch to a known server or offer to add the server;
//! a link to another scheme/port of a configured host is never offered.

use std::time::{Duration, Instant};

use rocketchat::auth::{AuthDeepLink, DeepLink};
use rocketchat::{Credentials, ServerUrl};
use rustket_core::ServerId;

/// How long a pending browser login waits for its deep link.
pub const PENDING_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, PartialEq, Eq)]
pub enum PendingKind {
    /// Legacy OAuth: expects `type=oauth` with this credential token.
    OAuth { credential_token: String },
    /// SAML: expects `type=saml` with this credential token (or polling).
    Saml { credential_token: String },
    /// CAS: polling only; no deep link expected.
    Cas { credential_token: String },
    /// Modern OAuth flow: expects a Token link.
    ModernOAuth,
    /// "Sign in with browser": expects a Token link.
    Browser,
}

impl std::fmt::Debug for PendingKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OAuth { .. } => "OAuth(<redacted>)",
            Self::Saml { .. } => "Saml(<redacted>)",
            Self::Cas { .. } => "Cas(<redacted>)",
            Self::ModernOAuth => "ModernOAuth",
            Self::Browser => "Browser",
        })
    }
}

impl PendingKind {
    pub fn expects_token(&self) -> bool {
        matches!(self, Self::ModernOAuth | Self::Browser)
    }
}

#[derive(Clone)]
pub struct PendingLogin {
    pub server_id: ServerId,
    pub server: ServerUrl,
    pub kind: PendingKind,
    pub started: Instant,
    /// The Token link accepted for this login (one per login; the same link
    /// again is a repeat, a different one an attack).
    pub token_claim: Option<Credentials>,
}

impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLogin")
            .field("server_id", &self.server_id)
            .field("server", &self.server.as_str())
            .field("kind", &self.kind)
            .field("token_claimed", &self.token_claim.is_some())
            .finish()
    }
}

impl PendingLogin {
    pub fn new(server_id: ServerId, server: ServerUrl, kind: PendingKind) -> Self {
        Self {
            server_id,
            server,
            kind,
            started: Instant::now(),
            token_claim: None,
        }
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.started) > PENDING_TTL
    }
}

/// What to do with a link.
#[derive(Clone, PartialEq, Eq)]
pub enum Decision {
    LoginOAuth {
        server_id: ServerId,
        credential_token: String,
        credential_secret: String,
    },
    LoginSaml {
        server_id: ServerId,
        credential_token: String,
    },
    /// Fetch `me` with the token and ask the user to confirm the account.
    ConfirmToken {
        server_id: ServerId,
        user_id: String,
        token: String,
    },
    /// A second Token link for a login that already got one: cancel the login.
    DuplicateToken {
        server_id: ServerId,
    },
    AuthError {
        server_id: ServerId,
        error: String,
    },
    OpenRoom {
        server_id: ServerId,
        rid: Option<String>,
        path: Option<String>,
    },
    OpenInvite {
        server_id: ServerId,
        path: String,
    },
    /// Unknown server: offer to add it (URL built from the untrusted link).
    OfferAddServer {
        url: ServerUrl,
    },
    /// The link names a configured host with another scheme or port: never
    /// offer it as a new server; warn instead.
    OtherOrigin {
        server_id: ServerId,
        url: ServerUrl,
    },
    Ignore(&'static str),
}

impl std::fmt::Debug for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoginOAuth { server_id, .. } => write!(f, "LoginOAuth({server_id}, <redacted>)"),
            Self::LoginSaml { server_id, .. } => write!(f, "LoginSaml({server_id}, <redacted>)"),
            Self::ConfirmToken {
                server_id, user_id, ..
            } => write!(f, "ConfirmToken({server_id}, {user_id}, <redacted>)"),
            Self::DuplicateToken { server_id } => write!(f, "DuplicateToken({server_id})"),
            Self::AuthError { server_id, .. } => write!(f, "AuthError({server_id})"),
            Self::OpenRoom {
                server_id,
                rid,
                path,
            } => write!(f, "OpenRoom({server_id}, {rid:?}, {path:?})"),
            Self::OpenInvite { server_id, .. } => write!(f, "OpenInvite({server_id})"),
            Self::OfferAddServer { url } => write!(f, "OfferAddServer({})", url.as_str()),
            Self::OtherOrigin { server_id, url } => {
                write!(f, "OtherOrigin({server_id}, {})", url.as_str())
            }
            Self::Ignore(r) => write!(f, "Ignore({r})"),
        }
    }
}

pub fn decide(
    link: &DeepLink,
    pending: &[PendingLogin],
    servers: &[(ServerId, ServerUrl)],
    now: Instant,
) -> Decision {
    let live = || {
        pending
            .iter()
            .filter(move |p| !p.is_expired(now) && link.matches(&p.server))
    };
    match link {
        DeepLink::Auth(AuthDeepLink::OAuth {
            credential_token,
            credential_secret,
            ..
        }) => live()
            .find(|p| {
                matches!(&p.kind, PendingKind::OAuth { credential_token: t } if t == credential_token)
            })
            .map_or(Decision::Ignore("no matching pending OAuth login"), |p| {
                Decision::LoginOAuth {
                    server_id: p.server_id.clone(),
                    credential_token: credential_token.clone(),
                    credential_secret: credential_secret.clone(),
                }
            }),
        DeepLink::Auth(AuthDeepLink::Saml {
            credential_token, ..
        }) => live()
            .find(|p| {
                matches!(&p.kind, PendingKind::Saml { credential_token: t } if t == credential_token)
            })
            .map_or(Decision::Ignore("no matching pending SAML login"), |p| {
                Decision::LoginSaml {
                    server_id: p.server_id.clone(),
                    credential_token: credential_token.clone(),
                }
            }),
        DeepLink::Auth(AuthDeepLink::Token { user_id, token, .. }) => {
            match live().find(|p| p.kind.expects_token()) {
                None => Decision::Ignore("no pending browser login"),
                Some(p) => match &p.token_claim {
                    Some(c) if c.user_id == *user_id && c.auth_token == *token => {
                        Decision::Ignore("same token link again")
                    }
                    Some(_) => Decision::DuplicateToken {
                        server_id: p.server_id.clone(),
                    },
                    None => Decision::ConfirmToken {
                        server_id: p.server_id.clone(),
                        user_id: user_id.clone(),
                        token: token.clone(),
                    },
                },
            }
        }
        DeepLink::Auth(AuthDeepLink::Error { error, .. }) => live()
            .next()
            .map_or(Decision::Ignore("auth error for no pending login"), |p| {
                Decision::AuthError {
                    server_id: p.server_id.clone(),
                    error: error.clone(),
                }
            }),
        DeepLink::Room(r) => match servers.iter().find(|(_, url)| link.matches(url)) {
            Some((id, _)) => Decision::OpenRoom {
                server_id: id.clone(),
                rid: r.rid.clone(),
                path: r.path.clone(),
            },
            None => offer(link, servers),
        },
        DeepLink::Invite { path, .. } => match servers.iter().find(|(_, url)| link.matches(url)) {
            Some((id, _)) => Decision::OpenInvite {
                server_id: id.clone(),
                path: path.clone(),
            },
            None => offer(link, servers),
        },
    }
}

fn offer(link: &DeepLink, servers: &[(ServerId, ServerUrl)]) -> Decision {
    let Ok(url) = link.untrusted_server_url() else {
        return Decision::Ignore("invalid host");
    };
    let host = |u: &ServerUrl| {
        u.url()
            .host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
    };
    match servers.iter().find(|(_, s)| host(s) == host(&url)) {
        Some((id, _)) => Decision::OtherOrigin {
            server_id: id.clone(),
            url,
        },
        None => Decision::OfferAddServer { url },
    }
}

/// Room name from a `path` such as `channel/general`, `group/x?msg=…`, `direct/bob`.
pub fn room_name_from_path(path: &str) -> Option<(&str, &str)> {
    let path = path.split('?').next().unwrap_or(path).trim_matches('/');
    let (kind, name) = path.split_once('/')?;
    let name = name.split('/').next().unwrap_or(name);
    (!name.is_empty()).then_some((kind, name))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::auth::parse_deep_link;

    fn server() -> ServerUrl {
        ServerUrl::parse("http://localhost:3000").unwrap()
    }

    fn pending(kind: PendingKind) -> PendingLogin {
        PendingLogin::new(ServerId("s1".into()), server(), kind)
    }

    fn servers() -> Vec<(ServerId, ServerUrl)> {
        vec![(ServerId("s1".into()), server())]
    }

    fn link(s: &str) -> DeepLink {
        parse_deep_link(s).unwrap()
    }

    #[test]
    fn oauth_needs_matching_pending_login() {
        let l = link(
            "rocketchat://auth?host=http://localhost:3000/&type=oauth&credentialToken=T1&credentialSecret=S",
        );
        let now = Instant::now();
        // Nothing pending.
        assert!(matches!(
            decide(&l, &[], &servers(), now),
            Decision::Ignore(_)
        ));
        // Wrong token.
        let p = pending(PendingKind::OAuth {
            credential_token: "OTHER".into(),
        });
        assert!(matches!(
            decide(&l, &[p], &servers(), now),
            Decision::Ignore(_)
        ));
        // Wrong kind (SAML pending with the same token).
        let p = pending(PendingKind::Saml {
            credential_token: "T1".into(),
        });
        assert!(matches!(
            decide(&l, &[p], &servers(), now),
            Decision::Ignore(_)
        ));
        // Match.
        let p = pending(PendingKind::OAuth {
            credential_token: "T1".into(),
        });
        assert_eq!(
            decide(&l, std::slice::from_ref(&p), &servers(), now),
            Decision::LoginOAuth {
                server_id: ServerId("s1".into()),
                credential_token: "T1".into(),
                credential_secret: "S".into()
            }
        );
        // Another server (port differs) does not match.
        let other = link(
            "rocketchat://auth?host=http://localhost:3001/&type=oauth&credentialToken=T1&credentialSecret=S",
        );
        assert!(matches!(
            decide(&other, std::slice::from_ref(&p), &servers(), now),
            Decision::Ignore(_)
        ));
        // Expired.
        let later = now + PENDING_TTL + Duration::from_secs(1);
        assert!(matches!(
            decide(&l, &[p], &servers(), later),
            Decision::Ignore(_)
        ));
    }

    #[test]
    fn saml_matches_token() {
        let l = link("rocketchat://auth?host=http://localhost:3000&type=saml&credentialToken=T2");
        let p = pending(PendingKind::Saml {
            credential_token: "T2".into(),
        });
        assert_eq!(
            decide(&l, &[p], &servers(), Instant::now()),
            Decision::LoginSaml {
                server_id: ServerId("s1".into()),
                credential_token: "T2".into()
            }
        );
    }

    #[test]
    fn token_link_requires_pending_browser_login_and_confirmation() {
        let l = link("rocketchat://auth?host=http://localhost:3000&userId=U&token=TOK");
        let now = Instant::now();
        // Unsolicited token link (login CSRF): ignored.
        assert!(matches!(
            decide(&l, &[], &servers(), now),
            Decision::Ignore(_)
        ));
        // Pending OAuth (legacy) does not accept token links.
        let p = pending(PendingKind::OAuth {
            credential_token: "T".into(),
        });
        assert!(matches!(
            decide(&l, &[p], &servers(), now),
            Decision::Ignore(_)
        ));
        // Pending browser login: confirmation, never a silent login.
        for kind in [PendingKind::Browser, PendingKind::ModernOAuth] {
            assert_eq!(
                decide(&l, &[pending(kind)], &servers(), now),
                Decision::ConfirmToken {
                    server_id: ServerId("s1".into()),
                    user_id: "U".into(),
                    token: "TOK".into()
                }
            );
        }
    }

    #[test]
    fn error_links_only_for_pending() {
        let l = link("rocketchat://auth?host=http://localhost:3000&error=denied");
        assert!(matches!(
            decide(&l, &[], &servers(), Instant::now()),
            Decision::Ignore(_)
        ));
        assert_eq!(
            decide(
                &l,
                &[pending(PendingKind::Browser)],
                &servers(),
                Instant::now()
            ),
            Decision::AuthError {
                server_id: ServerId("s1".into()),
                error: "denied".into()
            }
        );
    }

    #[test]
    fn room_links() {
        let l = link("rocketchat://room?host=localhost:3000&rid=R1&path=channel/general");
        // `localhost:3000` without scheme means https: not the configured
        // http server, and never offered as a duplicate of it (review #8).
        assert!(matches!(
            decide(&l, &[], &servers(), Instant::now()),
            Decision::OtherOrigin { .. }
        ));
        let other_port = link("rocketchat://room?host=http://localhost:4000&rid=R1");
        assert!(matches!(
            decide(&other_port, &[], &servers(), Instant::now()),
            Decision::OtherOrigin { .. }
        ));
        let l = link("rocketchat://room?host=http://localhost:3000&rid=R1&path=channel/general");
        assert_eq!(
            decide(&l, &[], &servers(), Instant::now()),
            Decision::OpenRoom {
                server_id: ServerId("s1".into()),
                rid: Some("R1".into()),
                path: Some("channel/general".into())
            }
        );
        let l = link("rocketchat://invite?host=https://chat.example.com&path=invite/ABC");
        assert_eq!(
            decide(&l, &[], &servers(), Instant::now()),
            Decision::OfferAddServer {
                url: ServerUrl::parse("https://chat.example.com").unwrap()
            }
        );
        assert_eq!(
            room_name_from_path("channel/general?msg=x"),
            Some(("channel", "general"))
        );
        assert_eq!(room_name_from_path("direct/bob"), Some(("direct", "bob")));
        assert_eq!(room_name_from_path("nothing"), None);
    }

    /// Review #24: one pending login accepts exactly one Token link.
    #[test]
    fn second_token_link_is_a_duplicate() {
        let l = link("rocketchat://auth?host=http://localhost:3000&userId=U&token=TOK");
        let mut p = pending(PendingKind::Browser);
        p.token_claim = Some(Credentials::new("OTHER", "T9"));
        assert_eq!(
            decide(&l, std::slice::from_ref(&p), &servers(), Instant::now()),
            Decision::DuplicateToken {
                server_id: ServerId("s1".into())
            }
        );
        // Review #10: the same link again is a harmless repeat.
        p.token_claim = Some(Credentials::new("U", "TOK"));
        assert!(matches!(
            decide(&l, &[p], &servers(), Instant::now()),
            Decision::Ignore(_)
        ));
    }

    #[test]
    fn debug_redacts_secrets() {
        let d = Decision::ConfirmToken {
            server_id: ServerId("s".into()),
            user_id: "U".into(),
            token: "TOKEN-SECRET".into(),
        };
        assert!(!format!("{d:?}").contains("TOKEN-SECRET"));
        let d = Decision::LoginOAuth {
            server_id: ServerId("s".into()),
            credential_token: "CT".into(),
            credential_secret: "OAUTH-SECRET".into(),
        };
        assert!(!format!("{d:?}").contains("OAUTH-SECRET"));
        let k = PendingKind::Saml {
            credential_token: "SAML-NONCE".into(),
        };
        assert!(!format!("{k:?}").contains("SAML-NONCE"));
        assert!(!format!("{:?}", pending(k)).contains("SAML-NONCE"));
    }

    /// Review #25: server binding, expiry and kind rules.
    mod decide_gaps {
        use super::*;

        fn url(s: &str) -> ServerUrl {
            ServerUrl::parse(s).unwrap()
        }
        fn pending_at(id: &str, server: &str, kind: PendingKind, started: Instant) -> PendingLogin {
            let mut p = PendingLogin::new(ServerId(id.into()), url(server), kind);
            p.started = started;
            p
        }
        fn servers() -> Vec<(ServerId, ServerUrl)> {
            vec![
                (ServerId("a".into()), url("http://localhost:3000")),
                (ServerId("b".into()), url("https://chat.example.com")),
            ]
        }

        #[test]
        fn token_link_bound_to_pending_server() {
            let now = Instant::now();
            let p = [pending_at(
                "a",
                "http://localhost:3000",
                PendingKind::Browser,
                now,
            )];
            let foreign =
                parse_deep_link("rocketchat://auth?host=https://chat.example.com&userId=U&token=T")
                    .unwrap();
            assert!(matches!(
                decide(&foreign, &p, &servers(), now),
                Decision::Ignore(_)
            ));
        }

        #[test]
        fn token_link_expired_pending() {
            let now = Instant::now();
            let l =
                parse_deep_link("rocketchat://auth?host=http://localhost:3000&userId=U&token=T")
                    .unwrap();
            let later = now + PENDING_TTL + Duration::from_secs(1);
            for kind in [PendingKind::ModernOAuth, PendingKind::Browser] {
                let p = [pending_at("a", "http://localhost:3000", kind, now)];
                assert!(matches!(
                    decide(&l, &p, &servers(), later),
                    Decision::Ignore(_)
                ));
            }
        }

        #[test]
        fn token_link_not_for_saml_or_cas() {
            let now = Instant::now();
            let l =
                parse_deep_link("rocketchat://auth?host=http://localhost:3000&userId=U&token=T")
                    .unwrap();
            for kind in [
                PendingKind::Saml {
                    credential_token: "x".into(),
                },
                PendingKind::Cas {
                    credential_token: "x".into(),
                },
            ] {
                let p = [pending_at("a", "http://localhost:3000", kind, now)];
                assert!(matches!(
                    decide(&l, &p, &servers(), now),
                    Decision::Ignore(_)
                ));
            }
        }

        #[test]
        fn saml_wrong_token_and_error_foreign_server() {
            let now = Instant::now();
            let p = [pending_at(
                "a",
                "http://localhost:3000",
                PendingKind::Saml {
                    credential_token: "GOOD".into(),
                },
                now,
            )];
            let l = parse_deep_link(
                "rocketchat://auth?host=http://localhost:3000&type=saml&credentialToken=EVIL",
            )
            .unwrap();
            assert!(matches!(
                decide(&l, &p, &servers(), now),
                Decision::Ignore(_)
            ));
            let e =
                parse_deep_link("rocketchat://auth?host=https://chat.example.com&error=x").unwrap();
            assert!(matches!(
                decide(&e, &p, &servers(), now),
                Decision::Ignore(_)
            ));
        }

        #[test]
        fn invite_for_known_server_and_second_server_picked() {
            let now = Instant::now();
            let l = parse_deep_link(
                "rocketchat://invite?host=https://chat.example.com&path=invite/ABC",
            )
            .unwrap();
            assert_eq!(
                decide(&l, &[], &servers(), now),
                Decision::OpenInvite {
                    server_id: ServerId("b".into()),
                    path: "invite/ABC".into()
                }
            );
        }
    }
}
