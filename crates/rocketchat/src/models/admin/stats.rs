//! Workspace statistics, instances, license (docs/admin.md §1, §3).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::RcDate;
use crate::models::lenient;

/// `GET statistics`: the fields shown on the Workspace page typed, everything else
/// kept in `extra` so "Download info" can re-serialise the whole document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Statistics {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `null` in a freshly generated snapshot (`refresh=true`).
    #[serde(
        rename = "_updatedAt",
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_at: Option<RcDate>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<RcDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unique_id: Option<String>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub online_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub busy_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub away_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub offline_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_connected_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub active_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub active_guests: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub non_active_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub app_users: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub uploads_total: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub uploads_total_size: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_channels: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_private_groups: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_direct: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_discussions: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_livechat: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_rooms: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_channel_messages: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_private_group_messages: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_direct_messages: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_discussions_messages: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_livechat_messages: Option<u64>,
    #[serde(deserialize_with = "count", skip_serializing_if = "Option::is_none")]
    pub total_messages: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mongo_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mongo_storage_engine: Option<String>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub process: Option<StatisticsProcess>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub migration: Option<StatisticsMigration>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub apps: Option<StatisticsApps>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A count that may arrive as a float.
fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        _ => None,
    })
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StatisticsProcess {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StatisticsMigration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub locked_at: Option<RcDate>,
    #[serde(
        deserialize_with = "lenient::opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub build_at: Option<RcDate>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StatisticsApps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_installed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_active: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_failed: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Entry of `instances.get`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Instance {
    pub address: Option<String>,
    pub current_status: InstanceStatus,
    pub instance_record: InstanceRecord,
    pub broadcast_auth: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InstanceStatus {
    pub connected: Option<bool>,
    pub last_heartbeat_time: Option<u64>,
    pub local: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InstanceRecord {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: Option<String>,
    pub pid: Option<u64>,
    #[serde(rename = "_createdAt", deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_updatedAt", deserialize_with = "lenient::opt")]
    pub updated_at: Option<RcDate>,
    /// `{host, port, tcpPort, os: {...}, nodeVersion, conns}`.
    pub extra_information: Option<Value>,
}

/// `licenses.info` → `license`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LicenseInfo {
    #[serde(deserialize_with = "lenient::vec")]
    pub active_modules: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub external_modules: Vec<Value>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub prevented_actions: BTreeMap<String, bool>,
    /// Empty for users without `view-privileged-setting`.
    #[serde(deserialize_with = "lenient::or_default")]
    pub limits: BTreeMap<String, LicenseLimit>,
    #[serde(deserialize_with = "lenient::vec")]
    pub tags: Vec<Value>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub trial: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub has_valid_license: bool,
    /// The license document itself (only for `view-privileged-setting`).
    pub license: Option<Value>,
    pub cloud_sync_announcement: Option<Value>,
}

impl LicenseInfo {
    pub fn has_module(&self, module: &str) -> bool {
        self.active_modules.iter().any(|m| m == module)
    }

    /// `preventedActions[action]` (e.g. `activeUsers`: creating users is blocked).
    pub fn is_prevented(&self, action: &str) -> bool {
        self.prevented_actions.get(action).copied().unwrap_or(false)
    }
}

/// `{max, value?}`; `max == -1` means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LicenseLimit {
    pub max: i64,
    pub value: Option<i64>,
}

/// `licenses.maxActiveUsers`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaxActiveUsers {
    pub max_active_users: Option<i64>,
    pub active_users: Option<i64>,
}

/// `cloud.registrationStatus` → `registrationStatus` (token redacted in `Debug`).
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CloudRegistrationStatus {
    pub workspace_registered: bool,
    pub workspace_id: Option<String>,
    pub unique_id: Option<String>,
    pub token: Option<String>,
    pub email: Option<String>,
}

impl fmt::Debug for CloudRegistrationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudRegistrationStatus")
            .field("workspace_registered", &self.workspace_registered)
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}
