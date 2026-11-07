//! REST calls and payloads of the right panel, verified against Rocket.Chat
//! 8.9.1. Payload builders are plain functions so they can be unit-tested.

use rocketchat::models::{
    Message, Paginated, Room, RoomType, Subscription, UserRef, UserSummary, lenient,
};
use rocketchat::rest::{Query, RoomMembersOptions};
use rocketchat::{RcDate, RestClient, Result};
use serde::Deserialize;
use serde_json::{Value, json};

/// Page size of every list in the panel.
pub const PAGE: u64 = 25;

/// Parses `{key: [...], count, offset, total}`, skipping invalid elements.
pub fn paginated<T: serde::de::DeserializeOwned>(mut v: Value, key: &str) -> Paginated<T> {
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (count, offset, total) = (num(&v, "count"), num(&v, "offset"), num(&v, "total"));
    let items = v.get_mut(key).map(Value::take).unwrap_or(Value::Null);
    let c = lenient::counted(items);
    Paginated {
        items: c.items,
        count,
        offset,
        total,
        raw_len: c.raw_len,
        dropped: c.dropped,
    }
}

/// Type-specific endpoint prefix (`channels`, `groups`, `im`).
pub fn prefix(t: RoomType) -> &'static str {
    match t {
        RoomType::Private => "groups",
        RoomType::Direct => "im",
        _ => "channels",
    }
}

// ---- lists ---------------------------------------------------------------

/// `rooms.membersOrderedByRole` (owners, moderators, then the rest).
pub async fn members(
    rest: RestClient,
    rid: String,
    filter: String,
    online_only: bool,
    offset: u64,
) -> Result<Paginated<UserSummary>> {
    let opts = RoomMembersOptions {
        offset: Some(offset),
        count: Some(PAGE),
        filter: (!filter.trim().is_empty()).then(|| filter.trim().to_owned()),
        status: if online_only {
            vec!["online".into(), "away".into(), "busy".into()]
        } else {
            Vec::new()
        },
    };
    rest.room_members(&rid, &opts).await
}

/// Filter of `chat.getThreadsList`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreadFilter {
    #[default]
    All,
    Following,
    Unread,
}

impl ThreadFilter {
    pub const ALL: [ThreadFilter; 3] = [Self::All, Self::Following, Self::Unread];

    pub fn param(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Following => Some("following"),
            Self::Unread => Some("unread"),
        }
    }
}

pub async fn threads(
    rest: RestClient,
    rid: String,
    filter: ThreadFilter,
    text: String,
    offset: u64,
) -> Result<Paginated<Message>> {
    let text = text.trim().to_owned();
    rest.threads_list(
        &rid,
        filter.param(),
        (!text.is_empty()).then_some(text.as_str()),
        offset,
        PAGE,
    )
    .await
}

pub async fn discussions(
    rest: RestClient,
    rid: String,
    text: String,
    offset: u64,
) -> Result<Paginated<Room>> {
    let text = text.trim();
    let v: Value = rest
        .get(
            "rooms.getDiscussions",
            &Query::new()
                .push("roomId", &rid)
                .opt("text", (!text.is_empty()).then_some(text))
                .push("offset", offset)
                .push("count", PAGE)
                .push("sort", r#"{"ts":-1}"#),
        )
        .await?;
    Ok(paginated(v, "discussions"))
}

/// `teams.listRooms` of a team main room.
pub async fn team_rooms(
    rest: RestClient,
    team_id: String,
    text: String,
    offset: u64,
) -> Result<Paginated<Room>> {
    let text = text.trim();
    let v: Value = rest
        .get(
            "teams.listRooms",
            &Query::new()
                .push("teamId", &team_id)
                .opt("filter", (!text.is_empty()).then_some(text))
                .push("offset", offset)
                .push("count", PAGE),
        )
        .await?;
    Ok(paginated(v, "rooms"))
}

/// Which message list (`chat.get*Messages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageList {
    Pinned,
    Starred,
    Mentions,
}

