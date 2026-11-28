//! Resolution tests (docs/policy.md sections 5 and 6).

#![allow(clippy::unwrap_used)]

use super::*;

fn ctx() -> Context {
    Context {
        client_version: "0.4.0".into(),
        os: Os::Linux,
        mas: false,
        self_update: true,
    }
}

fn s(v: &str) -> RawValue {
    RawValue::String(v.into())
}

fn list(items: &[&str]) -> RawValue {
    RawValue::List(items.iter().map(|i| s(i)).collect())
}

fn src(name: &str, trust: Trust) -> Snapshot {
    Snapshot::new(SourceId::Test(name.into()), trust)
}

fn full(name: &str) -> Snapshot {
    src(name, Trust::Full)
}

/// Rows as (key, source, status) for compact assertions.
fn rows(p: &Policy) -> Vec<(String, String, &'static str)> {
    p.rows()
        .iter()
        .map(|r| (r.key.clone(), r.source.to_string(), r.status.as_str()))
        .collect()
}

fn r(key: &str, source: &str, status: &'static str) -> (String, String, &'static str) {
    (key.into(), format!("test:{source}"), status)
}

#[test]
fn empty_policy() {
    let p = resolve(&[], &ctx());
    assert!(p.has_no_settings());
    assert!(!p.is_blocked());
    assert!(p.rows().is_empty());
    assert_eq!(p.client_version(), "0.4.0");
    let d = Policy::default();
    assert!(d.has_no_settings());
    assert!(d.allow_user_servers());
    assert!(d.allow_certificate_exceptions());
    assert!(d.allow_password_login());
    assert!(d.allow_token_login());
    assert!(!d.require_https());
    assert_eq!(d.allowed_servers(), None);
    assert!(d.managed_servers().is_empty());
    assert_eq!(d.update_mode(), None);
    assert_eq!(d.allow_secrets_file(), None);
    assert_eq!(d.notification_preview(), None);
    assert_eq!(d.hardware_acceleration(), None);
    assert_eq!(d.launch_at_login(), None);
    assert_eq!(d.organization_name(), None);
    assert_eq!(d.support_url(), None);
    assert_eq!(d.minimum_version(), None);
    // No sources, and a source without values, set nothing: the same
    // settings as the default, and no report rows.
    assert!(p.settings_eq(&d));
    let one = resolve(&[full("a")], &ctx());
    assert!(one.settings_eq(&d));
    assert!(one.rows().is_empty());
    assert!(!one.is_blocked());
}

#[test]
fn policy_is_send_sync_clone_eq() {
    fn assert_traits<T: Send + Sync + Clone + PartialEq + Default>() {}
    assert_traits::<Policy>();
    let a = resolve(
        &[full("a").with("RequireHttps", RawValue::Bool(true))],
        &ctx(),
    );
    let b = resolve(
        &[full("a").with("RequireHttps", RawValue::Bool(true))],
        &ctx(),
    );
    let c = resolve(
        &[full("a").with("RequireHttps", RawValue::Bool(false))],
        &ctx(),
    );
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.clone(), a);
}

#[test]
fn precedence_and_report() {
    let high = full("high")
        .with("RequireHttps", RawValue::Bool(true))
        .with("AllowedServers", list(&["a.example"]));
    let mid = full("mid")
        .with("RequireHttps", RawValue::Bool(false))
        .with("AllowUserServers", RawValue::Integer(0))
        .with("AllowedServers", list(&["b.example", "c.example"]));
    let low = full("low")
        .with("RequireHttps", RawValue::Integer(0))
        .with("AllowUserServers", RawValue::Bool(true))
        .with("LaunchAtLogin", RawValue::Bool(true));
    let p = resolve(&[high, mid, low], &ctx());
    assert!(p.require_https());
    assert!(!p.allow_user_servers());
    assert_eq!(p.launch_at_login(), Some(true));
    // Lists are replaced, never merged.
    let hosts: Vec<&str> = p
        .allowed_servers()
        .unwrap()
        .iter()
        .map(|p| p.host.as_str())
        .collect();
    assert_eq!(hosts, ["a.example"]);
    // Registry order, then source priority.
    assert_eq!(
        rows(&p),
        [
            r("AllowUserServers", "mid", "applied"),
            r("AllowUserServers", "low", "overridden"),
            r("AllowedServers", "high", "applied"),
            r("AllowedServers", "mid", "overridden"),
            r("RequireHttps", "high", "applied"),
            r("RequireHttps", "mid", "overridden"),
            r("RequireHttps", "low", "overridden"),
            r("LaunchAtLogin", "low", "applied"),
        ]
    );
    assert_eq!(p.rows()[0].value, "0");
    assert_eq!(p.rows()[3].value, "[\"b.example\", \"c.example\"]");
    let sources: Vec<String> = p.sources().iter().map(|s| s.id.to_string()).collect();
    assert_eq!(sources, ["test:high", "test:mid", "test:low"]);
}

