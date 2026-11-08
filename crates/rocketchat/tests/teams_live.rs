//! Live teams / discussions / video-conference tests against the dev server
//! (see CLAUDE.md). Skipped unless `RUSTKET_TEST_SERVER` is set. Global state
//! is never changed: throwaway users, teams and rooms are deleted at the end,
//! also when an assertion fails (the panic is re-raised after the cleanup).

#![allow(clippy::unwrap_used)]

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures::FutureExt;

use rocketchat::models::VideoConfAction;
use rocketchat::rest::videoconf::{NO_PROVIDER_APP, provider_error};
use rocketchat::rest::{CreateRoom, CreateTeam, MemberFilter, Page, TeamRef};
use rocketchat::session::{Session, SessionEvent, SessionEvents, SessionOptions};
use rocketchat::{RestClient, ServerUrl, TlsConfig};
use serde_json::{Value, json};

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

fn client(server: &ServerUrl) -> RestClient {
    RestClient::new(server.clone(), &TlsConfig::new().build().unwrap()).unwrap()
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = client(server);
    c.login_password(user, password, None)
        .await
        .unwrap_or_else(|e| panic!("login {user}: {e}"));
    c
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

/// Creates a throwaway user; returns `(id, username, logged-in client)`.
async fn temp_user(
    admin: &RestClient,
    server: &ServerUrl,
    prefix: &str,
) -> (String, String, RestClient) {
    let username = unique(prefix);
    let password = format!("{username}-pw");
    let r: Value = admin
        .post(
            "users.create",
            &json!({
                "username": username, "email": format!("{username}@rustket.local"),
                "password": password, "name": format!("Real {username}"), "verified": true,
                "requirePasswordChange": false,
            }),
        )
        .await
        .unwrap();
    let id = r["user"]["_id"].as_str().unwrap().to_owned();
    let rest = login(server, &username, &password).await;
    (id, username, rest)
}

async fn delete_user(admin: &RestClient, id: &str) {
    let r: Result<Value, _> = admin
        .post(
            "users.delete",
            &json!({ "userId": id, "confirmRelinquish": true }),
        )
        .await;
    if let Err(e) = r {
        eprintln!("cleanup: deleting user {id} failed: {e}");
    }
}

#[tokio::test]
async fn teams_round_trip() {
    let Some(server) = server() else { return };
    let admin = login(&server, "admin", "adminpass").await;
    let (oid, _oname, owner) = temp_user(&admin, &server, "tvl-own").await;
    let (mid, _mname, member) = temp_user(&admin, &server, "tvl-mem").await;
    let mut cleanup_rooms: Vec<String> = Vec::new();

    let result = AssertUnwindSafe(async {
        let team = owner
            .teams_create(&CreateTeam {
                name: unique("tvl-team"),
                members: vec![mid.clone()],
                topic: Some("Topic".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let t = TeamRef::Id(&team.id);
        cleanup_rooms.push(team.room_id.clone());
        // Main room carries the team; the team is listed for both users.
        let info = owner.rooms_info_extended(&team.room_id).await.unwrap();
        assert!(info.room.is_team());
        assert_eq!(owner.teams_info(t).await.unwrap().room_id, team.room_id);
        assert_eq!(
            member
                .teams_list(Page::default())
                .await
                .unwrap()
                .items
                .len(),
            1
        );

        // Rooms: create inside, add an existing one, auto-join, remove.
        let child = owner
            .channels_create(&CreateRoom {
                extra_data: Some(json!({ "teamId": team.id })),
                ..CreateRoom::new(unique("tvl-child"))
            })
            .await
            .unwrap();
        cleanup_rooms.push(child.id.clone());
        let loose = owner
            .groups_create(&CreateRoom::new(unique("tvl-loose")))
            .await
            .unwrap();
        cleanup_rooms.push(loose.id.clone());
        let available = owner
            .rooms_autocomplete_available_for_teams(Some("tvl-loose"))
            .await
            .unwrap();
        assert!(available.iter().any(|r| r.id == loose.id));
        owner.teams_add_rooms(t, &[&loose.id]).await.unwrap();
        let rooms = owner
            .teams_list_rooms(t, None, false, Page::new(0, 10))
            .await
            .unwrap();
        assert_eq!(rooms.total, 2);
        let auto = owner.teams_update_room(&child.id, true).await.unwrap();
        assert_eq!(auto.team_default, Some(true));
        let auto_only = owner
            .teams_list_rooms(t, None, true, Page::new(0, 10))
            .await
            .unwrap();
        assert_eq!(auto_only.items.len(), 1);
        let children = member
            .teams_list_children(t, None, None, Page::new(0, 10))
            .await
            .unwrap();
        assert!(children.items.iter().any(|r| r.id == team.room_id));
        owner.teams_remove_room(t, &loose.id).await.unwrap();

        // Members and roles.
        let members = owner
            .teams_members(t, &MemberFilter::default(), Page::new(0, 10))
            .await
            .unwrap();
        assert_eq!(members.total, 2);
        owner
            .teams_update_member(t, &mid, &["moderator"])
            .await
            .unwrap();
        let members = owner
            .teams_members(t, &MemberFilter::default(), Page::new(0, 10))
            .await
            .unwrap();
        assert!(
            members
                .items
                .iter()
                .any(|m| m.user.id == mid && m.has_role("moderator"))
        );
        owner.teams_update_member(t, &mid, &[]).await.unwrap();

        // The last owner cannot leave; a plain member cannot delete.
        let e = owner.teams_leave(t, &[]).await.unwrap_err();
        assert_eq!(e.api_error_code(), Some("last-owner-can-not-be-removed"));
        let e = member.teams_delete(t, &[]).await.unwrap_err();
        assert_eq!(e.api_error_code(), Some("unauthorized"));
        let rooms_of = member
            .teams_list_rooms_of_user(t, &mid, false, Page::new(0, 10))
            .await
            .unwrap();
        assert!(rooms_of.items.iter().any(|r| r.room.id == child.id));
        member.teams_leave(t, &[&child.id]).await.unwrap();
        assert!(
            member
                .teams_list(Page::default())
                .await
                .unwrap()
                .items
                .is_empty()
        );

        // Discussions of the main room.
        let disc = owner
            .rooms_create_discussion(&team.room_id, "Disc", None, &[])
            .await
            .unwrap();
        let list = owner
            .chat_get_discussions(&team.room_id, None, Page::new(0, 10))
            .await
            .unwrap();
        assert_eq!(list.items[0].drid.as_deref(), Some(disc.id.as_str()));

        // Delete the team together with the child room.
        owner.teams_delete(t, &[&child.id]).await.unwrap();
        assert!(owner.rooms_info(&child.id).await.is_err());
        assert!(owner.rooms_info(&team.room_id).await.is_err());
    })
    .catch_unwind()
    .await;
    for rid in cleanup_rooms {
        let _ = admin.rooms_delete(&rid).await;
    }
    delete_user(&admin, &oid).await;
    delete_user(&admin, &mid).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn convert_channel_to_team_and_back() {
    let Some(server) = server() else { return };
    let admin = login(&server, "admin", "adminpass").await;
    let (uid, _name, rest) = temp_user(&admin, &server, "tvl-conv").await;
    let mut room_id = None;
    let result = AssertUnwindSafe(async {
        let room = rest
            .channels_create(&CreateRoom::new(unique("tvl-conv")))
            .await
            .unwrap();
        room_id = Some(room.id.clone());
        let team = rest.channels_convert_to_team(&room.id).await.unwrap();
        assert_eq!(team.room_id, room.id);
        rest.teams_convert_to_channel(TeamRef::Id(&team.id), &[])
            .await
            .unwrap();
        let after = rest.rooms_info(&room.id).await.unwrap();
        assert!(!after.is_team() && after.team_id.is_none());
    })
    .catch_unwind()
    .await;
    if let Some(rid) = room_id {
        let _ = admin.rooms_delete(&rid).await;
    }
    delete_user(&admin, &uid).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Whether a `VideoConference` signal for `call_id` arrives within `secs`.
async fn signal_seen(events: &mut SessionEvents, call_id: &str, secs: u64) -> bool {
    let fut = async {
        loop {
            match events.recv().await {
                Some(SessionEvent::VideoConference(s)) if s.call_id == call_id => return true,
                Some(_) => {}
                None => return false,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(secs), fut)
        .await
        .unwrap_or(false)
}

#[tokio::test]
async fn video_conference_without_provider() {
    let Some(server) = server() else { return };
    let admin = login(&server, "admin", "adminpass").await;
    let (uid, _name, rest) = temp_user(&admin, &server, "tvl-vc").await;
    let (peer_id, peer_name, peer) = temp_user(&admin, &server, "tvl-vcp").await;
    let mut room_id = None;
    let tls = TlsConfig::new().build().unwrap();
    let result = AssertUnwindSafe(async {
        let room = rest
            .channels_create(&CreateRoom {
                members: vec![peer_name.clone()],
                ..CreateRoom::new(unique("tvl-vc"))
            })
            .await
            .unwrap();
        room_id = Some(room.id.clone());
        assert!(rest.videoconf_providers().await.unwrap().is_empty());
        let e = rest.videoconf_capabilities().await.unwrap_err();
        assert_eq!(provider_error(&e), Some(NO_PROVIDER_APP));
        assert!(rest.videoconf_joinable().await.unwrap().is_empty());
        assert_eq!(
            rest.videoconf_list(&room.id, Page::default())
                .await
                .unwrap()
                .total,
            0
        );

        // `start` fails the same way and pushes an ephemeral explanation.
        let (session, mut events) = Session::start(
            server.clone(),
            &tls,
            rest.credentials().unwrap(),
            SessionOptions::default(),
        )
        .await
        .unwrap();
        let e = rest
            .videoconf_start(&room.id, None, false)
            .await
            .unwrap_err();
        assert_eq!(provider_error(&e), Some(NO_PROVIDER_APP));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut ephemeral = false;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.recv()).await {
            if let SessionEvent::EphemeralMessage(m) = ev
                && m.rid == room.id
            {
                ephemeral = true;
                break;
            }
        }
        assert!(ephemeral, "no ephemeral provider message");

        // Unknown calls: the documented error shapes.
        let e = rest.videoconf_info("nope").await.unwrap_err();
        assert_eq!(e.api_error_code(), Some("invalid-params"));
        let e = rest.videoconf_join("nope", true, false).await.unwrap_err();
        assert_eq!(e.api_error_code(), Some("invalid-params"));
        let e = rest.videoconf_decline("nope").await.unwrap_err();
        assert!(matches!(e, rocketchat::Error::Api { status: 404, .. }));

        // The publisher never receives its own events, so the peer listens.
        let (peer_session, mut peer_events) = Session::start(
            server.clone(),
            &tls,
            peer.credentials().unwrap(),
            SessionOptions::default(),
        )
        .await
        .unwrap();
        // Positive control: the unvalidated room relay reaches the peer.
        let probe = json!({
            "action": "call",
            "params": { "callId": "relay-probe", "uid": uid, "rid": room.id },
        });
        let mut seen = false;
        for _ in 0..5 {
            session
                .call(
                    "stream-notify-room-users",
                    vec![
                        json!(format!("{}/video-conference", room.id)),
                        probe.clone(),
                    ],
                )
                .await
                .unwrap();
            if signal_seen(&mut peer_events, "relay-probe", 3).await {
                seen = true;
                break;
            }
        }
        assert!(seen, "the peer never saw the relayed probe");
        // A validated signal about a call that does not exist is not relayed.
        session
            .notify_video_conference(&peer_id, VideoConfAction::Call, "fake-call", &room.id)
            .await
            .unwrap();
        assert!(
            !signal_seen(&mut peer_events, "fake-call", 3).await,
            "fake call was relayed"
        );

        peer_session.close().await;
        session.close().await;
    })
    .catch_unwind()
    .await;
    if let Some(rid) = room_id {
        let _ = admin.rooms_delete(&rid).await;
    }
    delete_user(&admin, &uid).await;
    delete_user(&admin, &peer_id).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
