//! Omnichannel shots and live checks (`RUSTKET_SNAPSHOT_ONLY` unset, `glue`
//! (the integration shots) or `omni`):
//! the snapshot user becomes a livechat agent (and manager, for "Forward"),
//! a throwaway visitor opens a chat that the user serves; then the sidebar
//! groups, the open chat with its quick actions, the info and contact panels,
//! the availability toggle, the close and forward dialogs and the contact
//! center are shot (light + dark), the availability is toggled and the chat
//! is closed through the dialog. The visitor, its contact and the chat are
//! deleted at the end, also after a failure.

use std::path::Path;
use std::time::Duration;

use iced::futures::FutureExt;
use rocketchat::models::{AgentStatus, CloseChat, ForwardChat, ForwardTarget};
use rocketchat::{RestClient, ServerUrl};
use rustket::message::Message;
use rustket::omnichannel::dialogs::{self, close_chat};
use rustket::omnichannel::panel::PanelKind;
use rustket::omnichannel::room::{Msg as OmniMsg, Request};
use rustket::room;
use rustket_core::ServerId;
use rustket_core::config::Theme;
use rustket_core::omnichannel::room::QuickAction;
use serde_json::{Value, json};

use super::{Driver, admin, rest};

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

fn header(d: &mut Driver, sid: &ServerId, rid: &str, m: room::header::Message) {
    let m = room_msg(d, sid, rid, room::Message::Header(m));
    d.send(m);
}

fn dialog(d: &mut Driver, sid: &ServerId, rid: &str, m: dialogs::Msg) {
    let m = room_msg(d, sid, rid, room::Message::Omni(OmniMsg::Dialog(m)));
    d.send(m);
}

#[derive(Default)]
struct Created {
    agent: Option<String>,
    manager: Option<String>,
    token: Option<String>,
    rid: Option<String>,
}

async fn cleanup(server: &ServerUrl, c: &Created) {
    let admin = admin(server).await;
    if let Some(rid) = &c.rid {
        let _ = admin
            .livechat_close_chat(&CloseChat::new(rid, Some("snapshot cleanup".into())))
            .await;
        match admin
            .post_ok("livechat/rooms.delete", &json!({ "roomId": rid }))
            .await
        {
            Ok(()) => println!("deleted livechat room {rid}"),
            Err(e) => eprintln!("livechat/rooms.delete {rid}: {e}"),
        }
    }
    if let Some(token) = &c.token {
        let r: Result<Value, _> = admin
            .delete(&format!("livechat/visitor/{token}"), &Default::default())
            .await;
        match r {
            Ok(_) => println!("deleted visitor"),
            Err(e) => eprintln!("deleting visitor: {e}"),
        }
    }
    for (kind, id) in [("agent", &c.agent), ("manager", &c.manager)] {
        if let Some(id) = id {
            let r: Result<Value, _> = admin
                .delete(&format!("livechat/users/{kind}/{id}"), &Default::default())
                .await;
            if let Err(e) = r {
                eprintln!("removing livechat {kind}: {e}");
            }
        }
    }
    let _ = admin.logout().await;
}

pub async fn omni_shots(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
) {
    match std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() {
        Err(_) | Ok("omni" | "glue") => {}
        Ok(_) => return,
    }
    let mut created = Created::default();
    let r = std::panic::AssertUnwindSafe(scenario(d, server, out, sid, user, &mut created))
        .catch_unwind()
        .await;
    cleanup(server, &created).await;
    if let Err(p) = r {
        std::panic::resume_unwind(p);
    }
}

