//! Team helpers derived from the [`Workspace`] room map, and the team /
//! video-call permission rules (docs/teams-videoconf.md §1, §4, §5, §7.9).
//!
//! Team fields are only on **room** documents (8.9 never writes `teamId` /
//! `teamMain` into subscriptions), so everything here reads `room.team_id`
//! with the subscription only as a fallback. Team permissions are evaluated
//! in the scope of the team's main room, like the server; when the user is not
//! in the main room only global roles apply.

use rocketchat::models::RoomType;

use crate::permissions::{has_any_permission, has_permission, ids};
use crate::workspace::{RoomEntry, Workspace};

/// Permission ids of teams and calls.
pub mod perms {
    pub const DELETE_TEAM: &str = "delete-team";
    pub const CONVERT_TEAM: &str = "convert-team";
    pub const EDIT_TEAM: &str = "edit-team";
    pub const ADD_TEAM_MEMBER: &str = "add-team-member";
    pub const EDIT_TEAM_MEMBER: &str = "edit-team-member";
    pub const MOVE_ROOM_TO_TEAM: &str = "move-room-to-team";
    pub const CREATE_TEAM_CHANNEL: &str = "create-team-channel";
    pub const CREATE_TEAM_GROUP: &str = "create-team-group";
    pub const EDIT_TEAM_CHANNEL: &str = "edit-team-channel";
    pub const REMOVE_TEAM_CHANNEL: &str = "remove-team-channel";
    pub const VIEW_ALL_TEAM_CHANNELS: &str = "view-all-team-channels";
    pub const VIEW_ALL_TEAMS: &str = "view-all-teams";
    pub const CALL_MANAGEMENT: &str = "call-management";
    pub const VIDEOCONF_RING_USERS: &str = "videoconf-ring-users";
    pub const ACCESS_PERMISSIONS: &str = "access-permissions";
    pub const ASSIGN_ADMIN_ROLE: &str = "assign-admin-role";
}

/// Team id of `rid` (main or child room).
pub fn team_id_of<'a>(ws: &'a Workspace, rid: &str) -> Option<&'a str> {
    let e = ws.room(rid)?;
    entry_team_id(e)
}

fn entry_team_id(e: &RoomEntry) -> Option<&str> {
    e.room
        .as_ref()
        .and_then(|r| r.team_id.as_deref())
        .or_else(|| e.sub.as_ref().and_then(|s| s.team_id.as_deref()))
        .filter(|t| !t.is_empty())
}

/// The main room of team `team_id`, when it is in the workspace.
pub fn main_room_of(ws: &Workspace, team_id: &str) -> Option<String> {
    ws.rooms()
        .values()
        .find(|e| e.is_team() && entry_team_id(e) == Some(team_id))
        .map(|e| e.rid().to_owned())
}

/// `rid` is a child room of a team (has a `teamId`, is not the main room).
pub fn is_child_room(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .is_some_and(|e| !e.is_team() && entry_team_id(e).is_some())
}

/// `(team id, main room id if known)` of `rid`.
pub fn team_of(ws: &Workspace, rid: &str) -> Option<(String, Option<String>)> {
    let team = team_id_of(ws, rid)?.to_owned();
    let main = main_room_of(ws, &team);
    Some((team, main))
}

/// Subscribed rooms of the team: the main room first, then child rooms and
/// discussions of the main room, newest activity first.
pub fn children_of(ws: &Workspace, team_id: &str) -> Vec<String> {
    let main = main_room_of(ws, team_id);
    let mut rest: Vec<&RoomEntry> = ws
        .rooms()
        .values()
        .filter(|e| e.sub.is_some() && !e.is_team())
        .filter(|e| {
            entry_team_id(e) == Some(team_id)
                || (main.is_some()
                    && e.room
                        .as_ref()
                        .and_then(|r| r.prid.as_deref())
                        .or_else(|| e.sub.as_ref().and_then(|s| s.prid.as_deref()))
                        == main.as_deref())
        })
        .collect();
    rest.sort_by(|a, b| {
        b.last_activity()
            .cmp(&a.last_activity())
            .then_with(|| a.rid().cmp(b.rid()))
    });
    main.into_iter()
        .chain(rest.into_iter().map(|e| e.rid().to_owned()))
        .collect()
}

