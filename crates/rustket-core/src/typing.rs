//! Typing / recording / uploading indicators (`{rid}/user-activity`) and the
//! composer-side [`TypingThrottle`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// An activity expires this long after its last event.
pub const ACTIVITY_TIMEOUT: Duration = Duration::from_secs(15);
/// The composer re-sends "typing" at most this often.
pub const TYPING_RESEND: Duration = Duration::from_secs(3);
/// The composer sends "stop" after this much idle time.
pub const TYPING_IDLE: Duration = Duration::from_secs(5);

/// Activity names used by Rocket.Chat.
pub const USER_TYPING: &str = "user-typing";
pub const USER_RECORDING: &str = "user-recording";
pub const USER_UPLOADING: &str = "user-uploading";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Activity {
    Typing,
    Recording,
    Uploading,
}

impl Activity {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            USER_TYPING => Some(Self::Typing),
            USER_RECORDING => Some(Self::Recording),
            USER_UPLOADING => Some(Self::Uploading),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Typing => USER_TYPING,
            Self::Recording => USER_RECORDING,
            Self::Uploading => USER_UPLOADING,
        }
    }

    /// i18n key for a label like "alice is typing…".
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Typing => "typing.typing",
            Self::Recording => "typing.recording",
            Self::Uploading => "typing.uploading",
        }
    }
}

/// Typing context: a room, or a thread inside it.
pub type TypingKey = (String, Option<String>);

#[derive(Debug, Clone)]
struct Active {
    username: String,
    activity: Activity,
    last: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct TypingState {
    map: HashMap<TypingKey, Vec<Active>>,
}

/// Structured typing indicator for i18n: "a, b and 3 others are typing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypingLabel {
    /// Up to [`TypingLabel::MAX_NAMES`] usernames, in order of first activity.
    pub usernames: Vec<String>,
    /// Total number of active users (≥ `usernames.len()`).
    pub count: usize,
    /// Typing if anyone types, else recording, else uploading.
    pub activity: Activity,
}

impl TypingLabel {
    pub const MAX_NAMES: usize = 3;

    pub fn others(&self) -> usize {
        self.count - self.usernames.len()
    }
}

