//! Locale selection and the runtime translation registry.
//!
//! Bundled strings live in `locales/<component>.yml` (rust-i18n `_version: 2`,
//! one file per component, one line per bundled locale under each key). Use
//! `rust_i18n::t!("component.key")` everywhere a user sees text.
//!
//! External locale files (`crate::locales`, `docs/locales/README.md`) are
//! installed here with [`install`]; the [`Overrides`] backend that
//! `rust_i18n::i18n!` extends the bundled tables with looks them up first, so
//! a file overrides a bundled locale key by key and a key missing everywhere
//! falls back to English.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, PoisonError, RwLock};

use rustket_core::config::Language;

/// How a locale picks the `_one`/`_few`/`_many` variant of a plural key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluralRule {
    /// `one` for 1, else `many` (English, Spanish, German, …).
    OneOther,
    /// Russian, Ukrainian, Belarusian: `one` (1, 21, …), `few` (2–4, 22–24,
    /// …), `many` (the rest, 11–14).
    EastSlavic,
    /// No plural forms: always `many` (Thai, Chinese, Japanese, …).
    None,
}

impl PluralRule {
    pub const ALL: [Self; 3] = [Self::OneOther, Self::EastSlavic, Self::None];

    /// The name used in locale files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneOther => "one-other",
            Self::EastSlavic => "east-slavic",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// `one`, `few` or `many` for `n`.
    pub fn category(self, n: u64) -> &'static str {
        match self {
            Self::OneOther => {
                if n == 1 {
                    "one"
                } else {
                    "many"
                }
            }
            Self::EastSlavic => {
                let (m10, m100) = (n % 10, n % 100);
                if m10 == 1 && m100 != 11 {
                    "one"
                } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                    "few"
                } else {
                    "many"
                }
            }
            Self::None => "many",
        }
    }
}

/// A locale compiled into the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bundled {
    pub code: &'static str,
    /// The language's own name.
    pub name: &'static str,
    pub plural: PluralRule,
}

/// Every bundled locale, English (the base and fallback) first.
pub const BUNDLED: &[Bundled] = &[
    Bundled {
        code: "en",
        name: "English",
        plural: PluralRule::OneOther,
    },
    Bundled {
        code: "ru",
        name: "Русский",
        plural: PluralRule::EastSlavic,
    },
    Bundled {
        code: "es",
        name: "Español",
        plural: PluralRule::OneOther,
    },
    Bundled {
        code: "th",
        name: "ไทย",
        plural: PluralRule::None,
    },
    Bundled {
        code: "zh",
        name: "简体中文",
        plural: PluralRule::None,
    },
];

pub fn bundled(code: &str) -> Option<&'static Bundled> {
    BUNDLED.iter().find(|b| b.code == code)
}

/// An installed external locale (a parsed `<code>.toml`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Installed {
    /// `[meta] name`.
    pub name: String,
    /// `[meta] plural` when the file sets it.
    pub plural: Option<PluralRule>,
    /// Usable translations (key → text).
    pub strings: HashMap<String, String>,
}

static EXTERNAL: RwLock<BTreeMap<String, Installed>> = RwLock::new(BTreeMap::new());
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn external() -> std::sync::RwLockReadGuard<'static, BTreeMap<String, Installed>> {
    EXTERNAL.read().unwrap_or_else(PoisonError::into_inner)
}

/// Replaces the installed external locales (code → locale).
pub fn install(locales: BTreeMap<String, Installed>) {
    *EXTERNAL.write().unwrap_or_else(PoisonError::into_inner) = locales;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Changes whenever [`install`] replaces the external strings.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The current locale and [`generation`], for keys of cached views.
pub fn cache_tag() -> String {
    format!("{}#{}", &*rust_i18n::locale(), generation())
}

/// The rust-i18n backend extension with the external strings: consulted
/// before the bundled tables. It lists no messages, so
/// `messages_for_locale` of the combined backend stays the bundled one.
pub struct Overrides;

impl rust_i18n::Backend for Overrides {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        external().keys().map(|k| Cow::Owned(k.clone())).collect()
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        external()
            .get(locale)?
            .strings
            .get(key)
            .map(|s| Cow::Owned(s.clone()))
    }

    fn messages_for_locale(&self, _locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        None
    }
}

