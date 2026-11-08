//! Teams and discussions (docs/teams-videoconf.md §2, §6).
//!
//! Traps verified on 8.9.1: `rooms`/`roomsToRemove: []` is rejected on
//! `teams.leave`/`removeMember`/`delete` (the key is omitted when empty), and
//! `teams.convertToChannel` erases a main room listed in `roomsToRemove`, so
//! callers must never pass it there.

use serde::Serialize;
use serde_json::{Value, json};

use super::{Query, RestClient, field, paginated};
use crate::error::Result;
use crate::models::{
    Message, Paginated, Room, RoomInfoExtended, Team, TeamAutocompleteItem, TeamListItem,
    TeamMember, TeamRoomOfUser, TeamType, UserTeam,
};

/// How a team is addressed (`teamId` or `teamName`; never both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRef<'a> {
    Id(&'a str),
    Name(&'a str),
}

impl TeamRef<'_> {
    fn key(&self) -> (&'static str, &str) {
        match self {
            Self::Id(id) => ("teamId", id),
            Self::Name(n) => ("teamName", n),
        }
    }

    fn query(&self) -> Query {
        let (k, v) = self.key();
        Query::new().push(k, v)
    }

    /// A JSON body with the team key.
    pub fn body(&self) -> Value {
        let (k, v) = self.key();
        json!({ k: v })
    }
}

/// `offset`/`count` of a list request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Page {
    pub offset: u64,
    pub count: u64,
}

impl Page {
    pub fn new(offset: u64, count: u64) -> Self {
        Self { offset, count }
    }

    fn apply(&self, q: Query) -> Query {
        let q = q.push("offset", self.offset);
        if self.count > 0 {
            q.push("count", self.count)
        } else {
            q
        }
    }
}

/// Parameters of `teams.create`.
#[derive(Debug, Clone, Default)]
pub struct CreateTeam {
    pub name: String,
    pub private: bool,
    /// User ids or usernames.
    pub members: Vec<String>,
    /// Owner other than the caller (the caller is then not a member).
    pub owner: Option<String>,
    pub read_only: bool,
    pub topic: Option<String>,
    pub broadcast: bool,
    pub encrypted: bool,
}

impl CreateTeam {
    pub fn body(&self) -> Value {
        let mut extra = json!({ "broadcast": self.broadcast, "encrypted": self.encrypted });
        if let Some(t) = self.topic.as_deref().filter(|t| !t.is_empty()) {
            extra["topic"] = json!(t);
        }
        let mut b = json!({
            "name": self.name,
            "type": if self.private { TeamType::Private } else { TeamType::Public }.code(),
            "room": { "readOnly": self.read_only, "extraData": extra },
        });
        if !self.members.is_empty() {
            b["members"] = json!(self.members);
        }
        if let Some(o) = &self.owner {
            b["owner"] = json!(o);
        }
        b
    }
}

/// Which children `teams.listChildren` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildKind {
    Channels,
    Discussions,
}

/// Filters of `teams.members`.
#[derive(Debug, Clone, Default)]
pub struct MemberFilter {
    pub status: Vec<String>,
    pub username: Option<String>,
    pub name: Option<String>,
}

#[derive(Serialize)]
struct NewMember<'a> {
    #[serde(rename = "userId")]
    user_id: &'a str,
    #[serde(skip_serializing_if = "<[&str]>::is_empty")]
    roles: &'a [&'a str],
}

/// `body[key] = rooms` unless `rooms` is empty (the server rejects `[]`).
fn with_rooms(mut body: Value, key: &str, rooms: &[&str]) -> Value {
    if !rooms.is_empty() {
        body[key] = json!(rooms);
    }
    body
}

/// Body of `teams.delete`.
pub fn delete_body(team: TeamRef<'_>, rooms_to_remove: &[&str]) -> Value {
    with_rooms(team.body(), "roomsToRemove", rooms_to_remove)
}

/// Body of `teams.leave`.
pub fn leave_body(team: TeamRef<'_>, rooms: &[&str]) -> Value {
    with_rooms(team.body(), "rooms", rooms)
}