#[test]
fn limited_trust() {
    let user = src("user", Trust::Limited)
        .with("ManagedServers", list(&["https://evil.example"]))
        .with("SupportUrl", s("https://evil.example/help"))
        .with("RequireHttps", RawValue::Bool(true))
        .with("OrganizationName", s("User org"));
    let below = full("below")
        .with("ManagedServers", list(&["https://chat.corp.com"]))
        .with("SupportUrl", s("https://help.corp.com"))
        .with("RequireHttps", RawValue::Bool(false));
    let p = resolve(&[user.clone(), below], &ctx());
    // Class T from a limited source: ignored, the next source is considered.
    assert_eq!(p.managed_servers().len(), 1);
    assert_eq!(p.managed_servers()[0].url.as_str(), "https://chat.corp.com");
    assert_eq!(p.support_url(), Some("https://help.corp.com/"));
    // Other classes apply from a limited source.
    assert!(p.require_https());
    assert_eq!(p.organization_name(), Some("User org"));
    assert_eq!(
        rows(&p),
        [
            r("ManagedServers", "user", "ignored_limited_trust"),
            r("ManagedServers", "below", "applied"),
            r("RequireHttps", "user", "applied"),
            r("RequireHttps", "below", "overridden"),
            r("OrganizationName", "user", "applied"),
            r("SupportUrl", "user", "ignored_limited_trust"),
            r("SupportUrl", "below", "applied"),
        ]
    );
    // Alone, the limited source sets no T key.
    let p = resolve(&[user], &ctx());
    assert!(p.managed_servers().is_empty());
    assert!(!p.is_set(Key::ManagedServers));
    assert_eq!(p.support_url(), None);
}

