//! Trigger ids: every interaction registers one; a server interaction is
//! accepted only for a registered, unused trigger (docs/uikit.md §4.2).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rocketchat::models::ButtonContext;

/// How long a trigger stays valid after its HTTP request finished (RN rule).
pub const TRIGGER_TTL: Duration = Duration::from_secs(5);
/// Client time limit of one interaction request.
pub const REQUEST_TIMEOUT: Duration = rocketchat::rest::UI_INTERACTION_TIMEOUT;

/// What issued a trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginKind {
    MessageBlock,
    ViewBlock,
    ViewSubmit,
    ViewClosed,
    ActionButton(ButtonContext),
    SlashCommand,
}

/// Context of an interaction, used to place what the server answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub kind: OriginKind,
    pub rid: Option<String>,
    pub tmid: Option<String>,
    pub mid: Option<String>,
    pub view_id: Option<String>,
    /// The element that was used (for its loading state).
    pub action_id: Option<String>,
    /// Instance of the view `view_id` the interaction came from: view ids are
    /// often fixed strings, so a reopened view must not take a stale answer.
    pub generation: Option<u64>,
}

impl Origin {
    pub fn new(kind: OriginKind) -> Self {
        Self {
            kind,
            rid: None,
            tmid: None,
            mid: None,
            view_id: None,
            action_id: None,
            generation: None,
        }
    }

    pub fn rid(mut self, rid: Option<&str>) -> Self {
        self.rid = rid.map(str::to_owned);
        self
    }

    pub fn tmid(mut self, tmid: Option<&str>) -> Self {
        self.tmid = tmid.map(str::to_owned);
        self
    }

    pub fn mid(mut self, mid: Option<&str>) -> Self {
        self.mid = mid.map(str::to_owned);
        self
    }

    pub fn view(mut self, view_id: &str) -> Self {
        self.view_id = Some(view_id.to_owned());
        self
    }

    pub fn generation(mut self, generation: u64) -> Self {
        self.generation = Some(generation);
        self
    }

    pub fn action(mut self, action_id: &str) -> Self {
        self.action_id = Some(action_id.to_owned());
        self
    }
}

#[derive(Debug, Clone)]
pub struct Trigger {
    /// `None` for built-in slash commands: pushes for them are ignored.
    pub app_id: Option<String>,
    pub origin: Origin,
    issued: Instant,
    settled: Option<Instant>,
    consumed: bool,
    /// `type` of the server interaction accepted for this trigger.
    pub handled: Option<&'static str>,
}

impl Trigger {
    fn expired(&self, now: Instant) -> bool {
        match self.settled {
            Some(s) => now.duration_since(s) > TRIGGER_TTL,
            None => now.duration_since(self.issued) > TRIGGER_TTL + REQUEST_TIMEOUT,
        }
    }

    pub fn is_consumed(&self) -> bool {
        self.consumed
    }

    pub fn is_settled(&self) -> bool {
        self.settled.is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct TriggerRegistry {
    pending: HashMap<String, Trigger>,
}

impl TriggerRegistry {
    /// Registers a new 17-character trigger id.
    pub fn issue(&mut self, app_id: Option<&str>, origin: Origin, now: Instant) -> String {
        self.prune(now);
        let id = rocketchat::ids::random_id();
        self.pending.insert(
            id.clone(),
            Trigger {
                app_id: app_id.map(str::to_owned),
                origin,
                issued: now,
                settled: None,
                consumed: false,
                handled: None,
            },
        );
        id
    }

    /// The HTTP request of `trigger_id` finished; it stays valid for [`TRIGGER_TTL`].
    pub fn settle(&mut self, trigger_id: &str, now: Instant) {
        if let Some(t) = self.pending.get_mut(trigger_id) {
            t.settled.get_or_insert(now);
        }
    }

    /// Accepts a server interaction for `trigger_id`: `Some` only for a known,
    /// unexpired, unused trigger of an app (consumes it).
    pub fn accept(&mut self, trigger_id: &str, now: Instant) -> Option<&mut Trigger> {
        let t = self.pending.get_mut(trigger_id)?;
        if t.consumed || t.expired(now) || t.app_id.is_none() {
            return None;
        }
        t.consumed = true;
        Some(t)
    }

    pub fn get(&self, trigger_id: &str) -> Option<&Trigger> {
        self.pending.get(trigger_id)
    }

    pub fn get_mut(&mut self, trigger_id: &str) -> Option<&mut Trigger> {
        self.pending.get_mut(trigger_id)
    }

    /// Forgets expired triggers.
    pub fn prune(&mut self, now: Instant) {
        self.pending.retain(|_, t| !t.expired(now));
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new(OriginKind::MessageBlock).rid(Some("r"))
    }

    #[test]
    fn accept_consumes_once() {
        let now = Instant::now();
        let mut reg = TriggerRegistry::default();
        let id = reg.issue(Some("app"), origin(), now);
        assert_eq!(id.len(), 17);
        assert!(reg.accept("unknown", now).is_none());
        assert!(reg.accept(&id, now).is_some());
        assert!(reg.accept(&id, now).is_none());
        assert!(reg.get(&id).is_some_and(Trigger::is_consumed));
    }

    #[test]
    fn expiry_follows_settle() {
        let t0 = Instant::now();
        let mut reg = TriggerRegistry::default();
        let a = reg.issue(Some("app"), origin(), t0);
        let b = reg.issue(Some("app"), origin(), t0);
        // A slow request: still valid 8 s after issue, 4 s after settling.
        reg.settle(&a, t0 + Duration::from_secs(4));
        assert!(reg.accept(&a, t0 + Duration::from_secs(8)).is_some());
        // Never settled: valid until issue + TTL + request timeout.
        assert!(reg.accept(&b, t0 + Duration::from_secs(11)).is_none());
        let c = reg.issue(Some("app"), origin(), t0);
        reg.settle(&c, t0);
        assert!(reg.accept(&c, t0 + Duration::from_secs(6)).is_none());
        reg.prune(t0 + Duration::from_secs(60));
        assert!(reg.is_empty());
    }

    #[test]
    fn builtin_command_triggers_are_never_accepted() {
        let now = Instant::now();
        let mut reg = TriggerRegistry::default();
        let id = reg.issue(None, Origin::new(OriginKind::SlashCommand), now);
        assert!(reg.accept(&id, now).is_none());
    }
}
