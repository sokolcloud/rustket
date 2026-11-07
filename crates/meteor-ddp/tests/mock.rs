//! Integration tests against an in-process mock DDP server.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{MockConn, MockServer, wait_connected, wait_state, within};
use futures::StreamExt;
use meteor_ddp::{
    CallError, ConnectError, ConnectionState, DataKind, DataMessage, DdpClient, DdpConfig,
    HeartbeatConfig, HookError, RecvError, Routing, SubscribeError, SubscribeOptions, Subscription,
    SubscriptionState,
};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

async fn connect(server: &mut MockServer, config: DdpConfig) -> (DdpClient, MockConn) {
    let (client, conn) = tokio::join!(DdpClient::connect(config), server.accept_handshake("s1"));
    (client.expect("connect"), conn)
}

async fn connect_default(server: &mut MockServer) -> (DdpClient, MockConn) {
    let config = server.config();
    connect(server, config).await
}

fn stream_params(event: &str) -> Vec<Value> {
    vec![json!(event), json!({"useCollection": false, "args": []})]
}

#[tokio::test]
async fn handshake_reports_session() {
    let mut server = MockServer::start().await;
    let (client, _conn) = connect_default(&mut server).await;
    assert_eq!(
        client.state(),
        ConnectionState::Connected {
            session: "s1".into()
        }
    );
    client.close().await;
}

#[tokio::test]
async fn first_connect_failure_is_returned() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/websocket")).unwrap();
    let err = within(DdpClient::connect(DdpConfig::new(url)))
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectError::Io(_)), "{err:?}");
}

#[tokio::test]
async fn handshake_failed_is_fatal() {
    let mut server = MockServer::start().await;
    let config = server.config();
    let (result, _conn) = tokio::join!(DdpClient::connect(config), async {
        let mut conn = server.accept().await;
        conn.recv_msg("connect").await;
        conn.send(json!({"msg": "failed", "version": "pre1"})).await;
        conn
    });
    let err = result.unwrap_err();
    assert!(
        matches!(&err, ConnectError::Handshake(Some(v)) if v == "pre1"),
        "{err:?}"
    );
}

#[tokio::test]
async fn server_ping_gets_pong() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    conn.send(json!({"msg": "ping", "id": "abc"})).await;
    assert_eq!(conn.recv().await, json!({"msg": "pong", "id": "abc"}));
    conn.send(json!({"msg": "ping"})).await;
    assert_eq!(conn.recv().await, json!({"msg": "pong"}));
    client.close().await;
}

#[tokio::test]
async fn client_heartbeat_and_dead_connection() {
    let mut server = MockServer::start().await;
    let config = server.config().heartbeat(HeartbeatConfig {
        interval: Duration::from_millis(200),
        timeout: Duration::from_millis(1500),
    });
    let (client, mut conn) = connect(&mut server, config).await;
    let mut state = client.watch_state();

    // Idle connection: client pings; answering keeps it alive.
    let ping = conn.recv_msg("ping").await;
    assert!(ping["id"].is_string());
    conn.send(json!({"msg": "pong", "id": ping["id"]})).await;
    let ping2 = conn.recv_msg("ping").await;
    assert_ne!(ping["id"], ping2["id"]);
    assert!(client.state().is_connected());

    // Not answering: client gives up and reconnects.
    wait_state(&mut state, |s| {
        matches!(s, ConnectionState::Reconnecting { reason, .. } if reason.contains("heartbeat"))
    })
    .await;
    let _conn2 = server.accept_handshake("s2").await;
    wait_state(&mut state, |s| {
        *s == ConnectionState::Connected {
            session: "s2".into(),
        }
    })
    .await;
    client.close().await;
}

#[tokio::test]
async fn method_result_and_error() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;

    let c = client.clone();
    let call = tokio::spawn(async move { c.call("add", vec![json!(1), json!(2)]).await });
    let req = conn.recv_msg("method").await;
    assert_eq!(req["method"], "add");
    assert_eq!(req["params"], json!([1, 2]));
    conn.send(json!({"msg": "updated", "methods": [req["id"]]}))
        .await;
    conn.send(json!({"msg": "result", "id": req["id"], "result": {"sum": 3}}))
        .await;
    assert_eq!(within(call).await.unwrap().unwrap(), json!({"sum": 3}));

    // Missing result means null.
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("noop", vec![]).await });
    let req = conn.recv_msg("method").await;
    conn.send(json!({"msg": "result", "id": req["id"]})).await;
    assert_eq!(within(call).await.unwrap().unwrap(), Value::Null);

    // Error with details.
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("spam", vec![]).await });
    let req = conn.recv_msg("method").await;
    conn.send(json!({
        "msg": "result", "id": req["id"],
        "error": {
            "error": "too-many-requests",
            "reason": "slow down",
            "message": "slow down [too-many-requests]",
            "errorType": "Meteor.Error",
            "details": {"timeToReset": 1234}
        }
    }))
    .await;
    let err = within(call).await.unwrap().unwrap_err();
    let CallError::Server(e) = err else {
        panic!("unexpected {err:?}");
    };
    assert!(e.is("too-many-requests"));
    assert_eq!(e.reason.as_deref(), Some("slow down"));
    assert_eq!(e.error_type.as_deref(), Some("Meteor.Error"));
    assert_eq!(e.details, Some(json!({"timeToReset": 1234})));

    // Numeric error code.
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("x", vec![]).await });
    let req = conn.recv_msg("method").await;
    conn.send(json!({"msg": "result", "id": req["id"], "error": {"error": 403, "reason": "nope"}}))
        .await;
    let Err(CallError::Server(e)) = within(call).await.unwrap() else {
        panic!("expected server error");
    };
    assert_eq!(e.code(), "403");
    client.close().await;
}

