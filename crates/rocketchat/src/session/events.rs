//! Typed realtime events and their decoding from stream data messages.
//!
//! Argument shapes follow `packages/ddp-client/src/types/streams.ts` of Rocket.Chat 8.9
//! and were checked against the dev server (fixtures in `tests/fixtures/ddp/`).

use std::time::Duration;

use meteor_ddp::DataMessage;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RcDate;
use crate::models::{
    CustomEmoji, CustomUserStatus, Message, Permission, Role, Room, RoomType, Setting,
    Subscription, User, UserRef, UserStatus, lenient,
};

/// Stream (publication) names.
pub mod streams {
    pub const ROOM_MESSAGES: &str = "stream-room-messages";
    pub const NOTIFY_USER: &str = "stream-notify-user";
    pub const NOTIFY_LOGGED: &str = "stream-notify-logged";
    pub const NOTIFY_ALL: &str = "stream-notify-all";
    pub const NOTIFY_ROOM: &str = "stream-notify-room";
    pub const ROLES: &str = "stream-roles";
    /// Event name of the `stream-room-messages` subscription that delivers new and
    /// edited messages of every room the user can access: joined rooms *and* public
    /// channels the user never joined (see [`SessionEvent::Message`]).
    pub const MY_MESSAGES: &str = "__my_messages__";
}

/// Connection state of a [`Session`](super::Session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connecting or logging in.
    Connecting,
    /// Logged in; events flow.
    Connected,
    /// Connection lost; the next attempt starts after `retry_in`.
    Reconnecting {
        attempt: u32,
        retry_in: Duration,
        reason: String,
    },
    /// Stopped reconnecting (e.g. the server rejected the protocol version).
    /// [`Session::reconnect_now`](super::Session::reconnect_now) retries.
    Failed { reason: String },
    /// The session was closed.
    Closed,
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

impl From<&meteor_ddp::ConnectionState> for ConnectionState {
    fn from(s: &meteor_ddp::ConnectionState) -> Self {
        use meteor_ddp::ConnectionState as D;
        match s {
            D::Connecting => Self::Connecting,
            D::Connected { .. } => Self::Connected,
            D::Reconnecting {
                attempt,
                retry_in,
                reason,
            } => Self::Reconnecting {
                attempt: *attempt,
                retry_in: *retry_in,
                reason: reason.clone(),
            },
            D::Failed { reason } => Self::Failed {
                reason: reason.clone(),
            },
            D::Closed => Self::Closed,
        }
    }
}

/// Why the consumer should re-synchronise its state over REST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResyncReason {
    /// The connection was re-established; events in between were missed.
    Reconnected,
    /// A subscription queue overflowed and dropped this many events.
    Lagged { stream: String, dropped: u64 },
    /// The session event channel was full and events were dropped.
    Overflow,
    /// A stream subscription that the server had stopped (e.g. rate limited) is
    /// back; events in between were missed.
    StreamsRestored,
}

/// `inserted` / `updated` / `removed` of `*-changed` streams. Rocket.Chat also
/// sends `changed`, which is mapped to [`Updated`](Self::Updated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Inserted,
    Updated,
    Removed,
}

impl ChangeKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "inserted" | "added" => Some(Self::Inserted),
            "updated" | "changed" => Some(Self::Updated),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// `INotificationDesktop`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DesktopNotification {
    pub title: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_interaction: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<NotificationAction>>,
    pub payload: NotificationPayload,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationAction {
    pub action: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotificationPayload {
    /// Message id.
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub sender: Option<UserRef>,
    /// Room type; absent for notifications that are not about an openable room.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub room_type: Option<RoomType>,
    /// Room name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conference_id: Option<String>,
    pub message: NotificationMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_notification_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationMessage {
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    /// E2E encrypted content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// Arguments of `{rid}/deleteMessageBulk`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BulkDelete {
    pub rid: String,
    pub exclude_pinned: bool,
    pub ignore_discussion: bool,
    /// Time range of deleted messages (ignored when `ids` is set).
    pub ts: TimeRange,
    /// Only messages of these usernames (empty: all users).
    pub users: Vec<String>,
    /// Deleted message ids; take priority over `ts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_deleted_status: Option<bool>,
    /// Only file attachments were removed (messages stay).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_file_attachments_with: Option<Value>,
}

/// Mongo-style range `{"$gt"|"$gte": date, "$lt"|"$lte": date}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeRange {
    #[serde(rename = "$gt", skip_serializing_if = "Option::is_none")]
    pub gt: Option<RcDate>,
    #[serde(rename = "$gte", skip_serializing_if = "Option::is_none")]
    pub gte: Option<RcDate>,
    #[serde(rename = "$lt", skip_serializing_if = "Option::is_none")]
    pub lt: Option<RcDate>,
    #[serde(rename = "$lte", skip_serializing_if = "Option::is_none")]
    pub lte: Option<RcDate>,
}

impl TimeRange {
    /// Whether `ts` falls inside the range.
    pub fn contains(&self, ts: RcDate) -> bool {
        let t = ts.timestamp_millis();
        let ms = |d: &Option<RcDate>| d.map(|d| d.timestamp_millis());
        ms(&self.gt).is_none_or(|g| t > g)
            && ms(&self.gte).is_none_or(|g| t >= g)
            && ms(&self.lt).is_none_or(|l| t < l)
            && ms(&self.lte).is_none_or(|l| t <= l)
    }
}

/// `ICustomSound`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomSound {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub extension: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
}

