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
use rustket_core::quiet::{
    self, ExpiryAction, ExpiryStep, HoldStep, Quiet, StatusExpiry, StatusHold,
};

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
            Msg::Request(Request::Expiry(_) | Request::KeptExpiry(_) | Request::ReleaseHold) => {
                Task::none()
            }
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
    /// `kept`: the current expiry kept with a new text, stored as it is.
    pub(super) fn set_status_expiry(
        &mut self,
        sid: &ServerId,
        e: Option<StatusExpiry>,
        kept: bool,
    ) -> Task<Message> {
        let Some(entry) = self.config.server_mut(sid) else {
            return Task::none();
        };
        let e = e.map(|e| stored_expiry(e, kept, entry.status_hold.as_ref()));
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
            .filter(|h| !h.released || h.expires_at.is_some())
        else {
            return Task::none();
        };
        // The expiry followed so far was the old status's (the new one's
        // is followed from the next step on).
        h.released = true;
        h.expires_at = None;
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
            if let Some(s) = self.servers.get(sid) {
                for at in [s.workspace.scheduled().next_at(now), s.cache.retry_at()]
                    .into_iter()
                    .flatten()
                {
                    sooner(at);
                }
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
        // The remembered statuses outlive a sign-out (a rejected token, a
        // login on every start): they carry their account, and the steps
        // ignore those of another one.
        for sid in sids {
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
        let idle_away = self.desktop.idle.is_away(sid);
        let Some(s) = self.servers.get(sid) else {
            return (Task::none(), false);
        };
        if !crate::cache::online(s) {
            return (Task::none(), false);
        }
        let Some(inputs) = status_inputs(&s.workspace, idle_away, now) else {
            return (Task::none(), false);
        };
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return (Task::none(), false);
        };
        let epoch = s.generation;
        let Some(entry) = self.config.server_mut(sid) else {
            return (Task::none(), false);
        };
        let (call, changed) = status_step(entry, &inputs, busy_wanted, now);
        let task = match call {
            Some((c, keep)) => self.status_call(sid, epoch, rest, c, keep),
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
        keep: Option<rocketchat::RcDate>,
    ) -> Task<Message> {
        self.quiet.inflight.insert(sid.clone());
        let sid = sid.clone();
        let c2 = call.clone();
        Task::perform(
            async move {
                let (status, message, expires) = status_call_args(&c2, keep);
                match status {
                    Some(st) => rest.users_set_status(st, message, expires).await,
                    None => {
                        rest.users_set_status_message(message.unwrap_or_default(), expires)
                            .await
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
                    StatusCall::ExpireBusy(_) => ws.apply_my_status(UserStatus::Busy, Some("")),
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
                        expires_at: None,
                    });
                }
            }
            StatusCall::ExpireHold(_) if given_up => entry.status_expiry = None,
            StatusCall::ExpireBusy(h) if !failed => {
                entry.status_expiry = None;
                entry.status_hold = Some(h);
            }
            StatusCall::ExpireBusy(_) if given_up => entry.status_expiry = None,
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

/// The expiry stored for one the status menu set (`kept`: the current
/// one kept with a new text, whose restore is not the status before).
fn stored_expiry(e: StatusExpiry, kept: bool, hold: Option<&StatusHold>) -> StatusExpiry {
    if kept {
        e
    } else {
        quiet::expiry_for_hold(e, hold)
    }
}

/// What a status step reads of the account's workspace.
#[derive(Debug, Clone)]
struct StatusInputs {
    uid: String,
    /// The chosen status (`Workspace::my_chosen_status`).
    status: Option<UserStatus>,
    text: String,
    /// When the current status clears on the server, if ahead.
    keep: Option<rocketchat::RcDate>,
    /// The away status is the app's idle away.
    auto_away: bool,
}

fn status_inputs(
    ws: &rustket_core::workspace::Workspace,
    idle_away: bool,
    now: i64,
) -> Option<StatusInputs> {
    let (uid, me) = (ws.user_id()?, ws.me()?);
    Some(StatusInputs {
        uid: uid.to_owned(),
        status: ws.my_chosen_status(),
        text: me.status_text.clone().unwrap_or_default(),
        // A status change without `expiresAt` drops the server's expiry:
        // busy and its restore repeat it (the text stays, so does when it
        // clears).
        keep: ws.my_status_expiry(now),
        auto_away: quiet::idle_away(idle_away, me.status_default),
    })
}

/// One status step of `entry`'s account: the call (with the server expiry
/// it repeats) and whether `entry` changed.
fn status_step(
    entry: &mut rustket_core::config::ServerEntry,
    inputs: &StatusInputs,
    busy_wanted: bool,
    now: i64,
) -> (Option<(StatusCall, Option<rocketchat::RcDate>)>, bool) {
    let StatusInputs {
        uid,
        status,
        text,
        keep,
        auto_away,
    } = inputs;
    let (status, auto_away) = (*status, *auto_away);
    let mut changed = false;
    // An expiring status first (it may set the status the hold sees).
    if let Some(e) = entry.status_expiry.clone() {
        match quiet::expiry_step(&e, uid, now, status, text) {
            ExpiryStep::Wait => {}
            ExpiryStep::Forget => {
                entry.status_expiry = None;
                changed = true;
            }
            ExpiryStep::Clear(restore) => {
                // A pause that set busy over the expiring status ends
                // on the status after it.
                if let Some(h) = quiet::hold_after_expiry(&e, entry.status_hold.as_ref()) {
                    entry.status_hold = Some(h);
                    changed = true;
                }
                // A pause holding busy keeps it: only the text clears,
                // and the status waits for the end of the pause.
                let call = match quiet::expiry_action(restore, busy_wanted, status) {
                    ExpiryAction::Clear(st) => StatusCall::Expire(st),
                    ExpiryAction::ClearAndHold(st) => StatusCall::ExpireHold(st),
                    ExpiryAction::ClearToBusy(st) => StatusCall::ExpireBusy(StatusHold {
                        user_id: uid.clone(),
                        status: st,
                        text: String::new(),
                        released: false,
                        expires_at: None,
                    }),
                };
                return (Some((call, None)), changed);
            }
        }
    }
    // During a pause the hold follows when the server clears the status
    // (the pause's busy, or one the hold left alone; another client may
    // change it), keeping one that is due: busy again once it fired. An
    // online status's expiry only clears its text (as the app's own
    // expiries do), so it is not followed.
    let keep_ms = keep.as_ref().map(rocketchat::RcDate::timestamp_millis);
    if let Some(h) = entry
        .status_hold
        .as_mut()
        .filter(|h| busy_wanted && h.user_id == *uid && status.is_some())
    {
        let at = if status == Some(UserStatus::Online) && keep_ms.is_some() {
            None
        } else {
            quiet::followed_expiry(h.expires_at, keep_ms, now)
        };
        if h.expires_at != at {
            h.expires_at = at;
            changed = true;
        }
    }
    let step = quiet::hold_step(
        busy_wanted,
        entry.status_hold.as_ref(),
        uid,
        status,
        text,
        auto_away,
    );
    // The server's expiry of the status during a pause is not a choice of
    // the user: busy again.
    let again = |hold: Option<&StatusHold>| {
        hold.filter(|h| busy_wanted && h.user_id == *uid)
            .zip(status)
            .and_then(|(h, st)| quiet::hold_after_server_expiry(h, st, text, now))
    };
    let call = match step {
        HoldStep::Nothing => again(entry.status_hold.as_ref()).map(StatusCall::Busy),
        HoldStep::Keep(h) => {
            entry.status_hold = Some(StatusHold {
                expires_at: keep_ms,
                ..h
            });
            changed = true;
            None
        }
        HoldStep::Release => match again(entry.status_hold.as_ref()) {
            Some(h) => Some(StatusCall::Busy(h)),
            None => {
                if let Some(h) = entry.status_hold.as_mut() {
                    h.released = true;
                }
                changed = true;
                None
            }
        },
        HoldStep::Forget => {
            entry.status_hold = None;
            changed = true;
            None
        }
        HoldStep::SetBusy(h) => Some(StatusCall::Busy(StatusHold {
            expires_at: keep_ms,
            ..h
        })),
        HoldStep::Restore(st) => Some(StatusCall::Restore(st)),
    };
    (call.map(|c| (c, *keep)), changed)
}

/// `users.setStatus` arguments of `call` (status, `None`: text only; text,
/// `None`: kept; `expiresAt`). A call without `expiresAt` drops the
/// server's expiry, so the pause's busy repeats it (the hold's) and its
/// restore repeats `keep` (the current one); the app's own expiries (servers
/// before 8.6) send none.
fn status_call_args(
    call: &StatusCall,
    keep: Option<rocketchat::RcDate>,
) -> (
    Option<UserStatus>,
    Option<&'static str>,
    Option<rocketchat::RcDate>,
) {
    match call {
        // The text stays as it is.
        StatusCall::Busy(h) => (
            Some(UserStatus::Busy),
            None,
            h.expires_at.and_then(rocketchat::RcDate::from_millis),
        ),
        StatusCall::Restore(st) => (Some(*st), None, keep),
        StatusCall::Expire(Some(st)) => (Some(*st), Some(""), None),
        StatusCall::ExpireBusy(_) => (Some(UserStatus::Busy), Some(""), None),
        StatusCall::Expire(None) | StatusCall::ExpireHold(_) => (None, Some(""), None),
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
#[allow(clippy::unwrap_used)]
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

    #[test]
    fn pause_calls_repeat_the_server_expiry() {
        use rocketchat::RcDate;
        use rocketchat::rest::set_status_body;
        let body = |call: StatusCall, keep: Option<RcDate>| {
            let (st, msg, exp) = status_call_args(&call, keep);
            set_status_body(st, msg, exp)
        };
        let at = 1_795_000_000_000;
        let iso = RcDate::from_millis(at).unwrap().to_iso();
        let hold = |expires_at| StatusHold {
            user_id: "u".into(),
            status: UserStatus::Online,
            text: String::new(),
            released: false,
            expires_at,
        };
        // Busy repeats the hold's expiry (the one current when it started).
        assert_eq!(
            body(StatusCall::Busy(hold(Some(at))), None),
            serde_json::json!({"status": "busy", "expiresAt": iso})
        );
        assert_eq!(
            body(StatusCall::Busy(hold(None)), None),
            serde_json::json!({"status": "busy"})
        );
        // The restore repeats the current one.
        assert_eq!(
            body(
                StatusCall::Restore(UserStatus::Away),
                RcDate::from_millis(at)
            ),
            serde_json::json!({"status": "away", "expiresAt": iso})
        );
        // The app's own expiries send none.
        let keep = RcDate::from_millis(at);
        assert_eq!(
            body(StatusCall::Expire(Some(UserStatus::Online)), keep),
            serde_json::json!({"status": "online", "message": ""})
        );
        assert_eq!(
            body(StatusCall::Expire(None), keep),
            serde_json::json!({"message": ""})
        );
        assert_eq!(
            body(StatusCall::ExpireHold(UserStatus::Online), keep),
            serde_json::json!({"message": ""})
        );
        assert_eq!(
            body(StatusCall::ExpireBusy(hold(Some(at))), keep),
            serde_json::json!({"status": "busy", "message": ""})
        );
    }

    /// The own user document as `/me` returns it.
    fn me(status: &str, text: Option<&str>, expires_at: Option<i64>) -> rocketchat::models::User {
        let mut v = serde_json::json!({
            "_id": "u", "username": "alice", "status": status, "statusDefault": status,
        });
        if let Some(t) = text {
            v["statusText"] = t.into();
        }
        if let Some(at) = expires_at {
            v["statusExpiresAt"] = rocketchat::RcDate::from_millis(at).unwrap().to_iso().into();
        }
        serde_json::from_value(v).unwrap()
    }

    fn me_changed(ws: &mut rustket_core::workspace::Workspace, me: rocketchat::models::User) {
        crate::session::apply_event(ws, &rocketchat::SessionEvent::MeChanged(Box::new(me)));
    }

    /// The server's expiry of the own status fires, in the order 8.9.1 sends
    /// it: `user-status` (online, no text, no expiry), a step (the chosen
    /// status is still the stale `statusDefault`), then the `/me` re-read
    /// after the `userData` hint. Returns the step after that.
    fn server_expiry_fires(
        ws: &mut rustket_core::workspace::Workspace,
        entry: &mut rustket_core::config::ServerEntry,
        now: i64,
    ) -> Option<StatusCall> {
        crate::session::apply_event(
            ws,
            &rocketchat::SessionEvent::UserStatus {
                uid: "u".into(),
                username: "alice".into(),
                status: UserStatus::Online,
                status_text: Some(String::new()),
                status_expires_at: None,
            },
        );
        let (call, _) = step(ws, entry, now);
        assert!(call.is_none(), "{call:?}");
        me_changed(ws, me("online", None, None));
        step(ws, entry, now).0
    }

    /// A step while paused with "Busy while paused".
    fn step(
        ws: &rustket_core::workspace::Workspace,
        entry: &mut rustket_core::config::ServerEntry,
        now: i64,
    ) -> (Option<StatusCall>, bool) {
        let inputs = status_inputs(ws, false, now).unwrap();
        let (call, changed) = status_step(entry, &inputs, true, now);
        (call.map(|(c, _)| c), changed)
    }

    fn new_entry() -> rustket_core::config::ServerEntry {
        rustket_core::config::ServerEntry::new("http://localhost:3000", "Local")
    }

    /// Review: the server's expiry of the pause's busy (repeating the
    /// expiry of the status before it) cleared the hold's expiry while the
    /// chosen status was still the stale busy, so the pause released the
    /// hold and the user showed online for the rest of it.
    #[test]
    fn server_expiry_of_the_paused_busy_sets_busy_again() {
        let mut ws = rustket_core::workspace::Workspace::new();
        let now = 1_795_000_000_000;
        // Away "Lunch" until `at`, the pause set busy until `at` (the
        // server clears it a little early by this clock).
        let at = now + 30_000;
        me_changed(&mut ws, me("busy", Some("Lunch"), Some(at)));
        let mut entry = new_entry();
        entry.status_hold = Some(StatusHold {
            user_id: "u".into(),
            status: UserStatus::Away,
            text: "Lunch".into(),
            released: false,
            expires_at: Some(at),
        });
        assert_eq!(step(&ws, &mut entry, now), (None, false));
        let call = server_expiry_fires(&mut ws, &mut entry, now);
        let Some(StatusCall::Busy(h)) = call else {
            panic!("{call:?} {:?}", entry.status_hold)
        };
        assert_eq!(
            (h.status, h.text.as_str(), h.released, h.expires_at),
            (UserStatus::Online, "", false, None)
        );
        // Sent without `expiresAt`.
        assert_eq!(status_call_args(&StatusCall::Busy(h), None).2, None);
    }

    /// Review: on servers that clear statuses (8.6+), the expiry of a
    /// status the pause left alone (busy until then when it started, or
    /// picked with "Clear after" during it) dropped the user to online for
    /// the rest of the pause; the app's own expiries kept busy
    /// (`ClearAndHold`, `ClearToBusy`).
    #[test]
    fn server_expiry_during_a_pause_keeps_busy_over_a_released_hold() {
        let mut ws = rustket_core::workspace::Workspace::new();
        let now = 1_795_000_000_000;
        let at = now + 30_000;
        // Busy "Meeting" until `at` when the pause starts: nothing to set.
        me_changed(&mut ws, me("busy", Some("Meeting"), Some(at)));
        let mut entry = new_entry();
        assert_eq!(step(&ws, &mut entry, now), (None, true));
        let h = entry.status_hold.clone().unwrap();
        assert!(h.released);
        assert_eq!(h.expires_at, Some(at));
        let call = server_expiry_fires(&mut ws, &mut entry, now);
        assert!(
            matches!(&call, Some(StatusCall::Busy(h)) if h.status == UserStatus::Online && !h.released),
            "{call:?}"
        );

        // Away "Errand" until `at`, picked during the pause (a released
        // hold; the menu's choice drops the expiry followed so far).
        let mut entry = new_entry();
        entry.status_hold = Some(StatusHold {
            user_id: "u".into(),
            status: UserStatus::Online,
            text: String::new(),
            released: true,
            expires_at: None,
        });
        me_changed(&mut ws, me("away", Some("Errand"), Some(at)));
        assert_eq!(step(&ws, &mut entry, now), (None, true));
        assert_eq!(entry.status_hold.as_ref().unwrap().expires_at, Some(at));
        let call = server_expiry_fires(&mut ws, &mut entry, now);
        assert!(
            matches!(&call, Some(StatusCall::Busy(h)) if h.status == UserStatus::Online && !h.released),
            "{call:?}"
        );
        // Another client sets online without a text before the expiry: the
        // user's choice, nothing is set.
        let mut entry = new_entry();
        entry.status_hold = Some(StatusHold {
            user_id: "u".into(),
            status: UserStatus::Online,
            text: String::new(),
            released: true,
            expires_at: None,
        });
        me_changed(&mut ws, me("away", Some("Errand"), Some(at + 3_600_000)));
        let _ = step(&ws, &mut entry, now);
        me_changed(&mut ws, me("online", None, None));
        assert_eq!(step(&ws, &mut entry, now).0, None);
        assert_eq!(entry.status_hold.unwrap().expires_at, None);
    }

    /// Review: before 8.6, a new text over an app expiry kept from before
    /// a busy pause lost its restore (remapped to the hold's status, the
    /// expiring one), so the pause ended on that status instead of the one
    /// after the expiry.
    #[test]
    fn a_kept_app_expiry_restores_after_a_busy_pause() {
        let mut ws = rustket_core::workspace::Workspace::new();
        let now = 1_795_000_000_000;
        let at = now + 3_600_000;
        let hold = StatusHold {
            user_id: "u".into(),
            status: UserStatus::Away,
            text: "Lunch".into(),
            released: false,
            expires_at: None,
        };
        let kept = StatusExpiry {
            user_id: "u".into(),
            at,
            status: UserStatus::Away,
            text: "Lunch 2".into(),
            restore: Some(UserStatus::Online),
        };
        // A status picked during the pause still goes back to the hold's.
        assert_eq!(
            stored_expiry(kept.clone(), false, Some(&hold)).restore,
            None
        );
        let stored = stored_expiry(kept.clone(), true, Some(&hold));
        assert_eq!(stored, kept);
        let mut entry = new_entry();
        entry.status_hold = Some(hold);
        entry.status_expiry = Some(stored);
        me_changed(&mut ws, me("busy", Some("Lunch 2"), None));
        assert_eq!(step(&ws, &mut entry, now), (None, false));
        // At the expiry only the text clears; the pause ends on online.
        let (call, _) = step(&ws, &mut entry, at);
        assert_eq!(call, Some(StatusCall::Expire(None)));
        let h = entry.status_hold.as_ref().unwrap();
        assert_eq!((h.status, h.released), (UserStatus::Online, false));
    }

    /// Review: an online status with a text and "Clear after", picked
    /// during the pause, ended in busy when the server cleared its text;
    /// the app's own expiry of an online status only clears the text.
    #[test]
    fn server_expiry_of_an_online_status_picked_during_a_pause_keeps_online() {
        let mut ws = rustket_core::workspace::Workspace::new();
        let now = 1_795_000_000_000;
        let at = now + 30_000;
        // Picked in Rustket: the hold was released, nothing followed yet.
        let mut entry = new_entry();
        entry.status_hold = Some(StatusHold {
            user_id: "u".into(),
            status: UserStatus::Online,
            text: String::new(),
            released: true,
            expires_at: None,
        });
        me_changed(&mut ws, me("online", Some("WFH"), Some(at)));
        assert_eq!(step(&ws, &mut entry, now).0, None);
        assert_eq!(entry.status_hold.as_ref().unwrap().expires_at, None);
        assert_eq!(server_expiry_fires(&mut ws, &mut entry, now), None);

        // Set on the web while the pause's busy shows (the stale busy step
        // follows the expiry, then the hold is released with it).
        let mut ws = rustket_core::workspace::Workspace::new();
        me_changed(&mut ws, me("busy", None, None));
        let mut entry = new_entry();
        entry.status_hold = Some(StatusHold {
            user_id: "u".into(),
            status: UserStatus::Online,
            text: String::new(),
            released: false,
            expires_at: None,
        });
        crate::session::apply_event(
            &mut ws,
            &rocketchat::SessionEvent::UserStatus {
                uid: "u".into(),
                username: "alice".into(),
                status: UserStatus::Online,
                status_text: Some("WFH".into()),
                status_expires_at: rocketchat::RcDate::from_millis(at),
            },
        );
        let _ = step(&ws, &mut entry, now);
        me_changed(&mut ws, me("online", Some("WFH"), Some(at)));
        assert_eq!(step(&ws, &mut entry, now).0, None);
        let h = entry.status_hold.as_ref().unwrap();
        assert!(h.released);
        assert_eq!(h.expires_at, None);
        assert_eq!(server_expiry_fires(&mut ws, &mut entry, now), None);
    }
}