#[tokio::test]
async fn call_timeout() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let c = client.clone();
    let call = tokio::spawn(async move {
        c.call_with_timeout("slow", vec![], Duration::from_millis(100))
            .await
    });
    conn.recv_msg("method").await;
    assert!(matches!(
        within(call).await.unwrap(),
        Err(CallError::Timeout)
    ));
    client.close().await;
}

#[tokio::test]
async fn sub_ready_and_nosub() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;

    let c = client.clone();
    let sub = tokio::spawn(async move { c.subscribe("things", vec![json!(1)]).await });
    let req = conn.answer_sub("things").await;
    assert_eq!(req["params"], json!([1]));
    let mut sub = within(sub).await.unwrap().unwrap();
    assert_eq!(sub.id(), req["id"]);
    assert_eq!(sub.state(), SubscriptionState::Ready);

    // Refused subscription.
    let c = client.clone();
    let refused = tokio::spawn(async move { c.subscribe("secret", vec![]).await });
    let req = conn.recv_msg("sub").await;
    conn.send(json!({"msg": "nosub", "id": req["id"], "error": {"error": "not-allowed", "reason": "Not allowed"}}))
        .await;
    match within(refused).await.unwrap() {
        Err(SubscribeError::Server(Some(e))) => assert!(e.is("not-allowed")),
        other => panic!("unexpected {other:?}"),
    }

    // Server stops a ready subscription later.
    let mut state = sub.watch_state();
    conn.send(json!({"msg": "nosub", "id": sub.id()})).await;
    within(state.wait_for(|s| matches!(s, SubscriptionState::Stopped(None))))
        .await
        .unwrap();
    assert_eq!(within(sub.recv()).await.unwrap_err(), RecvError::Closed);
    client.close().await;
}

fn spawn_sub(
    client: &DdpClient,
    name: &str,
    params: Vec<Value>,
) -> tokio::task::JoinHandle<Result<Subscription, SubscribeError>> {
    let client = client.clone();
    let name = name.to_owned();
    tokio::spawn(async move { client.subscribe(&name, params).await })
}

async fn subscribe(
    client: &DdpClient,
    conn: &mut MockConn,
    name: &str,
    params: Vec<Value>,
) -> Subscription {
    let task = spawn_sub(client, name, params);
    conn.answer_sub(name).await;
    within(task).await.unwrap().unwrap()
}

fn first_arg(data: &DataMessage) -> Value {
    data.args().expect("args")[0].clone()
}

#[tokio::test]
async fn stream_events_are_routed_by_collection_and_event_name() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let mut all = client.data_messages();

    let rm = "stream-room-messages";
    let mut a = subscribe(&client, &mut conn, rm, stream_params("room-a")).await;
    let mut b = subscribe(&client, &mut conn, rm, stream_params("room-b")).await;
    let mut u = subscribe(
        &client,
        &mut conn,
        "stream-notify-user",
        stream_params("u1/message"),
    )
    .await;

    conn.stream_event(rm, "room-b", json!([{"msg": "to b"}]))
        .await;
    conn.stream_event(rm, "room-a", json!([{"msg": "to a"}]))
        .await;
    conn.stream_event("stream-notify-user", "u1/message", json!([{"x": 1}]))
        .await;
    conn.send(json!({"msg": "added", "collection": "users", "id": "u1", "fields": {"name": "A"}}))
        .await;
    conn.stream_event(rm, "room-a", json!([{"msg": "a2"}]))
        .await;
    conn.stream_event(rm, "room-b", json!([{"msg": "b2"}]))
        .await;

    assert_eq!(first_arg(&within(a.recv()).await.unwrap())["msg"], "to a");
    assert_eq!(first_arg(&within(a.recv()).await.unwrap())["msg"], "a2");
    assert_eq!(
        first_arg(&within(b.next()).await.unwrap().unwrap())["msg"],
        "to b"
    );
    assert_eq!(
        first_arg(&within(b.next()).await.unwrap().unwrap())["msg"],
        "b2"
    );
    let ev = within(u.recv()).await.unwrap();
    assert_eq!(ev.kind, DataKind::Changed);
    assert_eq!(ev.event_name(), Some("u1/message"));
    assert_eq!(first_arg(&ev)["x"], 1);

    // The broadcast sees everything, including collections nobody subscribed to by name.
    let mut seen = Vec::new();
    for _ in 0..6 {
        seen.push(within(all.recv()).await.unwrap());
    }
    assert_eq!(seen[0].event_name(), Some("room-b"));
    assert_eq!(seen[3].kind, DataKind::Added);
    assert_eq!(seen[3].collection, "users");
    assert_eq!(seen[3].fields.as_ref().unwrap()["name"], "A");
    client.close().await;
}

