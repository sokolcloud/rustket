//! Private settings with full metadata (`private-settings/get`), see docs/admin.md §14.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::RcDate;
use crate::models::lenient;

/// Widget type of a setting (`ISetting.type`). Unknown types decode as
/// [`SettingType::Unknown`] and should be rendered read-only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum SettingType {
    Boolean,
    #[default]
    String,
    RelativeUrl,
    Password,
    Int,
    Select,
    MultiSelect,
    Language,
    Color,
    Font,
    Code,
    Action,
    Asset,
    RoomPick,
    Group,
    Date,
    Lookup,
    Range,
    Timespan,
    Timezone,
    Unknown(String),
}

impl SettingType {
    pub fn parse(s: &str) -> Self {
        match s {
            "boolean" => Self::Boolean,
            "string" => Self::String,
            "relativeUrl" => Self::RelativeUrl,
            "password" => Self::Password,
            "int" => Self::Int,
            "select" => Self::Select,
            "multiSelect" => Self::MultiSelect,
            "language" => Self::Language,
            "color" => Self::Color,
            "font" => Self::Font,
            "code" => Self::Code,
            "action" => Self::Action,
            "asset" => Self::Asset,
            "roomPick" => Self::RoomPick,
            "group" => Self::Group,
            "date" => Self::Date,
            "lookup" => Self::Lookup,
            "range" => Self::Range,
            "timespan" => Self::Timespan,
            "timezone" => Self::Timezone,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::RelativeUrl => "relativeUrl",
            Self::Password => "password",
            Self::Int => "int",
            Self::Select => "select",
            Self::MultiSelect => "multiSelect",
            Self::Language => "language",
            Self::Color => "color",
            Self::Font => "font",
            Self::Code => "code",
            Self::Action => "action",
            Self::Asset => "asset",
            Self::RoomPick => "roomPick",
            Self::Group => "group",
            Self::Date => "date",
            Self::Lookup => "lookup",
            Self::Range => "range",
            Self::Timespan => "timespan",
            Self::Timezone => "timezone",
            Self::Unknown(s) => s,
        }
    }

    /// Types whose value the server checks as an integer (`int`, `range`, `timespan`).
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Int | Self::Range | Self::Timespan)
    }
}

impl Serialize for SettingType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SettingType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::String(s) => Self::parse(&s),
            _ => Self::Unknown(String::new()),
        })
    }
}

/// `enableQuery` / `displayQuery`: one Mongo filter or a list of filters. On 8.9 it
/// is a JSON string; the JSON value itself is accepted too. An unparsable string
/// decodes as `None` (logged at debug level).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingQuery {
    /// Every filter must match at least one setting document.
    pub filters: Vec<Value>,
}

impl SettingQuery {
    pub fn from_value(v: Value) -> Option<Self> {
        let v = match v {
            Value::String(s) => match serde_json::from_str::<Value>(&s) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, "invalid setting query");
                    return None;
                }
            },
            Value::Null => return None,
            v => v,
        };
        match v {
            Value::Array(filters) => Some(Self { filters }),
            Value::Object(_) => Some(Self { filters: vec![v] }),
            _ => None,
        }
    }
}

impl Serialize for SettingQuery {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Value::Array(self.filters.clone()).to_string().serialize(s)
    }
}

fn opt_query<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SettingQuery>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(SettingQuery::from_value))
}

/// Option of a `select` / `multiSelect` setting. Keys are strings on 8.9; numbers
/// are converted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SelectOption {
    #[serde(deserialize_with = "string_or_number")]
    pub key: String,
    pub i18n_label: String,
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// `fileConstraints` of an `asset` setting.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileConstraints {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub extensions: Vec<String>,
}

/// A setting document with full metadata (`ISetting`), as returned by
/// `private-settings/get`. Values of password/secret settings are redacted in `Debug`.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingDef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: SettingType,
    #[serde(deserialize_with = "lenient::opt")]
    pub group: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub section: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub tab: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub i18n_label: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub i18n_description: Option<String>,
    #[serde(deserialize_with = "sorter")]
    pub sorter: i64,
    pub value: Option<Value>,
    pub package_value: Option<Value>,
    #[serde(deserialize_with = "lenient::opt")]
    pub value_source: Option<String>,
    pub process_env_value: Option<Value>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub blocked: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub hidden: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub readonly: bool,
    #[serde(
        deserialize_with = "opt_query",
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_query: Option<SettingQuery>,
    #[serde(
        deserialize_with = "opt_query",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_query: Option<SettingQuery>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub enterprise: bool,
    #[serde(deserialize_with = "lenient::vec")]
    pub modules: Vec<String>,
    pub invalid_value: Option<Value>,
    #[serde(deserialize_with = "lenient::vec")]
    pub values: Vec<SelectOption>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub multiline: bool,
    #[serde(deserialize_with = "lenient::opt")]
    pub placeholder: Option<String>,
    /// Editor mode of a `code` setting (`application/json`, `text/css`, …).
    #[serde(deserialize_with = "lenient::opt")]
    pub code: Option<String>,
    /// `color` settings: `color` or `expression`.
    #[serde(deserialize_with = "lenient::opt")]
    pub editor: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub package_editor: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub min_value: Option<i64>,
    #[serde(deserialize_with = "lenient::opt")]
    pub max_value: Option<i64>,
    #[serde(deserialize_with = "lenient::opt")]
    pub action_text: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub lookup_endpoint: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub asset: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub file_constraints: Option<FileConstraints>,
    /// i18n key (or literal text with markup) of a warning shown with the widget.
    #[serde(deserialize_with = "lenient::opt")]
    pub alert: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub secret: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub public: bool,
    #[serde(deserialize_with = "lenient::opt")]
    pub autocomplete: Option<bool>,
    /// Server-side validation rules (not evaluated by the client).
    pub validation: Option<Value>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
}

