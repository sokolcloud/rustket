//! External UI translations: `<config dir>/locales/<code>.toml` files
//! (`docs/locales/README.md`). A file adds a language (`de.toml`,
//! `pt_BR.toml`) or overrides a bundled one key by key (`ru.toml`); the
//! strings are installed into [`crate::i18n`], where missing keys fall back
//! to English.
//!
//! Parsing is tolerant like the theme files: unknown keys, non-string
//! values, strings whose `%{placeholders}` differ from English and an
//! unknown plural rule become [`LocaleWarning`]s. Only a TOML syntax error or
//! a missing `[meta] name` is an error.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustket_core::config::Language;

use crate::i18n::{self, Installed, PluralRule};

/// Subdirectory of the config directory with the locale files.
pub const LOCALES_DIR: &str = "locales";
/// The locale file format version this build writes and understands.
pub const FORMAT_VERSION: i64 = 1;
/// Largest locale file read (a full template is ~200 KiB).
const MAX_FILE: u64 = 4 * 1024 * 1024;

pub fn locales_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCALES_DIR)
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum LocaleFileError {
    #[error("not valid TOML: {0}")]
    Syntax(String),
    #[error("[meta] name is missing")]
    MissingName,
}

/// Something in a locale file that was ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum LocaleWarning {
    /// A key this version does not know (dotted path; `strings.<key>` for
    /// a string key the UI does not use).
    UnknownKey(String),
    /// A value of the wrong type.
    BadValue { key: String, value: String },
    /// The `%{…}` placeholders differ from the English text (string skipped).
    Placeholders(String),
    /// `[meta] plural` is not a known rule.
    UnknownPlural(String),
    /// `[meta] version` is newer than [`FORMAT_VERSION`].
    NewerVersion(i64),
}

impl fmt::Display for LocaleWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(k) => write!(f, "unknown key {k}"),
            Self::BadValue { key, value } => write!(f, "bad value for {key}: {value}"),
            Self::Placeholders(k) => write!(f, "placeholders of {k} differ from English"),
            Self::UnknownPlural(p) => write!(f, "unknown plural rule {p}"),
            Self::NewerVersion(v) => write!(f, "format version {v} is newer than {FORMAT_VERSION}"),
        }
    }
}

/// `[meta]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocaleMeta {
    /// The language's own name.
    pub name: String,
    pub author: Option<String>,
    pub version: Option<i64>,
    /// `None`: not set (the bundled locale's rule, else one-other).
    pub plural: Option<PluralRule>,
}

/// A parsed locale file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocaleFile {
    pub meta: LocaleMeta,
    /// Usable translations: known keys, non-empty, English placeholders.
    pub strings: HashMap<String, String>,
    /// Texts skipped for their placeholders (key → text as written), kept
    /// for `--dump-locale` to show as comments.
    pub rejected: BTreeMap<String, String>,
    pub warnings: Vec<LocaleWarning>,
}

impl LocaleFile {
    /// Parses `src`, checking the strings against `english` (key → text).
    pub fn parse(src: &str, english: &BTreeMap<&str, &str>) -> Result<Self, LocaleFileError> {
        let root: toml::Table = src.parse().map_err(|e: toml::de::Error| {
            LocaleFileError::Syntax(rustket_ui::theme_file::syntax_message(src, &e))
        })?;
        let mut out = Self::default();
        let mut name = None;
        for (key, value) in &root {
            match key.as_str() {
                "meta" => {
                    let Some(t) = table(value, "meta", &mut out.warnings) else {
                        continue;
                    };
                    for (k, v) in t {
                        let path = format!("meta.{k}");
                        let s = || v.as_str().map(str::to_owned);
                        match (k.as_str(), s()) {
                            ("name", Some(s)) => name = Some(s),
                            ("author", Some(s)) => out.meta.author = Some(s),
                            ("plural", Some(s)) => match PluralRule::parse(s.trim()) {
                                Some(r) => out.meta.plural = Some(r),
                                None => out.warnings.push(LocaleWarning::UnknownPlural(s)),
                            },
                            ("version", _) => match v.as_integer() {
                                Some(n) => out.meta.version = Some(n),
                                None => out.warnings.push(bad(&path, v)),
                            },
                            ("name" | "author" | "plural", None) => {
                                out.warnings.push(bad(&path, v));
                            }
                            _ => out.warnings.push(LocaleWarning::UnknownKey(path)),
                        }
                    }
                }
                "strings" => {
                    let Some(t) = table(value, "strings", &mut out.warnings) else {
                        continue;
                    };
                    // `settings.title = "…"` without quotes is a nested table.
                    let mut flat = Vec::new();
                    flatten("", t, &mut flat);
                    for (k, v) in flat {
                        let path = format!("strings.{k}");
                        let Some(en) = english.get(k.as_str()) else {
                            out.warnings.push(LocaleWarning::UnknownKey(path));
                            continue;
                        };
                        let Some(s) = v.as_str() else {
                            out.warnings.push(bad(&path, v));
                            continue;
                        };
                        if s.is_empty() {
                            continue;
                        }
                        if placeholders(s) != placeholders(en) {
                            out.warnings.push(LocaleWarning::Placeholders(k.clone()));
                            out.rejected.insert(k, s.to_owned());
                            continue;
                        }
                        out.strings.insert(k, s.to_owned());
                    }
                }
                _ => out.warnings.push(LocaleWarning::UnknownKey(key.clone())),
            }
        }
        out.meta.name = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .ok_or(LocaleFileError::MissingName)?;
        if let Some(v) = out.meta.version.filter(|v| *v > FORMAT_VERSION) {
            out.warnings.push(LocaleWarning::NewerVersion(v));
        }
        Ok(out)
    }
}

