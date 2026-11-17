//! Quiet time: pausing notifications (by hand or outside working hours),
//! "Busy while paused", and custom statuses that clear themselves.
//!
//! All of it is local (app config, never the synced service room) and
//! driven by the app's timer: [`quiet_state`] says whether notifications
//! are paused now and until when, [`next_change`] when to look again,
//! [`hold_step`] what to do with the user's status during a pause and
//! [`expiry_step`] what to do with a status that has an expiry time.
//!
//! Working hours are in local wall time: a day's range may end on the next
//! day (`end <= start`, e.g. 22:00–06:00), and boundaries are computed per
//! date with the time zone's rules, so DST changes move them correctly (a
//! boundary in a skipped hour moves an hour on).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday};
use rocketchat::models::UserStatus;
use serde::{Deserialize, Serialize};

/// Minutes in a day.
pub const DAY_MINUTES: u16 = 24 * 60;

/// One weekday's working hours, in minutes after local midnight. `end <=
/// start` ends on the next day (`start == end`: the whole day from
/// `start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayHours {
    pub on: bool,
    pub start: u16,
    pub end: u16,
}

impl Default for DayHours {
    fn default() -> Self {
        Self {
            on: false,
            start: 9 * 60,
            end: 18 * 60,
        }
    }
}

impl DayHours {
    fn clamped(self) -> Self {
        Self {
            on: self.on,
            start: self.start % DAY_MINUTES,
            end: self.end % DAY_MINUTES,
        }
    }

    /// Ends on the next day.
    pub fn overnight(&self) -> bool {
        self.end <= self.start
    }
}

/// The user's working hours (`UiPrefs::working_hours`); outside them
/// notifications are paused when `enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkingHours {
    pub enabled: bool,
    /// Monday first; missing days count as off.
    pub days: Vec<DayHours>,
}

impl Default for WorkingHours {
    fn default() -> Self {
        let mut days = vec![
            DayHours {
                on: true,
                ..DayHours::default()
            };
            5
        ];
        days.extend([DayHours::default(); 2]);
        Self {
            enabled: false,
            days,
        }
    }
}

/// `date` at `minutes` local time; a time skipped by a DST change moves an
/// hour on, an ambiguous one takes the earlier instant.
fn local_instant<Tz: TimeZone>(tz: &Tz, date: NaiveDate, minutes: u16) -> Option<DateTime<Tz>> {
    let t = date.and_hms_opt(u32::from(minutes / 60), u32::from(minutes % 60), 0)?;
    tz.from_local_datetime(&t)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(t + Duration::hours(1))).earliest())
}

impl WorkingHours {
    /// The hours of weekday `wd`.
    pub fn day(&self, wd: Weekday) -> DayHours {
        self.days
            .get(wd.num_days_from_monday() as usize)
            .copied()
            .unwrap_or_default()
            .clamped()
    }

    pub fn set_day(&mut self, wd: Weekday, hours: DayHours) {
        let i = wd.num_days_from_monday() as usize;
        while self.days.len() <= i {
            self.days.push(DayHours::default());
        }
        self.days[i] = hours.clamped();
    }

    /// Whether the local wall time `t` is inside working hours (ignoring
    /// `enabled`).
    fn covers(&self, t: NaiveDateTime) -> bool {
        let m = (t.hour() * 60 + t.minute()) as u16;
        let wd = t.weekday();
        let today = self.day(wd);
        if today.on {
            if today.overnight() {
                if m >= today.start {
                    return true;
                }
            } else if (today.start..today.end).contains(&m) {
                return true;
            }
        }
        // The part after midnight of yesterday's range (a whole day from
        // `start` when `start == end`).
        let y = self.day(wd.pred());
        y.on && y.overnight() && m < y.end
    }

    /// Whether `at` is inside working hours; always `true` when they are
    /// not enabled (nothing is paused by them).
    pub fn is_working<Tz: TimeZone>(&self, at: &DateTime<Tz>) -> bool {
        !self.enabled || self.covers(at.naive_local())
    }

    /// The next instant after `now` where [`Self::is_working`] changes
    /// (within the next eight days); `None` when it never does.
    pub fn next_boundary<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        if !self.enabled {
            return None;
        }
        let tz = now.timezone();
        let today = now.date_naive();
        let mut candidates: Vec<DateTime<Tz>> = Vec::new();
        for offset in -1..=8 {
            let date = today + Duration::days(offset);
            let d = self.day(date.weekday());
            if !d.on {
                continue;
            }
            let end_date = if d.overnight() {
                date + Duration::days(1)
            } else {
                date
            };
            candidates.extend(local_instant(&tz, date, d.start));
            candidates.extend(local_instant(&tz, end_date, d.end));
        }
        candidates.retain(|c| c > now);
        candidates.sort();
        let working = self.is_working(now);
        candidates
            .into_iter()
            .find(|c| self.is_working(c) != working)
    }
}

/// The user's pause (`AppConfig::pause`), persisted so it survives a
/// restart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PauseState {
    /// Paused by hand until this time (UTC ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<i64>,
    /// Resumed by hand outside working hours: their pause is off until this
    /// time (UTC ms), the next working-hours boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_until: Option<i64>,
}

