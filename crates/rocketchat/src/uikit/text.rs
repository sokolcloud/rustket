//! UIKit text objects (`UK:blocks/text/*`, `UK:blocks/WithTranslations.ts`).

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// `type` of a text object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextKind {
    #[default]
    #[serde(rename = "plain_text")]
    Plain,
    #[serde(rename = "mrkdwn")]
    Mrkdwn,
    #[serde(other)]
    Unknown,
}

/// `i18n` of a text object: `t(key, {...args, ns, defaultValue: text})`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextI18n {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ns: Option<String>,
    #[serde(
        skip_serializing_if = "Map::is_empty",
        deserialize_with = "obj_or_empty"
    )]
    pub args: Map<String, Value>,
}

fn obj_or_empty<'de, D: Deserializer<'de>>(d: D) -> Result<Map<String, Value>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(m) => m,
        _ => Map::new(),
    })
}

/// `plain_text` / `mrkdwn`. Decodes leniently: a bare string is plain text, a
/// missing `text` is `""`, an invalid `i18n` is dropped.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TextObject {
    #[serde(rename = "type")]
    pub kind: TextKind,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbatim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub i18n: Option<TextI18n>,
}

impl TextObject {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn mrkdwn(text: impl Into<String>) -> Self {
        Self {
            kind: TextKind::Mrkdwn,
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn is_mrkdwn(&self) -> bool {
        self.kind == TextKind::Mrkdwn
    }

    /// Lenient conversion from JSON (`None` for values that are not text).
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(Self::plain(s.clone())),
            Value::Object(o) => {
                let kind = o
                    .get("type")
                    .cloned()
                    .and_then(|t| serde_json::from_value(t).ok())
                    .unwrap_or_default();
                let text = match o.get("text") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                Some(Self {
                    kind,
                    text,
                    emoji: o.get("emoji").and_then(Value::as_bool),
                    verbatim: o.get("verbatim").and_then(Value::as_bool),
                    i18n: o
                        .get("i18n")
                        .cloned()
                        .and_then(|i| serde_json::from_value::<TextI18n>(i).ok())
                        .filter(|i| !i.key.is_empty()),
                })
            }
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for TextObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Self::from_value(&v).ok_or_else(|| serde::de::Error::custom("expected a text object"))
    }
}

/// A banner `title`: a bare string or a text object.
#[derive(Debug, Clone, PartialEq)]
pub enum StrOrText {
    Str(String),
    Text(TextObject),
}

impl StrOrText {
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(Self::Str(s.clone())),
            Value::Object(_) => TextObject::from_value(v).map(Self::Text),
            _ => None,
        }
    }

    /// The raw text (no i18n).
    pub fn text(&self) -> &str {
        match self {
            Self::Str(s) => s,
            Self::Text(t) => &t.text,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_text_objects_leniently() {
        let t: TextObject = serde_json::from_value(json!({
            "type": "plain_text", "text": "Hello", "emoji": true,
            "i18n": {"key": "greeting", "ns": "app-x", "args": {"name": "Ana", "count": 2}}
        }))
        .unwrap();
        assert_eq!(t.kind, TextKind::Plain);
        assert_eq!(t.text, "Hello");
        let i = t.i18n.unwrap();
        assert_eq!(i.key, "greeting");
        assert_eq!(i.ns.as_deref(), Some("app-x"));
        assert_eq!(i.args["count"], 2);

        let m: TextObject = serde_json::from_value(json!({"type": "mrkdwn"})).unwrap();
        assert!(m.is_mrkdwn());
        assert_eq!(m.text, "");

        let s: TextObject = serde_json::from_value(json!("bare")).unwrap();
        assert_eq!(s, TextObject::plain("bare"));

        let u: TextObject =
            serde_json::from_value(json!({"type": "rich", "text": "x", "i18n": 5})).unwrap();
        assert_eq!(u.kind, TextKind::Unknown);
        assert!(u.i18n.is_none());

        assert!(serde_json::from_value::<TextObject>(json!(5)).is_err());
    }

    #[test]
    fn banner_title_forms() {
        assert_eq!(
            StrOrText::from_value(&json!("T")),
            Some(StrOrText::Str("T".into()))
        );
        let t = StrOrText::from_value(&json!({"type": "plain_text", "text": "X"})).unwrap();
        assert_eq!(t.text(), "X");
    }
}