fn sorter<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        _ => 0,
    })
}

impl SettingDef {
    /// Password-type, `secret: true`, or an id that names a secret (custom OAuth
    /// secrets are plain `string` settings without `secret`): never log or persist
    /// the value.
    pub fn is_sensitive(&self) -> bool {
        self.secret || self.kind == SettingType::Password || is_sensitive_id(&self.id)
    }

    pub fn is_group(&self) -> bool {
        self.kind == SettingType::Group
    }

    /// The value as a document for `enableQuery`/`displayQuery` evaluation, with
    /// `value` (and `editor`) replaced by the given (edited) state.
    pub fn query_document(&self, value: Option<&Value>, editor: Option<&str>) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("_id".into(), Value::String(self.id.clone()));
        m.insert("type".into(), Value::String(self.kind.as_str().to_owned()));
        if let Some(v) = value {
            m.insert("value".into(), v.clone());
        }
        if let Some(v) = &self.package_value {
            m.insert("packageValue".into(), v.clone());
        }
        if let Some(g) = &self.group {
            m.insert("group".into(), Value::String(g.clone()));
        }
        if let Some(s) = &self.section {
            m.insert("section".into(), Value::String(s.clone()));
        }
        if let Some(e) = editor {
            m.insert("editor".into(), Value::String(e.to_owned()));
        }
        m.insert("public".into(), Value::Bool(self.public));
        m.insert("blocked".into(), Value::Bool(self.blocked));
        m.insert("readonly".into(), Value::Bool(self.readonly));
        m.insert("enterprise".into(), Value::Bool(self.enterprise));
        m.insert("secret".into(), Value::Bool(self.secret));
        Value::Object(m)
    }
}

impl fmt::Debug for SettingDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("SettingDef");
        d.field("id", &self.id)
            .field("kind", &self.kind)
            .field("group", &self.group)
            .field("section", &self.section);
        // Values are never printed: not every secret is marked as one.
        d.finish_non_exhaustive()
    }
}

/// Setting ids whose value is a credential by naming convention
/// (`Accounts_OAuth_Custom-X-secret`, `…_Password`, `…_Token`, `…_Key`).
pub fn is_sensitive_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    [
        "secret",
        "password",
        "token",
        "_key",
        "-key",
        "privatekey",
        "apikey",
    ]
    .iter()
    .any(|s| lower.ends_with(s))
}

/// Decodes the result of `private-settings/get` without a date (a plain array);
/// invalid entries are skipped.
pub fn parse_private_settings(v: Value) -> Vec<SettingDef> {
    lenient::counted(v).items
}

/// One item of a bulk settings save (`POST /api/v1/settings`). `value` is redacted
/// in `Debug`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingChange {
    #[serde(rename = "_id")]
    pub id: String,
    pub value: Value,
    /// Only for `color` settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
}

impl fmt::Debug for SettingChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingChange")
            .field("id", &self.id)
            .field("editor", &self.editor)
            .finish_non_exhaustive()
    }
}

/// Action of a `private-settings-changed` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingEventAction {
    Inserted,
    Updated,
    Removed,
    Other,
}

/// Partial setting of a `stream-notify-logged` `private-settings-changed` event:
/// `{_id, value, editor?, properties?, enterprise?, requiredOnWizard?}`. For EE
/// settings `value` is the license-masked effective value; follow every event with a
/// delta `private-settings/get` (docs/admin.md §2.5).
#[derive(Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingPatch {
    #[serde(rename = "_id")]
    pub id: String,
    pub value: Option<Value>,
    #[serde(deserialize_with = "lenient::opt")]
    pub editor: Option<String>,
    pub properties: Option<Value>,
    #[serde(deserialize_with = "lenient::opt")]
    pub enterprise: Option<bool>,
}

impl fmt::Debug for SettingPatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingPatch")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SettingPatch {
    /// Decodes the event arguments `[action, partial setting]`.
    pub fn from_stream_args(args: &[Value]) -> Option<(SettingEventAction, Self)> {
        let action = match args.first()?.as_str()? {
            "inserted" => SettingEventAction::Inserted,
            "updated" => SettingEventAction::Updated,
            "removed" => SettingEventAction::Removed,
            _ => SettingEventAction::Other,
        };
        let patch: Self = serde_json::from_value(args.get(1)?.clone()).ok()?;
        (!patch.id.is_empty()).then_some((action, patch))
    }
}

/// Result of an `action` setting method / endpoint: `{message, params?}`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ActionResult {
    pub message: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub params: Vec<Value>,
}

/// `lookup` settings: `GET /api/<lookupEndpoint>` → `{data: [{key, label}]}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct LookupOption {
    #[serde(deserialize_with = "string_or_number")]
    pub key: String,
    pub label: String,
}

/// `saml.parseMetadata` result.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SamlMetadata {
    pub entry_point: Option<String>,
    #[serde(rename = "idpSLORedirectURL")]
    pub idp_slo_redirect_url: Option<String>,
    pub cert: Option<String>,
    pub identifier_format: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub warnings: Vec<Value>,
}