fn flatten<'a>(prefix: &str, t: &'a toml::Table, out: &mut Vec<(String, &'a toml::Value)>) {
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v.as_table() {
            Some(sub) => flatten(&key, sub, out),
            None => out.push((key, v)),
        }
    }
}

/// Sorted `%{name}` placeholders of `s`.
pub fn placeholders(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s
        .split("%{")
        .skip(1)
        .filter_map(|p| p.split_once('}').map(|(n, _)| n))
        .collect();
    v.sort_unstable();
    v
}

fn bad(key: &str, v: &toml::Value) -> LocaleWarning {
    LocaleWarning::BadValue {
        key: key.to_owned(),
        value: v.to_string(),
    }
}

fn table<'a>(
    v: &'a toml::Value,
    key: &str,
    warnings: &mut Vec<LocaleWarning>,
) -> Option<&'a toml::Table> {
    let t = v.as_table();
    if t.is_none() {
        warnings.push(bad(key, v));
    }
    t
}

/// Why a locale file cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// No `<code>.toml` (the selected locale was removed or renamed).
    NotFound,
    /// Unreadable.
    Read(String),
    /// Larger than the limit (size in bytes).
    TooLarge(u64),
    Parse(LocaleFileError),
}

/// One locale file.
#[derive(Debug, Clone)]
pub struct LocaleEntry {
    /// File stem: the locale code.
    pub code: String,
    pub file: Result<Arc<LocaleFile>, LoadError>,
}

impl LocaleEntry {
    fn load(code: String, path: &Path) -> Self {
        let file = read(path).and_then(|text| {
            LocaleFile::parse(&text, i18n::english())
                .map(Arc::new)
                .map_err(LoadError::Parse)
        });
        Self { code, file }
    }

    /// `[meta] name`; the code for a broken file.
    pub fn display_name(&self) -> String {
        match &self.file {
            Ok(f) => f.meta.name.clone(),
            Err(_) => self.code.clone(),
        }
    }
}

fn read(path: &Path) -> Result<String, LoadError> {
    let meta = std::fs::metadata(path).map_err(|e| LoadError::Read(e.to_string()))?;
    if meta.len() > MAX_FILE {
        return Err(LoadError::TooLarge(meta.len()));
    }
    std::fs::read_to_string(path).map_err(|e| LoadError::Read(e.to_string()))
}

/// Every `<code>.toml` in `dir` whose stem is a valid locale code, sorted
/// by code; a missing directory is an empty list.
pub fn scan(dir: &Path) -> Vec<LocaleEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<LocaleEntry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("toml") || !path.is_file() {
                return None;
            }
            let code = path.file_stem()?.to_str()?.to_owned();
            if !Language::valid_code(&code) || code == Language::SYSTEM {
                tracing::warn!(file = %path.display(), "locale file name is not a locale code; ignored");
                return None;
            }
            Some(LocaleEntry::load(code, &path))
        })
        .collect();
    out.sort_by(|a, b| a.code.cmp(&b.code));
    out
}

