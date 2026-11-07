//! In-process mock DDP server.
#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use meteor_ddp::{BackoffConfig, ConnectionState, DdpClient, DdpConfig};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

pub const WAIT: Duration = Duration::from_secs(10);

pub struct MockServer {
    pub port: u16,
    conns: mpsc::UnboundedReceiver<MockConn>,
}

impl MockServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, conns) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                        let _ = tx.send(MockConn { ws });
                    }
                });
            }
        });
        Self { port, conns }
    }

    pub fn url(&self) -> url::Url {
        url::Url::parse(&format!("ws://127.0.0.1:{}/websocket", self.port)).unwrap()
    }

    /// Config with fast backoff and long heartbeat (no client pings during tests).
    pub fn config(&self) -> DdpConfig {
        let mut config = DdpConfig::new(self.url()).backoff(BackoffConfig {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            ..BackoffConfig::default()
        });
        config.connect_timeout = Duration::from_secs(5);
        config.call_timeout = WAIT;
        config.subscribe_timeout = WAIT;
        config
    }

    /// Next accepted WebSocket connection.
    pub async fn accept(&mut self) -> MockConn {
        tokio::time::timeout(WAIT, self.conns.recv())
            .await
            .expect("timed out waiting for a connection")
            .expect("listener stopped")
    }

    /// Whether a connection arrives within `within`.
    pub async fn no_connection_within(&mut self, within: Duration) -> bool {
        tokio::time::timeout(within, self.conns.recv())
            .await
            .is_err()
    }

    /// Accept and complete the handshake.
    pub async fn accept_handshake(&mut self, session: &str) -> MockConn {
        let mut conn = self.accept().await;
        conn.handshake(session).await;
        conn
    }
}

pub struct MockConn {
    ws: WebSocketStream<TcpStream>,
}

impl MockConn {
    pub async fn send(&mut self, value: Value) {
        self.ws
            .send(Message::text(value.to_string()))
            .await
            .unwrap();
    }

    pub async fn send_raw(&mut self, msg: Message) {
        self.ws.send(msg).await.unwrap();
    }

    /// Next JSON text message from the client; `None` when the connection closed.
    pub async fn try_recv(&mut self) -> Option<Value> {
        loop {
            let frame = tokio::time::timeout(WAIT, self.ws.next())
                .await
                .expect("timed out waiting for a client frame");
            match frame {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return None,
                Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(&t).unwrap()),
                Some(Ok(_)) => {}
            }
        }
    }

    /// Every JSON message received within `window` (stops early on close).
    pub async fn collect_for(&mut self, window: Duration) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + window;
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Err(_) => return out,
                Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return out,
                Ok(Some(Ok(Message::Text(t)))) => out.push(serde_json::from_str(&t).unwrap()),
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    pub async fn recv(&mut self) -> Value {
        self.try_recv().await.expect("connection closed")
    }

    /// Next message with the given `msg` type, skipping others.
    pub async fn recv_msg(&mut self, msg: &str) -> Value {
        loop {
            let v = self.recv().await;
            if v["msg"] == msg {
                return v;
            }
        }
    }

    /// Wait until the client closes the connection.
    pub async fn closed(&mut self) {
        while self.try_recv().await.is_some() {}
    }

    pub async fn handshake(&mut self, session: &str) {
        self.send(json!({"server_id": "0"})).await;
        let connect = self.recv().await;
        assert_eq!(connect["msg"], "connect");
        assert_eq!(connect["version"], "1");
        assert_eq!(connect["support"], json!(["1", "pre2", "pre1"]));
        self.send(json!({"msg": "connected", "session": session}))
            .await;
    }

    /// Expect a method call and answer it with `result`.
    pub async fn answer_method(&mut self, method: &str, result: Value) -> Value {
        let call = self.recv_msg("method").await;
        assert_eq!(call["method"], method, "unexpected call {call}");
        self.send(json!({"msg": "result", "id": call["id"], "result": result}))
            .await;
        call
    }

    /// Expect a sub and answer it with `ready`.
    pub async fn answer_sub(&mut self, name: &str) -> Value {
        let sub = self.recv_msg("sub").await;
        assert_eq!(sub["name"], name, "unexpected sub {sub}");
        self.send(json!({"msg": "ready", "subs": [sub["id"]]}))
            .await;
        sub
    }

    pub async fn stream_event(&mut self, collection: &str, event: &str, args: Value) {
        self.send(json!({
            "msg": "changed",
            "collection": collection,
            "id": "id",
            "fields": {"eventName": event, "args": args}
        }))
        .await;
    }

    pub fn into_ws(self) -> WebSocketStream<TcpStream> {
        self.ws
    }

    /// Drop the TCP connection without a close frame.
    pub fn kill(self) {
        drop(self);
    }

    pub async fn close_frame(mut self) {
        let _ = self.ws.close(None).await;
    }
}

pub async fn wait_state(
    rx: &mut watch::Receiver<ConnectionState>,
    pred: impl FnMut(&ConnectionState) -> bool,
) -> ConnectionState {
    tokio::time::timeout(WAIT, rx.wait_for(pred))
        .await
        .expect("timed out waiting for state")
        .expect("state channel closed")
        .clone()
}

pub async fn wait_connected(client: &DdpClient) -> String {
    let mut rx = client.watch_state();
    match wait_state(&mut rx, ConnectionState::is_connected).await {
        ConnectionState::Connected { session } => session,
        _ => unreachable!(),
    }
}

pub async fn within<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(WAIT, fut).await.expect("timed out")
}
