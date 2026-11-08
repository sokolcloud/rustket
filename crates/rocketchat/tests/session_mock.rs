//! Session behaviour against an in-process mock Rocket.Chat (REST `me` and
//! `settings.public` plus DDP on the same port). Runs without a live server.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rocketchat::session::{
    BackoffConfig, ConnectionState, ResyncReason, Session, SessionEvent, SessionEvents,
    SessionOptions, global_subscriptions, room_subscriptions, stream_params,
};
use rocketchat::{Credentials, Error, ServerUrl, TlsConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_secs(10);
const UID: &str = "uid1";
const RID: &str = "room1";

/// Answer to a `sub`.
#[derive(Clone)]
enum SubReply {
    Ready,
    Nosub(Value),
    /// Never answer (the test may answer later with [`Mock::ready`]).
    Hold,
}

type SubPolicy = Box<dyn Fn(usize, &str, &str) -> SubReply + Send>;
type LoginPolicy = Box<dyn Fn(usize) -> Value + Send>;

struct State {
    sub: SubPolicy,
    login: LoginPolicy,
    real_name: bool,
    username: String,
    /// Drop every new socket (server unreachable).
    refuse: bool,
    /// `(connection index, client frame)`.
    log: Vec<(usize, Value)>,
    conns: Vec<mpsc::UnboundedSender<Option<Value>>>,
}

#[derive(Clone)]
struct Mock {
    port: u16,
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
}

fn login_ok(_: usize) -> Value {
    json!({"result": {"id": UID, "token": "t", "type": "resume"}})
}

impl Mock {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mock = Mock {
            port,
            state: Arc::new(Mutex::new(State {
                sub: Box::new(|_, _, _| SubReply::Ready),
                login: Box::new(login_ok),
                real_name: false,
                username: "alice".into(),
                refuse: false,
                log: Vec::new(),
                conns: Vec::new(),
            })),
            notify: Arc::new(Notify::new()),
        };
        let m = mock.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(m.clone().handle(stream));
            }
        });
        mock
    }

    fn server(&self) -> ServerUrl {
        ServerUrl::parse(&format!("http://127.0.0.1:{}", self.port)).unwrap()
    }

    fn set_sub(&self, f: impl Fn(usize, &str, &str) -> SubReply + Send + 'static) {
        self.state.lock().unwrap().sub = Box::new(f);
    }

    fn set_login(&self, f: impl Fn(usize) -> Value + Send + 'static) {
        self.state.lock().unwrap().login = Box::new(f);
    }

    async fn handle(self, mut stream: TcpStream) {
        let mut head = [0u8; 64];
        let n = stream.peek(&mut head).await.unwrap_or(0);
        let head = String::from_utf8_lossy(&head[..n]).to_string();
        if head.starts_with("GET /api/") {
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let body = if head.starts_with("GET /api/v1/me") {
                let username = self.state.lock().unwrap().username.clone();
                json!({"_id": UID, "username": username, "name": "Alice A", "success": true})
            } else {
                let real = self.state.lock().unwrap().real_name;
                json!({"settings": [{"_id": "UI_Use_Real_Name", "value": real}],
                       "count": 1, "offset": 0, "total": 1, "success": true})
            }
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
            return;
        }
        if self.state.lock().unwrap().refuse {
            return;
        }
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let idx = {
            let mut st = self.state.lock().unwrap();
            st.conns.push(cmd_tx);
            st.conns.len() - 1
        };
        let send = |v: Value| Message::text(v.to_string());
        if ws.send(send(json!({"server_id": "0"}))).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(Some(v)) => { let _ = ws.send(send(v)).await; }
                    _ => return, // kill: drop the socket
                },
                frame = ws.next() => {
                    let Some(Ok(Message::Text(t))) = frame else {
                        if matches!(frame, None | Some(Err(_)) | Some(Ok(Message::Close(_)))) {
                            self.state.lock().unwrap().log.push((idx, json!({"msg": "closed"})));
                            self.notify.notify_waiters();
                            return;
                        }
                        continue;
                    };
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let reply = {
                        let mut st = self.state.lock().unwrap();
                        st.log.push((idx, v.clone()));
                        match v["msg"].as_str().unwrap_or_default() {
                            "connect" => Some(json!({"msg": "connected", "session": format!("s{idx}")})),
                            "ping" => Some(json!({"msg": "pong", "id": v["id"]})),
                            "method" if v["method"] == "login" => {
                                let mut r = (st.login)(idx);
                                r["msg"] = json!("result");
                                r["id"] = v["id"].clone();
                                Some(r)
                            }
                            "method" => Some(json!({"msg": "result", "id": v["id"], "result": null})),
                            "sub" => {
                                let name = v["name"].as_str().unwrap_or_default();
                                let event = v["params"][0].as_str().unwrap_or_default();
                                match (st.sub)(idx, name, event) {
                                    SubReply::Ready => Some(json!({"msg": "ready", "subs": [v["id"]]})),
                                    SubReply::Nosub(e) => Some(json!({"msg": "nosub", "id": v["id"], "error": e})),
                                    SubReply::Hold => None,
                                }
                            }
                            _ => None,
                        }
                    };
                    self.notify.notify_waiters();
                    if let Some(r) = reply && ws.send(send(r)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    fn frames(&self, pred: impl Fn(usize, &Value) -> bool) -> Vec<(usize, Value)> {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|(i, v)| pred(*i, v))
            .cloned()
            .collect()
    }

    /// Waits until at least `n` logged frames match.
    async fn wait_frames(
        &self,
        n: usize,
        pred: impl Fn(usize, &Value) -> bool,
    ) -> Vec<(usize, Value)> {
        tokio::time::timeout(WAIT, async {
            loop {
                let notified = self.notify.notified();
                let got = self.frames(&pred);
                if got.len() >= n {
                    return got;
                }
                let _ = tokio::time::timeout(Duration::from_millis(50), notified).await;
            }
        })
        .await
        .expect("timed out waiting for client frames")
    }

    fn conn_count(&self) -> usize {
        self.state.lock().unwrap().conns.len()
    }

    fn send(&self, v: Value) {
        let st = self.state.lock().unwrap();
        let _ = st.conns.last().unwrap().send(Some(v));
    }

    fn kill(&self) {
        let st = self.state.lock().unwrap();
        let _ = st.conns.last().unwrap().send(None);
    }

    fn ready(&self, sub_id: &Value) {
        self.send(json!({"msg": "ready", "subs": [sub_id]}));
    }

    fn event(&self, collection: &str, event: &str, args: Value) {
        self.send(
            json!({"msg": "changed", "collection": collection, "id": "id",
                         "fields": {"eventName": event, "args": args}}),
        );
    }
}

