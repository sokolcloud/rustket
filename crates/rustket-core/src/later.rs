//! "Later": the user's starred messages as a to-do list, with a status
//! (in progress, done, archived) and an optional due date, synced between
//! the user's devices.
//!
//! The list itself is the server's stars (`chat.starMessage`); Rustket adds
//! a [`LaterBlob`] stored as JSON under [`LATER_KEY`](rocketchat::models::LATER_KEY)
//! in the `customFields` of the user's hidden service room
//! ([`crate::service_sync`]): two last-writer-wins maps by message id, one
//! for the status and one for the due date, so a status change on one
//! device and a due date set on another never lose each other. Only ids,
//! status words and times are stored, never message text.
//!
//! Stars that existed before the account first used Later count as
//! archived: the blob records when Later was first seeded ([`Seed`], the
//! earliest wins) and a message older than that without a status entry is
//! archived, a newer one in progress. Per-message entries for every old
//! star would not fit the service room's budget. Starring an old message in
//! Rustket writes an explicit "in progress" entry.
//!
//! [`LaterIndex`] keeps the starred messages this device knows (paged scans
//! of each room's starred messages, live updates) plus local copies of
//! messages with a reminder, for the Later view and reminder notifications.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use rocketchat::models::Message;
use serde_json::{Map, Value, json};

use crate::service_sync::{
    EditBlob, Entry, MAX_FUTURE_MS, Stamp, SyncBlob, covers_map, merge_entry, merge_map, stamp_of,
    str_of, uncovered_map, valid_id, written,
};
use crate::thread_index::StarredScan;

/// Format written into the blob's `v`. Readers accept any version.
pub const LATER_FORMAT: u64 = 1;
/// Most live status and due entries per account.
pub const MAX_LATER_ENTRIES: usize = 500;
/// Removed entries are forgotten after this long.
pub const TOMBSTONE_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// Most starred messages the index keeps (oldest go first).
pub const MAX_LATER_MESSAGES: usize = 3000;

/// Where a starred message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum LaterStatus {
    #[default]
    InProgress,
    Done,
    Archived,
}

impl LaterStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LaterStatus::InProgress => "open",
            LaterStatus::Done => "done",
            LaterStatus::Archived => "archived",
        }
    }

    /// Unknown words (a newer version's) read as in progress.
    pub fn parse(s: &str) -> Self {
        match s {
            "done" => LaterStatus::Done,
            "archived" => LaterStatus::Archived,
            _ => LaterStatus::InProgress,
        }
    }
}

/// A status entry. `status: None` is a tombstone (back to the default).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusMark {
    pub stamp: Stamp,
    /// As stored (unknown words are kept).
    pub status: Option<String>,
    pub rid: Option<String>,
}

impl Entry for StatusMark {
    fn stamp(&self) -> &Stamp {
        &self.stamp
    }
    fn is_tombstone(&self) -> bool {
        self.status.is_none()
    }
}

/// A due date entry. `at: None` is a tombstone (no due date).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DueMark {
    pub stamp: Stamp,
    pub at: Option<i64>,
    pub rid: Option<String>,
}

impl Entry for DueMark {
    fn stamp(&self) -> &Stamp {
        &self.stamp
    }
    fn is_tombstone(&self) -> bool {
        self.at.is_none()
    }
}

/// When the account first used Later: stars of older messages without a
/// status entry are archived. The earliest seed wins (two devices seeding
/// at once agree on it).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seed {
    pub t: i64,
    pub d: String,
}

fn stamp_json(o: &mut Map<String, Value>, s: &Stamp) {
    o.insert("t".into(), json!(s.t));
    if !s.d.is_empty() {
        o.insert("d".into(), json!(s.d));
    }
}

fn int_of(o: &Map<String, Value>, k: &str) -> Option<i64> {
    o.get(k)
        .and_then(|a| a.as_i64().or_else(|| a.as_f64().map(|f| f as i64)))
}

/// A change the user made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaterEdit {
    /// Records the first use of Later (nothing when a seed is known).
    Seed { at_ms: i64 },
    /// Sets the status of a starred message (`ts_ms`: the message's time,
    /// which tells the default status).
    Status {
        rid: String,
        mid: String,
        ts_ms: Option<i64>,
        status: LaterStatus,
    },
    Due {
        rid: String,
        mid: String,
        at_ms: Option<i64>,
    },
    /// Removed from Later (unstarred): both entries go.
    Forget { mid: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaterError {
    #[error("invalid room or message id")]
    InvalidId,
    #[error("too many Later entries")]
    Full,
    #[error("the synced data is too large")]
    TooLarge,
}

/// The synced Later state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaterBlob {
    pub status: BTreeMap<String, StatusMark>,
    pub due: BTreeMap<String, DueMark>,
    pub seeded: Option<Seed>,
    v: u64,
    extra: Map<String, Value>,
}

const KNOWN_KEYS: [&str; 4] = ["v", "status", "due", "seeded"];