#[test]
fn strict_values() {
    // Every key with a strict value, set to something invalid in the top
    // source, with a valid permissive value below that must not be used.
    let cases: [(&str, RawValue, RawValue); 9] = [
        ("AllowUserServers", s("no"), RawValue::Bool(true)),
        ("AllowedServers", RawValue::Integer(1), list(&["a.example"])),
        ("RequireHttps", RawValue::Integer(2), RawValue::Bool(false)),
        (
            "AllowCertificateExceptions",
            s("false"),
            RawValue::Bool(true),
        ),
        ("UpdateMode", s("never"), s("prompt")),
        (
            "AllowPasswordLogin",
            RawValue::List(vec![]),
            RawValue::Bool(true),
        ),
        (
            "AllowTokenLogin",
            RawValue::Unsupported("float".into()),
            RawValue::Bool(true),
        ),
        ("AllowSecretsFile", s(""), RawValue::Bool(true)),
        (
            "NotificationPreview",
            RawValue::Integer(-1),
            RawValue::Bool(true),
        ),
    ];
    for (name, bad, good) in cases {
        let key = Key::from_name(name).unwrap();
        assert!(key.spec().strict.is_some(), "{name}");
        let p = resolve(
            &[
                full("top").with(name, bad.clone()),
                full("low").with(name, good),
            ],
            &ctx(),
        );
        assert!(p.is_set(key), "{name}");
        match &p.rows()[0].status {
            RowStatus::Invalid {
                strict_applied: true,
                reason,
            } => assert!(!reason.to_string().is_empty()),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(p.rows()[1].status, RowStatus::Overridden, "{name}");
    }
    let invalid = |name: &str, bad: RawValue| resolve(&[full("top").with(name, bad)], &ctx());
    assert!(!invalid("AllowUserServers", s("no")).allow_user_servers());
    assert_eq!(
        invalid("AllowedServers", RawValue::Integer(1)).allowed_servers(),
        Some(&[][..])
    );
    assert!(invalid("RequireHttps", RawValue::Integer(2)).require_https());
    assert!(!invalid("AllowCertificateExceptions", s("x")).allow_certificate_exceptions());
    assert_eq!(
        invalid("UpdateMode", s("never")).update_mode(),
        Some(UpdateMode::Off)
    );
    assert!(!invalid("AllowPasswordLogin", s("x")).allow_password_login());
    assert!(!invalid("AllowTokenLogin", s("x")).allow_token_login());
    assert_eq!(
        invalid("AllowSecretsFile", s("x")).allow_secrets_file(),
        Some(false)
    );
    assert_eq!(
        invalid("NotificationPreview", s("x")).notification_preview(),
        Some(false)
    );
}

#[test]
fn ignored_invalid_values_stop_the_search() {
    let cases: [(&str, RawValue, RawValue); 6] = [
        (
            "ManagedServers",
            RawValue::Bool(true),
            list(&["https://a.example"]),
        ),
        ("HardwareAcceleration", s("yes"), RawValue::Bool(false)),
        ("LaunchAtLogin", RawValue::Integer(5), RawValue::Bool(true)),
        ("OrganizationName", s("   "), s("Acme")),
        (
            "SupportUrl",
            s("http://help.example"),
            s("https://help.example"),
        ),
        ("MinimumVersion", s("1.0"), s("9.0.0")),
    ];
    for (name, bad, good) in cases {
        let key = Key::from_name(name).unwrap();
        assert!(key.spec().strict.is_none(), "{name}");
        let p = resolve(
            &[full("top").with(name, bad), full("low").with(name, good)],
            &ctx(),
        );
        assert!(!p.is_set(key), "{name}: nothing applied");
        assert!(
            matches!(
                p.rows()[0].status,
                RowStatus::Invalid {
                    strict_applied: false,
                    ..
                }
            ),
            "{name}"
        );
        assert_eq!(
            p.rows()[1].status,
            RowStatus::Overridden,
            "{name}: not consulted"
        );
        assert!(!p.is_blocked(), "{name}");
    }
}

#[test]
fn string_key_validation() {
    let one = |name: &str, v: RawValue| resolve(&[full("a").with(name, v)], &ctx());
    assert_eq!(
        one("OrganizationName", s("  Acme Corp ")).organization_name(),
        Some("Acme Corp")
    );
    assert_eq!(
        one("OrganizationName", s(&"x".repeat(80)))
            .organization_name()
            .map(str::len),
        Some(80)
    );
    assert!(!one("OrganizationName", s(&"x".repeat(81))).is_set(Key::OrganizationName));
    for bad in [
        "Acme\nCorp",
        "Acme\u{2028}Corp",
        "Acme\u{2029}Corp",
        "Acme\u{202E}Corp",
        "Acme\u{2067}Corp",
        "Acme\u{200F}Corp",
    ] {
        let p = one("OrganizationName", s(bad));
        assert!(!p.is_set(Key::OrganizationName), "{bad:?}");
        assert!(matches!(
            p.rows()[0].status,
            RowStatus::Invalid {
                reason: InvalidReason::NotSingleLine,
                ..
            }
        ));
    }
    assert_eq!(
        one("OrganizationName", s(&"я".repeat(80)))
            .organization_name()
            .map(|n| n.chars().count()),
        Some(80)
    );
    // The stored value is the normalised URL that was validated.
    for (raw, stored) in [
        ("https://help.corp.com", "https://help.corp.com/"),
        (
            "https://help.corp.com/page?x=1",
            "https://help.corp.com/page?x=1",
        ),
        ("https:help.corp.com", "https://help.corp.com/"),
        ("HTTPS://Help.Corp.COM/a", "https://help.corp.com/a"),
    ] {
        assert_eq!(
            one("SupportUrl", s(raw)).support_url(),
            Some(stored),
            "{raw}"
        );
    }
    for bad in [
        "http://help.corp.com",
        "help.corp.com",
        "/help",
        "mailto:a@b.c",
        "https://",
        "https://help.example\n.evil.com",
        "https://help.\texample",
    ] {
        assert!(!one("SupportUrl", s(bad)).is_set(Key::SupportUrl), "{bad}");
    }
    assert_eq!(
        one("MinimumVersion", s(" 1.2.3 ")).minimum_version(),
        Some(Version {
            major: 1,
            minor: 2,
            patch: 3
        })
    );
}

#[test]
fn version_parsing() {
    let v = |s: &str| Version::parse(s);
    assert_eq!(
        v("0.4.10"),
        Some(Version {
            major: 0,
            minor: 4,
            patch: 10
        })
    );
    assert_eq!(v("01.0.0").unwrap().major, 1);
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "1.2.3-beta",
        "1.2.3+b",
        "v1.2.3",
        "+1.2.3",
        "1.-2.3",
        "1..3",
        "1.2.",
        " 1.2.3",
        "1.2.x",
        "99999999999999999999.0.0",
    ] {
        assert_eq!(v(bad), None, "{bad}");
    }
    assert!(v("0.10.0") > v("0.9.9"));
    assert!(v("1.0.0") > v("0.99.99"));
    assert_eq!(v("1.2.3").unwrap().to_string(), "1.2.3");
}

#[test]
fn minimum_version_blocks() {
    let with = |min: &str, client: &str| {
        resolve(
            &[full("a").with("MinimumVersion", s(min))],
            &Context {
                client_version: client.into(),
                ..ctx()
            },
        )
    };
    let p = with("0.5.0", "0.4.0");
    assert_eq!(
        p.blocking(),
        [Blocking::BelowMinimumVersion {
            required: Version::parse("0.5.0").unwrap(),
            client: "0.4.0".into(),
        }]
    );
    assert!(!with("0.4.0", "0.4.0").is_blocked());
    assert!(!with("0.3.9", "0.4.0").is_blocked());
    assert!(with("0.4.1", "0.4.0").is_blocked());
    // Semver order: a pre-release is older than its release, build
    // metadata is ignored.
    assert!(with("9.0.0", "0.4.0-dev").is_blocked());
    let p = with("0.5.0", "0.5.0-rc.1");
    assert_eq!(
        p.blocking(),
        [Blocking::BelowMinimumVersion {
            required: Version::parse("0.5.0").unwrap(),
            client: "0.5.0-rc.1".into(),
        }]
    );
    assert!(with("0.6.0", "0.5.0-rc.2").is_blocked());
    assert!(!with("0.4.0", "0.5.0-rc.1").is_blocked());
    assert!(!with("0.5.0", "0.5.0+build-1").is_blocked());
    assert!(with("0.5.1", "0.5.0+build").is_blocked());
    // A client version without a numeric core never blocks.
    assert!(!with("9.0.0", "").is_blocked());
    // An invalid MinimumVersion never blocks.
    assert!(!with("9.0", "0.4.0").is_blocked());
}

