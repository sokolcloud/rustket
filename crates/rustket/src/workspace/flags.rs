//! Syncs the account's flags with its hidden service room
//! (`rocketchat::models::service_room`, [`rustket_core::flags`]).
//!
//! The flags are one key of the generic service sync
//! ([`super::service_sync`], [`FlagsKey`]): edits go into a pending overlay
//! ([`FlagBlob`]) that the sidebar shows at once and the app persists in the
//! config (`pending_flags`), and the server's keys are pushed together. On
//! top of that the flag sync hides the service rooms: a service room seen
//! open again is hidden again.
//!
//! Nothing is ever unflagged automatically (a parent that is gone for this
//! device may be fine for another one).

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::Error;
use rustket_core::flags::FlagBlob;
use rustket_core::{Dirty, Workspace};

use super::service_sync::{BlobSync, MAX_PAUSE, RATE_LIMIT_PAUSE, ServiceKey, retry_delay};
pub use super::service_sync::{
    COALESCE, Out, PUSH_ATTEMPTS, PushFailure, ServiceStore, Status, VERIFY_AFTER,
};
use crate::error::{Failure, Res};
use crate::session::SessionHandle;

// Used by the tests below (`use super::*`).
#[cfg(test)]
use {
    super::service_sync::Next,
    rocketchat::models::{FLAGS_KEY, Room, ServiceFields},
    rustket_core::flags::FlagEdit,
    rustket_core::workspace::ConnectionState,
};

/// The flags as a synced key.
#[derive(Debug, Default)]
pub struct FlagsKey;

impl ServiceKey for FlagsKey {
    type Blob = FlagBlob;
    const DIRTY: Dirty = Dirty::FLAGS;
    const LOG: &'static str = "flags";
    // Flags keep their own cap (`MAX_FLAGGED_ITEMS`); their live entries
    // still count against the other keys.
    const BUDGETED: bool = false;

    fn stored(ws: &Workspace) -> &FlagBlob {
        ws.flag_blob()
    }

    fn overlay(ws: &Workspace) -> &FlagBlob {
        ws.flag_overlay()
    }

    fn set_overlay(ws: &mut Workspace, overlay: FlagBlob) {
        ws.set_flag_overlay(overlay);
    }
}

/// A finished push of the flags.
pub type Pushed = super::service_sync::Pushed<FlagBlob>;

#[derive(Debug, Clone)]
pub enum Msg {
    Pushed {
        epoch: u64,
        res: Result<Pushed, PushFailure>,
    },
    Hidden {
        epoch: u64,
        rid: String,
        res: Res<()>,
    },
    /// The pacing timer set for `at` fired.
    Pace { at: Instant },
}

/// Per-server flag sync (`ServerState::flags`): the flags' [`BlobSync`]
/// (reached through `Deref`) plus hiding the service rooms.
#[derive(Debug, Default)]
pub struct FlagSync {
    pub(super) sync: BlobSync<FlagsKey>,
    /// Service rooms a hide was sent for in this session (a failed one is
    /// sent again).
    hide_sent: HashSet<String>,
    /// Failed hides: failures so far, and not before when to send again.
    hide_retry: HashMap<String, (u32, Instant)>,
}

impl Deref for FlagSync {
    type Target = BlobSync<FlagsKey>;

    fn deref(&self) -> &Self::Target {
        &self.sync
    }
}

impl DerefMut for FlagSync {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sync
    }
}

impl FlagSync {
    /// Logout: everything goes.
    pub fn reset(&mut self) {
        self.sync.reset();
        self.hide_sent.clear();
        self.hide_retry.clear();
    }

    /// The pending times the planner waits for, for the pacing timer.
    fn next_wake(&self, ws: &Workspace, now: Instant) -> Option<Instant> {
        let hide = self
            .hide_retry
            .values()
            .map(|&(_, at)| at)
            .filter(|&at| at > now)
            .min();
        self.sync
            .next_wake(ws, now)
            .into_iter()
            .chain(hide.filter(|_| self.sync.live(ws)))
            .min()
    }

