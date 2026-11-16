//! Message reminders: "remind me about this message at 09:00", synced between
//! the user's devices.
//!
//! The synced form is a [`ReminderBlob`] stored as JSON under
//! [`REMINDERS_KEY`](rocketchat::models::REMINDERS_KEY) in the `customFields`
//! of the user's hidden service room ([`crate::service_sync`]). One
//! last-writer-wins entry per message id holds the room id, the thread id of
//! a reply and the due time (UTC ms); a reminder that was done, dismissed or
//! removed is a tombstone that expires after [`TOMBSTONE_TTL_MS`]. Only ids
//! and times are stored, never message text.
//!
//! Reminders fire while Rustket runs ([`due`]); the preset times of the
//! picker are computed in local time ([`preset_at`], [`custom_at`]).

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone};
use serde_json::{Map, Value, json};

use crate::service_sync::{
    EditBlob, Entry, MAX_FUTURE_MS, Stamp, SyncBlob, covers_map, merge_entry, merge_map, stamp_of,
    str_of, uncovered_map, valid_id, written,
};

/// Format written into the blob's `v`. Readers accept any version.
pub const REMINDERS_FORMAT: u64 = 1;
/// Most live reminders per account.
pub const MAX_REMINDERS: usize = 200;
/// Done or removed reminders are forgotten after this long.
pub const TOMBSTONE_TTL_MS: i64 = 30 * 24 * 3600 * 1000;

/// A reminder's entry. `at: None` is a tombstone (done, dismissed, removed).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReminderMark {
    pub stamp: Stamp,
    pub at: Option<i64>,
    pub rid: Option<String>,
    pub tmid: Option<String>,
}

impl Entry for ReminderMark {
    fn stamp(&self) -> &Stamp {
        &self.stamp
    }
    fn is_tombstone(&self) -> bool {
        self.at.is_none()
    }
}

impl ReminderMark {
    /// Lenient: a live entry needs a valid room id and a time, else it is a
    /// tombstone (it still hides older entries).
    fn from_object(o: &Map<String, Value>) -> Self {
        let rid = str_of(o, "r").filter(|r| valid_id(r));
        let tmid = str_of(o, "m").filter(|m| valid_id(m));
        let at = o
            .get("at")
            .and_then(|a| a.as_i64().or_else(|| a.as_f64().map(|f| f as i64)))
            .filter(|_| rid.is_some());
        Self {
            stamp: stamp_of(o),
            at,
            rid: at.and(rid),
            tmid: at.and(tmid),
        }
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        if let (Some(at), Some(rid)) = (self.at, &self.rid) {
            o.insert("r".into(), json!(rid));
            if let Some(t) = &self.tmid {
                o.insert("m".into(), json!(t));
            }
            o.insert("at".into(), json!(at));
        }
        o.insert("t".into(), json!(self.stamp.t));
        if !self.stamp.d.is_empty() {
            o.insert("d".into(), json!(self.stamp.d));
        }
        Value::Object(o)
    }
}

/// A change the user made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderEdit {
    /// A new reminder, or a new time for an existing one.
    Set {
        rid: String,
        tmid: Option<String>,
        mid: String,
        at_ms: i64,
    },
    /// Done, dismissed, opened or removed.
    Clear { mid: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReminderError {
    #[error("invalid room or message id")]
    InvalidId,
    #[error("too many reminders")]
    Full,
    #[error("the reminder time is in the past")]
    Past,
    #[error("the synced data is too large")]
    TooLarge,
}

/// The synced reminders: a last-writer-wins map by message id plus
/// top-level keys a newer version added (kept on write).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReminderBlob {
    pub items: BTreeMap<String, ReminderMark>,
    v: u64,
    extra: Map<String, Value>,
}

const KNOWN_KEYS: [&str; 2] = ["v", "items"];