#[test]
fn unreadable_source_blocks_but_others_resolve() {
    let denied = SourceError::new(SourceErrorKind::CannotOpen).with_detail("access denied");
    let p = resolve(
        &[
            Snapshot::unreadable(SourceId::WindowsMachine, denied.clone()),
            src("user", Trust::Limited).with("RequireHttps", RawValue::Bool(true)),
        ],
        &ctx(),
    );
    assert_eq!(
        p.blocking(),
        [Blocking::SourceUnreadable {
            source: SourceId::WindowsMachine,
            error: denied.clone(),
        }]
    );
    assert!(p.require_https());
    assert_eq!(rows(&p), [r("RequireHttps", "user", "applied")]);
    assert_eq!(p.sources()[0].error.as_ref(), Some(&denied));
    assert_eq!(denied.to_string(), "cannot open: access denied");
    assert_eq!(denied.kind.code(), "cannot_open");
    assert_eq!(p.sources()[1].trust, Trust::Limited);

    // Both blocking states at once.
    let p = resolve(
        &[
            full("a").with("MinimumVersion", s("1.0.0")),
            Snapshot::unreadable(
                SourceId::LinuxFile("b.json".into()),
                SourceErrorKind::InvalidJson,
            ),
        ],
        &ctx(),
    );
    assert_eq!(p.blocking().len(), 2);
    assert!(matches!(p.blocking()[0], Blocking::SourceUnreadable { .. }));
    assert!(matches!(
        p.blocking()[1],
        Blocking::BelowMinimumVersion { .. }
    ));
}

#[test]
fn unknown_and_not_applicable() {
    let a = full("a")
        .with("FutureKey", RawValue::Bool(true))
        .with("requirehttps", RawValue::Bool(true))
        .with("UpdateMode", s("off"))
        .with("HardwareAcceleration", RawValue::Bool(false));
    let b = full("b")
        .with("FutureKey", s("x"))
        .with("UpdateMode", s("prompt"));
    let mas = Context {
        os: Os::MacOs,
        mas: true,
        self_update: false,
        ..ctx()
    };
    let p = resolve(&[a.clone(), b.clone()], &mas);
    assert!(!p.require_https(), "lookup is case-sensitive");
    assert_eq!(p.update_mode(), None);
    assert_eq!(p.hardware_acceleration(), None);
    assert!(p.has_no_settings());
    assert_eq!(
        rows(&p),
        [
            r("UpdateMode", "a", "not_applicable"),
            r("UpdateMode", "b", "not_applicable"),
            r("HardwareAcceleration", "a", "not_applicable"),
            r("FutureKey", "a", "unknown"),
            r("FutureKey", "b", "unknown"),
            r("requirehttps", "a", "unknown"),
        ]
    );
    // Without the updater only UpdateMode is not applicable.
    let p = resolve(
        &[a.clone(), b.clone()],
        &Context {
            self_update: false,
            ..ctx()
        },
    );
    assert_eq!(p.update_mode(), None);
    assert_eq!(p.hardware_acceleration(), Some(false));
    // With it, the highest source wins.
    let p = resolve(&[a, b], &ctx());
    assert_eq!(p.update_mode(), Some(UpdateMode::Off));
}

#[test]
fn list_items_are_reported() {
    let p = resolve(
        &[full("a")
            .with(
                "ManagedServers",
                RawValue::List(vec![
                    s("https://chat.corp.com Corp"),
                    s("chat.other.com"),
                    RawValue::Integer(3),
                    s("https://CHAT.corp.com/"),
                    s("https://chat.corp.com Corp"),
                ]),
            )
            .with("AllowedServers", list(&["*.corp.com", "a*.b", "corp.com"]))],
        &ctx(),
    );
    let managed = p.managed_servers();
    assert_eq!(managed.len(), 1);
    assert_eq!(managed[0].name.as_deref(), Some("Corp"));
    let row = &p.rows()[0];
    assert_eq!(row.status, RowStatus::Applied);
    assert_eq!(row.dropped.len(), 3, "{:?}", row.dropped);
    // Positions refer to the list as written, in order.
    assert_eq!(row.dropped[0].position, 2);
    assert_eq!(row.dropped[0].item, "\"chat.other.com\"");
    assert_eq!(
        row.dropped[0].reason,
        DropReason::Server(ServerItemError::MissingScheme)
    );
    assert_eq!(row.dropped[1].position, 3);
    assert_eq!(
        row.dropped[1].to_string(),
        "item 3: expected a string, found integer"
    );
    assert_eq!(row.dropped[2].position, 4);
    assert_eq!(row.dropped[2].reason, DropReason::DuplicateServer);
    assert_eq!(row.dropped_omitted, 0);
    assert_eq!(p.allowed_servers().unwrap().len(), 2);
    assert_eq!(p.rows()[1].dropped.len(), 1);
    // A list whose items are all bad stays applied, empty.
    let p = resolve(
        &[full("a").with("AllowedServers", RawValue::List(vec![RawValue::Bool(true)]))],
        &ctx(),
    );
    assert_eq!(p.allowed_servers(), Some(&[][..]));
    assert_eq!(p.rows()[0].status, RowStatus::Applied);
}

