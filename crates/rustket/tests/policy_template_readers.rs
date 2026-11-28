//! The generated templates for administrators (docs/policy.md section 14)
//! through the app's real decoding: the registry values Group Policy writes
//! for the ADMX, and the forced settings of the macOS profile, resolve with
//! every key applied.

#![allow(clippy::unwrap_used)]

#[path = "../../rustket-core/tests/admin_templates/mod.rs"]
#[allow(dead_code)]
mod admin_templates;
#[path = "../../rustket-core/tests/admin_templates/xml.rs"]
#[allow(dead_code)]
mod xml;

use admin_templates::{Example, example};
use rustket::policy::managed::{ManagedPrefs, PrefValue, read_managed};
use rustket::policy::registry::{self, RegData, RegError, RegKey, read_source};
use rustket_core::policy::{Context, Os, RowStatus, Snapshot, SourceId, resolve, specs};
use xml::{El, Plist, get, parse_xml, plist_value, single};

fn generated(rel: &str) -> String {
    admin_templates::generate()
        .into_iter()
        .find(|(path, _)| path == rel)
        .unwrap_or_else(|| panic!("{rel} is not generated"))
        .1
}

/// A client that knows every key and is not older than any example
/// `MinimumVersion`.
fn context(os: Os) -> Context {
    let newest = specs()
        .iter()
        .map(|s| s.introduced)
        .max_by_key(|v| {
            v.split('.')
                .map(|n| n.parse::<u64>().unwrap())
                .collect::<Vec<_>>()
        })
        .unwrap();
    Context {
        client_version: newest.into(),
        os,
        mas: false,
        self_update: true,
    }
}

/// Every key applied, none invalid, unknown or dropped, nothing blocking.
fn assert_all_applied(snapshot: Snapshot, os: Os) {
    let policy = resolve(&[snapshot], &context(os));
    assert!(policy.blocking().is_empty(), "{:?}", policy.blocking());
    assert_eq!(policy.rows().len(), specs().len(), "{:?}", policy.rows());
    for row in policy.rows() {
        assert_eq!(row.status, RowStatus::Applied, "{row:?}");
        assert!(row.dropped.is_empty(), "{row:?}");
    }
    for s in specs() {
        assert!(policy.is_set(s.key), "{}", s.name);
    }
}

// --- Windows ------------------------------------------------------------------

/// A key of the fake registry.
#[derive(Debug, Clone, Default)]
struct FakeKey {
    values: Vec<(String, RegData)>,
    subkeys: Vec<(String, FakeKey)>,
}

impl RegKey for FakeKey {
    fn values(&self) -> Result<Vec<(String, RegData)>, RegError> {
        Ok(self.values.clone())
    }

    fn subkeys(&self) -> Result<Vec<String>, RegError> {
        Ok(self.subkeys.iter().map(|(n, _)| n.clone()).collect())
    }

    fn open(&self, name: &str) -> Result<Self, RegError> {
        self.subkeys
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, k)| k.clone())
            .ok_or(RegError {
                not_found: true,
                detail: None,
            })
    }
}

