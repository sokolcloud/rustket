//! Later and reminders (`crate::later`, `crate::reminders`): the blobs the
//! service rooms hold, this device's pending edits, the starred-message
//! index and the rows of the Later view.

use std::time::Instant;

use rocketchat::models::{Message, RoomType};

use crate::later::{LaterBlob, LaterIndex, LaterStatus, ts_ms};
use crate::reminders::{ReminderBlob, Reminders};
use crate::service_sync::SyncBlob;
use crate::thread_index::StarredScan;

use super::{Dirty, Workspace};

/// A tab of the Later view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LaterTab {
    #[default]
    InProgress,
    Reminders,
    Done,
    Archived,
}

impl LaterTab {
    pub const ALL: [LaterTab; 4] = [
        LaterTab::InProgress,
        LaterTab::Reminders,
        LaterTab::Done,
        LaterTab::Archived,
    ];
}

/// One row of the Later view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaterRow {
    pub mid: String,
    pub rid: String,
    /// The thread of a reply.
    pub tmid: Option<String>,
    /// Starred by the user (else a reminder only).
    pub starred: bool,
    pub status: LaterStatus,
    pub due_ms: Option<i64>,
    pub reminder_ms: Option<i64>,
    /// The message's time.
    pub ts_ms: Option<i64>,
}

impl Workspace {
    // --- Later -----------------------------------------------------------------

    /// The effective Later state: the stored blob with this device's
    /// pending edits.
    pub fn later(&self) -> &LaterBlob {
        &self.later
    }

    /// The Later blob of the user's service rooms, as the subscriptions last
    /// showed it.
    pub fn later_blob(&self) -> &LaterBlob {
        &self.later_blob
    }

    /// The pending edits set with [`Self::set_later_overlay`].
    pub fn later_overlay(&self) -> &LaterBlob {
        &self.later_overlay
    }

    /// This device's Later edits not known to be stored yet.
    pub fn set_later_overlay(&mut self, overlay: LaterBlob) {
        if self.later_overlay == overlay {
            return;
        }
        self.later_overlay = overlay;
        self.mark(Dirty::LATER);
        self.update_later();
    }

    pub(super) fn set_later_blob(&mut self, blob: LaterBlob) {
        if self.later_blob == blob {
            return;
        }
        self.later_blob = blob;
        self.mark(Dirty::LATER);
        self.update_later();
    }

    fn update_later(&mut self) {
        let later = self.later_blob.merged_with(&self.later_overlay);
        if later != self.later {
            self.later = later;
            self.mark(Dirty::LATER);
        }
    }

    /// Messages whose Later entries (status, due date) are orphans: the
    /// message is not starred here although its room's last complete
    /// starred scan finished after the entry was written (unstarred on
    /// another client). They are invisible; at the cap they are forgotten
    /// first. Nothing while the index is capped or Later does not track.
    pub fn later_orphans(&self) -> Vec<String> {
        if !self.later_tracking || self.later_index.is_full() {
            return Vec::new();
        }
        let orphan = |mid: &str, rid: Option<&str>, stamp: &crate::service_sync::Stamp| {
            !self.later_index.is_starred(mid)
                && rid
                    .and_then(|r| self.later_index.scan(r))
                    .and_then(|s| s.scanned_at)
                    .is_some_and(|at| at.timestamp_millis() > stamp.t)
        };
        let status = self
            .later
            .status
            .iter()
            .filter(|(mid, m)| m.status.is_some() && orphan(mid, m.rid.as_deref(), &m.stamp))
            .map(|(mid, _)| mid);
        let due = self
            .later
            .due
            .iter()
            .filter(|(mid, m)| m.at.is_some() && orphan(mid, m.rid.as_deref(), &m.stamp))
            .map(|(mid, _)| mid);
        let all: std::collections::BTreeSet<&String> = status.chain(due).collect();
        all.into_iter().cloned().collect()
    }

    /// Starred messages and reminder copies.
    pub fn later_index(&self) -> &LaterIndex {
        &self.later_index
    }

    /// The message of a Later row (starred or a reminder copy).
    pub fn later_message(&self, mid: &str) -> Option<&Message> {
        self.later_index.get(mid)
    }