/// Scope of team permission checks: the main room of the team of `rid`
/// (`rid` itself for a main room).
fn scope(ws: &Workspace, rid: &str) -> Option<String> {
    if ws.room(rid).is_some_and(RoomEntry::is_team) {
        return Some(rid.to_owned());
    }
    team_id_of(ws, rid).and_then(|t| main_room_of(ws, t))
}

fn team_perm(ws: &Workspace, perm: &str, rid: &str) -> bool {
    has_permission(ws, perm, scope(ws, rid).as_deref())
}

/// `teams.delete` needs `delete-team` **and** `delete-c|p` on the main room
/// (`eraseRoom`, §2.6).
pub fn can_delete_team(ws: &Workspace, main_rid: &str) -> bool {
    let erase = match ws.room(main_rid).map(RoomEntry::room_type) {
        Some(RoomType::Private) => ids::DELETE_P,
        Some(RoomType::Channel) => ids::DELETE_C,
        _ => return false,
    };
    team_perm(ws, perms::DELETE_TEAM, main_rid) && has_permission(ws, erase, Some(main_rid))
}

/// `teams.convertToChannel`.
pub fn can_convert_team(ws: &Workspace, main_rid: &str) -> bool {
    team_perm(ws, perms::CONVERT_TEAM, main_rid)
}

/// `teams.update` (Rustket edits through `rooms.saveRoomSettings`, `edit-room`).
pub fn can_edit_team(ws: &Workspace, main_rid: &str) -> bool {
    team_perm(ws, perms::EDIT_TEAM, main_rid)
}

/// "Add existing" rooms (`teams.addRooms`).
pub fn can_add_team_rooms(ws: &Workspace, rid: &str) -> bool {
    team_perm(ws, perms::MOVE_ROOM_TO_TEAM, rid)
}

/// "Create new" room in the team of type `t` (`c` or `p`).
pub fn can_create_team_room(ws: &Workspace, rid: &str, t: RoomType) -> bool {
    match t {
        RoomType::Channel => {
            team_perm(ws, perms::CREATE_TEAM_CHANNEL, rid)
                && has_permission(ws, ids::CREATE_C, None)
        }
        RoomType::Private => {
            team_perm(ws, perms::CREATE_TEAM_GROUP, rid) && has_permission(ws, ids::CREATE_P, None)
        }
        _ => false,
    }
}

/// Any room can be created in the team.
pub fn can_create_any_team_room(ws: &Workspace, rid: &str) -> bool {
    can_create_team_room(ws, rid, RoomType::Channel)
        || can_create_team_room(ws, rid, RoomType::Private)
}

/// Auto-join toggle (`teams.updateRoom`, `edit-team-channel` in the main room).
pub fn can_toggle_autojoin(ws: &Workspace, rid: &str) -> bool {
    team_perm(ws, perms::EDIT_TEAM_CHANNEL, rid)
}

/// "Remove from team" (`teams.removeRoom`).
pub fn can_remove_team_room(ws: &Workspace, rid: &str) -> bool {
    team_perm(ws, perms::REMOVE_TEAM_CHANNEL, rid)
}

/// Add members (`teams.addMembers`: `add-team-member` or `edit-team-member`).
pub fn can_add_team_members(ws: &Workspace, rid: &str) -> bool {
    has_any_permission(
        ws,
        &[perms::ADD_TEAM_MEMBER, perms::EDIT_TEAM_MEMBER],
        scope(ws, rid).as_deref(),
    )
}

/// Remove members / replace roles (`edit-team-member`).
pub fn can_manage_team_members(ws: &Workspace, rid: &str) -> bool {
    team_perm(ws, perms::EDIT_TEAM_MEMBER, rid)
}

