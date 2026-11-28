//! The macOS managed preferences source (docs/policy.md sections 2, 4 and
//! 5): values forced in the app's own preference domain
//! ([`crate::BUNDLE_ID`]) by a configuration profile, with full trust.
//!
//! Only forced names are read: a value that is present but not forced
//! (`defaults write`, a launch argument) is ignored entirely. Names are
//! matched exactly. Forced names of the domain that are not policy keys are
//! reported as unknown; they are found in the defaults' dictionary
//! representation. A binary that runs outside the app bundle (`cargo run`,
//! a binary copied out of the `.app`) sees no managed preferences: its
//! defaults search another domain first, so a forced name's value could come
//! from a launch argument; such a process reads nothing.
//!
//! The decoding is platform-neutral behind [`ManagedPrefs`] and tested with
//! a fake; only the adapter over `NSUserDefaults` is macOS-only.

use std::collections::BTreeSet;

use rustket_core::policy::{RawValue, Snapshot, SourceId, specs};

/// A preference value, as the adapter reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefValue {
    /// An `NSNumber` (a plist boolean is one too), by its `double` and
    /// `long long` values.
    Number {
        f64: f64,
        i64: i64,
    },
    String(String),
    Array(Vec<PrefValue>),
    /// Any other class (a dictionary, data, a date); the text names it.
    Other(String),
}

/// The preferences of the app.
pub trait ManagedPrefs {
    /// Names to examine besides the policy keys (the names the defaults
    /// know).
    fn names(&self) -> Vec<String>;
    /// Whether `name` is forced in the app's own domain.
    fn is_forced(&self, name: &str) -> bool;
    /// Whether `name` is forced in the global domain. The global domain is
    /// part of every domain's search list, so a name forced there by another
    /// profile also answers "forced" in the app's domain.
    fn is_forced_globally(&self, _name: &str) -> bool {
        false
    }
    fn value(&self, name: &str) -> Option<PrefValue>;
}

/// The snapshot of the forced preferences.
pub fn read_managed(prefs: &impl ManagedPrefs) -> Snapshot {
    let id = SourceId::MacManaged;
    let trust = id.default_trust();
    let mut snap = Snapshot::new(id, trust);
    let keys: BTreeSet<String> = specs().iter().map(|s| s.name.to_owned()).collect();
    let names: BTreeSet<String> = keys.iter().cloned().chain(prefs.names()).collect();
    for name in names {
        if !prefs.is_forced(&name) {
            continue;
        }
        // Another profile's global key is not an unknown Rustket key.
        if !keys.contains(&name) && prefs.is_forced_globally(&name) {
            continue;
        }
        if let Some(value) = prefs.value(&name) {
            snap.values.insert(name, raw(&value));
        }
    }
    snap
}

/// The snapshot of the forced preferences of a process whose bundle has the
/// identifier `bundle_id`: empty unless that is the app's own
/// ([`crate::BUNDLE_ID`]); `prefs` is not consulted then.
pub fn read_in_bundle<P: ManagedPrefs>(
    bundle_id: Option<&str>,
    prefs: impl FnOnce() -> P,
) -> Snapshot {
    if bundle_id != Some(crate::BUNDLE_ID) {
        let id = SourceId::MacManaged;
        let trust = id.default_trust();
        return Snapshot::new(id, trust);
    }
    read_managed(&prefs())
}

/// Section 4's mapping of one value.
fn raw(value: &PrefValue) -> RawValue {
    match value {
        PrefValue::Array(items) => RawValue::List(items.iter().map(item).collect()),
        other => item(other),
    }
}

/// A value that is not an array, or a list item (a nested array is not
/// supported).
fn item(value: &PrefValue) -> RawValue {
    match value {
        PrefValue::Number { f64, i64 } => integral(*f64, *i64).map_or_else(
            || RawValue::Unsupported("non-integral number".into()),
            RawValue::Integer,
        ),
        PrefValue::String(s) => RawValue::String(s.clone()),
        PrefValue::Array(_) => RawValue::Unsupported("nested array".into()),
        PrefValue::Other(kind) => RawValue::Unsupported(kind.clone()),
    }
}

