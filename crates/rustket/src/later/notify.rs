//! Desktop notifications of message reminders: one per due reminder (Open,
//! Snooze 1 hour, Done) and one summary of reminders missed while Rustket
//! was not running. Clicks and actions come back through the desktop
//! layer's foreign signals (`desktop::Request::ForeignSignal`), routed here
//! by id ([`ReminderNotifier::owns`]). A notification closes when its
//! reminder changes or goes (on another device too).

use std::collections::HashMap;
use std::sync::Arc;

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;

use crate::desktop::notifications::{
    ACTION_DEFAULT, Event, Notifier, Outgoing, Signal, TIMEOUT_NEVER, URGENCY_NORMAL,
};
use crate::desktop::sound::RcSound;
use crate::room::render::plural;

/// Key of the "Snooze 1 hour" action.
pub const ACTION_SNOOZE: &str = "snooze";
/// Key of the "Done" action.
pub const ACTION_DONE: &str = "done";

/// What a notification is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shown {
    /// The reminder of message `mid` due at `at`.
    Reminder { sid: ServerId, mid: String, at: i64 },
    /// The missed reminders of a server.
    Missed(ServerId),
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A showing finished, with the notification's sound.
    Shown(Shown, Event, Option<RcSound>),
    Closed,
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderAction {
    /// Open the message (and mark the reminder done).
    Open {
        sid: ServerId,
        mid: String,
    },
    Snooze {
        sid: ServerId,
        mid: String,
    },
    Done {
        sid: ServerId,
        mid: String,
    },
    /// Open the Later view's reminders.
    OpenLater(ServerId),
}

/// A reminder notification's content.
pub struct Content {
    /// Where the message is ("#room", "Thread in #room").
    pub place: String,
    /// Plain text of the message, when shown.
    pub body: Option<String>,
    /// The reminder sound (`app::reminders::reminder_sound`).
    pub sound: Option<RcSound>,
}

#[derive(Default)]
pub struct ReminderNotifier {
    backend: Option<Arc<dyn Notifier>>,
    /// Shown notifications (`None` while the showing is in flight).
    shown: HashMap<Shown, Option<u32>>,
}

impl std::fmt::Debug for ReminderNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReminderNotifier")
            .field("shown", &self.shown)
            .finish()
    }
}

impl ReminderNotifier {
    fn show(&mut self, backend: Arc<dyn Notifier>, key: Shown, out: Outgoing) -> Task<Msg> {
        if self.shown.contains_key(&key) {
            return Task::none();
        }
        self.shown.insert(key.clone(), None);
        self.backend = Some(Arc::clone(&backend));
        let sound = out.sound.clone();
        Task::perform(backend.show(out), move |e| {
            Msg::Shown(key.clone(), e, sound.clone())
        })
    }

    /// Notifies about the reminder of `mid` due at `at`.
    pub fn show_reminder(
        &mut self,
        backend: Arc<dyn Notifier>,
        sid: &ServerId,
        mid: &str,
        at: i64,
        c: Content,
    ) -> Task<Msg> {
        let out = Outgoing {
            replaces: None,
            title: t!("desktop.reminder_title", room = c.place.as_str()).into_owned(),
            body: c
                .body
                .filter(|b| !b.trim().is_empty())
                .unwrap_or_else(|| t!("desktop.reminder_body_hidden").into_owned()),
            image: None,
            sound: c.sound,
            actions: vec![
                (ACTION_DEFAULT.to_owned(), t!("desktop.open").into_owned()),
                (
                    ACTION_SNOOZE.to_owned(),
                    t!("desktop.reminder_snooze").into_owned(),
                ),
                (
                    ACTION_DONE.to_owned(),
                    t!("desktop.reminder_done").into_owned(),
                ),
            ],
            timeout: TIMEOUT_NEVER,
            urgency: URGENCY_NORMAL,
        };
        let key = Shown::Reminder {
            sid: sid.clone(),
            mid: mid.to_owned(),
            at,
        };
        self.show(backend, key, out)
    }

