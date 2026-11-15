//! Destination filter for anonymous downloads of URLs taken from message content.
//!
//! Such a URL (or a redirect from it) may name loopback, LAN, link-local or cloud
//! metadata addresses that only the user's machine can reach. A destination is
//! allowed when it is no more local than the workspace itself: a public workspace
//! reaches public addresses only, a workspace on a private network also reaches
//! private ones, a workspace on loopback reaches everything.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::sync::OnceCell;
use url::{Host, Url};

use crate::server_url::ServerUrl;

/// Time limit of the DNS lookup that classifies the workspace host.
const WORKSPACE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How local an address is, least local first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reach {
    Public,
    /// Private networks: RFC 1918, CGNAT, unique-local and site-local IPv6.
    Private,
    /// Loopback, link-local, unspecified, multicast, reserved, cloud metadata.
    Local,
}

fn classify_v4(ip: Ipv4Addr) -> Reach {
    let [a, b, c, d] = ip.octets();
    // Metadata services outside the link-local range (Alibaba, Oracle).
    if [a, b, c, d] == [100, 100, 100, 200] || [a, b, c, d] == [192, 0, 0, 192] {
        return Reach::Local;
    }
    match (a, b) {
        (0 | 127 | 224..=255, _) | (169, 254) => Reach::Local,
        (10, _) | (172, 16..=31) | (192, 168) | (100, 64..=127) | (198, 18..=19) => Reach::Private,
        // 192.0.0.0/24, IETF protocol assignments.
        (192, 0) if c == 0 => Reach::Private,
        _ => Reach::Public,
    }
}