impl ReminderBlob {
    /// The live reminders.
    pub fn view(&self) -> Reminders {
        let items = self
            .items
            .iter()
            .filter_map(|(mid, m)| {
                Some((
                    mid.clone(),
                    Reminder {
                        mid: mid.clone(),
                        rid: m.rid.clone()?,
                        tmid: m.tmid.clone(),
                        at_ms: m.at?,
                        set_by: m.stamp.d.clone(),
                    },
                ))
            })
            .collect();
        Reminders { items }
    }

    fn live(&self) -> usize {
        self.items.values().filter(|m| !m.is_tombstone()).count()
    }

    /// Records `edit` as pending entries of this blob (the device's overlay)
    /// on top of `stored`. Returns whether anything changed.
    pub fn edit(
        &mut self,
        stored: &Self,
        edit: &ReminderEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, ReminderError> {
        let current = stored.merged_with(self);
        match edit {
            ReminderEdit::Set {
                rid,
                tmid,
                mid,
                at_ms,
            } => {
                if !valid_id(rid) || !valid_id(mid) || tmid.as_deref().is_some_and(|t| !valid_id(t))
                {
                    return Err(ReminderError::InvalidId);
                }
                if *at_ms <= now_ms {
                    return Err(ReminderError::Past);
                }
                let prev = current.items.get(mid);
                if prev.is_some_and(|m| {
                    m.at == Some(*at_ms) && m.rid.as_ref() == Some(rid) && m.tmid == *tmid
                }) {
                    return Ok(false);
                }
                if prev.is_none_or(Entry::is_tombstone) && current.live() >= MAX_REMINDERS {
                    return Err(ReminderError::Full);
                }
                let mark = ReminderMark {
                    stamp: Stamp::after(prev.map(|m| &m.stamp), now_ms, device),
                    at: Some(*at_ms),
                    rid: Some(rid.clone()),
                    tmid: tmid.clone(),
                };
                Ok(merge_entry(&mut self.items, mid, &mark))
            }
            ReminderEdit::Clear { mid } => {
                let Some(prev) = current.items.get(mid).filter(|m| !m.is_tombstone()) else {
                    return Ok(false);
                };
                let mark = ReminderMark {
                    stamp: Stamp::after(Some(&prev.stamp), now_ms, device),
                    at: None,
                    rid: None,
                    tmid: None,
                };
                Ok(merge_entry(&mut self.items, mid, &mark))
            }
        }
    }
}

impl SyncBlob for ReminderBlob {
    const KEY: &'static str = rocketchat::models::REMINDERS_KEY;
    const TOMBSTONE_TTL_MS: Option<i64> = Some(TOMBSTONE_TTL_MS);

    /// Lenient: anything that is not an object is empty; ids that cannot key
    /// an entry and entries stamped more than [`MAX_FUTURE_MS`] ahead are
    /// dropped.
    fn from_value_at(v: Option<&Value>, now_ms: i64) -> Self {
        let max_t = now_ms.saturating_add(MAX_FUTURE_MS);
        let Some(root) = v.and_then(Value::as_object) else {
            return Self::default();
        };
        let mut b = Self {
            v: root.get("v").and_then(Value::as_u64).unwrap_or(0),
            extra: root
                .iter()
                .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            ..Self::default()
        };
        let items = root.get("items").and_then(Value::as_object);
        for (mid, e) in items.into_iter().flatten() {
            let empty = Map::new();
            let o = match e {
                Value::Object(o) => o,
                Value::Null => &empty,
                _ => continue,
            };
            if !valid_id(mid) || stamp_of(o).t > max_t {
                continue;
            }
            merge_entry(&mut b.items, mid, &ReminderMark::from_object(o));
        }
        b
    }

    fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value {
        let copies: Vec<&BTreeMap<String, ReminderMark>> =
            copies.iter().map(|c| &c.items).collect();
        let items: Map<String, Value> =
            written(&self.items, &copies, Self::TOMBSTONE_TTL_MS, now_ms)
                .map(|(k, m)| (k.clone(), m.to_value()))
                .collect();
        let mut root = self.extra.clone();
        root.insert("v".into(), json!(self.v.max(REMINDERS_FORMAT)));
        root.insert("items".into(), Value::Object(items));
        Value::Object(root)
    }

