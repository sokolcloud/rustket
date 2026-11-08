//! Apps-engine payloads: action buttons, installed apps, settings, logs,
//! translations, banners and the `stream-apps` events (docs/uikit.md §5.7, §6, §8).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::lenient;
use crate::RcDate;

/// `UIActionButtonContext`. A missing or unknown context is `Unknown`, which
/// is shown nowhere (the web compares the exact value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ButtonContext {
    #[serde(rename = "messageAction")]
    MessageAction,
    #[serde(rename = "roomAction")]
    RoomAction,
    #[serde(rename = "messageBoxAction")]
    MessageBoxAction,
    #[serde(rename = "userDropdownAction")]
    UserDropdownAction,
    #[serde(rename = "roomSideBarAction")]
    RoomSideBarAction,
    #[default]
    #[serde(rename = "unknown", other)]
    Unknown,
}

/// `when` of an action button.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionButtonWhen {
    /// `public_channel`, `private_channel`, `public_team`, `private_team`,
    /// `public_discussion`, `private_discussion`, `direct`, `direct_multiple`, `livechat`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub room_types: Option<Vec<String>>,
    /// `message`, `message-mobile`, `threads`, `starred` (default: all).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub message_action_context: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub has_one_permission: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub has_all_permissions: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub has_one_role: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub has_all_roles: Option<Vec<String>>,
}

/// `IUIActionButton` (`GET /api/apps/actionButtons`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionButton {
    pub app_id: String,
    pub action_id: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub context: ButtonContext,
    /// Key in the app's language bundle.
    #[serde(rename = "labelI18n")]
    pub label_i18n: String,
    /// `danger`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// `default` or `ai`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub when: Option<ActionButtonWhen>,
}

impl ActionButton {
    pub fn is_danger(&self) -> bool {
        self.variant.as_deref() == Some("danger")
    }

    pub fn is_ai(&self) -> bool {
        self.category.as_deref() == Some("ai")
    }
}

/// `AppStatus` (`AE:AppStatus.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStatus {
    #[default]
    Unknown,
    Constructed,
    Initialized,
    AutoEnabled,
    ManuallyEnabled,
    CompilerErrorDisabled,
    InvalidLicenseDisabled,
    InvalidInstallationDisabled,
    ErrorDisabled,
    ManuallyDisabled,
    InvalidSettingsDisabled,
    Disabled,
    #[serde(other)]
    Other,
}

impl AppStatus {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::AutoEnabled | Self::ManuallyEnabled)
    }

    pub fn is_disabled(self) -> bool {
        matches!(
            self,
            Self::CompilerErrorDisabled
                | Self::InvalidLicenseDisabled
                | Self::InvalidInstallationDisabled
                | Self::ErrorDisabled
                | Self::ManuallyDisabled
                | Self::InvalidSettingsDisabled
                | Self::Disabled
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown | Self::Other => "unknown",
            Self::Constructed => "constructed",
            Self::Initialized => "initialized",
            Self::AutoEnabled => "auto_enabled",
            Self::ManuallyEnabled => "manually_enabled",
            Self::CompilerErrorDisabled => "compiler_error_disabled",
            Self::InvalidLicenseDisabled => "invalid_license_disabled",
            Self::InvalidInstallationDisabled => "invalid_installation_disabled",
            Self::ErrorDisabled => "error_disabled",
            Self::ManuallyDisabled => "manually_disabled",
            Self::InvalidSettingsDisabled => "invalid_settings_disabled",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppAuthor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support: Option<String>,
}

/// `IPermission` of an app manifest.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPermission {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// `IAppInfoRest` (`GET /api/apps/installed`, `GET /api/apps/:id`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub name_slug: String,
    pub version: String,
    pub description: String,
    pub required_api_version: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub author: AppAuthor,
    pub class_file: String,
    pub icon_file: String,
    /// Base64 data URL of the icon (may be large; may be SVG).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_file_content: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub implements: Vec<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub essentials: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub permissions: Option<Vec<AppPermission>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addon: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub status: AppStatus,
    /// `languageContent`: `{lang: {key: text}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Value>,
    pub private: bool,
    pub migrated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license_validation: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_status: Option<Value>,
}

/// `{status, clusterStatus?}` of `GET /api/apps/:id/status`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppStatusInfo {
    #[serde(deserialize_with = "lenient::or_default")]
    pub status: AppStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_status: Option<Value>,
}

