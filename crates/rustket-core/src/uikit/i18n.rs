//! App translations (`GET /api/apps/languages`) and i18next-style lookup:
//! `{{name}}` interpolation, plural suffixes from `count`, context suffixes
//! (docs/uikit.md §2.1, §6.3, §6.4).

use std::collections::HashMap;

use rocketchat::models::{ActionButton, AppLanguages, LanguageMap};
use rocketchat::uikit::TextObject;
use serde_json::{Map, Value};

/// Translations of every app, by app id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppTranslations {
    apps: HashMap<String, LanguageMap>,
}

/// `pt-br` → `pt-BR`, `EN` → `en`, `zh_hans` → `zh-HANS`.
pub fn normalize_lang(lang: &str) -> String {
    let mut parts = lang.split(['-', '_']);
    let base = parts.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<String> = parts.map(str::to_ascii_uppercase).collect();
    if rest.is_empty() {
        base
    } else {
        format!("{base}-{}", rest.join("-"))
    }
}

/// CLDR plural category of `n` for `lang` (enough for the languages Rustket
/// ships plus a generic one/other rule).
pub fn plural_category(lang: &str, n: f64) -> &'static str {
    let base = normalize_lang(lang);
    let base = base.split('-').next().unwrap_or_default();
    let int = n.fract() == 0.0 && n >= 0.0;
    match base {
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" if int => {
            let i = n as u64;
            let (m10, m100) = (i % 10, i % 100);
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "many"
            }
        }
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" => "other",
        "ja" | "zh" | "ko" | "vi" | "th" | "id" | "ms" => "other",
        "fr" | "pt" if (0.0..2.0).contains(&n) => "one",
        _ if n == 1.0 => "one",
        _ => "other",
    }
}