    /// Per update: follows the session, prunes and applies the overlay,
    /// hides service rooms seen open and sets the pacing timer. The push
    /// starts with the server's other keys
    /// ([`super::service_sync::start_push`]).
    pub fn tick(
        &mut self,
        ws: &mut Workspace,
        session: Option<&SessionHandle>,
        now: Instant,
    ) -> Task<Msg> {
        if self.sync.follow(ws, session, now) {
            self.hide_sent.clear();
            self.hide_retry.clear();
        }
        let Some(h) = session else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        if self.sync.live(ws) {
            tasks.extend(self.hide_open(ws, h, now));
        }
        if let Some(at) = self.next_wake(ws, now) {
            tasks.push(self.sync.schedule(at, now).map(|at| Msg::Pace { at }));
        }
        Task::batch(tasks)
    }

    fn hide_open(&mut self, ws: &Workspace, h: &SessionHandle, now: Instant) -> Vec<Task<Msg>> {
        let mut tasks = Vec::new();
        for rid in ws.service_rooms() {
            let open = ws
                .room(rid)
                .and_then(|e| e.sub.as_ref())
                .is_some_and(|s| s.open);
            let waiting = self.hide_retry.get(rid).is_some_and(|&(_, at)| now < at);
            if !open || waiting || !self.hide_sent.insert(rid.clone()) {
                continue;
            }
            let (rest, epoch, rid) = (h.rest().clone(), self.sync.epoch, rid.clone());
            tasks.push(Task::perform(
                {
                    let rid = rid.clone();
                    async move { rest.hide_service_room(&rid).await.map_err(Failure::from) }
                },
                move |res| Msg::Hidden {
                    epoch,
                    rid: rid.clone(),
                    res,
                },
            ));
        }
        tasks
    }