/// `canChangeTeamRoles` for one role.
pub fn can_change_team_role(ws: &Workspace, rid: &str, role: &str) -> bool {
    match role {
        "owner" => team_perm(ws, ids::SET_OWNER, rid),
        "moderator" => team_perm(ws, ids::SET_MODERATOR, rid),
        "leader" => team_perm(ws, ids::SET_LEADER, rid),
        "admin" => {
            has_permission(ws, perms::ACCESS_PERMISSIONS, None)
                && has_permission(ws, perms::ASSIGN_ADMIN_ROLE, None)
        }
        _ => has_permission(ws, perms::ACCESS_PERMISSIONS, None),
    }
}

/// "Convert to team" of a channel or group: `create-team` + `edit-room`, not
/// in a team, not a discussion, not federated (the web gate, §2.2).
pub fn can_convert_to_team(ws: &Workspace, rid: &str) -> bool {
    let Some(e) = ws.room(rid) else {
        return false;
    };
    let room = e.room.as_ref();
    matches!(e.room_type(), RoomType::Channel | RoomType::Private)
        && entry_team_id(e).is_none()
        && !e.is_team()
        && !e.is_discussion()
        && room.and_then(|r| r.federated) != Some(true)
        && has_permission(ws, ids::CREATE_TEAM, None)
        && has_permission(ws, ids::CREATE_TEAM, Some(rid))
        && crate::permissions::can_edit_room(ws, rid)
}

/// The "Video call" room action (§7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallButton {
    Hidden,
    /// Shown but disabled (federated, read-only without `post-readonly`,
    /// archived).
    Disabled,
    Enabled,
}

/// Web gate of the "Video call" action (`useVideoCallRoomAction`).
pub fn video_call_button(ws: &Workspace, rid: &str) -> CallButton {
    let Some(e) = ws.room(rid) else {
        return CallButton::Hidden;
    };
    let s = ws.settings();
    let room = e.room.as_ref();
    let enabled_for_type = match e.room_type() {
        RoomType::Direct => s.bool_or("VideoConf_Enable_DMs", true),
        RoomType::Private if e.is_team() => s.bool_or("VideoConf_Enable_Teams", true),
        RoomType::Channel if e.is_team() => s.bool_or("VideoConf_Enable_Teams", true),
        RoomType::Private => s.bool_or("VideoConf_Enable_Groups", true),
        RoomType::Channel => s.bool_or("VideoConf_Enable_Channels", true),
        RoomType::Livechat => s.str("Omnichannel_call_provider") == Some("default-provider"),
        _ => false,
    };
    if !enabled_for_type || !has_permission(ws, perms::CALL_MANAGEMENT, Some(rid)) {
        return CallButton::Hidden;
    }
    let me = ws.username().unwrap_or_default();
    if !me.is_empty()
        && room
            .and_then(|r| r.muted.as_ref())
            .is_some_and(|m| m.iter().any(|u| u == me))
    {
        return CallButton::Hidden;
    }
    if e.room_type() == RoomType::Direct
        && room
            .and_then(|r| r.uids.as_ref())
            .is_some_and(|u| u.len() == 1)
    {
        return CallButton::Hidden;
    }
    let read_only = e.is_read_only() && !has_permission(ws, ids::POST_READONLY, Some(rid));
    if room.and_then(|r| r.federated) == Some(true) || read_only || e.is_archived() {
        return CallButton::Disabled;
    }
    CallButton::Enabled
}

