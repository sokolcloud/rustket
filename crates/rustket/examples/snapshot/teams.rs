//! Teams and video-conference shots (light + dark): a team main room (header
//! with the call button, sidebar Teams group), a child room with "Back to
//! team", Team channels / Team members panels, the leave and delete dialogs,
//! synthetic `video_conf` blocks, the incoming call popup and the
//! no-provider dialog (the dev server has no provider app).
//!
//! `RUSTKET_SNAPSHOT_ONLY=teams` skips the other component shots.

use std::path::Path;
use std::time::Duration;

use rocketchat::ServerUrl;
use rocketchat::models::{
    Message as RcMessage, UserRef, VideoConfKind, VideoConfStatus, VideoConfUser, VideoConference,
};
use rustket::message::Message;
use rustket::room::{self, header, panel};
use rustket::teams::{self as tm, chooser::Flow};
use rustket::videoconf::{self as vc, CallAction};
use rustket_core::ServerId;
use rustket_core::config::Theme;
use serde_json::{Value, json};

use super::{Driver, Ledger, admin};

/// `RUSTKET_SNAPSHOT_ONLY=teams`.
pub fn only() -> bool {
    std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() == Ok("teams")
}

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
}

fn room_msg(d: &Driver, sid: &ServerId, rid: &str, m: room::Message) -> Message {
    Message::Room(
        sid.clone(),
        d.app.servers[sid].generation,
        rid.to_owned(),
        m,
    )
}

async fn open_room(d: &mut Driver, sid: &ServerId, rid: &str) {
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.to_owned()),
    ));
    let (s2, r2) = (sid.clone(), rid.to_owned());
    d.pump_until("team room open", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
    })
    .await;
    d.settle(Duration::from_millis(800)).await;
}

async fn open_panel(d: &mut Driver, sid: &ServerId, rid: &str, kind: panel::Kind) {
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Header(header::Message::TogglePanel(kind)),
    );
    d.send(m);
    d.settle(Duration::from_secs(2)).await;
}

fn team_msg(d: &Driver, sid: &ServerId, rid: &str, m: tm::Msg) -> Message {
    let g = d.app.servers[sid].rooms[rid].panel.generation();
    room_msg(
        d,
        sid,
        rid,
        room::Message::Panel(panel::Message::Body(g, panel::BodyMsg::Team(m))),
    )
}

/// The team, owned by the snapshot user, with two child rooms (one with
/// auto-join) and a peer member. Returns `(team id, main rid, child rid)`.
async fn setup(server: &ServerUrl, ledger: &Ledger, uid: &str) -> (String, String, String) {
    let admin = admin(server).await;
    let suffix = format!("{:x}", chrono::Utc::now().timestamp_millis() as u32);
    let v: Value = admin
        .post(
            "teams.create",
            &json!({"name": format!("rk-snap-team-{suffix}"), "type": 0, "owner": uid,
                    "room": {"extraData": {"topic": "Release crew"}}}),
        )
        .await
        .expect("teams.create");
    let team = v["team"]["_id"].as_str().expect("team").to_owned();
    let main = v["team"]["roomId"].as_str().expect("room").to_owned();
    ledger.lock().expect("ledger").teams.push(team.clone());
    let mut child = String::new();
    for (name, members, auto) in [
        (format!("rk-snap-ops-{suffix}"), json!([uid]), true),
        (format!("rk-snap-design-{suffix}"), json!([]), false),
    ] {
        let v: Value = admin
            .post(
                "channels.create",
                &json!({"name": name, "members": members,
                        "extraData": {"teamId": team, "topic": "Deploys and on-call"}}),
            )
            .await
            .expect("channels.create in team");
        let rid = v["channel"]["_id"].as_str().expect("rid").to_owned();
        ledger.lock().expect("ledger").rooms.push(rid.clone());
        if auto {
            let _: Value = admin
                .post(
                    "teams.updateRoom",
                    &json!({"roomId": rid, "isDefault": true}),
                )
                .await
                .expect("autojoin");
            child = rid;
        }
    }
    let _: Value = admin
        .post(
            "teams.addMembers",
            &json!({"teamId": team, "members": [{"userId": admin.user_id().unwrap_or_default(), "roles": ["moderator"]}]}),
        )
        .await
        .expect("teams.addMembers");
    let _ = admin.logout().await;
    (team, main, child)
}

