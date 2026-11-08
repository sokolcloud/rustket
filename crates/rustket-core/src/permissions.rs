//! Permission checks with Rocket.Chat 8.9's rules.
//!
//! `has_permission` = (global roles of the user ∪ roles of the subscription for
//! `rid`) ∩ roles of the permission. The derived checks follow the server
//! (`canSendMessage`, `canDeleteMessage`, `updateMessage`, `setReaction`,
//! `createDiscussion`, `eraseRoom`) and the web client's message toolbar and
//! composer (`useEditMessageAction`, `chats/data.ts` `canDeleteMessage`,
//! `ReactionMessageAction`, `roomCoordinator.readOnly`), so the UI never offers an
//! action the server refuses. Pending (unsent) messages get no server actions.

use chrono::{DateTime, Utc};
use rocketchat::models::{Message, RoomType};

use crate::workspace::{RoomEntry, Workspace};

/// Permission ids used below.
pub mod ids {
    pub const EDIT_MESSAGE: &str = "edit-message";
    pub const DELETE_MESSAGE: &str = "delete-message";
    pub const DELETE_OWN_MESSAGE: &str = "delete-own-message";
    pub const FORCE_DELETE_MESSAGE: &str = "force-delete-message";
    pub const BYPASS_TIME_LIMIT: &str = "bypass-time-limit-edit-and-delete";
    pub const PIN_MESSAGE: &str = "pin-message";
    pub const POST_READONLY: &str = "post-readonly";
    pub const CREATE_C: &str = "create-c";
    pub const CREATE_P: &str = "create-p";
    pub const CREATE_D: &str = "create-d";
    pub const CREATE_TEAM: &str = "create-team";
    pub const START_DISCUSSION: &str = "start-discussion";
    pub const START_DISCUSSION_OTHER_USER: &str = "start-discussion-other-user";
    pub const MENTION_ALL: &str = "mention-all";
    pub const MENTION_HERE: &str = "mention-here";
    pub const VIEW_ROOM_ADMINISTRATION: &str = "view-room-administration";
    pub const VIEW_USER_ADMINISTRATION: &str = "view-user-administration";
    pub const VIEW_STATISTICS: &str = "view-statistics";
    pub const VIEW_PRIVILEGED_SETTING: &str = "view-privileged-setting";
    pub const EDIT_ROOM: &str = "edit-room";
    pub const DELETE_C: &str = "delete-c";
    pub const DELETE_P: &str = "delete-p";
    pub const DELETE_D: &str = "delete-d";
    pub const DELETE_TEAM_CHANNEL: &str = "delete-team-channel";
    pub const DELETE_TEAM_GROUP: &str = "delete-team-group";
    pub const ADD_USER_TO_JOINED_ROOM: &str = "add-user-to-joined-room";
    pub const ADD_USER_TO_ANY_C_ROOM: &str = "add-user-to-any-c-room";
    pub const ADD_USER_TO_ANY_P_ROOM: &str = "add-user-to-any-p-room";
    pub const REMOVE_USER: &str = "remove-user";
    pub const MUTE_USER: &str = "mute-user";
    pub const SET_OWNER: &str = "set-owner";
    pub const SET_MODERATOR: &str = "set-moderator";
    pub const SET_LEADER: &str = "set-leader";
    pub const ARCHIVE_ROOM: &str = "archive-room";
    pub const UNARCHIVE_ROOM: &str = "unarchive-room";
    pub const VIEW_C_ROOM: &str = "view-c-room";
    pub const VIEW_JOINED_ROOM: &str = "view-joined-room";
    pub const CREATE_INVITE_LINKS: &str = "create-invite-links";
    pub const CLEAN_CHANNEL_HISTORY: &str = "clean-channel-history";
    pub const VIEW_BROADCAST_MEMBER_LIST: &str = "view-broadcast-member-list";
}

/// Admin-area permissions; any of them shows the administration entry. Built from
/// the section table in [`crate::admin`] (docs/admin.md §1).
pub const ADMIN_PERMISSIONS: &[&str] = crate::admin::ADMIN_PERMISSIONS;

/// Whether the current user has `perm`, globally or (with `rid`) through their
/// room roles.
pub fn has_permission(ws: &Workspace, perm: &str, rid: Option<&str>) -> bool {
    let Some(allowed) = ws.permission_roles(perm) else {
        return false;
    };
    let global = ws.me().and_then(|m| m.roles.as_deref()).unwrap_or_default();
    let room = rid
        .and_then(|r| ws.subscription(r))
        .and_then(|s| s.roles.as_deref())
        .unwrap_or_default();
    global
        .iter()
        .chain(room.iter())
        .any(|r| allowed.iter().any(|a| a == r))
}

pub fn has_any_permission(ws: &Workspace, perms: &[&str], rid: Option<&str>) -> bool {
    perms.iter().any(|p| has_permission(ws, p, rid))
}

fn is_own(ws: &Workspace, m: &Message) -> bool {
    ws.user_id().is_some_and(|id| id == m.u.id)
}

/// A local message that the server does not know yet (Sending/Failed): the UI
/// offers retry/discard instead of server actions.
fn is_pending(m: &Message) -> bool {
    m.temp == Some(true)
}

fn is_muted(ws: &Workspace, entry: &RoomEntry) -> bool {
    let username = ws.username().unwrap_or_default();
    entry
        .room
        .as_ref()
        .and_then(|r| r.muted.as_ref())
        .is_some_and(|m| m.iter().any(|u| u == username))
}