    /// Applies a result.
    pub fn update(&mut self, msg: Msg, now: Instant) -> Vec<Out> {
        match msg {
            Msg::Pace { at } => {
                self.sync.paced(at);
                Vec::new()
            }
            Msg::Hidden { epoch, rid, res } => {
                if epoch != self.sync.epoch {
                    return Vec::new();
                }
                match res {
                    Ok(()) => {
                        self.hide_retry.remove(&rid);
                    }
                    Err(f) => {
                        let failures = self.hide_retry.get(&rid).map_or(0, |&(n, _)| n) + 1;
                        let wait = match f.error() {
                            Error::RateLimited { retry_after, .. } => {
                                retry_after.unwrap_or(RATE_LIMIT_PAUSE)
                            }
                            _ => retry_delay(failures),
                        }
                        .clamp(Duration::from_secs(1), MAX_PAUSE);
                        tracing::warn!(%rid, error = %f, "flags: could not hide the service room");
                        self.hide_sent.remove(&rid);
                        self.hide_retry.insert(rid, (failures, now + wait));
                    }
                }
                Vec::new()
            }
            Msg::Pushed { epoch, res } => self.sync.pushed(epoch, res, now),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::{RoomType, Subscription, User, UserRef};
    use rustket_core::Change;
    use rustket_core::flags::FlagColor;
    use serde_json::json;

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

    fn service_sub(blob: &FlagBlob, open: bool) -> Subscription {
        let mut f = ServiceFields::new("me");
        f.set(FLAGS_KEY, blob.to_value());
        Subscription {
            id: "sub-svc".into(),
            rid: "svc".into(),
            name: "rustket-sync-me".into(),
            t: RoomType::Private,
            u: UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            },
            open,
            roles: Some(vec!["owner".into()]),
            custom_fields: Some(f.to_custom_fields()),
            ts: RcDate::from_millis(1),
            ..Default::default()
        }
    }

    /// The service room of [`service_sub`]: the user's own.
    fn service_room() -> Room {
        Room {
            id: "svc".into(),
            t: RoomType::Private,
            u: Some(UserRef {
                id: "me".into(),
                username: Some("me".into()),
                name: None,
            }),
            users_count: Some(1),
            custom_fields: Some(ServiceFields::new("me").to_custom_fields()),
            // From before bindings existed: accepted without one.
            ts: RcDate::from_millis(1_700_000_000_000),
            ..Default::default()
        }
    }

    fn room(rid: &str, c: Option<FlagColor>) -> FlagEdit {
        FlagEdit::Room {
            rid: rid.into(),
            color: c,
        }
    }

    fn synced(ws: &Workspace) -> FlagSync {
        let mut s = FlagSync::default();
        s.follow_account(ws.me().map(|m| m.id.as_str()), |_| FlagBlob::default());
        s
    }

    fn pushed(s: &mut FlagSync, written: &FlagBlob, stored: &FlagBlob, now: Instant) {
        let epoch = s.epoch;
        s.in_flight = true;
        s.due = None;
        s.update(
            Msg::Pushed {
                epoch,
                res: Ok(Pushed {
                    written: written.clone(),
                    stored: stored.clone(),
                    rid: Some("svc".into()),
                }),
            },
            now,
        );
    }

    #[test]
    fn edits_show_at_once_and_push_after_coalescing() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        assert_eq!(s.plan(&ws, t0), Next::Idle);
        assert!(
            s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
                .unwrap()
        );
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        assert!(
            !s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
                .unwrap()
        );
        assert_eq!(s.status(), Status::Pending);
        let (uid, saved) = s.take_save().unwrap();
        assert_eq!((uid.as_str(), &saved), ("me", s.overlay()));
        assert!(s.take_save().is_none());
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + COALESCE));
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
        // Not while showing the offline cache or disconnected.
        ws.apply_connection(ConnectionState::Disconnected);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Idle);
    }

    #[test]
    fn the_overlay_leaves_only_with_the_subscription_echo() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        s.observe(&ws, t0);
        // The push's own read does not prune: no flicker before the echo.
        assert_eq!(s.overlay(), &written);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Idle, "already stored");
        ws.apply_subscription(Change::Inserted, service_sub(&written, false));
        ws.apply_room(Change::Inserted, service_room());
        s.observe(&ws, t0);
        s.apply(&mut ws);
        assert!(s.overlay().is_empty());
        assert_eq!(s.status(), Status::Synced);
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        assert!(s.take_save().is_some_and(|(_, b)| b.is_empty()));
    }

    #[test]
    fn entries_dropped_by_another_device_are_written_again() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        ws.apply_subscription(Change::Inserted, service_sub(&written, false));
        ws.apply_room(Change::Inserted, service_room());
        s.observe(&ws, t0);
        assert!(s.overlay().is_empty());
        // Another device that read before our write replaces the blob.
        let mut theirs = FlagBlob::default();
        theirs
            .edit(
                &FlagBlob::default(),
                &room("r2", Some(FlagColor::Blue)),
                1,
                "other",
            )
            .unwrap();
        ws.apply_subscription(Change::Updated, service_sub(&theirs, false));
        s.observe(&ws, t0 + COALESCE);
        assert_eq!(s.overlay(), &written);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
    }

    #[test]
    fn entries_never_shown_are_checked_after_the_window() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let written = s.overlay().clone();
        pushed(&mut s, &written, &written, t0);
        s.observe(&ws, t0 + VERIFY_AFTER / 2);
        assert_eq!(s.plan(&ws, t0 + VERIFY_AFTER / 2), Next::Idle);
        assert_eq!(s.next_wake(&ws, t0), Some(t0 + VERIFY_AFTER));
        s.observe(&ws, t0 + VERIFY_AFTER);
        assert_eq!(
            s.plan(&ws, t0 + VERIFY_AFTER),
            Next::Push,
            "read the store again"
        );
    }

    #[test]
    fn failures_back_off_and_give_up() {
        let mut ws = ws();
        let mut s = synced(&ws);
        let t0 = Instant::now();
        s.edit(&mut ws, &room("r1", Some(FlagColor::Red)), "dev", t0)
            .unwrap();
        let fail = |s: &mut FlagSync, e: Error| {
            s.in_flight = true;
            s.due = None;
            let epoch = s.epoch;
            s.update(
                Msg::Pushed {
                    epoch,
                    res: Err(PushFailure::Store(Failure::from(e))),
                },
                t0,
            )
        };
        assert!(fail(&mut s, Error::Timeout("push")).is_empty());
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + retry_delay(1)));
        s.update(
            Msg::Pushed {
                epoch: s.epoch,
                res: Err(PushFailure::Conflict),
            },
            t0,
        );
        assert_eq!(s.plan(&ws, t0), Next::Wait(t0 + retry_delay(2)));
        // A stale result of an earlier session changes nothing.
        s.update(
            Msg::Pushed {
                epoch: s.epoch.wrapping_sub(1),
                res: Err(PushFailure::Conflict),
            },
            t0,
        );
        assert_eq!(s.failures, 2);
        let denied = Error::from_response(
            400,
            None,
            br#"{"success":false,"error":"Not allowed [error-action-not-allowed]","errorType":"error-action-not-allowed"}"#,
        );
        let out = fail(&mut s, denied);
        assert!(matches!(out.as_slice(), [Out::Failed(_)]));
        assert!(matches!(s.status(), Status::Failed(_)));
        assert_eq!(s.plan(&ws, t0 + MAX_PAUSE), Next::Idle);
        // The edits stay shown; the next edit tries again.
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Red));
        s.edit(&mut ws, &room("r2", Some(FlagColor::Blue)), "dev", t0)
            .unwrap();
        assert_eq!(s.status(), Status::Pending);
        assert_eq!(s.plan(&ws, t0 + COALESCE), Next::Push);
    }

    #[test]
    fn failed_hides_are_sent_again() {
        let mut ws = ws();
        ws.apply_subscription(Change::Inserted, service_sub(&FlagBlob::default(), true));
        ws.apply_room(Change::Inserted, service_room());
        assert_eq!(ws.service_rooms(), ["svc"]);
        let mut s = synced(&ws);
        let t0 = Instant::now();
        let hidden = |s: &mut FlagSync, res: Res<()>, now: Instant| {
            s.hide_sent.insert("svc".into());
            let epoch = s.epoch;
            s.update(
                Msg::Hidden {
                    epoch,
                    rid: "svc".into(),
                    res,
                },
                now,
            );
        };
        let limited = Error::RateLimited {
            retry_after: Some(Duration::from_secs(40)),
            message: None,
        };
        hidden(&mut s, Err(Failure::from(limited)), t0);
        assert!(!s.hide_sent.contains("svc"));
        let at = t0 + Duration::from_secs(40);
        assert_eq!(s.hide_retry.get("svc"), Some(&(1, at)));
        assert_eq!(s.next_wake(&ws, t0), Some(at));
        hidden(&mut s, Err(Failure::from(Error::Timeout("hide"))), at);
        assert_eq!(s.hide_retry.get("svc"), Some(&(2, at + retry_delay(2))));
        hidden(&mut s, Ok(()), at + retry_delay(2));
        assert!(s.hide_retry.is_empty() && s.hide_sent.contains("svc"));
        assert_eq!(s.next_wake(&ws, at + retry_delay(2)), None);
    }

    #[test]
    fn accounts_keep_their_own_pending_edits() {
        let mut ws = ws();
        let mut s = FlagSync::default();
        let mut stored = FlagBlob::default();
        stored
            .edit(
                &FlagBlob::default(),
                &room("r1", Some(FlagColor::Green)),
                1,
                "d",
            )
            .unwrap();
        s.follow_account(Some("me"), |uid| {
            assert_eq!(uid, "me");
            stored.clone()
        });
        assert!(s.take_save().is_none(), "loaded, not changed");
        s.apply(&mut ws);
        assert_eq!(ws.room_flag("r1"), Some(FlagColor::Green));
        // Same account: not reloaded.
        s.follow_account(Some("me"), |_| unreachable!());
        // Signed in as someone else than the overlay's account: no push.
        s.follow_account(Some("other"), |_| stored.clone());
        assert_eq!(s.plan(&ws, Instant::now()), Next::Idle);
        s.follow_account(None, |_| unreachable!());
        assert!(
            !s.edit(
                &mut ws,
                &room("r9", Some(FlagColor::Red)),
                "d",
                Instant::now()
            )
            .unwrap()
        );
    }

    #[test]
    fn service_store_payloads() {
        // The blob goes under its key next to the marker and unknown keys.
        let mut f = ServiceFields::parse(Some(&json!({
            "rustketService": { "v": 1, "owner": "me" },
            "other": 1
        })))
        .unwrap();
        let mut b = FlagBlob::default();
        b.edit(
            &FlagBlob::default(),
            &room("r1", Some(FlagColor::Red)),
            7,
            "d",
        )
        .unwrap();
        f.set(FLAGS_KEY, b.to_value());
        let v = f.to_custom_fields();
        assert_eq!(v["other"], 1);
        assert_eq!(v["rustketService"]["owner"], "me");
        assert_eq!(
            FlagBlob::from_value(v.get(FLAGS_KEY)).to_value(),
            b.to_value()
        );
        assert!(retry_delay(1) < retry_delay(2));
        assert_eq!(retry_delay(50), Duration::from_secs(300));
    }
}
