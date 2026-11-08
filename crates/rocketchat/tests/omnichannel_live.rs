//! Live omnichannel test against the dev server (see CLAUDE.md and
//! `docs/omnichannel.md` §22.5). Skipped unless `RUSTKET_TEST_SERVER` is set.
//!
//! Creates two throwaway agents, one department (CE allows one in total), a
//! visitor and a chat; serves it, forwards it into the department queue, takes it
//! from the queue and closes it. Everything is removed at the end, also on
//! failure. No global setting is changed, except that department removal is
//! enabled just for the delete when the server forbids it, and restored.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rocketchat::auth::TwoFactorCode;
use rocketchat::models::{
    AgentStatus, CloseChat, ForwardChat, ForwardTarget, InquiryEventKind, InquiryStatus,
    SaveRoomInfo, TranscriptEmail,
};
use rocketchat::rest::{AgentQuery, DepartmentQuery, LivechatPage};
use rocketchat::session::{
    ChangeKind, OmnichannelEvent, Session, SessionEvent, SessionEvents, SessionOptions,
    inquiry_queue_subscriptions,
};
use rocketchat::{RestClient, ServerUrl, TlsConfig, TlsContext, TwoFactorMethod};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(20);
const BODY_TIMEOUT: Duration = Duration::from_secs(180);

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

fn tls() -> TlsContext {
    TlsConfig::new().build().unwrap()
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

async fn login(server: &ServerUrl, user: &str, password: &str) -> RestClient {
    let c = RestClient::new(server.clone(), &tls()).unwrap();
    c.login_password(user, password, None)
        .await
        .unwrap_or_else(|e| panic!("login {user}: {e}"));
    c
}

#[derive(Default)]
struct Created {
    users: Vec<String>,
    agents: Vec<String>,
    department: Option<String>,
    visitor_token: Option<String>,
    rid: Option<String>,
}

struct Agent {
    id: String,
    rest: RestClient,
    session: Session,
    events: SessionEvents,
}

async fn agent(server: &ServerUrl, admin: &RestClient, created: &Mutex<Created>) -> Agent {
    let username = unique("omnilive-a");
    let password = format!("{username}-pw");
    let r: Value = admin
        .post(
            "users.create",
            &json!({
                "username": username, "email": format!("{username}@rustket.local"),
                "password": password, "name": format!("Agent {username}"), "verified": true,
                "requirePasswordChange": false,
            }),
        )
        .await
        .unwrap();
    let id = r["user"]["_id"].as_str().unwrap().to_owned();
    created.lock().unwrap().users.push(id.clone());
    let _: Value = admin
        .post("livechat/users/agent", &json!({ "username": username }))
        .await
        .unwrap();
    created.lock().unwrap().agents.push(id.clone());
    let rest = login(server, &username, &password).await;
    // A DDP login makes the agent online (routable).
    let (session, mut events) = tokio::time::timeout(
        WAIT,
        Session::start(
            server.clone(),
            &tls(),
            rest.credentials().unwrap(),
            SessionOptions::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    expect(&mut events, |e| matches!(e, SessionEvent::Ready { .. })).await;
    Agent {
        id,
        rest,
        session,
        events,
    }
}

async fn expect(
    events: &mut SessionEvents,
    mut pred: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(e)) if pred(&e) => return e,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream ended"),
            Err(_) => panic!("timed out waiting for event"),
        }
    }
}

fn is_inquiry(e: &SessionEvent, rid: &str, kind: InquiryEventKind, status: InquiryStatus) -> bool {
    matches!(e.omnichannel(), Some(OmnichannelEvent::InquiryQueue { kind: k, inquiry, .. })
        if k == kind && inquiry.rid == rid && (kind == InquiryEventKind::Removed || inquiry.status == status))
}