/// `roles-change`: a user gained or lost a role (globally or in a room).
#[derive(Debug, Clone, PartialEq)]
pub struct RoleAssignment {
    /// `added`, `removed` or `changed`.
    pub kind: String,
    /// Role id (e.g. `moderator`).
    pub role: String,
    pub user: Option<UserRef>,
    /// Room id for room-scoped roles.
    pub scope: Option<String>,
}

/// A realtime event of a [`Session`](super::Session).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Connection(ConnectionState),
    /// The first login finished.
    Ready {
        me: Box<User>,
    },
    /// Re-synchronise over REST (`subscriptions.get`/`rooms.get` with
    /// `updatedSince`, `chat.syncMessages` for open rooms). Sent after every
    /// reconnect (once every subscription is renewed, or after `resync_wait` and
    /// then again once they are), when a stopped stream is back, and when events
    /// were dropped. Only sent while connected.
    Resync {
        reason: ResyncReason,
    },
    /// The token was rejected (revoked, expired, user deactivated). The session
    /// stopped; log in again. Always the last event.
    LoggedOut {
        reason: String,
    },
    /// Stream subscriptions the server stopped or refused (`"<stream>:<event>"`),
    /// retried in the background. Sent whenever the set changes; an empty list
    /// means every stream is back (a `Resync` follows).
    Degraded {
        streams: Vec<String>,
    },

    /// New or edited message (`__my_messages__`). The server sends these for every
    /// room the user can access, including public channels the user has not
    /// joined: then `room_participant` is `false`, and the consumer should ignore
    /// the message unless that room is open (previewed) or watched.
    Message {
        message: Box<Message>,
        room_participant: bool,
    },
    /// Ephemeral message for the current user only (`{uid}/message`), e.g. a
    /// slash-command reply or "you are not in this room".
    EphemeralMessage(Box<Message>),
    MessageDeleted {
        rid: String,
        id: String,
    },
    MessagesDeletedBulk(Box<BulkDelete>),
    /// Typing / recording / uploading. An empty `activities` list means stopped.
    /// `username` is the sender's username, or the real name when the sender's
    /// server uses `UI_Use_Real_Name`.
    UserActivity {
        rid: String,
        username: String,
        activities: Vec<String>,
        tmid: Option<String>,
    },
    MessagesRead {
        rid: String,
        until: Option<RcDate>,
        tmid: Option<String>,
    },
    /// `{rid}/videoconf`: a conference in the room changed.
    RoomVideoConference {
        rid: String,
        call_id: String,
    },
    /// `{rid}/messagesImported`: an import added messages anywhere in the room's
    /// history; reload it.
    MessagesImported {
        rid: String,
    },

    SubscriptionChanged {
        kind: ChangeKind,
        /// Removed payloads may only carry `_id`, `rid`, `u` and `t`.
        sub: Box<Subscription>,
    },
    RoomChanged {
        kind: ChangeKind,
        /// Removed payloads may only carry `_id`.
        room: Box<Room>,
    },
    Notification(Box<DesktopNotification>),

    UserStatus {
        uid: String,
        username: String,
        status: UserStatus,
        status_text: Option<String>,
    },
    UserNameChanged {
        uid: String,
        username: Option<String>,
        name: Option<String>,
    },
    UserDeleted {
        uid: String,
        /// `Delete` or `Unlink`.
        message_erasure_type: Option<String>,
        replace_by_user: Option<Value>,
    },
    /// A user (`username`) or room (`rid`) avatar changed.
    AvatarUpdated {
        username: Option<String>,
        rid: Option<String>,
        etag: Option<String>,
    },
    EmojiCustomUpdated(Box<CustomEmoji>),
    EmojiCustomDeleted(Box<CustomEmoji>),
    CustomUserStatusUpdated(Box<CustomUserStatus>),
    CustomUserStatusDeleted(Box<CustomUserStatus>),
    CustomSoundUpdated(Box<CustomSound>),
    CustomSoundDeleted(Box<CustomSound>),
    /// `roles-change`: role membership of a user changed. `user.name` is cleared
    /// (the server puts the acting user's name there).
    RolesChanged(Box<RoleAssignment>),
    /// `stream-roles`: a role definition changed.
    RoleChanged {
        kind: ChangeKind,
        role: Box<Role>,
    },
    PermissionChanged {
        kind: ChangeKind,
        permission: Box<Permission>,
    },
    SettingChanged {
        kind: ChangeKind,
        setting: Box<Setting>,
        public: bool,
    },

    /// UIKit interaction from an app (`{uid}/uiInteraction`).
    UiInteraction(Value),
    /// `{uid}/video-conference`: `{action, params: {callId, uid, rid}}`.
    VideoConference(Value),
    /// Another member asks for the room's E2E key.
    E2EKeyRequest {
        rid: String,
        key_id: String,
    },
    /// Changes of the user's own document: `{id, type: inserted|updated|removed, data|diff, unset}`.
    /// `services` (hashed login tokens, 2FA data) is removed.
    UserData(Value),
    /// The server ended this login (`{uid}/force_logout`).
    ForceLogout {
        session_id: Option<String>,
    },
    /// `{uid}/banners` (payload: `IBanner`).
    Banner(Value),
    /// `banner-changed` / `new-banner`.
    BannerChanged {
        banner_id: String,
    },
    /// `{uid}/calendar` (payload: `ICalendarNotification`).
    Calendar(Value),
    /// `{uid}/webdav`.
    Webdav(Value),

    /// An event this crate does not model, or one that failed to decode.
    Unknown {
        collection: String,
        event_name: String,
        args: Value,
    },
}

