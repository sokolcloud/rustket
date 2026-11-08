//! Message timelines of a room or a thread.
//!
//! Messages are ordered by `(ts, _id)` and deduplicated by `_id`. A room timeline
//! holds top-level messages and thread replies sent with `tshow` ("also send to
//! channel"); a thread timeline holds the replies of one thread plus its parent.
//! Local pending messages (client `_id` from `rocketchat::ids::random_id`) live in
//! the same ordering with a [`DeliveryState`] and are replaced when the server
//! echo with the same `_id` arrives.
//!
//! A server copy older than the stored one (by `_updatedAt`) never replaces it,
//! so a REST result read before a realtime update cannot roll it back.
//!
//! A room timeline only shows a contiguous range of history: from the newest
//! message down to the oldest loaded page (the *window*). Rocket.Chat broadcasts
//! changes to old messages (reactions, edits, thread-parent counters) and
//! `chat.syncMessages` returns them; such a message is updated when it is already
//! shown and ignored otherwise, so it never extends the window and leaves no gap
//! above it. Pagination always continues from the window's oldest edge.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, TimeZone, Utc};
use rocketchat::RcDate;
use rocketchat::models::{Message, Paginated};
use rocketchat::rest::HistoryCursor;

/// Default page size for history requests.
pub const PAGE_SIZE: u32 = 50;

static NEXT_TIMELINE_ID: AtomicU64 = AtomicU64::new(1);