pub async fn message_list(
    rest: RestClient,
    rid: String,
    which: MessageList,
    offset: u64,
) -> Result<Paginated<Message>> {
    match which {
        MessageList::Pinned => rest.get_pinned(&rid, offset, PAGE).await,
        MessageList::Starred => rest.get_starred(&rid, offset, PAGE).await,
        MessageList::Mentions => rest.get_mentioned(&rid, offset, PAGE).await,
    }
}

/// `chat.search` has no total: a full page means there may be more.
pub async fn search(
    rest: RestClient,
    rid: String,
    text: String,
    offset: u64,
) -> Result<Paginated<Message>> {
    let items = rest
        .search_messages(&rid, text.trim(), offset, PAGE)
        .await?;
    let n = items.len() as u64;
    Ok(Paginated {
        raw_len: items.len(),
        count: n,
        offset,
        total: if n >= PAGE {
            offset + n + 1
        } else {
            offset + n
        },
        dropped: 0,
        items,
    })
}

/// An uploaded file (`IUploadWithUser`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileItem {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub mime: Option<String>,
    pub type_group: Option<String>,
    /// Server-relative download path (`/file-upload/{id}/{name}`).
    pub path: Option<String>,
    pub url: Option<String>,
    #[serde(deserialize_with = "lenient::opt")]
    pub uploaded_at: Option<RcDate>,
    #[serde(deserialize_with = "lenient::opt")]
    pub user: Option<UserRef>,
}

impl FileItem {
    pub fn download_path(&self) -> Option<&str> {
        self.path
            .as_deref()
            .or(self.url.as_deref())
            .filter(|p| !p.is_empty())
    }
}

/// `typeGroup` filter values of the files list (`None` = all).
pub const FILE_TYPES: [Option<&str>; 6] = [
    None,
    Some("image"),
    Some("video"),
    Some("audio"),
    Some("text"),
    Some("application"),
];

pub fn files_query(rid: &str, type_group: Option<&str>, name: &str, offset: u64) -> Query {
    let name = name.trim();
    Query::new()
        .push("roomId", rid)
        .push("offset", offset)
        .push("count", PAGE)
        .push("sort", r#"{"uploadedAt":-1}"#)
        .opt("name", (!name.is_empty()).then_some(name))
        .opt("typeGroup", type_group)
        .push("onlyConfirmed", true)
}

/// `channels.files` / `groups.files` / `im.files`.
pub async fn files(
    rest: RestClient,
    t: RoomType,
    rid: String,
    type_group: Option<&'static str>,
    name: String,
    offset: u64,
) -> Result<Paginated<FileItem>> {
    let v: Value = rest
        .get(
            &format!("{}.files", prefix(t)),
            &files_query(&rid, type_group, &name, offset),
        )
        .await?;
    Ok(paginated(v, "files"))
}

// ---- notification preferences ----------------------------------------------

/// Alert level of a notification channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alert {
    #[default]
    Default,
    All,
    Mentions,
    Nothing,
}

impl Alert {
    pub const ALL: [Alert; 4] = [Self::Default, Self::All, Self::Mentions, Self::Nothing];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Nothing => "nothing",
        }
    }

    /// The web client's rule: the subscription value counts only when its
    /// origin is `subscription`.
    pub fn from_sub(origin: Option<&str>, value: Option<&str>) -> Self {
        if origin != Some("subscription") {
            return Self::Default;
        }
        match value {
            Some("all") => Self::All,
            Some("mentions") => Self::Mentions,
            Some("nothing") => Self::Nothing,
            _ => Self::Default,
        }
    }
}

/// The notification form, as loaded from the subscription.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NotificationPrefs {
    pub turn_on: bool,
    pub mute_group_mentions: bool,
    pub show_counter: bool,
    pub show_mentions: bool,
    pub desktop: Alert,
    pub mobile: Alert,
    pub email: Alert,
    /// `none`, `default` or a custom sound id.
    pub sound: String,
}

