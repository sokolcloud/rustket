//! Integration screenshots and live checks: the room header menu, the
//! "create discussion" dialog opened from a room, and the preview of a
//! channel the user has not joined (quick switcher → preview with the Join
//! bar → join → sidebar; leaving an unjoined preview forgets it).

use std::path::Path;
use std::time::Duration;

use rocketchat::ServerUrl;
use rustket::app::Modal;
use rustket::message::Message;
use rustket::room;
use rustket_core::ServerId;
use rustket_core::config::Theme;
use serde_json::{Value, json};

use super::{Driver, Ledger, admin};

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

fn in_sidebar(d: &Driver, sid: &ServerId, rid: &str) -> bool {
    d.app.servers[sid]
        .sidebar
        .sections
        .iter()
        .any(|s| s.items.iter().any(|i| i.rid == rid))
}

/// A public channel the snapshot user is not a member of, with a message.
async fn foreign_channel(server: &ServerUrl, ledger: &Ledger, name: &str, text: &str) -> String {
    let admin = admin(server).await;
    let v: Value = admin
        .post("channels.create", &json!({"name": name}))
        .await
        .expect("channels.create");
    let rid = v["channel"]["_id"].as_str().expect("rid").to_owned();
    ledger.lock().expect("ledger").rooms.push(rid.clone());
    let _: Value = admin
        .post("chat.postMessage", &json!({"roomId": rid, "text": text}))
        .await
        .expect("post");
    let _ = admin.logout().await;
    rid
}

/// Opens `name` through the quick switcher (spotlight finds unjoined rooms).
async fn open_via_switcher(d: &mut Driver, sid: &ServerId, name: &str, rid: &str) {
    use rustket::dialogs::quick_switcher::Message as Qs;
    d.send(Message::OpenQuickSwitcher);
    d.send(Message::QuickSwitcher(
        sid.clone(),
        Qs::Query(name.to_owned()),
    ));
    let n = name.to_owned();
    d.pump_until("spotlight result", Duration::from_secs(15), move |a| {
        a.modals.iter().any(|m| match m {
            Modal::QuickSwitcher(_, q) => q.remote.as_ref().is_some_and(|(qq, _)| *qq == n),
            _ => false,
        })
    })
    .await;
    d.send(Message::QuickSwitcher(sid.clone(), Qs::Submit));
    let (s2, r2) = (sid.clone(), rid.to_owned());
    d.pump_until("preview open", Duration::from_secs(20), move |a| {
        a.servers[&s2].active_room.as_deref() == Some(r2.as_str())
            && a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
    })
    .await;
}

