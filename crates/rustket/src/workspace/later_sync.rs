//! Syncs the account's Later state (`rustket_core::later`) and message
//! reminders (`rustket_core::reminders`) with its hidden service room: two
//! keys of the generic service sync ([`super::service_sync`]). Edits go into
//! pending overlays the Later view and the reminder timer use at once; the
//! app persists them in the config (`pending_service`) until the
//! subscriptions show them stored.

use rustket_core::later::{LaterBlob, LaterEdit};
use rustket_core::reminders::ReminderBlob;
use rustket_core::{Dirty, Workspace};

use super::service_sync::{BlobSync, ServiceKey};

/// The Later state as a synced key.
#[derive(Debug, Default)]
pub struct LaterKey;

impl ServiceKey for LaterKey {
    type Blob = LaterBlob;
    const DIRTY: Dirty = Dirty::LATER;
    const LOG: &'static str = "later";

    fn stored(ws: &Workspace) -> &LaterBlob {
        ws.later_blob()
    }

    fn overlay(ws: &Workspace) -> &LaterBlob {
        ws.later_overlay()
    }

    fn set_overlay(ws: &mut Workspace, overlay: LaterBlob) {
        ws.set_later_overlay(overlay);
    }

    /// A seed alone: the first start of an account must not create a
    /// service room (or fail where creating one is not allowed).
    fn bookkeeping(overlay: &LaterBlob) -> bool {
        overlay.status.is_empty() && overlay.due.is_empty()
    }

    fn unbudgeted(edit: &LaterEdit) -> bool {
        matches!(edit, LaterEdit::Seed { .. })
    }
}

/// The reminders as a synced key.
#[derive(Debug, Default)]
pub struct RemindersKey;

impl ServiceKey for RemindersKey {
    type Blob = ReminderBlob;
    const DIRTY: Dirty = Dirty::REMINDERS;
    const LOG: &'static str = "reminders";

    fn stored(ws: &Workspace) -> &ReminderBlob {
        ws.reminder_blob()
    }

    fn overlay(ws: &Workspace) -> &ReminderBlob {
        ws.reminder_overlay()
    }

    fn set_overlay(ws: &mut Workspace, overlay: ReminderBlob) {
        ws.set_reminder_overlay(overlay);
    }
}

/// Per-server Later sync (`ServerState::later`).
pub type LaterSync = BlobSync<LaterKey>;
/// Per-server reminder sync (`ServerState::reminders`).
pub type ReminderSync = BlobSync<RemindersKey>;

/// Messages of the Later sync.
pub type LaterMsg = super::service_sync::Msg<LaterBlob>;
/// Messages of the reminder sync.
pub type ReminderMsg = super::service_sync::Msg<ReminderBlob>;

impl LaterSync {
    /// Whether the account's first use of Later is still to be recorded:
    /// signed in, and neither the stored blob nor this device's edits know
    /// a seed.
    pub fn needs_seed(&self, ws: &Workspace) -> bool {
        self.account.is_some() && !ws.later().is_seeded()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::time::{Duration, Instant};

    use rocketchat::models::{
        FLAGS_KEY, LATER_KEY, MENTION_GROUPS_KEY, REMINDERS_KEY, ServiceFields, User,
    };
    use rustket_core::later::{LaterEdit, LaterStatus};
    use rustket_core::reminders::ReminderEdit;
    use rustket_core::service_sync::SyncBlob;
    use rustket_core::workspace::ConnectionState;

    use super::super::service_sync::{COALESCE, Outcome, key_checks, service_bytes};
    use super::*;

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        ws.apply_connection(ConnectionState::Connected);
        ws
    }

    #[test]
    fn both_keys_take_part_in_seed_checks_and_the_budget() {
        let keys: Vec<&str> = key_checks().iter().map(|k| k.key()).collect();
        for k in [FLAGS_KEY, MENTION_GROUPS_KEY, LATER_KEY, REMINDERS_KEY] {
            assert!(keys.contains(&k), "{k} in {keys:?}");
        }
        let mut ws = ws();
        let before = service_bytes(&ws, 0);
        let mut s = ReminderSync::default();
        s.follow_account(Some("me"), |_| ReminderBlob::default());
        let far = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let set = ReminderEdit::Set {
            rid: "r1".into(),
            tmid: None,
            mid: "m1".into(),
            at_ms: far,
        };
        assert_eq!(s.edit(&mut ws, &set, "dev", Instant::now()), Ok(true));
        assert!(service_bytes(&ws, 0) > before);
        assert_eq!(ws.reminders().get("m1").unwrap().at_ms, far);
    }

    #[test]
    fn seeding_runs_once_per_account() {
        let mut ws = ws();
        let mut s = LaterSync::default();
        assert!(!s.needs_seed(&ws), "signed out");
        s.follow_account(Some("me"), |_| LaterBlob::default());
        assert!(s.needs_seed(&ws));
        let seed = LaterEdit::Seed { at_ms: 1000 };
        assert_eq!(s.edit(&mut ws, &seed, "dev", Instant::now()), Ok(true));
        assert!(!s.needs_seed(&ws));
        assert_eq!(ws.later().default_status(Some(999)), LaterStatus::Archived);
        assert_eq!(s.edit(&mut ws, &seed, "dev", Instant::now()), Ok(false));
    }