/// Whether notifications are paused now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Quiet {
    pub paused: bool,
    /// When the pause ends (UTC ms), if known.
    pub until: Option<i64>,
    /// Paused because it is outside working hours.
    pub off_hours: bool,
}

/// The pause at `now`: a manual pause, or outside working hours unless the
/// user resumed until the next boundary.
pub fn quiet_state<Tz: TimeZone>(p: &PauseState, wh: &WorkingHours, now: &DateTime<Tz>) -> Quiet {
    let now_ms = now.timestamp_millis();
    let manual = p.paused_until.filter(|u| *u > now_ms);
    let resumed = p.resumed_until.is_some_and(|u| u > now_ms);
    let off_hours = !wh.is_working(now) && !resumed;
    let sched_end = off_hours
        .then(|| wh.next_boundary(now).map(|b| b.timestamp_millis()))
        .flatten();
    let until = match (manual, off_hours) {
        (Some(m), true) => Some(sched_end.map_or(m, |s| s.max(m))),
        (Some(m), false) => Some(m),
        (None, true) => sched_end,
        (None, false) => None,
    };
    Quiet {
        paused: manual.is_some() || off_hours,
        until,
        off_hours,
    }
}

/// Pauses by hand until `until_ms`.
pub fn pause(p: &mut PauseState, until_ms: i64) {
    p.paused_until = Some(until_ms);
}

/// Resumes by hand: the manual pause ends; outside working hours their
/// pause is off until the next boundary.
pub fn resume<Tz: TimeZone>(p: &mut PauseState, wh: &WorkingHours, now: &DateTime<Tz>) {
    p.paused_until = None;
    if !wh.is_working(now) {
        // No working day at all: resumed until the hours have a boundary.
        p.resumed_until = Some(
            wh.next_boundary(now)
                .map_or(UNTIL_BOUNDARY, |b| b.timestamp_millis()),
        );
    }
}

/// `PauseState::resumed_until` of a resume while working hours have no
/// boundary (no working day): until they get one ([`tidy`]).
pub const UNTIL_BOUNDARY: i64 = i64::MAX;

/// Forgets times that passed, and ties a resume without a boundary to the
/// working hours' next one once they have it; returns whether something
/// changed.
pub fn tidy<Tz: TimeZone>(p: &mut PauseState, wh: &WorkingHours, now: &DateTime<Tz>) -> bool {
    let now_ms = now.timestamp_millis();
    let before = p.clone();
    if p.paused_until.is_some_and(|u| u <= now_ms) {
        p.paused_until = None;
    }
    if p.resumed_until.is_some_and(|u| u <= now_ms) {
        p.resumed_until = None;
    }
    if p.resumed_until == Some(UNTIL_BOUNDARY) {
        if !wh.enabled {
            p.resumed_until = None;
        } else if let Some(b) = wh.next_boundary(now) {
            p.resumed_until = Some(b.timestamp_millis());
        }
    }
    *p != before
}

/// When the pause state may change next (UTC ms).
pub fn next_change<Tz: TimeZone>(
    p: &PauseState,
    wh: &WorkingHours,
    now: &DateTime<Tz>,
) -> Option<i64> {
    let now_ms = now.timestamp_millis();
    [
        p.paused_until,
        p.resumed_until.filter(|u| *u != UNTIL_BOUNDARY),
        wh.next_boundary(now).map(|b| b.timestamp_millis()),
    ]
    .into_iter()
    .flatten()
    .filter(|t| *t > now_ms)
    .min()
}

/// The pause presets of the status and tray menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PausePreset {
    Min30,
    Hour1,
    Hour2,
    /// Until the next day at the morning hour.
    Tomorrow,
}

impl PausePreset {
    pub const ALL: [PausePreset; 4] = [Self::Min30, Self::Hour1, Self::Hour2, Self::Tomorrow];

    /// The end of the pause from `now`.
    pub fn until<Tz: TimeZone>(self, now: &DateTime<Tz>, morning_hour: u32) -> DateTime<Tz> {
        match self {
            Self::Min30 => now.clone() + Duration::minutes(30),
            Self::Hour1 => now.clone() + Duration::hours(1),
            Self::Hour2 => now.clone() + Duration::hours(2),
            Self::Tomorrow => crate::reminders::day_at(now, 1, morning_hour),
        }
    }
}

/// "Clear after" of a custom status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClearAfter {
    #[default]
    Never,
    Min30,
    Hour1,
    Hour4,
    /// At the end of today (local midnight).
    Today,
    /// At the end of this week (Monday 00:00 local).
    ThisWeek,
}

impl ClearAfter {
    pub const ALL: [ClearAfter; 6] = [
        Self::Never,
        Self::Min30,
        Self::Hour1,
        Self::Hour4,
        Self::Today,
        Self::ThisWeek,
    ];