#[tokio::test]
async fn reconnect_runs_hook_and_resubscribes() {
    let mut server = MockServer::start().await;
    let hooks = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::new()));
    let (counter, sink) = (Arc::clone(&hooks), Arc::clone(&results));
    let config = server.config().on_connected(move |ddp| {
        let counter = Arc::clone(&counter);
        let sink = Arc::clone(&sink);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let user = ddp.call("login", vec![json!({"resume": "tok"})]).await?;
            sink.lock().unwrap().push(user);
            Ok(())
        }
    });
    let (client, mut conn) = tokio::join!(DdpClient::connect(config), async {
        let mut conn = server.accept_handshake("s1").await;
        let login = conn.answer_method("login", json!({"id": "u1"})).await;
        assert_eq!(login["params"], json!([{"resume": "tok"}]));
        conn
    });
    let client = client.unwrap();
    let rm = "stream-room-messages";
    let mut sub = subscribe(&client, &mut conn, rm, stream_params("room-a")).await;
    let mut state = client.watch_state();
    let mut sub_state = sub.watch_state();

    // Connection 1 dies without a close frame; connection 2 ends with a close frame.
    // Neither was stable, so the attempt counter keeps growing.
    conn.kill();
    for (round, session) in [(1, "s2"), (2, "s3")] {
        wait_state(
            &mut state,
            |s| matches!(s, ConnectionState::Reconnecting { attempt, .. } if *attempt == round),
        )
        .await;
        let mut next = server.accept_handshake(session).await;
        // The hook runs before anything else is sent.
        let first = next.recv().await;
        assert_eq!(first["msg"], "method", "{first}");
        assert_eq!(first["method"], "login");
        next.send(json!({"msg": "result", "id": first["id"], "result": {"id": "u1"}}))
            .await;
        let resub = next.answer_sub(rm).await;
        assert_eq!(resub["id"], sub.id());
        assert_eq!(resub["params"], json!(stream_params("room-a")));
        wait_state(&mut state, |s| {
            *s == ConnectionState::Connected {
                session: session.into(),
            }
        })
        .await;
        within(sub_state.wait_for(|s| *s == SubscriptionState::Ready))
            .await
            .unwrap();
        assert_eq!(hooks.load(Ordering::SeqCst), round as usize + 1);
        assert!(results.lock().unwrap().iter().all(|u| u["id"] == "u1"));
        assert_eq!(results.lock().unwrap().len(), round as usize + 1);

        next.stream_event(rm, "room-a", json!([{"msg": session}]))
            .await;
        assert_eq!(
            first_arg(&within(sub.recv()).await.unwrap())["msg"],
            session
        );
        if round == 1 {
            next.close_frame().await;
        } else {
            client.close().await;
            next.closed().await;
        }
    }
}

#[tokio::test]
async fn in_flight_call_fails_with_disconnected() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("sendMessage", vec![]).await });
    conn.recv_msg("method").await;
    conn.kill();
    assert!(matches!(
        within(call).await.unwrap(),
        Err(CallError::Disconnected)
    ));
    // The call is not retried on the new connection.
    let mut conn2 = server.accept_handshake("s2").await;
    wait_connected(&client).await;
    conn2.send(json!({"msg": "ping", "id": "p"})).await;
    assert_eq!(conn2.recv().await["msg"], "pong");
    client.close().await;
}

#[tokio::test]
async fn call_while_disconnected_waits_for_reconnect() {
    let mut server = MockServer::start().await;
    let (client, conn) = connect_default(&mut server).await;
    let mut state = client.watch_state();
    conn.kill();
    wait_state(&mut state, |s| !s.is_connected()).await;
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("later", vec![json!("x")]).await });
    let mut conn2 = server.accept_handshake("s2").await;
    let req = conn2.answer_method("later", json!("ok")).await;
    assert_eq!(req["params"], json!(["x"]));
    assert_eq!(within(call).await.unwrap().unwrap(), json!("ok"));
    client.close().await;
}

