//! The single producer of [`SessionEvents`](super::SessionEvents).
//!
//! One task owns the consumer channel. It decodes the DDP data feed in order and
//! merges in control events (connection state, `Resync`, `Degraded`, the terminal
//! event) sent through an unbounded control queue. Stream events never wait: when
//! the channel is full they are dropped and a `Resync{Overflow}` is queued.
//! Control events are never dropped and keep their relative order; while any of
//! them waits for room, stream events are dropped too (covered by the queued
//! resync). After the terminal event nothing else is sent and the stream ends.

use std::collections::VecDeque;
use std::sync::Arc;

use meteor_ddp::DataMessage;
use tokio::sync::{broadcast, mpsc};

use super::events::{ConnectionState, ResyncReason, SessionEvent, decode_event, streams};

pub(super) enum Ctl {
    Event(SessionEvent),
    /// Stop after delivering everything queued plus this final event.
    Terminate(Option<SessionEvent>),
}

pub(super) fn is_session_stream(collection: &str) -> bool {
    matches!(
        collection,
        streams::ROOM_MESSAGES
            | streams::NOTIFY_USER
            | streams::APPS
            | streams::NOTIFY_LOGGED
            | streams::NOTIFY_ALL
            | streams::NOTIFY_ROOM
            | streams::ROLES
            | streams::INQUIRY_QUEUE
            | streams::CANNED_RESPONSES
            | streams::ROOM_DATA
    )
}

struct Out {
    tx: mpsc::Sender<SessionEvent>,
    /// Control events (and the overflow resync) waiting for room, in order.
    pending: VecDeque<SessionEvent>,
    overflow_queued: bool,
}

impl Out {
    fn queue_overflow(&mut self) {
        if !self.overflow_queued {
            tracing::warn!("session event channel full; dropping events and requesting a resync");
            self.overflow_queued = true;
            self.pending.push_back(SessionEvent::Resync {
                reason: ResyncReason::Overflow,
            });
        }
    }

