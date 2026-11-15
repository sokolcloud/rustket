//! The per-account read queue (`rustket_core::reads`) wired to the session:
//! DDP `readMessages` / `readThreads`, REST `subscriptions.read` /
//! `chat.readThread` as the fallback. One request at a time, paced, with
//! one rate-limit handling for every caller ("Mark all as read", the All
//! unreads and Activity views). Callers tag requests with an [`Origin`] and
//! get it back when the request is done.

use std::time::Instant;

use iced::Task;
use rustket_core::reads::{Done, Next, Outcome, ReadQueue, ReadReq, Ticket, Transport};

use crate::error::{Failure, Res};
use crate::session::SessionHandle;

/// Who queued a read mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// "Mark all as read" (Shift+Esc), run `stamp`.
    ReadAll(u64),
    /// The All unreads / Catch up / Activity views (`crate::inbox`).
    Inbox,
}

pub type Queue = ReadQueue<Origin>;

#[derive(Debug, Clone)]
pub enum Msg {
    /// The pacing timer fired.
    Wake,
    /// A request ended.
    Sent {
        ticket: Ticket,
        via: Transport,
        result: Res<()>,
    },
}

/// Runs the queue's next step: sends a request or arms the pacing timer.
/// Without a session the queued requests fail at once.
pub fn pump(q: &mut Queue, session: Option<&SessionHandle>) -> (Task<Msg>, Vec<Done<Origin>>) {
    let Some(session) = session else {
        return (Task::none(), q.fail_queued());
    };
    let up = session.session().connection_state().is_connected();
    let task = match q.next(Instant::now(), up) {
        Next::Idle => Task::none(),
        Next::Wait(d) => Task::perform(tokio::time::sleep(d), |()| Msg::Wake),
        Next::Send { req, via, ticket } => {
            let session = session.clone();
            Task::perform(
                async move { send(&session, &req, via).await.map_err(Failure::from) },
                move |result| Msg::Sent {
                    ticket,
                    via,
                    result,
                },
            )
        }
    };
    (task, Vec::new())
}

async fn send(session: &SessionHandle, req: &ReadReq, via: Transport) -> rocketchat::Result<()> {
    match (req, via) {
        (ReadReq::Room { rid, threads }, Transport::Ddp) => {
            session.session().read_messages(rid, *threads).await
        }
        (ReadReq::Thread { tmid, .. }, Transport::Ddp) => {
            session.session().read_thread_method(tmid).await
        }
        (ReadReq::Room { rid, threads }, Transport::Rest) => {
            session.rest().subscriptions_read_with(rid, *threads).await
        }
        (ReadReq::Thread { tmid, .. }, Transport::Rest) => session.rest().read_thread(tmid).await,
    }
}

/// Applies a queue message, then runs the next step.
pub fn update(
    q: &mut Queue,
    msg: Msg,
    session: Option<&SessionHandle>,
) -> (Task<Msg>, Vec<Done<Origin>>) {
    let mut done = Vec::new();
    match msg {
        Msg::Wake => q.wake(),
        Msg::Sent {
            ticket,
            via,
            result,
        } => {
            let outcome = match &result {
                Ok(()) => Outcome::Ok,
                Err(f) => {
                    // No room ids or texts in the log.
                    tracing::debug!(?via, error = %f, "read mark failed");
                    Outcome::from_error(f.error())
                }
            };
            done.extend(q.finish(ticket, outcome, Instant::now()));
        }
    }
    let (task, failed) = pump(q, session);
    done.extend(failed);
    (task, done)
}