#[test]
fn values_shown_truncated() {
    let long = "x".repeat(1000);
    let p = resolve(&[full("a").with("Whatever", s(&long))], &ctx());
    assert_eq!(p.rows()[0].value.chars().count(), DISPLAY_MAX_CHARS);
}

#[test]
fn reasons_and_names_are_truncated() {
    let long = "x".repeat(10_000);
    let p = resolve(
        &[full("a")
            .with("UpdateMode", s(&long))
            .with("ManagedServers", list(&[&format!("https://{long}%zz")]))
            .with(&long, RawValue::Bool(true))],
        &ctx(),
    );
    let reason = match &p.rows()[1].status {
        RowStatus::Invalid { reason, .. } => reason.to_string(),
        other => panic!("{other:?}"),
    };
    assert!(reason.chars().count() < 200, "{reason}");
    let managed = &p.rows()[0];
    assert_eq!(managed.key, "ManagedServers");
    assert!(managed.dropped[0].item.chars().count() <= 80);
    assert!(managed.dropped[0].to_string().chars().count() < 200);
    let unknown = &p.rows()[2];
    assert_eq!(unknown.status, RowStatus::Unknown);
    assert_eq!(unknown.key.chars().count(), DISPLAY_MAX_CHARS);
    let dump = dump_json(&p).unwrap();
    assert!(dump.len() < 4000, "{} bytes", dump.len());
}

#[test]
fn dropped_items_are_capped() {
    let items: Vec<RawValue> = (0..1000).map(|_| RawValue::Integer(1)).collect();
    let p = resolve(
        &[full("a").with("AllowedServers", RawValue::List(items))],
        &ctx(),
    );
    let row = &p.rows()[0];
    assert_eq!(row.dropped.len(), MAX_DROPPED_REPORTED);
    assert_eq!(row.dropped_omitted, 1000 - MAX_DROPPED_REPORTED);
}

#[test]
fn unreadable_user_hive_has_limited_trust() {
    let mut hkcu = Snapshot::unreadable(SourceId::WindowsUser, SourceErrorKind::CannotRead);
    hkcu.values
        .insert("ManagedServers".into(), list(&["https://evil.example"]));
    hkcu.values
        .insert("SupportUrl".into(), s("https://evil.example"));
    assert_eq!(hkcu.trust, Trust::Limited);
    let p = resolve(&[hkcu], &ctx());
    assert!(!p.is_set(Key::ManagedServers));
    assert!(!p.is_set(Key::SupportUrl));
    assert_eq!(p.sources()[0].trust, Trust::Limited);
    assert!(p.sources()[0].trust_problems.is_empty());
    assert_eq!(
        Snapshot::unreadable(SourceId::WindowsMachine, SourceErrorKind::CannotOpen).trust,
        Trust::Full
    );
    assert_eq!(
        Snapshot::unreadable(SourceId::MacManaged, SourceErrorKind::CannotRead).trust,
        Trust::Full
    );
}

/// A valid value for every key, different from the unset behaviour.
fn valid_value(key: Key) -> RawValue {
    match key {
        Key::ManagedServers => list(&["https://chat.corp.com"]),
        Key::AllowedServers => list(&["*.corp.com"]),
        Key::UpdateMode => s("notify"),
        Key::OrganizationName => s("Acme"),
        Key::SupportUrl => s("https://help.corp.com"),
        Key::MinimumVersion => s("0.1.0"),
        Key::AllowUserServers
        | Key::RequireHttps
        | Key::AllowCertificateExceptions
        | Key::AllowPasswordLogin
        | Key::AllowTokenLogin
        | Key::AllowSecretsFile
        | Key::NotificationPreview
        | Key::HardwareAcceleration
        | Key::LaunchAtLogin => RawValue::Bool(false),
    }
}