impl NotificationPrefs {
    pub fn from_subscription(s: &Subscription) -> Self {
        Self {
            turn_on: s.disable_notifications != Some(true),
            mute_group_mentions: s.mute_group_mentions == Some(true),
            show_counter: s.hide_unread_status != Some(true),
            show_mentions: s.hide_mention_status != Some(true),
            desktop: Alert::from_sub(
                s.desktop_pref_origin.as_deref(),
                s.desktop_notifications.as_deref(),
            ),
            mobile: Alert::from_sub(
                s.mobile_pref_origin.as_deref(),
                s.mobile_push_notifications.as_deref(),
            ),
            email: Alert::from_sub(
                s.email_pref_origin.as_deref(),
                s.email_notifications.as_deref(),
            ),
            sound: s
                .audio_notification_value
                .clone()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "default".into()),
        }
    }

    /// Body of `rooms.saveNotification` (flags are `"0"`/`"1"` strings).
    pub fn payload(&self, rid: &str) -> Value {
        let flag = |b: bool| if b { "1" } else { "0" };
        json!({
            "roomId": rid,
            "notifications": {
                "disableNotifications": flag(!self.turn_on),
                "muteGroupMentions": flag(self.mute_group_mentions),
                "hideUnreadStatus": flag(!self.show_counter),
                "hideMentionStatus": flag(!self.show_mentions),
                "desktopNotifications": self.desktop.as_str(),
                "audioNotificationValue": self.sound,
                "mobilePushNotifications": self.mobile.as_str(),
                "emailNotifications": self.email.as_str(),
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct CustomSound {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
}

pub async fn custom_sounds(rest: RestClient) -> Result<Vec<CustomSound>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let v: Value = rest
            .get(
                "custom-sounds.list",
                &Query::new().push("offset", offset).push("count", 100),
            )
            .await?;
        let page: Paginated<CustomSound> = paginated(v, "sounds");
        let more = page.has_more() && page.raw_len > 0;
        offset += page.raw_len as u64;
        out.extend(page.items);
        if !more || out.len() > 1000 {
            return Ok(out);
        }
    }
}

pub async fn save_notifications(
    rest: RestClient,
    rid: String,
    prefs: NotificationPrefs,
) -> Result<()> {
    rest.post_ok("rooms.saveNotification", &prefs.payload(&rid))
        .await
}

// ---- room settings ------------------------------------------------------------

/// System message types hidden by "Hide system messages" (the web client's
/// `MessageTypesValues`).
pub const SYSTEM_MESSAGE_TYPES: &[&str] = &[
    "uj",
    "ujt",
    "ul",
    "ult",
    "ru",
    "removed-user-from-team",
    "au",
    "ui",
    "uir",
    "added-user-to-team",
    "mute_unmute",
    "user-banned",
    "user-unbanned",
    "r",
    "ut",
    "wm",
    "rm",
    "subscription-role-added",
    "subscription-role-removed",
    "room-archived",
    "room-unarchived",
    "room_changed_privacy",
    "room_changed_avatar",
    "room_changed_topic",
    "room-removed-read-only",
    "room-set-read-only",
    "room-disallowed-reacting",
    "room-allowed-reacting",
    "user-added-room-to-team",
    "user-converted-to-channel",
    "user-converted-to-team",
    "user-deleted-room-from-team",
    "user-removed-room-from-team",
    "room_changed_announcement",
    "room_changed_description",
];

/// The room settings form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoomSettings {
    pub name: String,
    pub topic: String,
    pub announcement: String,
    pub description: String,
    pub private: bool,
    pub read_only: bool,
    pub react_when_read_only: bool,
    pub archived: bool,
    pub hide_system_messages: bool,
}

impl RoomSettings {
    pub fn from_room(r: &Room) -> Self {
        let hide = match &r.sys_mes {
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Bool(b)) => !*b,
            _ => false,
        };
        Self {
            name: r.name.clone().unwrap_or_default(),
            topic: r.topic.clone().unwrap_or_default(),
            announcement: r.announcement.clone().unwrap_or_default(),
            description: r.description.clone().unwrap_or_default(),
            private: r.t == RoomType::Private,
            read_only: r.ro == Some(true),
            react_when_read_only: r.react_when_read_only == Some(true),
            archived: r.archived == Some(true),
            hide_system_messages: hide,
        }
    }

    /// Body of `rooms.saveRoomSettings` with only the fields that changed
    /// (archiving goes through `rooms.changeArchivationState`). `None` when
    /// nothing but the archive state changed.
    pub fn payload(&self, rid: &str, before: &Self) -> Option<Value> {
        let mut body = serde_json::Map::new();
        let mut set = |k: &str, v: Value| {
            body.insert(k.into(), v);
        };
        if self.name.trim() != before.name {
            set("roomName", self.name.trim().into());
        }
        if self.topic != before.topic {
            set("roomTopic", self.topic.clone().into());
        }
        if self.announcement != before.announcement {
            set("roomAnnouncement", self.announcement.clone().into());
        }
        if self.description != before.description {
            set("roomDescription", self.description.clone().into());
        }
        if self.private != before.private {
            set("roomType", if self.private { "p" } else { "c" }.into());
        }
        if self.read_only != before.read_only {
            set("readOnly", self.read_only.into());
        }
        if self.react_when_read_only != before.react_when_read_only {
            set("reactWhenReadOnly", self.react_when_read_only.into());
        }
        if self.hide_system_messages != before.hide_system_messages {
            let list: Vec<&str> = if self.hide_system_messages {
                SYSTEM_MESSAGE_TYPES.to_vec()
            } else {
                Vec::new()
            };
            set("systemMessages", json!(list));
        }
        if body.is_empty() {
            return None;
        }
        body.insert("rid".into(), rid.into());
        Some(Value::Object(body))
    }

    /// Client-side validation; returns an i18n key.
    pub fn validate(&self, is_discussion: bool) -> Option<&'static str> {
        let name = self.name.trim();
        if name.is_empty() {
            return Some("room_panel.err_name_required");
        }
        if !is_discussion && name.chars().any(char::is_whitespace) {
            return Some("room_panel.err_name_spaces");
        }
        None
    }
}