/// The policy key as Group Policy leaves it with `policy` enabled and the
/// example value entered, read from the ADMX only: the bool's
/// `enabledValue`, the enum item of the example, the text, and the list
/// items as numbered values in the list element's subkey.
fn write_enabled(root: &mut FakeKey, policy: &El) {
    let name = policy.attr("name").unwrap();
    assert_eq!(policy.attr("key"), Some(registry::POLICY_KEY), "{name}");
    let spec = specs().iter().find(|s| s.name == name).unwrap();
    if let Some(value_name) = policy.attr("valueName") {
        let enabled = policy.child("enabledValue").child("decimal");
        let v: u32 = enabled.attr("value").unwrap().parse().unwrap();
        root.values.push((value_name.into(), RegData::Dword(v)));
        return;
    }
    let elements = policy.child("elements");
    assert_eq!(elements.children.len(), 1, "{name}");
    let e = &elements.children[0];
    match (e.name.as_str(), example(spec.key)) {
        ("enum", Example::Str(want)) => {
            let item = e
                .children("item")
                .into_iter()
                .find(|i| i.child("value").child("string").text == want)
                .unwrap_or_else(|| panic!("{name}: no item {want}"));
            let v = item.child("value").child("string").text.clone();
            root.values
                .push((e.attr("valueName").unwrap().into(), RegData::Sz(v)));
        }
        ("text", Example::Str(text)) => {
            root.values.push((
                e.attr("valueName").unwrap().into(),
                RegData::Sz(text.into()),
            ));
        }
        ("list", Example::List(items)) => {
            let key = e.attr("key").unwrap();
            let sub = key
                .strip_prefix(registry::POLICY_KEY)
                .and_then(|k| k.strip_prefix('\\'))
                .unwrap_or_else(|| panic!("{name}: {key} is not below the policy key"));
            assert!(!sub.contains('\\'), "{name}: {key}");
            let prefix = e.attr("valuePrefix").unwrap();
            let values = items
                .iter()
                .enumerate()
                .map(|(i, v)| (format!("{prefix}{}", i + 1), RegData::Sz((*v).into())))
                .collect();
            root.subkeys.push((
                sub.into(),
                FakeKey {
                    values,
                    subkeys: Vec::new(),
                },
            ));
        }
        (other, ex) => panic!("{name}: element {other} with example {ex:?}"),
    }
}

/// Every policy of the generated ADMX enabled, in `HKLM` (the class T keys
/// are `Machine` only), through the real registry decoding.
#[test]
fn admx_enabled_with_examples_resolves_through_the_registry_reader() {
    let admx = parse_xml(&generated(admin_templates::ADMX));
    let policies = admx.child("policies").children("policy");
    assert_eq!(policies.len(), specs().len());
    let mut root = FakeKey::default();
    for p in policies {
        write_enabled(&mut root, p);
    }
    let snap = read_source(SourceId::WindowsMachine, || Ok(root.clone()));
    assert!(snap.error.is_none(), "{:?}", snap.error);
    assert_all_applied(snap, Os::Windows);
}

// --- macOS --------------------------------------------------------------------

/// Managed preferences that report every key of the profile as forced, with
/// the values as `NSUserDefaults` hands them over (a plist boolean is a
/// number).
struct ForcedPrefs(Vec<(String, Plist)>);

fn pref(v: &Plist) -> PrefValue {
    match v {
        Plist::Bool(b) => PrefValue::Number {
            f64: f64::from(u8::from(*b)),
            i64: i64::from(*b),
        },
        Plist::Integer(i) => PrefValue::Number {
            f64: *i as f64,
            i64: *i,
        },
        Plist::String(s) => PrefValue::String(s.clone()),
        Plist::Array(items) => PrefValue::Array(items.iter().map(pref).collect()),
        Plist::Dict(_) => PrefValue::Other("NSDictionary".into()),
    }
}

impl ManagedPrefs for ForcedPrefs {
    fn names(&self) -> Vec<String> {
        self.0.iter().map(|(k, _)| k.clone()).collect()
    }

    fn is_forced(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k == name)
    }

    fn value(&self, name: &str) -> Option<PrefValue> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| pref(v))
    }
}

/// The forced dictionary of the generated profile through the real managed
/// preferences decoding.
#[test]
fn mobileconfig_resolves_through_the_managed_preferences_reader() {
    let root = parse_xml(&generated(admin_templates::MOBILECONFIG));
    let profile = plist_value(root.child("dict"));
    let payload = single(get(&profile, "PayloadContent"));
    let domain = get(get(payload, "PayloadContent"), admin_templates::DOMAIN);
    assert_eq!(admin_templates::DOMAIN, rustket::BUNDLE_ID);
    let forced = single(get(domain, "Forced"));
    let Plist::Dict(settings) = get(forced, "mcx_preference_settings") else {
        panic!("mcx_preference_settings is not a dictionary")
    };
    assert_eq!(settings.len(), specs().len());
    let snap = read_managed(&ForcedPrefs(settings.clone()));
    assert!(snap.error.is_none(), "{:?}", snap.error);
    assert_all_applied(snap, Os::MacOs);
}
