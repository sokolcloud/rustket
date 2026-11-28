//! The key registry (docs/policy.md sections 3, 8 and 13): every policy key
//! with its type, class, strict value, applicability and refresh class.
//! Templates and the diagnostics page are generated from [`specs`].

use std::fmt;

use super::Context;

/// A policy key of set A. The discriminant is the index into [`specs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    ManagedServers,
    AllowUserServers,
    AllowedServers,
    RequireHttps,
    AllowCertificateExceptions,
    UpdateMode,
    AllowPasswordLogin,
    AllowTokenLogin,
    AllowSecretsFile,
    NotificationPreview,
    HardwareAcceleration,
    LaunchAtLogin,
    OrganizationName,
    SupportUrl,
    MinimumVersion,
}

impl Key {
    /// Every key, in registry order.
    pub const ALL: [Key; 15] = [
        Key::ManagedServers,
        Key::AllowUserServers,
        Key::AllowedServers,
        Key::RequireHttps,
        Key::AllowCertificateExceptions,
        Key::UpdateMode,
        Key::AllowPasswordLogin,
        Key::AllowTokenLogin,
        Key::AllowSecretsFile,
        Key::NotificationPreview,
        Key::HardwareAcceleration,
        Key::LaunchAtLogin,
        Key::OrganizationName,
        Key::SupportUrl,
        Key::MinimumVersion,
    ];

    pub fn spec(self) -> &'static KeySpec {
        &SPECS[self as usize]
    }

    /// The permanent PascalCase name.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// Exact, case-sensitive lookup.
    pub fn from_name(name: &str) -> Option<Key> {
        SPECS.iter().find(|s| s.name == name).map(|s| s.key)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Values of the `UpdateMode` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateMode {
    /// Check on schedule, install when the user clicks.
    Prompt,
    /// Report a new version, never install.
    Notify,
    /// No check, no network request.
    Off,
}

impl UpdateMode {
    /// Variant names as written in a policy (compared ASCII case-insensitively).
    pub const NAMES: &'static [&'static str] = &["prompt", "notify", "off"];

    pub fn as_str(self) -> &'static str {
        match self {
            UpdateMode::Prompt => "prompt",
            UpdateMode::Notify => "notify",
            UpdateMode::Off => "off",
        }
    }

    /// Exact lower-case name (coercion has already canonicalised the case).
    pub fn from_name(name: &str) -> Option<UpdateMode> {
        match name {
            "prompt" => Some(UpdateMode::Prompt),
            "notify" => Some(UpdateMode::Notify),
            "off" => Some(UpdateMode::Off),
            _ => None,
        }
    }
}

/// The type a key's raw value is coerced to (section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Bool,
    /// One of the listed lower-case variants.
    Enum(&'static [&'static str]),
    String,
    StringList,
}

impl ValueType {
    pub fn name(self) -> &'static str {
        match self {
            ValueType::Bool => "bool",
            ValueType::Enum(_) => "enum",
            ValueType::String => "string",
            ValueType::StringList => "string list",
        }
    }

    /// The name with an enum's variants: `enum(prompt, notify, off)`.
    pub fn describe(self) -> String {
        match self {
            ValueType::Enum(variants) => format!("enum({})", variants.join(", ")),
            other => other.name().to_owned(),
        }
    }
}

/// Key class (section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// R: limits the user.
    Restrictive,
    /// T: adds something the user will trust; limited sources may not set it.
    TrustExpanding,
    /// C: cosmetic.
    Cosmetic,
}

impl Class {
    /// The letter used in the specification's table.
    pub fn letter(self) -> char {
        match self {
            Class::Restrictive => 'R',
            Class::TrustExpanding => 'T',
            Class::Cosmetic => 'C',
        }
    }
}

/// The value applied when the deciding source sets a key to something invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictValue {
    Bool(bool),
    UpdateMode(UpdateMode),
    EmptyList,
}

/// Operating system of the running client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl Os {
    pub const ALL: &'static [Os] = &[Os::Windows, Os::MacOs, Os::Linux, Os::Other];

    /// The OS this binary was built for.
    pub fn current() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Os::Windows => "windows",
            Os::MacOs => "macos",
            Os::Linux => "linux",
            Os::Other => "other",
        }
    }
}

/// Where a key applies (section 3). A key that does not apply is reported
/// and otherwise ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applicability {
    pub oses: &'static [Os],
    /// Applies to the Mac App Store flavor.
    pub mas: bool,
    /// Applies only to builds with the `self-update` feature.
    pub needs_self_update: bool,
}

impl Applicability {
    const EVERYWHERE: Applicability = Applicability {
        oses: Os::ALL,
        mas: true,
        needs_self_update: false,
    };