/// Identifies one page request: the timeline instance and the request number.
/// A result whose ticket is no longer the outstanding request (the timeline was
/// reset, closed and reopened, or a newer request replaced it) is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageTicket {
    timeline: u64,
    request: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeliveryState {
    /// Known to the server.
    #[default]
    Sent,
    /// Local message waiting for the server.
    Sending,
    /// Sending failed; the UI offers retry/discard.
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineItem {
    pub message: Message,
    pub state: DeliveryState,
}

impl TimelineItem {
    pub fn is_pending(&self) -> bool {
        self.state != DeliveryState::Sent
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineKind {
    Room,
    Thread { tmid: String },
}

/// Cursor for loading older messages.
#[derive(Debug, Clone)]
pub enum PageCursor {
    /// Room history via `loadHistory`.
    History(HistoryCursor),
}

#[derive(Debug, Clone, Default)]
pub struct Pagination {
    /// More messages exist before the oldest loaded one (assumed until proven otherwise).
    pub has_more_before: bool,
    pub loading_before: bool,
    /// At least one page was loaded.
    pub loaded: bool,
    /// Error of the last page request.
    pub error: Option<String>,
    /// Messages the last successful page added to this timeline. A room history
    /// page can consist of thread replies only (hidden here); when this is 0 and
    /// `has_more_before` is still true, load the next page right away.
    pub last_page_added: usize,
    cursor: Option<PageCursor>,
    /// `ts` (ms) of the oldest message of the loaded window, `i64::MIN` once
    /// the start of the room (thread) is reached. `None` before the first page.
    window_floor: Option<i64>,
    /// Thread timelines: entries of loaded pages that could not be parsed
    /// (they still occupy server positions).
    dropped_total: u64,
    /// The outstanding page request.
    in_flight: Option<u64>,
    /// Messages deleted while the request was outstanding: dropped from its page.
    deleted_in_flight: HashSet<String>,
    /// Thread: loaded-prefix replies deleted while the request was outstanding
    /// (server positions moved down by this much).
    shift_in_flight: u64,
}

type Key = (i64, String);

fn ts_ms(m: &Message) -> i64 {
    m.ts.or(m.updated_at).map_or(0, |d| d.timestamp_millis())
}

#[derive(Debug, Clone)]
pub struct Timeline {
    rid: String,
    kind: TimelineKind,
    items: BTreeMap<Key, TimelineItem>,
    index: HashMap<String, i64>,
    /// Thread parent (thread timelines only).
    parent: Option<Message>,
    pub pagination: Pagination,
    /// Subscription `ls` captured when the room was opened.
    unread_since: Option<RcDate>,
    /// Largest server `_updatedAt` applied (server clock) for `chat.syncMessages`.
    last_updated_at: Option<RcDate>,
    version: u64,
    id: u64,
    next_request: u64,
}

impl Timeline {
    pub fn new_room(rid: impl Into<String>) -> Self {
        Self::new(rid.into(), TimelineKind::Room)
    }

    pub fn new_thread(rid: impl Into<String>, tmid: impl Into<String>) -> Self {
        Self::new(rid.into(), TimelineKind::Thread { tmid: tmid.into() })
    }

    fn new(rid: String, kind: TimelineKind) -> Self {
        Self {
            rid,
            kind,
            items: BTreeMap::new(),
            index: HashMap::new(),
            parent: None,
            pagination: Pagination {
                has_more_before: true,
                ..Default::default()
            },
            unread_since: None,
            last_updated_at: None,
            version: 0,
            id: NEXT_TIMELINE_ID.fetch_add(1, Ordering::Relaxed),
            next_request: 0,
        }
    }

    pub fn rid(&self) -> &str {
        &self.rid
    }

    pub fn kind(&self) -> &TimelineKind {
        &self.kind
    }

    pub fn tmid(&self) -> Option<&str> {
        match &self.kind {
            TimelineKind::Thread { tmid } => Some(tmid),
            TimelineKind::Room => None,
        }
    }

    /// Increments on every change; use it to memoise derived views.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn parent(&self) -> Option<&Message> {
        self.parent.as_ref()
    }

    pub fn last_updated_at(&self) -> Option<RcDate> {
        self.last_updated_at
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// Whether `m` belongs here (room: not a thread reply unless `tshow`; thread:
    /// a reply of this thread).
    pub fn accepts(&self, m: &Message) -> bool {
        if m.rid != self.rid {
            return false;
        }
        match &self.kind {
            TimelineKind::Room => m.tmid.is_none() || m.tshow == Some(true),
            TimelineKind::Thread { tmid } => m.tmid.as_deref() == Some(tmid.as_str()),
        }
    }

    /// Whether `m` is an older copy of what is stored (by `_updatedAt`).
    fn is_stale(&self, m: &Message) -> bool {
        let stored = match &self.kind {
            TimelineKind::Thread { tmid } if &m.id == tmid => self.parent.as_ref(),
            _ => self
                .get(&m.id)
                .filter(|i| !i.is_pending())
                .map(|i| &i.message),
        };
        matches!(
            (stored.and_then(|s| s.updated_at), m.updated_at),
            (Some(old), Some(new)) if new < old
        )
    }

    /// Whether a message that is not shown yet lies inside the loaded window
    /// (always true before the first page).
    fn in_window(&self, m: &Message) -> bool {
        match self.pagination.window_floor {
            Some(floor) => ts_ms(m) >= floor,
            None => true,
        }
    }

    fn note_updated(&mut self, m: &Message) {
        if let Some(u) = m.updated_at
            && self.last_updated_at.is_none_or(|l| u > l)
        {
            self.last_updated_at = Some(u);
        }
    }

    /// Inserts or replaces a server message. `_hidden` messages and copies older
    /// than the stored one are ignored; a message that no longer belongs here is
    /// removed; a room message outside the loaded window is only updated when
    /// already shown. Returns whether anything changed.
    pub fn upsert(&mut self, m: Message) -> bool {
        if m.hidden == Some(true) || self.is_stale(&m) {
            return false;
        }
        if let TimelineKind::Thread { tmid } = &self.kind
            && &m.id == tmid
        {
            self.note_updated(&m);
            if self.parent.as_ref() == Some(&m) {
                return false;
            }
            self.parent = Some(m);
            self.bump();
            return true;
        }
        if !self.accepts(&m) {
            return self.remove(&m.id);
        }
        self.note_updated(&m);
        if !self.contains(&m.id) && !self.in_window(&m) {
            return false;
        }
        self.put(m, DeliveryState::Sent)
    }

    fn put(&mut self, m: Message, state: DeliveryState) -> bool {
        let ts = ts_ms(&m);
        if let Some(old_ts) = self.index.get(&m.id).copied() {
            let key = (old_ts, m.id.clone());
            if let Some(old) = self.items.get(&key)
                && old.message == m
                && old.state == state
            {
                return false;
            }
            self.items.remove(&key);
        }
        self.index.insert(m.id.clone(), ts);
        self.items
            .insert((ts, m.id.clone()), TimelineItem { message: m, state });
        self.bump();
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        if self.pagination.in_flight.is_some() {
            self.pagination.deleted_in_flight.insert(id.to_owned());
        }
        if let TimelineKind::Thread { tmid } = &self.kind
            && tmid == id
            && self.parent.take().is_some()
        {
            self.bump();
            return true;
        }
        match self.index.remove(id) {
            Some(ts) => {
                let removed = self.items.remove(&(ts, id.to_owned()));
                // A loaded reply deleted while a thread page is outstanding
                // moved the server positions of every older reply.
                if matches!(self.kind, TimelineKind::Thread { .. })
                    && self.pagination.in_flight.is_some()
                    && removed.as_ref().is_some_and(|i| !i.is_pending())
                {
                    self.pagination.shift_in_flight += 1;
                }
                self.bump();
                true
            }
            None => false,
        }
    }

    /// Adds messages (any order) through [`Self::upsert`].
    pub fn extend(&mut self, messages: impl IntoIterator<Item = Message>) -> bool {
        self.extend_counting(messages) > 0
    }

    fn extend_counting(&mut self, messages: impl IntoIterator<Item = Message>) -> usize {
        let mut added = 0;
        for m in messages {
            let new = !self.contains(&m.id);
            if self.upsert(m) && new {
                added += 1;
            }
        }
        added
    }

    /// Adds a local message in `Sending` state (its `ts` should be "now").
    pub fn insert_pending(&mut self, m: Message) -> bool {
        self.put(m, DeliveryState::Sending)
    }

    /// Re-inserts a pending item kept outside the timeline (the workspace outbox)
    /// unless the message is already present.
    pub fn restore_pending(&mut self, item: TimelineItem) -> bool {
        if self.contains(&item.message.id) || !item.is_pending() {
            return false;
        }
        self.put(item.message, item.state)
    }

    pub fn mark_failed(&mut self, id: &str, error: impl Into<String>) -> bool {
        self.set_state(
            id,
            DeliveryState::Failed {
                error: error.into(),
            },
        )
    }

    /// Back to `Sending` (retry).
    pub fn mark_sending(&mut self, id: &str) -> bool {
        self.set_state(id, DeliveryState::Sending)
    }

    fn set_state(&mut self, id: &str, state: DeliveryState) -> bool {
        let Some(ts) = self.index.get(id).copied() else {
            return false;
        };
        match self.items.get_mut(&(ts, id.to_owned())) {
            Some(item) if item.is_pending() && item.state != state => {
                item.state = state;
                self.bump();
                true
            }
            _ => false,
        }
    }

    /// Removes a pending (not yet sent) message.
    pub fn discard_pending(&mut self, id: &str) -> bool {
        if self.get(id).is_some_and(TimelineItem::is_pending) {
            self.remove(id)
        } else {
            false
        }
    }

    pub fn get(&self, id: &str) -> Option<&TimelineItem> {
        let ts = self.index.get(id)?;
        self.items.get(&(*ts, id.to_owned()))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }

    /// Items oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &TimelineItem> + '_ {
        self.items.values()
    }

    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &Message> + '_ {
        self.items.values().map(|i| &i.message)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn oldest(&self) -> Option<&Message> {
        self.messages().next()
    }

    pub fn newest(&self) -> Option<&Message> {
        self.messages().next_back()
    }

    pub fn pending(&self) -> impl Iterator<Item = &TimelineItem> + '_ {
        self.items.values().filter(|i| i.is_pending())
    }

    /// Rewrites messages in place (thread parent included), e.g. to show the
    /// decrypted content of end-to-end encrypted messages. `f` returns whether
    /// it changed the message; the version is bumped if any did. `_id`, `ts`
    /// and `tmid` must not be changed.
    pub fn rewrite(&mut self, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut changed = false;
        if let Some(p) = self.parent.as_mut() {
            changed |= f(p);
        }
        for item in self.items.values_mut() {
            changed |= f(&mut item.message);
        }
        if changed {
            self.bump();
        }
        changed
    }

    /// Drops every server message (pending ones stay) and resets pagination, e.g.
    /// when a sync reports more changes than can be applied incrementally or an
    /// import added messages; the next [`Self::begin_load_older`] starts again at
    /// the newest message.
    /// Results of requests made before the reset are dropped.
    pub fn reset(&mut self) {
        self.items.retain(|_, i| i.is_pending());
        self.index = self
            .items
            .keys()
            .map(|(ts, id)| (id.clone(), *ts))
            .collect();
        self.pagination = Pagination {
            has_more_before: true,
            ..Default::default()
        };
        self.last_updated_at = None;
        self.bump();
    }

    // --- unread divider -------------------------------------------------------

    /// Captures the subscription's last-seen time when the room is opened.
    pub fn set_unread_since(&mut self, ls: Option<RcDate>) {
        if self.unread_since != ls {
            self.unread_since = ls;
            self.bump();
        }
    }

    pub fn unread_since(&self) -> Option<RcDate> {
        self.unread_since
    }

    /// Id of the first message after the captured `ls` not sent by `me`; the
    /// unread divider goes above it.
    pub fn unread_divider(&self, me: &str) -> Option<&str> {
        let ls = self.unread_since?.timestamp_millis();
        self.items
            .range((ls + 1, String::new())..)
            .map(|(_, i)| i)
            .find(|i| !i.is_pending() && i.message.u.id != me)
            .map(|i| i.message.id.as_str())
    }

    // --- pagination -----------------------------------------------------------

    /// Starts loading older room history. Returns the cursor to run
    /// (`HistoryCursor::next_page`) or `None` when already loading or at the start.
    /// Continues from the loaded window's oldest edge; without a loaded page it
    /// starts at the newest message (never below messages that merely arrived in
    /// realtime or are pending with a client-clock `ts`).
    pub fn begin_load_older(&mut self) -> Option<HistoryCursor> {
        self.begin_load_older_ticket().map(|(c, _)| c)
    }

    /// [`Self::begin_load_older`] with a ticket for [`Self::finish_load_older_ticket`].
    pub fn begin_load_older_ticket(&mut self) -> Option<(HistoryCursor, PageTicket)> {
        if self.pagination.loading_before || !self.pagination.has_more_before {
            return None;
        }
        let cursor = match &self.pagination.cursor {
            Some(PageCursor::History(c)) => c.clone(),
            _ => HistoryCursor::new(self.rid.clone(), PAGE_SIZE),
        };
        let ticket = self.start_request();
        Some((cursor, ticket))
    }

    fn start_request(&mut self) -> PageTicket {
        self.next_request += 1;
        self.pagination.in_flight = Some(self.next_request);
        self.pagination.deleted_in_flight.clear();
        self.pagination.shift_in_flight = 0;
        self.pagination.loading_before = true;
        self.bump();
        PageTicket {
            timeline: self.id,
            request: self.next_request,
        }
    }

    /// Ends the outstanding request if `ticket` is it (`None`: whichever is
    /// outstanding). Returns false for a stale result, which must be dropped.
    fn end_request(&mut self, ticket: Option<PageTicket>) -> bool {
        let current = match (ticket, self.pagination.in_flight) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(t), Some(r)) => t.timeline == self.id && t.request == r,
        };
        if current {
            self.pagination.in_flight = None;
            self.pagination.loading_before = false;
        }
        current
    }

    /// Completes [`Self::begin_load_older`] with the advanced cursor. Dropped when
    /// no request is outstanding (e.g. the timeline was reset meanwhile); use the
    /// ticket variant to also tell apart requests made before a reset.
    pub fn finish_load_older(
        &mut self,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        self.finish_older(None, cursor, result)
    }

    /// Completes [`Self::begin_load_older_ticket`]; a stale ticket is dropped.
    /// Returns whether the result was applied.
    pub fn finish_load_older_ticket(
        &mut self,
        ticket: PageTicket,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        self.finish_older(Some(ticket), cursor, result)
    }

    fn finish_older(
        &mut self,
        ticket: Option<PageTicket>,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        if !self.end_request(ticket) {
            return false;
        }
        let deleted = std::mem::take(&mut self.pagination.deleted_in_flight);
        match result {
            Ok(mut messages) => {
                self.pagination.error = None;
                let first = !self.pagination.loaded;
                let floor = if cursor.is_done() {
                    i64::MIN
                } else {
                    let oldest = messages.iter().filter(|m| m.ts.is_some()).map(ts_ms).min();
                    match (oldest, self.pagination.window_floor) {
                        (Some(o), Some(f)) => o.min(f),
                        (Some(o), None) => o,
                        (None, f) => f.unwrap_or(i64::MIN),
                    }
                };
                self.pagination.window_floor = Some(floor);
                messages.retain(|m| !deleted.contains(&m.id));
                self.pagination.last_page_added = self.extend_counting(messages);
                if first {
                    self.prune_below_window();
                }
                self.pagination.loaded = true;
                self.pagination.has_more_before = !cursor.is_done();
                self.pagination.cursor = Some(PageCursor::History(cursor));
            }
            Err(e) => self.pagination.error = Some(e),
        }
        self.bump();
        true
    }

    /// Drops server messages below the window (they arrived in realtime before
    /// the first page and would show above a gap).
    fn prune_below_window(&mut self) {
        let Some(floor) = self.pagination.window_floor else {
            return;
        };
        let before = self.items.len();
        self.items
            .retain(|(ts, _), item| item.is_pending() || *ts >= floor);
        if self.items.len() != before {
            self.index = self
                .items
                .keys()
                .map(|(ts, id)| (id.clone(), *ts))
                .collect();
        }
    }

    /// Starts loading thread replies (newest first, see
    /// `RestClient::thread_messages_latest`); returns the offset to request.
    /// Loaded replies are always the newest contiguous part of the thread, so
    /// the offset is their count: replies arriving or deleted in realtime keep
    /// it in step without bookkeeping.
    pub fn begin_load_thread(&mut self) -> Option<u64> {
        self.begin_load_thread_ticket().map(|(o, _)| o)
    }

    /// [`Self::begin_load_thread`] with a ticket for [`Self::finish_load_thread_ticket`].
    pub fn begin_load_thread_ticket(&mut self) -> Option<(u64, PageTicket)> {
        if self.pagination.loading_before || !self.pagination.has_more_before {
            return None;
        }
        let offset = if self.pagination.loaded {
            self.items.values().filter(|i| !i.is_pending()).count() as u64
                + self.pagination.dropped_total
        } else {
            0
        };
        let ticket = self.start_request();
        Some((offset, ticket))
    }

    /// Completes [`Self::begin_load_thread`] with a `chat.getThreadMessages`
    /// page (newest first); dropped when no request is outstanding.
    pub fn finish_load_thread(&mut self, result: Result<Paginated<Message>, String>) -> bool {
        self.finish_thread(None, result)
    }

    /// Completes [`Self::begin_load_thread_ticket`]; a stale ticket is dropped.
    pub fn finish_load_thread_ticket(
        &mut self,
        ticket: PageTicket,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        self.finish_thread(Some(ticket), result)
    }

    fn finish_thread(
        &mut self,
        ticket: Option<PageTicket>,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        if !self.end_request(ticket) {
            return false;
        }
        let deleted = std::mem::take(&mut self.pagination.deleted_in_flight);
        let shift = std::mem::take(&mut self.pagination.shift_in_flight);
        match result {
            // A loaded reply was deleted meanwhile: the page may have skipped
            // the reply next to the loaded part. Ask again.
            Ok(_) if shift > 0 => {
                self.pagination.error = None;
                self.pagination.last_page_added = 0;
            }
            Ok(mut page) => {
                self.pagination.error = None;
                let first = !self.pagination.loaded;
                let has_more = page.has_more() && page.raw_len > 0;
                let floor = if has_more {
                    let oldest = page.items.iter().map(ts_ms).min();
                    match (oldest, self.pagination.window_floor) {
                        (Some(o), Some(f)) => o.min(f),
                        (Some(o), None) => o,
                        (None, f) => f.unwrap_or(i64::MIN),
                    }
                } else {
                    i64::MIN
                };
                self.pagination.window_floor = Some(floor);
                self.pagination.dropped_total += page.dropped as u64;
                page.items.retain(|m| !deleted.contains(&m.id));
                self.pagination.last_page_added = self.extend_counting(page.items);
                if first {
                    self.prune_below_window();
                }
                self.pagination.loaded = true;
                self.pagination.has_more_before = has_more;
            }
            Err(e) => self.pagination.error = Some(e),
        }
        self.bump();
        true
    }

    /// Sets the `chat.syncMessages` watermark when no message provided one (an
    /// empty room), so a resync still asks for messages posted meanwhile.
    pub fn set_sync_watermark_if_missing(&mut self, at: RcDate) {
        if self.last_updated_at.is_none() {
            self.last_updated_at = Some(at);
        }
    }
}