fn is_unmuted(ws: &Workspace, entry: &RoomEntry) -> bool {
    let username = ws.username().unwrap_or_default();
    entry
        .room
        .as_ref()
        .and_then(|r| r.unmuted.as_ref())
        .is_some_and(|m| m.iter().any(|u| u == username))
}

/// The server's read-only rule (`canSendMessage`, `canDeleteMessage`,
/// `setReaction`): `ro` and neither `post-readonly` nor listed in `unmuted`.
fn read_only_blocks(ws: &Workspace, rid: &str, entry: &RoomEntry) -> bool {
    entry.is_read_only()
        && !has_permission(ws, ids::POST_READONLY, Some(rid))
        && !is_unmuted(ws, entry)
}

/// The room-state part of the server's `canSendMessage` (also run for edits and
/// new discussions), in its order: archived, blocked DM, read-only, muted.
/// Unlike [`post_block`] it does not require a subscription.
pub fn room_state_block(ws: &Workspace, rid: &str) -> Option<PostBlock> {
    let entry = ws.room(rid)?;
    if entry.is_archived() {
        return Some(PostBlock::Archived);
    }
    if let Some(sub) = entry.sub.as_ref() {
        if sub.blocker == Some(true) {
            return Some(PostBlock::Blocker);
        }
        if sub.blocked == Some(true) {
            return Some(PostBlock::Blocked);
        }
    }
    if read_only_blocks(ws, rid, entry) {
        return Some(PostBlock::ReadOnly);
    }
    if is_muted(ws, entry) {
        return Some(PostBlock::Muted);
    }
    None
}

/// `canEditMessage` as the web client's Edit action and the server's
/// `updateMessage` apply it: not for pending or system messages, only with a
/// subscription (web client); `edit-message`, or own message with
/// `Message_AllowEditing`; then `Message_AllowEditing_BlockEditInMinutes`
/// (truncated minutes `<` limit) unless `bypass-time-limit-edit-and-delete`;
/// finally the room must accept messages from the user ([`room_state_block`]:
/// archived, blocked DM, read-only, muted — the server refuses edits there).
pub fn can_edit_message(ws: &Workspace, m: &Message, now: DateTime<Utc>) -> bool {
    if is_pending(m) || m.is_system() || ws.subscription(&m.rid).is_none() {
        return false;
    }
    let rid = Some(m.rid.as_str());
    let has_perm = has_permission(ws, ids::EDIT_MESSAGE, rid);
    let edit_own = ws.settings().allow_editing() && is_own(ws, m);
    if !has_perm && !edit_own {
        return false;
    }
    let block = ws.settings().edit_block_minutes();
    if block > 0 && !has_permission(ws, ids::BYPASS_TIME_LIMIT, rid) {
        match m.ts {
            Some(ts) if (now - ts.0).num_minutes() < block => {}
            _ => return false,
        }
    }
    room_state_block(ws, &m.rid).is_none()
}

/// `canDeleteMessage`: never for pending or system messages (web client) or
/// without a subscription; then the server's `canDeleteMessageAsync`:
/// `force-delete-message` always; a `ts` is required; `Message_AllowDeleting`;
/// `delete-message`, or own message with `delete-own-message`; unless
/// `bypass-time-limit-edit-and-delete`, a positive
/// `Message_AllowDeleting_BlockDeleteInMinutes` decides alone with
/// `round(elapsed minutes) <= limit`; otherwise a read-only room refuses users
/// without `post-readonly` who are not in `unmuted`.
pub fn can_delete_message(ws: &Workspace, m: &Message, now: DateTime<Utc>) -> bool {
    if is_pending(m) || m.is_system() {
        return false;
    }
    let Some(entry) = ws.room(&m.rid).filter(|e| e.sub.is_some()) else {
        return false;
    };
    let rid = Some(m.rid.as_str());
    if has_permission(ws, ids::FORCE_DELETE_MESSAGE, rid) {
        return true;
    }
    let Some(ts) = m.ts else {
        return false;
    };
    if !ws.settings().allow_deleting() {
        return false;
    }
    let allowed = has_permission(ws, ids::DELETE_MESSAGE, rid)
        || (is_own(ws, m) && has_permission(ws, ids::DELETE_OWN_MESSAGE, rid));
    if !allowed {
        return false;
    }
    let block = ws.settings().delete_block_minutes();
    if block > 0 && !has_permission(ws, ids::BYPASS_TIME_LIMIT, rid) {
        // `Math.round(ms / 60000)`; f64::round matches it for non-negative values.
        let elapsed = ((now - ts.0).num_milliseconds() as f64 / 60_000.0).round();
        return elapsed <= block as f64;
    }
    !read_only_blocks(ws, &m.rid, entry)
}

/// `Message_AllowPinning` and `pin-message` in the room.
pub fn can_pin(ws: &Workspace, rid: &str) -> bool {
    ws.settings().allow_pinning() && has_permission(ws, ids::PIN_MESSAGE, Some(rid))
}

/// `Message_AllowStarring` and the user is subscribed.
pub fn can_star(ws: &Workspace, rid: &str) -> bool {
    ws.settings().allow_starring() && ws.subscription(rid).is_some()
}