/// Decodes one stream data message into an event. Never fails: unknown or
/// malformed payloads become [`SessionEvent::Unknown`] (logged at debug level).
pub fn decode_event(msg: &DataMessage) -> SessionEvent {
    let event_name = msg.event_name().unwrap_or_default();
    let args = msg.args().unwrap_or_default();
    match decode_known(&msg.collection, event_name, args) {
        Some(ev) => ev,
        None => {
            tracing::debug!(
                collection = %msg.collection,
                event = %event_name,
                "undecoded stream event"
            );
            SessionEvent::Unknown {
                collection: msg.collection.clone(),
                event_name: event_name.to_owned(),
                args: Value::Array(args.to_vec()),
            }
        }
    }
}

fn parse<T: DeserializeOwned>(v: &Value) -> Option<T> {
    match T::deserialize(v) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!(error = %e, ty = std::any::type_name::<T>(), "invalid event payload");
            None
        }
    }
}

/// Parses a message-bearing payload. Oversized `md` trees are dropped by the
/// model (see [`crate::models::md::within_limits`]).
fn parse_message<T: DeserializeOwned>(v: &Value) -> Option<T> {
    parse(v)
}

/// Removes `services` (hashed login tokens, 2FA data) from a `userData` payload.
fn strip_services(mut v: Value) -> Value {
    for key in ["data", "diff", "unset"] {
        if let Some(Value::Object(o)) = v.get_mut(key) {
            o.retain(|k, _| k != "services" && !k.starts_with("services."));
        }
    }
    v
}

