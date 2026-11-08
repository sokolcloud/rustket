//! Video conferences (`IVideoConference`, docs/teams-videoconf.md §7.2) and the
//! `{uid}/video-conference` signal (§7.5).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::{UserRef, lenient};
use crate::RcDate;

/// `VIDEO_CONF_RINGING_WINDOW_MS`.
pub const VIDEO_CONF_RINGING_WINDOW_MS: i64 = 15_000;
/// Longest id accepted in a `{uid}/video-conference` signal.
pub const SIGNAL_ID_MAX: usize = 64;

/// `VideoConferenceStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VideoConfStatus {
    #[default]
    Calling,
    Started,
    Expired,
    Ended,
    Declined,
    Unknown(u8),
}

impl VideoConfStatus {
    pub fn from_code(n: u8) -> Self {
        match n {
            0 => Self::Calling,
            1 => Self::Started,
            2 => Self::Expired,
            3 => Self::Ended,
            4 => Self::Declined,
            n => Self::Unknown(n),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Calling => 0,
            Self::Started => 1,
            Self::Expired => 2,
            Self::Ended => 3,
            Self::Declined => 4,
            Self::Unknown(n) => n,
        }
    }
}

impl Serialize for VideoConfStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for VideoConfStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(v.as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .map_or(Self::Unknown(u8::MAX), Self::from_code))
    }
}

/// `VideoConferenceType`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum VideoConfKind {
    Direct,
    /// `videoconference` (group call).
    #[default]
    Group,
    Livechat,
    Voip,
    Unknown(String),
}

impl VideoConfKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Direct => "direct",
            Self::Group => "videoconference",
            Self::Livechat => "livechat",
            Self::Voip => "voip",
            Self::Unknown(s) => s,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "direct" => Self::Direct,
            "videoconference" => Self::Group,
            "livechat" => Self::Livechat,
            "voip" => Self::Voip,
            s => Self::Unknown(s.to_owned()),
        }
    }
}

impl Serialize for VideoConfKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for VideoConfKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(Self::parse(v.as_str().unwrap_or_default()))
    }
}

/// `IVideoConferenceUser`: a conference member (not necessarily in the call).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoConfUser {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: Option<String>,
    pub name: Option<String>,
    #[serde(rename = "avatarETag", deserialize_with = "lenient::opt")]
    pub avatar_etag: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub joined: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub joined_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub declined: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub declined_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub left_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ringing_at: Option<RcDate>,
}

impl VideoConfUser {
    /// `hasJoinedVideoConference`: absent `joined` means joined.
    pub fn has_joined(&self) -> bool {
        self.joined != Some(false)
    }

    /// `isInVideoConference`.
    pub fn in_call(&self) -> bool {
        self.has_joined() && self.left_at.is_none()
    }

    /// `isRingingVideoConferenceMember` at `now_ms`.
    pub fn ringing_now(&self, now_ms: i64) -> bool {
        let Some(rang) = self.ringing_at.map(|d| d.timestamp_millis()) else {
            return false;
        };
        if self.declined == Some(true)
            && self
                .declined_at
                .is_some_and(|d| d.timestamp_millis() >= rang)
        {
            return false;
        }
        now_ms - rang < VIDEO_CONF_RINGING_WINDOW_MS
    }
}

/// `messages` of a conference.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfMessages {
    pub started: Option<String>,
    pub ended: Option<String>,
}

/// `VideoConferenceCapabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoConfCapabilities {
    #[serde(deserialize_with = "lenient::opt")]
    pub mic: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub cam: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub title: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub persistent_chat: Option<bool>,
    /// The call runs inside Rocket.Chat's own page; `join` returns no URL.
    #[serde(deserialize_with = "lenient::opt")]
    pub embedded: Option<bool>,
}

/// `video-conference.capabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub provider_name: String,
    pub capabilities: VideoConfCapabilities,
}

/// Entry of `video-conference.providers`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfProvider {
    pub key: String,
    pub label: String,
}