    /// Returns `false` when the consumer is gone.
    fn control(&mut self, event: SessionEvent) -> bool {
        if !self.pending.is_empty() {
            self.pending.push_back(event);
            return true;
        }
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(event)) => {
                self.pending.push_back(event);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn data(&mut self, event: SessionEvent) -> bool {
        if !self.pending.is_empty() {
            self.queue_overflow();
            return true;
        }
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.queue_overflow();
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

async fn next_feed(
    feed: &mut Option<broadcast::Receiver<Arc<DataMessage>>>,
) -> Result<Arc<DataMessage>, broadcast::error::RecvError> {
    match feed {
        Some(feed) => feed.recv().await,
        None => std::future::pending().await,
    }
}

/// Runs until the terminal event is delivered, the consumer is dropped, or every
/// control sender is gone. `observe` sees every decoded stream event first and
/// returns whether it is delivered.
pub(super) async fn run(
    tx: mpsc::Sender<SessionEvent>,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    mut feed: Option<broadcast::Receiver<Arc<DataMessage>>>,
    mut observe: impl FnMut(&SessionEvent) -> bool + Send,
) {
    let mut out = Out {
        tx,
        pending: VecDeque::new(),
        overflow_queued: false,
    };
    // A second handle for waiting on room, so `out` stays free to mutate.
    let sender = out.tx.clone();
    let mut terminating = false;
    loop {
        if terminating && out.pending.is_empty() {
            break;
        }
        let has_pending = !out.pending.is_empty();
        let alive = tokio::select! {
            biased;
            permit = sender.reserve(), if has_pending => match permit {
                Ok(permit) => {
                    if let Some(event) = out.pending.pop_front() {
                        if matches!(event, SessionEvent::Resync { reason: ResyncReason::Overflow }) {
                            out.overflow_queued = false;
                        }
                        permit.send(event);
                    }
                    true
                }
                Err(_) => false,
            },
            () = sender.closed(), if !has_pending => false,
            c = ctl.recv(), if !terminating => match c {
                Some(Ctl::Event(event)) => out.control(event),
                Some(Ctl::Terminate(last)) => {
                    terminating = true;
                    feed = None;
                    out.pending.extend(last);
                    true
                }
                None => {
                    terminating = true;
                    feed = None;
                    out.pending
                        .push_back(SessionEvent::Connection(ConnectionState::Closed));
                    true
                }
            },
            m = next_feed(&mut feed), if !terminating => match m {
                Ok(m) if is_session_stream(&m.collection) => {
                    let event = decode_event(&m);
                    if observe(&event) {
                        out.data(event)
                    } else {
                        true
                    }
                }
                Ok(_) => true,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "DDP feed lagged; requesting a resync");
                    out.control(SessionEvent::Resync {
                        reason: ResyncReason::Lagged {
                            stream: "ddp".into(),
                            dropped: n,
                        },
                    })
                }
                Err(broadcast::error::RecvError::Closed) => {
                    feed = None;
                    true
                }
            },
        };
        if !alive {
            break;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use meteor_ddp::DataKind;
    use serde_json::{Value, json};

    fn data(collection: &str, event: &str, args: Value) -> Arc<DataMessage> {
        let mut fields = serde_json::Map::new();
        fields.insert("eventName".into(), json!(event));
        fields.insert("args".into(), args);
        Arc::new(DataMessage {
            kind: DataKind::Changed,
            collection: collection.into(),
            id: "id".into(),
            fields: Some(fields),
            cleared: Vec::new(),
            before: None,
        })
    }

    fn del(id: &str) -> Arc<DataMessage> {
        data(
            streams::NOTIFY_ROOM,
            "r/deleteMessage",
            json!([{ "_id": id }]),
        )
    }

    fn deleted(id: &str) -> SessionEvent {
        SessionEvent::MessageDeleted {
            rid: "r".into(),
            id: id.into(),
        }
    }

    fn conn(s: ConnectionState) -> SessionEvent {
        SessionEvent::Connection(s)
    }

    struct Harness {
        rx: mpsc::Receiver<SessionEvent>,
        ctl: mpsc::UnboundedSender<Ctl>,
        feed: broadcast::Sender<Arc<DataMessage>>,
        task: tokio::task::JoinHandle<()>,
    }

    fn harness(capacity: usize, feed_capacity: usize) -> Harness {
        let (tx, rx) = mpsc::channel(capacity);
        let (ctl, ctl_rx) = mpsc::unbounded_channel();
        let (feed, feed_rx) = broadcast::channel(feed_capacity);
        let task = tokio::spawn(run(tx, ctl_rx, Some(feed_rx), |_| true));
        Harness {
            rx,
            ctl,
            feed,
            task,
        }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn overflow_drops_stream_events_and_queues_one_resync() {
        let mut h = harness(2, 64);
        for i in 0..10 {
            h.feed.send(del(&i.to_string())).unwrap();
        }
        settle().await;
        assert_eq!(h.rx.recv().await, Some(deleted("0")));
        assert_eq!(h.rx.recv().await, Some(deleted("1")));
        assert_eq!(
            h.rx.recv().await,
            Some(SessionEvent::Resync {
                reason: ResyncReason::Overflow
            })
        );
        h.feed.send(del("after")).unwrap();
        assert_eq!(h.rx.recv().await, Some(deleted("after")));
        settle().await;
        assert!(h.rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn control_events_keep_their_order_when_full() {
        let mut h = harness(1, 64);
        h.feed.send(del("0")).unwrap();
        settle().await;
        let expected = vec![
            conn(ConnectionState::Connecting),
            conn(ConnectionState::Connected),
            SessionEvent::Resync {
                reason: ResyncReason::Reconnected,
            },
            SessionEvent::Degraded { streams: vec![] },
        ];
        for e in &expected {
            h.ctl.send(Ctl::Event(e.clone())).unwrap();
            // Stream events arriving meanwhile are dropped behind the queue.
            h.feed.send(del("x")).unwrap();
        }
        settle().await;
        assert_eq!(h.rx.recv().await, Some(deleted("0")));
        drop(h.ctl);
        let mut got = Vec::new();
        while let Some(e) = h.rx.recv().await {
            got.push(e);
        }
        let controls: Vec<_> = got
            .iter()
            .filter(|e| {
                !matches!(
                    e,
                    SessionEvent::MessageDeleted { .. }
                        | SessionEvent::Resync {
                            reason: ResyncReason::Overflow
                        }
                )
            })
            .cloned()
            .collect();
        let mut expected = expected;
        expected.push(conn(ConnectionState::Closed));
        assert_eq!(controls, expected);
        assert!(got.contains(&SessionEvent::Resync {
            reason: ResyncReason::Overflow
        }));
    }

    #[tokio::test]
    async fn terminal_event_is_last() {
        let mut h = harness(100, 64);
        for i in 0..5 {
            h.feed.send(del(&i.to_string())).unwrap();
        }
        h.ctl
            .send(Ctl::Terminate(Some(SessionEvent::LoggedOut {
                reason: "x".into(),
            })))
            .unwrap();
        for i in 5..10 {
            h.feed.send(del(&i.to_string())).unwrap();
        }
        h.ctl
            .send(Ctl::Event(conn(ConnectionState::Connected)))
            .ok();
        let mut got = Vec::new();
        while let Some(e) = h.rx.recv().await {
            got.push(e);
        }
        assert_eq!(
            got.last(),
            Some(&SessionEvent::LoggedOut { reason: "x".into() })
        );
        assert_eq!(got.len(), 1, "control is handled first: {got:?}");
        h.task.await.unwrap();
    }

    #[tokio::test]
    async fn terminal_event_waits_for_room_and_stays_last() {
        let mut h = harness(1, 64);
        h.feed.send(del("0")).unwrap();
        settle().await;
        h.ctl
            .send(Ctl::Event(conn(ConnectionState::Connecting)))
            .unwrap();
        h.ctl
            .send(Ctl::Terminate(Some(conn(ConnectionState::Closed))))
            .unwrap();
        h.feed.send(del("late")).unwrap();
        settle().await;
        let mut got = Vec::new();
        while let Some(e) = h.rx.recv().await {
            got.push(e);
        }
        assert_eq!(
            got,
            vec![
                deleted("0"),
                conn(ConnectionState::Connecting),
                conn(ConnectionState::Closed)
            ]
        );
    }

    #[tokio::test]
    async fn lag_filter_and_consumer_drop() {
        let (tx, rx) = mpsc::channel(100);
        let (_ctl, ctl_rx) = mpsc::unbounded_channel();
        let (feed, feed_rx) = broadcast::channel(4);
        for i in 0..6 {
            feed.send(del(&i.to_string())).unwrap();
        }
        let task = tokio::spawn(run(tx, ctl_rx, Some(feed_rx), |_| true));
        let mut rx = rx;
        assert_eq!(
            rx.recv().await,
            Some(SessionEvent::Resync {
                reason: ResyncReason::Lagged {
                    stream: "ddp".into(),
                    dropped: 2
                }
            })
        );
        for i in 2..6 {
            assert_eq!(rx.recv().await, Some(deleted(&i.to_string())));
        }
        feed.send(data("users", "", json!([]))).unwrap();
        feed.send(del("x")).unwrap();
        assert_eq!(rx.recv().await, Some(deleted("x")));
        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("pump did not stop after the consumer was dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn observer_sees_decoded_events() {
        let (tx, mut rx) = mpsc::channel(10);
        let (ctl, ctl_rx) = mpsc::unbounded_channel();
        let (feed, feed_rx) = broadcast::channel(4);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        tokio::spawn(run(tx, ctl_rx, Some(feed_rx), move |e| {
            seen2.lock().unwrap().push(e.clone());
            // The observer may withhold an event.
            *e != deleted("2")
        }));
        feed.send(del("2")).unwrap();
        feed.send(del("1")).unwrap();
        assert_eq!(rx.recv().await, Some(deleted("1")));
        assert_eq!(*seen.lock().unwrap(), vec![deleted("2"), deleted("1")]);
        drop(ctl);
        assert_eq!(rx.recv().await, Some(conn(ConnectionState::Closed)));
        assert_eq!(rx.recv().await, None);
    }
}