// --- grouping helpers ----------------------------------------------------------

fn local_day<Tz: TimeZone>(d: DateTime<Utc>, tz: &Tz) -> chrono::NaiveDate {
    d.with_timezone(tz).date_naive()
}

/// Whether `cur` starts a new local day relative to `prev` (or is the first message).
pub fn is_new_day<Tz: TimeZone>(prev: Option<&Message>, cur: &Message, tz: &Tz) -> bool {
    let (Some(p), Some(c)) = (prev.and_then(|p| p.ts), cur.ts) else {
        return prev.is_none();
    };
    local_day(p.0, tz) != local_day(c.0, tz)
}

/// Whether `cur` continues `prev`'s group (no avatar/name header): same author,
/// alias, avatar and emoji, within `grouping_period_secs`, same local day, neither
/// is a system message, both in the same thread (or neither) and `cur` is not the
/// first unread message.
pub fn is_sequential<Tz: TimeZone>(
    prev: &Message,
    cur: &Message,
    grouping_period_secs: i64,
    tz: &Tz,
    unread_divider: Option<&str>,
) -> bool {
    if unread_divider == Some(cur.id.as_str()) {
        return false;
    }
    if prev.is_system() || cur.is_system() {
        return false;
    }
    if prev.groupable == Some(false) || cur.groupable == Some(false) {
        return false;
    }
    if prev.u.id != cur.u.id
        || prev.alias != cur.alias
        || prev.avatar != cur.avatar
        || prev.emoji != cur.emoji
    {
        return false;
    }
    if prev.tmid != cur.tmid {
        return false;
    }
    let (Some(p), Some(c)) = (prev.ts, cur.ts) else {
        return false;
    };
    let diff = c.timestamp_millis() - p.timestamp_millis();
    if diff < 0 || diff >= grouping_period_secs.saturating_mul(1000) {
        return false;
    }
    local_day(p.0, tz) == local_day(c.0, tz)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::test_util::{fixture, msg, ts};
    use chrono::FixedOffset;
    use rocketchat::models::History;

    #[test]
    fn ordering_and_dedup() {
        let mut t = Timeline::new_room("r");
        assert!(t.upsert(msg("b", "r", 2000, "u")));
        assert!(t.upsert(msg("a", "r", 1000, "u")));
        assert!(t.upsert(msg("c", "r", 2000, "u")));
        assert!(!t.upsert(msg("a", "r", 1000, "u")));
        let ids: Vec<_> = t.messages().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        // Edit replaces in place.
        let mut e = msg("b", "r", 2000, "u");
        e.msg = "edited".into();
        e.updated_at = Some(ts(5000));
        assert!(t.upsert(e));
        assert_eq!(t.len(), 3);
        assert_eq!(t.get("b").unwrap().message.msg, "edited");
        assert_eq!(t.last_updated_at(), Some(ts(5000)));
        assert!(t.remove("a"));
        assert!(!t.remove("a"));
        assert_eq!(t.oldest().unwrap().id, "b");
        assert_eq!(t.newest().unwrap().id, "c");
        // Other room ignored, hidden ignored.
        assert!(!t.upsert(msg("x", "other", 1, "u")));
        let mut h = msg("h", "r", 10, "u");
        h.hidden = Some(true);
        assert!(!t.upsert(h));
    }

    #[test]
    fn thread_replies() {
        let mut room = Timeline::new_room("r");
        let mut th = Timeline::new_thread("r", "p");
        let parent = msg("p", "r", 1000, "u");
        let mut reply = msg("r1", "r", 2000, "u");
        reply.tmid = Some("p".into());
        let mut shown = msg("r2", "r", 3000, "u");
        shown.tmid = Some("p".into());
        shown.tshow = Some(true);
        for m in [parent.clone(), reply.clone(), shown.clone()] {
            room.upsert(m.clone());
            th.upsert(m);
        }
        let ids: Vec<_> = room.messages().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["p", "r2"]);
        let ids: Vec<_> = th.messages().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["r1", "r2"]);
        assert_eq!(th.parent().unwrap().id, "p");
        // Parent update (tcount) applies to both.
        let mut p2 = parent;
        p2.tcount = Some(2);
        assert!(room.upsert(p2.clone()));
        assert!(th.upsert(p2));
        assert_eq!(th.parent().unwrap().tcount, Some(2));
        assert_eq!(room.get("p").unwrap().message.tcount, Some(2));
    }

    #[test]
    fn pending_reconciliation() {
        let mut t = Timeline::new_room("r");
        t.upsert(msg("a", "r", 1000, "u"));
        let id = rocketchat::ids::random_id();
        let local = msg(&id, "r", 9000, "me");
        assert!(t.insert_pending(local));
        assert_eq!(t.get(&id).unwrap().state, DeliveryState::Sending);
        assert!(t.mark_failed(&id, "network"));
        assert_eq!(
            t.get(&id).unwrap().state,
            DeliveryState::Failed {
                error: "network".into()
            }
        );
        assert!(t.mark_sending(&id));
        // Server echo with the same id and the server's ts.
        let echo = msg(&id, "r", 8000, "me");
        assert!(t.upsert(echo));
        assert_eq!(t.len(), 2);
        let item = t.get(&id).unwrap();
        assert_eq!(item.state, DeliveryState::Sent);
        assert_eq!(item.message.ts, Some(ts(8000)));
        assert!(!t.mark_failed(&id, "x"));
        assert!(!t.discard_pending(&id));
        assert_eq!(t.pending().count(), 0);
    }

    #[test]
    fn unread_divider() {
        let mut t = Timeline::new_room("r");
        t.upsert(msg("a", "r", 1000, "u"));
        t.upsert(msg("b", "r", 2000, "me"));
        t.upsert(msg("c", "r", 3000, "u"));
        assert_eq!(t.unread_divider("me"), None);
        t.set_unread_since(Some(ts(1000)));
        assert_eq!(t.unread_divider("me"), Some("c"));
        t.set_unread_since(Some(ts(500)));
        assert_eq!(t.unread_divider("me"), Some("a"));
        t.set_unread_since(Some(ts(3000)));
        assert_eq!(t.unread_divider("me"), None);
    }

    #[test]
    fn history_fixture_and_pagination() {
        let v = fixture("load_history.json");
        let h: History = serde_json::from_value(v["result"].clone())
            .or_else(|_| serde_json::from_value(v.clone()))
            .unwrap();
        assert!(!h.messages.is_empty());
        let rid = h.messages[0].rid.clone();
        let mut t = Timeline::new_room(rid.clone());
        let cursor = t.begin_load_older().unwrap();
        assert!(t.begin_load_older().is_none(), "already loading");
        t.finish_load_older(cursor, Ok(h.messages.clone()));
        assert!(t.pagination.loaded);
        assert!(!t.pagination.loading_before);
        // Sorted ascending.
        let tss: Vec<i64> = t.messages().map(ts_ms).collect();
        let mut sorted = tss.clone();
        sorted.sort();
        assert_eq!(tss, sorted);
        // Error path keeps state.
        let c = t.begin_load_older().unwrap();
        t.finish_load_older(c, Err("boom".into()));
        assert_eq!(t.pagination.error.as_deref(), Some("boom"));
    }

    #[test]
    fn stale_copies_are_ignored() {
        let mut t = Timeline::new_room("r");
        let mut v2 = msg("x", "r", 1000, "u");
        v2.msg = "v2".into();
        v2.updated_at = Some(ts(5000));
        assert!(t.upsert(v2));
        let mut v1 = msg("x", "r", 1000, "u");
        v1.msg = "v1".into();
        v1.updated_at = Some(ts(4000));
        assert!(!t.upsert(v1));
        assert_eq!(t.get("x").unwrap().message.msg, "v2");
        // Same for a thread parent.
        let mut th = Timeline::new_thread("r", "p");
        let mut p = msg("p", "r", 1000, "u");
        p.tcount = Some(2);
        p.updated_at = Some(ts(3000));
        assert!(th.upsert(p));
        let mut old = msg("p", "r", 1000, "u");
        old.tcount = Some(1);
        old.updated_at = Some(ts(2000));
        assert!(!th.upsert(old));
        assert_eq!(th.parent().unwrap().tcount, Some(2));
    }

    #[test]
    fn pending_message_does_not_anchor_history_cursor() {
        let mut t = Timeline::new_room("r");
        t.insert_pending(msg("local", "r", 1000, "me"));
        let c = t.begin_load_older().unwrap();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("before: None"), "{dbg}");
        // A failed first page does not anchor it either.
        t.finish_load_older(c, Err("offline".into()));
        t.upsert(msg("old", "r", 500, "u"));
        let c = t.begin_load_older().unwrap();
        assert!(format!("{c:?}").contains("before: None"));
    }

    #[test]
    fn window_and_last_page_added() {
        let mut t = Timeline::new_room("r");
        t.insert_pending(msg("local", "r", 100, "me"));
        let c = t.begin_load_older().unwrap();
        let mut reply = msg("reply", "r", 9000, "u");
        reply.tmid = Some("p".into());
        t.finish_load_older(c.clone(), Ok(vec![reply, msg("a", "r", 8000, "u")]));
        assert_eq!(
            t.pagination.last_page_added, 1,
            "the thread reply is hidden"
        );
        assert!(t.contains("local"), "pending items are never pruned");
        // Below the window: ignored; inside: inserted.
        assert!(!t.upsert(msg("old", "r", 7000, "u")));
        assert!(t.upsert(msg("new", "r", 9500, "u")));
    }

    #[test]
    fn thread_offset_follows_deleted_replies() {
        let reply = |id: &str, ms: i64| {
            let mut m = msg(id, "r", ms, "u");
            m.tmid = Some("p".into());
            m
        };
        let page = |items: Vec<Message>, offset: u64, total: u64| Paginated {
            raw_len: items.len(),
            count: items.len() as u64,
            items,
            offset,
            total,
            dropped: 0,
        };
        let mut t = Timeline::new_thread("r", "p");
        assert_eq!(t.begin_load_thread(), Some(0));
        // Newest first: r5, r4, r3 of six replies.
        t.finish_load_thread(Ok(page(
            vec![reply("r5", 6), reply("r4", 5), reply("r3", 4)],
            0,
            6,
        )));
        assert!(t.pagination.has_more_before);
        // Below the loaded window: an edit of an unloaded reply is ignored.
        assert!(!t.upsert(reply("r1", 2)));
        // A realtime reply and a deleted one keep the offset in step.
        assert!(t.upsert(reply("r9", 100)));
        assert!(t.remove("r4"));
        assert_eq!(t.begin_load_thread(), Some(3));
        // A loaded reply deleted while the page is outstanding: the page is
        // dropped and asked again.
        assert!(t.remove("r3"));
        assert!(t.finish_load_thread(Ok(page(vec![reply("r1", 2)], 3, 5))));
        assert!(!t.contains("r1"));
        assert!(t.pagination.has_more_before);
        assert_eq!(t.begin_load_thread(), Some(2));
        t.finish_load_thread(Ok(page(
            vec![reply("r2", 3), reply("r1", 2), reply("r0", 1)],
            2,
            5,
        )));
        assert!(!t.pagination.has_more_before);
        let ids: Vec<&str> = t.messages().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["r0", "r1", "r2", "r5", "r9"]);
        // Everything is loaded: any old reply is inside the window again.
        assert!(t.upsert(reply("late-edit", 0)));
        assert_eq!(t.begin_load_thread(), None);
    }

    #[test]
    fn thread_pagination() {
        let mut t = Timeline::new_thread("r", "p");
        // A reply that arrived before the first page: the page starts at 0.
        let mut early = msg("r3", "r", 3, "u");
        early.tmid = Some("p".into());
        t.upsert(early.clone());
        assert_eq!(t.begin_load_thread(), Some(0));
        let mut m = msg("r2", "r", 2, "u");
        m.tmid = Some("p".into());
        t.finish_load_thread(Ok(Paginated {
            items: vec![early, m],
            count: 2,
            offset: 0,
            total: 3,
            raw_len: 2,
            dropped: 0,
        }));
        assert!(t.pagination.has_more_before);
        assert_eq!(t.begin_load_thread(), Some(2));
        t.finish_load_thread(Ok(Paginated {
            items: vec![],
            count: 0,
            offset: 2,
            total: 2,
            raw_len: 0,
            dropped: 0,
        }));
        assert!(!t.pagination.has_more_before);
        assert_eq!(t.begin_load_thread(), None);
    }

    #[test]
    fn sequential_rules() {
        let utc = Utc;
        let a = msg("a", "r", 1_000_000, "u");
        let b = msg("b", "r", 1_060_000, "u");
        assert!(is_sequential(&a, &b, 300, &utc, None));
        assert!(!is_sequential(&a, &b, 30, &utc, None), "outside period");
        assert!(
            !is_sequential(&a, &b, 300, &utc, Some("b")),
            "unread divider"
        );
        let other = msg("b", "r", 1_060_000, "v");
        assert!(!is_sequential(&a, &other, 300, &utc, None));
        let mut sys = b.clone();
        sys.t = Some("uj".into());
        assert!(!is_sequential(&a, &sys, 300, &utc, None));
        let mut alias = b.clone();
        alias.alias = Some("bot".into());
        assert!(!is_sequential(&a, &alias, 300, &utc, None));
        let mut reply = b.clone();
        reply.tmid = Some("x".into());
        assert!(!is_sequential(&a, &reply, 300, &utc, None));
        let mut ungroup = b.clone();
        ungroup.groupable = Some(false);
        assert!(!is_sequential(&a, &ungroup, 300, &utc, None));
    }

    #[test]
    fn day_boundaries_in_local_time() {
        // 2026-01-01 23:30 UTC and 2026-01-02 00:10 UTC.
        let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(23, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let a = msg("a", "r", base, "u");
        let b = msg("b", "r", base + 40 * 60 * 1000, "u");
        let utc = Utc;
        let moscow = FixedOffset::east_opt(3 * 3600).unwrap();
        assert!(is_new_day(Some(&a), &b, &utc));
        assert!(!is_new_day(Some(&a), &b, &moscow));
        assert!(is_new_day(None, &a, &utc));
        assert!(!is_sequential(&a, &b, 3600, &utc, None));
        assert!(is_sequential(&a, &b, 3600, &moscow, None));
    }
}