    /// Live flags (not budgeted) past the budget: the seed still goes in,
    /// so it is not refused and retried on every update.
    #[test]
    fn the_seed_passes_a_full_budget() {
        use rustket_core::flags::{FlagColor, FlagEdit};
        let mut ws = ws();
        let mut flags = super::super::flags::FlagSync::default();
        flags.follow_account(Some("me"), |_| Default::default());
        let t0 = Instant::now();
        let mut n = 0;
        while service_bytes(&ws, chrono::Utc::now().timestamp_millis())
            <= rustket_core::service_sync::BUDGET_BYTES
        {
            let edit = FlagEdit::Room {
                rid: format!("{n:0>120}"),
                color: Some(FlagColor::Red),
            };
            assert_eq!(flags.edit(&mut ws, &edit, "dev", t0), Ok(true));
            n += 1;
        }
        let mut s = LaterSync::default();
        s.follow_account(Some("me"), |_| LaterBlob::default());
        let done = LaterEdit::Status {
            rid: "r1".into(),
            mid: "m1".into(),
            ts_ms: None,
            status: LaterStatus::Done,
        };
        assert!(s.edit(&mut ws, &done, "dev", t0).is_err(), "budgeted");
        let seed = LaterEdit::Seed { at_ms: 1000 };
        assert_eq!(s.edit(&mut ws, &seed, "dev", t0), Ok(true));
        assert!(!s.needs_seed(&ws));
    }

    /// A seed alone stays on this device (kept in the config) until a
    /// service room exists: no room is created for it and it is not shown
    /// as pending. Any other edit takes it along.
    #[test]
    fn a_seed_alone_waits_for_a_service_room() {
        use super::super::service_sync::Status;
        let mut ws = ws();
        let mut s = LaterSync::default();
        s.follow_account(Some("me"), |_| LaterBlob::default());
        let seed = LaterEdit::Seed { at_ms: 1000 };
        assert_eq!(s.edit(&mut ws, &seed, "dev", Instant::now()), Ok(true));
        assert!(!s.ready(&ws));
        assert_eq!(s.status(), Status::Synced);
        assert!(s.take_save().is_some(), "persisted");
        // A room created by another key's push: the seed goes out.
        s.adopt_created("svc");
        assert!(s.ready(&ws));
        // Without a room, a real edit takes the seed along.
        let mut t = LaterSync::default();
        t.follow_account(Some("me"), |_| LaterBlob::default());
        t.edit(&mut ws, &seed, "dev", Instant::now()).unwrap();
        let done = LaterEdit::Status {
            rid: "r1".into(),
            mid: "m1".into(),
            ts_ms: None,
            status: LaterStatus::Done,
        };
        assert_eq!(t.edit(&mut ws, &done, "dev", Instant::now()), Ok(true));
        assert!(t.ready(&ws));
        assert_eq!(t.status(), Status::Pending);
    }

    /// Later and reminders go out in the server's one combined push with
    /// the other keys.
    #[test]
    fn the_server_push_carries_later_and_reminders() {
        let entry = rustket_core::config::ServerEntry::new("http://localhost:3999", "t");
        let mut state = super::super::ServerState::from_entry(&entry, &Default::default()).unwrap();
        state.workspace = ws();
        state
            .later
            .follow_account(Some("me"), |_| LaterBlob::default());
        state
            .reminders
            .follow_account(Some("me"), |_| ReminderBlob::default());
        let t0 = Instant::now();
        let done = LaterEdit::Status {
            rid: "r1".into(),
            mid: "m1".into(),
            ts_ms: None,
            status: LaterStatus::Done,
        };
        assert_eq!(
            state.later.edit(&mut state.workspace, &done, "dev", t0),
            Ok(true)
        );
        let far = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let set = ReminderEdit::Set {
            rid: "r1".into(),
            tmid: None,
            mid: "m1".into(),
            at_ms: far,
        };
        assert_eq!(
            state.reminders.edit(&mut state.workspace, &set, "dev", t0),
            Ok(true)
        );
        let parts = state.service_parts(t0 + COALESCE).unwrap();
        let keys: Vec<(&'static str, u64)> = parts.iter().map(|p| p.key()).collect();
        assert_eq!(
            keys.iter().map(|k| k.0).collect::<Vec<_>>(),
            [LATER_KEY, REMINDERS_KEY]
        );
        let mut f = ServiceFields::new("me");
        f.set(LATER_KEY, state.later.overlay().to_value_at(0));
        f.set(REMINDERS_KEY, state.reminders.overlay().to_value_at(0));
        let o = Outcome::for_tests(keys, vec![f]);
        let pushed = state.service_pushed(&o, t0 + COALESCE);
        let later = pushed.later.unwrap();
        let reminders = pushed.reminders.unwrap();
        assert!(state.later.update(later, t0 + COALESCE).is_empty());
        assert!(state.reminders.update(reminders, t0 + COALESCE).is_empty());
        assert!(!state.later.in_flight && !state.reminders.in_flight);
        assert_eq!(state.later.created.as_deref(), Some("svc"));
        assert_eq!(state.reminders.created.as_deref(), Some("svc"));
        // The next edit waits for the server's push spacing, not only for
        // its own coalescing.
        let next = LaterEdit::Status {
            rid: "r1".into(),
            mid: "m2".into(),
            ts_ms: None,
            status: LaterStatus::Done,
        };
        let t1 = t0 + COALESCE + Duration::from_secs(1);
        assert_eq!(
            state.later.edit(&mut state.workspace, &next, "dev", t1),
            Ok(true)
        );
        assert!(state.service_parts(t1 + COALESCE).is_none());
        let spaced = t0 + COALESCE + super::super::service_sync::PUSH_SPACING;
        assert_eq!(
            state.later.plan(&state.workspace, t1 + COALESCE),
            super::super::service_sync::Next::Wait(spaced)
        );
        assert!(state.service_parts(spaced).is_some());
    }
}