    /// Mirrors whether the app tracks starred messages for Later. Tracking
    /// runs only while the server allows starring; call it on every update
    /// (the server settings may change).
    pub fn set_later_tracking(&mut self, on: bool) {
        self.later_wanted = on;
        self.apply_later_tracking();
    }

    pub(super) fn apply_later_tracking(&mut self) {
        let on = self.later_wanted && self.settings.allow_starring();
        if self.later_tracking == on {
            return;
        }
        self.later_tracking = on;
        if !on {
            self.later_index.clear();
        }
        self.mark(Dirty::LATER);
    }

    pub fn later_tracking(&self) -> bool {
        self.later_tracking
    }

    /// Whether rooms' starred messages are scanned: for the starred threads
    /// in Favorites or for Later.
    pub fn scans_starred(&self) -> bool {
        self.thread_index.pins_enabled() || self.later_tracking
    }

    /// The last starred-messages scan of a room as every consumer that
    /// tracks stars (Later, the starred threads) sees it: the older of
    /// their records, `None` while one of them has none (the room is due
    /// for them).
    pub fn starred_scan(&self, rid: &str) -> Option<StarredScan> {
        let later = self
            .later_tracking
            .then(|| self.later_index.scan(rid).copied());
        let pins = self
            .thread_index
            .pins_enabled()
            .then(|| self.thread_index.starred_scan(rid).copied());
        match (later, pins) {
            (Some(a), Some(b)) => {
                let (a, b) = (a?, b?);
                Some(StarredScan {
                    scanned_at: a.scanned_at.min(b.scanned_at),
                    lm: a.lm.min(b.lm),
                })
            }
            (Some(s), None) | (None, Some(s)) => s,
            (None, None) => None,
        }
    }

    /// Every room with a starred-messages scan of any consumer, with
    /// [`Self::starred_scan`] (a room another consumer has not scanned
    /// yet comes with an empty record: never scanned).
    pub fn starred_scans(&self) -> Vec<(String, StarredScan)> {
        let mut rids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        if self.later_tracking {
            rids.extend(self.later_index.scans().map(|(r, _)| r));
        }
        if self.thread_index.pins_enabled() {
            rids.extend(self.thread_index.starred_scans().map(|(r, _)| r));
        }
        rids.into_iter()
            .map(|r| (r.to_owned(), self.starred_scan(r).unwrap_or_default()))
            .collect()
    }

    /// The user starred or unstarred `m` here: Later lists it at once (the
    /// realtime echo confirms it).
    pub fn set_message_starred(&mut self, m: &Message, on: bool) {
        let Some(me) = self.user_id().map(str::to_owned) else {
            return;
        };
        if self.later_tracking && self.later_index.set_starred(m, &me, on, Instant::now()) {
            self.mark(Dirty::LATER);
            self.retain_reminder_copies();
        }
    }

    /// Keeps a local copy of `m` for its reminder (notification text, the
    /// Later view). Nothing without a live reminder for it.
    pub fn keep_reminder_copy(&mut self, m: &Message) {
        if self.reminders.get(&m.id).is_some() && self.later_index.keep_copy(m) {
            self.mark(Dirty::LATER);
        }
    }

    /// Messages fetched for Later or a reminder.
    pub fn apply_later_messages(&mut self, msgs: Vec<Message>) {
        let Some(me) = self.user_id().map(str::to_owned) else {
            return;
        };
        let now = Instant::now();
        let mut changed = false;
        for m in msgs {
            if self.later_tracking {
                changed |= self.later_index.observe(&m, &me, now);
            }
            if self.reminders.get(&m.id).is_some() {
                changed |= self.later_index.keep_copy(&m);
            }
        }
        if changed {
            self.mark(Dirty::LATER);
        }
    }