/// `IVideoConference` (+ `capabilities`/`chatAccess` from `video-conference.info`;
/// `providerData` is stripped by every endpoint).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoConference {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: VideoConfKind,
    pub rid: String,
    pub status: VideoConfStatus,
    #[serde(deserialize_with = "lenient::vec")]
    pub users: Vec<VideoConfUser>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub messages: VideoConfMessages,
    /// Provider base URL (not per user: never open it, `join` gives the URL).
    #[serde(deserialize_with = "lenient::opt")]
    pub url: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_by: Option<UserRef>,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ended_by: Option<UserRef>,
    #[serde(deserialize_with = "lenient::opt")]
    pub ended_at: Option<RcDate>,
    pub provider_name: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub ringing: Option<bool>,
    #[serde(deserialize_with = "lenient::opt")]
    pub discussion_rid: Option<String>,
    /// Group calls only.
    #[serde(deserialize_with = "lenient::opt")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub anonymous_users: Option<u64>,
    #[serde(deserialize_with = "lenient::opt")]
    pub capabilities: Option<VideoConfCapabilities>,
    pub chat_access: Option<Value>,
}

impl VideoConference {
    pub fn is_ended(&self) -> bool {
        self.ended_at.is_some()
    }

    /// Members that joined, with a username (what the block shows).
    pub fn joined_users(&self) -> impl Iterator<Item = &VideoConfUser> {
        self.users
            .iter()
            .filter(|u| u.has_joined() && u.username.as_deref().is_some_and(|n| !n.is_empty()))
    }

    pub fn creator_id(&self) -> Option<&str> {
        self.created_by.as_ref().map(|u| u.id.as_str())
    }
}

/// What `video-conference.start` tells the caller to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartInstructions {
    /// Ring `callee_id` (direct call in a 1:1 DM, Enterprise only).
    Direct {
        call_id: String,
        callee_id: String,
        provider_name: String,
    },
    /// Join right away.
    Conference {
        call_id: String,
        rid: String,
        provider_name: String,
    },
    Livechat {
        call_id: String,
        provider_name: String,
    },
}

impl StartInstructions {
    pub fn call_id(&self) -> &str {
        match self {
            Self::Direct { call_id, .. }
            | Self::Conference { call_id, .. }
            | Self::Livechat { call_id, .. } => call_id,
        }
    }

    /// Parses `data` of the `start` response; `None` without a `callId`.
    pub fn from_value(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let call_id = s("callId").filter(|c| !c.is_empty())?;
        let provider_name = s("providerName").unwrap_or_default();
        Some(match v.get("type").and_then(Value::as_str) {
            Some("direct") => Self::Direct {
                call_id,
                callee_id: s("calleeId")?,
                provider_name,
            },
            Some("livechat") => Self::Livechat {
                call_id,
                provider_name,
            },
            _ => Self::Conference {
                call_id,
                rid: s("rid").unwrap_or_default(),
                provider_name,
            },
        })
    }
}

/// `video-conference.join`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JoinResult {
    /// Empty for embedded providers (open `{server}/conference/<callId>`).
    pub url: String,
    pub provider_name: String,
    pub call_id: Option<String>,
    pub rid: Option<String>,
}

/// Entry of `video-conference.joinable`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JoinableCall {
    pub call_id: String,
    pub name: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub created_at: Option<RcDate>,
    pub users_count: u64,
    #[serde(deserialize_with = "lenient::vec")]
    pub participants: Vec<Value>,
    pub joined: bool,
    pub declined: bool,
    #[serde(deserialize_with = "lenient::opt")]
    pub ringing_at: Option<RcDate>,
}

/// `action` of a `{uid}/video-conference` signal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VideoConfAction {
    Call,
    Ring,
    Canceled,
    Accepted,
    Rejected,
    Confirmed,
    Join,
    End,
    Started,
    Other(String),
}

impl VideoConfAction {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Call => "call",
            Self::Ring => "ring",
            Self::Canceled => "canceled",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Confirmed => "confirmed",
            Self::Join => "join",
            Self::End => "end",
            Self::Started => "started",
            Self::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "call" => Self::Call,
            "ring" => Self::Ring,
            "canceled" => Self::Canceled,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "confirmed" => Self::Confirmed,
            "join" => Self::Join,
            "end" => Self::End,
            "started" => Self::Started,
            s => Self::Other(s.to_owned()),
        }
    }
}