pub async fn save_room_settings(rest: RestClient, body: Value) -> Result<()> {
    rest.post_ok("rooms.saveRoomSettings", &body).await
}

pub async fn set_archived(rest: RestClient, rid: String, archived: bool) -> Result<()> {
    rest.post_ok(
        "rooms.changeArchivationState",
        &json!({ "rid": rid, "action": if archived { "archive" } else { "unarchive" } }),
    )
    .await
}

// ---- members / user actions --------------------------------------------------------

/// A room role holder (`channels.roles` / `groups.roles`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct RoomRole {
    pub u: UserRef,
    pub roles: Vec<String>,
}

pub async fn room_roles(rest: RestClient, t: RoomType, rid: String) -> Result<Vec<RoomRole>> {
    if t == RoomType::Direct {
        return Ok(Vec::new());
    }
    let mut v: Value = rest
        .get(
            &format!("{}.roles", prefix(t)),
            &Query::new().push("roomId", &rid),
        )
        .await?;
    Ok(lenient::vec_from_value(
        v.get_mut("roles").map(Value::take).unwrap_or_default(),
    ))
}

/// A room role a member can be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Moderator,
    Leader,
}

impl Role {
    pub fn id(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Moderator => "moderator",
            Self::Leader => "leader",
        }
    }

    /// `channels.addOwner`, `groups.removeLeader`, …
    pub fn endpoint(self, t: RoomType, add: bool) -> String {
        let name = match self {
            Self::Owner => "Owner",
            Self::Moderator => "Moderator",
            Self::Leader => "Leader",
        };
        let verb = if add { "add" } else { "remove" };
        let p = if t == RoomType::Private {
            "groups"
        } else {
            "channels"
        };
        format!("{p}.{verb}{name}")
    }
}

/// A moderation action on a member, with its endpoint and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAction {
    SetRole(Role, bool),
    /// Mute (`true`) or unmute.
    Mute(bool),
    Remove,
    /// Ignore (`true`) or stop ignoring.
    Ignore(bool),
}

/// Endpoint call for `action` on `user` (id, username) in room `rid`.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Post(String, Value),
    Get(String, Vec<(String, String)>),
}

