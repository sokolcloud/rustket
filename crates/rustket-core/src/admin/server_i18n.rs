//! Server-defined strings of the admin screen (setting labels, hints, alerts,
//! sections, permission names, server error keys) from the server's own
//! translation files (docs/admin.md §14.12). Rustket's own UI strings stay in the
//! rust-i18n layer.

use std::collections::HashMap;
use std::sync::Arc;

use rocketchat::models::admin::{SelectOption, SettingDef};
use serde_json::Value;

/// Lookup chain: `Custom_Translations[lang]` → server file for `lang` → server
/// `en` → not found.
#[derive(Debug, Clone, Default)]
pub struct ServerI18n {
    pub lang: String,
    custom: HashMap<String, String>,
    translations: Arc<HashMap<String, String>>,
    en: Arc<HashMap<String, String>>,
}

impl ServerI18n {
    /// `translations` is the file for `lang` (pass the same map as `en` for English).
    pub fn new(
        lang: &str,
        translations: Arc<HashMap<String, String>>,
        en: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            lang: lang.to_owned(),
            custom: HashMap::new(),
            translations,
            en,
        }
    }

    /// Applies the public setting `Custom_Translations` (JSON
    /// `{"<lng>": {"key": "text"}}`); invalid JSON is ignored.
    pub fn with_custom_translations(mut self, setting_value: &str) -> Self {
        self.custom = parse_custom_translations(setting_value, &self.lang);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.custom.is_empty() && self.translations.is_empty() && self.en.is_empty()
    }

    pub fn t(&self, key: &str) -> Option<&str> {
        self.custom
            .get(key)
            .or_else(|| self.translations.get(key))
            .or_else(|| self.en.get(key))
            .map(String::as_str)
    }

    /// `t(key)` or the key itself.
    pub fn t_or_key<'a>(&'a self, key: &'a str) -> &'a str {
        self.t(key).unwrap_or(key)
    }

    /// `t(key)` with i18next `{{var}}` interpolation.
    pub fn t_with(&self, key: &str, vars: &[(&str, &str)]) -> Option<String> {
        self.t(key).map(|s| interpolate(s, vars))
    }

    /// Setting label: `t(i18nLabel)`, else `t(_id)`, else the raw `i18nLabel`, else
    /// `_id`.
    pub fn label_for(&self, def: &SettingDef) -> String {
        let label = def.i18n_label.as_deref().filter(|s| !s.is_empty());
        label
            .and_then(|l| self.t(l))
            .or_else(|| self.t(&def.id))
            .or(label)
            .unwrap_or(&def.id)
            .to_owned()
    }

    /// Setting hint (inline Markdown; do not interpolate `{{…}}`): only when the
    /// key exists.
    pub fn hint_for(&self, def: &SettingDef) -> Option<&str> {
        def.i18n_description
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|k| self.t(k))
    }

    /// Alert text (may contain `<b>`, `<br/>`, `<ul>`, `<li>`, `<a>` markup): the
    /// translation or the raw text.
    pub fn alert_for<'a>(&'a self, def: &'a SettingDef) -> Option<&'a str> {
        let a = def.alert.as_deref().filter(|s| !s.is_empty())?;
        Some(self.t(a).unwrap_or(a))
    }

    /// Section / tab / group title: the translation or the raw name.
    pub fn section_label<'a>(&'a self, name: &'a str) -> &'a str {
        self.t_or_key(name)
    }

    pub fn option_label<'a>(&'a self, opt: &'a SelectOption) -> &'a str {
        if opt.i18n_label.is_empty() {
            return &opt.key;
        }
        self.t_or_key(&opt.i18n_label)
    }

    /// Message of an `action` setting result: `t(message)` with `%s` replaced by
    /// `params` in order.
    pub fn action_message(&self, message: &str, params: &[Value]) -> String {
        sprintf(self.t_or_key(message), params)
    }
}

/// Entries of `Custom_Translations` for `lang`.
pub fn parse_custom_translations(setting_value: &str, lang: &str) -> HashMap<String, String> {
    let Ok(Value::Object(all)) = serde_json::from_str::<Value>(setting_value) else {
        return HashMap::new();
    };
    match all.get(lang) {
        Some(Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect(),
        _ => HashMap::new(),
    }
}

/// i18next `{{name}}` interpolation (unknown variables stay as they are).
pub fn interpolate(s: &str, vars: &[(&str, &str)]) -> String {
    let mut out = s.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
        out = out.replace(&format!("{{{{ {k} }}}}"), v);
    }
    out
}