/// Translated and total keys of a locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Coverage {
    pub translated: usize,
    pub total: usize,
}

impl Coverage {
    /// Whole percent: below 100 while anything is missing, above 0 once
    /// anything is translated.
    pub fn percent(self) -> usize {
        if self.total == 0 || self.translated >= self.total {
            return 100;
        }
        (self.translated * 100 / self.total).clamp(usize::from(self.translated > 0), 99)
    }

    pub fn complete(self) -> bool {
        self.translated >= self.total
    }
}

/// The locale files, installed into [`crate::i18n`].
#[derive(Debug, Default)]
pub struct Locales {
    /// `<config dir>/locales` (`None`: no config directory, e.g. tests).
    pub dir: Option<PathBuf>,
    pub entries: Vec<LocaleEntry>,
    /// Of every available locale (bundled and external), by code.
    pub coverage: BTreeMap<String, Coverage>,
}

impl Locales {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            ..Self::default()
        }
    }

    /// Reads the locale files again, installs their strings and re-applies
    /// `lang` (its file may have appeared, changed or gone).
    pub fn reload(&mut self, lang: &Language) {
        self.entries = self.dir.as_deref().map(scan).unwrap_or_default();
        self.install();
        i18n::apply(lang);
    }

    /// Installs the usable files into [`crate::i18n`] and computes the
    /// coverage of every locale.
    pub fn install(&mut self) {
        let installed: BTreeMap<String, Installed> = self
            .entries
            .iter()
            .filter_map(|e| {
                let f = e.file.as_ref().ok()?;
                Some((
                    e.code.clone(),
                    Installed {
                        name: f.meta.name.clone(),
                        plural: f.meta.plural,
                        strings: f.strings.clone(),
                    },
                ))
            })
            .collect();
        let english = i18n::english();
        let mut coverage = BTreeMap::new();
        for code in i18n::BUNDLED
            .iter()
            .map(|b| b.code)
            .chain(installed.keys().map(String::as_str))
        {
            let bundled = i18n::bundled_messages(code);
            let file = installed.get(code);
            let translated = english
                .keys()
                .filter(|k| {
                    bundled.get(*k).is_some_and(|s| !s.is_empty())
                        || file.is_some_and(|f| f.strings.contains_key(**k))
                })
                .count();
            coverage.insert(
                code.to_owned(),
                Coverage {
                    translated,
                    total: english.len(),
                },
            );
        }
        self.coverage = coverage;
        // Without a folder there are no files; the registry is left alone
        // (process-wide, shared by tests).
        if self.dir.is_some() {
            i18n::install(installed);
        }
    }

    pub fn get(&self, code: &str) -> Option<&LocaleEntry> {
        self.entries.iter().find(|e| e.code == code)
    }

    pub fn coverage(&self, code: &str) -> Option<Coverage> {
        self.coverage.get(code).copied()
    }
}