pub async fn glue_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    rid: &str,
    ledger: &Ledger,
) {
    use room::header::Message as Hm;
    // Back to the dev channel (other sections may have moved elsewhere).
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.to_owned()),
    ));
    let (s2, r2) = (sid.clone(), rid.to_owned());
    d.pump_until("dev channel", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
    })
    .await;
    d.settle(Duration::from_millis(500)).await;

    // Header menu (with "Create discussion"); a menu entry keeps its panel.
    let m = room_msg(d, sid, rid, room::Message::Header(Hm::ToggleMenu));
    d.send(m);
    both(d, out, "g1-header-menu");
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Header(Hm::MenuPanel(room::panel::Kind::Files)),
    );
    d.send(m);
    let m = room_msg(d, sid, rid, room::Message::Header(Hm::ToggleMenu));
    d.send(m);
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Header(Hm::MenuPanel(room::panel::Kind::Files)),
    );
    d.send(m);
    assert_eq!(
        d.app.servers[sid].rooms[rid].panel.kind(),
        Some(&room::panel::Kind::Files),
        "a menu entry must keep its open panel"
    );
    d.send(room_msg(
        d,
        sid,
        rid,
        room::Message::Panel(room::panel::Message::Close),
    ));

    // Create discussion from the room.
    let m = room_msg(d, sid, rid, room::Message::Header(Hm::CreateDiscussion));
    d.send(m);
    assert!(
        matches!(d.app.modals.last(), Some(Modal::CreateRoom(_, c)) if c.from_room),
        "create discussion dialog expected"
    );
    d.send(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Name("Release checklist".into()),
    ));
    both(d, out, "g2-create-discussion");
    d.send(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Close,
    ));

    // "Start a discussion" from a message: locked parent, excerpt, name
    // prefilled from the message.
    let mid = d.app.servers[sid]
        .workspace
        .timeline(rid)
        .and_then(|t| {
            t.iter()
                .rev()
                .find(|i| {
                    !i.is_pending()
                        && i.message.tmid.is_none()
                        && !i.message.is_system()
                        && !i.message.msg.is_empty()
                })
                .map(|i| i.message.id.clone())
        })
        .expect("a message in the dev channel");
    let m = room_msg(
        d,
        sid,
        rid,
        room::Message::Timeline(room::timeline::Message::Item(room::timeline::ItemMsg::Act(
            mid.clone(),
            room::timeline::Act::StartDiscussion,
        ))),
    );
    d.send(m);
    assert!(
        matches!(d.app.modals.last(), Some(Modal::CreateRoom(_, c))
            if c.pmid.as_ref().is_some_and(|p| p.0 == mid) && !c.name.is_empty()),
        "discussion-from-message dialog expected"
    );
    both(d, out, "g2b-discussion-from-message");
    d.send(Message::CreateRoom(
        sid.clone(),
        rustket::dialogs::create_room::Message::Close,
    ));

    // Preview of a channel the user has not joined.
    let suffix = format!("{:x}", chrono::Utc::now().timestamp_millis() as u32);
    let name = format!("rk-snap-preview-{suffix}");
    let prid = foreign_channel(
        server,
        ledger,
        &name,
        "Welcome to the preview channel. Join to take part!",
    )
    .await;
    open_via_switcher(d, sid, &name, &prid).await;
    {
        let s = &d.app.servers[sid];
        assert!(s.workspace.subscription(&prid).is_none(), "not joined");
        assert!(s.rooms[&prid].is_preview(&s.workspace));
        assert!(
            !in_sidebar(d, sid, &prid),
            "a preview is not in the sidebar"
        );
        let tl = s.workspace.timeline(&prid).expect("preview timeline");
        assert!(
            tl.iter().any(|i| i.message.msg.contains("preview channel")),
            "preview history shown"
        );
    }
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "g3-preview");

    // Join: subscription, sidebar entry, normal composer.
    d.send(room_msg(
        d,
        sid,
        &prid,
        room::Message::Composer(room::composer::Message::Join),
    ));
    let (s2, p2) = (sid.clone(), prid.clone());
    d.pump_until("joined", Duration::from_secs(20), move |a| {
        a.servers[&s2].workspace.subscription(&p2).is_some()
    })
    .await;
    d.send(Message::Noop);
    assert!(in_sidebar(d, sid, &prid), "joined room in the sidebar");
    assert!(!d.app.servers[sid].rooms[&prid].is_preview(&d.app.servers[sid].workspace));
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "g4-joined");

    // An unjoined preview left behind is forgotten (room, timeline, watch).
    let name2 = format!("rk-snap-preview2-{suffix}");
    let prid2 = foreign_channel(server, ledger, &name2, "Another channel.").await;
    open_via_switcher(d, sid, &name2, &prid2).await;
    let (s2, p2) = (sid.clone(), prid2.clone());
    d.pump_until("preview watch", Duration::from_secs(15), move |a| {
        a.servers[&s2].rooms.get(&p2).is_some_and(|r| r.has_watch())
    })
    .await;
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.to_owned()),
    ));
    let s = &d.app.servers[sid];
    assert!(s.workspace.room(&prid2).is_none(), "preview forgotten");
    assert!(s.workspace.timeline(&prid2).is_none());
    assert!(!s.rooms.contains_key(&prid2));
    // The joined one stays.
    assert!(s.workspace.room(&prid).is_some());
    println!("glue checks passed");
}
