//! App wiring of quiet time (`rustket_core::quiet`, `crate::quiet`): manual
//! pauses and working hours, "Busy while paused" with the status restored
//! afterwards, statuses that clear themselves, and the one timer that wakes
//! the app for those and for scheduled messages (`crate::drafts`).
//!
//! Status changes are reconciled after every update, one call per server at
//! a time and only while the server answers (initial load in), so a pause
//! that ended while Rustket was off, or a status that expired meanwhile, is
//! settled on the next start. The pause and the remembered statuses live in
//! the app config.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::UserStatus;
use rustket_core::ServerId;
use rustket_core::quiet::{self, ExpiryAction, ExpiryStep, HoldStep, Quiet, StatusExpiry};

use super::{App, Modal};
use crate::message::Message;
use crate::quiet::{Msg, Request, StatusCall};

/// Longest wait of the timer (the monotonic clock stops while the machine
/// sleeps; the wall clock is checked again after this).
const MAX_WAIT: Duration = Duration::from_secs(60);
/// Shortest wait of the timer (never a busy loop).
const MIN_WAIT: Duration = Duration::from_secs(1);
/// Wait after a failed status call before the next try.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Failed tries of a restore or an expiry before it is given up.
const MAX_FAILURES: u32 = 3;

/// Runtime state of `app::quiet`.
#[derive(Debug, Default)]
pub struct QuietRuntime {
    /// Servers with a status call running.
    inflight: HashSet<ServerId>,
    /// Servers whose last status call failed: no new call before this.
    retry_at: HashMap<ServerId, Instant>,
    /// Consecutive failures per server.
    failures: HashMap<ServerId, u32>,
    /// The timer is armed for this time.
    timer: Option<Instant>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl App {
    /// Whether notifications are paused now, and until when.
    pub fn quiet_now(&self) -> Quiet {
        quiet::quiet_state(
            &self.config.pause,
            &self.config.ui.working_hours,
            &chrono::Local::now(),
        )
    }

    pub(super) fn on_quiet(&mut self, m: Msg) -> Task<Message> {
        match m {
            Msg::Request(Request::Pause(until)) => {
                if until > now_ms() {
                    quiet::pause(&mut self.config.pause, until);
                }
                self.modals.retain(|m| !matches!(m, Modal::PauseCustom(_)));
                self.save_config()
            }
            Msg::Preset(p) => {
                let until = p.until(&chrono::Local::now(), self.config.ui.morning_hour());
                self.on_quiet(Msg::Request(Request::Pause(until.timestamp_millis())))
            }
            Msg::Request(Request::Resume) => {
                quiet::resume(
                    &mut self.config.pause,
                    &self.config.ui.working_hours,
                    &chrono::Local::now(),
                );
                self.save_config()
            }
            Msg::Request(Request::PauseCustom) => {
                if !self
                    .modals
                    .iter()
                    .any(|m| matches!(m, Modal::PauseCustom(_)))
                {
                    self.modals
                        .push(Modal::PauseCustom(crate::quiet::custom_picker()));
                }
                Task::none()
            }
            // Comes with its server through `set_status_expiry`.
            Msg::Request(Request::Expiry(_) | Request::ReleaseHold) => Task::none(),
            Msg::Picker(pm) => {
                let morning = self.config.ui.morning_hour();
                let Some(Modal::PauseCustom(picker)) = self.modals.last_mut() else {
                    return Task::none();
                };
                match picker.update(pm, chrono::Local::now(), morning) {
                    Some(crate::later::when::Picked::At(at)) => {
                        self.on_quiet(Msg::Request(Request::Pause(at)))
                    }
                    _ if picker.is_custom() => {
                        iced::widget::operation::focus(crate::later::when::DATE_INPUT_ID)
                    }
                    _ => Task::none(),
                }
            }
            Msg::SetBusy(on) => {
                self.config.ui.pause_sets_busy = on;
                self.save_config()
            }
            Msg::StatusDone(sid, epoch, call, r) => self.status_done(sid, epoch, call, r),
            Msg::Due(at) => {
                if self.quiet.timer == Some(at) {
                    self.quiet.timer = None;
                }
                Task::none()
            }
        }
    }

    /// The status just set from the status menu of server `sid` clears
    /// itself at `e.at` (`None`: it does not).
    pub(super) fn set_status_expiry(
        &mut self,
        sid: &ServerId,
        e: Option<StatusExpiry>,
    ) -> Task<Message> {
        let Some(entry) = self.config.server_mut(sid) else {
            return Task::none();
        };
        let e = e.map(|e| quiet::expiry_for_hold(e, entry.status_hold.as_ref()));
        if entry.status_expiry == e {
            return Task::none();
        }
        entry.status_expiry = e;
        self.save_config()
    }

    /// The user chose a status on server `sid`: a pause's busy hold is
    /// released (nothing is restored over the choice).
    pub(super) fn release_status_hold(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(h) = self
            .config
            .server_mut(sid)
            .and_then(|e| e.status_hold.as_mut())
            .filter(|h| !h.released)
        else {
            return Task::none();
        };
        h.released = true;
        self.save_config()
    }

    /// After every update: settles statuses (busy while paused, restores,
    /// expiries), shows the pause in the tray and arms the timer for the
    /// next pause change, status expiry or scheduled message.
    pub(super) fn quiet_tick(&mut self) -> Task<Message> {
        let local = chrono::Local::now();
        let now = local.timestamp_millis();
        let t = Instant::now();
        self.quiet.retry_at.retain(|_, at| *at > t);
        let mut save = quiet::tidy(
            &mut self.config.pause,
            &self.config.ui.working_hours,
            &local,
        );
        let q = quiet::quiet_state(&self.config.pause, &self.config.ui.working_hours, &local);
        let busy_wanted = q.paused && self.config.ui.pause_sets_busy;
        let line = q
            .paused
            .then(|| crate::quiet::paused_line(&q, local, self.config.ui.clock_24h));
        let labels = rustket_core::quiet::PausePreset::ALL
            .iter()
            .map(|p| crate::quiet::preset_label(*p))
            .collect();
        self.desktop.tray.set_pause(line, labels);
        // Only future times wake the timer: something overdue that cannot
        // be done now (offline, a call running or failed) is looked at
        // again on the next update, or after `MAX_WAIT`.
        let mut next: Option<i64> =
            quiet::next_change(&self.config.pause, &self.config.ui.working_hours, &local);
        let mut sooner = |at: i64| {
            if at > now {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        };
        let mut tasks = Vec::new();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        for sid in &sids {
            if let Some(at) = self
                .servers
                .get(sid)
                .and_then(|s| s.workspace.scheduled().next_at())
            {
                sooner(at);
            }
            if let Some(at) = self
                .config
                .server(sid)
                .and_then(|e| e.status_expiry.as_ref())
                .map(|e| e.at)
            {
                sooner(at);
            }
        }
        for sid in sids {
            // Signed out (not while the token is being looked up): the
            // remembered statuses go with the account.
            let signed_out = self
                .servers
                .get(&sid)
                .is_some_and(|s| !s.is_logged_in() && !s.restoring);
            if signed_out
                && let Some(e) = self.config.server_mut(&sid)
                && (e.status_hold.is_some() || e.status_expiry.is_some())
            {
                e.status_hold = None;
                e.status_expiry = None;
                save = true;
            }
            let (task, changed) = self.settle_status(&sid, busy_wanted, now);
            save |= changed;
            tasks.push(task);
        }
        tasks.push(self.arm_quiet_timer(next, now));
        if save {
            tasks.push(self.save_config());
        }
        Task::batch(tasks)
    }

    /// One status step for server `sid` (at most one call at a time).
    /// Returns the call and whether the config changed.
    fn settle_status(
        &mut self,
        sid: &ServerId,
        busy_wanted: bool,
        now: i64,
    ) -> (Task<Message>, bool) {
        if self.quiet.inflight.contains(sid)
            || self
                .quiet
                .retry_at
                .get(sid)
                .is_some_and(|t| *t > Instant::now())
        {
            return (Task::none(), false);
        }
        let auto_away = self.desktop.idle.is_away(sid);
        let Some(s) = self.servers.get(sid) else {
            return (Task::none(), false);
        };
        if !crate::cache::online(s) {
            return (Task::none(), false);
        }
        let (Some(uid), Some(me)) = (s.workspace.user_id(), s.workspace.me()) else {
            return (Task::none(), false);
        };
        let uid = uid.to_owned();
        let status = s.workspace.my_chosen_status();
        let text = me.status_text.clone().unwrap_or_default();
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return (Task::none(), false);
        };
        let epoch = s.generation;
        let Some(entry) = self.config.server_mut(sid) else {
            return (Task::none(), false);
        };
        let mut changed = false;
        // An expiring status first (it may set the status the hold sees).
        if let Some(e) = entry.status_expiry.clone() {
            match quiet::expiry_step(&e, &uid, now, status, &text) {
                ExpiryStep::Wait => {}
                ExpiryStep::Forget => {
                    entry.status_expiry = None;
                    changed = true;
                }
                ExpiryStep::Clear(restore) => {
                    // A pause holding busy keeps it: only the text clears,
                    // and the status waits for the end of the pause.
                    let call = match quiet::expiry_action(restore, busy_wanted) {
                        ExpiryAction::Clear(st) => StatusCall::Expire(st),
                        ExpiryAction::ClearAndHold(st) => StatusCall::ExpireHold(st),
                    };
                    return (self.status_call(sid, epoch, rest, call), changed);
                }
            }
        }
        let step = quiet::hold_step(
            busy_wanted,
            entry.status_hold.as_ref(),
            &uid,
            status,
            &text,
            auto_away,
        );
        let call = match step {
            HoldStep::Nothing => None,
            HoldStep::Keep(h) => {
                entry.status_hold = Some(h);
                changed = true;
                None
            }
            HoldStep::Release => {
                if let Some(h) = entry.status_hold.as_mut() {
                    h.released = true;
                }
                changed = true;
                None
            }
            HoldStep::Forget => {
                entry.status_hold = None;
                changed = true;
                None
            }
            HoldStep::SetBusy(h) => Some(StatusCall::Busy(h)),
            HoldStep::Restore(st) => Some(StatusCall::Restore(st)),
        };
        let task = match call {
            Some(c) => self.status_call(sid, epoch, rest, c),
            None => Task::none(),
        };
        (task, changed)
    }

    fn status_call(
        &mut self,
        sid: &ServerId,
        epoch: u64,
        rest: rocketchat::RestClient,
        call: StatusCall,
    ) -> Task<Message> {
        self.quiet.inflight.insert(sid.clone());
        let sid = sid.clone();
        let c2 = call.clone();
        Task::perform(
            async move {
                match c2 {
                    // The text stays as it is.
                    StatusCall::Busy(_) => rest.users_set_status(UserStatus::Busy, None).await,
                    StatusCall::Restore(st) => rest.users_set_status(st, None).await,
                    StatusCall::Expire(Some(st)) => rest.users_set_status(st, Some("")).await,
                    StatusCall::Expire(None) | StatusCall::ExpireHold(_) => {
                        rest.users_set_status_message("").await
                    }
                }
                .map_err(crate::error::Failure::from)
            },
            move |r| Message::Quiet(Msg::StatusDone(sid.clone(), epoch, call.clone(), r)),
        )
    }

    fn status_done(
        &mut self,
        sid: ServerId,
        epoch: u64,
        call: StatusCall,
        r: crate::error::Res<()>,
    ) -> Task<Message> {
        self.quiet.inflight.remove(&sid);
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let ws = &mut s.workspace;
        let uid = ws.user_id().map(str::to_owned);
        let failed = match r {
            Ok(()) => {
                self.quiet.failures.remove(&sid);
                self.quiet.retry_at.remove(&sid);
                match &call {
                    StatusCall::Busy(_) => ws.apply_my_status(UserStatus::Busy, None),
                    StatusCall::Restore(st) => ws.apply_my_status(*st, None),
                    StatusCall::Expire(Some(st)) => ws.apply_my_status(*st, Some("")),
                    StatusCall::Expire(None) | StatusCall::ExpireHold(_) => {
                        let username = ws.username().map(str::to_owned);
                        let shown = ws.my_status();
                        if let Some(uid) = &uid {
                            ws.apply_user_status(uid, username.as_deref(), shown, Some(""));
                        }
                    }
                }
                false
            }
            Err(e) => {
                tracing::warn!(server = %sid, error = %e, "status change failed");
                let n = self.quiet.failures.entry(sid.clone()).or_default();
                *n += 1;
                self.quiet
                    .retry_at
                    .insert(sid.clone(), Instant::now() + RETRY_AFTER);
                true
            }
        };
        let given_up =
            failed && self.quiet.failures.get(&sid).copied().unwrap_or(0) >= MAX_FAILURES;
        let Some(entry) = self.config.server_mut(&sid) else {
            return Task::none();
        };
        match call {
            StatusCall::Busy(h) if !failed => entry.status_hold = Some(h),
            StatusCall::Busy(h) if given_up => {
                // Never busy: nothing to restore later.
                entry.status_hold = Some(rustket_core::quiet::StatusHold {
                    released: true,
                    ..h
                });
            }
            StatusCall::Restore(_) if !failed || given_up => entry.status_hold = None,
            StatusCall::Expire(_) if !failed || given_up => entry.status_expiry = None,
            StatusCall::ExpireHold(st) if !failed => {
                entry.status_expiry = None;
                if let Some(uid) = uid {
                    entry.status_hold = Some(rustket_core::quiet::StatusHold {
                        user_id: uid,
                        status: st,
                        text: String::new(),
                        released: false,
                    });
                }
            }
            StatusCall::ExpireHold(_) if given_up => entry.status_expiry = None,
            _ => return Task::none(),
        }
        if given_up {
            self.quiet.failures.remove(&sid);
        }
        self.save_config()
    }

    fn arm_quiet_timer(&mut self, next: Option<i64>, now: i64) -> Task<Message> {
        let idle = self.config.pause == rustket_core::quiet::PauseState::default()
            && !self.config.ui.working_hours.enabled;
        let Some(wait) = timer_wait(next, now, idle, !self.quiet.retry_at.is_empty()) else {
            return Task::none();
        };
        let t = Instant::now();
        let at = t + wait;
        if self.quiet.timer.is_some_and(|x| x <= at && x > t) {
            return Task::none();
        }
        self.quiet.timer = Some(at);
        Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
            Message::Quiet(Msg::Due(at))
        })
    }
}