/// Text as a single-line TOML basic string.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A complete translation template (`rustket --dump-locale [CODE]`): every
/// key in stable order, grouped by component. For a code other than `en`
/// each string follows a `# en: "…"` comment with the English source, and
/// keys without a translation are commented out. `file` adds an external
/// locale's strings (and its name and plural rule) on top of the bundled
/// ones. `None` when `code` is neither bundled nor given a file.
pub fn dump_template(code: &str, file: Option<&LocaleFile>) -> Option<String> {
    let bundled = i18n::bundled(code);
    let author = file.and_then(|f| f.meta.author.as_deref()).map_or_else(
        || "# author = \"\"".to_owned(),
        |a| format!("author = {}", quote(a)),
    );
    let (name, plural) = match (bundled, file) {
        (_, Some(f)) => (
            f.meta.name.clone(),
            f.meta
                .plural
                .or(bundled.map(|b| b.plural))
                .unwrap_or(PluralRule::OneOther),
        ),
        (Some(b), None) => (b.name.to_owned(), b.plural),
        (None, None) => return None,
    };
    let english = i18n::english();
    let own = i18n::bundled_messages(code);
    let mut out = format!(
        "# Rustket translation template, generated by `rustket --dump-locale {code}`.\n\
         # Save it as <config dir>/locales/<code>.toml: the file name is the locale\n\
         # code (de.toml, pt_BR.toml; ru.toml fixes the bundled Russian strings).\n\
         # See docs/locales/README.md, then pick the language in Settings → Language.\n\
         #\n\
         # Keep every %{{placeholder}} of the English text: a string whose\n\
         # placeholders differ is skipped. Missing, empty or commented-out\n\
         # strings show in English. Keys ending in _one / _few / _many are plural\n\
         # forms picked by [meta] plural: \"one-other\" (1 → _one, else _many),\n\
         # \"east-slavic\" (Russian-style _one / _few / _many) or \"none\" (always\n\
         # _many). Keys ending in _fmt are chrono date formats (%d, %m, %Y, %b…).\n\n\
         [meta]\nname = {}\n{author}\nversion = {FORMAT_VERSION}\nplural = {}\n\n[strings]\n",
        quote(&name),
        quote(plural.as_str()),
    );
    let mut group = "";
    for (key, en) in english {
        let component = key.split('.').next().unwrap_or_default();
        if component != group {
            if !group.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("# --- {component} ---\n"));
            group = component;
        }
        if code == "en" && file.is_none() {
            out.push_str(&format!("{} = {}\n", quote(key), quote(en)));
            continue;
        }
        let text = file
            .and_then(|f| f.strings.get(*key).map(String::as_str))
            .or_else(|| own.get(key).copied())
            .filter(|s| !s.is_empty());
        if code != "en" {
            out.push_str(&format!("# en: {}\n", quote(en)));
        }
        let rejected = file.and_then(|f| f.rejected.get(*key));
        match (text, rejected) {
            (Some(t), _) => out.push_str(&format!("{} = {}\n", quote(key), quote(t))),
            // Skipped for its placeholders: kept to be fixed.
            (None, Some(r)) => out.push_str(&format!("# {} = {}\n", quote(key), quote(r))),
            (None, None) => out.push_str(&format!("# {} = \"\"\n", quote(key))),
        }
    }
    Some(out)
}

/// `--dump-locale` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    pub template: String,
    /// The external file's warnings, for stderr.
    pub warnings: Option<String>,
}

/// `--dump-locale`: the template of `code` — a bundled locale, or an
/// external one from the default locales folder. `Err`: an unknown code,
/// or an external file that cannot be read or parsed.
pub fn dump_for_cli(code: &str) -> Result<Dump, String> {
    let path = (i18n::bundled(code).is_none() && Language::valid_code(code))
        .then(|| rustket_core::config::AppConfig::default_dir().ok())
        .flatten()
        .map(|dir| locales_dir(&dir).join(format!("{code}.toml")))
        .filter(|p| p.exists());
    dump_file(code, path.as_deref())
}

fn dump_file(code: &str, path: Option<&Path>) -> Result<Dump, String> {
    let file = match path {
        Some(p) => match LocaleEntry::load(code.to_owned(), p).file {
            Ok(f) => Some((p, f)),
            Err(e) => return Err(load_error_text(&e, &p.display().to_string())),
        },
        None => None,
    };
    let warnings = file
        .as_ref()
        .filter(|(_, f)| !f.warnings.is_empty())
        .map(|(p, f)| {
            let mut msg = rust_i18n::t!("settings.locale_warnings", file = p.display().to_string())
                .into_owned();
            for w in &f.warnings {
                msg.push_str("\n• ");
                msg.push_str(&warning_text(w, code));
            }
            msg
        });
    let template =
        dump_template(code, file.as_ref().map(|(_, f)| f.as_ref())).ok_or_else(|| {
            let list: Vec<&str> = i18n::BUNDLED.iter().map(|b| b.code).collect();
            rust_i18n::t!("cli.unknown_locale", code = code, list = list.join(", ")).into_owned()
        })?;
    Ok(Dump { template, warnings })
}

/// Why the locale file `file` cannot be used, in the UI language.
pub fn load_error_text(e: &LoadError, file: &str) -> String {
    use rust_i18n::t;
    match e {
        LoadError::NotFound => t!("settings.locale_not_found", file = file),
        LoadError::Read(e) => t!("settings.locale_unreadable", file = file, error = e),
        LoadError::TooLarge(size) => t!("settings.locale_too_large", file = file, size = size),
        LoadError::Parse(LocaleFileError::MissingName) => {
            t!("settings.locale_no_name", file = file)
        }
        LoadError::Parse(LocaleFileError::Syntax(e)) => {
            t!("settings.locale_invalid", file = file, error = e)
        }
    }
    .into_owned()
}