    /// When a status set at `now` clears (`None`: never).
    pub fn at<Tz: TimeZone>(self, now: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        let midnight = |days: i64| {
            local_instant(&now.timezone(), now.date_naive() + Duration::days(days), 0)
                .unwrap_or_else(|| now.clone() + Duration::days(days))
        };
        match self {
            Self::Never => None,
            Self::Min30 => Some(now.clone() + Duration::minutes(30)),
            Self::Hour1 => Some(now.clone() + Duration::hours(1)),
            Self::Hour4 => Some(now.clone() + Duration::hours(4)),
            Self::Today => Some(midnight(1)),
            Self::ThisWeek => {
                let from_monday = i64::from(now.weekday().num_days_from_monday());
                Some(midnight(7 - from_monday))
            }
        }
    }
}

/// The status a pause set to busy, and what was there before
/// (`ServerEntry::status_hold`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusHold {
    pub user_id: String,
    /// The status before the pause (restored after it).
    pub status: UserStatus,
    /// The status text before the pause (the pause does not change it).
    #[serde(default)]
    pub text: String,
    /// Nothing to restore: the status was busy or invisible already, or
    /// the user changed it during the pause.
    #[serde(default)]
    pub released: bool,
}

/// What to do with the user's status on one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldStep {
    Nothing,
    /// Set busy, remembering `hold` once that succeeded.
    SetBusy(StatusHold),
    /// Remember `hold` without a call (nothing to restore later).
    Keep(StatusHold),
    /// The user changed the status during the pause.
    Release,
    /// The pause ended: set `status` again, then forget the hold.
    Restore(UserStatus),
    /// Forget the hold.
    Forget,
}

/// The status step for a server where `user_id` is signed in with the
/// chosen status `current` (`None`: not known, e.g. "offline" without a
/// connection: nothing is decided) and `text`; `busy_wanted`: notifications
/// are paused and "Busy while paused" is on. `auto_away`: the current away
/// status is the app's idle away (the status before it was online).
pub fn hold_step(
    busy_wanted: bool,
    hold: Option<&StatusHold>,
    user_id: &str,
    current: Option<UserStatus>,
    text: &str,
    auto_away: bool,
) -> HoldStep {
    let Some(current) = current else {
        return HoldStep::Nothing;
    };
    let hold = hold.filter(|h| h.user_id == user_id);
    match (busy_wanted, hold) {
        (true, None) => {
            let status = if auto_away && current == UserStatus::Away {
                UserStatus::Online
            } else {
                current
            };
            let h = StatusHold {
                user_id: user_id.to_owned(),
                status,
                text: text.to_owned(),
                released: matches!(status, UserStatus::Busy | UserStatus::Offline),
            };
            if h.released {
                HoldStep::Keep(h)
            } else {
                HoldStep::SetBusy(h)
            }
        }
        (true, Some(h)) if !h.released && current != UserStatus::Busy => HoldStep::Release,
        (true, Some(_)) => HoldStep::Nothing,
        (false, Some(h)) if !h.released && current == UserStatus::Busy => {
            HoldStep::Restore(h.status)
        }
        (false, Some(_)) => HoldStep::Forget,
        (false, None) => HoldStep::Nothing,
    }
}

/// A status set with "Clear after" (`ServerEntry::status_expiry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusExpiry {
    pub user_id: String,
    /// When it clears (UTC ms).
    pub at: i64,
    /// The status that was set.
    pub status: UserStatus,
    /// The text that was set.
    #[serde(default)]
    pub text: String,
    /// The status to go back to (`None`: the status was not changed, only
    /// the text is cleared).
    #[serde(default)]
    pub restore: Option<UserStatus>,
}

/// What to do with an expiring status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpiryStep {
    Wait,
    /// Clear the text and, if `Some`, set this status.
    Clear(Option<UserStatus>),
    /// The user changed it meanwhile (or another account): forget it.
    Forget,
}

/// The step for `e` at `now_ms`, with the account's chosen status `current`
/// (`None`: not known yet, it waits) and text. The text tells whether the
/// user changed the status meanwhile; the status is restored only if it is
/// still the one that was set.
pub fn expiry_step(
    e: &StatusExpiry,
    user_id: &str,
    now_ms: i64,
    current: Option<UserStatus>,
    text: &str,
) -> ExpiryStep {
    if e.user_id != user_id || text.trim() != e.text.trim() {
        return ExpiryStep::Forget;
    }
    let Some(current) = current.filter(|_| now_ms >= e.at) else {
        return ExpiryStep::Wait;
    };
    ExpiryStep::Clear(e.restore.filter(|_| current == e.status))
}

/// How an expiring status clears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpiryAction {
    /// Clear the text and, if `Some`, set this status.
    Clear(Option<UserStatus>),
    /// A pause holds busy: clear only the text, and restore this status
    /// when the pause ends (the hold's target, not released).
    ClearAndHold(UserStatus),
    /// A pause wants busy over the status that expired (another one, which
    /// the pause left alone): set busy and clear the text, restoring this
    /// status when the pause ends.
    ClearToBusy(UserStatus),
}