/// The number's integer value when it has one that fits `i64`: its
/// `double` is finite and whole and equals its `long long` (an unsigned
/// value above `i64::MAX` wraps and differs).
fn integral(f: f64, i: i64) -> Option<i64> {
    // 2^63: the first double above every i64.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let whole = f.is_finite() && f.fract() == 0.0 && (-LIMIT..LIMIT).contains(&f);
    let same = i as f64 == f;
    (whole && same).then_some(i)
}

/// The macOS snapshot. Runs inside an autorelease pool: the refresh reads
/// off the main thread.
#[cfg(target_os = "macos")]
pub fn read_macos() -> Snapshot {
    objc2::rc::autoreleasepool(|_| {
        let bundle = objc2_foundation::NSBundle::mainBundle().bundleIdentifier();
        let bundle = bundle.map(|id| id.to_string());
        read_in_bundle(bundle.as_deref(), os::Defaults::standard)
    })
}

/// The thin adapter over `NSUserDefaults`: safe calls only.
#[cfg(target_os = "macos")]
mod os {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{
        NSArray, NSData, NSDate, NSDictionary, NSNumber, NSString, NSUserDefaults,
    };

    use super::{ManagedPrefs, PrefValue};

    pub struct Defaults {
        defaults: Retained<NSUserDefaults>,
        domain: Retained<NSString>,
    }

    impl Defaults {
        pub fn standard() -> Self {
            Self {
                defaults: NSUserDefaults::standardUserDefaults(),
                domain: NSString::from_str(crate::BUNDLE_ID),
            }
        }
    }

    impl ManagedPrefs for Defaults {
        fn names(&self) -> Vec<String> {
            self.defaults
                .dictionaryRepresentation()
                .allKeys()
                .to_vec()
                .iter()
                .map(|k| k.to_string())
                .collect()
        }

        fn is_forced(&self, name: &str) -> bool {
            self.defaults
                .objectIsForcedForKey_inDomain(&NSString::from_str(name), &self.domain)
        }

        fn is_forced_globally(&self, name: &str) -> bool {
            // The value of the `NSGlobalDomain` constant.
            self.defaults.objectIsForcedForKey_inDomain(
                &NSString::from_str(name),
                &NSString::from_str("NSGlobalDomain"),
            )
        }

        fn value(&self, name: &str) -> Option<PrefValue> {
            self.defaults
                .objectForKey(&NSString::from_str(name))
                .map(|obj| value(&obj))
        }
    }

