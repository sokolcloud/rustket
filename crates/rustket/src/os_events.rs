//! Events the OS delivers outside winit (macOS Apple Events, see
//! `macos_events`), buffered until the app subscribes so the link that
//! launched the app is not lost.

use std::sync::Mutex;

use iced::Subscription;
use iced::futures::Stream;
use iced::futures::channel::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsEvent {
    /// A `rocketchat://` link opened while the app runs or that launched it.
    Url(String),
    /// The user asked to bring the app back (Dock icon click).
    Reopen,
}

struct Queue {
    pending: Vec<OsEvent>,
    tx: Option<mpsc::UnboundedSender<OsEvent>>,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    pending: Vec::new(),
    tx: None,
});

fn queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Delivers `event` to the subscription, or keeps it until there is one.
pub fn push(event: OsEvent) {
    let mut q = queue();
    let event = match &q.tx {
        Some(tx) => match tx.unbounded_send(event) {
            Ok(()) => return,
            Err(e) => e.into_inner(),
        },
        None => event,
    };
    q.tx = None;
    q.pending.push(event);
}

fn stream() -> impl Stream<Item = OsEvent> {
    let (tx, rx) = mpsc::unbounded();
    let mut q = queue();
    for event in q.pending.drain(..) {
        let _ = tx.unbounded_send(event);
    }
    q.tx = Some(tx);
    rx
}

/// Every [`OsEvent`], starting with those that arrived before.
pub fn subscription() -> Subscription<OsEvent> {
    Subscription::run(stream)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;

    #[tokio::test]
    async fn buffers_until_subscribed() {
        push(OsEvent::Url("rocketchat://a".into()));
        push(OsEvent::Reopen);
        let mut s = stream();
        assert_eq!(s.next().await, Some(OsEvent::Url("rocketchat://a".into())));
        assert_eq!(s.next().await, Some(OsEvent::Reopen));
        push(OsEvent::Url("rocketchat://b".into()));
        assert_eq!(s.next().await, Some(OsEvent::Url("rocketchat://b".into())));
        // A dropped subscription: events wait for the next one.
        drop(s);
        push(OsEvent::Reopen);
        assert_eq!(stream().next().await, Some(OsEvent::Reopen));
    }
}
