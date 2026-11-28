//! `ManagedServers` items, `AllowedServers` patterns and the server check of
//! docs/policy.md section 9.1. Server addresses are compared in the form
//! [`ServerUrl`] normalises them to: lower-case scheme and host, punycode,
//! no trailing dot, default port omitted, no trailing slash.

use rocketchat::ServerUrl;
use url::{Host, Url};

use super::Policy;
use super::resolve::breaks_single_line;
use super::value::{EMBED_MAX_CHARS, quote, truncate};

/// Longest display name of a managed server, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 80;

/// Text embedded in the errors is truncated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerItemError {
    #[error("the address must start with http:// or https://")]
    MissingScheme,
    #[error("invalid address: {0}")]
    Url(String),
    #[error("unsupported scheme {} (use http or https)", quote(.0))]
    Scheme(String),
    #[error("user names, queries and fragments are not allowed in a pattern")]
    ExtraParts,
    #[error("invalid port {}", quote(.0))]
    Port(String),
    #[error("`*` is only allowed as a leading `*.` before a domain name")]
    Wildcard,
}

impl ServerItemError {
    /// Stable short code (a [`super::DropReason`] code).
    pub fn code(&self) -> &'static str {
        match self {
            ServerItemError::MissingScheme => "missing_scheme",
            ServerItemError::Url(_) => "invalid_address",
            ServerItemError::Scheme(_) => "unsupported_scheme",
            ServerItemError::ExtraParts => "extra_parts",
            ServerItemError::Port(_) => "invalid_port",
            ServerItemError::Wildcard => "bad_wildcard",
        }
    }
}

/// One `ManagedServers` item: `URL` or `URL<whitespace>Display name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedServer {
    pub url: ServerUrl,
    /// One line: characters that break a single line (as for
    /// `OrganizationName`) become spaces, runs of whitespace one space;
    /// trimmed, at most [`MAX_DISPLAY_NAME_CHARS`] characters. `None` when
    /// absent or empty after that (the server's own name is used once it
    /// answers).
    pub name: Option<String>,
}

impl ManagedServer {
    /// Parses an item. The URL must name its scheme (http or https); the
    /// first whitespace separates it from the display name.
    pub fn parse(item: &str) -> Result<Self, ServerItemError> {
        let item = item.trim();
        let (url, name) = item.split_once(char::is_whitespace).unwrap_or((item, ""));
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(ServerItemError::MissingScheme);
        }
        // `ServerUrl`'s error text contains the input: report the reason only.
        let url =
            ServerUrl::parse(url).map_err(|_| ServerItemError::url(&server_url_reason(url)))?;
        let name = display_name(name);
        Ok(Self { url, name })
    }
}

impl ServerItemError {
    /// `text` is a parser's own message or a fixed one, never the input.
    fn url(text: &str) -> Self {
        ServerItemError::Url(truncate(text, EMBED_MAX_CHARS))
    }
}

/// Why `ServerUrl::parse` refused `raw` (whose scheme is http or https),
/// without quoting `raw`.
fn server_url_reason(raw: &str) -> String {
    match Url::parse(raw.trim()) {
        Err(e) => e.to_string(),
        Ok(u) if u.host_str().is_none_or(str::is_empty) => "missing host".to_owned(),
        Ok(u) if !u.username().is_empty() || u.password().is_some() => {
            "user names and passwords are not allowed".to_owned()
        }
        Ok(_) => "not a usable server address".to_owned(),
    }
}

