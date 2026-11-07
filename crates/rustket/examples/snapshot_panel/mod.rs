//! Header and right-panel screenshots for `snapshot.rs`: adds live data to the
//! throwaway dev channel (owner role, announcement, thread, pin, star, files,
//! discussion, a team) and renders every panel in light and dark.

use super::*;

use rocketchat::rest::UploadRequest;
use rustket::room::{self, header, panel};
use rustket_core::ServerId;

fn room_msg(d: &mut Driver, sid: &ServerId, rid: &str, m: room::Message) {
    let epoch = d.app.servers[sid].generation;
    d.send(Message::Room(sid.clone(), epoch, rid.to_owned(), m));
}

fn header_msg(d: &mut Driver, sid: &ServerId, rid: &str, m: header::Message) {
    room_msg(d, sid, rid, room::Message::Header(m));
}

fn panel_body(d: &mut Driver, sid: &ServerId, rid: &str, m: panel::BodyMsg) {
    let g = d.app.servers[sid].rooms[rid].panel.generation();
    room_msg(
        d,
        sid,
        rid,
        room::Message::Panel(panel::Message::Body(g, m)),
    );
}

/// Shows `kind` (closing whatever was open) and lets it load.
async fn open(d: &mut Driver, sid: &ServerId, rid: &str, kind: panel::Kind) {
    if d.app.servers[sid].rooms[rid].panel.kind() == Some(&kind) {
        return;
    }
    header_msg(d, sid, rid, header::Message::TogglePanel(kind));
    d.settle(Duration::from_millis(1500)).await;
}

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
}

async fn login_as(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = rest(server).await;
    c.login_password(user, password, None).await.expect("login");
    c
}

async fn post(c: &RestClient, body: Value) -> String {
    let v: Value = c.post("chat.postMessage", &body).await.expect("post");
    v["message"]["_id"].as_str().expect("message id").to_owned()
}

/// Live data for the panels; everything lives in throwaway rooms or is
/// recorded in the ledger.
async fn seed(
    server: &ServerUrl,
    rid: &str,
    user: &(String, String, String),
    ledger: &Ledger,
) -> String {
    let admin = admin(server).await;
    let peer_name = user.1.replace("rk-snap-", "rk-peer-");
    let me = login_as(server, &user.1, &user.2).await;
    let peer = login_as(server, &peer_name, &user.2).await;

    admin
        .post_ok(
            "channels.addOwner",
            &json!({"roomId": rid, "userId": user.0}),
        )
        .await
        .expect("addOwner");
    admin
        .post_ok(
            "rooms.saveRoomSettings",
            &json!({
                "rid": rid,
                "roomAnnouncement": "Release **0.2** is frozen: fixes only until Friday.",
                "roomDescription": "Coordination of the *Rustket* releases. Checklists live in https://rocket.chat",
            }),
        )
        .await
        .expect("saveRoomSettings");

    let parent = post(
        &peer,
        json!({"roomId": rid, "text": "Release checklist for 0.2: who takes the changelog?"}),
    )
    .await;
    post(
        &me,
        json!({"roomId": rid, "tmid": parent, "text": "I will write it tonight."}),
    )
    .await;
    post(
        &peer,
        json!({"roomId": rid, "tmid": parent, "text": "Thanks! Please mention the new panels."}),
    )
    .await;
    let other = post(
        &peer,
        json!({"roomId": rid, "text": "Old thread: CI cache keeps growing"}),
    )
    .await;
    post(
        &me,
        json!({"roomId": rid, "tmid": other, "text": "Fixed by pruning weekly."}),
    )
    .await;
    let staging = post(
        &peer,
        json!({"roomId": rid, "text": "Staging is up at https://staging.example.com"}),
    )
    .await;
    me.post_ok("chat.pinMessage", &json!({"messageId": staging}))
        .await
        .expect("pin");
    me.post_ok("chat.starMessage", &json!({"messageId": parent}))
        .await
        .expect("star");
    post(&peer, json!({"roomId": rid, "text": format!("@{} the release notes need a final review", user.1)})).await;
    for (name, mime, body) in [
        (
            "release-notes.txt",
            "text/plain",
            "Rustket 0.2\n- panels\n".as_bytes().to_vec(),
        ),
        ("build.log", "text/plain", vec![b'x'; 48 * 1024]),
    ] {
        peer.upload_file(rid, UploadRequest::new(name, mime, body))
            .await
            .expect("upload");
    }
    let disc: Value = peer
        .post(
            "rooms.createDiscussion",
            &json!({"prid": rid, "t_name": "Hotfix 0.1.1", "users": [user.1]}),
        )
        .await
        .expect("discussion");
    let drid = disc["discussion"]["_id"].as_str().expect("drid").to_owned();
    ledger.lock().expect("ledger").rooms.push(drid.clone());
    post(
        &peer,
        json!({"roomId": drid, "text": "Cherry-picking the crash fix here."}),
    )
    .await;

    let sfx = user.1.trim_start_matches("rk-snap-").to_owned();
    let team: Value = admin
        .post(
            "teams.create",
            &json!({"name": format!("rk-snap-team-{sfx}"), "type": 0, "members": [user.0]}),
        )
        .await
        .expect("teams.create");
    let team_id = team["team"]["_id"].as_str().expect("team id").to_owned();
    ledger.lock().expect("ledger").teams.push(team_id.clone());
    for name in [format!("rk-snap-tch-{sfx}"), format!("rk-snap-ops-{sfx}")] {
        let v: Value = admin
            .post(
                "channels.create",
                &json!({"name": name, "members": [user.1], "extraData": {"teamId": team_id}}),
            )
            .await
            .expect("team channel");
        let id = v["channel"]["_id"].as_str().expect("rid").to_owned();
        ledger.lock().expect("ledger").rooms.push(id);
    }
    let _ = me.logout().await;
    let _ = peer.logout().await;
    let _ = admin.logout().await;
    peer_name
}

