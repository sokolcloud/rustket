//! Which configured servers the policy lets run (docs/policy.md section
//! 9.1): the one place that decides a server's standing, and the reconcile
//! of `ManagedServers` with `config.servers`. Pure: no iced, no I/O.

use rocketchat::ServerUrl;
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, ServerEntry};
use rustket_core::policy::{Policy, ServerAccess};

/// Why the policy suspends a configured server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspension {
    /// `AllowedServers` is set and does not match it.
    NotAllowed,
    /// `RequireHttps` is set and it uses http.
    HttpsRequired,
    /// The policy is in a blocking state (section 6: a source cannot be
    /// read, or the client is older than `MinimumVersion`): no server runs.
    Blocked,
}

/// What the policy makes of a configured server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// It may run; `managed`: one of `ManagedServers` (never removable).
    Allowed { managed: bool },
    /// Listed, never connected, its sign-in and data kept.
    Suspended(Suspension),
}

impl Standing {
    pub fn is_allowed(self) -> bool {
        matches!(self, Standing::Allowed { .. })
    }

    pub fn suspension(self) -> Option<Suspension> {
        match self {
            Standing::Suspended(s) => Some(s),
            Standing::Allowed { .. } => None,
        }
    }
}

/// The standing of server `url` under `policy`. A blocked policy suspends
/// every server, managed ones included (section 9.7).
pub fn standing(policy: &Policy, url: &ServerUrl) -> Standing {
    if policy.is_blocked() {
        return Standing::Suspended(Suspension::Blocked);
    }
    match policy.server_access(url) {
        ServerAccess::Allowed => Standing::Allowed {
            managed: policy.is_managed_server(url),
        },
        ServerAccess::HttpsRequired => Standing::Suspended(Suspension::HttpsRequired),
        ServerAccess::NotAllowed => Standing::Suspended(Suspension::NotAllowed),
    }
}

/// Whether stored certificate pins of server `url` may be used: the policy
/// allows certificate exceptions (section 9.2) and it is not a managed
/// server, whose certificate must be trusted by the OS (section 9.1).
pub fn pins_allowed(policy: &Policy, url: &ServerUrl) -> bool {
    policy.allow_certificate_exceptions() && !policy.is_managed_server(url)
}

/// The standing of a URL typed or received before it is configured: the
/// same check, as a refusal reason.
pub fn refusal(policy: &Policy, url: &ServerUrl) -> Option<Suspension> {
    standing(policy, url).suspension()
}

/// Whether `entry` is the server `url` (normalised URL equality, so case,
/// a default port or a trailing slash make no difference).
pub fn same_server(entry: &ServerEntry, url: &ServerUrl) -> bool {
    ServerUrl::parse(&entry.url).is_ok_and(|u| u.as_str() == url.as_str())
}

/// Adds every `ManagedServers` entry that `config` lacks as an ordinary
/// entry (display name from the policy, else the host) and returns the
/// ids added. Existing entries are used as they are.
pub fn reconcile(policy: &Policy, config: &mut AppConfig) -> Vec<ServerId> {
    let mut added = Vec::new();
    for m in policy.managed_servers() {
        if config.servers.iter().any(|e| same_server(e, &m.url)) {
            continue;
        }
        let name = m.name.clone().unwrap_or_else(|| m.url.host());
        added.push(config.add_server(ServerEntry::new(m.url.as_str(), name)));
    }
    added
}