#[test]
fn changed_keys_covers_every_key() {
    let empty = Policy::default();
    let mut all = full("all");
    for key in Key::ALL {
        let p = resolve(&[full("a").with(key.name(), valid_value(key))], &ctx());
        assert!(p.is_set(key), "{key}");
        assert_eq!(p.changed_keys(&empty), [key], "{key}");
        assert_eq!(empty.changed_keys(&p), [key], "{key}");
        assert!(p.changed_keys(&p.clone()).is_empty());
        assert!(!p.settings_eq(&empty));
        all = all.with(key.name(), valid_value(key));
    }
    let p = resolve(&[all], &ctx());
    assert_eq!(p.changed_keys(&empty), Key::ALL);
    // A different value of the same key.
    let a = resolve(
        &[full("a").with("RequireHttps", RawValue::Bool(true))],
        &ctx(),
    );
    let b = resolve(
        &[full("a").with("RequireHttps", RawValue::Bool(false))],
        &ctx(),
    );
    assert_eq!(a.changed_keys(&b), [Key::RequireHttps]);
    // Only the report differs: no key changed.
    let c = resolve(
        &[
            full("a").with("RequireHttps", RawValue::Bool(true)),
            full("b").with("RequireHttps", RawValue::Bool(false)),
        ],
        &ctx(),
    );
    assert_ne!(a, c);
    assert!(a.settings_eq(&c));
    assert!(a.changed_keys(&c).is_empty());
}

#[test]
fn has_no_settings_ignores_blocking() {
    let p = resolve(
        &[Snapshot::unreadable(
            SourceId::WindowsMachine,
            SourceErrorKind::CannotOpen,
        )],
        &ctx(),
    );
    assert!(p.has_no_settings());
    assert!(p.is_blocked());
}

/// Raw user text never reaches a reason string: a bidirectional override
/// and a line break in a server item stay out of the reason and are escaped
/// in the item's display form.
#[test]
fn server_item_reasons_keep_raw_text_out() {
    let managed = [
        "https://ex\u{202E}ample.com\nName",
        "https://ex\u{202E}am\nple.com",
        "https://[::1\u{202E}\n]",
    ];
    let allowed = [
        "ex\u{202E}ample.com:8\n0",
        "ex\u{202E}am\nple.com",
        "a.example:8\u{202E}\n",
        "[::1\u{202E}\nx",
        "[::1]\u{202E}\n",
        "ft\u{202E}p\n://a.example",
    ];
    let p = resolve(
        &[full("a")
            .with("ManagedServers", list(&managed))
            .with("AllowedServers", list(&allowed))],
        &ctx(),
    );
    let rows = p.rows();
    assert_eq!(rows.len(), 2);
    let mut seen = 0;
    for row in rows {
        for d in &row.dropped {
            seen += 1;
            for text in [d.reason.to_string(), d.to_string(), d.item.clone()] {
                assert!(
                    !text.contains('\u{202E}') && !text.contains('\n'),
                    "{}: {text:?}",
                    row.key
                );
            }
        }
    }
    assert_eq!(seen, managed.len() + allowed.len());
    // The parsers on their own.
    for item in managed {
        let e = ManagedServer::parse(item).unwrap_err().to_string();
        assert!(!e.contains('\u{202E}') && !e.contains('\n'), "{e:?}");
    }
    for item in allowed {
        let e = ServerPattern::parse(item).unwrap_err().to_string();
        assert!(!e.contains('\u{202E}') && !e.contains('\n'), "{e:?}");
    }
}

