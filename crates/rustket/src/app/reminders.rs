//! Firing message reminders (`rustket_core::reminders`) while Rustket runs:
//! a timer wakes at the next due time (at least every minute, so a suspend
//! of the machine is noticed), each due reminder gets a desktop
//! notification (`crate::later::notify`), and reminders that were due before
//! this process first saw the account's reminders get one summary
//! notification per server. The notification text comes from the local
//! copy of the message or a fetch, never from the synced data (which holds
//! ids and times only). Clicking a reminder opens the message and marks the
//! reminder done; Snooze moves it an hour on; Done clears it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::UserStatus;
use rustket_core::ServerId;
use rustket_core::reminders::{self, ReminderEdit};
use rustket_core::room_list;
use rustket_core::workspace::{ConnectionState, LaterTab};

use super::App;
use crate::desktop::notifications::Signal;
use crate::error::Failure;
use crate::later::notify::{Content, ReminderAction, ReminderNotifier};
use crate::message::Message;

/// Longest wait of the timer (the monotonic clock stops while the machine
/// sleeps; the wall clock is checked again after this).
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// Snooze of the notification's button.
pub const SNOOZE: chrono::Duration = chrono::Duration::hours(1);
/// Characters of the message shown in a notification.
const BODY_CHARS: usize = 200;
/// Messages fetched at once per server for notification bodies
/// (`chat.getMessage` allows 10 a minute per IP, shared with the thread
/// loader and links): reminders due together beyond that show where they
/// are without the text.
const FETCH_AT_ONCE: usize = 2;

/// Due reminders get a notification (it stays until acted on, whatever the
/// desktop notification switch, the status and the focus are).
pub fn reminder_toast(ui: &rustket_core::config::UiPrefs) -> bool {
    ui.reminder_notifications
}

/// Why a reminder notification is silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderSoundOff {
    /// Settings → Notifications: "Play a sound" off (every sound).
    Sound,
    /// Settings → Notifications: reminder sound off.
    Setting,
    Paused,
    /// Busy acts like a pause here (the notification itself still shows).
    Busy,
    /// The account's new-message sound is "None".
    Account,
    /// The account's notification or master volume is zero.
    Volume,
}

/// The sound of a reminder notification: the account's message sound at
/// its volume, when "Play a sound" and the reminder sound are on,
/// notifications are not `paused` and the account's status is not `busy`.
pub fn reminder_sound(
    ui: &rustket_core::config::UiPrefs,
    paused: bool,
    busy: bool,
    account: &crate::desktop::AccountSound,
) -> Result<crate::desktop::sound::RcSound, ReminderSoundOff> {
    use crate::desktop::notifications::{MessageSoundOff, message_sound};
    if !ui.sound {
        return Err(ReminderSoundOff::Sound);
    }
    if !ui.reminder_sound {
        return Err(ReminderSoundOff::Setting);
    }
    if paused {
        return Err(ReminderSoundOff::Paused);
    }
    if busy {
        return Err(ReminderSoundOff::Busy);
    }
    message_sound(ui.sound, None, account).map_err(|o| match o {
        MessageSoundOff::Volume => ReminderSoundOff::Volume,
        MessageSoundOff::Setting => ReminderSoundOff::Sound,
        MessageSoundOff::Room | MessageSoundOff::Account => ReminderSoundOff::Account,
    })
}

/// Reminder firing state of the app.
#[derive(Debug, Default)]
pub struct ReminderTimer {
    /// Per server: when this process first saw the account's reminders
    /// (earlier due times were missed).
    since_ms: HashMap<ServerId, i64>,
    /// Reminders handled this run: `(server, message, due time)`.
    shown: HashSet<(ServerId, String, i64)>,
    /// Servers whose missed reminders were summed up.
    missed_shown: HashSet<ServerId>,
    /// Reminders whose message is being fetched for the notification.
    fetching: HashSet<(ServerId, String, i64)>,
    /// The timer is armed for this time.
    timer: Option<Instant>,
    pub notify: ReminderNotifier,
}