/// How long the timer sleeps for the next time `next` (UTC ms, a future
/// one): at least [`MIN_WAIT`] (a time that is already due never makes it
/// spin), at most [`MAX_WAIT`] (a suspend is noticed), sooner for a retry.
/// `None`: nothing to wait for (`idle`: no pause and no working hours).
fn timer_wait(next: Option<i64>, now: i64, idle: bool, retrying: bool) -> Option<Duration> {
    if next.is_none() && idle && !retrying {
        return None;
    }
    let wait = next.map_or(MAX_WAIT, |at| {
        Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0))
            .clamp(MIN_WAIT, MAX_WAIT)
    });
    Some(if retrying {
        wait.min(RETRY_AFTER)
    } else {
        wait
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timer_never_spins() {
        assert_eq!(timer_wait(None, 0, true, false), None);
        assert_eq!(timer_wait(None, 0, false, false), Some(MAX_WAIT));
        // Due already (a caller bug or a race): one second, not zero.
        assert_eq!(timer_wait(Some(-5), 0, true, false), Some(MIN_WAIT));
        assert_eq!(timer_wait(Some(0), 0, true, false), Some(MIN_WAIT));
        assert_eq!(
            timer_wait(Some(5_000), 0, true, false),
            Some(Duration::from_secs(5))
        );
        assert_eq!(timer_wait(Some(i64::MAX), 0, true, false), Some(MAX_WAIT));
        assert_eq!(
            timer_wait(None, 0, true, true),
            Some(RETRY_AFTER.min(MAX_WAIT))
        );
    }
}