/// A 1:1 DM (where `start` may ring the other user on Enterprise).
pub fn is_direct_call_room(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid).is_some_and(|e| {
        e.room_type() == RoomType::Direct
            && e.room
                .as_ref()
                .and_then(|r| r.uids.as_ref())
                .is_none_or(|u| u.len() <= 2)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{me, room, sub, ts};
    use crate::workspace::Change;
    use rocketchat::models::Permission;

    fn perm(id: &str, roles: &[&str]) -> Permission {
        Permission {
            id: id.into(),
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        let mut main = room("M", "dev", RoomType::Channel);
        main.team_id = Some("T".into());
        main.team_main = Some(true);
        let mut child = room("C", "child", RoomType::Private);
        child.team_id = Some("T".into());
        child.lm = Some(ts(2_000));
        let mut disc = room("D", "d1", RoomType::Channel);
        disc.prid = Some("M".into());
        disc.lm = Some(ts(3_000));
        let other = room("O", "other", RoomType::Channel);
        for r in [main, child, disc, other] {
            let rid = r.id.clone();
            let name = r.name.clone().unwrap();
            let t = r.t;
            ws.apply_room(Change::Inserted, r);
            let mut s = sub(&rid, &name, t);
            if rid == "M" {
                s.roles = Some(vec!["owner".into()]);
            }
            ws.apply_subscription(Change::Inserted, s);
        }
        for (p, roles) in [
            ("delete-team", &["admin", "owner"][..]),
            ("delete-c", &["admin", "owner"]),
            ("convert-team", &["admin", "owner"]),
            ("edit-team-channel", &["admin", "owner", "moderator"]),
            ("call-management", &["admin", "owner", "moderator", "user"]),
            ("set-owner", &["admin", "owner"]),
            ("create-team", &["admin", "user"]),
            ("edit-room", &["admin", "owner"]),
        ] {
            ws.apply_permission(Change::Inserted, perm(p, roles));
        }
        ws
    }

    #[test]
    fn derived_indexes() {
        let ws = ws();
        assert_eq!(main_room_of(&ws, "T").as_deref(), Some("M"));
        assert_eq!(team_id_of(&ws, "C"), Some("T"));
        assert!(is_child_room(&ws, "C") && !is_child_room(&ws, "M"));
        assert_eq!(children_of(&ws, "T"), vec!["M", "D", "C"]);
        assert_eq!(team_of(&ws, "O"), None);
    }

    #[test]
    fn permissions_scoped_to_main_room() {
        let ws = ws();
        // Owner of the main room: team rights apply to child rooms too.
        assert!(can_delete_team(&ws, "M"));
        assert!(can_convert_team(&ws, "M"));
        assert!(can_toggle_autojoin(&ws, "C"));
        assert!(can_change_team_role(&ws, "C", "owner"));
        assert!(!can_change_team_role(&ws, "C", "admin"));
        // `O` is no team room: no main-room scope, only global roles.
        assert!(!can_toggle_autojoin(&ws, "O"));
        // Converting: an owned plain channel only.
        assert!(!can_convert_to_team(&ws, "O"), "not owner of O");
        assert!(!can_convert_to_team(&ws, "M"), "already a team");
        assert!(!can_convert_to_team(&ws, "D"), "discussion");
    }

    #[test]
    fn call_button_rules() {
        let mut ws = ws();
        assert_eq!(video_call_button(&ws, "M"), CallButton::Enabled);
        let mut r = ws.room("O").unwrap().room.clone().unwrap();
        r.archived = Some(true);
        ws.apply_room(Change::Updated, r.clone());
        assert_eq!(video_call_button(&ws, "O"), CallButton::Disabled);
        r.archived = None;
        r.muted = Some(vec!["me".into()]);
        ws.apply_room(Change::Updated, r);
        assert_eq!(video_call_button(&ws, "O"), CallButton::Hidden);
        let mut self_dm = room("S", "me", RoomType::Direct);
        self_dm.uids = Some(vec!["me".into()]);
        ws.apply_room(Change::Inserted, self_dm);
        ws.apply_subscription(Change::Inserted, sub("S", "me", RoomType::Direct));
        assert_eq!(video_call_button(&ws, "S"), CallButton::Hidden);
        assert_eq!(video_call_button(&ws, "nope"), CallButton::Hidden);
    }
}
