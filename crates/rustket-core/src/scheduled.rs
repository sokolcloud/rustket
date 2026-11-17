//! Scheduled messages: text the user wrote to be sent at a later time.
//!
//! A scheduled message is an outbox entry with a send time
//! ([`crate::cache::OutboxEntry::send_at`]): it lives only on this device
//! (the offline cache persists it with the outbox, never the synced service
//! room) and is not a pending message of its timeline until it is due. Due
//! messages become ordinary queued outbox entries and go out like messages
//! written offline ([`ScheduledQueue::take_due`]); one whose time passed by
//! more than [`MISSED_GRACE_MS`] (Rustket was not running, or not connected)
//! is not sent automatically but marked missed for the user to decide.

use std::collections::BTreeMap;

use rocketchat::models::Message;

/// How late a scheduled message may still go out automatically.
pub const MISSED_GRACE_MS: i64 = 60_000;

/// A message waiting for its send time.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduled {
    /// The local message (client-generated `_id`, room, thread, text).
    pub message: Message,
    /// When to send it (UTC ms).
    pub send_at: i64,
    /// Its time passed while it could not be sent: it waits for the user.
    pub missed: bool,
    /// Being edited or rescheduled: it does not go out meanwhile (not
    /// persisted).
    pub held: bool,
    /// Why it came back from the outbox (the server refused it, or kept
    /// failing): shown with the missed entry (not persisted).
    pub error: Option<String>,
}

impl Scheduled {
    pub fn new(message: Message, send_at: i64) -> Self {
        Self {
            message,
            send_at,
            missed: false,
            held: false,
            error: None,
        }
    }

    /// A due message whose sending failed for good: back with the user as
    /// a missed entry, with the error.
    pub fn failed(message: Message, send_at: i64, error: String) -> Self {
        Self {
            missed: true,
            error: Some(error),
            ..Self::new(message, send_at)
        }
    }

    pub fn id(&self) -> &str {
        &self.message.id
    }
}

/// What [`ScheduledQueue::take_due`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Due {
    /// Taken out of the queue: to be sent now.
    pub send: Vec<Scheduled>,
    /// Marked missed just now.
    pub missed: Vec<String>,
}

/// The account's scheduled messages by message id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScheduledQueue {
    items: BTreeMap<String, Scheduled>,
}

impl ScheduledQueue {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Scheduled> {
        self.items.get(id)
    }

    /// Adds `s`, replacing an entry with the same id.
    pub fn insert(&mut self, s: Scheduled) {
        self.items.insert(s.message.id.clone(), s);
    }

    pub fn remove(&mut self, id: &str) -> Option<Scheduled> {
        self.items.remove(id)
    }

    /// A new send time; a missed message waits again.
    pub fn reschedule(&mut self, id: &str, at_ms: i64) -> bool {
        match self.items.get_mut(id) {
            Some(s) => {
                s.send_at = at_ms;
                s.missed = false;
                s.error = None;
                true
            }
            None => false,
        }
    }

    /// Replaces the text (empty text is refused).
    pub fn set_text(&mut self, id: &str, text: &str) -> bool {
        let text = text.trim();
        match self.items.get_mut(id) {
            Some(s) if !text.is_empty() && s.message.msg != text => {
                s.message.msg = text.to_owned();
                true
            }
            _ => false,
        }
    }

    /// Every entry, soonest first.
    pub fn sorted(&self) -> Vec<&Scheduled> {
        let mut v: Vec<&Scheduled> = self.items.values().collect();
        v.sort_by(|a, b| (a.send_at, a.id()).cmp(&(b.send_at, b.id())));
        v
    }

    /// Missed entries, oldest first.
    pub fn missed(&self) -> Vec<&Scheduled> {
        self.sorted().into_iter().filter(|s| s.missed).collect()
    }

    /// Entries of room `rid` (`tmid: None`) or of its thread `tmid`.
    pub fn count_in(&self, rid: &str, tmid: Option<&str>) -> usize {
        self.items
            .values()
            .filter(|s| s.message.rid == rid && s.message.tmid.as_deref() == tmid)
            .count()
    }

    /// When [`Self::take_due`] next has something to do after `now_ms`:
    /// the earliest send time ahead, or the end of the grace time of an
    /// overdue entry still waiting (not connected), when it turns missed.
    /// Missed entries wait for the user, held ones for their dialog to
    /// close (an update of its own).
    pub fn next_at(&self, now_ms: i64) -> Option<i64> {
        self.items
            .values()
            .filter(|s| !s.missed && !s.held)
            .map(|s| {
                if s.send_at > now_ms {
                    s.send_at
                } else {
                    s.send_at + MISSED_GRACE_MS + 1
                }
            })
            .filter(|at| *at > now_ms)
            .min()
    }

    /// Entries whose time came: within [`MISSED_GRACE_MS`] they are taken
    /// out to be sent while `online` (else they wait); later than that they
    /// are marked missed and stay.
    pub fn take_due(&mut self, now_ms: i64, online: bool) -> Due {
        let mut due = Due::default();
        let ids: Vec<String> = self
            .sorted()
            .into_iter()
            .filter(|s| !s.missed && !s.held && s.send_at <= now_ms)
            .map(|s| s.message.id.clone())
            .collect();
        for id in ids {
            let Some(s) = self.items.get_mut(&id) else {
                continue;
            };
            if now_ms - s.send_at > MISSED_GRACE_MS {
                s.missed = true;
                due.missed.push(id);
            } else if online && let Some(s) = self.items.remove(&id) {
                due.send.push(s);
            }
        }
        due
    }