/// Replaces `{{name}}` / `{{- name}}` with `args[name]`; unknown names stay.
pub fn interpolate(template: &str, args: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = after[..end].trim().trim_start_matches('-').trim();
        match args.get(name) {
            Some(Value::String(s)) => out.push_str(s),
            Some(Value::Null) | None => out.push_str(&rest[start..start + 2 + end + 2]),
            Some(v) => out.push_str(&v.to_string()),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

impl AppTranslations {
    /// Replaces everything with a `GET /api/apps/languages` result.
    pub fn set_all(&mut self, apps: Vec<AppLanguages>) {
        self.apps = apps.into_iter().map(|a| (a.app_id, a.languages)).collect();
    }

    /// Sets one app's bundle (`GET /api/apps/{id}/languages`, banner dictionaries).
    pub fn set_app(&mut self, app_id: &str, languages: LanguageMap) {
        self.apps.insert(app_id.to_owned(), languages);
    }

    pub fn remove_app(&mut self, app_id: &str) {
        self.apps.remove(app_id);
    }

    /// Raw lookup: user language, its base, then `en`.
    pub fn lookup(&self, app_id: &str, lang: &str, key: &str) -> Option<&str> {
        let bundles = self.apps.get(app_id)?;
        let lang = normalize_lang(lang);
        let base = lang.split('-').next().unwrap_or_default().to_owned();
        let find = |l: &str| -> Option<&str> {
            bundles
                .iter()
                .find(|(k, _)| normalize_lang(k) == l)
                .and_then(|(_, m)| m.get(key))
                .map(String::as_str)
        };
        find(&lang)
            .or_else(|| (base != lang).then(|| find(&base)).flatten())
            .or_else(|| find("en"))
    }

    /// i18next-style translation: tries `key_{context}_{plural}`,
    /// `key_{context}`, `key_{plural}`, `key_plural` (legacy), `key`, then
    /// interpolates `args`.
    pub fn translate(
        &self,
        app_id: &str,
        lang: &str,
        key: &str,
        args: &Map<String, Value>,
    ) -> Option<String> {
        let count = args.get("count").and_then(|c| match c {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        });
        let context = args
            .get("context")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty());
        let plural = count.map(|n| plural_category(lang, n));
        let mut candidates = Vec::with_capacity(5);
        if let (Some(c), Some(p)) = (context, plural) {
            candidates.push(format!("{key}_{c}_{p}"));
        }
        if let Some(c) = context {
            candidates.push(format!("{key}_{c}"));
        }
        if let Some(p) = plural {
            candidates.push(format!("{key}_{p}"));
            if p != "one" {
                candidates.push(format!("{key}_plural"));
            }
        }
        candidates.push(key.to_owned());
        candidates
            .iter()
            .find_map(|k| self.lookup(app_id, lang, k))
            .map(|t| interpolate(t, args))
    }

    /// The text to show for a text object (§2.1): its `i18n` key in namespace
    /// `ns` (`app-{id}`) or the app's own, falling back to `text`. Core apps
    /// (`*-core`) refer to Rocket.Chat's translations, which Rustket lacks, so
    /// they always show `text`.
    pub fn text(&self, t: &TextObject, app_id: Option<&str>, lang: &str) -> String {
        let Some(i18n) = &t.i18n else {
            return t.text.clone();
        };
        let ns_app = i18n
            .ns
            .as_deref()
            .and_then(|ns| ns.strip_prefix("app-"))
            .or(app_id);
        let Some(app) = ns_app.filter(|a| !a.ends_with("-core") || i18n.ns.is_some()) else {
            return t.text.clone();
        };
        self.translate(app, lang, &i18n.key, &i18n.args)
            .unwrap_or_else(|| t.text.clone())
    }

    /// Label of an app action button: `labelI18n` in the app's bundle, else the key.
    pub fn button_label(&self, b: &ActionButton, lang: &str) -> String {
        self.translate(&b.app_id, lang, &b.label_i18n, &Map::new())
            .unwrap_or_else(|| b.label_i18n.clone())
    }

    /// An app i18n key (slash command description/params, setting labels),
    /// else the key itself.
    pub fn key(&self, app_id: &str, lang: &str, key: &str) -> String {
        self.translate(app_id, lang, key, &Map::new())
            .unwrap_or_else(|| key.to_owned())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::language_map;
    use serde_json::json;

    fn tr() -> AppTranslations {
        let mut t = AppTranslations::default();
        t.set_all(vec![AppLanguages {
            app_id: "poll".into(),
            languages: language_map(&json!({
                "en": {"greeting": "Hello {{name}}", "votes_one": "{{count}} vote",
                       "votes_other": "{{count}} votes", "friend_female": "girlfriend",
                       "friend": "friend", "label": "Create poll", "nested": {"key": "deep"}},
                "pt-BR": {"label": "Criar enquete"},
                "ru": {"votes_one": "{{count}} голос", "votes_few": "{{count}} голоса",
                       "votes_many": "{{count}} голосов"}
            })),
        }]);
        t
    }

    #[test]
    fn lookup_falls_back_by_language() {
        let t = tr();
        assert_eq!(t.lookup("poll", "pt-br", "label"), Some("Criar enquete"));
        assert_eq!(t.lookup("poll", "pt", "label"), Some("Create poll"));
        assert_eq!(t.lookup("poll", "de", "label"), Some("Create poll"));
        assert_eq!(t.lookup("poll", "en", "nested.key"), Some("deep"));
        assert_eq!(t.lookup("other", "en", "label"), None);
    }

    #[test]
    fn plurals_context_and_interpolation() {
        let t = tr();
        let args = |v: Value| v.as_object().cloned().unwrap();
        assert_eq!(
            t.translate("poll", "en", "greeting", &args(json!({"name": "Ana"}))),
            Some("Hello Ana".into())
        );
        assert_eq!(
            t.translate("poll", "en", "votes", &args(json!({"count": 1}))),
            Some("1 vote".into())
        );
        assert_eq!(
            t.translate("poll", "en", "votes", &args(json!({"count": 5}))),
            Some("5 votes".into())
        );
        for (n, want) in [
            (1, "1 голос"),
            (3, "3 голоса"),
            (5, "5 голосов"),
            (21, "21 голос"),
            (12, "12 голосов"),
        ] {
            assert_eq!(
                t.translate("poll", "ru", "votes", &args(json!({"count": n}))),
                Some(want.into())
            );
        }
        assert_eq!(
            t.translate("poll", "en", "friend", &args(json!({"context": "female"}))),
            Some("girlfriend".into())
        );
        assert_eq!(
            t.translate("poll", "en", "friend", &args(json!({"context": "male"}))),
            Some("friend".into())
        );
        assert_eq!(
            interpolate("{{a}} {{- b}} {{c}}", &args(json!({"a": 1, "b": "x"}))),
            "1 x {{c}}"
        );
        assert_eq!(interpolate("open {{", &Map::new()), "open {{");
    }

    #[test]
    fn text_objects_and_core_apps() {
        let t = tr();
        let obj: TextObject = serde_json::from_value(json!({
            "type": "plain_text", "text": "fallback",
            "i18n": {"key": "greeting", "args": {"name": "Bo"}}
        }))
        .unwrap();
        assert_eq!(t.text(&obj, Some("poll"), "en"), "Hello Bo");
        assert_eq!(t.text(&obj, Some("videoconf-core"), "en"), "fallback");
        assert_eq!(t.text(&obj, None, "en"), "fallback");
        let missing: TextObject = serde_json::from_value(json!({
            "type": "plain_text", "text": "fallback", "i18n": {"key": "nope"}
        }))
        .unwrap();
        assert_eq!(t.text(&missing, Some("poll"), "en"), "fallback");
        let ns: TextObject = serde_json::from_value(json!({
            "type": "plain_text", "text": "x", "i18n": {"key": "label", "ns": "app-poll"}
        }))
        .unwrap();
        assert_eq!(t.text(&ns, Some("other-core"), "pt-BR"), "Criar enquete");
    }

    #[test]
    fn button_labels() {
        let t = tr();
        let b = ActionButton {
            app_id: "poll".into(),
            label_i18n: "label".into(),
            ..Default::default()
        };
        assert_eq!(t.button_label(&b, "en"), "Create poll");
        let raw = ActionButton {
            label_i18n: "raw_key".into(),
            ..b
        };
        assert_eq!(t.button_label(&raw, "en"), "raw_key");
        assert_eq!(normalize_lang("pt_br"), "pt-BR");
    }
}