/// `roomCoordinator.readOnly`: muted users are read-only; in a read-only room only
/// users in `unmuted` or with `post-readonly` may post.
pub fn is_read_only(ws: &Workspace, rid: &str) -> bool {
    let Some(entry) = ws.room(rid) else {
        return false;
    };
    is_muted(ws, entry) || read_only_blocks(ws, rid, entry)
}

/// Why the composer is disabled, if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostBlock {
    NotSubscribed,
    Archived,
    Muted,
    ReadOnly,
    /// DM: the current user blocked the partner.
    Blocker,
    /// DM: the partner blocked the current user.
    Blocked,
}

impl PostBlock {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::NotSubscribed => "composer.join_to_post",
            Self::Archived => "composer.archived",
            Self::Muted => "composer.muted",
            Self::ReadOnly => "composer.read_only",
            Self::Blocker => "composer.you_blocked",
            Self::Blocked => "composer.blocked",
        }
    }
}

/// `None` when the user may post in `rid`: subscribed, then [`room_state_block`].
pub fn post_block(ws: &Workspace, rid: &str) -> Option<PostBlock> {
    if ws.subscription(rid).is_none() {
        return Some(PostBlock::NotSubscribed);
    }
    room_state_block(ws, rid)
}

pub fn can_post(ws: &Workspace, rid: &str) -> bool {
    post_block(ws, rid).is_none()
}

/// Reactions (web `ReactionMessageAction`, server `setReaction`): subscribed, not
/// muted, and in a read-only room only with `reactWhenReadOnly`, `post-readonly`
/// or when listed in `unmuted`. Archived rooms accept reactions.
pub fn can_react(ws: &Workspace, rid: &str) -> bool {
    let Some(entry) = ws.room(rid) else {
        return false;
    };
    if entry.sub.is_none() || is_muted(ws, entry) {
        return false;
    }
    let react_when_ro = entry.room.as_ref().and_then(|r| r.react_when_read_only) == Some(true);
    react_when_ro || !read_only_blocks(ws, rid, entry)
}

pub fn can_create_channel(ws: &Workspace) -> bool {
    has_permission(ws, ids::CREATE_C, None)
}

pub fn can_create_group(ws: &Workspace) -> bool {
    has_permission(ws, ids::CREATE_P, None)
}

pub fn can_create_dm(ws: &Workspace) -> bool {
    has_permission(ws, ids::CREATE_D, None)
}

pub fn can_create_team(ws: &Workspace) -> bool {
    has_permission(ws, ids::CREATE_TEAM, None)
}

/// The parent a new discussion started from `rid` gets: a discussion's own
/// parent (`room.prid || room._id`, as the web client passes it; the server
/// refuses nested discussions).
pub fn discussion_parent(ws: &Workspace, rid: &str) -> String {
    ws.room(rid)
        .and_then(|e| {
            e.room
                .as_ref()
                .and_then(|r| r.prid.clone())
                .or_else(|| e.sub.as_ref().and_then(|s| s.prid.clone()))
        })
        .unwrap_or_else(|| rid.to_owned())
}

/// `Discussion_enabled` and `start-discussion` / `start-discussion-other-user`.
/// With a parent room (resolved through [`discussion_parent`]) the permission is
/// checked there and the parent must accept messages from the user
/// ([`room_state_block`]), as the server's `createDiscussion` requires.
pub fn can_create_discussion(ws: &Workspace, parent_rid: Option<&str>) -> bool {
    if !ws.settings().discussions_enabled() {
        return false;
    }
    let parent = parent_rid.map(|rid| discussion_parent(ws, rid));
    if let Some(p) = parent.as_deref()
        && room_state_block(ws, p).is_some()
    {
        return false;
    }
    has_any_permission(
        ws,
        &[ids::START_DISCUSSION, ids::START_DISCUSSION_OTHER_USER],
        parent.as_deref(),
    )
}

/// The web client's "Start discussion" message action: discussions enabled, the
/// message has no discussion yet, subscribed, not omnichannel, and
/// `start-discussion` (own message) / `start-discussion-other-user` in the room;
/// additionally the parent must accept messages (server `createDiscussion`).
pub fn can_start_discussion_from(ws: &Workspace, m: &Message) -> bool {
    if !ws.settings().discussions_enabled()
        || is_pending(m)
        || m.drid.is_some()
        || m.dcount.is_some()
    {
        return false;
    }
    let Some(entry) = ws.room(&m.rid).filter(|e| e.sub.is_some()) else {
        return false;
    };
    if matches!(entry.room_type(), RoomType::Livechat | RoomType::Voip) {
        return false;
    }
    let perm = if is_own(ws, m) {
        ids::START_DISCUSSION
    } else {
        ids::START_DISCUSSION_OTHER_USER
    };
    has_permission(ws, perm, Some(&m.rid))
        && room_state_block(ws, &discussion_parent(ws, &m.rid)).is_none()
}

/// Threads: `Threads_enabled`, not a system or pending message, and allowed to post.
pub fn can_reply_in_thread(ws: &Workspace, m: &Message) -> bool {
    ws.settings().threads_enabled() && !m.is_system() && !is_pending(m) && can_post(ws, &m.rid)
}

pub fn can_mention_all(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::MENTION_ALL, Some(rid))
}

pub fn can_mention_here(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::MENTION_HERE, Some(rid))
}

