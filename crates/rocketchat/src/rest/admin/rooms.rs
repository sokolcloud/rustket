//! Room administration (docs/admin.md §5).

use serde_json::{Value, json};

use super::{Page, paged};
use crate::error::Result;
use crate::models::Paginated;
use crate::models::admin::{AdminRoom, AdminRoomType, RoomSettingsUpdate};
use crate::rest::{Query, RestClient, field};

impl RestClient {
    /// `rooms.adminRooms`. An empty `types` sends all six types (like the web);
    /// without `Discussions`/`Teams` those rooms are excluded entirely.
    pub async fn admin_rooms(
        &self,
        filter: &str,
        types: &[AdminRoomType],
        page: &Page,
    ) -> Result<Paginated<AdminRoom>> {
        let mut q = page.apply(Query::new());
        if !filter.is_empty() {
            q = q.push("filter", filter);
        }
        let types: &[AdminRoomType] = if types.is_empty() {
            &AdminRoomType::ALL
        } else {
            types
        };
        for t in types {
            q = q.push("types[]", t.as_str());
        }
        let v: Value = self.get("rooms.adminRooms", &q).await?;
        paged(v, "rooms", page)
    }

    /// `rooms.adminRooms.getRoom` (room fields at the top level).
    pub async fn admin_room(&self, rid: &str) -> Result<AdminRoom> {
        self.get("rooms.adminRooms.getRoom", &Query::new().push("rid", rid))
            .await
    }

    /// `rooms.autocomplete.adminRooms`: any room by name (needs
    /// `view-room-administration`), e.g. for `roomPick` settings.
    pub async fn admin_rooms_autocomplete(&self, name: &str) -> Result<Vec<AdminRoom>> {
        let v: Value = self
            .get(
                "rooms.autocomplete.adminRooms",
                &Query::new().push("selector", json!({ "name": name })),
            )
            .await?;
        field(v, "items")
    }

    /// `rooms.saveRoomSettings`.
    pub async fn admin_save_room_settings(
        &self,
        rid: &str,
        update: &RoomSettingsUpdate,
    ) -> Result<()> {
        let mut body = serde_json::to_value(update)?;
        body["rid"] = Value::String(rid.to_owned());
        self.post_ok("rooms.saveRoomSettings", &body).await
    }

    /// `rooms.changeArchivationState`.
    pub async fn admin_change_archivation_state(&self, rid: &str, archive: bool) -> Result<()> {
        self.post_ok(
            "rooms.changeArchivationState",
            &json!({ "rid": rid, "action": if archive { "archive" } else { "unarchive" } }),
        )
        .await
    }

    /// `teams.delete` (a team's main room cannot be deleted with `rooms.delete`).
    pub async fn admin_delete_team(&self, team_id: &str, rooms_to_remove: &[String]) -> Result<()> {
        let mut body = json!({ "teamId": team_id });
        if !rooms_to_remove.is_empty() {
            body["roomsToRemove"] = json!(rooms_to_remove);
        }
        self.post_ok("teams.delete", &body).await
    }
}