/// A `{uid}/video-conference` payload `{action, params: {callId, uid, rid}}`
/// after shape validation (§7.5). Still untrusted: any member of a shared room
/// can publish one (`stream-notify-room-users` relays it verbatim), so the
/// consumer must confirm it with `video-conference.info` before acting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfSignal {
    pub action: VideoConfAction,
    pub call_id: String,
    /// The user the signal claims to come from.
    pub uid: String,
    pub rid: String,
}

impl VideoConfSignal {
    /// Strict parse: `action` a non-empty string of at most 32 chars (unknown
    /// ones become `Other`), `callId`/`uid`/`rid` non-empty strings of at most
    /// [`SIGNAL_ID_MAX`] chars without `/`. Anything else is `None`.
    pub fn parse(v: &Value) -> Option<Self> {
        let action = v.get("action")?.as_str()?;
        if action.is_empty() || action.len() > 32 {
            return None;
        }
        let params = v.get("params")?.as_object()?;
        let id = |k: &str| {
            params
                .get(k)?
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= SIGNAL_ID_MAX && !s.contains('/'))
                .map(str::to_owned)
        };
        Some(Self {
            action: VideoConfAction::parse(action),
            call_id: id("callId")?,
            uid: id("uid")?,
            rid: id("rid")?,
        })
    }

    /// The payload to publish (`{action, params: {callId, uid, rid}}`).
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "action": self.action.as_str(),
            "params": { "callId": self.call_id, "uid": self.uid, "rid": self.rid },
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn signal_validation() {
        let ok = json!({"action": "call", "params": {"callId": "c1", "uid": "u1", "rid": "r1"}});
        let s = VideoConfSignal::parse(&ok).unwrap();
        assert_eq!(s.action, VideoConfAction::Call);
        assert_eq!(VideoConfSignal::parse(&s.to_value()), Some(s));
        let other = json!({"action": "zzz", "params": {"callId": "c", "uid": "u", "rid": "r"}});
        assert_eq!(
            VideoConfSignal::parse(&other).unwrap().action,
            VideoConfAction::Other("zzz".into())
        );
        let long = "x".repeat(65);
        for bad in [
            json!({"anything": 1}),
            json!({"action": "call", "params": "x"}),
            json!({"action": "call", "params": {"callId": "c", "uid": "u"}}),
            json!({"action": "call", "params": {"callId": "", "uid": "u", "rid": "r"}}),
            json!({"action": "call", "params": {"callId": 5, "uid": "u", "rid": "r"}}),
            json!({"action": "call", "params": {"callId": long, "uid": "u", "rid": "r"}}),
            json!({"action": "call", "params": {"callId": "a/b", "uid": "u", "rid": "r"}}),
            json!({"action": "", "params": {"callId": "c", "uid": "u", "rid": "r"}}),
            json!({"action": 1, "params": {"callId": "c", "uid": "u", "rid": "r"}}),
            json!(["call"]),
        ] {
            assert_eq!(VideoConfSignal::parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn member_states() {
        let u: VideoConfUser = serde_json::from_value(json!({"_id": "u"})).unwrap();
        assert!(u.has_joined() && u.in_call());
        let u: VideoConfUser = serde_json::from_value(json!({
            "_id": "u", "joined": false, "ringingAt": {"$date": 10_000}
        }))
        .unwrap();
        assert!(!u.has_joined());
        assert!(u.ringing_now(20_000));
        assert!(!u.ringing_now(30_000));
        let u: VideoConfUser = serde_json::from_value(json!({
            "_id": "u", "ringingAt": {"$date": 10_000}, "declined": true,
            "declinedAt": {"$date": 11_000}
        }))
        .unwrap();
        assert!(!u.ringing_now(12_000));
    }

    #[test]
    fn start_instructions() {
        let d = StartInstructions::from_value(
            &json!({"type": "direct", "callId": "c", "calleeId": "b", "providerName": "jitsi"}),
        )
        .unwrap();
        assert!(matches!(d, StartInstructions::Direct { ref callee_id, .. } if callee_id == "b"));
        let g = StartInstructions::from_value(
            &json!({"type": "videoconference", "callId": "c", "rid": "r"}),
        )
        .unwrap();
        assert_eq!(g.call_id(), "c");
        assert!(StartInstructions::from_value(&json!({"type": "direct", "callId": "c"})).is_none());
        assert!(StartInstructions::from_value(&json!({"type": "videoconference"})).is_none());
    }
}