#[tokio::test]
async fn hook_auth_failure_is_terminal() {
    let mut server = MockServer::start().await;
    let config = server.config().on_connected(|ddp| async move {
        ddp.call("login", vec![json!({"resume": "tok"})]).await?;
        Ok(())
    });
    let (client, conn) = tokio::join!(DdpClient::connect(config), async {
        let mut conn = server.accept_handshake("s1").await;
        conn.answer_method("login", json!({"id": "u1"})).await;
        conn
    });
    let client = client.unwrap();
    let mut state = client.watch_state();
    conn.kill();

    let mut conn2 = server.accept_handshake("s2").await;
    let login = conn2.recv_msg("method").await;
    conn2
        .send(json!({
            "msg": "result", "id": login["id"],
            "error": {"error": 403, "reason": "You've been logged out by the server. Please log in again.", "message": "You've been logged out by the server. Please log in again. [403]", "errorType": "Meteor.Error"}
        }))
        .await;
    let failed = wait_state(&mut state, |s| matches!(s, ConnectionState::Failed { .. })).await;
    let ConnectionState::Failed { reason } = failed else {
        unreachable!()
    };
    assert!(reason.contains("403"), "{reason}");
    conn2.closed().await;
    assert!(
        server
            .no_connection_within(Duration::from_millis(600))
            .await
    );
    assert!(matches!(
        client.call("x", vec![]).await,
        Err(CallError::Disconnected)
    ));

    // A manual retry leaves the failed state.
    client.reconnect_now();
    let mut conn3 = server.accept_handshake("s3").await;
    conn3.answer_method("login", json!({"id": "u1"})).await;
    assert_eq!(wait_connected(&client).await, "s3");
    client.close().await;
}

#[tokio::test]
async fn hook_failure_on_first_connect_is_returned() {
    let mut server = MockServer::start().await;
    let config = server
        .config()
        .on_connected(|_ddp| async move { Err(HookError::fatal("bad credentials")) });
    let (result, _conn) = tokio::join!(DdpClient::connect(config), server.accept_handshake("s1"));
    match result {
        Err(ConnectError::Hook(e)) => {
            assert!(e.is_fatal());
            assert_eq!(e.message(), "bad credentials");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn retryable_hook_failure_reconnects() {
    let mut server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let config = server.config().on_connected(move |_ddp| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 1 {
                Err(HookError::retry("temporary"))
            } else {
                Ok(())
            }
        }
    });
    let (client, conn) = connect(&mut server, config).await;
    conn.kill();
    let _conn2 = server.accept_handshake("s2").await;
    let _conn3 = server.accept_handshake("s3").await;
    assert_eq!(wait_connected(&client).await, "s3");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    client.close().await;
}

#[tokio::test]
async fn unknown_and_malformed_frames_are_ignored() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    conn.send_raw(Message::text("not json {")).await;
    conn.send_raw(Message::binary(vec![1u8, 2, 3])).await;
    conn.send(json!([1, 2, 3])).await;
    conn.send(json!({"foo": "bar"})).await;
    conn.send(json!({"msg": "weird", "id": "1"})).await;
    conn.send(json!({"msg": "result"})).await;
    conn.send(json!({"msg": "result", "id": "999", "result": 1}))
        .await;
    conn.send(json!({"msg": "added", "id": "x"})).await;
    conn.send(json!({"msg": "ready", "subs": ["nope"]})).await;
    conn.send(json!({"msg": "nosub", "id": "nope"})).await;
    conn.send(json!({"msg": "error", "reason": "Bad request"}))
        .await;

    let c = client.clone();
    let call = tokio::spawn(async move { c.call("ok", vec![]).await });
    conn.answer_method("ok", json!(true)).await;
    assert_eq!(within(call).await.unwrap().unwrap(), json!(true));
    assert_eq!(
        client.state(),
        ConnectionState::Connected {
            session: "s1".into()
        }
    );
    client.close().await;
}

#[tokio::test]
async fn unsubscribe_and_drop_send_unsub() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let sub = subscribe(&client, &mut conn, "things", vec![]).await;
    let id = sub.id().to_owned();
    sub.unsubscribe();
    assert_eq!(conn.recv().await, json!({"msg": "unsub", "id": id}));

    let sub = subscribe(&client, &mut conn, "other", vec![]).await;
    let id = sub.id().to_owned();
    drop(sub);
    assert_eq!(conn.recv().await, json!({"msg": "unsub", "id": id}));

    // Unsubscribed while disconnected: not renewed after reconnect.
    let sub = subscribe(&client, &mut conn, "gone", vec![]).await;
    let keep = subscribe(&client, &mut conn, "kept", vec![]).await;
    let mut state = client.watch_state();
    conn.kill();
    wait_state(&mut state, |s| !s.is_connected()).await;
    drop(sub);
    let mut conn2 = server.accept_handshake("s2").await;
    let frames = conn2.collect_for(Duration::from_millis(300)).await;
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0]["msg"], "sub");
    assert_eq!(frames[0]["id"], keep.id());
    client.close().await;
}

#[tokio::test]
async fn reconnect_now_drops_live_connection() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    client.reconnect_now();
    conn.closed().await;
    let _conn2 = server.accept_handshake("s2").await;
    assert_eq!(wait_connected(&client).await, "s2");
    client.close().await;
}

