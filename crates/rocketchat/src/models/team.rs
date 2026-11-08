//! Teams (`ITeam`, `ITeamMember`) as returned by the `teams.*` endpoints
//! (docs/teams-videoconf.md §1, §2).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{Room, RoomType, UserRef, UserStatus, lenient};
use crate::RcDate;

/// `TeamType`: 0 public (main room `c`), 1 private (main room `p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TeamType {
    #[default]
    Public,
    Private,
    Unknown(u8),
}

impl TeamType {
    pub fn code(self) -> u8 {
        match self {
            Self::Public => 0,
            Self::Private => 1,
            Self::Unknown(n) => n,
        }
    }

    pub fn from_code(n: u8) -> Self {
        match n {
            0 => Self::Public,
            1 => Self::Private,
            n => Self::Unknown(n),
        }
    }

    /// Type of the main room.
    pub fn room_type(self) -> RoomType {
        match self {
            Self::Private => RoomType::Private,
            _ => RoomType::Channel,
        }
    }
}

impl Serialize for TeamType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for TeamType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(v.as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .map_or(Self::Unknown(u8::MAX), Self::from_code))
    }
}

/// `ITeam` (`teams.create` → `team`, `teams.info` → `teamInfo`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Team {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: TeamType,
    /// The main room.
    pub room_id: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub created_by: Option<UserRef>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub created_at: Option<RcDate>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

/// Former name of [`Team`].
pub type TeamInfo = Team;

/// Entry of `teams.list` / `teams.listAll`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamListItem {
    #[serde(flatten)]
    pub team: Team,
    /// Child rooms (main room excluded).
    #[serde(deserialize_with = "lenient::opt")]
    pub rooms: Option<u64>,
    #[serde(deserialize_with = "lenient::opt")]
    pub number_of_users: Option<u64>,
}

/// `user` of a [`TeamMember`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamMemberUser {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub status: Option<UserStatus>,
}

/// Entry of `teams.members`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamMember {
    pub user: TeamMemberUser,
    /// Absent when the member has no team role.
    #[serde(deserialize_with = "lenient::vec")]
    pub roles: Vec<String>,
    /// Who added the member.
    #[serde(deserialize_with = "lenient::opt")]
    pub created_by: Option<UserRef>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
}

impl TeamMember {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// Entry of `teams.autocomplete` (a team **main room**: `id` is the room id).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamAutocompleteItem {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: Option<String>,
    pub fname: Option<String>,
    pub t: RoomType,
    pub team_id: Option<String>,
    #[serde(rename = "avatarETag")]
    pub avatar_etag: Option<String>,
}

/// Entry of `teams.listRoomsOfUser`: a child room plus whether the user is its
/// only owner.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamRoomOfUser {
    #[serde(flatten)]
    pub room: Room,
    #[serde(deserialize_with = "lenient::or_default")]
    pub is_last_owner: bool,
}

/// Entry of `users.listTeams`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserTeam {
    #[serde(flatten)]
    pub team: Team,
    #[serde(deserialize_with = "lenient::or_default")]
    pub is_owner: bool,
}

/// `team` / `parent` added by `rooms.info` for team rooms and discussions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RoomParent {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: Option<String>,
    pub fname: Option<String>,
    pub t: RoomType,
    pub prid: Option<String>,
}

impl RoomParent {
    pub fn display_name(&self) -> &str {
        self.fname
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.name.as_deref())
            .unwrap_or_default()
    }
}

/// `rooms.info` with the extras of team rooms and discussions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomInfoExtended {
    pub room: Room,
    /// Team of a main or child room (`{_id, name, type, roomId}`).
    #[serde(deserialize_with = "lenient::opt")]
    pub team: Option<Team>,
    /// Main room of a child room, or the parent of a discussion.
    #[serde(deserialize_with = "lenient::opt")]
    pub parent: Option<RoomParent>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn team_type_codes() {
        let t: Team = serde_json::from_value(json!({"_id": "t", "type": 1})).unwrap();
        assert_eq!(t.kind, TeamType::Private);
        assert_eq!(t.kind.room_type(), RoomType::Private);
        let t: Team = serde_json::from_value(json!({"_id": "t", "type": 7})).unwrap();
        assert_eq!(t.kind, TeamType::Unknown(7));
        let t: Team = serde_json::from_value(json!({"_id": "t", "type": "x"})).unwrap();
        assert!(matches!(t.kind, TeamType::Unknown(_)));
        assert_eq!(serde_json::to_value(TeamType::Public).unwrap(), json!(0));
    }

    #[test]
    fn member_roles_tolerant() {
        let m: TeamMember =
            serde_json::from_value(json!({"user": {"_id": "u", "status": "nope"}, "roles": null}))
                .unwrap();
        assert!(m.roles.is_empty());
        assert_eq!(m.user.status, Some(UserStatus::Unknown));
    }
}