/// The bundled strings of `locale` (key → text; empty for an unknown
/// locale), without external overrides.
pub fn bundled_messages(locale: &str) -> BTreeMap<&'static str, &'static str> {
    crate::_rust_i18n_backend()
        .messages_for_locale(locale)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| match (k, v) {
            (Cow::Borrowed(k), Cow::Borrowed(v)) => Some((k, v)),
            _ => None,
        })
        .collect()
}

/// The bundled English strings: every key the UI uses.
pub fn english() -> &'static BTreeMap<&'static str, &'static str> {
    static EN: OnceLock<BTreeMap<&'static str, &'static str>> = OnceLock::new();
    EN.get_or_init(|| bundled_messages("en"))
}

/// Codes of every usable locale: the bundled ones, then the installed
/// external ones that add a language.
pub fn available() -> Vec<String> {
    let mut out: Vec<String> = BUNDLED.iter().map(|b| b.code.to_owned()).collect();
    for code in external().keys() {
        if bundled(code).is_none() {
            out.push(code.clone());
        }
    }
    out
}

pub fn is_available(code: &str) -> bool {
    bundled(code).is_some() || external().contains_key(code)
}

/// The native name of an available locale (`None` for an unknown code).
pub fn native_name(code: &str) -> Option<String> {
    if let Some(b) = bundled(code) {
        return Some(b.name.to_owned());
    }
    external().get(code).map(|l| l.name.clone())
}

/// The plural rule of `locale`: the external file's, else the bundled
/// locale's (also for regional variants such as `ru_RU`), else
/// [`PluralRule::OneOther`].
pub fn plural_rule(locale: &str) -> PluralRule {
    if let Some(rule) = external().get(locale).and_then(|l| l.plural) {
        return rule;
    }
    let lang = locale.split(['_', '-']).next().unwrap_or_default();
    bundled(locale)
        .or_else(|| bundled(lang))
        .map_or(PluralRule::OneOther, |b| b.plural)
}

/// The locale code the configured language uses: the configured code when
/// it is available, else the system language.
pub fn resolve(lang: &Language) -> String {
    if !lang.is_system() && is_available(lang.as_str()) {
        return lang.as_str().to_owned();
    }
    system()
}

/// The locale of the environment among the available ones.
pub fn system() -> String {
    from_env(|k| std::env::var(k).ok(), &available())
}

/// Locale from `LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG`: the first
/// non-empty variable decides (`LANGUAGE` may list several, tried in order).
/// An entry matches an available locale exactly (`pt_BR`), else by its
/// language (`ru_RU` → `ru`, `zh_TW` → `zh`); no match is English.
pub fn from_env(get: impl Fn(&str) -> Option<String>, available: &[String]) -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        let Some(v) = get(key).filter(|v| !v.is_empty() && v != "C" && v != "POSIX") else {
            continue;
        };
        for entry in v.split(':') {
            let code = entry.split(['.', '@']).next().unwrap_or_default();
            if code.is_empty() {
                continue;
            }
            let norm = |s: &str| s.replace('-', "_").to_ascii_lowercase();
            if let Some(exact) = available.iter().find(|a| norm(a) == norm(code)) {
                return exact.clone();
            }
            let lang = norm(code.split(['_', '-']).next().unwrap_or_default());
            if let Some(m) = available.iter().find(|a| norm(a) == lang) {
                return m.clone();
            }
        }
        return "en".to_owned();
    }
    "en".to_owned()
}

/// Makes the configured language the current locale.
pub fn apply(lang: &Language) {
    rust_i18n::set_locale(&resolve(lang));
}