/// The `ts` of a message in ms.
pub fn ts_ms(m: &Message) -> Option<i64> {
    m.ts.map(|t| t.timestamp_millis())
}

impl LaterBlob {
    /// The status of message `mid` sent at `ts_ms`: its entry, else archived
    /// for a message older than the seed, else in progress.
    pub fn status_of(&self, mid: &str, ts_ms: Option<i64>) -> LaterStatus {
        match self.status.get(mid).and_then(|m| m.status.as_deref()) {
            Some(s) => LaterStatus::parse(s),
            None => self.default_status(ts_ms),
        }
    }

    /// The status of a message without an entry.
    pub fn default_status(&self, ts_ms: Option<i64>) -> LaterStatus {
        match (&self.seeded, ts_ms) {
            (Some(s), Some(ts)) if ts < s.t => LaterStatus::Archived,
            _ => LaterStatus::InProgress,
        }
    }

    pub fn due_of(&self, mid: &str) -> Option<i64> {
        self.due.get(mid).and_then(|m| m.at)
    }

    /// Whether Later was seeded on any device.
    pub fn is_seeded(&self) -> bool {
        self.seeded.is_some()
    }

    fn live(&self) -> usize {
        self.status.values().filter(|m| !m.is_tombstone()).count()
            + self.due.values().filter(|m| !m.is_tombstone()).count()
    }

    /// Records `edit` as pending entries of this blob (the device's overlay)
    /// on top of `stored`. Returns whether anything changed.
    pub fn edit(
        &mut self,
        stored: &Self,
        edit: &LaterEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, LaterError> {
        let current = stored.merged_with(self);
        match edit {
            LaterEdit::Seed { at_ms } => {
                if current.seeded.is_some() {
                    return Ok(false);
                }
                self.seeded = Some(Seed {
                    t: *at_ms,
                    d: device.to_owned(),
                });
                Ok(true)
            }
            LaterEdit::Status {
                rid,
                mid,
                ts_ms,
                status,
            } => {
                if !valid_id(rid) || !valid_id(mid) {
                    return Err(LaterError::InvalidId);
                }
                let prev = current.status.get(mid);
                let live = prev.filter(|m| !m.is_tombstone());
                let next = if *status == current.default_status(*ts_ms) {
                    // The default needs no entry.
                    if live.is_none() {
                        return Ok(false);
                    }
                    None
                } else {
                    if live.is_some_and(|m| m.status.as_deref() == Some(status.as_str())) {
                        return Ok(false);
                    }
                    if live.is_none() && current.live() >= MAX_LATER_ENTRIES {
                        return Err(LaterError::Full);
                    }
                    Some(status.as_str().to_owned())
                };
                let mark = StatusMark {
                    stamp: Stamp::after(prev.map(|m| &m.stamp), now_ms, device),
                    rid: next.as_ref().map(|_| rid.clone()),
                    status: next,
                };
                Ok(merge_entry(&mut self.status, mid, &mark))
            }
            LaterEdit::Due { rid, mid, at_ms } => {
                if !valid_id(rid) || !valid_id(mid) {
                    return Err(LaterError::InvalidId);
                }
                let prev = current.due.get(mid);
                let live = prev.filter(|m| !m.is_tombstone());
                if live.map(|m| m.at) == Some(*at_ms) || (live.is_none() && at_ms.is_none()) {
                    return Ok(false);
                }
                if live.is_none() && current.live() >= MAX_LATER_ENTRIES {
                    return Err(LaterError::Full);
                }
                let mark = DueMark {
                    stamp: Stamp::after(prev.map(|m| &m.stamp), now_ms, device),
                    at: *at_ms,
                    rid: at_ms.map(|_| rid.clone()),
                };
                Ok(merge_entry(&mut self.due, mid, &mark))
            }
            LaterEdit::Forget { mid } => {
                let mut changed = false;
                if let Some(prev) = current.status.get(mid).filter(|m| !m.is_tombstone()) {
                    let mark = StatusMark {
                        stamp: Stamp::after(Some(&prev.stamp), now_ms, device),
                        status: None,
                        rid: None,
                    };
                    changed |= merge_entry(&mut self.status, mid, &mark);
                }
                if let Some(prev) = current.due.get(mid).filter(|m| !m.is_tombstone()) {
                    let mark = DueMark {
                        stamp: Stamp::after(Some(&prev.stamp), now_ms, device),
                        at: None,
                        rid: None,
                    };
                    changed |= merge_entry(&mut self.due, mid, &mark);
                }
                Ok(changed)
            }
        }
    }
}

impl SyncBlob for LaterBlob {
    const KEY: &'static str = rocketchat::models::LATER_KEY;
    const TOMBSTONE_TTL_MS: Option<i64> = Some(TOMBSTONE_TTL_MS);