fn str_at(args: &[Value], i: usize) -> Option<String> {
    args.get(i).and_then(Value::as_str).map(str::to_owned)
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

fn decode_known(collection: &str, event: &str, args: &[Value]) -> Option<SessionEvent> {
    match collection {
        streams::ROOM_MESSAGES => {
            let m: Message = parse_message(args.first()?)?;
            let room_participant = args
                .get(1)
                .and_then(|meta| meta.get("roomParticipant"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Some(SessionEvent::Message {
                message: Box::new(m),
                room_participant,
            })
        }
        streams::NOTIFY_USER => {
            let (_, kind) = event.split_once('/')?;
            decode_notify_user(kind, args)
        }
        streams::NOTIFY_ROOM => {
            let (rid, kind) = event.split_once('/')?;
            decode_notify_room(rid, kind, args)
        }
        streams::NOTIFY_LOGGED => decode_notify_logged(event, args),
        streams::NOTIFY_ALL => decode_notify_all(event, args),
        streams::ROLES => {
            let v = args.first()?;
            let kind = ChangeKind::parse(v.get("type")?.as_str()?)?;
            let role: Role = parse(v)?;
            Some(SessionEvent::RoleChanged {
                kind,
                role: Box::new(role),
            })
        }
        _ => None,
    }
}

fn change<T: DeserializeOwned>(args: &[Value]) -> Option<(ChangeKind, T)> {
    let kind = ChangeKind::parse(args.first()?.as_str()?)?;
    Some((kind, parse(args.get(1)?)?))
}

fn decode_notify_user(kind: &str, args: &[Value]) -> Option<SessionEvent> {
    let first = args.first();
    Some(match kind {
        "message" => SessionEvent::EphemeralMessage(Box::new(parse_message(first?)?)),
        "notification" => SessionEvent::Notification(Box::new(parse(first?)?)),
        "rooms-changed" => {
            let kind = ChangeKind::parse(args.first()?.as_str()?)?;
            let room: Room = parse_message(args.get(1)?)?;
            SessionEvent::RoomChanged {
                kind,
                room: Box::new(room),
            }
        }
        "subscriptions-changed" => {
            let (kind, sub) = change::<Subscription>(args)?;
            SessionEvent::SubscriptionChanged {
                kind,
                sub: Box::new(sub),
            }
        }
        "uiInteraction" => SessionEvent::UiInteraction(first?.clone()),
        "video-conference" => SessionEvent::VideoConference(first?.clone()),
        "e2ekeyRequest" => SessionEvent::E2EKeyRequest {
            rid: str_at(args, 0)?,
            key_id: str_at(args, 1).unwrap_or_default(),
        },
        "userData" => SessionEvent::UserData(strip_services(first?.clone())),
        "force_logout" => SessionEvent::ForceLogout {
            session_id: str_at(args, 0),
        },
        "banners" => SessionEvent::Banner(first?.clone()),
        "calendar" => SessionEvent::Calendar(first?.clone()),
        "webdav" => SessionEvent::Webdav(first?.clone()),
        _ => return None,
    })
}

fn decode_notify_room(rid: &str, kind: &str, args: &[Value]) -> Option<SessionEvent> {
    let rid = rid.to_owned();
    Some(match kind {
        "deleteMessage" => SessionEvent::MessageDeleted {
            rid,
            id: opt_str(args.first()?.get("_id"))?,
        },
        "deleteMessageBulk" => {
            let mut bulk: BulkDelete = parse(args.first()?)?;
            if bulk.rid.is_empty() {
                bulk.rid = rid;
            }
            SessionEvent::MessagesDeletedBulk(Box::new(bulk))
        }
        "user-activity" => SessionEvent::UserActivity {
            rid,
            username: str_at(args, 0)?,
            activities: match args.get(1) {
                // Legacy clients publish `[username, typing: bool]` here too.
                Some(Value::Bool(true)) => vec!["user-typing".to_owned()],
                Some(v) => lenient::vec_from_value(v.clone()),
                None => Vec::new(),
            },
            tmid: opt_str(args.get(2).and_then(|e| e.get("tmid"))),
        },
        "typing" => SessionEvent::UserActivity {
            rid,
            username: str_at(args, 0)?,
            activities: if args.get(1).and_then(Value::as_bool)? {
                vec!["user-typing".to_owned()]
            } else {
                Vec::new()
            },
            tmid: None,
        },
        "messagesRead" => {
            let v = args.first();
            SessionEvent::MessagesRead {
                rid,
                until: v
                    .and_then(|v| v.get("until"))
                    .and_then(|u| RcDate::deserialize(u).ok()),
                tmid: opt_str(v.and_then(|v| v.get("tmid"))),
            }
        }
        "videoconf" => SessionEvent::RoomVideoConference {
            rid,
            call_id: str_at(args, 0)?,
        },
        "messagesImported" => SessionEvent::MessagesImported { rid },
        _ => return None,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmojiData {
    emoji_data: CustomEmoji,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatusData {
    user_status_data: CustomUserStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SoundData {
    sound_data: CustomSound,
}

/// Rocket.Chat `PresenceStatusCode`.
pub fn status_from_code(code: i64) -> UserStatus {
    match code {
        0 => UserStatus::Offline,
        1 => UserStatus::Online,
        2 => UserStatus::Away,
        3 => UserStatus::Busy,
        _ => UserStatus::Unknown,
    }
}

fn decode_notify_logged(event: &str, args: &[Value]) -> Option<SessionEvent> {
    let first = args.first();
    Some(match event {
        "user-status" => {
            // [[uid, username, statusCode, statusText, name, roles, statusSource, statusExpiresAt]]
            let t = first?.as_array()?;
            SessionEvent::UserStatus {
                uid: str_at(t, 0)?,
                username: str_at(t, 1).unwrap_or_default(),
                status: status_from_code(t.get(2)?.as_i64()?),
                status_text: str_at(t, 3),
            }
        }
        "Users:NameChanged" => {
            let v = first?;
            SessionEvent::UserNameChanged {
                uid: opt_str(v.get("_id"))?,
                username: opt_str(v.get("username")),
                name: opt_str(v.get("name")),
            }
        }
        "Users:Deleted" => {
            let v = first?;
            SessionEvent::UserDeleted {
                uid: opt_str(v.get("userId"))?,
                message_erasure_type: opt_str(v.get("messageErasureType")),
                replace_by_user: v.get("replaceByUser").filter(|r| !r.is_null()).cloned(),
            }
        }
        "updateAvatar" => {
            let v = first?;
            let username = opt_str(v.get("username"));
            let rid = opt_str(v.get("rid"));
            if username.is_none() && rid.is_none() {
                return None;
            }
            SessionEvent::AvatarUpdated {
                username,
                rid,
                etag: opt_str(v.get("etag")),
            }
        }
        "updateEmojiCustom" => {
            SessionEvent::EmojiCustomUpdated(Box::new(parse::<EmojiData>(first?)?.emoji_data))
        }
        "deleteEmojiCustom" => {
            SessionEvent::EmojiCustomDeleted(Box::new(parse::<EmojiData>(first?)?.emoji_data))
        }
        "updateCustomUserStatus" => SessionEvent::CustomUserStatusUpdated(Box::new(
            parse::<UserStatusData>(first?)?.user_status_data,
        )),
        "deleteCustomUserStatus" => SessionEvent::CustomUserStatusDeleted(Box::new(
            parse::<UserStatusData>(first?)?.user_status_data,
        )),
        "roles-change" => {
            let v = first?;
            SessionEvent::RolesChanged(Box::new(RoleAssignment {
                kind: opt_str(v.get("type"))?,
                role: opt_str(v.get("_id"))?,
                user: v
                    .get("u")
                    .and_then(parse::<UserRef>)
                    .map(|u| UserRef { name: None, ..u }),
                scope: opt_str(v.get("scope")),
            }))
        }
        "permissions-changed" => {
            let (kind, permission) = change::<Permission>(args)?;
            SessionEvent::PermissionChanged {
                kind,
                permission: Box::new(permission),
            }
        }
        "private-settings-changed" => {
            let (kind, setting) = change::<Setting>(args)?;
            SessionEvent::SettingChanged {
                kind,
                setting: Box::new(setting),
                public: false,
            }
        }
        "banner-changed" | "new-banner" => SessionEvent::BannerChanged {
            banner_id: opt_str(first?.get("bannerId"))?,
        },
        _ => return None,
    })
}

fn decode_notify_all(event: &str, args: &[Value]) -> Option<SessionEvent> {
    Some(match event {
        "public-settings-changed" => {
            let (kind, setting) = change::<Setting>(args)?;
            SessionEvent::SettingChanged {
                kind,
                setting: Box::new(setting),
                public: true,
            }
        }
        "updateCustomSound" => SessionEvent::CustomSoundUpdated(Box::new(
            parse::<SoundData>(args.first()?)?.sound_data,
        )),
        "deleteCustomSound" => SessionEvent::CustomSoundDeleted(Box::new(
            parse::<SoundData>(args.first()?)?.sound_data,
        )),
        _ => return None,
    })
}
