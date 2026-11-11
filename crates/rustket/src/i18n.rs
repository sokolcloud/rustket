//! Locale selection. Strings live in `locales/<component>.yml` (rust-i18n
//! `_version: 2`, one file per component, every key carries `en` and `ru`).
//! Use `rust_i18n::t!("component.key")` everywhere a user sees text.

use rustket_core::config::Language;

pub const SUPPORTED: &[&str] = &["en", "ru"];

/// Resolves the configured language to a supported locale code.
pub fn resolve(lang: Language) -> &'static str {
    match lang {
        Language::En => "en",
        Language::Ru => "ru",
        Language::System => from_env(|k| std::env::var(k).ok()),
    }
}

/// Locale from `LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG` (first non-empty wins).
pub fn from_env(get: impl Fn(&str) -> Option<String>) -> &'static str {
    for key in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Some(v) = get(key).filter(|v| !v.is_empty() && v != "C" && v != "POSIX") {
            let code = v
                .split([':', '.', '@'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            return if code.starts_with("ru") { "ru" } else { "en" };
        }
    }
    "en"
}

pub fn apply(lang: Language) {
    rust_i18n::set_locale(resolve(lang));
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
        assert_eq!(from_env(env(&[("LANG", "ru_RU.UTF-8")])), "ru");
        assert_eq!(from_env(env(&[("LANG", "de_DE.UTF-8")])), "en");
        assert_eq!(
            from_env(env(&[("LC_ALL", "en_US.UTF-8"), ("LANG", "ru_RU.UTF-8")])),
            "en"
        );
        assert_eq!(from_env(env(&[("LC_ALL", "C"), ("LANG", "ru_RU")])), "ru");
        assert_eq!(from_env(env(&[])), "en");
    }

    /// `key -> (en, ru)` of every `locales/*.yml` (the flat `_version: 2`
    /// layout this crate uses: a key line, then indented `en:` / `ru:` lines).
    fn locale_entries() -> std::collections::BTreeMap<String, (String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut out = std::collections::BTreeMap::new();
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
                        out.insert(k.clone(), (String::new(), String::new()))
                            .is_none(),
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
                let e = out.get_mut(&k).unwrap();
                match lang {
                    "en" => e.0 = value,
                    "ru" => e.1 = value,
                    other => panic!("unexpected locale {other} for {k}"),
                }
            }
        }
        out
    }

    /// Review #30: every key has a non-empty en and ru text, and every literal
    /// `t!("…")` key used in the sources is defined.
    #[test]
    fn translations_complete() {
        let entries = locale_entries();
        assert!(entries.len() > 150);
        for (k, (en, ru)) in &entries {
            assert!(!en.is_empty(), "{k}: missing en");
            assert!(!ru.is_empty(), "{k}: missing ru");
            // Placeholders match between languages.
            let ph = |s: &str| {
                let mut v: Vec<String> = s
                    .split("%{")
                    .skip(1)
                    .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
                    .collect();
                v.sort();
                v
            };
            assert_eq!(ph(en), ph(ru), "{k}: placeholders differ");
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
        for loc in SUPPORTED {
            assert_ne!(
                rust_i18n::t!("login.sign_in", locale = loc),
                "login.sign_in"
            );
        }
        assert_ne!(
            rust_i18n::t!("login.sign_in", locale = "ru"),
            rust_i18n::t!("login.sign_in", locale = "en")
        );
    }
}