pub fn user_action_call(
    action: &UserAction,
    t: RoomType,
    rid: &str,
    team: Option<&str>,
    uid: &str,
    username: &str,
) -> Call {
    match action {
        UserAction::SetRole(role, add) => Call::Post(
            role.endpoint(t, *add),
            json!({ "roomId": rid, "userId": uid }),
        ),
        UserAction::Mute(mute) => Call::Post(
            if *mute {
                "rooms.muteUser"
            } else {
                "rooms.unmuteUser"
            }
            .into(),
            json!({ "roomId": rid, "username": username }),
        ),
        UserAction::Remove => match team {
            Some(team) => Call::Post(
                "teams.removeMember".into(),
                json!({ "teamId": team, "userId": uid }),
            ),
            None => Call::Post(
                format!(
                    "{}.kick",
                    if t == RoomType::Private {
                        "groups"
                    } else {
                        "channels"
                    }
                ),
                json!({ "roomId": rid, "userId": uid }),
            ),
        },
        UserAction::Ignore(ignore) => Call::Get(
            "chat.ignoreUser".into(),
            vec![
                ("rid".into(), rid.into()),
                ("userId".into(), uid.into()),
                ("ignore".into(), ignore.to_string()),
            ],
        ),
    }
}

pub async fn run_call(rest: RestClient, call: Call) -> Result<()> {
    match call {
        Call::Post(path, body) => rest.post_ok(&path, &body).await,
        Call::Get(path, q) => {
            let mut query = Query::new();
            for (k, v) in &q {
                query = query.push(k, v);
            }
            let _: Value = rest.get(&path, &query).await?;
            Ok(())
        }
    }
}

// ---- invites ---------------------------------------------------------------------------