/// The action of [`ExpiryStep::Clear`]`(restore)` while `busy_wanted`
/// (notifications paused with "Busy while paused") with the chosen status
/// `current`: the status to go back to waits for the end of the pause,
/// under busy.
pub fn expiry_action(
    restore: Option<UserStatus>,
    busy_wanted: bool,
    current: Option<UserStatus>,
) -> ExpiryAction {
    match restore {
        Some(st) if busy_wanted && current == Some(UserStatus::Busy) => {
            ExpiryAction::ClearAndHold(st)
        }
        Some(st) if busy_wanted => ExpiryAction::ClearToBusy(st),
        r => ExpiryAction::Clear(r),
    }
}

/// An expiry set while a pause holds busy goes back to the status before
/// the pause (the hold's), not to the busy the pause set; a status change
/// never "restores" the status it sets (`restore: None`).
pub fn expiry_for_hold(mut e: StatusExpiry, hold: Option<&StatusHold>) -> StatusExpiry {
    if let Some(h) = hold.filter(|h| !h.released && h.user_id == e.user_id)
        && e.restore.is_some()
    {
        e.restore = Some(h.status);
    }
    e.restore = e.restore.filter(|s| *s != e.status);
    e
}

/// The hold once `e` expires: a pause that set busy over the expiring
/// status (the hold remembers it) ends on the status after the expiry.
/// `None`: the hold does not change.
pub fn hold_after_expiry(e: &StatusExpiry, hold: Option<&StatusHold>) -> Option<StatusHold> {
    let h = hold.filter(|h| !h.released && h.user_id == e.user_id && h.status == e.status)?;
    Some(StatusHold {
        status: e.restore?,
        text: String::new(),
        ..h.clone()
    })
}

/// Whether the app's idle away is what makes the chosen status away (for
/// [`hold_step`]'s `auto_away`): only when the server sent no
/// `statusDefault`, which an idle away never changes (an away there was
/// chosen, possibly on another device).
pub fn idle_away(app_idle_away: bool, status_default: Option<UserStatus>) -> bool {
    app_idle_away
        && status_default
            .filter(|s| *s != UserStatus::Unknown)
            .is_none()
}