/// Codes and English texts of source errors and trust problems are pinned;
/// the codes are unique.
#[test]
fn source_error_and_trust_problem_texts() {
    use SourceErrorKind as K;
    use TrustSubject::{Directory, File};
    let kinds = [
        (K::CannotOpen, "cannot_open", "cannot open"),
        (
            K::CannotReadMetadata,
            "cannot_read_metadata",
            "cannot read metadata",
        ),
        (K::CannotRead, "cannot_read", "cannot read"),
        (K::TooLarge, "too_large", "larger than 1 MiB"),
        (K::InvalidJson, "invalid_json", "invalid JSON"),
        (
            K::NotAnObject,
            "not_an_object",
            "the top-level value is not a JSON object",
        ),
        (
            K::CannotListDirectory,
            "cannot_list_directory",
            "cannot list the directory",
        ),
        (K::Unstable, "unstable", "changed during every read"),
    ];
    assert_eq!(MAX_FILE_BYTES, 1 << 20, "the TooLarge text says 1 MiB");
    // An exhaustive match: a new kind does not compile until it is listed.
    let kind_index = |k: K| match k {
        K::CannotOpen => 0,
        K::CannotReadMetadata => 1,
        K::CannotRead => 2,
        K::TooLarge => 3,
        K::InvalidJson => 4,
        K::NotAnObject => 5,
        K::CannotListDirectory => 6,
        K::Unstable => 7,
    };
    let mut listed: Vec<usize> = kinds.iter().map(|(k, ..)| kind_index(*k)).collect();
    listed.sort_unstable();
    assert_eq!(listed, (0..8).collect::<Vec<_>>(), "every kind listed once");
    assert_eq!(
        K::ALL.map(kind_index),
        [0, 1, 2, 3, 4, 5, 6, 7],
        "ALL is complete"
    );
    let mut codes = std::collections::HashSet::new();
    for (kind, code, text) in kinds {
        assert_eq!(kind.code(), code);
        assert_eq!(kind.to_string(), text);
        assert!(codes.insert(code), "{code} repeated");
    }
    assert_eq!(
        SourceError::new(K::CannotRead)
            .with_detail("Permission denied")
            .to_string(),
        "cannot read: Permission denied"
    );
    let problems = [
        (
            TrustProblem::WrongOwner {
                subject: File,
                found: 1000,
                expected: 0,
            },
            "file_wrong_owner",
            "the file is owned by uid 1000, not 0",
        ),
        (
            TrustProblem::WrongOwner {
                subject: Directory,
                found: 5,
                expected: 0,
            },
            "directory_wrong_owner",
            "the directory is owned by uid 5, not 0",
        ),
        (
            TrustProblem::Writable { subject: File },
            "file_writable",
            "the file is writable by group or others",
        ),
        (
            TrustProblem::Writable { subject: Directory },
            "directory_writable",
            "the directory is writable by group or others",
        ),
        (
            TrustProblem::DirectoryChanged,
            "directory_changed",
            "the directory changed while it was read",
        ),
        (
            TrustProblem::NotChecked {
                subject: File,
                detail: None,
            },
            "file_not_checked",
            "the file could not be checked",
        ),
        (
            TrustProblem::NotChecked {
                subject: File,
                detail: Some("No such file".into()),
            },
            "file_not_checked",
            "the file could not be checked: No such file",
        ),
        (
            TrustProblem::NotChecked {
                subject: Directory,
                detail: None,
            },
            "directory_not_checked",
            "the directory could not be checked",
        ),
        (
            TrustProblem::NotChecked {
                subject: Directory,
                detail: Some("x".into()),
            },
            "directory_not_checked",
            "the directory could not be checked: x",
        ),
    ];
    // An exhaustive match over the shapes (variant and subject): a new one
    // does not compile until it is listed.
    let shape_index = |p: &TrustProblem| {
        let sub = |s: &TrustSubject| match s {
            File => 0,
            Directory => 1,
        };
        match p {
            TrustProblem::WrongOwner { subject, .. } => sub(subject),
            TrustProblem::Writable { subject } => 2 + sub(subject),
            TrustProblem::DirectoryChanged => 4,
            TrustProblem::NotChecked { subject, .. } => 5 + sub(subject),
        }
    };
    let mut shapes: Vec<usize> = problems.iter().map(|(p, ..)| shape_index(p)).collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(shapes, (0..7).collect::<Vec<_>>(), "every shape listed");
    let mut problem_codes = std::collections::HashSet::new();
    for (problem, code, text) in &problems {
        assert_eq!(problem.code(), *code);
        assert_eq!(problem.to_string(), *text);
        problem_codes.insert(*code);
    }
    // Seven shapes, seven codes, none shared with a source error.
    assert_eq!(problem_codes.len(), 7);
    assert_eq!(
        problem_codes,
        TrustProblem::CODES.into_iter().collect(),
        "CODES is complete"
    );
    assert!(problem_codes.is_disjoint(&codes));
}

/// The log codes of invalid values are pinned and unique.
#[test]
fn invalid_reason_codes() {
    let reasons = [
        (
            InvalidReason::Coerce(CoerceError::WrongType {
                expected: "boolean",
                found: "string".into(),
            }),
            "wrong_type",
        ),
        (
            InvalidReason::Coerce(CoerceError::NotBoolInteger(2)),
            "not_bool_integer",
        ),
        (
            InvalidReason::Coerce(CoerceError::UnknownVariant {
                value: "sometimes".into(),
                variants: &["prompt"],
            }),
            "unknown_variant",
        ),
        (InvalidReason::Coerce(CoerceError::Empty), "empty"),
        (InvalidReason::NotSingleLine, "not_single_line"),
        (InvalidReason::TooLong { max: 80 }, "too_long"),
        (InvalidReason::NotUrl(url::ParseError::EmptyHost), "not_url"),
        (InvalidReason::NotHttpsUrl, "not_https_url"),
        (InvalidReason::BadVersion, "bad_version"),
        (InvalidReason::Internal(Key::UpdateMode), "internal"),
    ];
    // An exhaustive match: a new reason does not compile until it is listed.
    let index = |r: &InvalidReason| match r {
        InvalidReason::Coerce(CoerceError::WrongType { .. }) => 0,
        InvalidReason::Coerce(CoerceError::NotBoolInteger(_)) => 1,
        InvalidReason::Coerce(CoerceError::UnknownVariant { .. }) => 2,
        InvalidReason::Coerce(CoerceError::Empty) => 3,
        InvalidReason::NotSingleLine => 4,
        InvalidReason::TooLong { .. } => 5,
        InvalidReason::NotUrl(_) => 6,
        InvalidReason::NotHttpsUrl => 7,
        InvalidReason::BadVersion => 8,
        InvalidReason::Internal(_) => 9,
    };
    let mut listed: Vec<usize> = reasons.iter().map(|(r, _)| index(r)).collect();
    listed.sort_unstable();
    assert_eq!(
        listed,
        (0..10).collect::<Vec<_>>(),
        "every reason listed once"
    );
    let mut codes = std::collections::HashSet::new();
    for (reason, code) in &reasons {
        assert_eq!(reason.code(), *code, "{reason:?}");
        assert!(codes.insert(*code), "{code} repeated");
        assert!(
            code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{code}"
        );
    }
    assert_eq!(
        codes,
        InvalidReason::CODES.into_iter().collect(),
        "CODES is complete"
    );
}