    /// Later messages and scans from the offline cache.
    pub(super) fn restore_later(&mut self, later: crate::cache::CachedLater) {
        let me = self.user_id().map(str::to_owned);
        let scans = if self.later_tracking {
            later
                .scans
                .into_iter()
                .map(|s| {
                    (
                        s.rid,
                        StarredScan {
                            scanned_at: s.scanned_at,
                            lm: s.scanned_lm,
                        },
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        // Starred ones while tracking; copies only for a live reminder.
        let starred = |m: &Message| {
            self.later_tracking && me.as_deref().is_some_and(|me| m.is_starred_by(me))
        };
        let msgs = later
            .messages
            .into_iter()
            .filter(|m| starred(m) || self.reminders.get(&m.id).is_some())
            .collect();
        if self.later_index.restore(msgs, scans, me.as_deref()) {
            self.mark(Dirty::LATER);
        }
    }

    /// A live (or fetched) message: Later follows its star.
    pub(super) fn later_observe(&mut self, m: &Message) {
        if !self.later_tracking && self.later_index.get(&m.id).is_none() {
            return;
        }
        let Some(me) = self.user_id().map(str::to_owned) else {
            return;
        };
        let changed = if self.later_tracking {
            self.later_index.observe(m, &me, Instant::now())
        } else {
            self.later_index.keep_copy(m)
        };
        if changed {
            self.mark(Dirty::LATER);
            self.retain_reminder_copies();
        }
    }

    pub(super) fn later_remove(&mut self, mid: &str) {
        if self.later_index.remove(mid) {
            self.mark(Dirty::LATER);
        }
    }

    pub(super) fn later_forget_room(&mut self, rid: &str) {
        if self.later_index.forget_room(rid) {
            self.mark(Dirty::LATER);
        }
    }

    pub(super) fn later_scan_page(&mut self, rid: &str, msgs: &[Message], started: Instant) {
        if !self.later_tracking {
            return;
        }
        let Some(me) = self.user_id().map(str::to_owned) else {
            return;
        };
        if self.later_index.apply_scan_page(rid, msgs, &me, started) {
            self.mark(Dirty::LATER);
        }
    }

    pub(super) fn later_finish_scan(&mut self, rid: &str, started: Instant, scan: StarredScan) {
        if self.later_tracking && self.later_index.finish_scan(rid, started, scan) {
            self.mark(Dirty::LATER);
            self.retain_reminder_copies();
        }
    }

    pub(super) fn later_forget_scan(&mut self, rid: &str) {
        if self.later_index.forget_scan(rid) {
            self.mark(Dirty::LATER);
        }
    }

    // --- reminders -------------------------------------------------------------

    /// The live reminders: the stored blob with this device's pending edits.
    pub fn reminders(&self) -> &Reminders {
        &self.reminders
    }

    pub fn reminder_blob(&self) -> &ReminderBlob {
        &self.reminder_blob
    }

    pub fn reminder_overlay(&self) -> &ReminderBlob {
        &self.reminder_overlay
    }

    /// This device's reminder edits not known to be stored yet.
    pub fn set_reminder_overlay(&mut self, overlay: ReminderBlob) {
        if self.reminder_overlay == overlay {
            return;
        }
        self.reminder_overlay = overlay;
        self.mark(Dirty::REMINDERS);
        self.update_reminders();
    }

    pub(super) fn set_reminder_blob(&mut self, blob: ReminderBlob) {
        if self.reminder_blob == blob {
            return;
        }
        self.reminder_blob = blob;
        self.mark(Dirty::REMINDERS);
        self.update_reminders();
    }

    fn update_reminders(&mut self) {
        let view = self
            .reminder_blob
            .merged_with(&self.reminder_overlay)
            .view();
        if view != self.reminders {
            self.reminders = view;
            self.mark(Dirty::REMINDERS);
            self.retain_reminder_copies();
        }
    }

    /// Copies of messages whose reminder is gone are dropped.
    fn retain_reminder_copies(&mut self) {
        let reminders = &self.reminders;
        if self
            .later_index
            .retain_copies(|mid| reminders.get(mid).is_some())
        {
            self.mark(Dirty::LATER);
        }
    }

    // --- the Later view --------------------------------------------------------

    /// Whether room `rid` can have Later rows: the user is in it and it is
    /// not an omnichannel room.
    fn later_room(&self, rid: &str) -> bool {
        self.rooms.get(rid).is_some_and(|e| {
            e.sub.is_some() && !matches!(e.room_type(), RoomType::Livechat | RoomType::Voip)
        })
    }

    fn later_row(&self, mid: &str, rid: &str, m: Option<&Message>, starred: bool) -> LaterRow {
        let ts = m.and_then(ts_ms);
        LaterRow {
            mid: mid.to_owned(),
            rid: rid.to_owned(),
            tmid: m.and_then(|m| m.tmid.clone()),
            starred,
            status: self.later.status_of(mid, ts),
            due_ms: self.later.due_of(mid),
            reminder_ms: self.reminders.get(mid).map(|r| r.at_ms),
            ts_ms: ts,
        }
    }

    /// The rows of a Later tab, in the tab's order: In progress by due date
    /// (undated last, newest first), Reminders soonest first, Done and
    /// Archived newest first.
    pub fn later_rows(&self, tab: LaterTab) -> Vec<LaterRow> {
        let mut rows: Vec<LaterRow> = match tab {
            LaterTab::Reminders => self
                .reminders
                .iter()
                .filter(|r| self.later_room(&r.rid))
                .map(|r| {
                    let m = self.later_index.get(&r.mid);
                    let mut row =
                        self.later_row(&r.mid, &r.rid, m, self.later_index.is_starred(&r.mid));
                    if row.tmid.is_none() {
                        row.tmid = r.tmid.clone();
                    }
                    row
                })
                .collect(),
            _ => {
                let want = match tab {
                    LaterTab::Done => LaterStatus::Done,
                    LaterTab::Archived => LaterStatus::Archived,
                    _ => LaterStatus::InProgress,
                };
                self.later_index
                    .starred()
                    .filter(|m| self.later_room(&m.rid))
                    .map(|m| self.later_row(&m.id, &m.rid, Some(m), true))
                    .filter(|r| r.status == want)
                    .collect()
            }
        };
        match tab {
            LaterTab::InProgress => rows.sort_by(|a, b| {
                (
                    a.due_ms.is_none(),
                    a.due_ms,
                    std::cmp::Reverse(a.ts_ms),
                    &a.mid,
                )
                    .cmp(&(
                        b.due_ms.is_none(),
                        b.due_ms,
                        std::cmp::Reverse(b.ts_ms),
                        &b.mid,
                    ))
            }),
            LaterTab::Reminders => {}
            LaterTab::Done | LaterTab::Archived => rows.sort_by(|a, b| {
                (std::cmp::Reverse(a.ts_ms), &a.mid).cmp(&(std::cmp::Reverse(b.ts_ms), &b.mid))
            }),
        }
        rows
    }

    /// Rows per tab, in [`LaterTab::ALL`] order (one pass, no rows built).
    pub fn later_counts(&self) -> [usize; 4] {
        let mut counts = [0; 4];
        for m in self.later_index.starred() {
            if !self.later_room(&m.rid) {
                continue;
            }
            let i = match self.later.status_of(&m.id, ts_ms(m)) {
                LaterStatus::InProgress => 0,
                LaterStatus::Done => 2,
                LaterStatus::Archived => 3,
            };
            counts[i] += 1;
        }
        counts[1] = self
            .reminders
            .iter()
            .filter(|r| self.later_room(&r.rid))
            .count();
        counts
    }

    /// The row of message `mid` as its Later tabs show it (starred, else a
    /// reminder), without building a tab's rows.
    pub fn later_row_of(&self, mid: &str) -> Option<LaterRow> {
        let reminder = self.reminders.get(mid).filter(|r| self.later_room(&r.rid));
        let m = self.later_index.get(mid);
        if self.later_index.is_starred(mid)
            && let Some(m) = m.filter(|m| self.later_room(&m.rid))
        {
            let mut row = self.later_row(mid, &m.rid, Some(m), true);
            if row.tmid.is_none() {
                row.tmid = reminder.and_then(|r| r.tmid.clone());
            }
            return Some(row);
        }
        let r = reminder?;
        let mut row = self.later_row(mid, &r.rid, m, false);
        if row.tmid.is_none() {
            row.tmid = r.tmid.clone();
        }
        Some(row)
    }

    /// The reminders of rooms the user is in (not omnichannel): the ones
    /// that fire and that the Later view lists. Those of rooms the user
    /// left stay synced (the room may come back) but stay quiet.
    pub fn active_reminders(&self) -> Reminders {
        self.reminders.filtered(|r| self.later_room(&r.rid))
    }

    /// Active reminders due at `now_ms` or earlier (the sidebar's Later row
    /// counts them).
    pub fn overdue_reminders(&self, now_ms: i64) -> usize {
        self.active_reminders().overdue(now_ms)
    }
}
