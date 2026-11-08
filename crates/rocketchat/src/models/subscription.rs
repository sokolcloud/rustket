use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RoomType, UserRef, lenient};
use crate::RcDate;

/// ISubscription: the current user's membership state in a room.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Subscription {
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fname: Option<String>,
    pub t: RoomType,
    pub u: UserRef,
    pub open: bool,
    pub alert: bool,
    #[serde(deserialize_with = "lenient::or_default")]
    pub unread: u64,
    #[serde(deserialize_with = "lenient::or_default")]
    pub user_mentions: u64,
    #[serde(deserialize_with = "lenient::or_default")]
    pub group_mentions: u64,
    /// Thread ids with unread messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunread: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunread_user: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunread_group: Option<Vec<String>>,
    /// Last seen.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ls: Option<RcDate>,
    /// Last reply (in a thread).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub lr: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    /// Favorite.
    pub f: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_unread_status: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_mention_status: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_group_mentions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_pref_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desktop_notifications: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile_pref_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile_push_notifications: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_pref_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_notifications: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_notification_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_highlights: Option<Vec<String>>,
    #[serde(rename = "E2EKey", skip_serializing_if = "Option::is_none")]
    pub e2e_key: Option<String>,
    #[serde(rename = "E2ESuggestedKey", skip_serializing_if = "Option::is_none")]
    pub e2e_suggested_key: Option<String>,
    /// Previous room keys wrapped for this user, newest first (≤ 10).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub old_room_keys: Option<Vec<OldRoomKey>>,
    /// Old keys offered with `E2ESuggestedKey` (only in unprojected documents).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub suggested_old_room_keys: Option<Vec<OldRoomKey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_main: Option<bool>,
    /// The current user blocked the DM partner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<bool>,
    /// The DM partner blocked the current user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_hold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    /// `INVITED` while an invitation is pending (8.9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub inviter: Option<UserRef>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
}

impl Subscription {
    pub fn display_name(&self) -> &str {
        self.fname
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.name)
    }

    pub fn has_unread_threads(&self) -> bool {
        self.tunread.as_ref().is_some_and(|t| !t.is_empty())
    }
}

/// Entry of `oldRoomKeys` / `suggestedOldRoomKeys`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OldRoomKey {
    #[serde(rename = "e2eKeyId")]
    pub e2e_key_id: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    /// The old room key wrapped for this user (`kid + base64`).
    #[serde(rename = "E2EKey")]
    pub e2e_key: String,
}