fn is_sub(event: &'static str) -> impl Fn(usize, &Value) -> bool {
    move |_, v| v["msg"] == "sub" && v["params"][0] == event
}

fn options() -> SessionOptions {
    SessionOptions {
        backoff: BackoffConfig {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            ..BackoffConfig::default()
        },
        subscribe_timeout: Duration::from_secs(3),
        resync_wait: Duration::from_secs(3),
        stream_retry_min: Duration::from_millis(50),
        stream_retry_max: Duration::from_millis(200),
        ..SessionOptions::default()
    }
}

async fn start_with(mock: &Mock, opts: SessionOptions) -> Result<(Session, SessionEvents), Error> {
    let tls = TlsConfig::new().build().unwrap();
    tokio::time::timeout(
        WAIT,
        Session::start(mock.server(), &tls, Credentials::new(UID, "tok"), opts),
    )
    .await
    .expect("start timed out")
}

async fn start(mock: &Mock) -> (Session, SessionEvents) {
    let (s, mut e) = start_with(mock, options()).await.unwrap();
    expect(&mut e, |e| matches!(e, SessionEvent::Ready { .. })).await;
    (s, e)
}

async fn expect(events: &mut SessionEvents, pred: impl Fn(&SessionEvent) -> bool) -> SessionEvent {
    tokio::time::timeout(WAIT, async {
        loop {
            match events.recv().await {
                Some(e) if pred(&e) => return e,
                Some(_) => {}
                None => panic!("event stream ended"),
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

async fn collect_for(events: &mut SessionEvents, window: Duration) -> Vec<SessionEvent> {
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    while let Ok(Some(e)) = tokio::time::timeout_at(deadline, events.recv()).await {
        out.push(e);
    }
    out
}

async fn drain_until_end(events: &mut SessionEvents) -> Vec<SessionEvent> {
    tokio::time::timeout(WAIT, async {
        let mut out = Vec::new();
        while let Some(e) = events.recv().await {
            out.push(e);
        }
        out
    })
    .await
    .expect("event stream did not end")
}

fn is_resync(e: &SessionEvent) -> bool {
    matches!(e, SessionEvent::Resync { .. })
}

#[tokio::test]
async fn subscribes_every_stream_with_exact_params() {
    let mock = Mock::start().await;
    let (session, _events) = start(&mock).await;

    let subs: BTreeSet<String> = mock
        .frames(|i, v| i == 0 && v["msg"] == "sub")
        .into_iter()
        .map(|(_, v)| format!("{}|{}", v["name"].as_str().unwrap(), v["params"]))
        .collect();
    let golden: BTreeSet<String> =
        ["stream-room-messages|[\"__my_messages__\",{\"args\":[],\"useCollection\":false}]"]
            .into_iter()
            .map(str::to_owned)
            .chain(
                [
                    "message",
                    "notification",
                    "rooms-changed",
                    "subscriptions-changed",
                    "uiInteraction",
                    "e2ekeyRequest",
                    "userData",
                    "video-conference",
                    "force_logout",
                    "calendar",
                    "banners",
                    "webdav",
                ]
                .iter()
                .map(|e| {
                    format!(
                        "stream-notify-user|[\"{UID}/{e}\",{{\"args\":[],\"useCollection\":false}}]"
                    )
                }),
            )
            .chain(
                [
                    "user-status",
                    "Users:NameChanged",
                    "Users:Deleted",
                    "updateAvatar",
                    "updateEmojiCustom",
                    "deleteEmojiCustom",
                    "roles-change",
                    "permissions-changed",
                    "private-settings-changed",
                    "updateCustomUserStatus",
                    "deleteCustomUserStatus",
                    "banner-changed",
                    "new-banner",
                ]
                .iter()
                .map(|e| {
                    format!(
                        "stream-notify-logged|[\"{e}\",{{\"args\":[],\"useCollection\":false}}]"
                    )
                }),
            )
            .chain(
                [
                    "public-settings-changed",
                    "updateCustomSound",
                    "deleteCustomSound",
                ]
                .iter()
                .map(|e| {
                    format!("stream-notify-all|[\"{e}\",{{\"args\":[],\"useCollection\":false}}]")
                }),
            )
            .chain(["stream-roles|[\"roles\",{\"args\":[],\"useCollection\":false}]".to_owned()])
            .collect();
    assert_eq!(subs, golden);
    assert_eq!(global_subscriptions(UID).len(), golden.len());

    // Room streams, then unsub on drop.
    let watch = session.watch_room(RID).await.unwrap();
    assert!(watch.missing().is_empty());
    let room: Vec<(usize, Value)> =
        mock.frames(|_, v| v["msg"] == "sub" && v["name"] == "stream-notify-room");
    let got: BTreeSet<String> = room.iter().map(|(_, v)| v["params"].to_string()).collect();
    let want: BTreeSet<String> = [
        "deleteMessage",
        "deleteMessageBulk",
        "user-activity",
        "messagesRead",
        "videoconf",
        "messagesImported",
        "e2e.keyRequest",
    ]
    .iter()
    .map(|e| serde_json::to_string(&stream_params(&format!("{RID}/{e}"))).unwrap())
    .collect();
    assert_eq!(got, want);
    assert_eq!(room_subscriptions(RID, false).len(), want.len());
    assert_eq!(room_subscriptions(RID, true).len(), want.len() + 1);
    let ids: BTreeSet<String> = room.iter().map(|(_, v)| v["id"].to_string()).collect();
    drop(watch);
    let unsubs = mock
        .wait_frames(ids.len(), |_, v| v["msg"] == "unsub")
        .await;
    let unsub_ids: BTreeSet<String> = unsubs.iter().map(|(_, v)| v["id"].to_string()).collect();
    assert_eq!(unsub_ids, ids);
    session.close().await;
}

#[tokio::test]
async fn start_fails_without_a_required_stream() {
    let mock = Mock::start().await;
    mock.set_sub(|_, _, event| {
        if event == "__my_messages__" {
            SubReply::Nosub(json!({"error": "too-many-requests", "reason": "slow down",
                                   "details": {"timeToReset": 5000}}))
        } else {
            SubReply::Ready
        }
    });
    match start_with(&mock, options()).await {
        Err(Error::RateLimited { retry_after, .. }) => {
            assert_eq!(retry_after, Some(Duration::from_millis(5000)));
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
    // The connection is closed again.
    mock.wait_frames(1, |_, v| v["msg"] == "closed").await;

    // A stream that never answers fails start too (after subscribe_timeout).
    let mock = Mock::start().await;
    mock.set_sub(|_, _, event| {
        if event == format!("{UID}/subscriptions-changed") {
            SubReply::Hold
        } else {
            SubReply::Ready
        }
    });
    let opts = SessionOptions {
        subscribe_timeout: Duration::from_millis(500),
        ..options()
    };
    let err = start_with(&mock, opts).await.expect_err("start succeeded");
    assert!(matches!(err, Error::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn refused_optional_stream_is_accepted_silently() {
    let mock = Mock::start().await;
    mock.set_sub(|_, _, event| {
        if event == "private-settings-changed" {
            SubReply::Nosub(json!({"error": "not-allowed", "reason": "Not allowed"}))
        } else {
            SubReply::Ready
        }
    });
    let (session, mut events) = start(&mock).await;
    let got = collect_for(&mut events, Duration::from_millis(500)).await;
    assert!(
        !got.iter()
            .any(|e| matches!(e, SessionEvent::Degraded { .. })),
        "{got:?}"
    );
    assert!(session.degraded_streams().is_empty());
    // Not retried.
    assert_eq!(
        mock.frames(|_, v| is_sub("private-settings-changed")(0, v))
            .len(),
        1
    );
    session.close().await;
}

#[tokio::test]
async fn resync_after_reconnect_waits_for_renewed_streams() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    // On the second connection, hold `user-status` until the test releases it.
    mock.set_sub(|conn, _, event| {
        if conn >= 1 && event == "user-status" {
            SubReply::Hold
        } else {
            SubReply::Ready
        }
    });
    mock.kill();
    expect(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Connection(ConnectionState::Reconnecting { .. })
        )
    })
    .await;
    expect(&mut events, |e| {
        matches!(e, SessionEvent::Connection(ConnectionState::Connected))
    })
    .await;
    let held = mock
        .wait_frames(1, |i, v| i == 1 && is_sub("user-status")(i, v))
        .await;
    let early = collect_for(&mut events, Duration::from_millis(700)).await;
    assert!(
        !early.iter().any(is_resync),
        "resync before renewal: {early:?}"
    );
    mock.ready(&held[0].1["id"]);
    let ev = expect(&mut events, is_resync).await;
    assert_eq!(
        ev,
        SessionEvent::Resync {
            reason: ResyncReason::Reconnected
        }
    );
    session.close().await;
}

#[tokio::test]
async fn resync_timeout_emits_again_when_streams_are_ready() {
    let mock = Mock::start().await;
    let opts = SessionOptions {
        resync_wait: Duration::from_millis(400),
        ..options()
    };
    let (session, mut events) = start_with(&mock, opts).await.unwrap();
    mock.set_sub(|conn, _, event| {
        if conn >= 1 && event == "user-status" {
            SubReply::Hold
        } else {
            SubReply::Ready
        }
    });
    mock.kill();
    expect(&mut events, |e| {
        matches!(e, SessionEvent::Connection(ConnectionState::Connected))
    })
    .await;
    // That was start()'s Connected; now the reconnect.
    expect(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Connection(ConnectionState::Reconnecting { .. })
        )
    })
    .await;
    expect(&mut events, |e| {
        matches!(e, SessionEvent::Connection(ConnectionState::Connected))
    })
    .await;
    let t0 = tokio::time::Instant::now();
    expect(&mut events, is_resync).await;
    assert!(
        t0.elapsed() >= Duration::from_millis(300),
        "{:?}",
        t0.elapsed()
    );
    let held = mock
        .wait_frames(1, |i, v| i == 1 && is_sub("user-status")(i, v))
        .await;
    mock.ready(&held[0].1["id"]);
    expect(&mut events, is_resync).await;
    session.close().await;
}

#[tokio::test]
async fn stopped_stream_is_resubscribed_and_reported() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    let first = mock.wait_frames(1, is_sub("__my_messages__")).await;
    // The server stops the subscription (rate limiter).
    mock.send(json!({"msg": "nosub", "id": first[0].1["id"],
                     "error": {"error": "too-many-requests", "details": {"timeToReset": 100}}}));
    let ev = expect(&mut events, |e| matches!(e, SessionEvent::Degraded { .. })).await;
    assert_eq!(
        ev,
        SessionEvent::Degraded {
            streams: vec!["stream-room-messages:__my_messages__".into()]
        }
    );
    // Re-subscribed after timeToReset, then healthy again plus a resync.
    mock.wait_frames(2, is_sub("__my_messages__")).await;
    expect(
        &mut events,
        |e| matches!(e, SessionEvent::Degraded { streams } if streams.is_empty()),
    )
    .await;
    assert_eq!(
        expect(&mut events, is_resync).await,
        SessionEvent::Resync {
            reason: ResyncReason::StreamsRestored
        }
    );
    assert!(session.degraded_streams().is_empty());
    // Events flow through the new subscription.
    mock.event(
        "stream-room-messages",
        "__my_messages__",
        json!([{"_id": "m1", "rid": RID, "msg": "hi"}, {"roomParticipant": true}]),
    );
    expect(&mut events, |e| {
        matches!(e, SessionEvent::Message { message, room_participant: true } if message.id == "m1")
    })
    .await;
    session.close().await;
}

#[tokio::test]
async fn watch_room_partial_and_total_failure() {
    let mock = Mock::start().await;
    let (session, _events) = start(&mock).await;
    let allow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let allow2 = allow.clone();
    mock.set_sub(move |_, _, event| {
        if event.ends_with("/messagesRead") && !allow2.load(std::sync::atomic::Ordering::SeqCst) {
            SubReply::Nosub(json!({"error": "error-not-allowed"}))
        } else {
            SubReply::Ready
        }
    });
    let watch = session.watch_room(RID).await.unwrap();
    assert_eq!(
        watch.missing(),
        vec![format!("stream-notify-room:{RID}/messagesRead")]
    );
    // Retried in the background until it succeeds.
    mock.wait_frames(2, |_, v| {
        v["msg"] == "sub" && v["params"][0] == format!("{RID}/messagesRead")
    })
    .await;
    allow.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(WAIT, async {
        while !watch.missing().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("room stream not restored");
    drop(watch);

    mock.set_sub(|_, name, _| {
        if name == "stream-notify-room" {
            SubReply::Nosub(json!({"error": "error-not-allowed", "reason": "Not allowed"}))
        } else {
            SubReply::Ready
        }
    });
    let err = session
        .watch_room("other")
        .await
        .expect_err("watch succeeded");
    assert!(matches!(err, Error::Method(_)), "{err:?}");
    session.close().await;
}

#[tokio::test]
async fn logged_out_is_the_last_event() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    mock.set_login(|conn| {
        if conn >= 1 {
            json!({"error": {"error": 403, "reason": "You've been logged out by the server"}})
        } else {
            login_ok(conn)
        }
    });
    for i in 0..20 {
        mock.event(
            "stream-notify-room",
            &format!("{RID}/deleteMessage"),
            json!([{"_id": format!("d{i}")}]),
        );
    }
    mock.kill();
    let rest = drain_until_end(&mut events).await;
    assert!(
        matches!(rest.last(), Some(SessionEvent::LoggedOut { reason }) if reason.contains("403")
            || reason.contains("logged out")),
        "{rest:?}"
    );
    assert_eq!(session.connection_state(), ConnectionState::Closed);
    // No reconnect attempts afterwards.
    let conns = mock.conn_count();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(mock.conn_count(), conns);
}

#[tokio::test]
async fn token_revoked_during_start_is_unauthorized() {
    let mock = Mock::start().await;
    mock.set_sub(|conn, _, _| {
        if conn == 0 {
            SubReply::Hold
        } else {
            SubReply::Ready
        }
    });
    mock.set_login(|conn| {
        if conn >= 1 {
            json!({"error": {"error": 403, "reason": "You've been logged out by the server"}})
        } else {
            login_ok(conn)
        }
    });
    let m = mock.clone();
    tokio::spawn(async move {
        m.wait_frames(1, |_, v| v["msg"] == "sub").await;
        m.kill();
    });
    let err = start_with(&mock, options())
        .await
        .expect_err("start succeeded");
    assert!(err.is_unauthorized(), "{err:?}");
}

#[tokio::test]
async fn user_activity_identity() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    session
        .set_user_activity(RID, None, &["user-typing"])
        .await
        .unwrap();
    session
        .set_user_activity(RID, Some("t1"), &[])
        .await
        .unwrap();
    let calls = mock
        .wait_frames(2, |_, v| v["method"] == "stream-notify-room")
        .await;
    assert_eq!(
        calls[0].1["params"],
        json!([format!("{RID}/user-activity"), "alice", ["user-typing"], {}])
    );
    assert_eq!(
        calls[1].1["params"],
        json!([format!("{RID}/user-activity"), "alice", [], {"tmid": "t1"}])
    );

    // A username change of the own user is picked up.
    mock.event(
        "stream-notify-user",
        &format!("{UID}/userData"),
        json!([{"id": UID, "type": "updated", "diff": {"username": "alice2"}}]),
    );
    expect(&mut events, |e| matches!(e, SessionEvent::UserData(_))).await;
    assert_eq!(session.me().unwrap().username.as_deref(), Some("alice2"));
    session
        .set_user_activity(RID, None, &["user-typing"])
        .await
        .unwrap();
    let calls = mock
        .wait_frames(3, |_, v| v["method"] == "stream-notify-room")
        .await;
    assert_eq!(calls[2].1["params"][1], "alice2");

    // UI_Use_Real_Name switched on: the real name is published.
    mock.event(
        "stream-notify-all",
        "public-settings-changed",
        json!(["updated", {"_id": "UI_Use_Real_Name", "value": true}]),
    );
    expect(&mut events, |e| {
        matches!(e, SessionEvent::SettingChanged { .. })
    })
    .await;
    session
        .set_user_activity(RID, None, &["user-typing"])
        .await
        .unwrap();
    let calls = mock
        .wait_frames(4, |_, v| v["method"] == "stream-notify-room")
        .await;
    assert_eq!(calls[3].1["params"][1], "Alice A");
    session.close().await;

    // Read at start.
    let mock = Mock::start().await;
    mock.state.lock().unwrap().real_name = true;
    let (session, _events) = start(&mock).await;
    session
        .set_user_activity(RID, None, &["user-typing"])
        .await
        .unwrap();
    let calls = mock
        .wait_frames(1, |_, v| v["method"] == "stream-notify-room")
        .await;
    assert_eq!(calls[0].1["params"][1], "Alice A");
}

#[tokio::test]
async fn dropping_and_closing_end_everything() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    let _watch = session.watch_room(RID).await.unwrap();
    drop(session);
    let rest = drain_until_end(&mut events).await;
    assert_eq!(
        rest.last(),
        Some(&SessionEvent::Connection(ConnectionState::Closed)),
        "{rest:?}"
    );
    // The socket is closed even though the RoomWatch is still alive.
    mock.wait_frames(1, |_, v| v["msg"] == "closed").await;

    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    session.close().await;
    let rest = drain_until_end(&mut events).await;
    assert_eq!(
        rest.last(),
        Some(&SessionEvent::Connection(ConnectionState::Closed))
    );
    assert_eq!(
        rest.iter()
            .filter(|e| matches!(e, SessionEvent::Connection(ConnectionState::Closed)))
            .count(),
        1
    );
}

#[tokio::test]
async fn non_participant_messages_are_flagged() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    mock.event(
        "stream-room-messages",
        "__my_messages__",
        json!([{"_id": "p1", "rid": "pub", "msg": "x"}, {"roomParticipant": false, "roomType": "c"}]),
    );
    let ev = expect(&mut events, |e| matches!(e, SessionEvent::Message { .. })).await;
    assert!(matches!(
        ev,
        SessionEvent::Message {
            room_participant: false,
            ..
        }
    ));
    session.close().await;
}

fn set_refuse(mock: &Mock, refuse: bool) {
    mock.state.lock().unwrap().refuse = refuse;
}

#[tokio::test]
async fn watch_room_offline_subscribes_when_back() {
    let mock = Mock::start().await;
    let opts = SessionOptions {
        subscribe_timeout: Duration::from_millis(500),
        // A retry backoff would show up as a long delay below.
        stream_retry_min: Duration::from_secs(30),
        stream_retry_max: Duration::from_secs(60),
        ..options()
    };
    let (session, mut events) = start_with(&mock, opts).await.unwrap();
    set_refuse(&mock, true);
    mock.kill();
    expect(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Connection(ConnectionState::Reconnecting { .. })
        )
    })
    .await;
    let t0 = tokio::time::Instant::now();
    let watch = session.watch_room(RID).await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert_eq!(watch.missing().len(), room_subscriptions(RID, false).len());
    // Offline for several subscribe timeouts: nothing is reported as degraded.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(session.degraded_streams().is_empty());
    set_refuse(&mock, false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !watch.missing().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("room streams not subscribed after reconnect");
    let ev = expect(&mut events, is_resync).await;
    assert_eq!(
        ev,
        SessionEvent::Resync {
            reason: ResyncReason::Reconnected
        }
    );
    let got = collect_for(&mut events, Duration::from_millis(300)).await;
    assert!(
        !got.iter()
            .any(|e| matches!(e, SessionEvent::Degraded { .. })),
        "{got:?}"
    );
    session.close().await;
}

#[tokio::test]
async fn failed_watch_room_reports_nothing() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    mock.set_sub(|_, name, _| {
        if name == "stream-notify-room" {
            SubReply::Nosub(json!({"error": "error-not-allowed"}))
        } else {
            SubReply::Ready
        }
    });
    session.watch_room(RID).await.expect_err("watch succeeded");
    let got = collect_for(&mut events, Duration::from_millis(500)).await;
    assert!(
        !got.iter()
            .any(|e| matches!(e, SessionEvent::Degraded { .. }) || is_resync(e)),
        "{got:?}"
    );

    // Dropping a watch with a down stream is not a restoration either.
    mock.set_sub(|_, _, event| {
        if event.ends_with("/messagesRead") {
            SubReply::Nosub(json!({"error": "error-not-allowed"}))
        } else {
            SubReply::Ready
        }
    });
    let watch = session.watch_room(RID).await.unwrap();
    expect(
        &mut events,
        |e| matches!(e, SessionEvent::Degraded { streams } if streams.len() == 1),
    )
    .await;
    drop(watch);
    let got = collect_for(&mut events, Duration::from_millis(500)).await;
    assert!(!got.iter().any(is_resync), "{got:?}");
    session.close().await;
}