fn embedded_v4(hi: u16, lo: u16) -> Ipv4Addr {
    let [a, b] = hi.to_be_bytes();
    let [c, d] = lo.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

fn classify_v6(ip: Ipv6Addr) -> Reach {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    let s = ip.segments();
    // `::/96`: unspecified, loopback and the deprecated IPv4-compatible range.
    if s[..6].iter().all(|&x| x == 0) {
        return Reach::Local;
    }
    // AWS IMDS over IPv6.
    if s == [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254] {
        return Reach::Local;
    }
    // NAT64 (well-known and local-use prefixes) and 6to4 embed an IPv4 address.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        return classify_v4(embedded_v4(s[6], s[7]));
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 1 {
        return classify_v4(embedded_v4(s[6], s[7])).max(Reach::Private);
    }
    if s[0] == 0x2002 {
        return classify_v4(embedded_v4(s[1], s[2]));
    }
    let first = s[0];
    if first & 0xffc0 == 0xfe80 || first & 0xff00 == 0xff00 {
        // Link-local, multicast.
        Reach::Local
    } else if first & 0xffc0 == 0xfec0 || first & 0xfe00 == 0xfc00 {
        // Site-local, unique-local.
        Reach::Private
    } else {
        Reach::Public
    }
}

pub(crate) fn classify(ip: IpAddr) -> Reach {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

/// Host name as used for comparisons: lowercase, no trailing dot.
fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn is_localhost_name(host: &str) -> bool {
    let h = normalize_host(host);
    h == "localhost" || h.ends_with(".localhost")
}

/// Filter shared by the foreign download path (URL checks) and the DNS resolver
/// of its HTTP client (resolved addresses).
pub(crate) struct DestinationGuard {
    workspace_https: bool,
    /// Workspace host as in its URL (`url::Url::host_str`), normalized.
    workspace_host: String,
    /// How local the workspace is; unset until its host name was resolved.
    reach: OnceCell<Reach>,
    /// Hosts of requests in flight. Only these are filtered by the resolver, so
    /// that the name of an HTTP proxy on a private address still resolves.
    watched: Mutex<HashMap<String, usize>>,
}

impl DestinationGuard {
    pub(crate) fn new(server: &ServerUrl) -> Self {
        let url = server.url();
        let known = match url.host() {
            Some(Host::Ipv4(ip)) => Some(classify_v4(ip)),
            Some(Host::Ipv6(ip)) => Some(classify_v6(ip)),
            Some(Host::Domain(d)) if is_localhost_name(d) => Some(Reach::Local),
            Some(Host::Domain(_)) => None,
            None => Some(Reach::Public),
        };
        Self {
            workspace_https: url.scheme() == "https",
            workspace_host: normalize_host(url.host_str().unwrap_or_default()),
            reach: OnceCell::new_with(known),
            watched: Mutex::new(HashMap::new()),
        }
    }

    /// The most local class of destination that may be contacted. Resolves the
    /// workspace host on first use; while that fails only public addresses pass.
    pub(crate) async fn allowed_reach(&self) -> Reach {
        let host = self.workspace_host.as_str();
        let lookup = || async move {
            let addrs =
                tokio::time::timeout(WORKSPACE_LOOKUP_TIMEOUT, tokio::net::lookup_host((host, 0)))
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
            addrs.map(|a| classify(a.ip())).max().ok_or(())
        };
        self.reach
            .get_or_try_init(lookup)
            .await
            .copied()
            .unwrap_or(Reach::Public)
    }

    fn current_reach(&self) -> Reach {
        self.reach.get().copied().unwrap_or(Reach::Public)
    }

    fn is_workspace_host(&self, host: &str) -> bool {
        !self.workspace_host.is_empty() && normalize_host(host) == self.workspace_host
    }

    /// Validates one hop of an anonymous download and returns the URL to request,
    /// without userinfo (reqwest would send it as Basic credentials). Refuses
    /// schemes other than http(s), plain http when the workspace is https, and
    /// hosts that are literally more local than `allowed`. Host names are checked
    /// again when they are resolved ([`FilteringResolver`]).
    pub(crate) fn check(&self, mut url: Url, allowed: Reach) -> Result<Url, String> {
        match url.scheme() {
            "https" => {}
            "http" if !self.workspace_https => {}
            "http" => return Err("plain http from an https workspace".into()),
            other => return Err(format!("unsupported scheme: {other}")),
        }
        let reach = match url.host() {
            Some(Host::Ipv4(ip)) => classify_v4(ip),
            Some(Host::Ipv6(ip)) => classify_v6(ip),
            Some(Host::Domain(d)) if is_localhost_name(d) => Reach::Local,
            Some(Host::Domain(d)) if !d.is_empty() => Reach::Public,
            _ => return Err("missing host".into()),
        };
        let host = url.host_str().unwrap_or_default().to_owned();
        if reach > allowed && !self.is_workspace_host(&host) {
            return Err(format!("{host}: destination not allowed"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            let stripped = url.set_username("").is_ok() && url.set_password(None).is_ok();
            if !stripped {
                return Err(format!("{host}: credentials in the URL"));
            }
        }
        Ok(url)
    }

    /// Marks the host of `url` as a download destination until the returned
    /// value is dropped. Hold it while the request is being sent.
    pub(crate) fn watch(&self, url: &Url) -> Watch<'_> {
        let host = normalize_host(url.host_str().unwrap_or_default());
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        *watched.entry(host.clone()).or_default() += 1;
        Watch { guard: self, host }
    }

    fn is_watched(&self, host: &str) -> bool {
        self.watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(host)
    }

    /// Addresses of `name` that may be connected to: all of them for the
    /// workspace host and for names that are not download destinations (a
    /// proxy), otherwise those that are not more local than the workspace.
    fn filter(&self, name: &str, addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
        let host = normalize_host(name);
        if self.is_workspace_host(&host) || !self.is_watched(&host) {
            return addrs;
        }
        let allowed = self.current_reach();
        addrs
            .into_iter()
            .filter(|a| classify(a.ip()) <= allowed)
            .collect()
    }
}

/// Registration of a host with [`DestinationGuard::watch`].
pub(crate) struct Watch<'a> {
    guard: &'a DestinationGuard,
    host: String,
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        let mut watched = self
            .guard
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = watched.get_mut(&self.host) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                watched.remove(&self.host);
            }
        }
    }
}

/// System DNS resolution that drops addresses refused by the guard, so the check
/// applies to the addresses actually connected to (no DNS rebinding window).
pub(crate) struct FilteringResolver(pub(crate) Arc<DestinationGuard>);

