//! Team, discussion and video-conference payloads captured from the 8.9 dev
//! server (`fixtures/teams/`), and the strict `{uid}/video-conference` decoding.
#![allow(clippy::unwrap_used)]

use rocketchat::models::{
    Message, ProviderCapabilities, Room, RoomInfoExtended, Team, TeamListItem, TeamMember,
    TeamRoomOfUser, TeamType, UserTeam, VideoConfAction, VideoConfKind, VideoConfStatus,
    VideoConference,
};
use rocketchat::rest::videoconf::{NO_PROVIDER_APP, provider_error};
use rocketchat::session::{DataKind, DataMessage, SessionEvent, decode_event};
use rocketchat::{Error, models::lenient};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/teams/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn list<T: serde::de::DeserializeOwned>(name: &str, key: &str) -> Vec<T> {
    lenient::vec_from_value(fixture(name)[key].clone())
}

#[test]
fn team_documents() {
    let t: Team = serde_json::from_value(fixture("teams_create")["team"].clone()).unwrap();
    assert_eq!(t.kind, TeamType::Public);
    assert!(!t.room_id.is_empty());
    assert!(t.created_by.is_some() && t.created_at.is_some());
    let info: Team = serde_json::from_value(fixture("teams_info")["teamInfo"].clone()).unwrap();
    assert_eq!(info.id, t.id);
    let teams: Vec<TeamListItem> = list("teams_list", "teams");
    assert_eq!(teams[0].rooms, Some(2));
    assert_eq!(teams[0].number_of_users, Some(2));
    assert_eq!(teams[0].team.id, t.id);
    let mine: Vec<UserTeam> = list("users_list_teams", "teams");
    assert!(mine[0].is_owner);
}

#[test]
fn team_rooms() {
    let rooms: Vec<Room> = list("teams_list_rooms", "rooms");
    assert_eq!(rooms.len(), 2);
    assert!(rooms.iter().all(|r| r.team_id.is_some() && !r.is_team()));
    let children: Vec<Room> = list("teams_list_children", "data");
    assert!(children[0].is_team(), "the main room comes first");
    let of_user: Vec<TeamRoomOfUser> = list("teams_list_rooms_of_user", "rooms");
    assert!(
        of_user
            .iter()
            .all(|r| r.is_last_owner && !r.room.id.is_empty())
    );
    let updated: Room =
        serde_json::from_value(fixture("teams_update_room")["room"].clone()).unwrap();
    assert_eq!(updated.team_default, Some(true));
    let removed: Room =
        serde_json::from_value(fixture("teams_remove_room")["room"].clone()).unwrap();
    assert!(removed.team_id.is_none());
    let available: Vec<Room> = list("rooms_available_for_teams", "items");
    assert_eq!(available[0].t, rocketchat::models::RoomType::Private);
    let info: RoomInfoExtended = serde_json::from_value(fixture("rooms_info_team_child")).unwrap();
    let team = info.team.unwrap();
    let parent = info.parent.unwrap();
    assert_eq!(team.room_id, parent.id);
    assert_eq!(info.room.team_id.as_deref(), Some(team.id.as_str()));
}

#[test]
fn team_members() {
    let members: Vec<TeamMember> = list("teams_members", "members");
    assert_eq!(members.len(), 2);
    assert!(members[0].roles.is_empty(), "roles absent → empty");
    assert!(members[1].has_role("owner"));
    assert!(members[1].user.username.is_some() && members[1].created_by.is_some());
}

#[test]
fn discussions_list() {
    let msgs: Vec<Message> = list("chat_get_discussions", "messages");
    assert_eq!(msgs[0].t.as_deref(), Some("discussion-created"));
    assert!(msgs[0].drid.is_some());
}

#[test]
fn error_shapes() {
    let e = |name: &str, status: u16| {
        Error::from_response(status, None, fixture(name).to_string().as_bytes())
    };
    assert_eq!(
        e("error_last_owner", 400).api_error_code(),
        Some("last-owner-can-not-be-removed")
    );
    assert_eq!(
        e("error_empty_rooms", 400).api_error_code(),
        Some("invalid-params")
    );
    let no_provider = e("vc_no_provider", 400);
    assert_eq!(provider_error(&no_provider), Some(NO_PROVIDER_APP));
    assert_eq!(provider_error(&e("vc_invalid_params", 400)), None);
    assert_eq!(provider_error(&e("error_unauthorized", 403)), None);
}

#[test]
fn videoconf_documents() {
    let providers = fixture("vc_providers");
    assert_eq!(providers["data"], json!([]));
    assert_eq!(fixture("vc_list")["total"], 0);
    assert_eq!(fixture("vc_joinable")["calls"], json!([]));
    // Synthetic `video-conference.info` (no provider on the dev server),
    // following IVideoConference.ts.
    let v: VideoConference = serde_json::from_value(json!({
        "_id": "call1", "type": "direct", "rid": "r1", "status": 0,
        "users": [{"_id": "u1", "username": "alice", "name": "Alice", "avatarETag": null,
                   "ts": "2026-11-08T00:00:00.000Z", "joined": false}],
        "messages": {"started": "m1"}, "url": "https://meet.example/x",
        "createdBy": {"_id": "u1", "username": "alice", "name": "Alice"},
        "createdAt": "2026-11-08T00:00:00.000Z", "providerName": "jitsi",
        "capabilities": {"mic": true, "cam": true, "title": false},
        "chatAccess": {"rid": "r1", "membersWithoutAccess": [], "canInvite": false},
        "providerData": {"secret": 1}, "success": true
    }))
    .unwrap();
    assert_eq!(v.kind, VideoConfKind::Direct);
    assert_eq!(v.status, VideoConfStatus::Calling);
    assert_eq!(v.joined_users().count(), 0);
    assert_eq!(v.capabilities.unwrap().mic, Some(true));
    let caps: ProviderCapabilities = serde_json::from_value(
        json!({"providerName": "jitsi", "capabilities": {"mic": true, "cam": false}}),
    )
    .unwrap();
    assert_eq!(caps.capabilities.cam, Some(false));
}

fn notify_user(args: Value) -> DataMessage {
    DataMessage {
        kind: DataKind::Changed,
        collection: "stream-notify-user".into(),
        id: "id".into(),
        fields: json!({"eventName": "me/video-conference", "args": args})
            .as_object()
            .cloned(),
        cleared: Vec::new(),
        before: None,
    }
}

/// Anything a co-member relays through `stream-notify-room-users` arrives
/// verbatim (§7.5): only well-formed signals become `VideoConference`.
#[test]
fn video_conference_signals_are_validated() {
    let ok = decode_event(&notify_user(
        json!([{"action": "accepted", "params": {"callId": "c", "uid": "u", "rid": "r"}}]),
    ));
    assert!(matches!(
        ok,
        SessionEvent::VideoConference(ref s) if s.action == VideoConfAction::Accepted && s.rid == "r"
    ));
    for spoof in [
        json!([{"anything": 1}]),
        json!([{"action": "call", "params": {"callId": "c", "uid": "u"}}]),
        json!([{"action": "call", "params": "c"}]),
        json!([]),
    ] {
        assert!(
            matches!(
                decode_event(&notify_user(spoof.clone())),
                SessionEvent::Unknown { .. }
            ),
            "{spoof}"
        );
    }
}