    fn merge(&mut self, other: &Self) -> bool {
        let changed = merge_map(&mut self.items, &other.items);
        self.v = self.v.max(other.v);
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        changed
    }

    fn covers_at(&self, other: &Self, now_ms: i64) -> bool {
        covers_map(&self.items, &other.items, Self::TOMBSTONE_TTL_MS, now_ms)
    }

    fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self {
        Self {
            items: uncovered_map(&self.items, &stored.items, Self::TOMBSTONE_TTL_MS, now_ms),
            ..Self::default()
        }
    }

    fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool {
        let before = self.items.len();
        self.items = uncovered_map(&self.items, &stored.items, Self::TOMBSTONE_TTL_MS, now_ms);
        self.items.len() != before
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

impl EditBlob for ReminderBlob {
    type Edit = ReminderEdit;
    type Error = ReminderError;

    fn apply_edit(
        &mut self,
        stored: &Self,
        edit: &ReminderEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, ReminderError> {
        self.edit(stored, edit, now_ms, device)
    }

    fn too_large() -> ReminderError {
        ReminderError::TooLarge
    }
}

/// A live reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub mid: String,
    pub rid: String,
    /// The thread of a reply (opened in the thread panel).
    pub tmid: Option<String>,
    pub at_ms: i64,
    /// The device that set it.
    pub set_by: String,
}

/// The live reminders by message id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reminders {
    items: BTreeMap<String, Reminder>,
}

impl Reminders {
    pub fn get(&self, mid: &str) -> Option<&Reminder> {
        self.items.get(mid)
    }

    /// Every reminder, soonest first.
    pub fn iter(&self) -> impl Iterator<Item = &Reminder> {
        let mut v: Vec<&Reminder> = self.items.values().collect();
        v.sort_by(|a, b| (a.at_ms, &a.mid).cmp(&(b.at_ms, &b.mid)));
        v.into_iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The reminders `keep` accepts.
    pub fn filtered(&self, keep: impl Fn(&Reminder) -> bool) -> Reminders {
        Reminders {
            items: self
                .items
                .iter()
                .filter(|(_, r)| keep(r))
                .map(|(k, r)| (k.clone(), r.clone()))
                .collect(),
        }
    }

    /// Reminders due at `now_ms` or earlier.
    pub fn overdue(&self, now_ms: i64) -> usize {
        self.items.values().filter(|r| r.at_ms <= now_ms).count()
    }

    /// The first due time after `now_ms`.
    pub fn next_due_after(&self, now_ms: i64) -> Option<i64> {
        self.items
            .values()
            .map(|r| r.at_ms)
            .filter(|at| *at > now_ms)
            .min()
    }
}

/// What [`due`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Due<'a> {
    /// Reminders to notify about now.
    pub fire: Vec<&'a Reminder>,
    /// Reminders that were due before this process saw them: one summary.
    pub missed: Vec<&'a Reminder>,
}

/// The reminders due at `now_ms`, skipping those already handled (`shown`:
/// `(mid, at_ms)`, so a reminder set to a new time fires again). Those due
/// before `since_ms` (when this process first saw the account's reminders)
/// were missed.
pub fn due<'a>(
    view: &'a Reminders,
    now_ms: i64,
    since_ms: i64,
    shown: &HashSet<(String, i64)>,
) -> Due<'a> {
    let mut d = Due::default();
    for r in view.iter() {
        if r.at_ms > now_ms || shown.contains(&(r.mid.clone(), r.at_ms)) {
            continue;
        }
        if r.at_ms < since_ms {
            d.missed.push(r);
        } else {
            d.fire.push(r);
        }
    }
    d
}

/// The picker's preset times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preset {
    In20Min,
    In1Hour,
    In3Hours,
    /// The next day at the morning hour.
    Tomorrow,
    /// The next Monday at the morning hour (a Monday gives the following one).
    NextWeek,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::In20Min,
        Preset::In1Hour,
        Preset::In3Hours,
        Preset::Tomorrow,
        Preset::NextWeek,
    ];
}