fn find_room(
    d: &Driver,
    sid: &ServerId,
    f: impl Fn(&rustket_core::workspace::RoomEntry) -> bool,
) -> Option<String> {
    d.app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| f(e))
        .map(|e| e.rid().to_owned())
}

async fn open_room(d: &mut Driver, sid: &ServerId, rid: &str) {
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open(rid.to_owned()),
    ));
    let (s, r) = (sid.clone(), rid.to_owned());
    d.pump_until("room open", Duration::from_secs(20), move |a| {
        a.servers[&s].rooms.get(&r).is_some_and(|v| v.loaded)
    })
    .await;
    d.settle(Duration::from_millis(800)).await;
}

pub(super) async fn panel_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    rid: &str,
    user: &(String, String, String),
    ledger: &Ledger,
) {
    let peer = seed(server, rid, user, ledger).await;
    {
        let (s, r) = (sid.clone(), rid.to_owned());
        d.pump_until("owner role and team", Duration::from_secs(20), move |a| {
            let ws = &a.servers[&s].workspace;
            ws.subscription(&r)
                .and_then(|x| x.roles.as_ref())
                .is_some_and(|roles| roles.iter().any(|x| x == "owner"))
                && ws.rooms().values().any(|e| e.is_team())
                && ws.rooms().values().any(|e| e.is_discussion())
        })
        .await;
    }
    d.settle(Duration::from_secs(1)).await;

    // Header and its menu.
    both(d, out, "p00-header");
    header_msg(d, sid, rid, header::Message::ToggleMenu);
    both(d, out, "p01-header-menu");
    header_msg(d, sid, rid, header::Message::CloseMenu);

    let shots: Vec<(&str, panel::Kind)> = vec![
        ("p02-info", panel::Kind::Info),
        ("p03-members", panel::Kind::Members),
        ("p04-threads", panel::Kind::Threads),
        ("p06-discussions", panel::Kind::Discussions),
        ("p07-pinned", panel::Kind::Pinned),
        ("p08-starred", panel::Kind::Starred),
        ("p09-mentions", panel::Kind::Mentions),
        ("p10-files", panel::Kind::Files),
        ("p12-notifications", panel::Kind::NotificationPrefs),
        ("p13-room-settings", panel::Kind::RoomSettings),
        ("p14-user-info", panel::Kind::UserInfo(peer.clone())),
    ];
    for (name, kind) in shots {
        open(d, sid, rid, kind).await;
        both(d, out, name);
    }

    // Members: all (the throwaway users are offline for the server).
    open(d, sid, rid, panel::Kind::Members).await;
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Members(panel::members::Msg::Filter(panel::members::Filter::All)),
    );
    d.settle(Duration::from_millis(1200)).await;
    both(d, out, "p03b-members-all");
    // A profile from the list and Back: the "All" filter and query survive.
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Members(panel::members::Msg::Query("rk-".into())),
    );
    d.settle(Duration::from_millis(1200)).await;
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Members(panel::members::Msg::Open(peer.clone())),
    );
    d.settle(Duration::from_millis(1500)).await;
    room_msg(d, sid, rid, room::Message::Panel(panel::Message::Back));
    d.settle(Duration::from_millis(300)).await;
    both(d, out, "p03c-members-after-back");

    // A built-in sound selected in the notification preferences.
    open(d, sid, rid, panel::Kind::NotificationPrefs).await;
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Notify(panel::notify::Msg::Sound("chime".into())),
    );
    d.settle(Duration::from_millis(200)).await;
    both(d, out, "p12b-notifications-chime");

    // Thread placeholder of the first thread parent.
    let tmid = d.app.servers[sid].workspace.timeline(rid).and_then(|t| {
        t.messages()
            .find(|m| m.tcount.unwrap_or(0) > 1)
            .map(|m| m.id.clone())
    });
    if let Some(tmid) = tmid {
        open(d, sid, rid, panel::Kind::Thread(tmid)).await;
        both(d, out, "p05-thread");
    }

    // Search with a query (debounced).
    open(d, sid, rid, panel::Kind::Search).await;
    both(d, out, "p11a-search-empty");
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Messages(panel::messages::Msg::Query("release".into())),
    );
    d.settle(Duration::from_millis(1500)).await;
    both(d, out, "p11-search");

    // Invite with an autocomplete query.
    open(d, sid, rid, panel::Kind::Invite).await;
    panel_body(
        d,
        sid,
        rid,
        panel::BodyMsg::Invite(panel::invite::Msg::Query("rk-".into())),
    );
    d.settle(Duration::from_millis(1500)).await;
    both(d, out, "p15-invite");

    // Leave confirmation from the menu.
    header_msg(
        d,
        sid,
        rid,
        header::Message::TogglePanel(panel::Kind::Invite),
    );
    header_msg(
        d,
        sid,
        rid,
        header::Message::Room(room::panel::common::RoomAction::Leave),
    );
    both(d, out, "p16-leave-confirm");
    header_msg(d, sid, rid, header::Message::Cancel);

    // Team main room: badge and team channels.
    if let Some(team) = find_room(d, sid, |e| e.is_team()) {
        open_room(d, sid, &team).await;
        open(d, sid, &team, panel::Kind::TeamChannels).await;
        both(d, out, "p17-team-channels");
    }
    // Discussion: parent link in the header.
    if let Some(disc) = find_room(d, sid, |e| e.is_discussion()) {
        open_room(d, sid, &disc).await;
        open(d, sid, &disc, panel::Kind::Info).await;
        both(d, out, "p18-discussion-info");
    }
    // DM: presence in the header, Info opens the profile.
    let peer2 = peer.clone();
    if let Some(dm) = find_room(d, sid, move |e| {
        e.room_type() == rocketchat::models::RoomType::Direct && e.name() == peer2
    }) {
        open_room(d, sid, &dm).await;
        open(d, sid, &dm, panel::Kind::UserInfo(peer.clone())).await;
        both(d, out, "p19-dm-user-info");
        header_msg(d, sid, &dm, header::Message::ToggleMenu);
        both(d, out, "p19b-dm-menu");
        header_msg(d, sid, &dm, header::Message::CloseMenu);
    }
    // Back to the dev channel for the rest of the example.
    open_room(d, sid, rid).await;
}