impl ReminderTimer {
    /// Server `sid` signed out or went: its state goes.
    fn forget(&mut self, sid: &ServerId) {
        self.since_ms.remove(sid);
        self.missed_shown.remove(sid);
        self.shown.retain(|(s, _, _)| s != sid);
        self.fetching.retain(|(s, _, _)| s != sid);
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The notification body: the message's plain text, cut. An encrypted
/// message shows its text only when this process decrypted it
/// (`crate::e2e::shown`); the server's text of one is never shown.
fn body_of(m: &rocketchat::models::Message) -> Option<String> {
    if crate::e2e::shown::undecrypted(m) {
        return Some(rust_i18n::t!("desktop.encrypted_body").into_owned());
    }
    let text = crate::room::render::plain_text(m);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    let mut out: String = text.chars().take(BODY_CHARS).collect();
    if text.chars().count() > BODY_CHARS {
        out.push('…');
    }
    Some(out)
}

impl App {
    /// Where a reminder's message is, for the notification title.
    fn reminder_place(&self, sid: &ServerId, rid: &str, tmid: Option<&str>) -> String {
        let Some(s) = self.servers.get(sid) else {
            return String::new();
        };
        let real = s.workspace.use_real_names(self.config.ui.use_real_names);
        let room = s
            .workspace
            .room(rid)
            .map(|e| room_list::display_name(&s.workspace, e, real))
            .unwrap_or_default();
        match tmid {
            Some(_) => rust_i18n::t!("later.thread_in", room = room.as_str()).into_owned(),
            None => room,
        }
    }

    /// Shows the notification of a due reminder (its message from the local
    /// copy, if any).
    fn show_reminder(&mut self, sid: &ServerId, mid: &str, at: i64) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        let Some(r) = s.workspace.reminders().get(mid).cloned() else {
            return Task::none();
        };
        let body = s
            .workspace
            .later_message(mid)
            .filter(|_| self.config.ui.notification_preview)
            .and_then(body_of);
        // Reminders still show while notifications are paused or the
        // account is Busy (the user asked for them), silently.
        let busy = s.workspace.my_status() == UserStatus::Busy;
        let account = crate::desktop::AccountSound::of(&s.workspace);
        let sound = reminder_sound(&self.config.ui, self.quiet_now().paused, busy, &account);
        let place = self.reminder_place(sid, &r.rid, r.tmid.as_deref());
        let backend = self.desktop.notifications.backend();
        let content = Content {
            place,
            body,
            sound: sound.ok(),
        };
        self.reminder_timer
            .notify
            .show_reminder(backend, sid, mid, at, content)
            .map(Message::ReminderNotify)
    }

    /// Per update: fires due reminders, sums up missed ones, closes
    /// notifications of reminders that changed elsewhere and arms the timer.
    pub(super) fn reminders_tick(&mut self) -> Task<Message> {
        let now = now_ms();
        let notify_on = reminder_toast(&self.config.ui);
        let mut tasks = Vec::new();
        let mut next: Option<i64> = None;
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        let gone: Vec<ServerId> = self
            .reminder_timer
            .since_ms
            .keys()
            .filter(|sid| self.servers.get(*sid).is_none_or(|s| !s.is_logged_in()))
            .cloned()
            .collect();
        for sid in gone {
            self.reminder_timer.forget(&sid);
            tasks.push(
                self.reminder_timer
                    .notify
                    .close_server(&sid)
                    .map(Message::ReminderNotify),
            );
        }
        for sid in sids {
            let Some(s) = self.servers.get(&sid) else {
                continue;
            };
            if !s.is_logged_in() {
                continue;
            }
            let since = *self
                .reminder_timer
                .since_ms
                .entry(sid.clone())
                .or_insert(now);
            // Reminders of rooms the user left neither fire nor count as
            // missed (the Later view cannot list them either).
            let view = s.workspace.active_reminders();
            let mine: HashSet<(String, i64)> = self
                .reminder_timer
                .shown
                .iter()
                .filter(|(s, _, _)| *s == sid)
                .map(|(_, m, a)| (m.clone(), *a))
                .collect();
            let due = reminders::due(&view, now, since, &mine);
            let fire: Vec<(String, i64)> =
                due.fire.iter().map(|r| (r.mid.clone(), r.at_ms)).collect();
            let missed: Vec<(String, i64)> = due
                .missed
                .iter()
                .map(|r| (r.mid.clone(), r.at_ms))
                .collect();
            for (mid, at) in &missed {
                self.reminder_timer
                    .shown
                    .insert((sid.clone(), mid.clone(), *at));
            }
            if !missed.is_empty()
                && notify_on
                && self.reminder_timer.missed_shown.insert(sid.clone())
            {
                let (busy, account) = self.servers.get(&sid).map_or_else(Default::default, |s| {
                    (
                        s.workspace.my_status() == UserStatus::Busy,
                        crate::desktop::AccountSound::of(&s.workspace),
                    )
                });
                let sound =
                    reminder_sound(&self.config.ui, self.quiet_now().paused, busy, &account).ok();
                let backend = self.desktop.notifications.backend();
                tasks.push(
                    self.reminder_timer
                        .notify
                        .show_missed(backend, &sid, missed.len(), sound)
                        .map(Message::ReminderNotify),
                );
            }
            for (mid, at) in fire {
                self.reminder_timer
                    .shown
                    .insert((sid.clone(), mid.clone(), at));
                if !notify_on {
                    continue;
                }
                let have_copy = self
                    .servers
                    .get(&sid)
                    .is_some_and(|s| s.workspace.later_message(&mid).is_some());
                let in_flight = self
                    .reminder_timer
                    .fetching
                    .iter()
                    .filter(|(s, _, _)| *s == sid)
                    .count();
                let fetch = self.servers.get(&sid).and_then(|s| {
                    let live = *s.workspace.connection() == ConnectionState::Connected;
                    (!have_copy
                        && live
                        && self.config.ui.notification_preview
                        && in_flight < FETCH_AT_ONCE)
                        .then(|| s.session.as_ref().map(|h| (h.rest().clone(), s.generation)))
                        .flatten()
                });
                match fetch {
                    Some((rest, epoch)) => {
                        let key = (sid.clone(), mid.clone(), at);
                        self.reminder_timer.fetching.insert(key);
                        let (fsid, fmid) = (sid.clone(), mid.clone());
                        tasks.push(Task::perform(
                            async move { rest.get_message(&fmid).await.map_err(Failure::from) },
                            move |r| {
                                Message::ReminderFetched(
                                    fsid.clone(),
                                    epoch,
                                    mid.clone(),
                                    at,
                                    r.map(Box::new),
                                )
                            },
                        ));
                    }
                    None => tasks.push(self.show_reminder(&sid, &mid, at)),
                }
            }
            next = match (next, view.next_due_after(now)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        // Notifications of reminders cleared or moved (here or elsewhere).
        for (sid, mid, at) in self.reminder_timer.notify.reminders() {
            let current = self
                .servers
                .get(&sid)
                .and_then(|s| s.workspace.reminders().get(&mid))
                .map(|r| r.at_ms);
            if current != Some(at) {
                tasks.push(
                    self.reminder_timer
                        .notify
                        .close_reminder(&sid, &mid, at)
                        .map(Message::ReminderNotify),
                );
            }
        }
        tasks.push(self.arm_reminder_timer(next, now));
        Task::batch(tasks)
    }

    fn arm_reminder_timer(&mut self, next: Option<i64>, now: i64) -> Task<Message> {
        let wait = next.map_or(MAX_WAIT, |at| {
            Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0)).min(MAX_WAIT)
        });
        let logged_in = self.servers.values().any(|s| s.is_logged_in());
        if !logged_in {
            return Task::none();
        }
        let at = Instant::now() + wait;
        let t = Instant::now();
        if self.reminder_timer.timer.is_some_and(|x| x <= at && x > t) {
            return Task::none();
        }
        self.reminder_timer.timer = Some(at);
        // Created lazily: no runtime is needed until the task runs.
        Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
            Message::RemindersDue(at)
        })
    }

    /// The timer armed for `at` fired (the next `after_update` fires what
    /// is due).
    pub(super) fn on_reminders_due(&mut self, at: Instant) -> Task<Message> {
        if self.reminder_timer.timer == Some(at) {
            self.reminder_timer.timer = None;
        }
        Task::none()
    }

    /// A due reminder's message was fetched for its notification.
    pub(super) fn on_reminder_fetched(
        &mut self,
        sid: ServerId,
        epoch: u64,
        mid: String,
        at: i64,
        r: Result<Box<rocketchat::models::Message>, Failure>,
    ) -> Task<Message> {
        if !self
            .reminder_timer
            .fetching
            .remove(&(sid.clone(), mid.clone(), at))
        {
            return Task::none();
        }
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        match r {
            Ok(m) => s.workspace.apply_later_messages(vec![*m]),
            Err(e) => tracing::debug!(error = %e, "reminder message not fetched"),
        }
        self.show_reminder(&sid, &mid, at)
    }

    /// A message of the reminder notifications.
    pub(super) fn on_reminder_notify(&mut self, m: crate::later::notify::Msg) -> Task<Message> {
        let (task, sound) = self.reminder_timer.notify.update(m);
        // The notification did not play it (it cannot, or it failed).
        if let Some(sound) = sound {
            self.desktop.sound.play_alone(&sound);
        }
        task.map(Message::ReminderNotify)
    }

    /// A notification signal: the reminders' own, else the calls'.
    pub(super) fn on_foreign_signal(&mut self, id: u32, signal: Signal) -> Task<Message> {
        if !self.reminder_timer.notify.owns(id) {
            return self.on_calls(crate::videoconf::Message::Notify(
                crate::videoconf::notify::Msg::Signal(id, signal),
            ));
        }
        let Some(action) = self.reminder_timer.notify.signal(id, signal) else {
            return Task::none();
        };
        self.on_reminder_action(action)
    }

    /// What the user asked for in a reminder notification. Opening leaves
    /// the current screen: an admin screen with unsaved settings asks first
    /// and replays the action after "Discard".
    pub(super) fn on_reminder_action(&mut self, action: ReminderAction) -> Task<Message> {
        let show = self.desktop.show_window().map(Message::Desktop);
        let leaves = matches!(
            action,
            ReminderAction::Open { .. } | ReminderAction::OpenLater(_)
        );
        let commit = if leaves {
            if !self.may_leave(Message::ReminderAction(action.clone())) {
                return show;
            }
            self.commit_flag_drafts()
        } else {
            Task::none()
        };
        let show = Task::batch([show, commit]);
        match action {
            ReminderAction::Open { sid, mid } => {
                let target = self.servers.get(&sid).and_then(|s| {
                    let r = s.workspace.reminders().get(&mid)?;
                    Some((r.rid.clone(), r.tmid.clone()))
                });
                // Revealed first: clearing drops the local copy of an
                // unstarred message, which the reveal shows without a fetch.
                let reveal = match target {
                    Some((rid, tmid)) => self.reveal_message(&sid, &rid, tmid, &mid),
                    None => Task::none(),
                };
                let clear =
                    self.set_reminder_with(&sid, ReminderEdit::Clear { mid: mid.clone() }, false);
                Task::batch([show, reveal, clear])
            }
            ReminderAction::Snooze { sid, mid } => {
                let Some(r) = self
                    .servers
                    .get(&sid)
                    .and_then(|s| s.workspace.reminders().get(&mid).cloned())
                else {
                    return Task::none();
                };
                self.set_reminder_with(
                    &sid,
                    ReminderEdit::Set {
                        rid: r.rid,
                        tmid: r.tmid,
                        mid,
                        at_ms: (chrono::Utc::now() + SNOOZE).timestamp_millis(),
                    },
                    false,
                )
            }
            ReminderAction::Done { sid, mid } => {
                self.set_reminder_with(&sid, ReminderEdit::Clear { mid }, false)
            }
            ReminderAction::OpenLater(sid) => {
                let a = self.activate_for(sid.clone(), false);
                let open = self.open_later(&sid, Some(LaterTab::Reminders), false);
                Task::batch([show, a, open])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q3: a Busy status silences the alarm like a pause; the toast stays.
    #[test]
    fn busy_and_pause_silence_the_alarm() {
        use crate::desktop::AccountSound;
        use crate::desktop::sound::RcSound;
        let ui = rustket_core::config::UiPrefs::default();
        let acc = AccountSound::default();
        assert!(ui.reminder_sound);
        assert_eq!(
            reminder_sound(&ui, false, false, &acc),
            Ok(RcSound {
                name: "chime".into(),
                volume: 100
            })
        );
        assert_eq!(
            reminder_sound(&ui, true, false, &acc),
            Err(ReminderSoundOff::Paused)
        );
        assert_eq!(
            reminder_sound(&ui, false, true, &acc),
            Err(ReminderSoundOff::Busy)
        );
        assert!(reminder_toast(&ui));
        let off = rustket_core::config::UiPrefs {
            reminder_sound: false,
            ..ui.clone()
        };
        assert_eq!(
            reminder_sound(&off, false, false, &acc),
            Err(ReminderSoundOff::Setting)
        );
        // "Play a sound" off silences reminders too.
        let silent = rustket_core::config::UiPrefs {
            sound: false,
            reminder_sound: true,
            ..ui.clone()
        };
        assert_eq!(
            reminder_sound(&silent, false, false, &acc),
            Err(ReminderSoundOff::Sound)
        );
    }

    /// Reminders play the account's message sound at its volume.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn reminder_sound_follows_the_account() {
        use crate::desktop::AccountSound;
        let ui = rustket_core::config::UiPrefs::default();
        let acc = AccountSound {
            new_message: Some("ding".into()),
            notifications_volume: Some(80.0),
            master_volume: Some(50.0),
            ..AccountSound::default()
        };
        let s = reminder_sound(&ui, false, false, &acc).unwrap();
        assert_eq!((s.name.as_str(), s.volume), ("ding", 40));
        let none = AccountSound {
            new_message: Some("none".into()),
            ..AccountSound::default()
        };
        assert_eq!(
            reminder_sound(&ui, false, false, &none),
            Err(ReminderSoundOff::Account)
        );
        let silent = AccountSound {
            master_volume: Some(0.0),
            ..AccountSound::default()
        };
        assert_eq!(
            reminder_sound(&ui, false, false, &silent),
            Err(ReminderSoundOff::Volume)
        );
    }

    #[test]
    fn bodies_are_one_line_and_cut() {
        let m = rocketchat::models::Message {
            msg: format!("line one\n\nline   two {}", "x".repeat(300)),
            ..Default::default()
        };
        let b = body_of(&m).unwrap_or_default();
        assert!(b.starts_with("line one line two"));
        assert_eq!(b.chars().count(), BODY_CHARS + 1);
        let enc = rocketchat::models::Message {
            t: Some("e2e".into()),
            ..Default::default()
        };
        let hidden = Some(rust_i18n::t!("desktop.encrypted_body").into_owned());
        assert_eq!(body_of(&enc), hidden);
        // Not decrypted here: the server's text (an old format's
        // ciphertext, or plaintext next to an envelope) is not shown.
        let legacy = rocketchat::models::Message {
            t: Some("e2e".into()),
            msg: "Zm9vYmFyYmF6cXV4".into(),
            ..Default::default()
        };
        assert_eq!(body_of(&legacy), hidden);
        let mut planted: rocketchat::models::Message = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "msg": "pay to this account",
            "content": {"algorithm": "rc.v2.aes-sha2", "ciphertext": "x"}
        }))
        .unwrap_or_default();
        planted.t = None;
        assert_eq!(body_of(&planted), hidden);
    }
}