    /// Lenient: anything that is not an object is empty; ids that cannot key
    /// an entry and entries stamped more than [`MAX_FUTURE_MS`] ahead are
    /// dropped; a live entry without a valid room id is a tombstone.
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
        let empty = Map::new();
        let objects = |key: &str| {
            root.get(key)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| match v {
                    Value::Object(o) => Some((k, o)),
                    Value::Null => Some((k, &empty)),
                    _ => None,
                })
                .filter(|(k, o)| valid_id(k) && stamp_of(o).t <= max_t)
        };
        for (mid, o) in objects("status") {
            let rid = str_of(o, "r").filter(|r| valid_id(r));
            let status = str_of(o, "s").filter(|_| rid.is_some());
            let m = StatusMark {
                stamp: stamp_of(o),
                rid: status.as_ref().and(rid),
                status,
            };
            merge_entry(&mut b.status, mid, &m);
        }
        for (mid, o) in objects("due") {
            let rid = str_of(o, "r").filter(|r| valid_id(r));
            let at = int_of(o, "at").filter(|_| rid.is_some());
            let m = DueMark {
                stamp: stamp_of(o),
                rid: at.and(rid),
                at,
            };
            merge_entry(&mut b.due, mid, &m);
        }
        if let Some(o) = root.get("seeded").and_then(Value::as_object) {
            let s = stamp_of(o);
            if s.t > 0 && s.t <= max_t {
                b.seeded = Some(Seed { t: s.t, d: s.d });
            }
        }
        b
    }

    fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value {
        let ttl = Self::TOMBSTONE_TTL_MS;
        let status_copies: Vec<&BTreeMap<String, StatusMark>> =
            copies.iter().map(|c| &c.status).collect();
        let status: Map<String, Value> = written(&self.status, &status_copies, ttl, now_ms)
            .map(|(k, m)| {
                let mut o = Map::new();
                if let (Some(s), Some(r)) = (&m.status, &m.rid) {
                    o.insert("r".into(), json!(r));
                    o.insert("s".into(), json!(s));
                }
                stamp_json(&mut o, &m.stamp);
                (k.clone(), Value::Object(o))
            })
            .collect();
        let due_copies: Vec<&BTreeMap<String, DueMark>> = copies.iter().map(|c| &c.due).collect();
        let due: Map<String, Value> = written(&self.due, &due_copies, ttl, now_ms)
            .map(|(k, m)| {
                let mut o = Map::new();
                if let (Some(at), Some(r)) = (m.at, &m.rid) {
                    o.insert("r".into(), json!(r));
                    o.insert("at".into(), json!(at));
                }
                stamp_json(&mut o, &m.stamp);
                (k.clone(), Value::Object(o))
            })
            .collect();
        let mut root = self.extra.clone();
        root.insert("v".into(), json!(self.v.max(LATER_FORMAT)));
        root.insert("status".into(), Value::Object(status));
        root.insert("due".into(), Value::Object(due));
        if let Some(s) = &self.seeded {
            let mut o = Map::new();
            stamp_json(
                &mut o,
                &Stamp {
                    t: s.t,
                    d: s.d.clone(),
                },
            );
            root.insert("seeded".into(), Value::Object(o));
        }
        Value::Object(root)
    }

    fn merge(&mut self, other: &Self) -> bool {
        let mut changed = merge_map(&mut self.status, &other.status);
        changed |= merge_map(&mut self.due, &other.due);
        if let Some(s) = &other.seeded
            && self.seeded.as_ref().is_none_or(|m| s < m)
        {
            self.seeded = Some(s.clone());
            changed = true;
        }
        self.v = self.v.max(other.v);
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        changed
    }

    fn covers_at(&self, other: &Self, now_ms: i64) -> bool {
        let ttl = Self::TOMBSTONE_TTL_MS;
        covers_map(&self.status, &other.status, ttl, now_ms)
            && covers_map(&self.due, &other.due, ttl, now_ms)
            && seed_covered(self.seeded.as_ref(), other.seeded.as_ref())
    }

    fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self {
        let ttl = Self::TOMBSTONE_TTL_MS;
        Self {
            status: uncovered_map(&self.status, &stored.status, ttl, now_ms),
            due: uncovered_map(&self.due, &stored.due, ttl, now_ms),
            seeded: self
                .seeded
                .clone()
                .filter(|s| !seed_covered(stored.seeded.as_ref(), Some(s))),
            ..Self::default()
        }
    }

    fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool {
        let before = self.len();
        let left = self.uncovered_by_at(stored, now_ms);
        self.status = left.status;
        self.due = left.due;
        self.seeded = left.seeded;
        self.len() != before
    }

    fn is_empty(&self) -> bool {
        self.status.is_empty() && self.due.is_empty() && self.seeded.is_none()
    }

    fn len(&self) -> usize {
        self.status.len() + self.due.len() + usize::from(self.seeded.is_some())
    }
}

/// `have` holds a seed at least as early as `want`'s.
fn seed_covered(have: Option<&Seed>, want: Option<&Seed>) -> bool {
    match (have, want) {
        (_, None) => true,
        (Some(h), Some(w)) => h <= w,
        (None, Some(_)) => false,
    }
}