    pub fn applies(&self, ctx: &Context) -> bool {
        self.oses.contains(&ctx.os)
            && (self.mas || !ctx.mas)
            && (!self.needs_self_update || ctx.self_update)
    }
}

/// When a changed value takes effect (section 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshClass {
    Live,
    NextStart,
    NextSignIn,
}

impl RefreshClass {
    pub fn as_str(self) -> &'static str {
        match self {
            RefreshClass::Live => "live",
            RefreshClass::NextStart => "next start",
            RefreshClass::NextSignIn => "next sign-in",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub key: Key,
    pub name: &'static str,
    pub value_type: ValueType,
    pub class: Class,
    /// `None`: an invalid value is ignored and lower sources are not consulted.
    pub strict: Option<StrictValue>,
    pub applicability: Applicability,
    /// The client version that introduced the key.
    pub introduced: &'static str,
    pub refresh: RefreshClass,
}

/// Version that introduced set A.
const SET_A: &str = "0.4.0";

const fn spec(
    key: Key,
    name: &'static str,
    value_type: ValueType,
    class: Class,
    strict: Option<StrictValue>,
) -> KeySpec {
    KeySpec {
        key,
        name,
        value_type,
        class,
        strict,
        applicability: Applicability::EVERYWHERE,
        introduced: SET_A,
        refresh: RefreshClass::Live,
    }
}

static SPECS: [KeySpec; 15] = {
    use Class::{Cosmetic as C, Restrictive as R, TrustExpanding as T};
    use StrictValue::Bool as B;
    use ValueType::{Bool, StringList};
    [
        spec(Key::ManagedServers, "ManagedServers", StringList, T, None),
        spec(
            Key::AllowUserServers,
            "AllowUserServers",
            Bool,
            R,
            Some(B(false)),
        ),
        spec(
            Key::AllowedServers,
            "AllowedServers",
            StringList,
            R,
            Some(StrictValue::EmptyList),
        ),
        spec(Key::RequireHttps, "RequireHttps", Bool, R, Some(B(true))),
        spec(
            Key::AllowCertificateExceptions,
            "AllowCertificateExceptions",
            Bool,
            R,
            Some(B(false)),
        ),
        KeySpec {
            applicability: Applicability {
                oses: Os::ALL,
                mas: false,
                needs_self_update: true,
            },
            ..spec(
                Key::UpdateMode,
                "UpdateMode",
                ValueType::Enum(UpdateMode::NAMES),
                R,
                Some(StrictValue::UpdateMode(UpdateMode::Off)),
            )
        },
        KeySpec {
            refresh: RefreshClass::NextSignIn,
            ..spec(
                Key::AllowPasswordLogin,
                "AllowPasswordLogin",
                Bool,
                R,
                Some(B(false)),
            )
        },
        KeySpec {
            refresh: RefreshClass::NextSignIn,
            ..spec(
                Key::AllowTokenLogin,
                "AllowTokenLogin",
                Bool,
                R,
                Some(B(false)),
            )
        },
        // The Mac App Store flavor never writes the secrets file.
        KeySpec {
            applicability: Applicability {
                oses: Os::ALL,
                mas: false,
                needs_self_update: false,
            },
            ..spec(
                Key::AllowSecretsFile,
                "AllowSecretsFile",
                Bool,
                R,
                Some(B(false)),
            )
        },
        spec(
            Key::NotificationPreview,
            "NotificationPreview",
            Bool,
            R,
            Some(B(false)),
        ),
        KeySpec {
            applicability: Applicability {
                oses: Os::ALL,
                mas: false,
                needs_self_update: false,
            },
            refresh: RefreshClass::NextStart,
            ..spec(
                Key::HardwareAcceleration,
                "HardwareAcceleration",
                Bool,
                C,
                None,
            )
        },
        spec(Key::LaunchAtLogin, "LaunchAtLogin", Bool, C, None),
        spec(
            Key::OrganizationName,
            "OrganizationName",
            ValueType::String,
            C,
            None,
        ),
        spec(Key::SupportUrl, "SupportUrl", ValueType::String, T, None),
        spec(
            Key::MinimumVersion,
            "MinimumVersion",
            ValueType::String,
            R,
            None,
        ),
    ]
};

/// Every key's spec, in registry order.
pub fn specs() -> &'static [KeySpec] {
    &SPECS
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn index_matches_discriminant() {
        assert_eq!(Key::ALL.len(), SPECS.len());
        for (i, s) in SPECS.iter().enumerate() {
            assert_eq!(s.key as usize, i, "{}", s.name);
            assert_eq!(Key::ALL[i], s.key);
            assert_eq!(s.key.spec().name, s.name);
            assert_eq!(Key::from_name(s.name), Some(s.key));
        }
    }

