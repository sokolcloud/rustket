//! Normalised Rocket.Chat server base URL and URL builders.

use std::fmt;
use std::str::FromStr;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::error::Error;

/// Characters escaped inside a single path segment.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%')
    .add(b'\\')
    .add(b'^')
    .add(b'|');

/// Characters escaped inside a query component.
const QUERY: &AsciiSet = &SEGMENT.add(b'&').add(b'=').add(b'+').add(b':').add(b'@');

pub(crate) fn encode_segment(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

pub(crate) fn encode_query(s: &str) -> String {
    utf8_percent_encode(s, QUERY).to_string()
}

/// Canonical server base URL: `scheme://host[:port][/prefix]`, without a trailing slash.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ServerUrl {
    base: String,
    url: Url,
}

impl ServerUrl {
    /// Parses user input such as `chat.example.com`, `https://chat.example.com/`,
    /// `http://localhost:3000` or `https://example.com/chat`. The scheme defaults to
    /// https; query strings and fragments are dropped.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(Error::Url("empty server address".into()));
        }
        let with_scheme = if trimmed.contains("://") {
            trimmed.to_owned()
        } else {
            format!("https://{trimmed}")
        };
        let mut url =
            Url::parse(&with_scheme).map_err(|e| Error::Url(format!("{trimmed}: {e}")))?;
        match url.scheme() {
            "http" | "https" => {}
            other => return Err(Error::Url(format!("unsupported scheme: {other}"))),
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(Error::Url(format!("{trimmed}: missing host")));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Url(
                "credentials in the server URL are not supported".into(),
            ));
        }
        if let Some(h) = url.host_str()
            && h.ends_with('.')
        {
            let trimmed_host = h.trim_end_matches('.').to_owned();
            url.set_host(Some(&trimmed_host))
                .map_err(|e| Error::Url(format!("{trimmed}: {e}")))?;
        }
        url.set_query(None);
        url.set_fragment(None);
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&path);
        let base = url.as_str().trim_end_matches('/').to_owned();
        Ok(Self { base, url })
    }

    /// The base URL without a trailing slash.
    pub fn as_str(&self) -> &str {
        &self.base
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn is_https(&self) -> bool {
        self.url.scheme() == "https"
    }

    /// Path prefix (`/chat` for `https://example.com/chat`), empty for root deployments.
    pub fn path_prefix(&self) -> &str {
        self.url.path().trim_end_matches('/')
    }

    /// `scheme://host[:port]` without the path prefix.
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }

    /// Host for display: `chat.example.com`, `localhost:3000`, `example.com/chat`.
    pub fn host(&self) -> String {
        let mut s = self.url.host_str().unwrap_or_default().to_owned();
        if let Some(port) = self.url.port() {
            s.push(':');
            s.push_str(&port.to_string());
        }
        s.push_str(self.path_prefix());
        s
    }

    /// `{base}/{path}` for a server-relative path.
    pub fn join(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// `{base}/api/v1/{path}`; `path` may contain a query string.
    pub fn api(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.base, path.trim_start_matches('/'))
    }

    /// `{base}/api/{path}` (e.g. `info`).
    pub fn api_root(&self, path: &str) -> String {
        format!("{}/api/{}", self.base, path.trim_start_matches('/'))
    }

    /// DDP endpoint: `ws(s)://host[/prefix]/websocket`.
    pub fn websocket(&self) -> String {
        let scheme = if self.is_https() { "wss" } else { "ws" };
        let rest = self
            .base
            .split_once("://")
            .map_or(self.base.as_str(), |(_, r)| r);
        format!("{scheme}://{rest}/websocket")
    }

    pub fn avatar_user(&self, username: &str, size: Option<u32>) -> String {
        let mut s = format!("{}/avatar/{}", self.base, encode_segment(username));
        if let Some(size) = size {
            s.push_str(&format!("?size={size}"));
        }
        s
    }

    pub fn avatar_room(&self, rid: &str, size: Option<u32>) -> String {
        let mut s = format!("{}/avatar/room/{}", self.base, encode_segment(rid));
        if let Some(size) = size {
            s.push_str(&format!("?size={size}"));
        }
        s
    }

    /// Custom emoji image: `{base}/emoji-custom/{name}.{ext}`.
    pub fn emoji_custom(&self, name: &str, extension: &str) -> String {
        format!(
            "{}/emoji-custom/{}.{}",
            self.base,
            encode_segment(name),
            encode_segment(extension)
        )
    }

    /// Absolute URL for an attachment/image link that may be absolute
    /// (`https://…`), protocol-relative (`//…`) or server-relative (`/file-upload/…`).
    /// Server-relative paths that already contain the path prefix are not prefixed twice.
    pub fn absolute(&self, path_or_url: &str) -> String {
        let p = path_or_url.trim();
        if let Some(rest) = p.strip_prefix("//") {
            return format!("{}://{rest}", self.url.scheme());
        }
        // Anything with a scheme (`HTTPS://…`, `https:host/x`, `data:`) is absolute;
        // `owns()` decides whether credentials may be attached.
        if Url::parse(p).is_ok() {
            return p.to_owned();
        }
        let prefix = self.path_prefix();
        if !prefix.is_empty() && (p.starts_with(&format!("{prefix}/")) || p == prefix) {
            return format!("{}{p}", self.origin());
        }
        self.join(p)
    }

    /// Whether `url` (absolute) points to this server (same origin and below the prefix).
    pub fn owns(&self, url: &str) -> bool {
        let Ok(u) = Url::parse(url) else {
            return false;
        };
        crate::rest::same_origin(&u, &self.url)
            && (self.path_prefix().is_empty()
                || u.path() == self.path_prefix()
                || u.path().starts_with(&format!("{}/", self.path_prefix())))
    }

    /// Same scheme, host (lowercase, punycode, no trailing dot) and effective port.
    pub fn same_origin(&self, other: &ServerUrl) -> bool {
        crate::rest::same_origin(&self.url, &other.url)
    }

    /// Strict match of a deep-link `host` value (the server's `Meteor.absoluteUrl()`)
    /// against this server: same origin (no scheme means https) and the same path
    /// prefix (trailing slashes ignored).
    pub fn matches_link_host(&self, host: &str) -> bool {
        let Ok(other) = ServerUrl::parse(host) else {
            return false;
        };
        self.same_origin(&other) && other.path_prefix() == self.path_prefix()
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.base)
    }
}