impl EditBlob for LaterBlob {
    type Edit = LaterEdit;
    type Error = LaterError;

    fn apply_edit(
        &mut self,
        stored: &Self,
        edit: &LaterEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, LaterError> {
        self.edit(stored, edit, now_ms, device)
    }

    fn too_large() -> LaterError {
        LaterError::TooLarge
    }
}

/// A recent star change seen on this device (own action or live update).
#[derive(Debug, Clone, Copy)]
struct StarChange {
    on: bool,
    at: Instant,
}

/// Messages the Later view lists: every message the user starred that this
/// device knows, and copies of messages with a reminder.
#[derive(Debug, Clone, Default)]
pub struct LaterIndex {
    /// Starred by the user: mid -> message.
    starred: HashMap<String, Message>,
    /// Not starred, kept for a reminder: mid -> message.
    copies: HashMap<String, Message>,
    /// rid -> last complete starred-messages scan.
    scans: HashMap<String, StarredScan>,
    /// Scans in progress: rid -> (requested at, starred ids so far).
    pending: HashMap<String, (Instant, BTreeSet<String>)>,
    changes: HashMap<String, StarChange>,
}

/// `new` is an older copy than `old`.
fn older(new: &Message, old: &Message) -> bool {
    matches!((new.updated_at, old.updated_at), (Some(n), Some(o)) if n < o)
}

/// `m` starred (`on`) or not by `me`.
fn with_star(m: &Message, me: &str, on: bool) -> Message {
    let mut c = m.clone();
    let mut list = c.starred.take().unwrap_or_default();
    list.retain(|r| r.id != me);
    if on {
        list.push(rocketchat::models::IdRef { id: me.to_owned() });
    }
    c.starred = Some(list);
    c
}

fn gone(m: &Message) -> bool {
    m.hidden == Some(true) || m.t.as_deref() == Some("rm")
}

impl LaterIndex {
    /// A starred message or a reminder copy.
    pub fn get(&self, mid: &str) -> Option<&Message> {
        self.starred.get(mid).or_else(|| self.copies.get(mid))
    }

    pub fn is_starred(&self, mid: &str) -> bool {
        self.starred.contains_key(mid)
    }

    /// The starred messages.
    pub fn starred(&self) -> impl Iterator<Item = &Message> {
        self.starred.values()
    }

    /// Whether the index dropped starred messages for its cap
    /// ([`MAX_LATER_MESSAGES`]): a message missing here may still be starred.
    pub fn is_full(&self) -> bool {
        self.starred.len() >= MAX_LATER_MESSAGES
    }

    /// Reminder copies (not starred).
    pub fn copies(&self) -> impl Iterator<Item = &Message> {
        self.copies.values()
    }

    pub fn scan(&self, rid: &str) -> Option<&StarredScan> {
        self.scans.get(rid)
    }

    pub fn scans(&self) -> impl Iterator<Item = (&str, &StarredScan)> + '_ {
        self.scans.iter().map(|(r, s)| (r.as_str(), s))
    }

    /// Rooms with a message here.
    pub fn rooms(&self) -> impl Iterator<Item = &String> {
        self.starred
            .values()
            .chain(self.copies.values())
            .map(|m| &m.rid)
    }

    fn note(&mut self, mid: &str, on: bool, now: Instant) {
        if self.changes.len() > 512 {
            self.changes
                .retain(|_, c| now.duration_since(c.at).as_secs() < 3600);
        }
        self.changes
            .insert(mid.to_owned(), StarChange { on, at: now });
    }

    fn changed_since(&self, mid: &str, since: Instant) -> Option<bool> {
        self.changes
            .get(mid)
            .filter(|c| c.at >= since)
            .map(|c| c.on)
    }

    fn put_starred(&mut self, m: Message) -> bool {
        if let Some(old) = self.starred.get(&m.id)
            && (older(&m, old) || *old == m)
        {
            return false;
        }
        self.copies.remove(&m.id);
        self.starred.insert(m.id.clone(), m);
        self.evict();
        true
    }

    fn evict(&mut self) {
        if self.starred.len() <= MAX_LATER_MESSAGES {
            return;
        }
        let mut by_ts: Vec<(Option<i64>, String)> = self
            .starred
            .values()
            .map(|m| (ts_ms(m), m.id.clone()))
            .collect();
        by_ts.sort();
        let excess = self.starred.len() - MAX_LATER_MESSAGES;
        for (_, mid) in by_ts.into_iter().take(excess) {
            self.starred.remove(&mid);
        }
    }

    /// A message seen live (or fetched): starred by `me` it is listed,
    /// otherwise it leaves; a hidden or deleted one leaves either way. A
    /// reminder copy is updated.
    pub fn observe(&mut self, m: &Message, me: &str, now: Instant) -> bool {
        if gone(m) {
            return self.remove(&m.id);
        }
        let on = m.is_starred_by(me);
        let was = self.starred.contains_key(&m.id);
        if was != on {
            self.note(&m.id, on, now);
        }
        if on {
            return self.put_starred(m.clone());
        }
        let mut changed = false;
        if self.starred.remove(&m.id).is_some() {
            self.copies.insert(m.id.clone(), m.clone());
            changed = true;
        }
        if let Some(c) = self.copies.get_mut(&m.id)
            && !older(m, c)
            && c != m
        {
            *c = m.clone();
            changed = true;
        }
        changed
    }

