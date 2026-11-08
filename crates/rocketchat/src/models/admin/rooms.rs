//! Room administration (docs/admin.md §5).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::models::{RoomType, UserRef, lenient};

/// A room with the admin projection (`rooms.adminRooms`, `rooms.adminRooms.getRoom`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AdminRoom {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(deserialize_with = "lenient::or_default")]
    pub t: RoomType,
    pub name: Option<String>,
    pub fname: Option<String>,
    /// Parent room of a discussion.
    pub prid: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub u: Option<UserRef>,
    #[serde(deserialize_with = "lenient::vec")]
    pub usernames: Vec<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub uids: Vec<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
    pub users_count: Option<u64>,
    pub msgs: Option<u64>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ro: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub react_when_read_only: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub default: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub favorite: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub featured: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub archived: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub broadcast: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub encrypted: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub federated: Option<bool>,
    pub team_id: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub team_main: Option<bool>,
    pub topic: Option<String>,
    pub announcement: Option<String>,
    pub description: Option<String>,
    pub custom_fields: Option<Value>,
    #[serde(rename = "avatarETag")]
    pub avatar_etag: Option<String>,
    #[serde(deserialize_with = "lenient::vec")]
    pub muted: Vec<String>,
}

impl AdminRoom {
    /// Display name: `fname`, `name`, or the DM usernames joined with " x " (like the web).
    pub fn display_name(&self) -> String {
        if let Some(n) = self.fname.as_deref().or(self.name.as_deref())
            && !n.is_empty()
        {
            return n.to_owned();
        }
        self.usernames.join(" x ")
    }

    pub fn is_discussion(&self) -> bool {
        self.prid.is_some()
    }

    pub fn is_team_main(&self) -> bool {
        self.team_main == Some(true)
    }
}

/// `types[]` filter of `rooms.adminRooms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdminRoomType {
    Channel,
    Private,
    Direct,
    Livechat,
    Discussions,
    Teams,
}

impl AdminRoomType {
    /// All six types: what the web sends without a type filter.
    pub const ALL: [Self; 6] = [
        Self::Channel,
        Self::Private,
        Self::Direct,
        Self::Livechat,
        Self::Discussions,
        Self::Teams,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Channel => "c",
            Self::Private => "p",
            Self::Direct => "d",
            Self::Livechat => "l",
            Self::Discussions => "discussions",
            Self::Teams => "teams",
        }
    }
}

/// `favorite` of `rooms.saveRoomSettings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteSetting {
    pub default_value: bool,
    pub favorite: bool,
}

/// `rooms.saveRoomSettings` body (without `rid`): only set fields are sent. The admin
/// form always sends `default` and `favorite` (docs/admin.md §5.2).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_announcement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_description: Option<String>,
    /// Data URL of the new avatar, or `null` to reset (send `Some(Value::Null)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_avatar: Option<Value>,
    /// `c` or `p`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub react_when_read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite: Option<FavoriteSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub featured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_messages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
}