    #[test]
    fn lookup_is_exact() {
        assert_eq!(Key::from_name("RequireHttps"), Some(Key::RequireHttps));
        assert_eq!(Key::from_name("requirehttps"), None);
        assert_eq!(Key::from_name("RequireHTTPS"), None);
        assert_eq!(Key::from_name(" RequireHttps"), None);
        assert_eq!(Key::from_name(""), None);
    }

    #[test]
    fn names_are_unique_pascal_case() {
        for (i, s) in SPECS.iter().enumerate() {
            assert!(s.name.starts_with(|c: char| c.is_ascii_uppercase()));
            assert!(s.name.chars().all(|c| c.is_ascii_alphanumeric()));
            assert!(SPECS[i + 1..].iter().all(|o| o.name != s.name));
        }
    }

    #[test]
    fn strict_value_matches_type() {
        for s in specs() {
            match (s.strict, s.value_type) {
                (None, _)
                | (Some(StrictValue::Bool(_)), ValueType::Bool)
                | (Some(StrictValue::UpdateMode(_)), ValueType::Enum(_))
                | (Some(StrictValue::EmptyList), ValueType::StringList) => {}
                other => panic!("{}: {other:?}", s.name),
            }
        }
    }

    #[test]
    fn introduced_and_refresh() {
        for s in specs() {
            assert_eq!(s.introduced, "0.4.0");
            let expected = match s.key {
                Key::HardwareAcceleration => RefreshClass::NextStart,
                Key::AllowPasswordLogin | Key::AllowTokenLogin => RefreshClass::NextSignIn,
                _ => RefreshClass::Live,
            };
            assert_eq!(s.refresh, expected, "{}", s.name);
        }
    }

    #[test]
    fn applicability() {
        let ctx = |os, mas, self_update| Context {
            client_version: "0.4.0".into(),
            os,
            mas,
            self_update,
        };
        let applies = |k: Key, c: &Context| k.spec().applicability.applies(c);
        let linux = ctx(Os::Linux, false, true);
        let no_updater = ctx(Os::Windows, false, false);
        let mas = ctx(Os::MacOs, true, false);
        assert!(Key::ALL.iter().all(|k| applies(*k, &linux)));
        assert!(!applies(Key::UpdateMode, &no_updater));
        assert!(applies(Key::HardwareAcceleration, &no_updater));
        assert!(!applies(Key::UpdateMode, &mas));
        assert!(!applies(Key::HardwareAcceleration, &mas));
        assert!(!applies(Key::AllowSecretsFile, &mas));
        assert!(applies(Key::AllowSecretsFile, &no_updater));
        assert!(applies(Key::AllowSecretsFile, &ctx(Os::MacOs, false, true)));
        for k in Key::ALL {
            if k != Key::UpdateMode {
                assert!(applies(k, &no_updater), "{k}");
            }
            if !matches!(
                k,
                Key::UpdateMode | Key::HardwareAcceleration | Key::AllowSecretsFile
            ) {
                assert!(applies(k, &mas), "{k}");
            }
        }
    }

    /// Section 13: shipped keys are never removed or retyped. The fixture is
    /// append-only.
    #[test]
    fn registry_snapshot() {
        let fixture = include_str!("../../tests/fixtures/policy/registry.txt");
        let mut listed = Vec::new();
        for line in fixture.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(fields.len(), 3, "bad fixture line: {line}");
            let key = Key::from_name(fields[0])
                .unwrap_or_else(|| panic!("shipped key {} was removed", fields[0]));
            let spec = key.spec();
            assert_eq!(
                spec.value_type.describe(),
                fields[1],
                "{} retyped",
                fields[0]
            );
            assert_eq!(
                spec.class.letter().to_string(),
                fields[2],
                "{} changed class",
                fields[0]
            );
            listed.push(fields[0]);
        }
        for s in specs() {
            assert!(
                listed.contains(&s.name),
                "{} is missing from tests/fixtures/policy/registry.txt",
                s.name
            );
        }
    }

    /// Section 11: the Mac App Store build rejects binaries with these markers.
    #[test]
    fn no_store_markers_in_names() {
        const MARKERS: [&str; 4] = [
            "api.github.com",
            "install-kind",
            "SHA256SUMS",
            "didDismissAlert",
        ];
        for s in specs() {
            for m in MARKERS {
                assert!(!s.name.contains(m), "{} contains {m}", s.name);
            }
            if let ValueType::Enum(variants) = s.value_type {
                for v in variants {
                    assert!(MARKERS.iter().all(|m| !v.contains(m)), "{v}");
                }
            }
        }
    }
}