#[tokio::test]
async fn close_stops_everything() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let mut sub = subscribe(&client, &mut conn, "things", vec![]).await;
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("pending", vec![]).await });
    conn.recv_msg("method").await;

    client.close().await;
    assert_eq!(client.state(), ConnectionState::Closed);
    conn.closed().await;
    assert!(matches!(
        within(call).await.unwrap(),
        Err(CallError::Closed)
    ));
    assert_eq!(within(sub.recv()).await.unwrap_err(), RecvError::Closed);
    assert_eq!(sub.state(), SubscriptionState::Closed);
    assert!(matches!(
        client.call("x", vec![]).await,
        Err(CallError::Closed)
    ));
    assert!(matches!(
        client.subscribe("x", vec![]).await,
        Err(SubscribeError::Closed)
    ));
    assert!(
        server
            .no_connection_within(Duration::from_millis(300))
            .await
    );
}

#[tokio::test]
async fn dropping_last_client_closes_connection() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let state = client.watch_state();
    drop(client);
    conn.closed().await;
    let mut state = state;
    wait_state(&mut state, |s| *s == ConnectionState::Closed).await;
}

#[tokio::test]
async fn spawn_connects_in_background() {
    let mut server = MockServer::start().await;
    let client = DdpClient::spawn(server.config());
    let _conn = server.accept_handshake("bg").await;
    assert_eq!(wait_connected(&client).await, "bg");
    client.close().await;
}

fn spawn_call(
    client: &DdpClient,
    method: &str,
) -> tokio::task::JoinHandle<Result<Value, CallError>> {
    let client = client.clone();
    let method = method.to_owned();
    tokio::spawn(async move { client.call(&method, vec![]).await })
}

async fn subscribe_with(
    client: &DdpClient,
    conn: Option<&mut MockConn>,
    name: &str,
    params: Vec<Value>,
    options: SubscribeOptions,
) -> Subscription {
    let task = {
        let client = client.clone();
        let name = name.to_owned();
        tokio::spawn(async move { client.subscribe_with(&name, params, options).await })
    };
    if let Some(conn) = conn {
        conn.answer_sub(name).await;
    }
    within(task).await.unwrap().unwrap()
}

fn login_hook(config: DdpConfig) -> DdpConfig {
    config.on_connected(|ddp| async move {
        ddp.call("login", vec![json!({"resume": "tok"})]).await?;
        Ok(())
    })
}

/// Catches: user calls sent during the hook, queued calls sent before subs, and
/// subs re-sent more than once.
#[tokio::test]
async fn reconnect_order_hook_then_subs_then_queued_calls() {
    let mut server = MockServer::start().await;
    let config = login_hook(server.config());
    let (client, mut conn) = tokio::join!(DdpClient::connect(config), async {
        let mut conn = server.accept_handshake("s1").await;
        conn.answer_method("login", json!({})).await;
        conn
    });
    let client = client.unwrap();
    let a = subscribe(&client, &mut conn, "a", vec![]).await;
    let b = subscribe(&client, &mut conn, "b", vec![json!(1)]).await;
    let mut state = client.watch_state();
    conn.kill();
    wait_state(&mut state, |s| !s.is_connected()).await;

    let early = spawn_call(&client, "early");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut conn2 = server.accept_handshake("s2").await;
    let login = conn2.recv().await;
    assert_eq!(login["method"], "login", "{login}");

    // While the hook is running, nothing else may be sent.
    let during = spawn_call(&client, "during");
    let frames = conn2.collect_for(Duration::from_millis(300)).await;
    assert!(frames.is_empty(), "sent during hook: {frames:?}");
    conn2
        .send(json!({"msg": "result", "id": login["id"], "result": {}}))
        .await;

    let frames = conn2.collect_for(Duration::from_millis(500)).await;
    assert_eq!(frames.len(), 4, "{frames:?}");
    let mut sub_ids: Vec<_> = frames[..2]
        .iter()
        .map(|f| {
            assert_eq!(f["msg"], "sub", "{f}");
            f["id"].as_str().unwrap().to_owned()
        })
        .collect();
    sub_ids.sort();
    let mut expected = vec![a.id().to_owned(), b.id().to_owned()];
    expected.sort();
    assert_eq!(sub_ids, expected);
    assert_eq!(frames[2]["method"], "early");
    assert_eq!(frames[3]["method"], "during");
    for f in &frames[2..] {
        conn2
            .send(json!({"msg": "result", "id": f["id"], "result": f["method"]}))
            .await;
    }
    assert_eq!(within(early).await.unwrap().unwrap(), "early");
    assert_eq!(within(during).await.unwrap().unwrap(), "during");
    client.close().await;
}