    /// The user (`me`) starred or unstarred `m` here (applied before the
    /// echo). The kept copy carries the new star state, as the echo will
    /// (the offline cache tells starred messages from reminder copies by it).
    pub fn set_starred(&mut self, m: &Message, me: &str, on: bool, now: Instant) -> bool {
        self.note(&m.id, on, now);
        if on {
            self.copies.remove(&m.id);
            let changed = self
                .starred
                .get(&m.id)
                .is_none_or(|old| !old.is_starred_by(me));
            let base = self.starred.get(&m.id).unwrap_or(m);
            let copy = with_star(base, me, true);
            self.starred.insert(m.id.clone(), copy);
            self.evict();
            changed
        } else {
            match self.starred.remove(&m.id) {
                Some(old) => {
                    self.copies
                        .insert(old.id.clone(), with_star(&old, me, false));
                    true
                }
                None => false,
            }
        }
    }

    /// Keeps a copy of `m` (a reminder was set on it).
    pub fn keep_copy(&mut self, m: &Message) -> bool {
        if self.starred.contains_key(&m.id) || gone(m) {
            return false;
        }
        match self.copies.get(&m.id) {
            Some(c) if c == m || older(m, c) => false,
            _ => {
                self.copies.insert(m.id.clone(), m.clone());
                true
            }
        }
    }

    /// Drops copies `keep` rejects (their reminder is gone).
    pub fn retain_copies(&mut self, keep: impl Fn(&str) -> bool) -> bool {
        let before = self.copies.len();
        self.copies.retain(|mid, _| keep(mid));
        self.copies.len() != before
    }

    /// A page of room `rid`'s starred messages, requested at `started`.
    pub fn apply_scan_page(
        &mut self,
        rid: &str,
        msgs: &[Message],
        me: &str,
        started: Instant,
    ) -> bool {
        let entry = self
            .pending
            .entry(rid.to_owned())
            .or_insert_with(|| (started, BTreeSet::new()));
        if entry.0 != started {
            *entry = (started, BTreeSet::new());
        }
        let mut found = Vec::new();
        for m in msgs {
            if m.rid != rid || gone(m) || !m.is_starred_by(me) {
                continue;
            }
            found.push(m.clone());
        }
        let mut changed = false;
        for m in found {
            if let Some((_, set)) = self.pending.get_mut(rid) {
                set.insert(m.id.clone());
            }
            // A star change seen since the scan started wins over its copy.
            if self.changed_since(&m.id, started) == Some(false) {
                continue;
            }
            changed |= self.put_starred(m);
        }
        changed
    }

    /// The scan of `rid` requested at `started` is complete: the room's
    /// starred messages are the ones it found, except for star changes seen
    /// since it started.
    pub fn finish_scan(&mut self, rid: &str, started: Instant, scan: StarredScan) -> bool {
        let found = match self.pending.remove(rid) {
            Some((at, set)) if at == started => set,
            _ => BTreeSet::new(),
        };
        let drop: Vec<String> = self
            .starred
            .values()
            .filter(|m| m.rid == rid && !found.contains(&m.id))
            .filter(|m| self.changed_since(&m.id, started) != Some(true))
            .map(|m| m.id.clone())
            .collect();
        let mut changed = !drop.is_empty();
        for mid in drop {
            if let Some(m) = self.starred.remove(&mid) {
                self.copies.insert(mid, m);
            }
        }
        changed |= self.scans.insert(rid.to_owned(), scan) != Some(scan);
        changed
    }

    /// Forgets a room's scan (scanned again).
    pub fn forget_scan(&mut self, rid: &str) -> bool {
        self.scans.remove(rid).is_some()
    }

    /// A deleted (or hidden) message.
    pub fn remove(&mut self, mid: &str) -> bool {
        let a = self.starred.remove(mid).is_some();
        let b = self.copies.remove(mid).is_some();
        a || b
    }

    /// The user left room `rid`.
    pub fn forget_room(&mut self, rid: &str) -> bool {
        let before = self.starred.len() + self.copies.len() + self.scans.len();
        self.starred.retain(|_, m| m.rid != rid);
        self.copies.retain(|_, m| m.rid != rid);
        self.scans.remove(rid);
        self.pending.remove(rid);
        before != self.starred.len() + self.copies.len() + self.scans.len()
    }

    /// Rewrites the messages of room `rid` (E2E decryption, renames).
    pub fn rewrite(&mut self, rid: &str, f: &mut dyn FnMut(&mut Message) -> bool) -> bool {
        let mut changed = false;
        for m in self
            .starred
            .values_mut()
            .chain(self.copies.values_mut())
            .filter(|m| m.rid == rid)
        {
            changed |= f(m);
        }
        changed
    }