/// A warning of locale `code`'s file in the UI language (key paths stay as
/// written).
pub fn warning_text(w: &LocaleWarning, code: &str) -> String {
    use rust_i18n::t;
    match w {
        LocaleWarning::UnknownKey(k) => t!("settings.theme_w_unknown", key = k),
        LocaleWarning::BadValue { key, value } => {
            t!("settings.theme_w_bad", key = key, value = value)
        }
        LocaleWarning::Placeholders(k) => t!("settings.locale_w_placeholders", key = k),
        LocaleWarning::UnknownPlural(p) => t!(
            "settings.locale_w_plural",
            value = p,
            rule = i18n::plural_rule(code).as_str()
        ),
        LocaleWarning::NewerVersion(v) => t!("settings.theme_w_version", version = v),
    }
    .into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn en() -> BTreeMap<&'static str, &'static str> {
        [
            ("a.title", "Title"),
            ("a.count", "%{n} items"),
            ("b.hello", "Hello, %{name} and %{other}"),
            ("b.bye", "Bye"),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn parse_is_tolerant() {
        let src = r#"
[meta]
name = " Deutsch "
author = "Someone"
version = 9
plural = "dual"
colour = "x"

[strings]
"a.title" = "Titel"
"a.count" = "%{count} Dinge"
"b.hello" = "Hallo %{other}, %{name}"
"a.nope" = "x"

[strings.b]
missing = 5
bye = ""

[extra]
"#;
        let f = LocaleFile::parse(src, &en()).unwrap();
        assert_eq!(f.meta.name, "Deutsch");
        assert_eq!(f.meta.author.as_deref(), Some("Someone"));
        assert_eq!(f.meta.plural, None);
        assert_eq!(f.strings.len(), 2);
        assert_eq!(f.strings["a.title"], "Titel");
        assert_eq!(f.strings["b.hello"], "Hallo %{other}, %{name}");
        let w = &f.warnings;
        assert!(w.contains(&LocaleWarning::UnknownPlural("dual".into())));
        assert!(w.contains(&LocaleWarning::UnknownKey("meta.colour".into())));
        assert!(w.contains(&LocaleWarning::Placeholders("a.count".into())));
        assert!(w.contains(&LocaleWarning::UnknownKey("strings.a.nope".into())));
        assert!(w.contains(&LocaleWarning::UnknownKey("strings.b.missing".into())));
        assert!(w.contains(&LocaleWarning::UnknownKey("extra".into())));
        assert!(w.contains(&LocaleWarning::NewerVersion(9)));
        assert_eq!(w.len(), 7, "{w:?}");

        let f = LocaleFile::parse(
            "[meta]\nname = \"X\"\nplural = \"none\"\n[strings]\n\"a.title\" = 1",
            &en(),
        )
        .unwrap();
        assert_eq!(f.meta.plural, Some(PluralRule::None));
        assert!(
            matches!(&f.warnings[..], [LocaleWarning::BadValue { key, .. }] if key == "strings.a.title")
        );

        assert_eq!(
            LocaleFile::parse("[strings]\n\"a.title\" = \"T\"", &en()),
            Err(LocaleFileError::MissingName)
        );
        assert_eq!(
            LocaleFile::parse("[meta]\nname = \"  \"", &en()),
            Err(LocaleFileError::MissingName)
        );
        let Err(LocaleFileError::Syntax(e)) = LocaleFile::parse("[meta]\nname = \"X\n", &en())
        else {
            panic!("syntax error expected");
        };
        assert!(e.starts_with("line 2, column"), "{e}");
    }

    #[test]
    fn scan_and_coverage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&dir.path().join("missing")).is_empty());
        std::fs::write(
            dir.path().join("pt_BR.toml"),
            "[meta]\nname = \"Português\"\n[strings]\n\"login.sign_in\" = \"Entrar\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.toml"), "[meta").unwrap();
        std::fs::write(dir.path().join("bad name.toml"), "[meta]\nname = \"x\"").unwrap();
        std::fs::write(dir.path().join("system.toml"), "[meta]\nname = \"x\"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let list = scan(dir.path());
        let codes: Vec<&str> = list.iter().map(|e| e.code.as_str()).collect();
        assert_eq!(codes, ["broken", "pt_BR"]);
        assert!(matches!(list[0].file, Err(LoadError::Parse(_))));
        assert_eq!(list[0].display_name(), "broken");
        assert_eq!(list[1].display_name(), "Português");
        assert_eq!(list[1].file.as_ref().unwrap().strings.len(), 1);

        let c = Coverage {
            translated: 1,
            total: 1000,
        };
        assert_eq!(c.percent(), 1);
        let c = Coverage {
            translated: 0,
            total: 1000,
        };
        assert_eq!(c.percent(), 0);
        assert!(!c.complete());
        let c = Coverage {
            translated: 999,
            total: 1000,
        };
        assert_eq!(c.percent(), 99);
        let c = Coverage {
            translated: 620,
            total: 1000,
        };
        assert_eq!(c.percent(), 62);
        assert_eq!(Coverage::default().percent(), 100);
    }

    #[test]
    fn templates_round_trip() {
        let english = i18n::english();
        let en = dump_template("en", None).unwrap();
        assert!(en.contains("[meta]\nname = \"English\""));
        assert!(en.contains("plural = \"one-other\""));
        assert!(en.contains("# --- settings ---\n"));
        assert!(!en.contains("# en: "));
        let f = LocaleFile::parse(&en, english).unwrap();
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.strings.len(), english.len());
        for (k, v) in english {
            assert_eq!(f.strings[*k], *v, "{k}");
        }
        // Russian: an English comment before each string.
        let ru = dump_template("ru", None).unwrap();
        assert!(ru.contains("plural = \"east-slavic\""));
        let sign_in = format!(
            "# en: \"{}\"\n\"login.sign_in\" = \"{}\"\n",
            english["login.sign_in"],
            rust_i18n::t!("login.sign_in", locale = "ru")
        );
        assert!(ru.contains(&sign_in), "{sign_in}");
        let f = LocaleFile::parse(&ru, english).unwrap();
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.meta.plural, Some(PluralRule::EastSlavic));
        assert_eq!(f.strings.len(), i18n::bundled_messages("ru").len());
        let th = dump_template("th", None).unwrap();
        assert!(th.contains("name = \"ไทย\"") && th.contains("plural = \"none\""));
        assert!(LocaleFile::parse(&th, english).unwrap().warnings.is_empty());
        // An external file: its strings, the others commented out.
        let file = LocaleFile::parse(
            "[meta]\nname = \"Deutsch\"\n[strings]\n\"login.sign_in\" = \"Anmelden\"\n",
            english,
        )
        .unwrap();
        let de = dump_template("de", Some(&file)).unwrap();
        assert!(de.contains("\"login.sign_in\" = \"Anmelden\"\n"));
        assert!(de.contains("\n# \"login.") && de.contains("# en: "));
        let again = LocaleFile::parse(&de, english).unwrap();
        assert_eq!(again.strings, file.strings);
        assert_eq!(dump_template("de", None), None);
        assert!(de.contains("\n# author = \"\"\n"));
        // The author is kept; text skipped for its placeholders stays as a comment.
        let file = LocaleFile::parse(
            "[meta]\nname = \"Deutsch\"\nauthor = \"Max\"\n[strings]\n\"teams.members_count_one\" = \"ein Mitglied\"\n",
            english,
        )
        .unwrap();
        assert_eq!(file.rejected["teams.members_count_one"], "ein Mitglied");
        let de = dump_template("de", Some(&file)).unwrap();
        assert!(de.contains("\nauthor = \"Max\"\n"));
        assert!(de.contains("\n# \"teams.members_count_one\" = \"ein Mitglied\"\n"));
        assert!(quote("a\"b\\c\nd\u{1}").eq("\"a\\\"b\\\\c\\nd\\u0001\""));
    }

    #[test]
    fn dump_reports_file_problems() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("de.toml");
        std::fs::write(&path, "[meta\nname = \"Deutsch\"").unwrap();
        let e = dump_file("de", Some(&path)).unwrap_err();
        assert!(
            e.contains(&path.display().to_string()) && e.contains("line 1"),
            "{e}"
        );
        std::fs::write(
            &path,
            "[meta]\nname = \"Deutsch\"\nplural = \"dual\"\nx = 1\n",
        )
        .unwrap();
        let d = dump_file("de", Some(&path)).unwrap();
        let w = d.warnings.unwrap();
        assert!(w.contains(&path.display().to_string()), "{w}");
        assert!(w.contains("meta.x") && w.contains("dual"), "{w}");
        assert!(d.template.contains("name = \"Deutsch\""));
        assert!(dump_file("de", None).is_err());
        assert!(dump_file("ru", None).unwrap().warnings.is_none());
    }
}