async fn scenario(
    d: &mut Driver,
    server: &ServerUrl,
    out: &Path,
    sid: &ServerId,
    user: &(String, String, String),
    created: &mut Created,
) {
    let (uid, username, password) = user;
    let admin = admin(server).await;
    // The snapshot user becomes an agent (available right away) and a
    // manager (`transfer-livechat-guest` for "Forward").
    let _: Value = admin
        .post("livechat/users/agent", &json!({"username": username}))
        .await
        .expect("make agent");
    created.agent = Some(uid.clone());
    let _: Value = admin
        .post("livechat/users/manager", &json!({"username": username}))
        .await
        .expect("make manager");
    created.manager = Some(uid.clone());
    let s2 = sid.clone();
    d.pump_until("omnichannel enabled", Duration::from_secs(30), move |_| {
        rustket::omnichannel::shared::get(&s2)
            .is_some_and(|o| o.enabled && o.is_agent && o.agent_status == AgentStatus::Available)
    })
    .await;

    // A throwaway visitor opens a chat (visitor endpoints need no auth).
    let anon = rest(server).await;
    let suffix = format!("{:x}", chrono::Utc::now().timestamp_millis() as u32);
    let token = format!("rk-snap-vt-{suffix}");
    created.token = Some(token.clone());
    let vname = format!("Dana Visitor {}", &suffix[suffix.len() - 4..]);
    let _: Value = anon
        .post(
            "livechat/visitor",
            &json!({"visitor": {"token": token, "name": vname,
                "email": format!("rk-snap-v-{suffix}@rustket.local")}}),
        )
        .await
        .expect("visitor");
    let r: Value = anon
        .get(
            "livechat/room",
            &rocketchat::rest::Query::new().push("token", &token),
        )
        .await
        .expect("livechat room");
    let rid = r["room"]["_id"].as_str().expect("rid").to_owned();
    created.rid = Some(rid.clone());
    let say = |text: &'static str| {
        let (anon, token, rid) = (anon.clone(), token.clone(), rid.clone());
        async move {
            let _: Value = anon
                .post(
                    "livechat/message",
                    &json!({"token": token, "rid": rid, "msg": text}),
                )
                .await
                .expect("visitor message");
        }
    };
    say("Hi! My order #4521 has not arrived yet.").await;
    // Auto_Selection may pick another routable agent of the shared server.
    let room = admin.rooms_info(&rid).await.expect("rooms.info");
    if room.served_by_agent().map(|s| s.id) != Some(uid.clone()) {
        admin
            .livechat_forward_chat(&ForwardChat {
                room_id: rid.clone(),
                target: ForwardTarget::Agent(uid.clone()),
                comment: None,
            })
            .await
            .expect("forward to the snapshot user");
    }
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("chat subscription", Duration::from_secs(20), move |a| {
        a.servers[&s2].workspace.subscription(&r2).is_some()
    })
    .await;
    // Some content for the info panel and the timeline.
    let agent = RestClient::new(
        server.clone(),
        &rocketchat::TlsConfig::new().build().expect("tls"),
    )
    .expect("rest");
    agent
        .login_password(username, password, None)
        .await
        .expect("agent login");
    let _: Value = agent
        .post(
            "chat.sendMessage",
            &json!({"message": {"rid": rid, "msg": "Hello Dana! Let me check the delivery status."}}),
        )
        .await
        .expect("agent message");
    say("Thanks, I ordered it last Monday.").await;
    let room = admin.rooms_info(&rid).await.expect("rooms.info");
    let mut info = rocketchat::models::SaveRoomInfo::from_room(&room);
    info.room.topic = Some("Delivery delay".into());
    info.room.tags = vec!["delivery".into(), "vip".into()];
    if let Err(e) = agent.livechat_save_room_info(&info).await {
        eprintln!(
            "agent saveInfo failed (servedBy {:?}, me {uid}): {e}",
            room.served_by_agent().map(|s| s.id)
        );
        admin
            .livechat_save_room_info(&info)
            .await
            .expect("saveInfo");
    }
    let _ = agent.logout().await;

    // Open the chat from the sidebar ("Chats in progress").
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Open(rid.clone()),
    ));
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("chat open", Duration::from_secs(20), move |a| {
        a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
            && a.servers[&s2]
                .workspace
                .room(&r2)
                .and_then(|e| e.room.as_ref())
                .is_some_and(|r| r.tags.as_ref().is_some_and(|t| t.len() == 2))
    })
    .await;
    d.settle(Duration::from_secs(1)).await;
    both(d, out, "o1-chat");

    // Room information panel.
    header(
        d,
        sid,
        &rid,
        room::header::Message::TogglePanel(room::panel::Kind::Omnichannel(PanelKind::Info)),
    );
    d.settle(Duration::from_millis(1500)).await;
    both(d, out, "o2-chat-info");

    // Contact information panel.
    header(
        d,
        sid,
        &rid,
        room::header::Message::Omni(Request::Panel(PanelKind::Contact)),
    );
    d.settle(Duration::from_millis(1500)).await;
    both(d, out, "o3-contact-info");
    header(
        d,
        sid,
        &rid,
        room::header::Message::Omni(Request::Panel(PanelKind::Contact)),
    );

    // Availability toggle in the status menu, and a live round trip.
    {
        use rustket::workspace::status_menu::Message as Sm;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        d.settle(Duration::from_millis(300)).await;
        both(d, out, "o4-availability");
        d.send(Message::StatusMenu(
            sid.clone(),
            Sm::Omni(rustket::omnichannel::status::Msg::Set(
                AgentStatus::NotAvailable,
            )),
        ));
        let s2 = sid.clone();
        d.pump_until("not available", Duration::from_secs(15), move |a| {
            a.servers[&s2]
                .workspace
                .me()
                .is_some_and(|m| m.agent_status() == AgentStatus::NotAvailable)
        })
        .await;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        both(d, out, "o4b-not-available");
        d.send(Message::StatusMenu(
            sid.clone(),
            Sm::Omni(rustket::omnichannel::status::Msg::Set(
                AgentStatus::Available,
            )),
        ));
        let s2 = sid.clone();
        d.pump_until("available again", Duration::from_secs(15), move |a| {
            a.servers[&s2]
                .workspace
                .me()
                .is_some_and(|m| m.agent_status() == AgentStatus::Available)
        })
        .await;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(false)));
    }

    // Forward dialog (the user is a manager here).
    header(
        d,
        sid,
        &rid,
        room::header::Message::Omni(Request::Quick(QuickAction::Forward)),
    );
    assert!(
        matches!(
            d.app.servers[sid].rooms[&rid].omni.dialog,
            Some(dialogs::Dialog::Forward(_))
        ),
        "forward dialog expected"
    );
    d.settle(Duration::from_millis(1500)).await;
    both(d, out, "o5-forward-dialog");
    dialog(
        d,
        sid,
        &rid,
        dialogs::Msg::Forward(dialogs::forward_chat::Msg::Cancel),
    );

    // Contact center.
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Omni(
            rustket::omnichannel::sidebar::Msg::ContactCenter,
        ),
    ));
    d.send(Message::Omni(
        sid.clone(),
        d.app.servers[sid].generation,
        rustket::omnichannel::Message::ContactCenter(
            rustket::omnichannel::contact_center::Msg::Tab(
                rustket::omnichannel::contact_center::Tab::Chats,
            ),
        ),
    ));
    d.settle(Duration::from_millis(1500)).await;
    assert!(d.app.servers[sid].omni.contact_center.is_some());
    both(d, out, "o6-contact-center");
    d.send(Message::Omni(
        sid.clone(),
        d.app.servers[sid].generation,
        rustket::omnichannel::Message::ContactCenter(
            rustket::omnichannel::contact_center::Msg::Close,
        ),
    ));

    // Close dialog: a comment is required on the dev server.
    header(
        d,
        sid,
        &rid,
        room::header::Message::Omni(Request::Quick(QuickAction::Close)),
    );
    d.settle(Duration::from_millis(800)).await;
    dialog(
        d,
        sid,
        &rid,
        dialogs::Msg::Close(close_chat::Msg::Comment(
            "Delivery rescheduled for Friday".into(),
        )),
    );
    both(d, out, "o7-close-dialog");
    dialog(d, sid, &rid, dialogs::Msg::Close(close_chat::Msg::Submit));
    let (s2, r2) = (sid.clone(), rid.clone());
    d.pump_until("chat closed", Duration::from_secs(20), move |a| {
        !a.servers[&s2].rooms.contains_key(&r2)
            && a.servers[&s2].workspace.subscription(&r2).is_none()
    })
    .await;
    let closed = admin.rooms_info(&rid).await.expect("rooms.info");
    assert!(closed.open != Some(true), "closed on the server");
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "o8-after-close");

    // The closed chat from the contact center: read-only with its history.
    {
        use rustket::omnichannel::Message as Om;
        use rustket::omnichannel::contact_center::{Msg as Cc, Tab};
        let cc = |d: &Driver, m: Cc| {
            Message::Omni(
                sid.clone(),
                d.app.servers[sid].generation,
                Om::ContactCenter(m),
            )
        };
        d.send(Message::Sidebar(
            sid.clone(),
            rustket::workspace::sidebar::Message::Omni(
                rustket::omnichannel::sidebar::Msg::ContactCenter,
            ),
        ));
        let m = cc(d, Cc::Tab(Tab::Chats));
        d.send(m);
        let m = cc(d, Cc::Query(vname.clone()));
        d.send(m);
        let (s2, r2) = (sid.clone(), rid.clone());
        d.pump_until("closed chat listed", Duration::from_secs(15), move |a| {
            a.servers[&s2]
                .omni
                .contact_center
                .as_ref()
                .is_some_and(|c| c.chats.iter().any(|r| r.id == r2))
        })
        .await;
        let m = cc(d, Cc::OpenChat(rid.clone()));
        d.send(m);
        let (s2, r2) = (sid.clone(), rid.clone());
        d.pump_until("closed chat open", Duration::from_secs(20), move |a| {
            a.servers[&s2].rooms.get(&r2).is_some_and(|r| r.loaded)
        })
        .await;
        d.settle(Duration::from_secs(1)).await;
        let s = &d.app.servers[sid];
        assert_eq!(
            rustket::omnichannel::room::state_of(
                &s.workspace,
                &rustket::omnichannel::room::omni_of(sid),
                &rid
            ),
            Some(rustket_core::omnichannel::room::ComposerState::Closed)
        );
        assert!(
            s.workspace.timeline(&rid).is_some_and(|t| t
                .messages()
                .any(|m| m.t.as_deref() == Some("livechat-close"))),
            "close message in the history"
        );
        both(d, out, "o9-closed-chat");
    }
    let _ = admin.logout().await;
    println!("omnichannel checks passed");
}
