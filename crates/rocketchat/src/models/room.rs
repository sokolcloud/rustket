use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Message, RoomType, UserRef, lenient};
use crate::RcDate;

/// IRoom (channels, groups, DMs, discussions, teams, omnichannel).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Room {
    #[serde(rename = "_id")]
    pub id: String,
    pub t: RoomType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announcement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ro: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    #[serde(rename = "e2eKeyId", skip_serializing_if = "Option::is_none")]
    pub e2e_key_id: Option<String>,
    /// Members waiting for the room key (≤ 50); a hint for key distribution.
    #[serde(
        rename = "usersWaitingForE2EKeys",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub users_waiting_for_e2e_keys: Option<Vec<WaitingUser>>,
    /// Parent room of a discussion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_main: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usernames: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uids: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub last_message: Option<Box<Message>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub lm: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub msgs: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub users_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmuted: Option<Vec<String>>,
    /// `true`, `false` or a list of hidden system message types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sys_mes: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub featured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub react_when_read_only: Option<bool>,
    /// Joining needs a code (`joinCode`) unless the user has
    /// `join-without-join-code`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_code_required: Option<bool>,
    #[serde(rename = "avatarETag", skip_serializing_if = "Option::is_none")]
    pub avatar_etag: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub u: Option<UserRef>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub federated: Option<bool>,
    // Omnichannel (kept loose).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_by: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_hold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

impl Room {
    /// `fname` if set, else `name`.
    pub fn display_name(&self) -> &str {
        self.fname
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.name.as_deref())
            .unwrap_or_default()
    }

    pub fn is_discussion(&self) -> bool {
        self.prid.is_some()
    }

    pub fn is_team(&self) -> bool {
        self.team_main == Some(true)
    }
}

/// Entry of `usersWaitingForE2EKeys`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WaitingUser {
    pub user_id: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
}

/// `teams.info` → `teamInfo`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamInfo {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    /// 0 = public, 1 = private.
    #[serde(rename = "type")]
    pub kind: u8,
    pub room_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<UserRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<RcDate>,
    #[serde(rename = "_updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<RcDate>,
}