/// `{key, i18nLabel}` of a select setting.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettingValue {
    pub key: String,
    #[serde(rename = "i18nLabel")]
    pub i18n_label: String,
}

/// `ISetting` of an app. Unknown fields are kept in `extra` so a setting posted
/// back is the one received with a new `value`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSetting {
    pub id: String,
    /// `boolean`, `code`, `color`, `font`, `int`, `select`, `string`,
    /// `multiSelect`, `password`, `roomPick`.
    #[serde(rename = "type")]
    pub kind: String,
    pub package_value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub required: bool,
    pub public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub values: Option<Vec<AppSettingValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiline: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(rename = "i18nLabel")]
    pub i18n_label: String,
    #[serde(rename = "i18nDescription", skip_serializing_if = "Option::is_none")]
    pub i18n_description: Option<String>,
    #[serde(rename = "i18nAlert", skip_serializing_if = "Option::is_none")]
    pub i18n_alert: Option<String>,
    #[serde(rename = "i18nPlaceholder", skip_serializing_if = "Option::is_none")]
    pub i18n_placeholder: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl AppSetting {
    /// `value`, else `packageValue`.
    pub fn effective_value(&self) -> &Value {
        self.value
            .as_ref()
            .filter(|v| !v.is_null())
            .unwrap_or(&self.package_value)
    }
}

/// One entry of an app log item.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppLogEntry {
    /// `error`, `warning`, `info`, `log`, `debug`, `success`.
    pub severity: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub timestamp: Option<RcDate>,
    pub caller: String,
    /// Arguments as logged (strings, or JSON values on older servers).
    #[serde(deserialize_with = "lenient::vec")]
    pub args: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// `ILogItem`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppLogItem {
    #[serde(rename = "_id")]
    pub id: String,
    pub app_id: String,
    pub method: String,
    pub instance_id: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub start_time: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub end_time: Option<RcDate>,
    /// Milliseconds.
    #[serde(deserialize_with = "lenient::opt")]
    pub total_time: Option<f64>,
    #[serde(rename = "_createdAt", deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::vec")]
    pub entries: Vec<AppLogEntry>,
}

/// `{offset, logs, count, total}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppLogs {
    pub offset: u64,
    #[serde(deserialize_with = "lenient::vec")]
    pub logs: Vec<AppLogItem>,
    pub count: u64,
    pub total: u64,
}

/// Filters of `GET /api/apps/:id/logs` and `GET /api/apps/logs`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppLogsQuery {
    pub count: Option<u64>,
    pub offset: Option<u64>,
    /// `0` errors, `1` + warn/info/log, `2` + debug/success.
    pub log_level: Option<u8>,
    pub method: Option<String>,
    pub instance_id: Option<String>,
    /// ISO dates; applied to `_updatedAt`.
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    /// Mongo sort JSON, e.g. `{"_updatedAt":-1}`.
    pub sort: Option<String>,
}

/// `{instanceIds, methods}` of `GET /api/apps/:id/logs/distinctValues`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppLogsDistinct {
    #[serde(deserialize_with = "lenient::vec")]
    pub instance_ids: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub methods: Vec<String>,
}

/// `{lang: {key: text}}`. Non-string values are skipped; nested objects are
/// flattened with `.` (i18next key separator).
pub type LanguageMap = BTreeMap<String, BTreeMap<String, String>>;

fn flatten_into(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>, depth: usize) {
    match v {
        Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
        }
        Value::Object(o) if depth < 8 => {
            for (k, c) in o {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_into(&key, c, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Parses a `languages` object leniently.
pub fn language_map(v: &Value) -> LanguageMap {
    let mut out = LanguageMap::new();
    if let Value::Object(langs) = v {
        for (lang, keys) in langs {
            let mut m = BTreeMap::new();
            flatten_into("", keys, &mut m, 0);
            out.insert(lang.clone(), m);
        }
    }
    out
}

/// One app's translations (`GET /api/apps/languages`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppLanguages {
    pub app_id: String,
    pub languages: LanguageMap,
}

impl AppLanguages {
    pub fn from_value(v: &Value) -> Option<Self> {
        Some(Self {
            app_id: v.get("id")?.as_str()?.to_owned(),
            languages: v.get("languages").map(language_map).unwrap_or_default(),
        })
    }
}

/// `{totalMarketplaceEnabled, totalPrivateEnabled, maxMarketplaceApps, maxPrivateApps}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppsCount {
    pub total_marketplace_enabled: u64,
    pub total_private_enabled: u64,
    pub max_marketplace_apps: u64,
    pub max_private_apps: u64,
}

/// `surface` of a banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BannerSurface {
    #[default]
    Banner,
    Modal,
    #[serde(other)]
    Unknown,
}

/// `IBanner` (`GET /api/v1/banners?platform=web`). `view` stays raw JSON; parse
/// it with `uikit::RawView`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Banner {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "lenient::vec")]
    pub platform: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub expire_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub start_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub surface: BannerSurface,
    pub view: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary: Option<Value>,
}

