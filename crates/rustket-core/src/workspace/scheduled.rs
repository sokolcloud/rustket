//! Scheduled messages of the workspace (`crate::scheduled`).

use rocketchat::RcDate;

use crate::scheduled::{Due, Scheduled, ScheduledQueue};

use super::{Dirty, Workspace};

impl Workspace {
    /// The account's scheduled messages.
    pub fn scheduled(&self) -> &ScheduledQueue {
        &self.scheduled
    }

    /// Schedules a new local message ([`Self::new_local_message`]) for
    /// `send_at` (UTC ms).
    pub fn schedule_message(&mut self, message: rocketchat::models::Message, send_at: i64) {
        self.scheduled.insert(Scheduled::new(message, send_at));
        self.mark(Dirty::SCHEDULED);
    }

    /// Puts back an entry read from the offline cache (unless one with its
    /// id is there).
    pub fn restore_scheduled(&mut self, s: Scheduled) {
        if self.scheduled.get(s.id()).is_some() {
            return;
        }
        self.scheduled.insert(s);
        self.mark(Dirty::SCHEDULED);
    }

    /// A due message (its pending entry `message`) failed for good: it
    /// leaves the outbox and its timeline and waits again with the
    /// scheduled ones as missed, for the user to send, reschedule or
    /// delete. Its send time is its local time (when it fell due).
    pub fn return_failed_scheduled(&mut self, message: rocketchat::models::Message, error: &str) {
        let (rid, tmid, id) = (
            message.rid.clone(),
            message.tmid.clone(),
            message.id.clone(),
        );
        self.discard_pending(&rid, tmid.as_deref(), &id);
        let send_at = message
            .ts
            .map(|t| t.timestamp_millis())
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        self.scheduled
            .insert(Scheduled::failed(message, send_at, error.to_owned()));
        self.mark(Dirty::SCHEDULED);
    }

    pub fn reschedule(&mut self, id: &str, at_ms: i64) -> bool {
        let done = self.scheduled.reschedule(id, at_ms);
        if done {
            self.mark(Dirty::SCHEDULED);
        }
        done
    }

    pub fn set_scheduled_text(&mut self, id: &str, text: &str) -> bool {
        let done = self.scheduled.set_text(id, text);
        if done {
            self.mark(Dirty::SCHEDULED);
        }
        done
    }

    pub fn remove_scheduled(&mut self, id: &str) -> Option<Scheduled> {
        let s = self.scheduled.remove(id);
        if s.is_some() {
            self.mark(Dirty::SCHEDULED);
        }
        s
    }

    /// Takes the entries due now ([`ScheduledQueue::take_due`]); the ones
    /// taken carry `now_ms` as their local time (the server stamps the
    /// real one).
    pub fn take_due_scheduled(&mut self, now_ms: i64, online: bool) -> Due {
        let mut due = self.scheduled.take_due(now_ms, online);
        if !due.send.is_empty() || !due.missed.is_empty() {
            self.mark(Dirty::SCHEDULED);
        }
        for s in &mut due.send {
            s.message.ts = RcDate::from_millis(now_ms).or(s.message.ts);
        }
        due
    }

    /// Whether scheduled messages are kept on disk (set by the app); new
    /// ones are refused without it, they would be lost on quitting.
    pub fn scheduled_storage(&self) -> bool {
        self.scheduled_storage
    }

    pub fn set_scheduled_storage(&mut self, on: bool) {
        self.scheduled_storage = on;
    }

    /// Scheduled messages dropped because their room went (left, removed):
    /// `(room name, count)`, once.
    pub fn take_dropped_scheduled(&mut self) -> Vec<(String, usize)> {
        std::mem::take(&mut self.dropped_scheduled)
    }

    /// Keeps entry `id` (one with an open edit or reschedule dialog) from
    /// going out; every other entry is released.
    pub fn hold_scheduled(&mut self, id: Option<&str>) {
        if self.scheduled.hold_only(id) {
            self.mark(Dirty::SCHEDULED);
        }
    }

    /// Records the status the user chose (`users.setStatus` succeeded):
    /// `statusDefault` and the shown status and text.
    pub fn apply_my_status(&mut self, status: rocketchat::models::UserStatus, text: Option<&str>) {
        let Some(uid) = self.user_id().map(str::to_owned) else {
            return;
        };
        let username = self.username().map(str::to_owned);
        if let Some(me) = self.me.as_mut() {
            me.status_default = Some(status);
        }
        self.apply_user_status(&uid, username.as_deref(), status, text);
    }

    /// The status the user chose (`crate::quiet::chosen_status`).
    pub fn my_chosen_status(&self) -> Option<rocketchat::models::UserStatus> {
        crate::quiet::chosen_status(
            self.me.as_ref().and_then(|m| m.status_default),
            self.my_status(),
        )
    }

    /// Takes one entry out to be sent now, whatever its time.
    pub fn take_scheduled_now(&mut self, id: &str, now_ms: i64) -> Option<Scheduled> {
        let mut s = self.remove_scheduled(id)?;
        s.message.ts = RcDate::from_millis(now_ms).or(s.message.ts);
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use rocketchat::models::{Room, Subscription, UserStatus};

    use crate::sync::InitialData;
    use crate::workspace::{Change, Workspace};

    #[test]
    fn leaving_a_room_reports_its_dropped_messages() {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: rocketchat::models::User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![Subscription {
                id: "s1".into(),
                rid: "R1".into(),
                name: "general".into(),
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "R1".into(),
                name: Some("general".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        for t in ["a", "b"] {
            let m = ws.new_local_message("R1", t, None, false);
            ws.schedule_message(m, 1_000);
        }
        let m = ws.new_local_message("R2", "c", None, false);
        ws.schedule_message(m, 1_000);
        ws.apply_subscription(
            Change::Removed,
            Subscription {
                rid: "R1".into(),
                ..Default::default()
            },
        );
        assert_eq!(ws.scheduled().len(), 1);
        assert_eq!(ws.take_dropped_scheduled(), [("general".to_owned(), 2)]);
        assert!(ws.take_dropped_scheduled().is_empty());
    }

    /// A room gone from a later initial load (left while the app was off)
    /// is reported by its name, not its id.
    #[test]
    fn a_room_gone_from_the_initial_load_is_reported_by_name() {
        let me = rocketchat::models::User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        };
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: me.clone(),
            subscriptions: vec![Subscription {
                id: "s1".into(),
                rid: "R1".into(),
                name: "general".into(),
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "R1".into(),
                name: Some("general".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let m = ws.new_local_message("R1", "a", None, false);
        ws.schedule_message(m, 1_000);
        ws.apply_initial(InitialData {
            me,
            ..Default::default()
        });
        assert!(ws.scheduled().is_empty());
        assert_eq!(ws.take_dropped_scheduled(), [("general".to_owned(), 1)]);
    }

    #[test]
    fn the_chosen_status_is_recorded() {
        let mut ws = Workspace::new();
        ws.apply_me(rocketchat::models::User {
            id: "me".into(),
            username: Some("me".into()),
            status: Some(UserStatus::Offline),
            ..Default::default()
        });
        assert_eq!(ws.my_chosen_status(), None, "offline: not known");
        ws.apply_my_status(UserStatus::Busy, None);
        assert_eq!(ws.my_chosen_status(), Some(UserStatus::Busy));
        assert_eq!(ws.me().unwrap().status_default, Some(UserStatus::Busy));
    }
}