/// A managed server's display name as stored, `None` when nothing is left.
fn display_name(raw: &str) -> Option<String> {
    let spaced: String = raw
        .chars()
        .map(|c| if breaks_single_line(c) { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = truncate_plain(&collapsed, MAX_DISPLAY_NAME_CHARS);
    (!name.is_empty()).then_some(name)
}

/// The first `max` characters of `s` (no ellipsis: this is the stored value).
fn truncate_plain(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
}

/// One `AllowedServers` pattern: `[scheme://]host[:port][/path-prefix]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPattern {
    /// `None` matches http and https.
    pub scheme: Option<Scheme>,
    /// Normalised host (lower case, punycode, IPv6 in brackets, no
    /// trailing dot) without the `*.` prefix.
    pub host: String,
    /// `*.host`: any subdomain of `host`, not `host` itself.
    pub wildcard: bool,
    /// `None` means the server's scheme's default port.
    pub port: Option<u16>,
    /// Normalised path prefix without a trailing slash; empty matches any path.
    pub path: String,
}

impl ServerPattern {
    pub fn parse(pattern: &str) -> Result<Self, ServerItemError> {
        let pattern = pattern.trim();
        if pattern.contains(['?', '#', '@']) {
            return Err(ServerItemError::ExtraParts);
        }
        let (scheme, rest) = match pattern.split_once("://") {
            Some((s, rest)) => match s.to_ascii_lowercase().as_str() {
                "http" => (Some(Scheme::Http), rest),
                "https" => (Some(Scheme::Https), rest),
                _ => return Err(ServerItemError::Scheme(truncate(s, EMBED_MAX_CHARS))),
            },
            None => (None, pattern),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let (wildcard, authority) = match authority.strip_prefix("*.") {
            Some(a) => (true, a),
            None => (false, authority),
        };
        let (host, port) = split_port(authority)?;
        if host.contains('*') {
            return Err(ServerItemError::Wildcard);
        }
        if host.is_empty() {
            return Err(ServerItemError::url("missing host"));
        }
        // A special scheme gives the host the same IDNA and IP normalisation
        // as a server address; the port is kept separately because the URL
        // parser drops a scheme's default port.
        let url = Url::parse(&format!("http://{host}{path}"))
            .map_err(|e| ServerItemError::url(&e.to_string()))?;
        if wildcard && !matches!(url.host(), Some(Host::Domain(_))) {
            return Err(ServerItemError::Wildcard);
        }
        // The url crate's serialisation, as `ServerUrl` has it (IPv6 in
        // brackets, IPv4-mapped addresses in hex groups).
        let normalised_host = match url.host_str() {
            Some(h) => h.trim_end_matches('.').to_owned(),
            None => return Err(ServerItemError::url("missing host")),
        };
        if normalised_host.is_empty() {
            return Err(ServerItemError::url("missing host"));
        }
        Ok(Self {
            scheme,
            host: normalised_host,
            wildcard,
            port,
            path: url.path().trim_end_matches('/').to_owned(),
        })
    }

    pub fn matches(&self, server: &ServerUrl) -> bool {
        let url = server.url();
        if self.scheme.is_some_and(|s| s.as_str() != url.scheme()) {
            return false;
        }
        let port_ok = match self.port {
            Some(p) => url.port_or_known_default() == Some(p),
            None => url.port().is_none(),
        };
        if !port_ok {
            return false;
        }
        let Some(host) = url.host_str().map(|h| h.trim_end_matches('.')) else {
            return false;
        };
        let host_ok = if self.wildcard {
            host.len() > self.host.len() + 1
                && host.ends_with(&self.host)
                && host[..host.len() - self.host.len()].ends_with('.')
        } else {
            host == self.host
        };
        if !host_ok {
            return false;
        }
        let prefix = server.path_prefix();
        self.path.is_empty()
            || prefix == self.path
            || prefix
                .strip_prefix(self.path.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Splits `host[:port]` (IPv6 in brackets).
fn split_port(authority: &str) -> Result<(&str, Option<u16>), ServerItemError> {
    let (host, port) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => {
                let (host, rest) = authority.split_at(end + 1);
                match rest {
                    "" => (host, None),
                    _ => match rest.strip_prefix(':') {
                        Some(p) => (host, Some(p)),
                        None => return Err(ServerItemError::url("unexpected text after `]`")),
                    },
                }
            }
            None => return Err(ServerItemError::url("`[` without `]`")),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => Some(
            p.parse::<u16>()
                .map_err(|_| ServerItemError::Port(truncate(p, EMBED_MAX_CHARS)))?,
        ),
        Some(p) => return Err(ServerItemError::Port(truncate(p, EMBED_MAX_CHARS))),
    };
    Ok((host, port))
}

/// Outcome of [`Policy::server_access`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAccess {
    Allowed,
    /// `RequireHttps` is set and the server uses http.
    HttpsRequired,
    /// `AllowedServers` is set and no pattern matches.
    NotAllowed,
}

impl Policy {
    /// Whether `server` is one of the `ManagedServers` (same normalised URL).
    pub fn is_managed_server(&self, server: &ServerUrl) -> bool {
        self.managed_servers()
            .iter()
            .any(|m| m.url.as_str() == server.as_str())
    }

    /// Section 9.1: `RequireHttps` refuses http for every server; otherwise
    /// a managed server is always allowed, and any server is when
    /// `AllowedServers` is not set.
    pub fn server_access(&self, server: &ServerUrl) -> ServerAccess {
        if self.require_https() && !server.is_https() {
            return ServerAccess::HttpsRequired;
        }
        if self.is_managed_server(server) {
            return ServerAccess::Allowed;
        }
        match self.allowed_servers() {
            None => ServerAccess::Allowed,
            Some(patterns) if patterns.iter().any(|p| p.matches(server)) => ServerAccess::Allowed,
            Some(_) => ServerAccess::NotAllowed,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_server_allowed(&self, server: &ServerUrl) -> bool {
        self.server_access(server) == ServerAccess::Allowed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::{Context, Os, RawValue, Snapshot, SourceId, Trust, resolve};

    fn url(s: &str) -> ServerUrl {
        ServerUrl::parse(s).unwrap()
    }

    #[test]
    fn managed_items() {
        for (item, expected_url, expected_name) in [
            ("https://chat.corp.com", "https://chat.corp.com", None),
            ("  https://chat.corp.com/  ", "https://chat.corp.com", None),
            (
                "https://chat.corp.com Corp chat",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            (
                "https://chat.corp.com\t  Corp   chat  ",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            // Characters that break a single line become spaces.
            (
                "https://chat.corp.com Corp\nchat",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            (
                "https://chat.corp.com Corp\tchat",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            (
                "https://chat.corp.com Corp\u{202E}chat",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            (
                "https://chat.corp.com Corp\u{2028}\u{1}chat",
                "https://chat.corp.com",
                Some("Corp chat"),
            ),
            (
                "https://chat.corp.com \u{1}\u{7f}\u{202E}\u{200F}",
                "https://chat.corp.com",
                None,
            ),
            ("HTTP://Chat.Corp.COM:80/x/", "http://chat.corp.com/x", None),
            (
                "https://bücher.example",
                "https://xn--bcher-kva.example",
                None,
            ),
            ("http://[::1]:3000", "http://[::1]:3000", None),
            ("https://chat.corp.com.", "https://chat.corp.com", None),
        ] {
            let m = ManagedServer::parse(item).unwrap();
            assert_eq!(m.url.as_str(), expected_url, "{item}");
            assert_eq!(m.name.as_deref(), expected_name, "{item}");
        }
        let long = format!("https://a.example {}", "n".repeat(100));
        let m = ManagedServer::parse(&long).unwrap();
        assert_eq!(m.name.unwrap().chars().count(), MAX_DISPLAY_NAME_CHARS);
        // The cap applies after collapsing.
        let spaced = format!("https://a.example {}x", "n \u{202E}\t ".repeat(40));
        let name = ManagedServer::parse(&spaced).unwrap().name.unwrap();
        assert_eq!(name.chars().count(), MAX_DISPLAY_NAME_CHARS - 1);
        assert!(name.starts_with("n n n") && !name.ends_with(' '));
        for bad in [
            "chat.corp.com",
            "chat.corp.com Corp",
            "ftp://chat.corp.com",
            "https://",
            "https://u:p@chat.corp.com",
            "https//chat.corp.com",
            "",
        ] {
            assert!(ManagedServer::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pattern_parsing() {
        let p = ServerPattern::parse("HTTPS://*.Corp.COM.:8443/Chat/").unwrap();
        assert_eq!(
            p,
            ServerPattern {
                scheme: Some(Scheme::Https),
                host: "corp.com".into(),
                wildcard: true,
                port: Some(8443),
                path: "/Chat".into(),
            }
        );
        let p = ServerPattern::parse("[::1]:3000").unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, Some(3000));
        assert_eq!(
            ServerPattern::parse("bücher.example").unwrap().host,
            "xn--bcher-kva.example"
        );
        assert_eq!(ServerPattern::parse("example.com/").unwrap().path, "");
        assert_eq!(
            ServerPattern::parse("[::ffff:1.2.3.4]").unwrap().host,
            "[::ffff:102:304]"
        );
        assert_eq!(ServerPattern::parse("10.0.0.1").unwrap().host, "10.0.0.1");
        for bad in [
            "ftp://example.com",
            "ws://example.com",
            "example.com:99999",
            "example.com:",
            "example.com:x",
            "a*.example.com",
            "*example.com",
            "*.*.example.com",
            "example.*",
            "*.127.0.0.1",
            "*.[::1]",
            "user@example.com",
            "example.com/?q",
            "example.com#x",
            "[::1",
            "[::1]x",
            "",
            "*.",
            "https://",
            "exa mple.com",
        ] {
            assert!(ServerPattern::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn errors_quote_values_stably() {
        assert_eq!(
            ServerPattern::parse("фтп\u{7}://a.example")
                .unwrap_err()
                .to_string(),
            r#"unsupported scheme "фтп\u0007" (use http or https)"#
        );
        assert_eq!(
            ServerPattern::parse("a.example:x\u{7}")
                .unwrap_err()
                .to_string(),
            r#"invalid port "x\u0007""#
        );
    }

    #[test]
    fn errors_embed_truncated_text() {
        let long = "x".repeat(5000);
        for bad in [
            format!("{long}://a.example"),
            format!("a.example:{long}"),
            format!("[{long}"),
            format!("https://{long}%zz"),
        ] {
            let e = ServerPattern::parse(&bad).unwrap_err();
            assert!(e.to_string().chars().count() < 200, "{e}");
        }
        let e = ManagedServer::parse(&format!("https://{long}%zz")).unwrap_err();
        assert!(e.to_string().chars().count() < 200, "{e}");
        let e = ManagedServer::parse(&format!("https://[{long}")).unwrap_err();
        assert!(e.to_string().chars().count() < 200, "{e}");
    }

    #[test]
    fn pattern_matching() {
        for (pattern, server, expected) in [
            // Scheme given and omitted.
            ("https://chat.corp.com", "https://chat.corp.com", true),
            ("https://chat.corp.com", "http://chat.corp.com", false),
            ("http://chat.corp.com", "http://chat.corp.com", true),
            ("chat.corp.com", "https://chat.corp.com", true),
            ("chat.corp.com", "http://chat.corp.com", true),
            // Default and explicit ports.
            ("chat.corp.com", "https://chat.corp.com:443", true),
            ("chat.corp.com", "https://chat.corp.com:8443", false),
            ("chat.corp.com", "http://chat.corp.com:443", false),
            ("chat.corp.com:443", "https://chat.corp.com", true),
            ("https://chat.corp.com:443", "https://chat.corp.com", true),
            ("chat.corp.com:8443", "https://chat.corp.com:8443", true),
            ("chat.corp.com:8443", "https://chat.corp.com", false),
            (
                "http://chat.corp.com:3000",
                "http://chat.corp.com:3000",
                true,
            ),
            // Wildcard subdomain versus bare domain.
            ("*.corp.com", "https://chat.corp.com", true),
            ("*.corp.com", "https://a.b.corp.com", true),
            ("*.corp.com", "https://corp.com", false),
            ("*.corp.com", "https://evilcorp.com", false),
            ("*.corp.com", "https://corp.com.evil.com", false),
            ("corp.com", "https://chat.corp.com", false),
            // Path prefix.
            ("corp.com/chat", "https://corp.com/chat", true),
            ("corp.com/chat", "https://corp.com/chat/team", true),
            ("corp.com/chat/", "https://corp.com/chat", true),
            ("corp.com/chat", "https://corp.com/chatroom", false),
            ("corp.com/chat", "https://corp.com", false),
            ("corp.com/Chat", "https://corp.com/chat", false),
            ("corp.com", "https://corp.com/any/path", true),
            // Upper-case host, IDN, IPv6, trailing dot.
            ("CHAT.Corp.Com", "https://chat.corp.com", true),
            ("chat.corp.com", "https://CHAT.CORP.COM", true),
            ("bücher.example", "https://xn--bcher-kva.example", true),
            ("xn--bcher-kva.example", "https://bücher.example", true),
            ("*.bücher.example", "https://chat.bücher.example", true),
            ("[::1]:3000", "http://[::1]:3000", true),
            ("[0:0::1]:3000", "http://[::1]:3000", true),
            ("[::1]", "http://[::1]:3000", false),
            (
                "[::ffff:1.2.3.4]:3000",
                "http://[::ffff:1.2.3.4]:3000",
                true,
            ),
            (
                "[::ffff:102:304]:3000",
                "http://[::ffff:1.2.3.4]:3000",
                true,
            ),
            ("[2001:DB8::1]", "https://[2001:db8::1]", true),
            ("127.0.0.1:3000", "http://127.0.0.1:3000", true),
            ("chat.corp.com.", "https://chat.corp.com", true),
            ("chat.corp.com", "https://chat.corp.com.", true),
            ("*.corp.com.", "https://chat.corp.com.", true),
        ] {
            let p = ServerPattern::parse(pattern).unwrap();
            assert_eq!(p.matches(&url(server)), expected, "{pattern} vs {server}");
        }
    }

    fn policy(values: &[(&str, RawValue)]) -> Policy {
        let mut snap = Snapshot::new(SourceId::Test("a".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        let ctx = Context {
            client_version: "0.4.0".into(),
            os: Os::Linux,
            mas: false,
            self_update: true,
        };
        resolve(&[snap], &ctx)
    }

    fn list(items: &[&str]) -> RawValue {
        RawValue::List(
            items
                .iter()
                .map(|s| RawValue::String((*s).into()))
                .collect(),
        )
    }

    #[test]
    fn server_access() {
        use ServerAccess::*;
        let empty = Policy::default();
        assert_eq!(empty.server_access(&url("http://anything:3000")), Allowed);

        let p = policy(&[
            (
                "ManagedServers",
                list(&["https://managed.example Managed", "http://plain.example"]),
            ),
            ("AllowedServers", list(&["*.corp.com", "not a pattern"])),
        ]);
        assert_eq!(p.server_access(&url("https://managed.example")), Allowed);
        assert_eq!(
            p.server_access(&url("https://managed.example:443/")),
            Allowed
        );
        assert_eq!(p.server_access(&url("http://plain.example")), Allowed);
        assert_eq!(p.server_access(&url("https://chat.corp.com")), Allowed);
        assert_eq!(p.server_access(&url("https://other.example")), NotAllowed);
        assert_eq!(p.allowed_servers().unwrap().len(), 1);
        assert!(p.is_managed_server(&url("https://managed.example")));
        assert!(!p.is_managed_server(&url("https://managed.example/sub")));

        // An empty list allows managed servers only.
        let p = policy(&[
            ("ManagedServers", list(&["https://managed.example"])),
            ("AllowedServers", list(&[])),
        ]);
        assert!(p.is_server_allowed(&url("https://managed.example")));
        assert!(!p.is_server_allowed(&url("https://chat.corp.com")));

        // RequireHttps refuses http for every server, managed or not.
        let p = policy(&[
            ("ManagedServers", list(&["http://plain.example"])),
            ("RequireHttps", RawValue::Bool(true)),
        ]);
        assert_eq!(p.server_access(&url("http://plain.example")), HttpsRequired);
        assert_eq!(p.server_access(&url("http://other.example")), HttpsRequired);
        assert_eq!(p.server_access(&url("https://other.example")), Allowed);
        let p = policy(&[("RequireHttps", RawValue::Bool(false))]);
        assert_eq!(p.server_access(&url("http://other.example")), Allowed);
    }
}