#[tokio::test]
async fn timed_out_queued_call_is_not_sent() {
    let mut server = MockServer::start().await;
    let (client, conn) = connect_default(&mut server).await;
    let mut state = client.watch_state();
    conn.kill();
    wait_state(&mut state, |s| !s.is_connected()).await;
    let stale = client
        .call_with_timeout("stale", vec![], Duration::from_millis(100))
        .await;
    assert!(matches!(stale, Err(CallError::Timeout)), "{stale:?}");
    let mut conn2 = server.accept_handshake("s2").await;
    let fresh = spawn_call(&client, "fresh");
    let req = conn2.recv().await;
    assert_eq!(req["method"], "fresh", "{req}");
    conn2
        .send(json!({"msg": "result", "id": req["id"], "result": 1}))
        .await;
    assert_eq!(within(fresh).await.unwrap().unwrap(), 1);
    client.close().await;
}

#[tokio::test]
async fn late_result_after_timeout_is_ignored() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let c = client.clone();
    let slow = tokio::spawn(async move {
        c.call_with_timeout("slow", vec![], Duration::from_millis(100))
            .await
    });
    let req = conn.recv_msg("method").await;
    assert!(matches!(
        within(slow).await.unwrap(),
        Err(CallError::Timeout)
    ));
    conn.send(json!({"msg": "result", "id": req["id"], "result": 1}))
        .await;
    let next = spawn_call(&client, "next");
    conn.answer_method("next", json!(2)).await;
    assert_eq!(within(next).await.unwrap().unwrap(), 2);
    client.close().await;
}

#[tokio::test]
async fn backoff_resets_after_stable_connection() {
    let mut server = MockServer::start().await;
    let mut config = server.config();
    config.backoff.stable_after = Duration::from_millis(300);
    let (client, conn) = connect(&mut server, config).await;
    let mut state = client.watch_state();
    let reconnecting = |attempt: u32| move |s: &ConnectionState| matches!(s, ConnectionState::Reconnecting { attempt: a, .. } if *a == attempt);

    // Unstable connections: the attempt counter grows.
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.kill();
    wait_state(&mut state, reconnecting(1)).await;
    let conn = server.accept_handshake("s2").await;
    wait_state(&mut state, ConnectionState::is_connected).await;
    conn.kill();
    wait_state(&mut state, reconnecting(2)).await;

    // A connection that stays up for `stable_after` resets it.
    let conn = server.accept_handshake("s3").await;
    wait_state(&mut state, ConnectionState::is_connected).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.kill();
    wait_state(&mut state, reconnecting(1)).await;
    client.close().await;
}

#[tokio::test]
async fn presence_and_explicit_routing() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;

    // Rocket.Chat presence: empty first param, eventName is the user id.
    let presence = "stream-user-presence";
    let mut p = subscribe(
        &client,
        &mut conn,
        presence,
        vec![json!(""), json!({"added": ["u1", "u2"]})],
    )
    .await;
    conn.stream_event(presence, "u1", json!([["alice", 1]]))
        .await;
    conn.stream_event(presence, "u2", json!([["bob", 2]])).await;
    assert_eq!(within(p.recv()).await.unwrap().event_name(), Some("u1"));
    assert_eq!(within(p.recv()).await.unwrap().event_name(), Some("u2"));

    let nu = "stream-notify-user";
    let prefix = SubscribeOptions {
        routing: Routing::EventNamePrefix("u1/".into()),
        ..SubscribeOptions::default()
    };
    let mut by_prefix = subscribe_with(
        &client,
        Some(&mut conn),
        nu,
        stream_params("u1/message"),
        prefix,
    )
    .await;
    let all = SubscribeOptions {
        routing: Routing::Collection,
        ..SubscribeOptions::default()
    };
    let mut by_collection = subscribe_with(
        &client,
        Some(&mut conn),
        nu,
        stream_params("u1/rooms-changed"),
        all,
    )
    .await;
    let exact = SubscribeOptions {
        routing: Routing::EventName("u1/x".into()),
        ..SubscribeOptions::default()
    };
    let mut by_name =
        subscribe_with(&client, Some(&mut conn), nu, stream_params("u1/x"), exact).await;

    conn.send(json!({"msg": "changed", "collection": nu, "id": "id", "fields": {}}))
        .await;
    conn.stream_event(nu, "u1/message", json!([1])).await;
    conn.stream_event(nu, "u2/message", json!([2])).await;
    conn.stream_event(nu, "u1/rooms-changed", json!([3])).await;
    conn.stream_event(nu, "u1/x", json!([4])).await;

    let next = async |s: &mut Subscription| {
        within(s.recv())
            .await
            .unwrap()
            .event_name()
            .map(str::to_owned)
    };
    assert_eq!(next(&mut by_prefix).await.as_deref(), Some("u1/message"));
    assert_eq!(
        next(&mut by_prefix).await.as_deref(),
        Some("u1/rooms-changed")
    );
    assert_eq!(next(&mut by_prefix).await.as_deref(), Some("u1/x"));
    assert_eq!(next(&mut by_collection).await, None);
    for expected in ["u1/message", "u2/message", "u1/rooms-changed", "u1/x"] {
        assert_eq!(next(&mut by_collection).await.as_deref(), Some(expected));
    }
    assert_eq!(next(&mut by_name).await.as_deref(), Some("u1/x"));
    client.close().await;
}