/// The codes of dropped list items are pinned and unique; `CODES` lists
/// every one.
#[test]
fn drop_reason_codes() {
    let reasons = [
        (DropReason::Empty, "empty"),
        (
            DropReason::NotAString {
                found: "integer".into(),
            },
            "not_a_string",
        ),
        (
            DropReason::Server(ServerItemError::MissingScheme),
            "missing_scheme",
        ),
        (
            DropReason::Server(ServerItemError::Url("x".into())),
            "invalid_address",
        ),
        (
            DropReason::Server(ServerItemError::Scheme("ftp".into())),
            "unsupported_scheme",
        ),
        (
            DropReason::Server(ServerItemError::ExtraParts),
            "extra_parts",
        ),
        (
            DropReason::Server(ServerItemError::Port("x".into())),
            "invalid_port",
        ),
        (
            DropReason::Server(ServerItemError::Wildcard),
            "bad_wildcard",
        ),
        (DropReason::DuplicateServer, "duplicate_server"),
    ];
    // An exhaustive match: a new reason does not compile until it is listed.
    let index = |r: &DropReason| match r {
        DropReason::Empty => 0,
        DropReason::NotAString { .. } => 1,
        DropReason::Server(e) => match e {
            ServerItemError::MissingScheme => 2,
            ServerItemError::Url(_) => 3,
            ServerItemError::Scheme(_) => 4,
            ServerItemError::ExtraParts => 5,
            ServerItemError::Port(_) => 6,
            ServerItemError::Wildcard => 7,
        },
        DropReason::DuplicateServer => 8,
    };
    let mut listed: Vec<usize> = reasons.iter().map(|(r, _)| index(r)).collect();
    listed.sort_unstable();
    assert_eq!(
        listed,
        (0..9).collect::<Vec<_>>(),
        "every reason listed once"
    );
    let mut codes = std::collections::HashSet::new();
    for (reason, code) in &reasons {
        assert_eq!(reason.code(), *code, "{reason:?}");
        assert!(codes.insert(*code), "{code} repeated");
    }
    assert_eq!(codes, DropReason::CODES.into_iter().collect());
}

/// `RowStatus::CODES` lists every status name.
#[test]
fn row_status_codes() {
    let statuses = [
        RowStatus::Applied,
        RowStatus::Overridden,
        RowStatus::Invalid {
            reason: InvalidReason::BadVersion,
            strict_applied: false,
        },
        RowStatus::Unknown,
        RowStatus::NotApplicable,
        RowStatus::IgnoredLimitedTrust,
    ];
    // An exhaustive match: a new status does not compile until it is listed.
    let index = |s: &RowStatus| match s {
        RowStatus::Applied => 0,
        RowStatus::Overridden => 1,
        RowStatus::Invalid { .. } => 2,
        RowStatus::Unknown => 3,
        RowStatus::NotApplicable => 4,
        RowStatus::IgnoredLimitedTrust => 5,
    };
    assert_eq!(statuses.each_ref().map(index), [0, 1, 2, 3, 4, 5]);
    assert_eq!(statuses.each_ref().map(RowStatus::as_str), RowStatus::CODES);
}

#[test]
fn trust_all_is_complete() {
    // An exhaustive match: a new level does not compile until it is listed.
    let index = |t: Trust| match t {
        Trust::Full => 0,
        Trust::Limited => 1,
    };
    assert_eq!(Trust::ALL.map(index), [0, 1]);
    assert_eq!(Trust::ALL.map(Trust::as_str), ["full", "limited"]);
}

#[test]
fn display_text_keeps_names_on_one_line() {
    assert_eq!(display_text("RequireHttps"), "RequireHttps");
    assert_eq!(display_text("Ключ 鍵 \"q\""), "Ключ 鍵 \"q\"");
    assert_eq!(display_text("Foo\nBar"), "Foo\\u000aBar");
    assert_eq!(display_text("a\u{202E}b.json"), "a\\u202eb.json");
    assert_eq!(display_text("x\u{2028}y\ttab"), "x\\u2028y\\u0009tab");
    // `quote` is JSON escaping plus the same rule.
    assert_eq!(
        RawValue::String("a\u{202E}\n".into()).display(),
        "\"a\\u202e\\n\""
    );
}