impl Resolve for FilteringResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let guard = Arc::clone(&self.0);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let had_any = !resolved.is_empty();
            let addrs = guard.filter(&host, resolved);
            if had_any && addrs.is_empty() {
                tracing::debug!(%host, "download destination resolves to a local address");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "destination not allowed",
                )
                .into());
            }
            Ok::<Addrs, Box<dyn std::error::Error + Send + Sync>>(Box::new(addrs.into_iter()))
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn guard(server: &str) -> DestinationGuard {
        DestinationGuard::new(&ServerUrl::parse(server).unwrap())
    }

    fn check(g: &DestinationGuard, url: &str, allowed: Reach) -> Result<Url, String> {
        g.check(Url::parse(url).unwrap(), allowed)
    }

    #[test]
    fn address_classes() {
        for (ip, reach) in [
            ("8.8.8.8", Reach::Public),
            ("93.184.216.34", Reach::Public),
            ("172.32.0.1", Reach::Public),
            ("100.128.0.1", Reach::Public),
            ("10.1.2.3", Reach::Private),
            ("172.16.0.1", Reach::Private),
            ("172.31.255.255", Reach::Private),
            ("192.168.1.1", Reach::Private),
            ("100.64.0.1", Reach::Private),
            ("198.18.0.1", Reach::Private),
            ("127.0.0.1", Reach::Local),
            ("127.255.255.254", Reach::Local),
            ("0.0.0.0", Reach::Local),
            ("169.254.169.254", Reach::Local),
            ("100.100.100.200", Reach::Local),
            ("192.0.0.192", Reach::Local),
            ("224.0.0.1", Reach::Local),
            ("255.255.255.255", Reach::Local),
            ("2606:4700:4700::1111", Reach::Public),
            ("::1", Reach::Local),
            ("::", Reach::Local),
            ("::7f00:1", Reach::Local),
            ("::ffff:127.0.0.1", Reach::Local),
            ("::ffff:192.168.0.1", Reach::Private),
            ("::ffff:8.8.8.8", Reach::Public),
            ("64:ff9b::7f00:1", Reach::Local),
            ("64:ff9b::808:808", Reach::Public),
            ("2002:7f00:1::1", Reach::Local),
            ("2002:c0a8:1::1", Reach::Private),
            ("fe80::1", Reach::Local),
            ("ff02::1", Reach::Local),
            ("fc00::1", Reach::Private),
            ("fd12:3456::1", Reach::Private),
            ("fd00:ec2::254", Reach::Local),
            ("fec0::1", Reach::Private),
        ] {
            assert_eq!(classify(ip.parse().unwrap()), reach, "{ip}");
        }
    }

    #[test]
    fn public_workspace_refuses_local_destinations() {
        let g = guard("https://chat.example.com");
        for url in [
            "https://127.0.0.1:8080/admin/shutdown",
            "https://192.168.1.1/cgi-bin/reboot",
            "https://10.0.0.5/x.png",
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/x",
            "https://[::ffff:127.0.0.1]/x",
            "https://[fd00::1]/x",
            "https://[fe80::1]/x",
            "https://localhost/x",
            "https://LOCALHOST./x",
            "https://dev.localhost:3000/x",
            // Alternative spellings of 127.0.0.1.
            "https://2130706433/x",
            "https://0x7f.1/x",
            "https://0177.0.0.1/x",
            "https://0.0.0.0/x",
        ] {
            assert!(check(&g, url, Reach::Public).is_err(), "{url}");
        }
        assert!(check(&g, "https://cdn.example.org/a.png", Reach::Public).is_ok());
        assert!(check(&g, "https://8.8.8.8/a.png", Reach::Public).is_ok());
    }

    #[test]
    fn https_workspace_refuses_plain_http_and_other_schemes() {
        let g = guard("https://chat.example.com");
        assert!(check(&g, "http://cdn.example.org/a.png", Reach::Public).is_err());
        assert!(check(&g, "http://192.168.1.1/apply.cgi", Reach::Local).is_err());
        assert!(check(&g, "ftp://cdn.example.org/a.png", Reach::Local).is_err());
        assert!(check(&g, "file:///etc/passwd", Reach::Local).is_err());
        assert!(check(&g, "data:image/png;base64,AAAA", Reach::Local).is_err());
        let h = guard("http://chat.example.com");
        assert!(check(&h, "http://cdn.example.org/a.png", Reach::Public).is_ok());
        assert!(check(&h, "http://192.168.1.1/apply.cgi", Reach::Public).is_err());
    }

    #[test]
    fn userinfo_is_stripped() {
        let g = guard("https://chat.example.com");
        let u = check(
            &g,
            "https://admin:secret@cdn.example.org/a.png?x=1",
            Reach::Public,
        )
        .unwrap();
        assert_eq!(u.as_str(), "https://cdn.example.org/a.png?x=1");
        let u = check(&g, "https://admin@cdn.example.org/a.png", Reach::Public).unwrap();
        assert_eq!(u.as_str(), "https://cdn.example.org/a.png");
        assert!(
            check(
                &g,
                "https://admin:secret@192.168.0.1/apply.cgi",
                Reach::Public
            )
            .is_err()
        );
    }

    #[test]
    fn private_workspace_reaches_private_but_not_local() {
        let g = guard("http://10.0.0.2:3000");
        assert_eq!(g.current_reach(), Reach::Private);
        let allowed = g.current_reach();
        assert!(check(&g, "http://192.168.1.1/a.png", allowed).is_ok());
        assert!(check(&g, "http://cdn.example.org/a.png", allowed).is_ok());
        assert!(check(&g, "http://127.0.0.1:8080/x", allowed).is_err());
        assert!(check(&g, "http://169.254.169.254/x", allowed).is_err());
        // The workspace's own host always passes.
        assert!(check(&g, "http://10.0.0.2:3000/api/v1/info", Reach::Public).is_ok());
    }

    #[test]
    fn loopback_workspace_reaches_everything() {
        for server in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            let g = guard(server);
            assert_eq!(g.current_reach(), Reach::Local, "{server}");
            assert!(check(&g, "http://127.0.0.1:9000/bucket/x", Reach::Local).is_ok());
        }
        // Not resolved yet: a named workspace is public until the lookup ran.
        assert_eq!(
            guard("https://chat.example.com").current_reach(),
            Reach::Public
        );
    }

    #[test]
    fn resolver_filter_covers_watched_hosts_only() {
        let g = guard("https://chat.example.com");
        let addrs = |list: &[&str]| -> Vec<SocketAddr> {
            list.iter()
                .map(|ip| SocketAddr::new(ip.parse().unwrap(), 0))
                .collect()
        };
        let mixed = addrs(&["127.0.0.1", "93.184.216.34", "::1", "10.0.0.1"]);
        // A name nobody downloads from (an HTTP proxy) is not filtered.
        assert_eq!(g.filter("proxy.corp", mixed.clone()), mixed);
        let target = Url::parse("https://Rebind.Evil.example./x").unwrap();
        {
            let _watch = g.watch(&target);
            let _again = g.watch(&target);
            assert_eq!(
                g.filter("rebind.evil.example.", mixed.clone()),
                addrs(&["93.184.216.34"])
            );
            assert!(
                g.filter("rebind.evil.example", addrs(&["127.0.0.1"]))
                    .is_empty()
            );
        }
        assert_eq!(g.filter("rebind.evil.example", mixed.clone()), mixed);
        // The workspace host is never filtered.
        let own = Url::parse("https://chat.example.com/api/v1/info").unwrap();
        let _watch = g.watch(&own);
        assert_eq!(g.filter("chat.example.com", mixed.clone()), mixed);
    }

    #[tokio::test]
    async fn download_refuses_local_destinations() {
        use crate::error::Error;
        // An IP-literal workspace needs no DNS lookup to be classified.
        let tls = crate::tls::TlsConfig::new().build().unwrap();
        let c =
            crate::rest::RestClient::new(ServerUrl::parse("https://203.0.113.5").unwrap(), &tls)
                .unwrap();
        for url in [
            "http://192.168.1.1/cgi-bin/reboot",
            "http://127.0.0.1:8080/admin/shutdown",
            "https://127.0.0.1:8080/admin/shutdown",
            "https://admin:secret@192.168.0.1/apply.cgi",
            "https://[::1]:8080/x",
            "https://localhost:8080/x",
            "http://cdn.example.org/a.png",
        ] {
            let e = c.download(url).await.err().unwrap();
            assert!(matches!(e, Error::Url(_)), "{url}: {e:?}");
        }
    }
}