    /// One notification for `count` reminders missed on server `sid`, with
    /// `sound`.
    pub fn show_missed(
        &mut self,
        backend: Arc<dyn Notifier>,
        sid: &ServerId,
        count: usize,
        sound: Option<RcSound>,
    ) -> Task<Msg> {
        let n = count as u64;
        let out = Outgoing {
            replaces: None,
            title: t!(
                format!("desktop.reminders_missed_title_{}", plural(n)),
                count = n
            )
            .into_owned(),
            body: t!("desktop.reminders_missed_body").into_owned(),
            image: None,
            sound,
            actions: vec![(ACTION_DEFAULT.to_owned(), t!("desktop.open").into_owned())],
            timeout: TIMEOUT_NEVER,
            urgency: URGENCY_NORMAL,
        };
        self.show(backend, Shown::Missed(sid.clone()), out)
    }

    /// Whether notification `id` is a reminder's.
    pub fn owns(&self, id: u32) -> bool {
        self.shown.values().any(|v| *v == Some(id))
    }

    /// Reminders shown now: `(server, message, due time)`.
    pub fn reminders(&self) -> Vec<(ServerId, String, i64)> {
        self.shown
            .keys()
            .filter_map(|k| match k {
                Shown::Reminder { sid, mid, at } => Some((sid.clone(), mid.clone(), *at)),
                Shown::Missed(_) => None,
            })
            .collect()
    }

    fn close(&mut self, key: &Shown) -> Task<Msg> {
        let Some(Some(id)) = self.shown.remove(key) else {
            return Task::none();
        };
        match &self.backend {
            Some(b) => Task::perform(b.close(id), |()| Msg::Closed),
            None => Task::none(),
        }
    }

    /// Closes the notification of a reminder that changed or went.
    pub fn close_reminder(&mut self, sid: &ServerId, mid: &str, at: i64) -> Task<Msg> {
        self.close(&Shown::Reminder {
            sid: sid.clone(),
            mid: mid.to_owned(),
            at,
        })
    }

    /// Closes every notification of server `sid` (signed out, removed).
    pub fn close_server(&mut self, sid: &ServerId) -> Task<Msg> {
        let keys: Vec<Shown> = self
            .shown
            .keys()
            .filter(|k| match k {
                Shown::Reminder { sid: s, .. } | Shown::Missed(s) => s == sid,
            })
            .cloned()
            .collect();
        Task::batch(keys.iter().map(|k| self.close(k)).collect::<Vec<_>>())
    }

    /// Returns the follow-up and the sound the app plays, as the backend
    /// did not (it cannot, or the notification failed).
    pub fn update(&mut self, m: Msg) -> (Task<Msg>, Option<RcSound>) {
        match m {
            Msg::Shown(key, Event::Shown { id, server_sound }, sound) => {
                match self.shown.get_mut(&key) {
                    Some(slot) => {
                        *slot = Some(id);
                        (Task::none(), sound.filter(|_| !server_sound))
                    }
                    // Closed while the showing was in flight.
                    None => match &self.backend {
                        Some(b) => (Task::perform(b.close(id), |()| Msg::Closed), None),
                        None => (Task::none(), None),
                    },
                }
            }
            Msg::Shown(key, Event::Failed(e), sound) => {
                tracing::debug!(error = %e, "reminder notification not shown");
                let current = self.shown.remove(&key).is_some();
                (Task::none(), sound.filter(|_| current))
            }
            Msg::Closed => (Task::none(), None),
        }
    }