/// `%s` substitution in order (sprintf as used by action results).
pub fn sprintf(s: &str, params: &[Value]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = params.iter();
    let mut rest = s;
    while let Some(i) = rest.find("%s") {
        out.push_str(&rest[..i]);
        match it.next() {
            Some(Value::String(p)) => out.push_str(p),
            Some(p) => out.push_str(&p.to_string()),
            None => out.push_str("%s"),
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// In-memory cache of parsed server translation files per server version and
/// language (they are public data).
#[derive(Debug, Default)]
pub struct ServerI18nCache {
    files: HashMap<(String, String), Arc<HashMap<String, String>>>,
}

impl ServerI18nCache {
    pub fn get(&self, version: &str, lang: &str) -> Option<Arc<HashMap<String, String>>> {
        self.files
            .get(&(version.to_owned(), lang.to_owned()))
            .cloned()
    }

    pub fn insert(
        &mut self,
        version: &str,
        lang: &str,
        file: HashMap<String, String>,
    ) -> Arc<HashMap<String, String>> {
        let file = Arc::new(file);
        self.files
            .insert((version.to_owned(), lang.to_owned()), file.clone());
        file
    }
}

/// Language codes of the server's translation files with English names, for the
/// `language` setting widget (plus "Default" = `""`).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("ar", "Arabic"),
    ("az", "Azerbaijani"),
    ("be-BY", "Belarusian"),
    ("bg", "Bulgarian"),
    ("bn-BD", "Bengali (Bangladesh)"),
    ("bn-IN", "Bengali (India)"),
    ("bs", "Bosnian"),
    ("ca", "Catalan"),
    ("cs", "Czech"),
    ("cy", "Welsh"),
    ("da", "Danish"),
    ("de-AT", "German (Austria)"),
    ("de-IN", "German (India)"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Spanish"),
    ("et", "Estonian"),
    ("eu", "Basque"),
    ("fa", "Persian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("gl", "Galician"),
    ("he", "Hebrew"),
    ("hi-IN", "Hindi (India)"),
    ("hi", "Hindi"),
    ("hr", "Croatian"),
    ("hu", "Hungarian"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ka-GE", "Georgian"),
    ("km", "Khmer"),
    ("ko", "Korean"),
    ("ku", "Kurdish"),
    ("lo", "Lao"),
    ("lt", "Lithuanian"),
    ("lv", "Latvian"),
    ("mn", "Mongolian"),
    ("ms-MY", "Malay"),
    ("nb", "Norwegian Bokmål"),
    ("nl", "Dutch"),
    ("nn", "Norwegian Nynorsk"),
    ("pa-IN", "Punjabi"),
    ("pl", "Polish"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("se", "Northern Sami"),
    ("si", "Sinhala"),
    ("sk-SK", "Slovak"),
    ("sl-SI", "Slovenian"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("sv", "Swedish"),
    ("ta-IN", "Tamil"),
    ("th-TH", "Thai"),
    ("tl", "Tagalog"),
    ("tr", "Turkish"),
    ("ug", "Uyghur"),
    ("uk", "Ukrainian"),
    ("vi-VN", "Vietnamese"),
    ("zh-HK", "Chinese (Hong Kong)"),
    ("zh-TW", "Chinese (Taiwan)"),
    ("zh", "Chinese"),
];

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_util::fixture;

    fn map(v: Value) -> Arc<HashMap<String, String>> {
        Arc::new(serde_json::from_value(v).unwrap())
    }

    #[test]
    fn fallback_chain() {
        let en = map(serde_json::json!({"A": "en A", "B": "en B", "C": "en C"}));
        let ru = map(serde_json::json!({"A": "ru A"}));
        let i = ServerI18n::new("ru", ru, en)
            .with_custom_translations(r#"{"ru": {"B": "custom B"}, "en": {"C": "x"}}"#);
        assert_eq!(i.t("A"), Some("ru A"));
        assert_eq!(i.t("B"), Some("custom B"));
        assert_eq!(i.t("C"), Some("en C"));
        assert_eq!(i.t("D"), None);
        assert_eq!(i.t_or_key("D"), "D");
        let bad = ServerI18n::default().with_custom_translations("not json");
        assert!(bad.is_empty());
    }

    #[test]
    fn labels_from_fixture() {
        let en = map(fixture("admin/i18n_en.json"));
        let ru = map(fixture("admin/i18n_ru.json"));
        let i = ServerI18n::new("ru", ru, en.clone());
        let defs: Vec<SettingDef> = rocketchat::models::admin::parse_private_settings(fixture(
            "admin/private_settings.json",
        ));
        let ldap = defs.iter().find(|d| d.id == "LDAP_Enable").unwrap();
        assert!(!i.label_for(ldap).is_empty());
        assert_ne!(i.label_for(ldap), "LDAP_Enable");
        // A label that is not a key falls back to the raw text.
        let raw = SettingDef {
            id: "x_id".into(),
            i18n_label: Some("logo (svg, png, jpg)".into()),
            ..SettingDef::default()
        };
        assert_eq!(i.label_for(&raw), "logo (svg, png, jpg)");
        let none = SettingDef {
            id: "x_id".into(),
            ..SettingDef::default()
        };
        assert_eq!(i.label_for(&none), "x_id");
        assert_eq!(i.hint_for(&none), None);
        assert_eq!(
            ServerI18n::new("en", en.clone(), en).t("Reset_section_settings"),
            Some("Restore defaults")
        );
    }

    #[test]
    fn formatting() {
        assert_eq!(
            sprintf("Sent %s of %s", &[Value::from(3), Value::from("5")]),
            "Sent 3 of 5"
        );
        assert_eq!(sprintf("x %s", &[]), "x %s");
        assert_eq!(
            interpolate("Go to {{url}} now", &[("url", "https://x")]),
            "Go to https://x now"
        );
        let mut c = ServerI18nCache::default();
        c.insert("8.9.1", "en", HashMap::new());
        assert!(c.get("8.9.1", "en").is_some());
        assert!(c.get("8.9.0", "en").is_none());
        assert_eq!(LANGUAGES.len(), 68);
    }

    #[test]
    fn truncated_file_is_unavailable() {
        let r = rocketchat::rest::admin::server_translations_from_bytes(br#"{"a":"b","c":"#);
        assert!(r.is_err());
        let ok =
            rocketchat::rest::admin::server_translations_from_bytes(br#"{"a":"b","n":1}"#).unwrap();
        assert_eq!(ok.len(), 1);
    }
}