pub fn can_view_room_admin(ws: &Workspace) -> bool {
    has_permission(ws, ids::VIEW_ROOM_ADMINISTRATION, None)
}

/// Whether to show the administration entry at all.
pub fn can_view_admin(ws: &Workspace) -> bool {
    has_any_permission(ws, ADMIN_PERMISSIONS, None)
}

pub fn can_edit_room(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::EDIT_ROOM, Some(rid))
}

/// The main room of team `team_id`, when the user is in it.
fn team_main_room(ws: &Workspace, team_id: &str) -> Option<String> {
    ws.rooms()
        .values()
        .find(|e| {
            e.is_team()
                && (e.room.as_ref().and_then(|r| r.team_id.as_deref()) == Some(team_id)
                    || e.sub.as_ref().and_then(|s| s.team_id.as_deref()) == Some(team_id))
        })
        .map(|e| e.rid().to_owned())
}

/// `rooms.delete` (`eraseRoom`, web `useDeleteRoom`): `delete-{t}` in the room
/// (channels, private groups and DMs); never federated rooms or team main rooms
/// (those go through `teams.delete`); a room inside a team additionally needs
/// `delete-team-channel` / `delete-team-group` in the team's main room.
pub fn can_delete_room(ws: &Workspace, rid: &str) -> bool {
    let Some(entry) = ws.room(rid) else {
        return false;
    };
    let room = entry.room.as_ref();
    if room.and_then(|r| r.federated) == Some(true) {
        return false;
    }
    if entry.is_team() {
        return false;
    }
    let t = entry.room_type();
    let (perm, team_perm) = match t {
        RoomType::Channel => (ids::DELETE_C, ids::DELETE_TEAM_CHANNEL),
        RoomType::Private => (ids::DELETE_P, ids::DELETE_TEAM_GROUP),
        RoomType::Direct => (ids::DELETE_D, ids::DELETE_TEAM_GROUP),
        _ => return false,
    };
    if !has_permission(ws, perm, Some(rid)) {
        return false;
    }
    let team_id = room
        .and_then(|r| r.team_id.as_deref())
        .or_else(|| entry.sub.as_ref().and_then(|s| s.team_id.as_deref()));
    match team_id {
        Some(team) => {
            let main = team_main_room(ws, team);
            has_permission(ws, team_perm, main.as_deref())
        }
        None => true,
    }
}

pub fn can_add_users(ws: &Workspace, rid: &str) -> bool {
    let any = match ws.room(rid).map(|e| e.room_type()) {
        Some(RoomType::Channel) => ids::ADD_USER_TO_ANY_C_ROOM,
        Some(RoomType::Private) => ids::ADD_USER_TO_ANY_P_ROOM,
        _ => return false,
    };
    has_any_permission(ws, &[ids::ADD_USER_TO_JOINED_ROOM, any], Some(rid))
}

pub fn can_remove_users(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::REMOVE_USER, Some(rid))
}

pub fn can_mute_users(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::MUTE_USER, Some(rid))
}

pub fn can_archive_room(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::ARCHIVE_ROOM, Some(rid))
}

pub fn can_create_invite_links(ws: &Workspace, rid: &str) -> bool {
    has_permission(ws, ids::CREATE_INVITE_LINKS, Some(rid))
}

