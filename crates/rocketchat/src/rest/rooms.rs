//! Rooms and subscriptions.

use serde::Serialize;
use serde_json::{Value, json};

use super::{Query, RestClient, field, paginated};
use crate::RcDate;
use crate::error::Result;
use crate::models::{Paginated, Room, RoomType, Subscription, UpdatedSince, UserSummary};

/// Parameters of `channels.create` / `groups.create`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRoom {
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<Value>,
}

impl CreateRoom {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
}

/// Options of [`RestClient::room_members`].
#[derive(Debug, Clone, Default)]
pub struct RoomMembersOptions {
    pub offset: Option<u64>,
    pub count: Option<u64>,
    /// Text filter on username/name.
    pub filter: Option<String>,
    /// e.g. `["online", "away"]`.
    pub status: Vec<String>,
}

impl RestClient {
    /// `GET subscriptions.get[?updatedSince=]`.
    pub async fn subscriptions_get(
        &self,
        updated_since: Option<RcDate>,
    ) -> Result<UpdatedSince<Subscription>> {
        self.get(
            "subscriptions.get",
            &Query::new().opt("updatedSince", updated_since.map(|d| d.to_iso())),
        )
        .await
    }

    /// `GET subscriptions.getOne?roomId=`.
    pub async fn subscriptions_get_one(&self, rid: &str) -> Result<Subscription> {
        let v: Value = self
            .get("subscriptions.getOne", &Query::new().push("roomId", rid))
            .await?;
        field(v, "subscription")
    }

    /// `GET rooms.get[?updatedSince=]`.
    pub async fn rooms_get(&self, updated_since: Option<RcDate>) -> Result<UpdatedSince<Room>> {
        self.get(
            "rooms.get",
            &Query::new().opt("updatedSince", updated_since.map(|d| d.to_iso())),
        )
        .await
    }

    /// `GET rooms.info?roomId=`.
    pub async fn rooms_info(&self, rid: &str) -> Result<Room> {
        let v: Value = self
            .get("rooms.info", &Query::new().push("roomId", rid))
            .await?;
        field(v, "room")
    }

    /// Mark a room (and its threads) as read.
    pub async fn subscriptions_read(&self, rid: &str) -> Result<()> {
        self.post_ok(
            "subscriptions.read",
            &json!({ "rid": rid, "readThreads": true }),
        )
        .await
    }

    /// Mark as unread starting from `first_unread_message_id`, or the whole room.
    pub async fn subscriptions_unread(
        &self,
        rid: &str,
        first_unread_message_id: Option<&str>,
    ) -> Result<()> {
        let body = match first_unread_message_id {
            Some(id) => json!({ "firstUnreadMessage": { "_id": id } }),
            None => json!({ "roomId": rid }),
        };
        self.post_ok("subscriptions.unread", &body).await
    }

    pub async fn rooms_favorite(&self, rid: &str, favorite: bool) -> Result<()> {
        self.post_ok(
            "rooms.favorite",
            &json!({ "roomId": rid, "favorite": favorite }),
        )
        .await
    }

    /// Hide a room from the sidebar (until a new message arrives).
    pub async fn rooms_hide(&self, rid: &str) -> Result<()> {
        self.post_ok("rooms.hide", &json!({ "roomId": rid })).await
    }

    /// Re-open a hidden room.
    pub async fn rooms_open(&self, rid: &str) -> Result<()> {
        self.post_ok("rooms.open", &json!({ "roomId": rid })).await
    }

    pub async fn rooms_leave(&self, rid: &str) -> Result<()> {
        self.post_ok("rooms.leave", &json!({ "roomId": rid })).await
    }

    /// Join a public room (`rooms.join` works for any joinable room type).
    pub async fn rooms_join(&self, rid: &str, join_code: Option<&str>) -> Result<Room> {
        let mut body = json!({ "roomId": rid });
        if let Some(c) = join_code {
            body["joinCode"] = Value::String(c.into());
        }
        let v: Value = self.post("rooms.join", &body).await?;
        field(v, "room")
    }

    /// `channels.join`.
    pub async fn channels_join(&self, rid: &str) -> Result<Room> {
        let v: Value = self
            .post("channels.join", &json!({ "roomId": rid }))
            .await?;
        field(v, "channel")
    }

    /// Open (or create) a DM with one user.
    pub async fn im_create(&self, username: &str) -> Result<Room> {
        let v: Value = self
            .post("im.create", &json!({ "username": username }))
            .await?;
        field(v, "room")
    }

    /// Open (or create) a group DM.
    pub async fn im_create_group(&self, usernames: &[&str]) -> Result<Room> {
        let v: Value = self
            .post("im.create", &json!({ "usernames": usernames.join(",") }))
            .await?;
        field(v, "room")
    }

    pub async fn channels_create(&self, params: &CreateRoom) -> Result<Room> {
        let v: Value = self.post("channels.create", params).await?;
        field(v, "channel")
    }

    pub async fn groups_create(&self, params: &CreateRoom) -> Result<Room> {
        let v: Value = self.post("groups.create", params).await?;
        field(v, "group")
    }

    /// Room members of any room type (`rooms.membersOrderedByRole`: owners,
    /// moderators, then others; each member carries `roles`).
    pub async fn room_members(
        &self,
        rid: &str,
        opts: &RoomMembersOptions,
    ) -> Result<Paginated<UserSummary>> {
        let mut q = Query::new()
            .push("roomId", rid)
            .opt("offset", opts.offset)
            .opt("count", opts.count)
            .opt("filter", opts.filter.as_deref());
        for s in &opts.status {
            q = q.push("status[]", s);
        }
        let v: Value = self.get("rooms.membersOrderedByRole", &q).await?;
        paginated(v, "members")
    }

    /// Type-specific members endpoint (`channels.members` / `groups.members` / `im.members`).
    pub async fn room_members_by_type(
        &self,
        rid: &str,
        room_type: RoomType,
        offset: u64,
        count: u64,
    ) -> Result<Paginated<UserSummary>> {
        let prefix = match room_type {
            RoomType::Private => "groups",
            RoomType::Direct => "im",
            _ => "channels",
        };
        let v: Value = self
            .get(
                &format!("{prefix}.members"),
                &Query::new()
                    .push("roomId", rid)
                    .push("offset", offset)
                    .push("count", count),
            )
            .await?;
        paginated(v, "members")
    }

    /// Delete a room (owner/admin).
    pub async fn rooms_delete(&self, rid: &str) -> Result<()> {
        self.post_ok("rooms.delete", &json!({ "roomId": rid }))
            .await
    }

    /// Create a discussion in `prid`.
    pub async fn rooms_create_discussion(
        &self,
        prid: &str,
        name: &str,
        reply: Option<&str>,
        users: &[&str],
    ) -> Result<Room> {
        let mut body = json!({ "prid": prid, "t_name": name, "users": users });
        if let Some(r) = reply {
            body["reply"] = Value::String(r.into());
        }
        let v: Value = self.post("rooms.createDiscussion", &body).await?;
        field(v, "discussion")
    }
}