/// The account's chosen status: `statusDefault` when the server sent it,
/// else the shown status, where "offline" (no connection) is not known.
pub fn chosen_status(status_default: Option<UserStatus>, shown: UserStatus) -> Option<UserStatus> {
    match status_default.filter(|s| *s != UserStatus::Unknown) {
        Some(s) => Some(s),
        None => Some(shown).filter(|s| !matches!(s, UserStatus::Offline | UserStatus::Unknown)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{FixedOffset, Local, Utc};

    fn hm(h: u16, m: u16) -> u16 {
        h * 60 + m
    }

    fn at<Tz: TimeZone>(tz: &Tz, d: u32, h: u32, m: u32) -> DateTime<Tz> {
        tz.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
    }

    /// Mon–Fri 09:00–18:00 (2026-11-16 is a Monday).
    fn office() -> WorkingHours {
        WorkingHours {
            enabled: true,
            ..WorkingHours::default()
        }
    }

    #[test]
    fn office_hours_and_boundaries() {
        let tz = FixedOffset::east_opt(7 * 3600).unwrap();
        let wh = office();
        assert!(wh.is_working(&at(&tz, 16, 9, 0)));
        assert!(wh.is_working(&at(&tz, 16, 17, 59)));
        assert!(!wh.is_working(&at(&tz, 16, 18, 0)));
        assert!(!wh.is_working(&at(&tz, 16, 8, 59)));
        assert!(!wh.is_working(&at(&tz, 21, 12, 0)), "Saturday");
        assert_eq!(
            wh.next_boundary(&at(&tz, 16, 12, 0)),
            Some(at(&tz, 16, 18, 0))
        );
        assert_eq!(
            wh.next_boundary(&at(&tz, 16, 18, 0)),
            Some(at(&tz, 17, 9, 0))
        );
        // Friday evening: Monday morning.
        assert_eq!(
            wh.next_boundary(&at(&tz, 20, 19, 0)),
            Some(at(&tz, 23, 9, 0))
        );
        // Disabled: always working, no boundary.
        let off = WorkingHours::default();
        assert!(off.is_working(&at(&tz, 21, 3, 0)));
        assert_eq!(off.next_boundary(&at(&tz, 21, 3, 0)), None);
        // No working day at all: never a boundary, always paused.
        let mut none = office();
        for wd in [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ] {
            none.set_day(wd, DayHours::default());
        }
        assert!(!none.is_working(&at(&tz, 16, 12, 0)));
        assert_eq!(none.next_boundary(&at(&tz, 16, 12, 0)), None);
    }

    #[test]
    fn overnight_ranges_reach_into_the_next_day() {
        let tz = Utc;
        let mut wh = WorkingHours {
            enabled: true,
            days: Vec::new(),
        };
        // Monday 22:00 – Tuesday 06:00, and Tuesday 06:00–14:00 right after.
        wh.set_day(
            Weekday::Mon,
            DayHours {
                on: true,
                start: hm(22, 0),
                end: hm(6, 0),
            },
        );
        wh.set_day(
            Weekday::Tue,
            DayHours {
                on: true,
                start: hm(6, 0),
                end: hm(14, 0),
            },
        );
        assert!(!wh.is_working(&at(&tz, 16, 21, 59)));
        assert!(wh.is_working(&at(&tz, 16, 22, 0)));
        assert!(wh.is_working(&at(&tz, 17, 3, 0)), "Tuesday night part");
        assert!(wh.is_working(&at(&tz, 17, 6, 0)));
        assert!(!wh.is_working(&at(&tz, 17, 14, 0)));
        // The 06:00 seam does not change anything: the next change is 14:00.
        assert_eq!(
            wh.next_boundary(&at(&tz, 16, 23, 0)),
            Some(at(&tz, 17, 14, 0))
        );
        // Sunday 22:00 into Monday.
        wh.set_day(
            Weekday::Sun,
            DayHours {
                on: true,
                start: hm(22, 0),
                end: hm(2, 0),
            },
        );
        assert!(wh.is_working(&at(&tz, 16, 1, 0)));
        assert!(!wh.is_working(&at(&tz, 16, 2, 0)));
        // A whole day (start == end).
        wh.set_day(
            Weekday::Wed,
            DayHours {
                on: true,
                start: 0,
                end: 0,
            },
        );
        assert!(wh.is_working(&at(&tz, 18, 0, 0)));
        assert!(wh.is_working(&at(&tz, 18, 23, 59)));
        assert!(!wh.is_working(&at(&tz, 19, 0, 0)));
    }

    /// A zone with the EU's 2026 rules at UTC+1: summer time from
    /// 2026-03-29 01:00 UTC (02:00 local is skipped) to 2026-10-25 01:00 UTC
    /// (02:00–03:00 local happens twice).
    #[derive(Debug, Clone, Copy)]
    struct Cet;

    #[derive(Debug, Clone, Copy)]
    struct CetOffset(FixedOffset);

    impl chrono::Offset for CetOffset {
        fn fix(&self) -> FixedOffset {
            self.0
        }
    }

    impl Cet {
        fn std() -> FixedOffset {
            FixedOffset::east_opt(3600).unwrap()
        }
        fn dst() -> FixedOffset {
            FixedOffset::east_opt(7200).unwrap()
        }
    }

    impl TimeZone for Cet {
        type Offset = CetOffset;

        fn from_offset(_: &CetOffset) -> Self {
            Cet
        }

        fn offset_from_local_date(&self, local: &NaiveDate) -> chrono::LocalResult<CetOffset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> chrono::LocalResult<CetOffset> {
            let valid: Vec<FixedOffset> = [Cet::dst(), Cet::std()]
                .into_iter()
                .filter(|off| {
                    let utc = *local - Duration::seconds(i64::from(off.local_minus_utc()));
                    self.offset_from_utc_datetime(&utc).0 == *off
                })
                .collect();
            match valid.as_slice() {
                [] => chrono::LocalResult::None,
                [o] => chrono::LocalResult::Single(CetOffset(*o)),
                [a, b, ..] => chrono::LocalResult::Ambiguous(CetOffset(*a), CetOffset(*b)),
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> CetOffset {
            self.offset_from_utc_datetime(&utc.and_hms_opt(12, 0, 0).unwrap())
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> CetOffset {
            let start = NaiveDate::from_ymd_opt(2026, 3, 29)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap();
            let end = NaiveDate::from_ymd_opt(2026, 10, 25)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap();
            CetOffset(if (start..end).contains(utc) {
                Cet::dst()
            } else {
                Cet::std()
            })
        }
    }

    fn utc(mo: u32, d: u32, h: u32, m: u32) -> i64 {
        Utc.with_ymd_and_hms(2026, mo, d, h, m, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn dst_changes_keep_local_times() {
        let wh = office();
        // Friday evening before the spring change: Monday 09:00 summer time
        // (07:00 UTC, not 08:00).
        let fri = Cet.with_ymd_and_hms(2026, 3, 27, 19, 0, 0).unwrap();
        let b = wh.next_boundary(&fri).unwrap();
        assert_eq!(b.timestamp_millis(), utc(3, 30, 7, 0));
        // And back in autumn: 08:00 UTC again.
        let fri = Cet.with_ymd_and_hms(2026, 10, 23, 19, 0, 0).unwrap();
        assert_eq!(
            wh.next_boundary(&fri).unwrap().timestamp_millis(),
            utc(10, 26, 8, 0)
        );
        // A start in the skipped hour moves an hour on (03:30 summer time).
        let mut night = WorkingHours {
            enabled: true,
            days: Vec::new(),
        };
        night.set_day(
            Weekday::Sun,
            DayHours {
                on: true,
                start: hm(2, 30),
                end: hm(10, 0),
            },
        );
        let sat = Cet.with_ymd_and_hms(2026, 3, 28, 23, 0, 0).unwrap();
        assert_eq!(
            night.next_boundary(&sat).unwrap().timestamp_millis(),
            utc(3, 29, 1, 30)
        );
        // A start in the repeated hour takes the first one.
        let sat = Cet.with_ymd_and_hms(2026, 10, 24, 23, 0, 0).unwrap();
        assert_eq!(
            night.next_boundary(&sat).unwrap().timestamp_millis(),
            utc(10, 25, 0, 30)
        );
        // Inside the range on the short day; it ends at 10:00 summer time.
        let in_range = Cet.with_ymd_and_hms(2026, 3, 29, 5, 0, 0).unwrap();
        assert!(night.is_working(&in_range));
        assert_eq!(
            night.next_boundary(&in_range).unwrap().timestamp_millis(),
            utc(3, 29, 8, 0)
        );
        // `Local` works too (whatever the machine's zone is).
        let now = Local::now();
        if let Some(b) = wh.next_boundary(&now) {
            assert!(b > now);
            assert_ne!(wh.is_working(&b), wh.is_working(&now));
        }
    }

    #[test]
    fn manual_pause_resume_and_overrides() {
        let tz = Utc;
        let wh = office();
        let mut p = PauseState::default();
        let noon = at(&tz, 16, 12, 0);
        assert_eq!(quiet_state(&p, &wh, &noon), Quiet::default());
        // 1 h pause in working hours.
        pause(&mut p, (noon + Duration::hours(1)).timestamp_millis());
        let q = quiet_state(&p, &wh, &noon);
        assert!(q.paused && !q.off_hours);
        assert_eq!(q.until, p.paused_until);
        assert_eq!(next_change(&p, &wh, &noon), p.paused_until);
        // Expired.
        let later = at(&tz, 16, 13, 0);
        assert!(!quiet_state(&p, &wh, &later).paused);
        assert!(tidy(&mut p, &wh, &later));
        assert_eq!(p, PauseState::default());
        // Off hours: paused until 09:00 the next morning.
        let evening = at(&tz, 16, 20, 0);
        let q = quiet_state(&p, &wh, &evening);
        assert!(q.paused && q.off_hours);
        assert_eq!(q.until, Some(at(&tz, 17, 9, 0).timestamp_millis()));
        // A manual resume holds until the next boundary only.
        resume(&mut p, &wh, &evening);
        assert_eq!(p.resumed_until, Some(at(&tz, 17, 9, 0).timestamp_millis()));
        assert!(!quiet_state(&p, &wh, &evening).paused);
        assert!(!quiet_state(&p, &wh, &at(&tz, 17, 12, 0)).paused);
        assert!(
            quiet_state(&p, &wh, &at(&tz, 17, 18, 0)).paused,
            "next evening"
        );
        // Resuming a manual pause in working hours leaves no override.
        let mut p = PauseState::default();
        pause(&mut p, (noon + Duration::hours(2)).timestamp_millis());
        resume(&mut p, &wh, &noon);
        assert_eq!(p, PauseState::default());
        // A manual pause beyond the end of working hours: the later end.
        let mut p = PauseState::default();
        let late = at(&tz, 16, 17, 30);
        pause(&mut p, (late + Duration::hours(2)).timestamp_millis());
        let q = quiet_state(&p, &wh, &at(&tz, 16, 18, 30));
        assert!(q.paused && q.off_hours);
        assert_eq!(q.until, Some(at(&tz, 17, 9, 0).timestamp_millis()));
    }

    #[test]
    fn presets() {
        let tz = Utc;
        let now = at(&tz, 18, 14, 10); // Wednesday
        assert_eq!(PausePreset::Min30.until(&now, 9), at(&tz, 18, 14, 40));
        assert_eq!(PausePreset::Hour2.until(&now, 9), at(&tz, 18, 16, 10));
        assert_eq!(PausePreset::Tomorrow.until(&now, 9), at(&tz, 19, 9, 0));
        assert_eq!(ClearAfter::Never.at(&now), None);
        assert_eq!(ClearAfter::Hour4.at(&now), Some(at(&tz, 18, 18, 10)));
        assert_eq!(ClearAfter::Today.at(&now), Some(at(&tz, 19, 0, 0)));
        assert_eq!(ClearAfter::ThisWeek.at(&now), Some(at(&tz, 23, 0, 0)));
        // On a Sunday "this week" ends at midnight.
        let sunday = at(&tz, 22, 10, 0);
        assert_eq!(ClearAfter::ThisWeek.at(&sunday), Some(at(&tz, 23, 0, 0)));
    }

    #[test]
    fn busy_while_paused_and_restore_unless_changed() {
        use UserStatus::*;
        let step = |wanted, hold: Option<&StatusHold>, cur| {
            hold_step(wanted, hold, "u", Some(cur), "Lunch", false)
        };
        let HoldStep::SetBusy(h) = step(true, None, Online) else {
            panic!("busy expected");
        };
        assert_eq!(
            (h.status, h.text.as_str(), h.released),
            (Online, "Lunch", false)
        );
        // Paused and busy: nothing to do.
        assert_eq!(step(true, Some(&h), Busy), HoldStep::Nothing);
        // The pause ended: back to online.
        assert_eq!(step(false, Some(&h), Busy), HoldStep::Restore(Online));
        // The user picked another status during the pause: kept.
        assert_eq!(step(true, Some(&h), Away), HoldStep::Release);
        let released = StatusHold {
            released: true,
            ..h.clone()
        };
        assert_eq!(step(true, Some(&released), Away), HoldStep::Nothing);
        assert_eq!(step(false, Some(&released), Away), HoldStep::Forget);
        // Changed after the pause ended (not busy): nothing restored.
        assert_eq!(step(false, Some(&h), Online), HoldStep::Forget);
        // Busy or invisible already: remembered without a call.
        assert!(matches!(step(true, None, Busy), HoldStep::Keep(h) if h.released));
        assert!(matches!(step(true, None, Offline), HoldStep::Keep(h) if h.released));
        // The app's idle away counts as online.
        assert!(matches!(
            hold_step(true, None, "u", Some(Away), "", true),
            HoldStep::SetBusy(h) if h.status == Online
        ));
        // A hold of another account is ignored.
        let other = StatusHold {
            user_id: "x".into(),
            ..h
        };
        assert!(matches!(
            step(true, Some(&other), Online),
            HoldStep::SetBusy(_)
        ));
        assert_eq!(step(false, None, Online), HoldStep::Nothing);
    }

    #[test]
    fn expiring_status_clears_unless_changed() {
        use UserStatus::*;
        let e = StatusExpiry {
            user_id: "u".into(),
            at: 1_000,
            status: Busy,
            text: "In a meeting".into(),
            restore: Some(Online),
        };
        assert_eq!(
            expiry_step(&e, "u", 999, Some(Busy), "In a meeting"),
            ExpiryStep::Wait
        );
        assert_eq!(
            expiry_step(&e, "u", 1_000, Some(Busy), "In a meeting"),
            ExpiryStep::Clear(Some(Online))
        );
        // Expired while the app was off: cleared on the next start.
        assert_eq!(
            expiry_step(&e, "u", 99_000, Some(Busy), "In a meeting"),
            ExpiryStep::Clear(Some(Online))
        );
        // The status changed but not the text: only the text goes.
        assert_eq!(
            expiry_step(&e, "u", 1_000, Some(Away), "In a meeting"),
            ExpiryStep::Clear(None)
        );
        // The text changed: the user's new status stays.
        assert_eq!(
            expiry_step(&e, "u", 500, Some(Busy), "Back"),
            ExpiryStep::Forget
        );
        assert_eq!(
            expiry_step(&e, "u", 1_000, Some(Busy), ""),
            ExpiryStep::Forget
        );
        assert_eq!(
            expiry_step(&e, "v", 1_000, Some(Busy), "In a meeting"),
            ExpiryStep::Forget
        );
    }
    #[test]
    fn unknown_status_decides_nothing() {
        use UserStatus::*;
        let h = StatusHold {
            user_id: "u".into(),
            status: Online,
            text: String::new(),
            released: false,
        };
        // "offline" without a connection while statusDefault stays busy.
        assert_eq!(chosen_status(None, Offline), None);
        assert_eq!(chosen_status(Some(Busy), Offline), Some(Busy));
        assert_eq!(chosen_status(Some(Offline), Offline), Some(Offline));
        assert_eq!(chosen_status(None, Away), Some(Away));
        assert_eq!(
            hold_step(true, Some(&h), "u", None, "", false),
            HoldStep::Nothing
        );
        assert_eq!(
            hold_step(false, Some(&h), "u", None, "", false),
            HoldStep::Nothing
        );
        assert_eq!(
            hold_step(true, None, "u", None, "", false),
            HoldStep::Nothing
        );
        let e = StatusExpiry {
            user_id: "u".into(),
            at: 1_000,
            status: Busy,
            text: String::new(),
            restore: Some(Online),
        };
        assert_eq!(expiry_step(&e, "u", 5_000, None, ""), ExpiryStep::Wait);
    }

    #[test]
    fn expiries_and_pauses_keep_the_right_status() {
        use UserStatus::*;
        // Busy with an expiry, then a pause: the expiry clears the text and
        // the status to go back to waits for the end of the pause.
        assert_eq!(
            expiry_action(Some(Online), true, Some(Busy)),
            ExpiryAction::ClearAndHold(Online)
        );
        assert_eq!(
            expiry_action(Some(Online), false, Some(Busy)),
            ExpiryAction::Clear(Some(Online))
        );
        assert_eq!(
            expiry_action(None, true, Some(Busy)),
            ExpiryAction::Clear(None)
        );
        // Away for 30 min, then off hours (the app was off meanwhile, or the
        // user chose Away during the pause): busy takes over, the status
        // after the expiry comes back when the pause ends.
        assert_eq!(
            expiry_action(Some(Online), true, Some(Away)),
            ExpiryAction::ClearToBusy(Online)
        );
        // An expiry set during a busy pause goes back to the pre-pause status.
        let hold = StatusHold {
            user_id: "u".into(),
            status: Away,
            text: String::new(),
            released: false,
        };
        let e = StatusExpiry {
            user_id: "u".into(),
            at: 5,
            status: Online,
            text: "Lunch".into(),
            restore: Some(Busy),
        };
        assert_eq!(expiry_for_hold(e.clone(), Some(&hold)).restore, Some(Away));
        let released = StatusHold {
            released: true,
            ..hold.clone()
        };
        assert_eq!(
            expiry_for_hold(e.clone(), Some(&released)).restore,
            Some(Busy)
        );
        let text_only = StatusExpiry {
            restore: None,
            ..e.clone()
        };
        assert_eq!(expiry_for_hold(text_only, Some(&hold)).restore, None);
        let same = StatusExpiry {
            status: Away,
            ..e.clone()
        };
        assert_eq!(expiry_for_hold(same, Some(&hold)).restore, None);
        // Busy for an hour chosen during a busy pause (the status was busy
        // already): it still goes back to the pre-pause status.
        let busy = StatusExpiry {
            status: Busy,
            restore: Some(Busy),
            ..e.clone()
        };
        let online = StatusHold {
            status: Online,
            ..hold.clone()
        };
        assert_eq!(
            expiry_for_hold(busy.clone(), Some(&online)).restore,
            Some(Online)
        );
        assert_eq!(
            expiry_for_hold(busy, None).restore,
            None,
            "nothing to restore"
        );
    }

    #[test]
    fn an_expiry_under_a_busy_pause_moves_the_hold() {
        use UserStatus::*;
        // Away until 18:20 (back to online); the pause at 18:00 set busy
        // over it: the pause ends on online, not on the expired away.
        let e = StatusExpiry {
            user_id: "u".into(),
            at: 5,
            status: Away,
            text: "Lunch".into(),
            restore: Some(Online),
        };
        let hold = StatusHold {
            user_id: "u".into(),
            status: Away,
            text: "Lunch".into(),
            released: false,
        };
        let h = hold_after_expiry(&e, Some(&hold)).unwrap();
        assert_eq!((h.status, h.text.as_str(), h.released), (Online, "", false));
        assert_eq!(
            hold_step(false, Some(&h), "u", Some(Busy), "", false),
            HoldStep::Restore(Online)
        );
        // Another status held, a released hold, a text-only expiry: unchanged.
        let other = StatusHold {
            status: Online,
            ..hold.clone()
        };
        assert_eq!(hold_after_expiry(&e, Some(&other)), None);
        let released = StatusHold {
            released: true,
            ..hold.clone()
        };
        assert_eq!(hold_after_expiry(&e, Some(&released)), None);
        let text_only = StatusExpiry {
            restore: None,
            ..e.clone()
        };
        assert_eq!(hold_after_expiry(&text_only, Some(&hold)), None);
        assert_eq!(hold_after_expiry(&e, None), None);
    }

    #[test]
    fn a_chosen_away_is_not_the_idle_away() {
        use UserStatus::*;
        assert!(idle_away(true, None));
        assert!(idle_away(true, Some(Unknown)));
        // Away chosen on the phone while this desktop idles: kept.
        assert!(!idle_away(true, Some(Away)));
        assert!(!idle_away(false, None));
        assert!(matches!(
            hold_step(true, None, "u", Some(Away), "", idle_away(true, Some(Away))),
            HoldStep::SetBusy(h) if h.status == Away
        ));
    }

    #[test]
    fn resume_without_working_days_and_whole_days() {
        let tz = Utc;
        let mut wh = WorkingHours {
            enabled: true,
            days: Vec::new(),
        };
        let now = at(&tz, 16, 12, 0);
        assert!(quiet_state(&PauseState::default(), &wh, &now).paused);
        let mut p = PauseState::default();
        resume(&mut p, &wh, &now);
        assert_eq!(p.resumed_until, Some(UNTIL_BOUNDARY));
        assert!(!quiet_state(&p, &wh, &now).paused);
        assert_eq!(next_change(&p, &wh, &now), None);
        assert!(!tidy(&mut p, &wh, &now));
        // A working day appears: the resume lasts until its boundary.
        wh.set_day(
            Weekday::Tue,
            DayHours {
                on: true,
                start: hm(9, 0),
                end: hm(17, 0),
            },
        );
        assert!(tidy(&mut p, &wh, &now));
        assert_eq!(p.resumed_until, Some(at(&tz, 17, 9, 0).timestamp_millis()));
        // Working hours turned off: forgotten.
        let mut p = PauseState {
            resumed_until: Some(UNTIL_BOUNDARY),
            ..PauseState::default()
        };
        assert!(tidy(&mut p, &WorkingHours::default(), &now));
        assert_eq!(p, PauseState::default());
        // 09:00–09:00 is 24 hours: the check and the boundary agree.
        let mut wh = WorkingHours {
            enabled: true,
            days: Vec::new(),
        };
        wh.set_day(
            Weekday::Mon,
            DayHours {
                on: true,
                start: hm(9, 0),
                end: hm(9, 0),
            },
        );
        assert!(!wh.is_working(&at(&tz, 16, 8, 59)));
        assert!(wh.is_working(&at(&tz, 16, 9, 0)));
        assert!(wh.is_working(&at(&tz, 17, 8, 59)));
        assert!(!wh.is_working(&at(&tz, 17, 9, 0)));
        assert_eq!(
            wh.next_boundary(&at(&tz, 16, 12, 0)),
            Some(at(&tz, 17, 9, 0))
        );
    }
}