    /// A click, action or dismissal of notification `id`.
    pub fn signal(&mut self, id: u32, signal: Signal) -> Option<ReminderAction> {
        let key = self
            .shown
            .iter()
            .find(|(_, v)| **v == Some(id))
            .map(|(k, _)| k.clone())?;
        self.shown.remove(&key);
        let Signal::Action(a) = signal else {
            return None;
        };
        match key {
            Shown::Missed(sid) => Some(ReminderAction::OpenLater(sid)),
            Shown::Reminder { sid, mid, .. } => Some(match a.as_str() {
                ACTION_SNOOZE => ReminderAction::Snooze { sid, mid },
                ACTION_DONE => ReminderAction::Done { sid, mid },
                _ => ReminderAction::Open { sid, mid },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::desktop::notifications::MockNotifier;

    fn sid() -> ServerId {
        ServerId("s".into())
    }

    #[test]
    fn signals_map_to_actions_and_owned_ids() {
        let mock = MockNotifier::default();
        let backend: Arc<dyn Notifier> = Arc::new(mock.clone());
        let mut n = ReminderNotifier::default();
        let content = || Content {
            place: "#general".into(),
            body: None,
            sound: None,
        };
        let _ = n.show_reminder(backend.clone(), &sid(), "m1", 10, content());
        // Shown twice: one notification.
        let _ = n.show_reminder(backend.clone(), &sid(), "m1", 10, content());
        let key = Shown::Reminder {
            sid: sid(),
            mid: "m1".into(),
            at: 10,
        };
        let _ = n.update(Msg::Shown(
            key,
            Event::Shown {
                id: 7,
                server_sound: false,
            },
            None,
        ));
        assert!(n.owns(7) && !n.owns(8));
        assert_eq!(n.reminders(), vec![(sid(), "m1".to_owned(), 10)]);
        assert_eq!(
            n.signal(7, Signal::Action(ACTION_SNOOZE.into())),
            Some(ReminderAction::Snooze {
                sid: sid(),
                mid: "m1".into()
            })
        );
        assert!(!n.owns(7));
        let _ = n.show_missed(backend.clone(), &sid(), 3, None);
        let _ = n.update(Msg::Shown(
            Shown::Missed(sid()),
            Event::Shown {
                id: 9,
                server_sound: false,
            },
            None,
        ));
        assert_eq!(
            n.signal(9, Signal::Action(ACTION_DEFAULT.into())),
            Some(ReminderAction::OpenLater(sid()))
        );
        let _ = n.show_reminder(backend.clone(), &sid(), "m2", 20, content());
        let key = Shown::Reminder {
            sid: sid(),
            mid: "m2".into(),
            at: 20,
        };
        let _ = n.update(Msg::Shown(
            key,
            Event::Shown {
                id: 11,
                server_sound: false,
            },
            None,
        ));
        // Dismissed: nothing to do, no longer owned.
        assert_eq!(n.signal(11, Signal::Closed), None);
        assert!(!n.owns(11));
    }

    /// The app plays a reminder's sound only when its notification did not.
    #[test]
    fn reminder_sound_plays_locally_only_without_the_os() {
        let chime = Some(RcSound {
            name: "chime".into(),
            volume: 100,
        });
        let backend: Arc<dyn Notifier> = Arc::new(MockNotifier::default());
        let mut n = ReminderNotifier::default();
        let key = Shown::Missed(sid());
        let shown = |server_sound| Event::Shown {
            id: 3,
            server_sound,
        };
        let _ = n.show_missed(backend.clone(), &sid(), 2, chime.clone());
        assert_eq!(
            n.update(Msg::Shown(key.clone(), shown(true), chime.clone()))
                .1,
            None
        );
        assert_eq!(
            n.update(Msg::Shown(key.clone(), shown(false), chime.clone()))
                .1,
            chime
        );
        let failed = || Event::Failed("no server".into());
        assert_eq!(
            n.update(Msg::Shown(key.clone(), failed(), chime.clone())).1,
            chime
        );
        // Gone (closed in flight): silent.
        assert_eq!(n.update(Msg::Shown(key, failed(), chime.clone())).1, None);
    }
}