/// Upload button: `FileUpload_Enabled` and allowed to post.
pub fn can_upload(ws: &Workspace, rid: &str) -> bool {
    ws.settings().file_upload_enabled() && can_post(ws, rid)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{me, msg, room, sub, ts};
    use crate::workspace::Change;
    use rocketchat::models::{Permission, Setting};
    use serde_json::json;

    fn perm(ws: &mut Workspace, id: &str, roles: &[&str]) {
        ws.apply_permission(
            Change::Updated,
            Permission {
                id: id.into(),
                roles: roles.iter().map(|r| (*r).to_owned()).collect(),
                updated_at: None,
            },
        );
    }

    fn setting(ws: &mut Workspace, id: &str, v: serde_json::Value) {
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: id.into(),
                value: v,
                ..Default::default()
            },
        );
    }

    fn base() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me("me", "me"));
        ws.apply_subscription(Change::Inserted, sub("r", "room", RoomType::Channel));
        ws.apply_room(Change::Inserted, room("r", "room", RoomType::Channel));
        perm(&mut ws, ids::DELETE_OWN_MESSAGE, &["user"]);
        perm(
            &mut ws,
            ids::DELETE_MESSAGE,
            &["admin", "owner", "moderator"],
        );
        perm(&mut ws, ids::EDIT_MESSAGE, &["admin", "owner", "moderator"]);
        perm(&mut ws, ids::PIN_MESSAGE, &["owner", "moderator", "admin"]);
        perm(
            &mut ws,
            ids::POST_READONLY,
            &["admin", "owner", "moderator"],
        );
        perm(&mut ws, ids::CREATE_C, &["admin", "user", "bot"]);
        perm(&mut ws, ids::CREATE_TEAM, &["admin"]);
        ws
    }

    fn now() -> DateTime<Utc> {
        ts(100 * 60_000).0
    }

    fn set_room(ws: &mut Workspace, f: impl FnOnce(&mut rocketchat::models::Room)) {
        let mut r = ws.room("r").unwrap().room.clone().unwrap();
        f(&mut r);
        ws.apply_room(Change::Updated, r);
    }

    fn set_sub(
        ws: &mut Workspace,
        rid: &str,
        f: impl FnOnce(&mut rocketchat::models::Subscription),
    ) {
        let mut s = ws.subscription(rid).unwrap().clone();
        f(&mut s);
        ws.apply_subscription(Change::Updated, s);
    }

    fn mins_ago(id: &str, uid: &str, ms_ago: i64) -> Message {
        msg(id, "r", now().timestamp_millis() - ms_ago, uid)
    }

    #[test]
    fn global_and_room_roles() {
        let mut ws = base();
        assert!(has_permission(&ws, ids::CREATE_C, None));
        assert!(!has_permission(&ws, ids::CREATE_TEAM, None));
        assert!(!has_permission(&ws, ids::PIN_MESSAGE, Some("r")));
        assert!(!has_permission(&ws, "unknown-perm", None));
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["owner".into()]));
        assert!(has_permission(&ws, ids::PIN_MESSAGE, Some("r")));
        assert!(
            !has_permission(&ws, ids::PIN_MESSAGE, None),
            "room role needs rid"
        );
        assert!(can_pin(&ws, "r"));
        setting(&mut ws, "Message_AllowPinning", json!(false));
        assert!(!can_pin(&ws, "r"));
    }

    #[test]
    fn edit_rules() {
        let mut ws = base();
        let own = msg("m", "r", 95 * 60_000, "me");
        let other = msg("o", "r", 95 * 60_000, "x");
        assert!(can_edit_message(&ws, &own, now()));
        assert!(!can_edit_message(&ws, &other, now()));
        setting(&mut ws, "Message_AllowEditing_BlockEditInMinutes", json!(3));
        assert!(!can_edit_message(&ws, &own, now()), "5 minutes > 3");
        setting(
            &mut ws,
            "Message_AllowEditing_BlockEditInMinutes",
            json!(10),
        );
        assert!(can_edit_message(&ws, &own, now()));
        setting(&mut ws, "Message_AllowEditing", json!(false));
        assert!(!can_edit_message(&ws, &own, now()));
        // Moderator can edit others' messages regardless of the setting.
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["moderator".into()]));
        assert!(can_edit_message(&ws, &other, now()));
        // System messages never.
        let mut sys = own.clone();
        sys.t = Some("uj".into());
        assert!(!can_edit_message(&ws, &sys, now()));
    }

    #[test]
    fn edit_time_limit_truncates_minutes_and_is_exclusive() {
        // Server: moment().diff(ts, 'minutes') >= block is refused.
        let mut ws = base();
        setting(&mut ws, "Message_AllowEditing_BlockEditInMinutes", json!(5));
        assert!(can_edit_message(
            &ws,
            &mins_ago("a", "me", 5 * 60_000 - 1),
            now()
        ));
        assert!(!can_edit_message(
            &ws,
            &mins_ago("b", "me", 5 * 60_000),
            now()
        ));
        let mut no_ts = mins_ago("c", "me", 0);
        no_ts.ts = None;
        assert!(!can_edit_message(&ws, &no_ts, now()), "web client hides it");
        perm(&mut ws, ids::BYPASS_TIME_LIMIT, &["user"]);
        assert!(can_edit_message(
            &ws,
            &mins_ago("d", "me", 60 * 60_000),
            now()
        ));
    }

    #[test]
    fn edit_refused_in_archived_room() {
        let mut ws = base();
        set_room(&mut ws, |r| r.archived = Some(true));
        assert!(!can_edit_message(&ws, &mins_ago("m", "me", 60_000), now()));
    }

    #[test]
    fn edit_refused_in_read_only_room_without_post_readonly() {
        let mut ws = base();
        set_room(&mut ws, |r| r.ro = Some(true));
        let own = mins_ago("m", "me", 60_000);
        assert!(!can_edit_message(&ws, &own, now()));
        set_room(&mut ws, |r| r.unmuted = Some(vec!["me".into()]));
        assert!(can_edit_message(&ws, &own, now()), "unmuted");
        set_room(&mut ws, |r| r.unmuted = None);
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["owner".into()]));
        assert!(can_edit_message(&ws, &own, now()), "post-readonly");
    }

    #[test]
    fn edit_refused_when_muted_even_for_moderators() {
        let mut ws = base();
        set_room(&mut ws, |r| r.muted = Some(vec!["me".into()]));
        assert!(!can_edit_message(&ws, &mins_ago("m", "me", 60_000), now()));
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["moderator".into()]));
        assert!(!can_edit_message(&ws, &mins_ago("o", "x", 60_000), now()));
    }

    #[test]
    fn edit_refused_in_blocked_dm() {
        let mut ws = base();
        let mut d = sub("d", "bob", RoomType::Direct);
        d.blocked = Some(true);
        ws.apply_subscription(Change::Inserted, d);
        let own = msg("m", "d", now().timestamp_millis() - 60_000, "me");
        assert!(!can_edit_message(&ws, &own, now()));
        set_sub(&mut ws, "d", |s| {
            s.blocked = None;
            s.blocker = Some(true);
        });
        assert!(!can_edit_message(&ws, &own, now()));
        set_sub(&mut ws, "d", |s| s.blocker = None);
        assert!(can_edit_message(&ws, &own, now()));
    }

    #[test]
    fn edit_and_delete_refused_for_pending_and_unsubscribed() {
        let mut ws = base();
        ws.open_room("r");
        let local = ws.new_local_message("r", "hi", None, false);
        assert!(!can_edit_message(&ws, &local, now()));
        assert!(!can_delete_message(&ws, &local, now()));
        perm(&mut ws, ids::FORCE_DELETE_MESSAGE, &["user"]);
        assert!(!can_delete_message(&ws, &local, now()));
        let elsewhere = msg("m", "nope", now().timestamp_millis(), "me");
        assert!(!can_edit_message(&ws, &elsewhere, now()));
        assert!(!can_delete_message(&ws, &elsewhere, now()));
    }

    #[test]
    fn delete_rules() {
        let mut ws = base();
        let own = msg("m", "r", 95 * 60_000, "me");
        let other = msg("o", "r", 95 * 60_000, "x");
        assert!(can_delete_message(&ws, &own, now()));
        assert!(!can_delete_message(&ws, &other, now()));
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(2),
        );
        assert!(!can_delete_message(&ws, &own, now()));
        perm(&mut ws, ids::BYPASS_TIME_LIMIT, &["user"]);
        assert!(can_delete_message(&ws, &own, now()));
        setting(&mut ws, "Message_AllowDeleting", json!(false));
        assert!(!can_delete_message(&ws, &own, now()));
        perm(&mut ws, ids::FORCE_DELETE_MESSAGE, &["user"]);
        assert!(can_delete_message(&ws, &other, now()));
    }

    #[test]
    fn delete_own_needs_delete_own_message() {
        let mut ws = base();
        perm(&mut ws, ids::DELETE_OWN_MESSAGE, &["admin"]);
        assert!(!can_delete_message(
            &ws,
            &mins_ago("m", "me", 60_000),
            now()
        ));
    }

    #[test]
    fn delete_others_with_room_moderator_role() {
        let mut ws = base();
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["moderator".into()]));
        let other = mins_ago("o", "x", 10 * 60_000);
        assert!(can_delete_message(&ws, &other, now()));
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(20),
        );
        assert!(can_delete_message(&ws, &other, now()), "inside the block");
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(5),
        );
        assert!(!can_delete_message(&ws, &other, now()), "outside the block");
    }

    #[test]
    fn delete_time_limit_rounds_and_is_inclusive() {
        // Server: Math.round(elapsed minutes) <= block.
        let mut ws = base();
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(5),
        );
        assert!(can_delete_message(
            &ws,
            &mins_ago("a", "me", 5 * 60_000 + 10_000),
            now()
        ));
        assert!(can_delete_message(
            &ws,
            &mins_ago("b", "me", 5 * 60_000 + 29_999),
            now()
        ));
        assert!(!can_delete_message(
            &ws,
            &mins_ago("c", "me", 5 * 60_000 + 30_000),
            now()
        ));
    }

    #[test]
    fn delete_without_ts_only_with_force_delete() {
        let mut ws = base();
        let mut m = mins_ago("m", "me", 0);
        m.ts = None;
        assert!(!can_delete_message(&ws, &m, now()));
        perm(&mut ws, ids::FORCE_DELETE_MESSAGE, &["user"]);
        assert!(can_delete_message(&ws, &m, now()));
    }

    #[test]
    fn delete_refused_in_read_only_room() {
        let mut ws = base();
        set_room(&mut ws, |r| r.ro = Some(true));
        let own = mins_ago("m", "me", 60_000);
        assert!(!can_delete_message(&ws, &own, now()));
        // A positive time block decides alone (server order).
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(5),
        );
        assert!(can_delete_message(&ws, &own, now()));
        setting(
            &mut ws,
            "Message_AllowDeleting_BlockDeleteInMinutes",
            json!(0),
        );
        set_room(&mut ws, |r| r.unmuted = Some(vec!["me".into()]));
        assert!(can_delete_message(&ws, &own, now()), "unmuted");
        set_room(&mut ws, |r| r.unmuted = None);
        perm(&mut ws, ids::POST_READONLY, &["user"]);
        assert!(can_delete_message(&ws, &own, now()), "post-readonly");
    }

    #[test]
    fn delete_never_offered_for_system_messages() {
        let mut ws = base();
        let mut sys = mins_ago("s", "me", 60_000);
        sys.t = Some("uj".into());
        assert!(!can_delete_message(&ws, &sys, now()));
        perm(&mut ws, ids::FORCE_DELETE_MESSAGE, &["user"]);
        assert!(!can_delete_message(&ws, &sys, now()));
    }

    #[test]
    fn posting_rules() {
        let mut ws = base();
        assert!(can_post(&ws, "r"));
        assert!(can_react(&ws, "r"));
        set_room(&mut ws, |r| r.ro = Some(true));
        assert_eq!(post_block(&ws, "r"), Some(PostBlock::ReadOnly));
        assert!(!can_react(&ws, "r"));
        set_room(&mut ws, |r| r.react_when_read_only = Some(true));
        assert!(can_react(&ws, "r"));
        set_room(&mut ws, |r| r.unmuted = Some(vec!["me".into()]));
        assert!(can_post(&ws, "r"));
        set_room(&mut ws, |r| {
            r.ro = Some(false);
            r.muted = Some(vec!["me".into()]);
        });
        assert_eq!(post_block(&ws, "r"), Some(PostBlock::Muted));
        set_room(&mut ws, |r| {
            r.muted = None;
            r.archived = Some(true);
        });
        assert_eq!(post_block(&ws, "r"), Some(PostBlock::Archived));
        assert_eq!(post_block(&ws, "nope"), Some(PostBlock::NotSubscribed));

        let mut d = sub("d", "bob", RoomType::Direct);
        d.blocked = Some(true);
        ws.apply_subscription(Change::Inserted, d.clone());
        assert_eq!(post_block(&ws, "d"), Some(PostBlock::Blocked));
        d.blocked = None;
        d.blocker = Some(true);
        ws.apply_subscription(Change::Updated, d);
        assert_eq!(post_block(&ws, "d"), Some(PostBlock::Blocker));
    }

    #[test]
    fn readonly_with_post_readonly_permission() {
        let mut ws = base();
        set_room(&mut ws, |r| r.ro = Some(true));
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["owner".into()]));
        assert!(can_post(&ws, "r"));
        assert!(can_react(&ws, "r"));
    }

    #[test]
    fn react_allowed_in_archived_rooms() {
        let mut ws = base();
        set_room(&mut ws, |r| r.archived = Some(true));
        assert!(can_react(&ws, "r"));
        assert!(!can_post(&ws, "r"));
    }

    #[test]
    fn react_refused_when_muted_or_unsubscribed() {
        let mut ws = base();
        set_room(&mut ws, |r| {
            r.muted = Some(vec!["me".into()]);
            r.react_when_read_only = Some(true);
        });
        assert!(!can_react(&ws, "r"));
        ws.apply_room(Change::Inserted, room("x", "x", RoomType::Channel));
        assert!(!can_react(&ws, "x"));
    }

    #[test]
    fn discussion_needs_a_writable_parent() {
        let mut ws = base();
        perm(&mut ws, ids::START_DISCUSSION, &["user"]);
        assert!(can_create_discussion(&ws, Some("r")));
        assert!(can_create_discussion(&ws, None));
        for f in [
            (|r: &mut rocketchat::models::Room| r.ro = Some(true)) as fn(&mut _),
            |r| r.archived = Some(true),
            |r| r.muted = Some(vec!["me".into()]),
        ] {
            let mut ws = ws.clone();
            set_room(&mut ws, f);
            assert!(!can_create_discussion(&ws, Some("r")));
        }
        setting(&mut ws, "Discussion_enabled", json!(false));
        assert!(!can_create_discussion(&ws, Some("r")));
    }

    #[test]
    fn discussion_inside_a_discussion_uses_its_parent() {
        let mut ws = base();
        perm(&mut ws, ids::START_DISCUSSION, &["user"]);
        let mut d = room("d", "disc", RoomType::Private);
        d.prid = Some("r".into());
        ws.apply_subscription(Change::Inserted, sub("d", "disc", RoomType::Private));
        ws.apply_room(Change::Inserted, d);
        assert_eq!(discussion_parent(&ws, "d"), "r");
        assert!(can_create_discussion(&ws, Some("d")));
        set_room(&mut ws, |r| r.ro = Some(true));
        assert!(
            !can_create_discussion(&ws, Some("d")),
            "parent is read-only"
        );
    }

    #[test]
    fn start_discussion_message_action() {
        let mut ws = base();
        perm(&mut ws, ids::START_DISCUSSION, &["user"]);
        let own = mins_ago("m", "me", 0);
        let other = mins_ago("o", "x", 0);
        assert!(can_start_discussion_from(&ws, &own));
        assert!(!can_start_discussion_from(&ws, &other));
        perm(&mut ws, ids::START_DISCUSSION_OTHER_USER, &["user"]);
        assert!(can_start_discussion_from(&ws, &other));
        let mut with_disc = own.clone();
        with_disc.drid = Some("d".into());
        assert!(!can_start_discussion_from(&ws, &with_disc));
    }

    #[test]
    fn delete_room_uses_delete_t() {
        let mut ws = base();
        perm(&mut ws, ids::DELETE_C, &["admin", "owner"]);
        perm(&mut ws, ids::DELETE_P, &["admin", "owner"]);
        perm(&mut ws, ids::DELETE_D, &["admin"]);
        assert!(!can_delete_room(&ws, "r"));
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["owner".into()]));
        assert!(can_delete_room(&ws, "r"));
        let mut p = sub("p", "priv", RoomType::Private);
        p.roles = Some(vec!["owner".into()]);
        ws.apply_subscription(Change::Inserted, p);
        assert!(can_delete_room(&ws, "p"));
        perm(&mut ws, ids::DELETE_P, &["admin"]);
        assert!(!can_delete_room(&ws, "p"), "delete-p, not delete-c");
        ws.apply_subscription(Change::Inserted, sub("d", "bob", RoomType::Direct));
        assert!(!can_delete_room(&ws, "d"));
        ws.apply_user_role_change("me", "admin", true, None);
        assert!(can_delete_room(&ws, "d"), "admins may delete DMs");
        ws.apply_subscription(Change::Inserted, sub("l", "visitor", RoomType::Livechat));
        assert!(!can_delete_room(&ws, "l"));
        set_room(&mut ws, |r| r.federated = Some(true));
        assert!(!can_delete_room(&ws, "r"), "federated");
    }

    #[test]
    fn delete_team_room_needs_team_permission() {
        let mut ws = base();
        perm(&mut ws, ids::DELETE_C, &["admin", "owner"]);
        perm(
            &mut ws,
            ids::DELETE_TEAM_CHANNEL,
            &["admin", "owner", "moderator"],
        );
        let mut main = room("tm", "team", RoomType::Private);
        main.team_main = Some(true);
        main.team_id = Some("T".into());
        ws.apply_subscription(Change::Inserted, sub("tm", "team", RoomType::Private));
        ws.apply_room(Change::Inserted, main);
        set_room(&mut ws, |r| r.team_id = Some("T".into()));
        set_sub(&mut ws, "r", |s| s.roles = Some(vec!["owner".into()]));
        assert!(!can_delete_room(&ws, "r"), "owner of the channel only");
        set_sub(&mut ws, "tm", |s| s.roles = Some(vec!["moderator".into()]));
        assert!(can_delete_room(&ws, "r"));
        set_sub(&mut ws, "tm", |s| s.roles = Some(vec!["owner".into()]));
        assert!(
            !can_delete_room(&ws, "tm"),
            "team main rooms use teams.delete"
        );
    }

    #[test]
    fn room_level_helpers() {
        let mut ws = base();
        ws.apply_subscription(Change::Inserted, sub("p", "priv", RoomType::Private));
        ws.apply_subscription(Change::Inserted, sub("d", "bob", RoomType::Direct));
        for (id, roles) in [
            (
                ids::ADD_USER_TO_JOINED_ROOM,
                &["admin", "owner", "moderator"][..],
            ),
            (ids::ADD_USER_TO_ANY_C_ROOM, &["user"]),
            (ids::ADD_USER_TO_ANY_P_ROOM, &["admin"]),
            (ids::REMOVE_USER, &["owner"]),
            (ids::MUTE_USER, &["moderator"]),
            (ids::ARCHIVE_ROOM, &["owner"]),
            (ids::UNARCHIVE_ROOM, &["user"]),
            (ids::CREATE_INVITE_LINKS, &["owner"]),
            (ids::MENTION_ALL, &["owner"]),
            (ids::MENTION_HERE, &["user"]),
            (ids::EDIT_ROOM, &["owner"]),
            (ids::CREATE_P, &["user"]),
            (ids::CREATE_D, &["admin"]),
            (ids::VIEW_ROOM_ADMINISTRATION, &["admin"]),
        ] {
            perm(&mut ws, id, roles);
        }
        assert!(can_add_users(&ws, "r"), "add-user-to-any-c-room");
        assert!(
            !can_add_users(&ws, "p"),
            "groups need add-user-to-any-p-room"
        );
        assert!(!can_add_users(&ws, "d"));
        assert!(!can_remove_users(&ws, "r"));
        assert!(!can_mute_users(&ws, "r"));
        assert!(
            !can_archive_room(&ws, "r"),
            "unarchive-room is not archive-room"
        );
        assert!(!can_create_invite_links(&ws, "r"));
        assert!(!can_mention_all(&ws, "r"));
        assert!(can_mention_here(&ws, "r"));
        assert!(!can_edit_room(&ws, "r"));
        assert!(can_create_group(&ws));
        assert!(!can_create_dm(&ws), "create-d, not create-c");
        assert!(!can_view_room_admin(&ws));
        set_sub(&mut ws, "r", |s| {
            s.roles = Some(vec!["owner".into(), "moderator".into()])
        });
        assert!(can_remove_users(&ws, "r"));
        assert!(can_mute_users(&ws, "r"));
        assert!(can_archive_room(&ws, "r"));
        assert!(can_create_invite_links(&ws, "r"));
        assert!(can_mention_all(&ws, "r"));
        assert!(can_edit_room(&ws, "r"), "room roles count");
        assert!(!can_edit_room(&ws, "p"), "room roles are per room");
    }

    #[test]
    fn upload_and_thread_reply_follow_posting() {
        let mut ws = base();
        let m = mins_ago("m", "x", 0);
        assert!(can_upload(&ws, "r"));
        assert!(can_reply_in_thread(&ws, &m));
        let mut sys = m.clone();
        sys.t = Some("uj".into());
        assert!(!can_reply_in_thread(&ws, &sys));
        setting(&mut ws, "Threads_enabled", json!(false));
        assert!(!can_reply_in_thread(&ws, &m));
        setting(&mut ws, "Threads_enabled", json!(true));
        setting(&mut ws, "FileUpload_Enabled", json!(false));
        assert!(!can_upload(&ws, "r"));
        setting(&mut ws, "FileUpload_Enabled", json!(true));
        set_room(&mut ws, |r| r.ro = Some(true));
        assert!(!can_upload(&ws, "r"));
        assert!(!can_reply_in_thread(&ws, &m));
    }

    #[test]
    fn misc() {
        let mut ws = base();
        assert!(can_create_channel(&ws));
        assert!(!can_create_team(&ws));
        assert!(can_star(&ws, "r"));
        assert!(!can_star(&ws, "nope"));
        assert!(!can_view_admin(&ws));
        perm(&mut ws, ids::VIEW_STATISTICS, &["user"]);
        assert!(can_view_admin(&ws));
    }

    #[test]
    fn fixture_permissions() {
        let ws = crate::test_util::fixture_workspace();
        // alice is a plain user.
        assert!(has_permission(&ws, "access-marketplace", None));
        assert!(!has_permission(&ws, "access-permissions", None));
    }
}