/// Whether managed server `url` takes the server's site name: its entry's
/// name is still exactly the host, the placeholder the reconcile gives an
/// item without a name (section 9.1). The add-server flow names an entry
/// from the site name when it adds it and never renames it later, so the
/// rule is kept to managed servers; a later name in the policy item does
/// not rename the entry, the site name still does.
pub fn takes_site_name(policy: &Policy, url: &ServerUrl, display_name: &str) -> bool {
    policy.is_managed_server(url) && display_name == url.host()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rustket_core::policy::{RawValue, Snapshot, SourceId, Trust, resolve};

    use super::*;

    fn policy(values: &[(&str, RawValue)]) -> Policy {
        let mut snap = Snapshot::new(SourceId::Test("t".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        resolve(&[snap], &crate::policy::context())
    }

    fn list(items: &[&str]) -> RawValue {
        RawValue::List(
            items
                .iter()
                .map(|s| RawValue::String((*s).into()))
                .collect(),
        )
    }

    fn url(s: &str) -> ServerUrl {
        ServerUrl::parse(s).unwrap()
    }

    #[test]
    fn standing_of_each_kind() {
        let empty = Policy::default();
        assert_eq!(
            standing(&empty, &url("http://a.example")),
            Standing::Allowed { managed: false }
        );
        let p = policy(&[
            ("ManagedServers", list(&["https://m.example Corp"])),
            ("AllowedServers", list(&["*.corp.example"])),
        ]);
        assert_eq!(
            standing(&p, &url("https://M.example/")),
            Standing::Allowed { managed: true }
        );
        assert_eq!(
            standing(&p, &url("https://chat.corp.example")),
            Standing::Allowed { managed: false }
        );
        assert_eq!(
            standing(&p, &url("https://other.example")),
            Standing::Suspended(Suspension::NotAllowed)
        );
        let https = policy(&[
            ("ManagedServers", list(&["http://m.example"])),
            ("RequireHttps", RawValue::Bool(true)),
        ]);
        assert_eq!(
            standing(&https, &url("http://m.example")),
            Standing::Suspended(Suspension::HttpsRequired),
            "managed or not"
        );
        assert_eq!(
            refusal(&https, &url("http://x.example")),
            Some(Suspension::HttpsRequired)
        );
        assert_eq!(refusal(&https, &url("https://x.example")), None);
    }

    #[test]
    fn a_blocked_policy_suspends_every_server() {
        let below = policy(&[
            ("MinimumVersion", RawValue::String("999.0.0".into())),
            ("ManagedServers", list(&["https://m.example"])),
        ]);
        assert!(below.is_blocked());
        for u in ["https://m.example", "https://other.example"] {
            assert_eq!(
                standing(&below, &url(u)),
                Standing::Suspended(Suspension::Blocked),
                "{u}"
            );
        }
        let unreadable = resolve(
            &[Snapshot::unreadable(
                SourceId::Test("t".into()),
                rustket_core::policy::SourceErrorKind::CannotRead,
            )],
            &crate::policy::context(),
        );
        assert_eq!(
            standing(&unreadable, &url("https://a.example")),
            Standing::Suspended(Suspension::Blocked)
        );
    }

    #[test]
    fn pins_need_exceptions_and_an_unmanaged_server() {
        let u = url("https://m.example");
        assert!(pins_allowed(&Policy::default(), &u));
        let managed = policy(&[("ManagedServers", list(&["https://m.example"]))]);
        assert!(!pins_allowed(&managed, &u), "the OS must trust it");
        assert!(pins_allowed(&managed, &url("https://other.example")));
        let off = policy(&[("AllowCertificateExceptions", RawValue::Bool(false))]);
        assert!(!pins_allowed(&off, &url("https://other.example")));
    }

    #[test]
    fn reconcile_adds_once_and_recognises_variants() {
        let p = policy(&[(
            "ManagedServers",
            list(&[
                "https://chat.corp.example Corp chat",
                "https://named.example",
            ]),
        )]);
        let mut cfg = AppConfig::default();
        let added = reconcile(&p, &mut cfg);
        assert_eq!(added.len(), 2);
        assert_eq!(cfg.servers[0].display_name, "Corp chat");
        assert_eq!(cfg.servers[1].display_name, "named.example", "the host");
        assert!(
            reconcile(&p, &mut cfg).is_empty(),
            "a second run adds nothing"
        );

        // Case, default port and trailing slash name the same server.
        let mut cfg = AppConfig::default();
        cfg.add_server(ServerEntry::new("HTTPS://Chat.Corp.Example:443/", "Mine"));
        let added = reconcile(
            &policy(&[("ManagedServers", list(&["https://chat.corp.example"]))]),
            &mut cfg,
        );
        assert!(added.is_empty());
        assert_eq!(cfg.servers.len(), 1);
        assert_eq!(cfg.servers[0].display_name, "Mine", "used as it is");
    }

    #[test]
    fn site_name_only_for_a_managed_entry_still_named_by_host() {
        let p = policy(&[(
            "ManagedServers",
            list(&["https://a.example", "https://b.example Bee"]),
        )]);
        assert!(takes_site_name(&p, &url("https://a.example"), "a.example"));
        assert!(!takes_site_name(&p, &url("https://a.example"), "Renamed"));
        // Added without a name, the item named later: the entry was not
        // renamed by the policy and still takes the site name.
        assert!(takes_site_name(&p, &url("https://b.example"), "b.example"));
        assert!(!takes_site_name(&p, &url("https://b.example"), "Bee"));
        assert!(!takes_site_name(&p, &url("https://c.example"), "c.example"));
    }
}