#[tokio::test]
async fn identical_subscriptions_share_one_server_sub() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let rm = "stream-room-messages";

    // Two concurrent identical subscribes: one `sub`, both resolve on `ready`.
    let t1 = spawn_sub(&client, rm, stream_params("GENERAL"));
    let t2 = spawn_sub(&client, rm, stream_params("GENERAL"));
    let req = conn.recv_msg("sub").await;
    assert!(
        conn.collect_for(Duration::from_millis(200))
            .await
            .is_empty()
    );
    conn.send(json!({"msg": "ready", "subs": [req["id"]]}))
        .await;
    let mut a = within(t1).await.unwrap().unwrap();
    let mut b = within(t2).await.unwrap().unwrap();
    assert_eq!(a.id(), req["id"]);
    assert_eq!(b.id(), req["id"]);

    // Joining a ready subscription resolves at once without a new `sub`.
    let mut c = within(client.subscribe(rm, stream_params("GENERAL")))
        .await
        .unwrap();
    assert_eq!(c.state(), SubscriptionState::Ready);

    conn.stream_event(rm, "GENERAL", json!([{"msg": "1"}]))
        .await;
    conn.stream_event(rm, "GENERAL", json!([{"msg": "2"}]))
        .await;
    for sub in [&mut a, &mut b, &mut c] {
        assert_eq!(first_arg(&within(sub.recv()).await.unwrap())["msg"], "1");
        assert_eq!(first_arg(&within(sub.recv()).await.unwrap())["msg"], "2");
    }

    // `unsub` only when the last handle goes away.
    drop(a);
    drop(b);
    assert!(
        conn.collect_for(Duration::from_millis(200))
            .await
            .is_empty()
    );
    drop(c);
    assert_eq!(conn.recv().await, json!({"msg": "unsub", "id": req["id"]}));

    // A later `nosub` reaches every handle.
    let x1 = spawn_sub(&client, "x", vec![json!({"a": 1, "b": 2})]);
    let x2 = spawn_sub(&client, "x", vec![json!({"b": 2, "a": 1})]);
    let req = conn.answer_sub("x").await;
    let (mut x1, mut x2) = (
        within(x1).await.unwrap().unwrap(),
        within(x2).await.unwrap().unwrap(),
    );
    conn.send(json!({"msg": "nosub", "id": req["id"]})).await;
    for sub in [&mut x1, &mut x2] {
        assert_eq!(within(sub.recv()).await.unwrap_err(), RecvError::Closed);
        assert_eq!(sub.state(), SubscriptionState::Stopped(None));
    }
    client.close().await;
}

#[tokio::test]
async fn full_queue_reports_lag_and_keepalive_buffers_nothing() {
    let mut server = MockServer::start().await;
    let (client, mut conn) = connect_default(&mut server).await;
    let mut all = client.data_messages();
    let small = SubscribeOptions {
        buffer: Some(2),
        ..SubscribeOptions::default()
    };
    let mut sub = subscribe_with(&client, Some(&mut conn), "c", vec![], small).await;
    let keepalive = SubscribeOptions {
        buffer: Some(0),
        ..SubscribeOptions::default()
    };
    let mut keep = subscribe_with(&client, None, "c", vec![], keepalive).await;

    for i in 1..=5 {
        conn.send(json!({"msg": "added", "collection": "c", "id": i.to_string()}))
            .await;
    }
    for _ in 1..=5 {
        within(all.recv()).await.unwrap();
    }
    // Older queued messages first, then the gap, then newer messages.
    assert_eq!(within(sub.recv()).await.unwrap().id, "1");
    assert_eq!(within(sub.recv()).await.unwrap().id, "2");
    assert_eq!(within(sub.recv()).await.unwrap_err(), RecvError::Lagged(3));
    conn.send(json!({"msg": "added", "collection": "c", "id": "6"}))
        .await;
    assert_eq!(within(sub.next()).await.unwrap().unwrap().id, "6");

    // A second gap; drops while the marker is still queued add to it.
    for i in 7..=9 {
        conn.send(json!({"msg": "added", "collection": "c", "id": i.to_string()}))
            .await;
    }
    for _ in 6..=9 {
        within(all.recv()).await.unwrap();
    }
    assert_eq!(within(sub.recv()).await.unwrap().id, "7");
    conn.send(json!({"msg": "added", "collection": "c", "id": "10"}))
        .await;
    within(all.recv()).await.unwrap();
    assert_eq!(within(sub.recv()).await.unwrap().id, "8");
    assert_eq!(
        within(sub.next()).await.unwrap(),
        Err(meteor_ddp::Lagged(2))
    );
    conn.send(json!({"msg": "added", "collection": "c", "id": "11"}))
        .await;
    assert_eq!(within(sub.recv()).await.unwrap().id, "11");

    assert!(futures::FutureExt::now_or_never(keep.recv()).is_none());
    conn.send(json!({"msg": "nosub", "id": sub.id()})).await;
    assert_eq!(within(keep.recv()).await.unwrap_err(), RecvError::Closed);
    client.close().await;
}