#[tokio::test]
async fn restoration_resyncs_while_another_stream_stays_down() {
    let mock = Mock::start().await;
    let opts = SessionOptions {
        resync_wait: Duration::from_millis(300),
        ..options()
    };
    let (session, mut events) = start_with(&mock, opts).await.unwrap();
    // `webdav` is refused for good from now on; `calendar` only once.
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once2 = once.clone();
    mock.set_sub(move |_, _, event| {
        let limited = json!({"error": "too-many-requests", "details": {"timeToReset": 50}});
        let refused = event.ends_with("/webdav")
            || (event.ends_with("/calendar")
                && !once2.swap(true, std::sync::atomic::Ordering::SeqCst));
        if refused {
            SubReply::Nosub(limited)
        } else {
            SubReply::Ready
        }
    });
    let stop = |mock: &Mock, event: &str| {
        let ev = format!("{UID}/{event}");
        let sub = mock
            .frames(|_, v| v["msg"] == "sub" && v["params"][0] == ev.as_str())
            .pop()
            .unwrap();
        mock.send(json!({"msg": "nosub", "id": sub.1["id"],
                         "error": {"error": "too-many-requests"}}));
    };
    stop(&mock, "webdav");
    expect(
        &mut events,
        |e| matches!(e, SessionEvent::Degraded { streams } if streams.len() == 1),
    )
    .await;
    for round in 0..2 {
        stop(&mock, "calendar");
        // calendar comes back (after one refusal in the first round).
        let ev = expect(&mut events, is_resync).await;
        assert_eq!(
            ev,
            SessionEvent::Resync {
                reason: ResyncReason::StreamsRestored
            },
            "round {round}"
        );
    }
    assert_eq!(
        session.degraded_streams(),
        vec![format!("stream-notify-user:{UID}/webdav")]
    );
    session.close().await;
}

#[tokio::test]
async fn identity_is_refreshed_after_reconnect() {
    let mock = Mock::start().await;
    let (session, mut events) = start(&mock).await;
    mock.state.lock().unwrap().username = "renamed".into();
    mock.kill();
    expect(&mut events, is_resync).await;
    tokio::time::timeout(WAIT, async {
        while session.me().unwrap().username.as_deref() != Some("renamed") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("me not refreshed");
    session
        .set_user_activity(RID, None, &["user-typing"])
        .await
        .unwrap();
    let calls = mock
        .wait_frames(1, |_, v| v["method"] == "stream-notify-room")
        .await;
    assert_eq!(calls[0].1["params"][1], "renamed");
    session.close().await;
}