fn synthetic_call(
    id: &str,
    rid: &str,
    kind: VideoConfKind,
    status: VideoConfStatus,
    creator: (&str, &str),
    joined: &[(&str, &str)],
    ended: bool,
) -> VideoConference {
    VideoConference {
        id: id.into(),
        kind,
        rid: rid.into(),
        status,
        users: joined
            .iter()
            .map(|(u, n)| VideoConfUser {
                id: format!("id-{u}"),
                username: Some((*u).into()),
                name: Some((*n).into()),
                ..Default::default()
            })
            .collect(),
        created_by: Some(UserRef {
            id: creator.0.into(),
            username: Some(creator.1.into()),
            name: Some(creator.1.into()),
        }),
        provider_name: "jitsi".into(),
        title: Some("Release sync".into()),
        url: Some("https://meet.example.org/rk-release".into()),
        ended_at: ended
            .then(|| rocketchat::RcDate::from_millis(1_794_000_000_000))
            .flatten(),
        capabilities: Some(rocketchat::models::VideoConfCapabilities {
            mic: Some(true),
            cam: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn videoconf_message(id: &str, rid: &str, call: &str, user: (&str, &str), ms: i64) -> RcMessage {
    serde_json::from_value(json!({
        "_id": id, "rid": rid, "t": "videoconf", "msg": "Conference Call", "groupable": false,
        "u": {"_id": user.0, "username": user.1},
        "ts": {"$date": ms}, "_updatedAt": {"$date": ms},
        "blocks": [{"type": "video_conf", "callId": call, "blockId": call, "appId": "videoconf-core"}]
    }))
    .expect("synthetic videoconf message")
}

pub async fn teams_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
    ledger: &Ledger,
) {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").is_ok_and(|v| v != "teams") {
        return;
    }
    let (uid, username) = (user.0.clone(), user.1.clone());
    let (_team, main, child) = setup(server, ledger, &uid).await;
    let (s2, m2, c2) = (sid.clone(), main.clone(), child.clone());
    d.pump_until(
        "team rooms in the workspace",
        Duration::from_secs(30),
        move |a| {
            let ws = &a.servers[&s2].workspace;
            ws.room(&m2).is_some_and(|e| e.is_team() && e.sub.is_some())
                && ws
                    .room(&c2)
                    .is_some_and(|e| e.room.as_ref().is_some_and(|r| r.team_id.is_some()))
        },
    )
    .await;

    // Team main room: header (team chip, call button), sidebar Teams group.
    open_room(d, sid, &main).await;
    both(d, out, "teams-01-main-room");

    // Team channels and members panels.
    open_panel(d, sid, &main, panel::Kind::TeamChannels).await;
    both(d, out, "teams-02-channels-panel");
    open_panel(d, sid, &main, panel::Kind::TeamMembers).await;
    both(d, out, "teams-03-members-panel");

    // Team info with the leave and delete dialogs.
    open_panel(d, sid, &main, panel::Kind::TeamInfo).await;
    both(d, out, "teams-04-info-panel");
    for (flow, name) in [
        (Flow::Leave, "teams-05-leave"),
        (Flow::Delete, "teams-06-delete"),
    ] {
        let m = team_msg(d, sid, &main, tm::Msg::Info(tm::info::Msg::Start(flow)));
        d.send(m);
        d.settle(Duration::from_secs(2)).await;
        both(d, out, name);
        let m = team_msg(
            d,
            sid,
            &main,
            tm::Msg::Info(tm::info::Msg::Chooser(tm::chooser::Msg::Next)),
        );
        d.send(m);
        both(d, out, &format!("{name}-confirm"));
        let m = team_msg(
            d,
            sid,
            &main,
            tm::Msg::Info(tm::info::Msg::Chooser(tm::chooser::Msg::Cancel)),
        );
        d.send(m);
        let m = team_msg(
            d,
            sid,
            &main,
            tm::Msg::Info(tm::info::Msg::Chooser(tm::chooser::Msg::Cancel)),
        );
        d.send(m);
    }
    let m = room_msg(d, sid, &main, room::Message::Panel(panel::Message::Close));
    d.send(m);

    // Synthetic `video_conf` blocks (no provider on the dev server): an
    // ongoing group call and an ended unanswered direct call.
    {
        let s = d.app.servers.get_mut(sid).expect("server");
        let now = chrono::Utc::now().timestamp_millis();
        let ongoing = synthetic_call(
            "rk-snap-call-1",
            &main,
            VideoConfKind::Group,
            VideoConfStatus::Started,
            ("id-admin", "admin"),
            &[("admin", "Administrator"), (&username, &username)],
            false,
        );
        let ended = synthetic_call(
            "rk-snap-call-2",
            &main,
            VideoConfKind::Direct,
            VideoConfStatus::Declined,
            (&uid, &username),
            &[],
            true,
        );
        s.workspace.videoconf_mut().finish(
            "rk-snap-call-1",
            Ok(ongoing),
            std::time::Instant::now(),
        );
        s.workspace
            .videoconf_mut()
            .finish("rk-snap-call-2", Ok(ended), std::time::Instant::now());
        for (i, call) in ["rk-snap-call-1", "rk-snap-call-2"].iter().enumerate() {
            s.workspace.apply_message(videoconf_message(
                &format!("rk-snap-vc-{i}"),
                &main,
                call,
                ("id-admin", "admin"),
                now + i as i64,
            ));
        }
    }
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "teams-07-videoconf-blocks");

    // Incoming direct call popup (a verified synthetic call).
    let incoming = synthetic_call(
        "rk-snap-call-3",
        &main,
        VideoConfKind::Direct,
        VideoConfStatus::Calling,
        ("id-peer", "Release bot"),
        &[],
        false,
    );
    d.app.calls.inject_incoming(sid, &uid, &incoming);
    both(d, out, "teams-08-incoming-call");
    d.send(Message::Calls(vc::Message::Mute(
        sid.clone(),
        "rk-snap-call-3".into(),
    )));

    // The header "Video call" button on a server without a provider.
    d.send(Message::Calls(vc::Message::Action(
        sid.clone(),
        CallAction::Start(main.clone()),
    )));
    d.pump_until("no-provider dialog", Duration::from_secs(15), |a| {
        matches!(&a.calls.dialog, Some((_, vc::Dialog::NoProvider { .. })))
    })
    .await;
    both(d, out, "teams-09-no-provider");
    d.send(Message::Calls(vc::Message::CloseDialog));

    // Child room: "Back to team" in the header.
    open_room(d, sid, &child).await;
    both(d, out, "teams-10-child-room");
}