#[tokio::test]
async fn huge_durations_do_not_overflow() {
    let mut server = MockServer::start().await;
    let mut config = server.config().heartbeat(HeartbeatConfig {
        interval: Duration::MAX,
        timeout: Duration::MAX,
    });
    config.connect_timeout = Duration::MAX;
    config.write_timeout = Duration::MAX;
    let (client, mut conn) = connect(&mut server, config).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let call = spawn_call(&client, "x");
    conn.answer_method("x", json!(1)).await;
    assert_eq!(within(call).await.unwrap().unwrap(), 1);
    assert!(client.state().is_connected());

    // Interval disabled but finite timeout, and vice versa.
    for (interval, timeout) in [
        (Duration::MAX, Duration::from_secs(1)),
        (Duration::from_millis(50), Duration::MAX),
    ] {
        let config = server
            .config()
            .heartbeat(HeartbeatConfig { interval, timeout });
        let (client, mut conn) = connect(&mut server, config).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let call = spawn_call(&client, "y");
        conn.answer_method("y", json!(2)).await;
        assert_eq!(within(call).await.unwrap().unwrap(), 2);
        client.close().await;
    }
    client.close().await;
}

#[tokio::test]
async fn slow_large_message_keeps_connection_alive() {
    use futures::SinkExt;
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let connect = ws.next().await.unwrap().unwrap();
        assert!(connect.to_text().unwrap().contains("connect"));
        ws.send(Message::text(
            json!({"msg": "connected", "session": "s1"}).to_string(),
        ))
        .await
        .unwrap();
        // One 60 KB text frame trickled at 1 KB / 50 ms (about 3 s).
        let pad = "a".repeat(60_000);
        let payload =
            json!({"msg": "added", "collection": "big", "id": "x", "fields": {"pad": pad}})
                .to_string();
        let mut frame = vec![0x81u8, 126];
        frame.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        let tcp = ws.get_mut();
        for chunk in frame.chunks(1024) {
            tcp.write_all(chunk).await.unwrap();
            tcp.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/websocket")).unwrap();
    let mut config = DdpConfig::new(url).heartbeat(HeartbeatConfig {
        interval: Duration::from_millis(200),
        timeout: Duration::from_millis(400),
    });
    config.connect_timeout = Duration::from_secs(5);
    let client = within(DdpClient::connect(config)).await.unwrap();
    let mut state = client.watch_state();
    state.borrow_and_update();
    let mut all = client.data_messages();
    let big = within(all.recv()).await.unwrap();
    assert_eq!(big.collection, "big");
    assert!(!state.has_changed().unwrap(), "{:?}", *state.borrow());
    assert!(client.state().is_connected());
    client.close().await;
    server.abort();
}

#[tokio::test]
async fn panicking_hook_is_retried() {
    let mut server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let config = server.config().on_connected(move |_ddp| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                panic!("hook bug");
            }
            Ok(())
        }
    });
    let client = DdpClient::spawn(config);
    let _conn1 = server.accept_handshake("s1").await;
    let _conn2 = server.accept_handshake("s2").await;
    assert_eq!(wait_connected(&client).await, "s2");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    client.close().await;
}

#[tokio::test]
async fn subscribe_in_failed_state_fails_fast() {
    let mut server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let config = server.config().on_connected(move |_ddp| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                Ok(())
            } else {
                Err(HookError::fatal("revoked"))
            }
        }
    });
    let (client, conn) = connect(&mut server, config).await;
    let mut state = client.watch_state();
    conn.kill();
    let _conn2 = server.accept_handshake("s2").await;
    wait_state(&mut state, |s| matches!(s, ConnectionState::Failed { .. })).await;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        client.subscribe_with_timeout("x", vec![], Duration::from_secs(30)),
    )
    .await
    .expect("subscribe must fail fast");
    assert!(
        matches!(result, Err(SubscribeError::Disconnected)),
        "{result:?}"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_flood_does_not_starve_calls() {
    use futures::SinkExt;

    let mut server = MockServer::start().await;
    let (client, conn) = connect_default(&mut server).await;
    let (mut tx, mut rx) = conn.into_ws().split();
    let pad = "x".repeat(4000);
    let nested: Vec<Value> = (0..50)
        .map(|_| json!({"a": 1, "b": [1, 2, 3, {"c": "d"}]}))
        .collect();
    let frame = json!({"msg": "changed", "collection": "flood", "id": "id",
        "fields": {"pad": pad, "nested": nested}})
    .to_string();
    let flood =
        tokio::spawn(async move { while tx.feed(Message::text(frame.clone())).await.is_ok() {} });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let c = client.clone();
    let call = tokio::spawn(async move { c.call("during-flood", vec![]).await });
    let seen = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(m)) = rx.next().await {
            if let Message::Text(t) = m
                && t.contains("during-flood")
            {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(seen, Ok(true), "call not sent during inbound flood");
    flood.abort();
    call.abort();
    client.close().await;
}