/// Translates a key computed at runtime (e.g. `room_list` i18n keys). Falls back
/// to `fallback` when the key is missing in every locale.
pub fn tr_dyn(key: &str, fallback: &str) -> String {
    let s = rust_i18n::t!(key).into_owned();
    if s == key || s.ends_with(key) {
        fallback.to_owned()
    } else {
        s
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Bundled locales whose yml lines must be complete (non-empty text for
    /// every key). The others are checked only where a line exists.
    const BUNDLED_COMPLETE: &[&str] = &["en", "ru", "es", "th", "zh"];

    fn codes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn env_resolution() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let bundled = codes(&["en", "ru", "es", "th", "zh"]);
        let from = |pairs, avail: &[String]| from_env(env(pairs), avail);
        assert_eq!(from(&[("LANG", "ru_RU.UTF-8")], &bundled), "ru");
        assert_eq!(from(&[("LANG", "de_DE.UTF-8")], &bundled), "en");
        assert_eq!(
            from(
                &[("LC_ALL", "en_US.UTF-8"), ("LANG", "ru_RU.UTF-8")],
                &bundled
            ),
            "en"
        );
        assert_eq!(from(&[("LC_ALL", "C"), ("LANG", "ru_RU")], &bundled), "ru");
        assert_eq!(from(&[], &bundled), "en");
        assert_eq!(from(&[("LANG", "es_MX.UTF-8")], &bundled), "es");
        assert_eq!(from(&[("LANG", "th_TH.UTF-8")], &bundled), "th");
        for zh in [
            &[("LANG", "zh_CN.UTF-8")][..],
            &[("LANG", "zh_TW.UTF-8")],
            &[("LANG", "zh_HK")],
            &[("LANG", "zh")],
        ] {
            assert_eq!(from(zh, &bundled), "zh", "{zh:?}");
        }
        // LANGUAGE is a priority list.
        assert_eq!(
            from(&[("LANGUAGE", "de_DE:es"), ("LANG", "ru_RU")], &bundled),
            "es"
        );
        // External locales: exact code first, then the language.
        let mut ext = bundled.clone();
        ext.extend(codes(&["pt_BR", "de"]));
        assert_eq!(from(&[("LANG", "pt_BR.UTF-8")], &ext), "pt_BR");
        assert_eq!(from(&[("LANG", "pt-br")], &ext), "pt_BR");
        assert_eq!(from(&[("LANG", "pt_PT.UTF-8")], &ext), "en");
        assert_eq!(from(&[("LANG", "de_AT.UTF-8")], &ext), "de");
        let mut zh_tw = bundled.clone();
        zh_tw.push("zh_TW".into());
        assert_eq!(from(&[("LANG", "zh_TW.UTF-8")], &zh_tw), "zh_TW");
        assert_eq!(from(&[("LANG", "zh_CN.UTF-8")], &zh_tw), "zh");
    }

    #[test]
    fn plural_rules() {
        let r = PluralRule::EastSlavic;
        let cats: Vec<&str> = [1, 21, 11, 3, 13, 25, 0, 102]
            .iter()
            .map(|n| r.category(*n))
            .collect();
        assert_eq!(
            cats,
            ["one", "one", "many", "few", "many", "many", "many", "few"]
        );
        assert_eq!(PluralRule::OneOther.category(1), "one");
        assert_eq!(PluralRule::OneOther.category(0), "many");
        assert_eq!(PluralRule::OneOther.category(21), "many");
        assert_eq!(PluralRule::None.category(1), "many");
        for r in PluralRule::ALL {
            assert_eq!(PluralRule::parse(r.as_str()), Some(r));
        }
        assert_eq!(PluralRule::parse("two"), None);
        assert_eq!(plural_rule("ru"), PluralRule::EastSlavic);
        assert_eq!(plural_rule("ru_RU"), PluralRule::EastSlavic);
        assert_eq!(plural_rule("th"), PluralRule::None);
        assert_eq!(plural_rule("zh"), PluralRule::None);
        assert_eq!(plural_rule("es"), PluralRule::OneOther);
        assert_eq!(plural_rule("xx_unknown"), PluralRule::OneOther);
    }

    /// The only test that installs external strings (the registry and the
    /// rust-i18n locale are process-wide): fake locale codes and one obscure
    /// Russian key, so parallel tests keep their texts.
    #[test]
    fn overrides_backend() {
        let key = "cli.env_backtrace";
        let bundled_ru = rust_i18n::t!(key, locale = "ru").into_owned();
        let mut map = BTreeMap::new();
        map.insert(
            "xx_TEST".to_owned(),
            Installed {
                name: "Testish".into(),
                plural: Some(PluralRule::None),
                strings: [
                    ("login.sign_in".to_owned(), "Sign in, xx".to_owned()),
                    ("cli.usage".to_owned(), "xx %{bin}".to_owned()),
                ]
                .into_iter()
                .collect(),
            },
        );
        map.insert(
            "ru".to_owned(),
            Installed {
                name: "Русский".into(),
                plural: None,
                strings: [(key.to_owned(), "переопределено".to_owned())]
                    .into_iter()
                    .collect(),
            },
        );
        install(map);
        let g = generation();
        assert_eq!(
            rust_i18n::t!("login.sign_in", locale = "xx_TEST"),
            "Sign in, xx"
        );
        assert_eq!(
            rust_i18n::t!("cli.usage", locale = "xx_TEST", bin = "rk"),
            "xx rk"
        );
        // Missing keys fall back to English.
        assert_eq!(
            rust_i18n::t!("cli.options", locale = "xx_TEST"),
            rust_i18n::t!("cli.options", locale = "en")
        );
        // A bundled locale is overridden key by key.
        assert_eq!(rust_i18n::t!(key, locale = "ru"), "переопределено");
        assert_ne!(
            rust_i18n::t!("cli.options", locale = "ru"),
            rust_i18n::t!("cli.options", locale = "en")
        );
        assert!(is_available("xx_TEST"));
        assert!(available().contains(&"xx_TEST".to_owned()));
        assert_eq!(available().iter().filter(|c| *c == "ru").count(), 1);
        assert_eq!(native_name("xx_TEST").as_deref(), Some("Testish"));
        assert_eq!(native_name("zh").as_deref(), Some("简体中文"));
        assert_eq!(plural_rule("xx_TEST"), PluralRule::None);
        assert_eq!(plural_rule("ru"), PluralRule::EastSlavic);
        assert_eq!(resolve(&Language::new("xx_TEST").unwrap()), "xx_TEST");
        assert_eq!(resolve(&Language::new("ru").unwrap()), "ru");
        assert_eq!(
            resolve(&Language::new("yy_GONE").unwrap()),
            resolve(&Language::system())
        );
        // The bundled tables stay pristine.
        assert_eq!(bundled_messages("ru").get(key).copied(), Some(&*bundled_ru));
        assert!(!english().contains_key("xx_TEST"));
        install(BTreeMap::new());
        assert!(generation() > g);
        assert_eq!(rust_i18n::t!(key, locale = "ru"), bundled_ru);
        assert!(!is_available("xx_TEST"));
    }

    /// `key -> locale -> text` of every `locales/*.yml` (the flat
    /// `_version: 2` layout this crate uses: a key line, then one indented
    /// `<locale>: "text"` line per locale).
    fn locale_entries() -> BTreeMap<String, BTreeMap<String, String>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("yml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let mut key: Option<String> = None;
            for line in text.lines() {
                if line.trim().is_empty() || line.starts_with("_version") {
                    continue;
                }
                if !line.starts_with(' ') {
                    let k = line.trim_end_matches(':').to_owned();
                    assert!(
                        out.insert(k.clone(), BTreeMap::new()).is_none(),
                        "duplicate key {k} ({})",
                        path.display()
                    );
                    key = Some(k);
                    continue;
                }
                let (lang, value) = line.trim().split_once(": ").unwrap();
                let value: String = serde_json::from_str(value)
                    .unwrap_or_else(|_| value.trim_matches('"').to_owned());
                let k = key.clone().unwrap();
                assert!(bundled(lang).is_some(), "unexpected locale {lang} for {k}");
                let e = out.get_mut(&k).unwrap();
                assert!(
                    e.insert(lang.to_owned(), value).is_none(),
                    "duplicate {lang} for {k}"
                );
            }
        }
        out
    }

    /// Sorted `%{name}` placeholders.
    fn placeholders(s: &str) -> Vec<String> {
        let mut v: Vec<String> = s
            .split("%{")
            .skip(1)
            .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
            .collect();
        v.sort();
        v
    }

    /// Review #30: every key has a non-empty text in each locale of
    /// [`BUNDLED_COMPLETE`], every text has the English placeholders, and
    /// every literal `t!("…")` key used in the sources is defined.
    #[test]
    fn translations_complete() {
        let entries = locale_entries();
        assert!(entries.len() > 150);
        for complete in BUNDLED_COMPLETE {
            assert!(bundled(complete).is_some(), "{complete} is not bundled");
        }
        for (k, texts) in &entries {
            for loc in BUNDLED_COMPLETE {
                assert!(
                    texts.get(*loc).is_some_and(|t| !t.is_empty()),
                    "{k}: missing {loc}"
                );
            }
            let en = texts.get("en").map(String::as_str).unwrap_or_default();
            for (loc, text) in texts {
                assert!(!text.is_empty(), "{k}: empty {loc}");
                assert_eq!(
                    placeholders(en),
                    placeholders(text),
                    "{k}: placeholders of {loc} differ from en"
                );
            }
        }
        // Keys computed by rustket-core.
        for k in [
            "sidebar.unread",
            "sidebar.favorites",
            "sidebar.threads",
            "sidebar.thread_loading",
            "sidebar.thread_removed",
            "sidebar.thread_unavailable",
            "preview.encrypted",
            "sidebar.teams",
            "sidebar.discussions",
            "sidebar.channels",
            "sidebar.private_groups",
            "sidebar.direct_messages",
            "sidebar.conversations",
            "sidebar.omnichannel",
            "sidebar.flag.red",
            "sidebar.flag.orange",
            "sidebar.flag.yellow",
            "sidebar.flag.green",
            "sidebar.flag.blue",
            "sidebar.flag.purple",
            "typing.typing",
            "typing.typing_many",
            "typing.recording",
            "typing.recording_many",
            "typing.uploading",
            "typing.uploading_many",
            "preview.image",
            "preview.file",
        ] {
            assert!(entries.contains_key(k), "{k} missing");
        }
        // Literal keys in the sources.
        fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, files);
                } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                    files.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut used = 0;
        for f in files {
            let text: String = std::fs::read_to_string(&f)
                .unwrap()
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for part in text.split("t!(\"").skip(1) {
                let Some((key, _)) = part.split_once('"') else {
                    continue;
                };
                if key.contains('{') || key.contains(' ') || !key.contains('.') {
                    continue;
                }
                used += 1;
                assert!(
                    entries.contains_key(key),
                    "{key} used in {} is not defined",
                    f.display()
                );
            }
        }
        assert!(used > 100);
        // The runtime agrees with the files.
        assert_eq!(english().len(), entries.len());
        for loc in BUNDLED_COMPLETE {
            assert_ne!(
                rust_i18n::t!("login.sign_in", locale = loc),
                "login.sign_in"
            );
            assert_eq!(bundled_messages(loc).len(), entries.len(), "{loc}");
        }
        assert_ne!(
            rust_i18n::t!("login.sign_in", locale = "ru"),
            rust_i18n::t!("login.sign_in", locale = "en")
        );
    }
}