    /// Messages and scans from the offline cache (`me`: whose stars). Data
    /// already here wins.
    pub fn restore(
        &mut self,
        msgs: Vec<Message>,
        scans: Vec<(String, StarredScan)>,
        me: Option<&str>,
    ) -> bool {
        let mut changed = false;
        for m in msgs {
            if self.get(&m.id).is_some() || gone(&m) {
                continue;
            }
            if me.is_some_and(|me| m.is_starred_by(me)) {
                self.starred.insert(m.id.clone(), m);
            } else {
                self.copies.insert(m.id.clone(), m);
            }
            changed = true;
        }
        for (rid, s) in scans {
            if let std::collections::hash_map::Entry::Vacant(e) = self.scans.entry(rid) {
                e.insert(s);
                changed = true;
            }
        }
        self.evict();
        changed
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::RcDate;
    use rocketchat::models::UserRef;

    fn status(mid: &str, ts: Option<i64>, s: LaterStatus) -> LaterEdit {
        LaterEdit::Status {
            rid: "R1".into(),
            mid: mid.into(),
            ts_ms: ts,
            status: s,
        }
    }

    #[test]
    fn parse_is_lenient_and_round_trips() {
        let v = json!({
            "v": 2,
            "future": 1,
            "seeded": {"t": 500, "d": "A"},
            "status": {
                "m1": {"r": "R1", "s": "done", "t": 10, "d": "A"},
                "m2": {"r": "R1", "s": "someday", "t": 10, "d": "A"},
                "m3": {"s": "done", "t": 10},
                "a.b": {"r": "R1", "s": "done", "t": 10},
                "m4": null,
            },
            "due": {
                "m1": {"r": "R1", "at": 9000, "t": 11, "d": "B"},
                "m5": {"r": "R1", "at": "x", "t": 11},
            }
        });
        let b = LaterBlob::from_value_at(Some(&v), 0);
        assert_eq!(b.status_of("m1", None), LaterStatus::Done);
        // Unknown words are kept and read as in progress.
        assert_eq!(b.status_of("m2", None), LaterStatus::InProgress);
        assert_eq!(b.status["m2"].status.as_deref(), Some("someday"));
        assert!(b.status["m3"].is_tombstone() && b.status["m4"].is_tombstone());
        assert!(!b.status.contains_key("a.b"));
        assert_eq!(b.due_of("m1"), Some(9000));
        assert!(b.due["m5"].is_tombstone());
        assert_eq!(
            b.seeded,
            Some(Seed {
                t: 500,
                d: "A".into()
            })
        );
        let out = b.to_value_at(100);
        assert_eq!(out["v"], json!(2));
        assert_eq!(out["future"], json!(1));
        assert_eq!(out["status"]["m2"]["s"], json!("someday"));
        assert_eq!(out["seeded"], json!({"t": 500, "d": "A"}));
        assert_eq!(LaterBlob::from_value_at(Some(&out), 0), b);
    }

    #[test]
    fn seed_makes_older_stars_archived_and_the_earliest_wins() {
        let stored = LaterBlob::default();
        let mut o = LaterBlob::default();
        assert_eq!(o.status_of("m", Some(100)), LaterStatus::InProgress);
        assert_eq!(
            o.edit(&stored, &LaterEdit::Seed { at_ms: 1000 }, 1000, "A"),
            Ok(true)
        );
        assert_eq!(
            o.edit(&stored, &LaterEdit::Seed { at_ms: 2000 }, 2000, "A"),
            Ok(false)
        );
        assert_eq!(o.status_of("old", Some(999)), LaterStatus::Archived);
        assert_eq!(o.status_of("new", Some(1000)), LaterStatus::InProgress);
        assert_eq!(o.status_of("unknown", None), LaterStatus::InProgress);
        // Another device seeded earlier: its seed wins and covers ours.
        let mut theirs = LaterBlob::default();
        theirs
            .edit(
                &LaterBlob::default(),
                &LaterEdit::Seed { at_ms: 500 },
                500,
                "B",
            )
            .unwrap();
        let merged = o.merged_with(&theirs);
        assert_eq!(merged.seeded.as_ref().unwrap().t, 500);
        assert_eq!(theirs.merged_with(&o), merged);
        assert!(theirs.covers_at(&o, 0));
        assert!(!o.covers_at(&theirs, 0));
        let mut overlay = o.clone();
        assert!(overlay.prune_covered_at(&theirs, 0));
        assert!(overlay.is_empty());
    }

    #[test]
    fn status_entries_only_for_non_defaults() {
        let mut stored = LaterBlob::default();
        stored
            .edit(
                &LaterBlob::default(),
                &LaterEdit::Seed { at_ms: 1000 },
                1000,
                "A",
            )
            .unwrap();
        let mut o = LaterBlob::default();
        // A new message in progress: nothing to store.
        assert_eq!(
            o.edit(
                &stored,
                &status("n", Some(2000), LaterStatus::InProgress),
                3000,
                "A"
            ),
            Ok(false)
        );
        // An old message archived by default: nothing either.
        assert_eq!(
            o.edit(
                &stored,
                &status("o", Some(10), LaterStatus::Archived),
                3000,
                "A"
            ),
            Ok(false)
        );
        // An old message put in progress: an explicit entry.
        assert_eq!(
            o.edit(
                &stored,
                &status("o", Some(10), LaterStatus::InProgress),
                3000,
                "A"
            ),
            Ok(true)
        );
        assert_eq!(
            o.merged_with(&stored).status_of("o", Some(10)),
            LaterStatus::InProgress
        );
        assert_eq!(o.status["o"].status.as_deref(), Some("open"));
        // Back to its default: a tombstone.
        assert_eq!(
            o.edit(
                &stored,
                &status("o", Some(10), LaterStatus::Archived),
                3001,
                "A"
            ),
            Ok(true)
        );
        assert!(o.status["o"].is_tombstone());
        assert_eq!(
            o.merged_with(&stored).status_of("o", Some(10)),
            LaterStatus::Archived
        );
        assert_eq!(
            o.edit(
                &stored,
                &status("n", Some(2000), LaterStatus::Done),
                3002,
                "A"
            ),
            Ok(true)
        );
        assert_eq!(
            o.edit(
                &stored,
                &status("n", Some(2000), LaterStatus::Done),
                3003,
                "A"
            ),
            Ok(false)
        );
        let bad = LaterEdit::Status {
            rid: "$r".into(),
            mid: "n".into(),
            ts_ms: None,
            status: LaterStatus::Done,
        };
        assert_eq!(o.edit(&stored, &bad, 3004, "A"), Err(LaterError::InvalidId));
    }

    #[test]
    fn status_and_due_merge_independently_and_forget_clears_both() {
        let base = LaterBlob::default();
        let mut a = LaterBlob::default();
        a.edit(&base, &status("m", None, LaterStatus::Done), 100, "A")
            .unwrap();
        let mut b = LaterBlob::default();
        let due = LaterEdit::Due {
            rid: "R1".into(),
            mid: "m".into(),
            at_ms: Some(9000),
        };
        b.edit(&base, &due, 100, "B").unwrap();
        let m = a.merged_with(&b);
        assert_eq!(m.status_of("m", None), LaterStatus::Done);
        assert_eq!(m.due_of("m"), Some(9000));
        let mut o = LaterBlob::default();
        assert_eq!(
            o.edit(&m, &LaterEdit::Forget { mid: "m".into() }, 200, "A"),
            Ok(true)
        );
        let after = m.merged_with(&o);
        assert_eq!(after.status_of("m", None), LaterStatus::InProgress);
        assert_eq!(after.due_of("m"), None);
        assert_eq!(
            o.edit(&m, &LaterEdit::Forget { mid: "m".into() }, 201, "A"),
            Ok(false)
        );
        assert_eq!(
            o.edit(&m, &LaterEdit::Forget { mid: "x".into() }, 201, "A"),
            Ok(false)
        );
    }

    #[test]
    fn cap_counts_live_entries() {
        let stored = LaterBlob::default();
        let mut o = LaterBlob::default();
        for n in 0..MAX_LATER_ENTRIES {
            o.edit(
                &stored,
                &status(&format!("m{n}"), None, LaterStatus::Done),
                10,
                "A",
            )
            .unwrap();
        }
        assert_eq!(
            o.edit(&stored, &status("x", None, LaterStatus::Done), 10, "A"),
            Err(LaterError::Full)
        );
        // Changing an existing entry is fine.
        assert_eq!(
            o.edit(&stored, &status("m0", None, LaterStatus::Archived), 11, "A"),
            Ok(true)
        );
        o.edit(&stored, &LaterEdit::Forget { mid: "m1".into() }, 12, "A")
            .unwrap();
        assert_eq!(
            o.edit(&stored, &status("x", None, LaterStatus::Done), 13, "A"),
            Ok(true)
        );
    }

    fn msg(id: &str, rid: &str, starred: bool, ts: i64) -> Message {
        Message {
            id: id.into(),
            rid: rid.into(),
            msg: id.into(),
            ts: RcDate::from_millis(ts),
            u: UserRef {
                id: "u".into(),
                username: Some("u".into()),
                name: None,
            },
            starred: starred.then(|| vec![rocketchat::models::IdRef { id: "me".into() }]),
            ..Default::default()
        }
    }

    #[test]
    fn index_follows_scans_and_live_updates() {
        let mut idx = LaterIndex::default();
        let t0 = Instant::now();
        let page = [
            msg("a", "r", true, 1),
            msg("b", "r", false, 2),
            msg("c", "other", true, 3),
        ];
        assert!(idx.apply_scan_page("r", &page, "me", t0));
        assert!(idx.is_starred("a") && !idx.is_starred("b") && !idx.is_starred("c"));
        // A star made after the scan started survives its finish.
        let t1 = t0 + std::time::Duration::from_millis(5);
        idx.set_starred(&msg("d", "r", false, 4), "me", true, t1);
        assert!(idx.finish_scan("r", t0, StarredScan::default()));
        assert!(idx.is_starred("a") && idx.is_starred("d"));
        assert!(idx.scan("r").is_some());
        // The next scan no longer finds `a`: it leaves (its copy stays for
        // a reminder until pruned).
        let t2 = t1 + std::time::Duration::from_millis(5);
        idx.apply_scan_page("r", &[msg("d", "r", true, 4)], "me", t2);
        idx.finish_scan("r", t2, StarredScan::default());
        assert!(!idx.is_starred("a") && idx.get("a").is_some());
        assert!(idx.retain_copies(|_| false));
        assert!(idx.get("a").is_none());
        // Live: unstarred and hidden copies leave.
        assert!(idx.observe(&msg("d", "r", false, 4), "me", t2));
        assert!(!idx.is_starred("d"));
        assert!(idx.observe(&msg("e", "r", true, 5), "me", t2));
        let mut hidden = msg("e", "r", true, 5);
        hidden.hidden = Some(true);
        assert!(idx.observe(&hidden, "me", t2));
        assert!(idx.get("e").is_none());
        idx.observe(&msg("f", "r", true, 6), "me", t2);
        assert!(idx.forget_room("r"));
        assert!(idx.get("f").is_none() && idx.scan("r").is_none());
    }

    /// The blob of a service subscription captured from the dev server.
    #[test]
    fn reads_the_captured_service_fields() {
        let v = crate::test_util::fixture("subscriptions_get_one_service_later.json");
        let later = &v["subscription"]["customFields"][rocketchat::models::LATER_KEY];
        let b = LaterBlob::from_value_at(Some(later), 1_794_738_100_000);
        assert_eq!(b.status_of("Ab3cD4eF5gH6iJ7kL", None), LaterStatus::Done);
        assert_eq!(b.due_of("Ab3cD4eF5gH6iJ7kL"), Some(1_794_800_000_000));
        assert_eq!(
            b.status_of("Mn8oP9qR0sT1uV2wX", Some(0)),
            LaterStatus::InProgress
        );
        assert!(b.status["Yz3aB4cD5eF6gH7iJ"].is_tombstone());
        // Older than the seed without an entry: archived.
        assert_eq!(
            b.status_of("other", Some(1_794_600_000_000)),
            LaterStatus::Archived
        );
        let back =
            LaterBlob::from_value_at(Some(&b.to_value_at(1_794_738_100_000)), 1_794_738_100_000);
        assert_eq!(back, b);
    }

    /// Own star toggles keep the copy's `starred` in step, so a restart
    /// (the offline cache) puts it in the right list.
    #[test]
    fn own_star_toggles_survive_a_restore() {
        let mut idx = LaterIndex::default();
        let t = Instant::now();
        assert!(idx.set_starred(&msg("a", "r", false, 1), "me", true, t));
        assert!(!idx.set_starred(&msg("a", "r", false, 1), "me", true, t));
        let a = idx.get("a").unwrap().clone();
        assert!(a.is_starred_by("me"));
        // Unstarred and starred again (a failed unstar put back).
        assert!(idx.set_starred(&msg("b", "r", true, 2), "me", true, t));
        assert!(idx.set_starred(&msg("b", "r", true, 2), "me", false, t));
        assert!(!idx.get("b").unwrap().is_starred_by("me"));
        let unstarred = idx.get("b").unwrap().clone();
        assert!(idx.set_starred(&unstarred, "me", true, t));
        assert!(idx.set_starred(&msg("c", "r", true, 3), "me", true, t));
        assert!(idx.set_starred(&msg("c", "r", true, 3), "me", false, t));
        let c = idx.get("c").unwrap().clone();
        assert!(!c.is_starred_by("me"));
        let b = idx.get("b").unwrap().clone();
        let mut back = LaterIndex::default();
        back.restore(vec![a, b, c], Vec::new(), Some("me"));
        assert!(back.is_starred("a") && back.is_starred("b"));
        assert!(!back.is_starred("c") && back.get("c").is_some());
    }

    #[test]
    fn index_restores_and_rewrites() {
        let mut idx = LaterIndex::default();
        assert!(idx.restore(
            vec![msg("a", "r", true, 1), msg("b", "r", false, 2)],
            vec![("r".into(), StarredScan::default())],
            Some("me"),
        ));
        assert!(idx.is_starred("a") && !idx.is_starred("b") && idx.get("b").is_some());
        let mut n = 0;
        assert!(idx.rewrite("r", &mut |m| {
            n += 1;
            m.msg = "x".into();
            true
        }));
        assert_eq!(n, 2);
        assert_eq!(idx.rooms().filter(|r| *r == "r").count(), 2);
        assert!(idx.keep_copy(&msg("c", "r", false, 3)));
        assert!(!idx.keep_copy(&msg("a", "r", true, 1)));
    }
}
