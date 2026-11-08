//! Desktop notification of a verified incoming call (Accept / Decline is in
//! the popup; the notification raises the window or accepts). Closed when the
//! call is resolved.

use std::collections::HashMap;
use std::sync::Arc;

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;

use crate::desktop::notifications::{
    ACTION_DEFAULT, Event, Notifier, Outgoing, Signal, TIMEOUT_NEVER, URGENCY_CRITICAL,
};

/// Key of the "Accept" action.
pub const ACTION_ACCEPT: &str = "accept";

#[derive(Debug, Clone)]
pub enum Msg {
    Shown(ServerId, String, Event),
    /// A notification server signal the desktop layer did not own.
    Signal(u32, Signal),
    Closed,
}

#[derive(Default)]
pub struct CallNotifier {
    backend: Option<Arc<dyn Notifier>>,
    /// Shown notifications (`None` while the showing is in flight).
    shown: HashMap<(ServerId, String), Option<u32>>,
}

impl std::fmt::Debug for CallNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallNotifier")
            .field("shown", &self.shown)
            .finish()
    }
}

impl CallNotifier {
    pub fn show(
        &mut self,
        backend: Arc<dyn Notifier>,
        sid: &ServerId,
        call_id: &str,
        caller: &str,
    ) -> Task<Msg> {
        let key = (sid.clone(), call_id.to_owned());
        if self.shown.contains_key(&key) {
            return Task::none();
        }
        self.shown.insert(key, None);
        self.backend = Some(Arc::clone(&backend));
        let title = if caller.is_empty() {
            t!("videoconf.incoming_title").into_owned()
        } else {
            t!("videoconf.incoming_from", name = caller).into_owned()
        };
        let out = Outgoing {
            replaces: None,
            title,
            body: t!("videoconf.incoming_body").into_owned(),
            image: None,
            sound: false,
            actions: vec![
                (ACTION_DEFAULT.to_owned(), t!("videoconf.open").into_owned()),
                (
                    ACTION_ACCEPT.to_owned(),
                    t!("videoconf.accept").into_owned(),
                ),
            ],
            timeout: TIMEOUT_NEVER,
            urgency: URGENCY_CRITICAL,
        };
        let (sid, call_id) = (sid.clone(), call_id.to_owned());
        Task::perform(backend.show(out), move |e| {
            Msg::Shown(sid.clone(), call_id.clone(), e)
        })
    }

    /// Closes the notification of `call_id`, if one is shown.
    pub fn close(&mut self, sid: &ServerId, call_id: &str) -> Task<Msg> {
        let Some(Some(id)) = self.shown.remove(&(sid.clone(), call_id.to_owned())) else {
            return Task::none();
        };
        match &self.backend {
            Some(b) => Task::perform(b.close(id), |()| Msg::Closed),
            None => Task::none(),
        }
    }
}

/// `Some((server, call, accept))` when the user activated a call notification.
pub fn update(n: &mut CallNotifier, m: Msg) -> (Option<(ServerId, String, bool)>, Task<Msg>) {
    let activated = match m {
        Msg::Shown(sid, call_id, Event::Shown { id, .. }) => {
            let key = (sid, call_id);
            match n.shown.get_mut(&key) {
                Some(slot) => *slot = Some(id),
                // Resolved while the showing was in flight.
                None => {
                    if let Some(b) = &n.backend {
                        return (None, Task::perform(b.close(id), |()| Msg::Closed));
                    }
                }
            }
            None
        }
        Msg::Shown(sid, call_id, Event::Failed(e)) => {
            tracing::debug!(error = %e, "call notification not shown");
            n.shown.remove(&(sid, call_id));
            None
        }
        Msg::Signal(id, signal) => {
            let Some(key) = n
                .shown
                .iter()
                .find(|(_, v)| **v == Some(id))
                .map(|(k, _)| k.clone())
            else {
                return (None, Task::none());
            };
            match signal {
                Signal::Action(a) => {
                    n.shown.remove(&key);
                    Some((key.0, key.1, a == ACTION_ACCEPT))
                }
                Signal::Closed => {
                    n.shown.remove(&key);
                    None
                }
            }
        }
        Msg::Closed => None,
    };
    (activated, Task::none())
}