impl fmt::Debug for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerUrl({})", self.base)
    }
}

impl FromStr for ServerUrl {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

impl Serialize for ServerUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.base)
    }
}

impl<'de> Deserialize<'de> for ServerUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn p(s: &str) -> String {
        ServerUrl::parse(s).unwrap().as_str().to_owned()
    }

    #[test]
    fn normalises_input() {
        assert_eq!(p("chat.example.com"), "https://chat.example.com");
        assert_eq!(
            p("  https://chat.example.com/ "),
            "https://chat.example.com"
        );
        assert_eq!(p("http://localhost:3000"), "http://localhost:3000");
        assert_eq!(p("https://example.com/chat/"), "https://example.com/chat");
        assert_eq!(p("https://example.com/chat//"), "https://example.com/chat");
        assert_eq!(
            p("HTTPS://Chat.Example.COM:443/"),
            "https://chat.example.com"
        );
        assert_eq!(p("https://example.com/?x=1#y"), "https://example.com");
        assert_eq!(p("localhost:3000"), "https://localhost:3000");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(ServerUrl::parse("").is_err());
        assert!(ServerUrl::parse("ftp://example.com").is_err());
        assert!(ServerUrl::parse("ws://example.com").is_err());
        assert!(ServerUrl::parse("https://").is_err());
        assert!(ServerUrl::parse("https://u:p@example.com").is_err());
    }

    #[test]
    fn builders() {
        let s = ServerUrl::parse("https://example.com/chat").unwrap();
        assert_eq!(s.api("me"), "https://example.com/chat/api/v1/me");
        assert_eq!(
            s.api("/rooms.get?x=1"),
            "https://example.com/chat/api/v1/rooms.get?x=1"
        );
        assert_eq!(s.api_root("info"), "https://example.com/chat/api/info");
        assert_eq!(s.websocket(), "wss://example.com/chat/websocket");
        assert_eq!(s.host(), "example.com/chat");
        assert_eq!(s.origin(), "https://example.com");
        assert_eq!(
            s.avatar_user("john doe", Some(64)),
            "https://example.com/chat/avatar/john%20doe?size=64"
        );
        assert_eq!(
            s.avatar_room("GENERAL", None),
            "https://example.com/chat/avatar/room/GENERAL"
        );
        assert_eq!(
            s.emoji_custom("party", "gif"),
            "https://example.com/chat/emoji-custom/party.gif"
        );
        assert_eq!(
            s.absolute("/file-upload/abc/x.png"),
            "https://example.com/chat/file-upload/abc/x.png"
        );
        assert_eq!(
            s.absolute("/chat/file-upload/abc/x.png"),
            "https://example.com/chat/file-upload/abc/x.png"
        );
        assert_eq!(
            s.absolute("https://cdn.example.org/a.png"),
            "https://cdn.example.org/a.png"
        );
        assert_eq!(
            s.absolute("//cdn.example.org/a.png"),
            "https://cdn.example.org/a.png"
        );
        assert!(s.owns("https://example.com/chat/file-upload/x"));
        assert!(!s.owns("https://example.com/other"));
        assert!(!s.owns("https://evil.com/chat/x"));

        let l = ServerUrl::parse("http://localhost:3000").unwrap();
        assert_eq!(l.websocket(), "ws://localhost:3000/websocket");
        assert_eq!(l.host(), "localhost:3000");
        assert_eq!(
            l.absolute("file-upload/x"),
            "http://localhost:3000/file-upload/x"
        );
    }

    #[test]
    fn host_matching() {
        let l = ServerUrl::parse("http://localhost:3000").unwrap();
        assert!(l.matches_link_host("http://localhost:3000/"));
        assert!(
            !l.matches_link_host("localhost:3000"),
            "no scheme means https"
        );
        assert!(!l.matches_link_host("http://localhost:3001"));
        let o = ServerUrl::parse("https://chat.corp.com").unwrap();
        for ok in [
            "https://chat.corp.com",
            "chat.corp.com",
            "CHAT.corp.com/",
            "chat.corp.com.",
            "https://chat.corp.com:443",
            "https://chat.corp.com/?x=1#y",
        ] {
            assert!(o.matches_link_host(ok), "{ok}");
        }
        for bad in [
            "http://chat.corp.com",
            "http://chat.corp.com:80/",
            "chat.corp.com:8443",
            "https://chat.corp.com@evil.com",
            "evil.com#@chat.corp.com",
            "https://evil.com\\@chat.corp.com",
            "wss://chat.corp.com",
            "javascript://chat.corp.com",
            "ftp://chat.corp.com",
            "https://chat.corp.com/other",
            "",
        ] {
            assert!(!o.matches_link_host(bad), "{bad}");
        }
        let p = ServerUrl::parse("https://corp.com/chat").unwrap();
        assert!(p.matches_link_host("corp.com/chat/"));
        assert!(p.matches_link_host("https://corp.com/chat"));
        assert!(!p.matches_link_host("https://corp.com"));
        assert!(
            !ServerUrl::parse("https://corp.com")
                .unwrap()
                .matches_link_host("https://corp.com/chat")
        );
        assert!(!p.matches_link_host("corp.com/other"));
        assert!(o.same_origin(&ServerUrl::parse("chat.corp.com.").unwrap()));
        let idn = ServerUrl::parse("https://bücher.example").unwrap();
        assert!(idn.matches_link_host("https://xn--bcher-kva.example"));
    }

    #[test]
    fn absolute_is_safe() {
        let s = ServerUrl::parse("https://chat.corp.com").unwrap();
        for (input, owned) in [
            ("HTTPS://evil.com/x", false),
            ("https:evil.com/x", false),
            ("//evil.com/x", false),
            (" https://evil.com", false),
            ("/file-upload/x", true),
            ("\\\\evil.com/x", true),
            ("http://chat.corp.com/x", false),
            ("https://chat.corp.com/x", true),
        ] {
            let abs = s.absolute(input);
            assert_eq!(s.owns(&abs), owned, "{input} -> {abs}");
        }
    }
}