    fn value(obj: &AnyObject) -> PrefValue {
        if let Some(n) = obj.downcast_ref::<NSNumber>() {
            PrefValue::Number {
                f64: n.as_f64(),
                i64: n.as_i64(),
            }
        } else if let Some(s) = obj.downcast_ref::<NSString>() {
            PrefValue::String(s.to_string())
        } else if let Some(a) = obj.downcast_ref::<NSArray>() {
            PrefValue::Array(a.to_vec().iter().map(|o| value(o)).collect())
        } else if obj.downcast_ref::<NSDictionary>().is_some() {
            PrefValue::Other("dictionary".into())
        } else if obj.downcast_ref::<NSData>().is_some() {
            PrefValue::Other("data".into())
        } else if obj.downcast_ref::<NSDate>().is_some() {
            PrefValue::Other("date".into())
        } else {
            PrefValue::Other(obj.class().name().to_string_lossy().into_owned())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use rustket_core::policy::{Context, Os, RowStatus, Trust, resolve};

    use super::*;

    /// Preferences by name: value, forced in the app's domain.
    #[derive(Default)]
    struct Fake {
        prefs: BTreeMap<String, (PrefValue, bool)>,
        /// Forced, but only in another domain.
        forced_elsewhere: BTreeSet<String>,
        /// Forced in the global domain (so also "forced" in the app's).
        forced_globally: BTreeSet<String>,
    }

    impl Fake {
        fn forced(mut self, name: &str, v: PrefValue) -> Self {
            self.prefs.insert(name.into(), (v, true));
            self
        }

        fn free(mut self, name: &str, v: PrefValue) -> Self {
            self.prefs.insert(name.into(), (v, false));
            self
        }
    }

    impl ManagedPrefs for Fake {
        fn names(&self) -> Vec<String> {
            self.prefs
                .keys()
                .chain(&self.forced_elsewhere)
                .cloned()
                .collect()
        }

        fn is_forced(&self, name: &str) -> bool {
            self.prefs.get(name).is_some_and(|(_, f)| *f) || self.forced_globally.contains(name)
        }

        fn is_forced_globally(&self, name: &str) -> bool {
            self.forced_globally.contains(name)
        }

        fn value(&self, name: &str) -> Option<PrefValue> {
            self.prefs.get(name).map(|(v, _)| v.clone())
        }
    }

    fn num(f: f64) -> PrefValue {
        let i = f as i64;
        PrefValue::Number { f64: f, i64: i }
    }

    fn st(s: &str) -> PrefValue {
        PrefValue::String(s.into())
    }

    fn s(v: &str) -> RawValue {
        RawValue::String(v.into())
    }

    #[test]
    fn forced_and_not_forced() {
        let snap = read_managed(
            &Fake::default()
                .forced("RequireHttps", num(1.0))
                .free("AllowUserServers", num(0.0)),
        );
        assert_eq!(snap.id, SourceId::MacManaged);
        assert_eq!(snap.trust, Trust::Full);
        assert_eq!(snap.error, None);
        assert_eq!(
            snap.values.into_iter().collect::<Vec<_>>(),
            [("RequireHttps".to_owned(), RawValue::Integer(1))]
        );
    }

    /// A name forced only in the global domain (another profile's
    /// `AppleShowScrollBars`) is not reported as an unknown key; a policy key
    /// forced there still counts, and so does an unknown name forced in the
    /// app's domain.
    #[test]
    fn globally_forced_names_are_not_unknown_keys() {
        let mut fake = Fake::default()
            .forced("SomeFutureKey", num(1.0))
            .free("AppleShowScrollBars", st("Always"))
            .free("RequireHttps", num(1.0));
        fake.forced_globally.insert("AppleShowScrollBars".into());
        fake.forced_globally.insert("RequireHttps".into());
        let snap = read_managed(&fake);
        assert_eq!(
            snap.values.keys().map(String::as_str).collect::<Vec<_>>(),
            ["RequireHttps", "SomeFutureKey"]
        );
    }

    #[test]
    fn numbers() {
        let snap = read_managed(
            &Fake::default()
                .forced("AllowTokenLogin", num(0.0))
                .forced("AllowPasswordLogin", num(1.0))
                .forced("LaunchAtLogin", num(1.5))
                .forced("NotificationPreview", num(-3.0))
                .forced("RequireHttps", num(f64::NAN))
                .forced(
                    "AllowUserServers",
                    // An unsigned long long above i64::MAX: its long long wraps.
                    PrefValue::Number {
                        f64: 1.8e19,
                        i64: -1,
                    },
                ),
        );
        let v = &snap.values;
        assert_eq!(v["AllowTokenLogin"], RawValue::Integer(0));
        assert_eq!(v["AllowPasswordLogin"], RawValue::Integer(1));
        assert_eq!(v["NotificationPreview"], RawValue::Integer(-3));
        for name in ["LaunchAtLogin", "RequireHttps", "AllowUserServers"] {
            assert!(matches!(v[name], RawValue::Unsupported(_)), "{name}");
        }
        assert_eq!(integral(i64::MAX as f64, i64::MAX), None, "2^63 is out");
        assert_eq!(integral(-(2f64.powi(63)), i64::MIN), Some(i64::MIN));
        assert_eq!(integral(f64::INFINITY, i64::MAX), None);
        // Booleans coerce from 1 and 0.
        let p = resolve(&[snap], &ctx());
        assert!(!p.allow_token_login());
        assert!(p.allow_password_login());
    }

    fn ctx() -> Context {
        Context {
            client_version: "0.4.0".into(),
            os: Os::MacOs,
            mas: false,
            self_update: false,
        }
    }

    #[test]
    fn strings_and_arrays() {
        let snap = read_managed(
            &Fake::default()
                .forced("OrganizationName", st("Example"))
                .forced(
                    "AllowedServers",
                    PrefValue::Array(vec![st("a.com"), st("b.com")]),
                )
                .forced(
                    "ManagedServers",
                    PrefValue::Array(vec![
                        st("https://a.com"),
                        num(7.0),
                        PrefValue::Array(vec![st("x")]),
                    ]),
                ),
        );
        let v = &snap.values;
        assert_eq!(v["OrganizationName"], s("Example"));
        assert_eq!(
            v["AllowedServers"],
            RawValue::List(vec![s("a.com"), s("b.com")])
        );
        assert_eq!(
            v["ManagedServers"],
            RawValue::List(vec![
                s("https://a.com"),
                RawValue::Integer(7),
                RawValue::Unsupported("nested array".into()),
            ])
        );
        let p = resolve(&[snap], &ctx());
        assert_eq!(p.managed_servers().len(), 1);
    }

    #[test]
    fn other_kinds_are_unsupported() {
        let snap = read_managed(
            &Fake::default()
                .forced("AllowedServers", PrefValue::Other("dictionary".into()))
                .forced("SupportUrl", PrefValue::Other("date".into())),
        );
        assert_eq!(
            snap.values["AllowedServers"],
            RawValue::Unsupported("dictionary".into())
        );
        let p = resolve(&[snap], &ctx());
        assert!(matches!(
            p.rows()[0].status,
            RowStatus::Invalid {
                strict_applied: true,
                ..
            }
        ));
        assert_eq!(p.allowed_servers(), Some(&[][..]));
    }

    #[test]
    fn unknown_forced_names_are_reported() {
        let snap = read_managed(
            &Fake::default()
                .forced("RequireHTTPS", num(1.0))
                .free("SomethingElse", st("x")),
        );
        assert_eq!(
            snap.values.keys().collect::<Vec<_>>(),
            ["RequireHTTPS"],
            "matched exactly, unknown"
        );
        let p = resolve(&[snap], &ctx());
        assert_eq!(p.rows()[0].status, RowStatus::Unknown);
        assert!(!p.require_https());
    }

    #[test]
    fn forced_only_outside_the_domain_is_ignored() {
        let mut fake = Fake::default().free("RequireHttps", num(1.0));
        fake.forced_elsewhere.insert("RequireHttps".into());
        fake.forced_elsewhere.insert("OtherAppKey".into());
        assert!(read_managed(&fake).values.is_empty());
    }

    /// Policy keys are examined even when the defaults do not list them.
    #[test]
    fn known_keys_without_a_listing() {
        struct Unlisted;
        impl ManagedPrefs for Unlisted {
            fn names(&self) -> Vec<String> {
                Vec::new()
            }
            fn is_forced(&self, name: &str) -> bool {
                name == "RequireHttps"
            }
            fn value(&self, _: &str) -> Option<PrefValue> {
                Some(num(1.0))
            }
        }
        let snap = read_managed(&Unlisted);
        assert_eq!(snap.values.len(), 1);
        assert_eq!(snap.values["RequireHttps"], RawValue::Integer(1));
    }

    #[test]
    fn outside_the_app_bundle_nothing_is_read() {
        let fake = || Fake::default().forced("UpdateMode", st("prompt"));
        for bundle in [
            None,
            Some("rustket"),
            Some("io.github.sokolcloud.rustket.other"),
        ] {
            let snap = read_in_bundle(bundle, fake);
            assert_eq!(snap.id, SourceId::MacManaged, "{bundle:?}");
            assert!(snap.values.is_empty(), "{bundle:?}");
            assert_eq!(snap.error, None);
        }
        let snap = read_in_bundle(Some(crate::BUNDLE_ID), fake);
        assert_eq!(snap.values["UpdateMode"], s("prompt"));
    }

    /// The real reader on a Mac (CI runs it on the macOS job), read-only.
    /// A test binary runs outside the app bundle, so `read_macos` yields an
    /// empty snapshot; the adapter over `NSUserDefaults` is run directly
    /// too. Values a profile on the machine forces are tolerated.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_reader_runs_read_only() {
        let check = |snap: &Snapshot| {
            assert_eq!(snap.id, SourceId::MacManaged);
            assert_eq!(snap.trust, Trust::Full);
            assert_eq!(snap.error, None);
            assert!(snap.trust_problems.is_empty());
        };
        let snap = read_macos();
        check(&snap);
        assert!(snap.values.is_empty(), "outside the bundle: {snap:?}");
        let snap = objc2::rc::autoreleasepool(|_| read_managed(&os::Defaults::standard()));
        check(&snap);
        let _ = resolve(&[snap], &ctx());
    }
}