    /// Holds entry `id` (and releases every other); returns whether
    /// something changed.
    pub fn hold_only(&mut self, id: Option<&str>) -> bool {
        let mut changed = false;
        for (k, s) in &mut self.items {
            let held = Some(k.as_str()) == id;
            if s.held != held {
                s.held = held;
                changed = true;
            }
        }
        changed
    }

    /// Drops the entries of rooms `keep` rejects (rooms left or gone).
    pub fn retain_rooms(&mut self, keep: impl Fn(&str) -> bool) -> bool {
        let before = self.items.len();
        self.items.retain(|_, s| keep(&s.message.rid));
        self.items.len() != before
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn msg(id: &str, rid: &str, tmid: Option<&str>) -> Message {
        Message {
            id: id.into(),
            rid: rid.into(),
            msg: format!("text {id}"),
            tmid: tmid.map(str::to_owned),
            ..Default::default()
        }
    }

    fn queue() -> ScheduledQueue {
        let mut q = ScheduledQueue::default();
        q.insert(Scheduled::new(msg("a", "R1", None), 10_000));
        q.insert(Scheduled::new(msg("b", "R1", Some("T1")), 5_000));
        q.insert(Scheduled::new(msg("c", "R2", None), 200_000));
        q
    }

    #[test]
    fn order_counts_and_edits() {
        let mut q = queue();
        let ids: Vec<&str> = q.sorted().iter().map(|s| s.id()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
        assert_eq!(q.count_in("R1", None), 1);
        assert_eq!(q.count_in("R1", Some("T1")), 1);
        assert_eq!(q.count_in("R3", None), 0);
        assert_eq!(q.next_at(0), Some(5_000));
        assert!(q.set_text("a", "  new  "));
        assert_eq!(q.get("a").unwrap().message.msg, "new");
        assert!(!q.set_text("a", "new"), "unchanged");
        assert!(!q.set_text("a", "   "), "empty refused");
        assert!(!q.set_text("x", "y"));
        assert!(q.reschedule("c", 1_000));
        assert_eq!(q.next_at(0), Some(1_000));
        assert!(!q.reschedule("x", 1));
        assert!(q.retain_rooms(|r| r != "R2"));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn due_entries_go_out_late_ones_are_missed() {
        let mut q = queue();
        // Nothing is due yet.
        assert_eq!(q.take_due(4_999, true), Due::default());
        // Offline: due entries wait within the grace time.
        let d = q.take_due(10_000, false);
        assert!(d.send.is_empty() && d.missed.is_empty());
        assert_eq!(q.len(), 3);
        // Online: both go out, soonest first.
        let d = q.take_due(5_000 + MISSED_GRACE_MS, true);
        let ids: Vec<&str> = d.send.iter().map(|s| s.id()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(q.len(), 1);
        // Too late (the app was off): missed, kept, not sent.
        let d = q.take_due(200_000 + MISSED_GRACE_MS + 1, true);
        assert!(d.send.is_empty());
        assert_eq!(d.missed, ["c"]);
        assert!(q.get("c").unwrap().missed);
        assert_eq!(q.next_at(400_000), None, "missed entries wait for the user");
        assert_eq!(q.missed().len(), 1);
        // Marked once.
        assert_eq!(q.take_due(400_000, true), Due::default());
        // Rescheduled: it waits again.
        q.reschedule("c", 500_000);
        assert!(!q.get("c").unwrap().missed);
        assert_eq!(q.take_due(500_000, true).send.len(), 1);
        assert!(q.is_empty());
    }

    #[test]
    fn held_entries_wait_and_follow_the_rules_once_released() {
        let mut q = queue();
        assert!(q.hold_only(Some("b")));
        assert!(!q.hold_only(Some("b")));
        // "a" is due and goes; "b" is held (its dialog is open).
        let d = q.take_due(10_000, true);
        let ids: Vec<&str> = d.send.iter().map(|s| s.id()).collect();
        assert_eq!(ids, ["a"]);
        assert!(q.get("b").is_some());
        // An overdue held entry does not hide a later one from the timer.
        q.insert(Scheduled::new(msg("d", "R2", None), 20_000));
        assert_eq!(q.next_at(10_000), Some(20_000));
        // One waiting offline wakes the timer when it turns missed.
        assert!(q.hold_only(Some("d")));
        assert_eq!(q.take_due(20_000, false), Due::default());
        assert!(q.hold_only(Some("b")));
        assert_eq!(q.next_at(20_000), Some(20_000 + MISSED_GRACE_MS + 1));
        q.remove("d");
        // Released after the grace time: missed, not sent.
        assert!(q.hold_only(None));
        let d = q.take_due(5_000 + MISSED_GRACE_MS + 1, true);
        assert_eq!(d.missed, ["b"]);
    }
}