/// `date` at `hour:minute` local time; a time skipped by a DST change moves
/// an hour on.
fn local_at<Tz: TimeZone>(
    tz: &Tz,
    date: NaiveDate,
    hour: u32,
    minute: u32,
) -> Option<DateTime<Tz>> {
    let t = date.and_hms_opt(hour, minute, 0)?;
    tz.from_local_datetime(&t)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(t + Duration::hours(1))).earliest())
}

/// The time of preset `p` from `now`, in `now`'s time zone.
pub fn preset_at<Tz: TimeZone>(p: Preset, now: &DateTime<Tz>, morning_hour: u32) -> DateTime<Tz> {
    let hour = morning_hour.min(23);
    let day = |days: i64| {
        let date = now.date_naive() + Duration::days(days);
        local_at(&now.timezone(), date, hour, 0)
            .unwrap_or_else(|| now.clone() + Duration::days(days))
    };
    match p {
        Preset::In20Min => now.clone() + Duration::minutes(20),
        Preset::In1Hour => now.clone() + Duration::hours(1),
        Preset::In3Hours => now.clone() + Duration::hours(3),
        Preset::Tomorrow => day(1),
        Preset::NextWeek => {
            let from_monday = i64::from(now.weekday().num_days_from_monday());
            day(7 - from_monday)
        }
    }
}

/// `days` days after `now`'s date at `hour:00` local time (due dates).
pub fn day_at<Tz: TimeZone>(now: &DateTime<Tz>, days: i64, hour: u32) -> DateTime<Tz> {
    let date = now.date_naive() + Duration::days(days);
    local_at(&now.timezone(), date, hour.min(23), 0)
        .unwrap_or_else(|| now.clone() + Duration::days(days))
}