async fn wait_until<F, Fut>(mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..40 {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

async fn scenario(server: ServerUrl, admin: RestClient, created: Arc<Mutex<Created>>) {
    let creation: Value = admin
        .get(
            "livechat/department/isDepartmentCreationAvailable",
            &Default::default(),
        )
        .await
        .unwrap();
    if creation["isDepartmentCreationAvailable"] != true {
        // Another run or agent holds the single CE department slot.
        eprintln!("skipping omnichannel live scenario: the CE department slot is taken");
        return;
    }

    let mut a = agent(&server, &admin, &created).await;
    let mut b = agent(&server, &admin, &created).await;

    // Availability.
    assert_eq!(
        a.rest
            .livechat_agent_status(Some(AgentStatus::NotAvailable))
            .await
            .unwrap(),
        AgentStatus::NotAvailable
    );
    let ev = expect(&mut a.events, |e| e.agent_status().is_some()).await;
    assert_eq!(ev.agent_status(), Some(AgentStatus::NotAvailable));
    assert_eq!(
        a.rest.livechat_agent_status(None).await.unwrap(),
        AgentStatus::Available,
        "no status toggles"
    );
    // B must not get the chat first.
    b.rest
        .livechat_agent_status(Some(AgentStatus::NotAvailable))
        .await
        .unwrap();
    let e = admin.livechat_agent_status(None).await.unwrap_err();
    assert_eq!(e.livechat_error_code(), Some("Agent not found"));

    let routing = a.rest.livechat_routing_config().await.unwrap();
    assert!(routing.auto_assign_agent, "dev server: Auto_Selection");
    let license = a.rest.licenses_info().await.unwrap();
    assert!(!license.has_module("livechat-enterprise"));
    assert!(
        a.rest
            .livechat_tags(None, None, true)
            .await
            .unwrap_err()
            .is_enterprise_only()
    );

    // Queue streams (no departments yet).
    let qwatch = a
        .session
        .watch_streams(&inquiry_queue_subscriptions(&a.id, []))
        .await
        .unwrap();
    assert!(qwatch.refused().is_empty(), "{:?}", qwatch.refused());

    // Visitor and chat (visitor endpoints need no auth).
    let anon = RestClient::new(server.clone(), &tls()).unwrap();
    let token = unique("omnilive-vt");
    created.lock().unwrap().visitor_token = Some(token.clone());
    let vname = unique("Omnilive Visitor");
    let v: Value = anon
        .post(
            "livechat/visitor",
            &json!({"visitor": {"token": token, "name": vname,
                    "email": format!("{}@rustket.local", unique("omnilive-v"))}}),
        )
        .await
        .unwrap();
    let visitor_id = v["visitor"]["_id"].as_str().unwrap().to_owned();
    let r: Value = anon
        .get(
            "livechat/room",
            &rocketchat::rest::Query::new().push("token", &token),
        )
        .await
        .unwrap();
    let rid = r["room"]["_id"].as_str().unwrap().to_owned();
    created.lock().unwrap().rid = Some(rid.clone());
    let _: Value = anon
        .post(
            "livechat/message",
            &json!({"token": token, "rid": rid, "msg": "hello from the live test"}),
        )
        .await
        .unwrap();

    // Auto_Selection may pick any routable agent of the shared server: make sure A serves it.
    let room = admin.rooms_info(&rid).await.unwrap();
    if room.served_by_agent().map(|s| s.id) != Some(a.id.clone()) {
        admin
            .livechat_forward_chat(&ForwardChat {
                room_id: rid.clone(),
                target: ForwardTarget::Agent(a.id.clone()),
                comment: None,
            })
            .await
            .unwrap();
    }
    expect(&mut a.events, |e| {
        matches!(e, SessionEvent::SubscriptionChanged { kind: ChangeKind::Inserted, sub } if sub.rid == rid)
    })
    .await;
    let room = a.rest.rooms_info(&rid).await.unwrap();
    assert!(room.is_omnichannel_open());
    assert_eq!(room.served_by_agent().unwrap().id, a.id);
    assert_eq!(room.visitor().unwrap().id, visitor_id);
    let sub = a.rest.subscriptions_get_one(&rid).await.unwrap();
    assert_eq!(sub.v.as_ref().unwrap().id, visitor_id);

    // Info, contact, history.
    let vis = a.rest.livechat_visitor_info(&visitor_id).await.unwrap();
    assert_eq!(vis.name.as_deref(), Some(vname.as_str()));
    let contact_id = room.contact_id.clone().unwrap();
    let contact = a
        .rest
        .omnichannel_contact(&contact_id)
        .await
        .unwrap()
        .unwrap();
    assert!(contact.unknown);
    assert!(
        a.rest
            .omnichannel_contact("nonexistent-contact")
            .await
            .unwrap()
            .is_none()
    );
    let chans = a
        .rest
        .omnichannel_contact_channels(&contact_id)
        .await
        .unwrap();
    assert_eq!(chans[0].visitor.visitor_id, visitor_id);
    let msgs = a
        .rest
        .livechat_room_messages(&rid, None, LivechatPage::new(0, 50), true)
        .await
        .unwrap();
    assert!(msgs.items.iter().any(|m| m.is_from_visitor()));
    let hist = a
        .rest
        .livechat_visitor_chat_history(&rid, &visitor_id, LivechatPage::new(0, 10))
        .await
        .unwrap();
    assert!(hist.items.iter().any(|r| r.id == rid));
    let inq = a
        .rest
        .livechat_inquiry_for_room(&rid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inq.status, InquiryStatus::Taken);
    a.rest
        .livechat_custom_fields(LivechatPage::default())
        .await
        .unwrap();
    let agents = a
        .rest
        .livechat_agents(&AgentQuery {
            text: Some(b.session.me().unwrap().username.unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(agents.items.iter().any(|x| x.id == b.id));

    // Save info: tags must always be sent in full.
    let mut info = SaveRoomInfo::from_room(&room);
    info.room.topic = Some("live topic".into());
    info.room.tags = vec!["alpha".into(), "beta".into()];
    a.rest.livechat_save_room_info(&info).await.unwrap();
    let saved = a.rest.rooms_info(&rid).await.unwrap();
    assert_eq!(saved.tags.as_deref().unwrap(), ["alpha", "beta"]);
    assert_eq!(saved.topic.as_deref(), Some("live topic"));

    // Forward is not allowed for plain agents.
    let e = a
        .rest
        .livechat_forward_chat(&ForwardChat {
            room_id: rid.clone(),
            target: ForwardTarget::Agent(b.id.clone()),
            comment: None,
        })
        .await
        .unwrap_err();
    assert_eq!(e.livechat_error_code(), Some("error-unauthorized"));

    // Department with A; membership arrives as departmentAgentData.
    let d: Value = admin
        .post(
            "livechat/department",
            &json!({
                "department": {"enabled": true, "name": unique("omnilive-dept"),
                    "email": format!("{}@rustket.local", unique("omnilive-d")),
                    "showOnRegistration": true, "showOnOfflineForm": false,
                    "allowReceiveForwardOffline": true, "chatClosingTags": ["dept-tag"]},
                "agents": [{"agentId": a.id, "count": 0, "order": 0}],
            }),
        )
        .await
        .unwrap();
    let dept = d["department"]["_id"].as_str().unwrap().to_owned();
    created.lock().unwrap().department = Some(dept.clone());
    let ev = expect(&mut a.events, |e| {
        matches!(
            e.omnichannel(),
            Some(OmnichannelEvent::DepartmentAgentChanged(_))
        )
    })
    .await;
    let Some(OmnichannelEvent::DepartmentAgentChanged(c)) = ev.omnichannel() else {
        unreachable!()
    };
    assert_eq!(c.department_id, dept);
    let mine = a
        .rest
        .livechat_agent_departments(&a.id, true)
        .await
        .unwrap();
    assert!(
        mine.iter()
            .any(|d| d.department_id == dept && d.department_enabled)
    );
    let fetched = a.rest.livechat_department(&dept).await.unwrap().unwrap();
    assert!(fetched.allow_receive_forward_offline);
    assert!(
        a.rest
            .livechat_department("nonexistent")
            .await
            .unwrap()
            .is_none()
    );
    let list = a
        .rest
        .livechat_departments(&DepartmentQuery::for_forward(None, None))
        .await
        .unwrap();
    assert!(list.items.iter().any(|d| d.id == dept));
    let dwatch = a
        .session
        .watch_streams(&inquiry_queue_subscriptions(&a.id, [dept.as_str()]))
        .await
        .unwrap();
    assert!(dwatch.refused().is_empty());

    // A unavailable, admin forwards into the department: the chat is queued there.
    a.rest
        .livechat_agent_status(Some(AgentStatus::NotAvailable))
        .await
        .unwrap();
    admin
        .livechat_forward_chat(&ForwardChat {
            room_id: rid.clone(),
            target: ForwardTarget::Department(dept.clone()),
            comment: Some("to the queue".into()),
        })
        .await
        .unwrap();
    expect(&mut a.events, |e| {
        is_inquiry(e, &rid, InquiryEventKind::Changed, InquiryStatus::Queued)
    })
    .await;
    let queued = a
        .rest
        .livechat_inquiries_queued(Some(25), None)
        .await
        .unwrap();
    let inquiry = queued
        .items
        .iter()
        .find(|i| i.rid == rid)
        .expect("department member sees the inquiry")
        .clone();
    assert_eq!(inquiry.department.as_deref(), Some(dept.as_str()));
    let b_queue = b
        .rest
        .livechat_inquiries_queued(Some(25), None)
        .await
        .unwrap();
    assert!(!b_queue.items.iter().any(|i| i.rid == rid), "non-member");
    let one = a
        .rest
        .livechat_inquiry_for_room(&rid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(one.status, InquiryStatus::Queued);

    // Taking needs an available agent; B (not in the department, so routing
    // does not hand it over by itself) takes it.
    let e = b.rest.livechat_take_inquiry(&inquiry.id).await.unwrap_err();
    assert_eq!(
        e.livechat_error_code(),
        Some("error-agent-status-service-offline")
    );
    b.rest
        .livechat_agent_status(Some(AgentStatus::Available))
        .await
        .unwrap();
    b.rest.livechat_take_inquiry(&inquiry.id).await.unwrap();
    expect(&mut b.events, |e| {
        matches!(e, SessionEvent::SubscriptionChanged { kind: ChangeKind::Inserted, sub } if sub.rid == rid)
    })
    .await;
    expect(&mut a.events, |e| {
        is_inquiry(e, &rid, InquiryEventKind::Changed, InquiryStatus::Taken)
    })
    .await;
    let served = b.rest.rooms_info(&rid).await.unwrap();
    assert_eq!(served.served_by_agent().unwrap().id, b.id);
    let e = b.rest.livechat_take_inquiry(&inquiry.id).await.unwrap_err();
    assert_eq!(e.livechat_error_code(), Some("error-inquiry-taken"));

    // Close: the comment is required on the dev server.
    if let Err(e) = b
        .rest
        .livechat_close_chat(&CloseChat::new(&rid, None))
        .await
    {
        assert_eq!(e.livechat_error_code(), Some("error-comment-is-required"));
        b.rest
            .livechat_close_chat(&CloseChat {
                tags: vec!["closing".into()],
                transcript_email: TranscriptEmail::none(),
                ..CloseChat::new(&rid, Some("resolved by the live test".into()))
            })
            .await
            .unwrap();
    }
    expect(&mut a.events, |e| {
        is_inquiry(e, &rid, InquiryEventKind::Removed, InquiryStatus::Unknown)
    })
    .await;
    let closed = admin.rooms_info(&rid).await.unwrap();
    assert!(!closed.is_omnichannel_open());
    assert!(closed.closed_at.is_some());
    let tags = closed.tags.unwrap_or_default();
    assert!(tags.contains(&"dept-tag".to_owned()), "{tags:?}");
    let e = b
        .rest
        .livechat_close_chat(&CloseChat::new(&rid, Some("again".into())))
        .await
        .unwrap_err();
    assert_eq!(e.livechat_error_code(), Some("error-not-authorized"));
    let history_ok = wait_until(|| {
        let rest = b.rest.clone();
        let cid = contact_id.clone();
        let rid = rid.clone();
        async move {
            rest.omnichannel_contact_history(&cid, None, LivechatPage::new(0, 10))
                .await
                .is_ok_and(|h| h.items.iter().any(|r| r.id == rid))
        }
    })
    .await;
    assert!(history_ok, "closed chat in the contact history");

    drop(qwatch);
    drop(dwatch);
    // DDP logout makes agents not-available (and ends the sessions).
    let _ = a.session.call("logout", vec![]).await;
    let _ = b.session.call("logout", vec![]).await;
    a.session.close().await;
    b.session.close().await;
}

async fn cleanup(admin: &RestClient, created: &Mutex<Created>) {
    let c = std::mem::take(&mut *created.lock().unwrap());
    if let Some(rid) = &c.rid {
        let _ = admin
            .livechat_close_chat(&CloseChat::new(rid, Some("cleanup".into())))
            .await;
        if let Err(e) = admin
            .post_ok("livechat/rooms.delete", &json!({ "roomId": rid }))
            .await
        {
            eprintln!("cleanup: deleting room {rid} failed: {e}");
        }
    }
    if let Some(token) = &c.visitor_token {
        let r: Result<Value, _> = admin
            .delete(&format!("livechat/visitor/{token}"), &Default::default())
            .await;
        if let Err(e) = r {
            eprintln!("cleanup: deleting visitor failed: {e}");
        }
    }
    if let Some(dept) = &c.department {
        let path = format!("livechat/department/{dept}");
        let r: Result<Value, _> = admin.delete(&path, &Default::default()).await;
        if r.is_err() {
            // Department removal is disabled by default: enable it just for this delete.
            let digest = Sha256::digest(b"adminpass");
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            let tf = TwoFactorCode {
                code: hex,
                method: TwoFactorMethod::Password,
            };
            let setting = "settings/Omnichannel_enable_department_removal";
            let on: Result<Value, _> = admin
                .post_with(setting, &json!({ "value": true }), Some(&tf))
                .await;
            let del: Result<Value, _> = admin.delete(&path, &Default::default()).await;
            let off: Result<Value, _> = admin
                .post_with(setting, &json!({ "value": false }), Some(&tf))
                .await;
            if on.is_err() || del.is_err() || off.is_err() {
                eprintln!("cleanup: department {dept}: on={on:?} del={del:?} off={off:?}");
            }
        }
    }
    for id in &c.agents {
        let _: Result<Value, _> = admin
            .delete(&format!("livechat/users/agent/{id}"), &Default::default())
            .await;
    }
    for id in &c.users {
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
    let _ = admin.logout().await;
}

#[tokio::test]
async fn omnichannel_agent_flow() {
    let Some(server) = server() else { return };
    let admin = login(&server, "admin", "adminpass").await;
    let created = Arc::new(Mutex::new(Created::default()));
    let task = tokio::spawn(scenario(
        server.clone(),
        admin.clone(),
        Arc::clone(&created),
    ));
    let abort = task.abort_handle();
    let result = tokio::time::timeout(BODY_TIMEOUT, task).await;
    if result.is_err() {
        abort.abort();
    }
    cleanup(&admin, &created).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e.into_panic()),
        Err(_) => panic!("test body timed out after {BODY_TIMEOUT:?}"),
    }
}