/// Body of `teams.removeMember`.
pub fn remove_member_body(team: TeamRef<'_>, uid: &str, rooms: &[&str]) -> Value {
    let mut b = team.body();
    b["userId"] = json!(uid);
    with_rooms(b, "rooms", rooms)
}

impl RestClient {
    /// `teams.create`.
    pub async fn teams_create(&self, params: &CreateTeam) -> Result<Team> {
        let v: Value = self.post("teams.create", &params.body()).await?;
        field(v, "team")
    }

    /// `teams.info`.
    pub async fn teams_info(&self, team: TeamRef<'_>) -> Result<Team> {
        let v: Value = self.get("teams.info", &team.query()).await?;
        field(v, "teamInfo")
    }

    /// `teams.list`: the caller's teams.
    pub async fn teams_list(&self, page: Page) -> Result<Paginated<TeamListItem>> {
        let v: Value = self.get("teams.list", &page.apply(Query::new())).await?;
        paginated(v, "teams")
    }

    /// `teams.listAll` (`view-all-teams`).
    pub async fn teams_list_all(&self, page: Page) -> Result<Paginated<TeamListItem>> {
        let v: Value = self.get("teams.listAll", &page.apply(Query::new())).await?;
        paginated(v, "teams")
    }

    /// `teams.autocomplete`: main rooms whose name starts with `name`.
    pub async fn teams_autocomplete(&self, name: &str) -> Result<Vec<TeamAutocompleteItem>> {
        let v: Value = self
            .get("teams.autocomplete", &Query::new().push("name", name))
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("teams").cloned().unwrap_or_default(),
        ))
    }

    /// `teams.update` (renames/retypes the main room too).
    pub async fn teams_update(
        &self,
        team: TeamRef<'_>,
        name: Option<&str>,
        kind: Option<TeamType>,
    ) -> Result<()> {
        let mut data = json!({});
        if let Some(n) = name {
            data["name"] = json!(n);
        }
        if let Some(k) = kind {
            data["type"] = json!(k.code());
        }
        let mut body = team.body();
        body["data"] = data;
        self.post_ok("teams.update", &body).await
    }

    /// `teams.delete`: erases `rooms_to_remove` too, detaches the other rooms.
    pub async fn teams_delete(&self, team: TeamRef<'_>, rooms_to_remove: &[&str]) -> Result<()> {
        self.post_ok("teams.delete", &delete_body(team, rooms_to_remove))
            .await
    }

    /// `teams.convertToChannel`. **Never** pass the main room in
    /// `rooms_to_remove`: the server erases it.
    pub async fn teams_convert_to_channel(
        &self,
        team: TeamRef<'_>,
        rooms_to_remove: &[&str],
    ) -> Result<()> {
        let mut body = team.body();
        body["roomsToRemove"] = json!(rooms_to_remove);
        self.post_ok("teams.convertToChannel", &body).await
    }

    /// `channels.convertToTeam`.
    pub async fn channels_convert_to_team(&self, rid: &str) -> Result<Team> {
        let v: Value = self
            .post("channels.convertToTeam", &json!({ "channelId": rid }))
            .await?;
        field(v, "team")
    }

    /// `groups.convertToTeam`.
    pub async fn groups_convert_to_team(&self, rid: &str) -> Result<Team> {
        let v: Value = self
            .post("groups.convertToTeam", &json!({ "roomId": rid }))
            .await?;
        field(v, "team")
    }

    /// `teams.listRooms`: child rooms (never the main room). `filter` matches
    /// `name`; `auto_join_only` keeps `teamDefault` rooms.
    pub async fn teams_list_rooms(
        &self,
        team: TeamRef<'_>,
        filter: Option<&str>,
        auto_join_only: bool,
        page: Page,
    ) -> Result<Paginated<Room>> {
        let q = team
            .query()
            .opt("filter", filter.filter(|f| !f.is_empty()))
            .opt("type", auto_join_only.then_some("autoJoin"));
        let v: Value = self.get("teams.listRooms", &page.apply(q)).await?;
        paginated(v, "rooms")
    }

    /// `teams.listChildren`: the main room, visible child rooms and the main
    /// room's discussions (list key `data`).
    pub async fn teams_list_children(
        &self,
        team: TeamRef<'_>,
        filter: Option<&str>,
        kind: Option<ChildKind>,
        page: Page,
    ) -> Result<Paginated<Room>> {
        let q = team
            .query()
            .opt("filter", filter.filter(|f| !f.is_empty()))
            .opt(
                "type",
                kind.map(|k| match k {
                    ChildKind::Channels => "channels",
                    ChildKind::Discussions => "discussions",
                }),
            );
        let v: Value = self.get("teams.listChildren", &page.apply(q)).await?;
        let mut p: Paginated<Room> = paginated(v, "data")?;
        // `count` echoes the request, not the page size.
        p.count = p.raw_len as u64;
        Ok(p)
    }

    /// `teams.listRoomsOfUser` (`offset` is always answered as 0).
    pub async fn teams_list_rooms_of_user(
        &self,
        team: TeamRef<'_>,
        uid: &str,
        can_user_delete: bool,
        page: Page,
    ) -> Result<Paginated<TeamRoomOfUser>> {
        let q = team
            .query()
            .push("userId", uid)
            .opt("canUserDelete", can_user_delete.then_some("true"));
        let v: Value = self.get("teams.listRoomsOfUser", &page.apply(q)).await?;
        let mut p: Paginated<TeamRoomOfUser> = paginated(v, "rooms")?;
        p.offset = page.offset;
        Ok(p)
    }

    /// `teams.addRooms` (the caller must own each room). The returned rooms
    /// are pre-update copies.
    pub async fn teams_add_rooms(&self, team: TeamRef<'_>, rooms: &[&str]) -> Result<Vec<Room>> {
        let mut body = team.body();
        body["rooms"] = json!(rooms);
        let v: Value = self.post("teams.addRooms", &body).await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("rooms").cloned().unwrap_or_default(),
        ))
    }

    /// `teams.removeRoom`: the room survives as a normal room.
    pub async fn teams_remove_room(&self, team: TeamRef<'_>, rid: &str) -> Result<Room> {
        let mut body = team.body();
        body["roomId"] = json!(rid);
        let v: Value = self.post("teams.removeRoom", &body).await?;
        field(v, "room")
    }

    /// `teams.updateRoom`: auto-join on/off.
    pub async fn teams_update_room(&self, rid: &str, is_default: bool) -> Result<Room> {
        let v: Value = self
            .post(
                "teams.updateRoom",
                &json!({ "roomId": rid, "isDefault": is_default }),
            )
            .await?;
        field(v, "room")
    }

    /// `teams.members`.
    pub async fn teams_members(
        &self,
        team: TeamRef<'_>,
        filter: &MemberFilter,
        page: Page,
    ) -> Result<Paginated<TeamMember>> {
        let mut q = team
            .query()
            .opt(
                "username",
                filter.username.as_deref().filter(|s| !s.is_empty()),
            )
            .opt("name", filter.name.as_deref().filter(|s| !s.is_empty()));
        for s in &filter.status {
            q = q.push("status[]", s);
        }
        let v: Value = self.get("teams.members", &page.apply(q)).await?;
        paginated(v, "members")
    }

    /// `teams.addMembers`: `(user id, roles)` pairs.
    pub async fn teams_add_members(
        &self,
        team: TeamRef<'_>,
        members: &[(&str, &[&str])],
    ) -> Result<()> {
        let list: Vec<NewMember<'_>> = members
            .iter()
            .map(|(user_id, roles)| NewMember { user_id, roles })
            .collect();
        let mut body = team.body();
        body["members"] = serde_json::to_value(list)?;
        self.post_ok("teams.addMembers", &body).await
    }

    /// `teams.updateMember`: **replaces** the member's team roles.
    pub async fn teams_update_member(
        &self,
        team: TeamRef<'_>,
        uid: &str,
        roles: &[&str],
    ) -> Result<()> {
        let mut body = team.body();
        body["member"] = json!({ "userId": uid, "roles": roles });
        self.post_ok("teams.updateMember", &body).await
    }

    /// `teams.removeMember` (also from `rooms`). Never pass the main room.
    pub async fn teams_remove_member(
        &self,
        team: TeamRef<'_>,
        uid: &str,
        rooms: &[&str],
    ) -> Result<()> {
        self.post_ok("teams.removeMember", &remove_member_body(team, uid, rooms))
            .await
    }

    /// `teams.leave` (also `rooms`). Never pass the main room.
    pub async fn teams_leave(&self, team: TeamRef<'_>, rooms: &[&str]) -> Result<()> {
        self.post_ok("teams.leave", &leave_body(team, rooms)).await
    }

    /// `rooms.autocomplete.availableForTeams`: rooms the caller owns that are
    /// in no team.
    pub async fn rooms_autocomplete_available_for_teams(
        &self,
        name: Option<&str>,
    ) -> Result<Vec<Room>> {
        let v: Value = self
            .get(
                "rooms.autocomplete.availableForTeams",
                &Query::new().opt("name", name.filter(|n| !n.is_empty())),
            )
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("items").cloned().unwrap_or_default(),
        ))
    }

    /// `users.listTeams`.
    pub async fn users_list_teams(&self, uid: &str) -> Result<Vec<UserTeam>> {
        let v: Value = self
            .get("users.listTeams", &Query::new().push("userId", uid))
            .await?;
        Ok(crate::models::lenient::vec_from_value(
            v.get("teams").cloned().unwrap_or_default(),
        ))
    }

    /// `rooms.info` with `team` / `parent`.
    pub async fn rooms_info_extended(&self, rid: &str) -> Result<RoomInfoExtended> {
        self.get("rooms.info", &Query::new().push("roomId", rid))
            .await
    }

    /// `chat.getDiscussions`: the `discussion-created` messages of `rid`
    /// (newest first), filtered by title.
    pub async fn chat_get_discussions(
        &self,
        rid: &str,
        text: Option<&str>,
        page: Page,
    ) -> Result<Paginated<Message>> {
        let q = Query::new()
            .push("roomId", rid)
            .opt("text", text.filter(|t| !t.trim().is_empty()));
        let v: Value = self.get("chat.getDiscussions", &page.apply(q)).await?;
        paginated(v, "messages")
    }

    /// `rooms.getDiscussions`: discussion rooms of `rid`.
    pub async fn rooms_get_discussions(&self, rid: &str, page: Page) -> Result<Paginated<Room>> {
        let v: Value = self
            .get(
                "rooms.getDiscussions",
                &page.apply(Query::new().push("roomId", rid)),
            )
            .await?;
        paginated(v, "discussions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_room_lists_are_omitted() {
        let t = TeamRef::Id("T");
        assert_eq!(delete_body(t, &[]), json!({"teamId": "T"}));
        assert_eq!(
            delete_body(t, &["a"]),
            json!({"teamId": "T", "roomsToRemove": ["a"]})
        );
        assert_eq!(leave_body(t, &[]), json!({"teamId": "T"}));
        assert_eq!(
            remove_member_body(TeamRef::Name("n"), "u", &[]),
            json!({"teamName": "n", "userId": "u"})
        );
        assert_eq!(
            remove_member_body(t, "u", &["r"]),
            json!({"teamId": "T", "userId": "u", "rooms": ["r"]})
        );
    }

    #[test]
    fn create_body() {
        let b = CreateTeam {
            name: "dev".into(),
            private: true,
            members: vec!["bob".into()],
            topic: Some("x".into()),
            ..Default::default()
        }
        .body();
        assert_eq!(
            b,
            json!({"name": "dev", "type": 1, "members": ["bob"],
                   "room": {"readOnly": false, "extraData": {"broadcast": false, "encrypted": false, "topic": "x"}}})
        );
    }
}