/// Adds one user (`channels.invite` / `groups.invite`, one username per call
/// like the web client).
pub async fn invite_users(
    rest: RestClient,
    t: RoomType,
    rid: String,
    usernames: Vec<String>,
) -> Result<()> {
    let path = format!(
        "{}.invite",
        if t == RoomType::Private {
            "groups"
        } else {
            "channels"
        }
    );
    for u in usernames {
        let _: Value = rest
            .post(&path, &json!({ "roomId": rid, "username": u }))
            .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Invite {
    #[serde(rename = "_id")]
    pub id: String,
    pub url: String,
    pub days: u64,
    #[serde(rename = "maxUses")]
    pub max_uses: u64,
    #[serde(deserialize_with = "lenient::opt")]
    pub expires: Option<RcDate>,
}

pub async fn find_or_create_invite(rest: RestClient, rid: String) -> Result<Invite> {
    rest.post(
        "findOrCreateInvite",
        &json!({ "rid": rid, "days": 7, "maxUses": 0 }),
    )
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn paginated_skips_invalid() {
        let v = json!({"files": [{"_id": "a", "name": "x.txt", "size": 3}, 5], "total": 9, "offset": 0, "count": 2});
        let p: Paginated<FileItem> = paginated(v, "files");
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.raw_len, 2);
        assert!(p.has_more());
    }

    #[test]
    fn notification_prefs_roundtrip() {
        let sub: Subscription = serde_json::from_value(json!({
            "_id": "s", "rid": "r", "name": "n", "t": "c", "u": {"_id": "u"},
            "disableNotifications": false, "hideMentionStatus": true,
            "audioNotificationValue": "none", "muteGroupMentions": true,
            "desktopNotifications": "mentions", "desktopPrefOrigin": "subscription",
            "emailNotifications": "nothing", "emailPrefOrigin": "user",
            "mobilePushNotifications": "all"
        }))
        .unwrap();
        let p = NotificationPrefs::from_subscription(&sub);
        assert!(p.turn_on && p.mute_group_mentions && p.show_counter && !p.show_mentions);
        assert_eq!(p.desktop, Alert::Mentions);
        assert_eq!(p.email, Alert::Default, "origin user is not the room's");
        assert_eq!(p.mobile, Alert::Default, "no origin");
        assert_eq!(
            p.payload("r"),
            json!({"roomId": "r", "notifications": {
                "disableNotifications": "0", "muteGroupMentions": "1",
                "hideUnreadStatus": "0", "hideMentionStatus": "1",
                "desktopNotifications": "mentions", "audioNotificationValue": "none",
                "mobilePushNotifications": "default", "emailNotifications": "default"
            }})
        );
    }

    #[test]
    fn room_settings_payload_only_changes() {
        let room: Room = serde_json::from_value(json!({
            "_id": "r", "t": "c", "name": "dev", "topic": "t", "ro": false, "sysMes": true
        }))
        .unwrap();
        let before = RoomSettings::from_room(&room);
        assert!(!before.hide_system_messages);
        assert_eq!(before.payload("r", &before), None);
        let mut after = before.clone();
        after.topic = "new".into();
        after.private = true;
        after.hide_system_messages = true;
        let p = after.payload("r", &before).unwrap();
        assert_eq!(p["rid"], "r");
        assert_eq!(p["roomTopic"], "new");
        assert_eq!(p["roomType"], "p");
        assert!(p.get("roomName").is_none());
        assert_eq!(
            p["systemMessages"].as_array().unwrap().len(),
            SYSTEM_MESSAGE_TYPES.len()
        );
        let mut archived = before.clone();
        archived.archived = true;
        assert_eq!(archived.payload("r", &before), None);
    }

    #[test]
    fn room_settings_from_sysmes_list() {
        let room: Room =
            serde_json::from_value(json!({"_id": "r", "t": "p", "sysMes": ["uj"]})).unwrap();
        let s = RoomSettings::from_room(&room);
        assert!(s.hide_system_messages && s.private);
    }

    #[test]
    fn room_settings_validation() {
        let mut s = RoomSettings {
            name: " ".into(),
            ..Default::default()
        };
        assert_eq!(s.validate(false), Some("room_panel.err_name_required"));
        s.name = "a b".into();
        assert_eq!(s.validate(false), Some("room_panel.err_name_spaces"));
        assert_eq!(s.validate(true), None);
        s.name = "ab".into();
        assert_eq!(s.validate(false), None);
    }

    #[test]
    fn user_action_endpoints() {
        let c = user_action_call(
            &UserAction::SetRole(Role::Owner, true),
            RoomType::Channel,
            "r",
            None,
            "u",
            "bob",
        );
        assert_eq!(
            c,
            Call::Post(
                "channels.addOwner".into(),
                json!({"roomId": "r", "userId": "u"})
            )
        );
        let c = user_action_call(
            &UserAction::SetRole(Role::Leader, false),
            RoomType::Private,
            "r",
            None,
            "u",
            "bob",
        );
        assert!(matches!(c, Call::Post(p, _) if p == "groups.removeLeader"));
        let c = user_action_call(
            &UserAction::Mute(true),
            RoomType::Private,
            "r",
            None,
            "u",
            "bob",
        );
        assert_eq!(
            c,
            Call::Post(
                "rooms.muteUser".into(),
                json!({"roomId": "r", "username": "bob"})
            )
        );
        let c = user_action_call(&UserAction::Remove, RoomType::Private, "r", None, "u", "b");
        assert!(matches!(c, Call::Post(p, _) if p == "groups.kick"));
        let c = user_action_call(
            &UserAction::Remove,
            RoomType::Channel,
            "r",
            Some("t"),
            "u",
            "b",
        );
        assert_eq!(
            c,
            Call::Post(
                "teams.removeMember".into(),
                json!({"teamId": "t", "userId": "u"})
            )
        );
        let c = user_action_call(
            &UserAction::Ignore(false),
            RoomType::Channel,
            "r",
            None,
            "u",
            "b",
        );
        assert!(
            matches!(c, Call::Get(p, q) if p == "chat.ignoreUser" && q.contains(&("ignore".into(), "false".into())))
        );
    }

    #[test]
    fn files_query_params() {
        let q = files_query("r", Some("image"), " cat ", 25);
        let s = q.apply("channels.files");
        assert!(s.contains("roomId=r"));
        assert!(s.contains("typeGroup=image"));
        assert!(s.contains("name=cat"));
        assert!(s.contains("offset=25"));
        assert!(s.contains("onlyConfirmed=true"));
        let q = files_query("r", None, "", 0);
        assert!(!q.apply("x").contains("typeGroup"));
        assert!(!q.apply("x").contains("name="));
    }

    #[test]
    fn alert_from_sub() {
        assert_eq!(
            Alert::from_sub(Some("subscription"), Some("all")),
            Alert::All
        );
        assert_eq!(Alert::from_sub(None, Some("all")), Alert::Default);
    }
}