impl Banner {
    /// `dictionary` as a language map.
    pub fn dictionary_map(&self) -> LanguageMap {
        self.dictionary
            .as_ref()
            .map(language_map)
            .unwrap_or_default()
    }
}

/// Stream name of the apps-engine events.
pub const STREAM_APPS: &str = "stream-apps";
/// The single `stream-apps` event Rustket subscribes (every change is wrapped in it).
pub const STREAM_APPS_EVENT: &str = "apps";

/// A `stream-apps` `apps` event: `[[key, args]]` (§6.5).
#[derive(Debug, Clone, PartialEq)]
pub enum AppsStreamEvent {
    Added(String),
    Removed(String),
    Updated(String),
    StatusUpdate {
        app_id: String,
        status: AppStatus,
    },
    SettingUpdated {
        app_id: String,
        setting: Value,
    },
    CommandAdded(String),
    CommandDisabled(String),
    CommandUpdated(String),
    CommandRemoved(String),
    ActionsChanged,
    /// A key this crate does not model (kept with its arguments).
    Unknown {
        key: String,
        args: Value,
    },
}

impl AppsStreamEvent {
    /// Decodes the `args` of a `stream-apps` `apps` data message.
    pub fn decode(args: &[Value]) -> Option<Self> {
        let pair = args.first()?.as_array()?;
        let key = pair.first()?.as_str()?;
        let inner = pair.get(1).cloned().unwrap_or(Value::Array(Vec::new()));
        let first = inner.as_array().and_then(|a| a.first()).cloned();
        let str_arg = || first.as_ref().and_then(Value::as_str).map(str::to_owned);
        let ev = match key {
            "app/added" => str_arg().map(Self::Added),
            "app/removed" => str_arg().map(Self::Removed),
            "app/updated" => str_arg().map(Self::Updated),
            "app/statusUpdate" => first.as_ref().and_then(|f| {
                Some(Self::StatusUpdate {
                    app_id: f.get("appId")?.as_str()?.to_owned(),
                    status: f
                        .get("status")
                        .cloned()
                        .and_then(|s| serde_json::from_value(s).ok())
                        .unwrap_or_default(),
                })
            }),
            "app/settingUpdated" => first.as_ref().and_then(|f| {
                Some(Self::SettingUpdated {
                    app_id: f.get("appId")?.as_str()?.to_owned(),
                    setting: f.get("setting").cloned().unwrap_or(Value::Null),
                })
            }),
            "command/added" => str_arg().map(Self::CommandAdded),
            "command/disabled" => str_arg().map(Self::CommandDisabled),
            "command/updated" => str_arg().map(Self::CommandUpdated),
            "command/removed" => str_arg().map(Self::CommandRemoved),
            "actions/changed" => Some(Self::ActionsChanged),
            _ => None,
        };
        Some(ev.unwrap_or_else(|| Self::Unknown {
            key: key.to_owned(),
            args: inner,
        }))
    }

    /// The apps-stream event carried by a session event (delivered as
    /// [`SessionEvent::Unknown`](crate::session::SessionEvent::Unknown) with
    /// collection `stream-apps`, event `apps`).
    pub fn from_session_event(ev: &crate::session::SessionEvent) -> Option<Self> {
        match ev {
            crate::session::SessionEvent::Unknown {
                collection,
                event_name,
                args,
            } if collection == STREAM_APPS && event_name == STREAM_APPS_EVENT => {
                Self::decode(args.as_array()?)
            }
            _ => None,
        }
    }

    /// Whether the slash-command list should be refetched.
    pub fn affects_commands(&self) -> bool {
        matches!(
            self,
            Self::CommandAdded(_)
                | Self::CommandDisabled(_)
                | Self::CommandUpdated(_)
                | Self::CommandRemoved(_)
        )
    }
}