impl TypingState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a `user-activity` event. An empty or unknown activity list stops
    /// that user's activity. Returns whether the visible state changed.
    pub fn apply(
        &mut self,
        rid: &str,
        tmid: Option<&str>,
        username: &str,
        activities: &[String],
        now: Instant,
    ) -> bool {
        let key = (rid.to_owned(), tmid.map(str::to_owned));
        let activity = activities.iter().filter_map(|a| Activity::parse(a)).min();
        let list = self.map.entry(key.clone()).or_default();
        let pos = list.iter().position(|a| a.username == username);
        let changed = match (activity, pos) {
            (Some(activity), Some(i)) => {
                let changed = list[i].activity != activity;
                list[i].activity = activity;
                list[i].last = now;
                changed
            }
            (Some(activity), None) => {
                list.push(Active {
                    username: username.to_owned(),
                    activity,
                    last: now,
                });
                true
            }
            (None, Some(i)) => {
                list.remove(i);
                true
            }
            (None, None) => false,
        };
        if list.is_empty() {
            self.map.remove(&key);
        }
        changed
    }

    /// Drops activities older than [`ACTIVITY_TIMEOUT`]. Returns whether any expired.
    pub fn expire(&mut self, now: Instant) -> bool {
        let mut changed = false;
        self.map.retain(|_, list| {
            let before = list.len();
            list.retain(|a| now.saturating_duration_since(a.last) < ACTIVITY_TIMEOUT);
            changed |= list.len() != before;
            !list.is_empty()
        });
        changed
    }

    /// When the next activity would expire (for scheduling [`Self::expire`]).
    pub fn next_expiry(&self) -> Option<Instant> {
        self.map
            .values()
            .flatten()
            .map(|a| a.last + ACTIVITY_TIMEOUT)
            .min()
    }

    /// Stops a user's activity everywhere in a room (e.g. their message arrived).
    pub fn stop_user(&mut self, rid: &str, tmid: Option<&str>, username: &str) -> bool {
        self.apply(rid, tmid, username, &[], Instant::now())
    }

    /// Active usernames for a room/thread, excluding `exclude` (the current user).
    pub fn active(
        &self,
        rid: &str,
        tmid: Option<&str>,
        now: Instant,
        exclude: Option<&str>,
    ) -> Vec<(&str, Activity)> {
        let key = (rid.to_owned(), tmid.map(str::to_owned));
        self.map
            .get(&key)
            .map(|list| {
                list.iter()
                    .filter(|a| now.saturating_duration_since(a.last) < ACTIVITY_TIMEOUT)
                    .filter(|a| Some(a.username.as_str()) != exclude)
                    .map(|a| (a.username.as_str(), a.activity))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn label(
        &self,
        rid: &str,
        tmid: Option<&str>,
        now: Instant,
        exclude: Option<&str>,
    ) -> Option<TypingLabel> {
        let active = self.active(rid, tmid, now, exclude);
        let activity = active.iter().map(|(_, a)| *a).min()?;
        Some(TypingLabel {
            usernames: active
                .iter()
                .take(TypingLabel::MAX_NAMES)
                .map(|(u, _)| (*u).to_owned())
                .collect(),
            count: active.len(),
            activity,
        })
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// What the composer should send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingAction {
    /// `user-activity` with `["user-typing"]`.
    Start,
    /// `user-activity` with `[]`.
    Stop,
}

/// Composer typing notifications: "start" at most every [`TYPING_RESEND`] while
/// the user types, "stop" after [`TYPING_IDLE`] without input, on send, on clearing
/// the text or on leaving the room.
#[derive(Debug, Clone, Default)]
pub struct TypingThrottle {
    last_sent: Option<Instant>,
    last_input: Option<Instant>,
}

impl TypingThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.last_sent.is_some()
    }

    /// The user edited the text. Call [`Self::on_cleared`] instead when it became empty.
    pub fn on_input(&mut self, now: Instant) -> Option<TypingAction> {
        self.last_input = Some(now);
        match self.last_sent {
            Some(t) if now.saturating_duration_since(t) < TYPING_RESEND => None,
            _ => {
                self.last_sent = Some(now);
                Some(TypingAction::Start)
            }
        }
    }

    /// Periodic check; returns `Stop` after the idle timeout.
    pub fn tick(&mut self, now: Instant) -> Option<TypingAction> {
        match (self.last_sent, self.last_input) {
            (Some(_), Some(i)) if now.saturating_duration_since(i) >= TYPING_IDLE => self.stop(),
            _ => None,
        }
    }

    /// When [`Self::tick`] should run next.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.last_sent?;
        self.last_input.map(|i| i + TYPING_IDLE)
    }

    pub fn on_send(&mut self) -> Option<TypingAction> {
        self.stop()
    }

    pub fn on_cleared(&mut self) -> Option<TypingAction> {
        self.stop()
    }

    pub fn on_leave(&mut self) -> Option<TypingAction> {
        self.stop()
    }

    fn stop(&mut self) -> Option<TypingAction> {
        self.last_input = None;
        self.last_sent.take().map(|_| TypingAction::Stop)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn acts(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn typing_lifecycle() {
        let t0 = Instant::now();
        let mut s = TypingState::new();
        assert!(s.apply("r", None, "alice", &acts(&[USER_TYPING]), t0));
        assert!(!s.apply("r", None, "alice", &acts(&[USER_TYPING]), t0));
        assert!(s.apply("r", None, "bob", &acts(&[USER_RECORDING]), t0));
        assert!(s.apply("r", Some("th"), "carol", &acts(&[USER_TYPING]), t0));
        let l = s.label("r", None, t0, None).unwrap();
        assert_eq!(l.usernames, vec!["alice", "bob"]);
        assert_eq!(l.count, 2);
        assert_eq!(l.activity, Activity::Typing);
        // Own username excluded.
        let l = s.label("r", None, t0, Some("alice")).unwrap();
        assert_eq!(l.usernames, vec!["bob"]);
        assert_eq!(l.activity, Activity::Recording);
        // Thread is separate.
        assert_eq!(s.label("r", Some("th"), t0, None).map(|l| l.count), Some(1));
        // Empty list stops.
        assert!(s.apply("r", None, "alice", &[], t0));
        assert_eq!(s.label("r", None, t0, None).map(|l| l.count), Some(1));
        // Expiry.
        let later = t0 + ACTIVITY_TIMEOUT;
        assert!(s.label("r", None, later, None).is_none());
        assert_eq!(s.next_expiry(), Some(t0 + ACTIVITY_TIMEOUT));
        assert!(s.expire(later));
        assert!(s.next_expiry().is_none());
    }

    #[test]
    fn label_truncates_names() {
        let t0 = Instant::now();
        let mut s = TypingState::new();
        for u in ["a", "b", "c", "d", "e"] {
            s.apply("r", None, u, &acts(&[USER_TYPING]), t0);
        }
        let l = s.label("r", None, t0, None).unwrap();
        assert_eq!(l.usernames.len(), 3);
        assert_eq!(l.count, 5);
        assert_eq!(l.others(), 2);
    }

    #[test]
    fn throttle() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut th = TypingThrottle::new();
        assert_eq!(th.on_input(t0), Some(TypingAction::Start));
        assert_eq!(th.on_input(t0 + ms(1000)), None);
        assert_eq!(th.on_input(t0 + ms(2999)), None);
        assert_eq!(th.on_input(t0 + ms(3000)), Some(TypingAction::Start));
        assert_eq!(th.tick(t0 + ms(7999)), None);
        assert_eq!(th.next_deadline(), Some(t0 + ms(8000)));
        assert_eq!(th.tick(t0 + ms(8000)), Some(TypingAction::Stop));
        assert_eq!(th.tick(t0 + ms(9000)), None);
        assert_eq!(th.next_deadline(), None);
        assert_eq!(th.on_send(), None);
        assert_eq!(th.on_input(t0 + ms(9000)), Some(TypingAction::Start));
        assert_eq!(th.on_send(), Some(TypingAction::Stop));
        assert_eq!(th.on_input(t0 + ms(9100)), Some(TypingAction::Start));
        assert_eq!(th.on_leave(), Some(TypingAction::Stop));
        assert!(!th.is_active());
    }
}