/// A custom date and time in `tz`; `None` for a time a DST change skips or
/// an invalid time.
pub fn custom_at<Tz: TimeZone>(date: NaiveDate, h: u32, m: u32, tz: &Tz) -> Option<DateTime<Tz>> {
    let t = date.and_hms_opt(h, m, 0)?;
    tz.from_local_datetime(&t).earliest()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{FixedOffset, Timelike, Weekday};

    fn set(mid: &str, at: i64) -> ReminderEdit {
        ReminderEdit::Set {
            rid: "R1".into(),
            tmid: None,
            mid: mid.into(),
            at_ms: at,
        }
    }

    #[test]
    fn parse_is_lenient() {
        let v = json!({
            "v": 3,
            "future": {"x": 1},
            "items": {
                "m1": {"r": "R1", "m": "T1", "at": 5000, "t": 10, "d": "A"},
                "m2": {"at": 5000, "t": 10, "d": "A"},
                "a.b": {"r": "R1", "at": 5000, "t": 10},
                "$x": {"r": "R1", "at": 5000, "t": 10},
                "m3": {"r": "R1", "at": "soon", "t": 10},
                "m4": {"r": "R1", "at": 5000, "t": 10 + MAX_FUTURE_MS + 1},
                "m5": null,
                "m6": 7,
            }
        });
        let b = ReminderBlob::from_value_at(Some(&v), 0);
        let view = b.view();
        assert_eq!(view.len(), 1);
        let r = view.get("m1").unwrap();
        assert_eq!(
            (r.rid.as_str(), r.tmid.as_deref(), r.at_ms),
            ("R1", Some("T1"), 5000)
        );
        assert_eq!(r.set_by, "A");
        // Entries without a room or time are tombstones; bad ids and far
        // future stamps are dropped.
        assert!(b.items["m2"].is_tombstone() && b.items["m3"].is_tombstone());
        assert!(b.items["m5"].is_tombstone());
        assert!(!b.items.contains_key("a.b") && !b.items.contains_key("m4"));
        assert!(!b.items.contains_key("m6"));
        let out = b.to_value_at(100);
        assert_eq!(out["v"], json!(3));
        assert_eq!(out["future"], json!({"x": 1}));
        assert_eq!(
            ReminderBlob::from_value_at(Some(&json!([1])), 0),
            ReminderBlob::default()
        );
        assert_eq!(ReminderBlob::default().to_value_at(0)["v"], json!(1));
    }

    /// The blob of a service subscription captured from the dev server.
    #[test]
    fn reads_the_captured_service_fields() {
        let v = crate::test_util::fixture("subscriptions_get_one_service_later.json");
        let r = &v["subscription"]["customFields"][rocketchat::models::REMINDERS_KEY];
        let b = ReminderBlob::from_value_at(Some(r), 1_794_738_100_000);
        let view = b.view();
        assert_eq!(view.len(), 2);
        let reply = view.get("Wx3yZ4aB5cD6eF7gH").unwrap();
        assert_eq!(reply.tmid.as_deref(), Some("Kl8mN9oP0qR1sT2uV"));
        assert_eq!(reply.set_by, "dev-b");
        assert!(b.items["Ij8kL9mN0oP1qR2sT"].is_tombstone());
        assert_eq!(
            view.next_due_after(1_794_738_100_000),
            Some(1_794_810_000_000)
        );
    }

    #[test]
    fn set_reschedule_clear() {
        let stored = ReminderBlob::default();
        let mut o = ReminderBlob::default();
        assert_eq!(o.edit(&stored, &set("m1", 2000), 1000, "A"), Ok(true));
        assert_eq!(o.edit(&stored, &set("m1", 2000), 1000, "A"), Ok(false));
        assert_eq!(o.edit(&stored, &set("m1", 3000), 1001, "A"), Ok(true));
        assert_eq!(o.view().get("m1").unwrap().at_ms, 3000);
        assert_eq!(
            o.edit(&stored, &set("m2", 900), 1000, "A"),
            Err(ReminderError::Past)
        );
        let clear = ReminderEdit::Clear { mid: "m1".into() };
        assert_eq!(o.edit(&stored, &clear, 1002, "A"), Ok(true));
        assert!(o.view().is_empty());
        assert!(o.items["m1"].is_tombstone());
        assert_eq!(o.edit(&stored, &clear, 1003, "A"), Ok(false));
        let bad = ReminderEdit::Set {
            rid: "a.b".into(),
            tmid: None,
            mid: "m".into(),
            at_ms: 5000,
        };
        assert_eq!(
            o.edit(&stored, &bad, 1000, "A"),
            Err(ReminderError::InvalidId)
        );
        // The written form carries ids and times only.
        let out = o.to_value_at(1004);
        assert_eq!(out["items"]["m1"], json!({"t": 1002, "d": "A"}));
    }

    #[test]
    fn cap_counts_live_only() {
        let stored = ReminderBlob::default();
        let mut o = ReminderBlob::default();
        for n in 0..MAX_REMINDERS {
            o.edit(&stored, &set(&format!("m{n}"), 5000), 1000, "A")
                .unwrap();
        }
        assert_eq!(
            o.edit(&stored, &set("extra", 5000), 1000, "A"),
            Err(ReminderError::Full)
        );
        // A reschedule needs no room.
        assert_eq!(o.edit(&stored, &set("m0", 6000), 1001, "A"), Ok(true));
        let clear = ReminderEdit::Clear { mid: "m1".into() };
        o.edit(&stored, &clear, 1002, "A").unwrap();
        assert_eq!(o.edit(&stored, &set("extra", 5000), 1003, "A"), Ok(true));
    }

    #[test]
    fn merge_is_commutative_and_newer_wins() {
        let mut a = ReminderBlob::default();
        a.edit(&ReminderBlob::default(), &set("m1", 5000), 100, "A")
            .unwrap();
        let mut b = ReminderBlob::default();
        b.edit(&ReminderBlob::default(), &set("m1", 7000), 100, "B")
            .unwrap();
        let ab = a.merged_with(&b);
        let ba = b.merged_with(&a);
        assert_eq!(ab, ba);
        // Same time: the larger device id wins everywhere.
        assert_eq!(ab.view().get("m1").unwrap().at_ms, 7000);
        assert_eq!(ab.merged_with(&ab), ab);
        assert!(ab.covers_at(&a, 0) && ab.covers_at(&b, 0));
    }

    #[test]
    fn tombstones_expire_on_write_and_count_as_covered() {
        let mut o = ReminderBlob::default();
        o.edit(&ReminderBlob::default(), &set("m1", 5000), 100, "A")
            .unwrap();
        o.edit(
            &ReminderBlob::default(),
            &ReminderEdit::Clear { mid: "m1".into() },
            200,
            "A",
        )
        .unwrap();
        let later = 200 + TOMBSTONE_TTL_MS + 1;
        assert_eq!(o.to_value_at(later)["items"], json!({}));
        assert!(ReminderBlob::default().covers_at(&o, later));
        assert!(!ReminderBlob::default().covers_at(&o, 300));
    }

    #[test]
    fn due_splits_fire_and_missed() {
        let mut o = ReminderBlob::default();
        let s = ReminderBlob::default();
        o.edit(&s, &set("old", 1000), 0, "A").unwrap();
        o.edit(&s, &set("now", 3000), 0, "A").unwrap();
        o.edit(&s, &set("later", 9000), 0, "A").unwrap();
        let view = o.view();
        let mut shown = HashSet::new();
        let d = due(&view, 5000, 2000, &shown);
        assert_eq!(
            d.missed.iter().map(|r| r.mid.as_str()).collect::<Vec<_>>(),
            ["old"]
        );
        assert_eq!(
            d.fire.iter().map(|r| r.mid.as_str()).collect::<Vec<_>>(),
            ["now"]
        );
        shown.insert(("now".to_owned(), 3000));
        shown.insert(("old".to_owned(), 1000));
        let d = due(&view, 5000, 2000, &shown);
        assert!(d.fire.is_empty() && d.missed.is_empty());
        // Rescheduled: fires again.
        o.edit(&s, &set("now", 4000), 1, "A").unwrap();
        let view = o.view();
        let d = due(&view, 5000, 2000, &shown);
        assert_eq!(d.fire.len(), 1);
        assert_eq!(view.overdue(5000), 2);
        assert_eq!(view.next_due_after(5000), Some(9000));
        assert_eq!(view.next_due_after(9000), None);
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(7 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
    }

    #[test]
    fn preset_times() {
        // 2026-11-16 is a Monday.
        let mon = at(2026, 11, 16, 23, 59);
        assert_eq!(mon.weekday(), Weekday::Mon);
        assert_eq!(preset_at(Preset::Tomorrow, &mon, 9), at(2026, 11, 17, 9, 0));
        assert_eq!(preset_at(Preset::NextWeek, &mon, 9), at(2026, 11, 23, 9, 0));
        let early = at(2026, 11, 17, 0, 1);
        assert_eq!(
            preset_at(Preset::Tomorrow, &early, 8),
            at(2026, 11, 18, 8, 0)
        );
        let sun = at(2026, 11, 22, 12, 0);
        assert_eq!(preset_at(Preset::NextWeek, &sun, 9), at(2026, 11, 23, 9, 0));
        let p = preset_at(Preset::In20Min, &mon, 9);
        assert_eq!((p.day(), p.hour(), p.minute()), (17, 0, 19));
        assert_eq!(
            preset_at(Preset::In3Hours, &mon, 9),
            at(2026, 11, 17, 2, 59)
        );
        assert_eq!(day_at(&mon, 0, 18), at(2026, 11, 16, 18, 0));
        let date = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(
            custom_at(date, 14, 30, &mon.timezone()),
            Some(at(2026, 12, 1, 14, 30))
        );
        assert_eq!(custom_at(date, 25, 0, &mon.timezone()), None);
    }
}
