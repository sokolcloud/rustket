//! Colored flags on rooms and threads, synced between the user's devices.
//!
//! The synced form is a [`FlagBlob`] stored as JSON under
//! [`FLAGS_KEY`](rocketchat::models::FLAGS_KEY) in the `customFields` of the
//! user's hidden service room (`rocketchat::models::service_room`). Stores
//! replace the whole object, so the blob is a set of last-writer-wins
//! entries: every room, thread and color name carries a [`Stamp`] (time and
//! device), merging keeps the larger entry per key, and removals are
//! tombstones (`null` values). Writers read, merge their pending entries,
//! write and read again until the store shows them ([`push`]).
//!
//! [`Flags`] is the effective view: the store's blob merged with the
//! device's pending entries. One flag per item; [`FlagColor`] is the
//! palette. Pure, except [`push`], which is generic over a [`FlagStore`].

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use serde_json::{Map, Value, json};

/// Longest flag name in characters.
pub const MAX_FLAG_NAME_CHARS: usize = 40;
/// Most flagged items (rooms and threads) a device adds flags up to.
pub const MAX_FLAGGED_ITEMS: usize = 1000;
/// Format written into the blob's `v`. Readers accept any version.
pub const FLAGS_FORMAT: u64 = 1;
/// Longest room or message id accepted as a key.
const MAX_ID_CHARS: usize = 128;
/// How far ahead of the local clock a stored stamp may be (a year: clocks
/// differ, but not by that much).
pub const MAX_FUTURE_MS: i64 = 365 * 24 * 3600 * 1000;

/// The flag palette, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FlagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl FlagColor {
    pub const ALL: [Self; 6] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
    ];

    /// Stable id, used in the blob and in section ids.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Position in [`Self::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// i18n key of the default name (`sidebar.flag.<id>`).
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Red => "sidebar.flag.red",
            Self::Orange => "sidebar.flag.orange",
            Self::Yellow => "sidebar.flag.yellow",
            Self::Green => "sidebar.flag.green",
            Self::Blue => "sidebar.flag.blue",
            Self::Purple => "sidebar.flag.purple",
        }
    }
}

/// A flag name as stored: control characters removed, trimmed, at most
/// [`MAX_FLAG_NAME_CHARS`]; `None` when empty (the default name).
pub fn clean_flag_name(name: &str) -> Option<String> {
    let n: String = name.chars().filter(|c| !c.is_control()).collect();
    let n: String = n.trim().chars().take(MAX_FLAG_NAME_CHARS).collect();
    let n = n.trim_end();
    (!n.is_empty()).then(|| n.to_owned())
}

/// Whether `id` can key a blob entry (Rocket.Chat ids are alphanumeric).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && !id.chars().any(|c| c == '.' || c == '$' || c.is_control())
}

/// When an entry was written, and by which device: ordered by time, then
/// device id (so every device picks the same winner).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Stamp {
    /// Unix milliseconds of the writer's clock.
    pub t: i64,
    /// The writer's device id.
    pub d: String,
}

impl Stamp {
    /// A stamp for a new entry replacing `prev`: the local time, but always
    /// after `prev` (clocks of devices differ).
    pub fn after(prev: Option<&Stamp>, now_ms: i64, device: &str) -> Self {
        let t = prev.map_or(now_ms, |p| now_ms.max(p.t.saturating_add(1)));
        Self {
            t,
            d: device.to_owned(),
        }
    }
}

/// A room's entry. `color: None` is a tombstone (no flag). Colors are kept
/// as stored, so ids a newer version added survive merges.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomMark {
    pub stamp: Stamp,
    pub color: Option<String>,
}

/// A thread's entry: the color and the thread's room (rows and parent
/// fetches need it before the parent is loaded).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadMark {
    pub stamp: Stamp,
    pub color: Option<String>,
    pub rid: Option<String>,
}

/// A color's name. `name: None` is the default name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameMark {
    pub stamp: Stamp,
    pub name: Option<String>,
}

/// A change the user made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagEdit {
    /// Flags a room (`None`: removes its flag).
    Room {
        rid: String,
        color: Option<FlagColor>,
    },
    /// Flags a thread of room `rid` (`None`: removes its flag).
    Thread {
        rid: String,
        tmid: String,
        color: Option<FlagColor>,
    },
    /// Names a color (`None` or blank: back to the default name).
    Rename {
        color: FlagColor,
        name: Option<String>,
    },
    /// Removes the color from every room and thread.
    ClearColor(FlagColor),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FlagError {
    #[error("invalid room or message id")]
    InvalidId,
    #[error("too many flagged items")]
    Full,
}

/// The synced flags: per-entry last-writer-wins maps plus top-level keys a
/// newer version added (kept on write).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlagBlob {
    pub rooms: BTreeMap<String, RoomMark>,
    pub threads: BTreeMap<String, ThreadMark>,
    /// Color id -> name.
    pub names: BTreeMap<String, NameMark>,
    v: u64,
    extra: Map<String, Value>,
}

const KNOWN_KEYS: [&str; 4] = ["v", "rooms", "threads", "names"];

fn stamp_of(o: &Map<String, Value>) -> Stamp {
    let t = o
        .get("t")
        .and_then(|t| t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)))
        .unwrap_or(0);
    let d = o
        .get("d")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Stamp { t, d }
}

fn str_of(o: &Map<String, Value>, k: &str) -> Option<String> {
    o.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Entries of a map member, leniently: a bare string is an unstamped value
/// (`t` 0, so any stamped entry wins); other non-objects are skipped.
fn entries<'a>(
    root: &'a Map<String, Value>,
    key: &str,
) -> impl Iterator<Item = (&'a String, Map<String, Value>)> + 'a {
    root.get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            let o = match v {
                Value::Object(o) => o.clone(),
                Value::String(s) => {
                    let mut o = Map::new();
                    o.insert("c".into(), Value::String(s.clone()));
                    o.insert("n".into(), Value::String(s.clone()));
                    o
                }
                Value::Null => Map::new(),
                _ => return None,
            };
            Some((k, o))
        })
}

/// Puts `m` unless the stored entry is at least as large.
fn merge_entry<M: Ord + Clone>(map: &mut BTreeMap<String, M>, k: &str, m: &M) -> bool {
    match map.get(k) {
        Some(cur) if cur >= m => false,
        _ => {
            map.insert(k.to_owned(), m.clone());
            true
        }
    }
}

fn merge_map<M: Ord + Clone>(dst: &mut BTreeMap<String, M>, src: &BTreeMap<String, M>) -> bool {
    let mut changed = false;
    for (k, m) in src {
        changed |= merge_entry(dst, k, m);
    }
    changed
}

fn covers_map<M: Ord>(have: &BTreeMap<String, M>, want: &BTreeMap<String, M>) -> bool {
    want.iter()
        .all(|(k, m)| have.get(k).is_some_and(|h| h >= m))
}

fn uncovered_map<M: Ord + Clone>(
    mine: &BTreeMap<String, M>,
    have: &BTreeMap<String, M>,
) -> BTreeMap<String, M> {
    mine.iter()
        .filter(|(k, m)| have.get(*k).is_none_or(|h| h < *m))
        .map(|(k, m)| (k.clone(), m.clone()))
        .collect()
}

impl FlagBlob {
    /// Reads a stored blob. Lenient: anything that is not an object is
    /// empty, malformed entries are skipped, ids that cannot key an entry
    /// are dropped, names are cleaned, and entries stamped more than
    /// [`MAX_FUTURE_MS`] ahead of the local clock are dropped (no later edit
    /// could ever win over them).
    pub fn from_value(v: Option<&Value>) -> Self {
        Self::from_value_at(v, chrono::Utc::now().timestamp_millis())
    }

    /// [`Self::from_value`] with the local clock at `now_ms`.
    pub fn from_value_at(v: Option<&Value>, now_ms: i64) -> Self {
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
        let usable = |key: &'static str| {
            entries(root, key).filter(move |(k, o)| valid_id(k) && stamp_of(o).t <= max_t)
        };
        for (rid, o) in usable("rooms") {
            let m = RoomMark {
                stamp: stamp_of(&o),
                color: str_of(&o, "c"),
            };
            merge_entry(&mut b.rooms, rid, &m);
        }
        for (tmid, o) in usable("threads") {
            let m = ThreadMark {
                stamp: stamp_of(&o),
                color: str_of(&o, "c"),
                rid: str_of(&o, "rid").filter(|r| valid_id(r)),
            };
            merge_entry(&mut b.threads, tmid, &m);
        }
        for (color, o) in usable("names") {
            let m = NameMark {
                stamp: stamp_of(&o),
                name: o.get("n").and_then(Value::as_str).and_then(clean_flag_name),
            };
            merge_entry(&mut b.names, color, &m);
        }
        b
    }

    /// The stored form. The format version never goes down.
    pub fn to_value(&self) -> Value {
        let stamp = |s: &Stamp, o: &mut Map<String, Value>| {
            o.insert("t".into(), json!(s.t));
            if !s.d.is_empty() {
                o.insert("d".into(), json!(s.d));
            }
        };
        let rooms: Map<String, Value> = self
            .rooms
            .iter()
            .map(|(k, m)| {
                let mut o = Map::new();
                o.insert("c".into(), json!(m.color));
                stamp(&m.stamp, &mut o);
                (k.clone(), Value::Object(o))
            })
            .collect();
        let threads: Map<String, Value> = self
            .threads
            .iter()
            .map(|(k, m)| {
                let mut o = Map::new();
                o.insert("c".into(), json!(m.color));
                if let Some(rid) = &m.rid {
                    o.insert("rid".into(), json!(rid));
                }
                stamp(&m.stamp, &mut o);
                (k.clone(), Value::Object(o))
            })
            .collect();
        let names: Map<String, Value> = self
            .names
            .iter()
            .map(|(k, m)| {
                let mut o = Map::new();
                o.insert("n".into(), json!(m.name));
                stamp(&m.stamp, &mut o);
                (k.clone(), Value::Object(o))
            })
            .collect();
        let mut root = self.extra.clone();
        root.insert("v".into(), json!(self.v.max(FLAGS_FORMAT)));
        root.insert("rooms".into(), Value::Object(rooms));
        root.insert("threads".into(), Value::Object(threads));
        root.insert("names".into(), Value::Object(names));
        Value::Object(root)
    }

    /// No entries (tombstones count as entries).
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty() && self.threads.is_empty() && self.names.is_empty()
    }

    /// Number of entries, tombstones included.
    pub fn len(&self) -> usize {
        self.rooms.len() + self.threads.len() + self.names.len()
    }

    /// Takes every entry of `other` that is larger than this blob's own (and
    /// top-level keys this blob lacks). Commutative and idempotent on the
    /// entries. Returns whether an entry changed.
    pub fn merge(&mut self, other: &FlagBlob) -> bool {
        let mut changed = merge_map(&mut self.rooms, &other.rooms);
        changed |= merge_map(&mut self.threads, &other.threads);
        changed |= merge_map(&mut self.names, &other.names);
        self.v = self.v.max(other.v);
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        changed
    }

    /// `self` merged with `other`.
    pub fn merged(&self, other: &FlagBlob) -> FlagBlob {
        let mut b = self.clone();
        b.merge(other);
        b
    }

    /// Every entry of `other` is here, at least as new.
    pub fn covers(&self, other: &FlagBlob) -> bool {
        covers_map(&self.rooms, &other.rooms)
            && covers_map(&self.threads, &other.threads)
            && covers_map(&self.names, &other.names)
    }

    /// The entries of this blob that `stored` does not hold (at least as
    /// new): what still has to be written.
    pub fn uncovered_by(&self, stored: &FlagBlob) -> FlagBlob {
        FlagBlob {
            rooms: uncovered_map(&self.rooms, &stored.rooms),
            threads: uncovered_map(&self.threads, &stored.threads),
            names: uncovered_map(&self.names, &stored.names),
            ..FlagBlob::default()
        }
    }

    /// Drops the entries `stored` holds (at least as new). Returns whether
    /// any were dropped.
    pub fn prune_covered(&mut self, stored: &FlagBlob) -> bool {
        let before = self.len();
        let left = self.uncovered_by(stored);
        self.rooms = left.rooms;
        self.threads = left.threads;
        self.names = left.names;
        self.len() != before
    }

    /// The effective flags of the blob.
    pub fn view(&self) -> Flags {
        let rooms = self
            .rooms
            .iter()
            .filter_map(|(rid, m)| {
                let c = FlagColor::parse(m.color.as_deref()?)?;
                Some((rid.clone(), c))
            })
            .collect();
        let threads = self
            .threads
            .iter()
            .filter_map(|(tmid, m)| {
                let c = FlagColor::parse(m.color.as_deref()?)?;
                Some((tmid.clone(), (m.rid.clone()?, c)))
            })
            .collect();
        let names = self
            .names
            .iter()
            .filter_map(|(c, m)| Some((FlagColor::parse(c)?, m.name.clone()?)))
            .collect();
        Flags {
            rooms,
            threads,
            names,
        }
    }

    /// Records `edit` as pending entries of this blob (the device's
    /// overlay) on top of `stored` (the store's blob), stamped after the
    /// entries they replace. Returns whether anything changed: an edit that
    /// matches the current state adds nothing.
    pub fn edit(
        &mut self,
        stored: &FlagBlob,
        edit: &FlagEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, FlagError> {
        let current = stored.merged(self);
        let view = current.view();
        let color_str = |c: &Option<FlagColor>| c.map(|c| c.as_str().to_owned());
        match edit {
            FlagEdit::Room { rid, color } => {
                if !valid_id(rid) {
                    return Err(FlagError::InvalidId);
                }
                if view.room(rid) == *color {
                    return Ok(false);
                }
                if color.is_some() && view.room(rid).is_none() && view.len() >= MAX_FLAGGED_ITEMS {
                    return Err(FlagError::Full);
                }
                let m = RoomMark {
                    stamp: Stamp::after(current.rooms.get(rid).map(|m| &m.stamp), now_ms, device),
                    color: color_str(color),
                };
                Ok(merge_entry(&mut self.rooms, rid, &m))
            }
            FlagEdit::Thread { rid, tmid, color } => {
                if !valid_id(rid) || !valid_id(tmid) {
                    return Err(FlagError::InvalidId);
                }
                if view.thread(tmid) == *color {
                    return Ok(false);
                }
                if color.is_some() && view.thread(tmid).is_none() && view.len() >= MAX_FLAGGED_ITEMS
                {
                    return Err(FlagError::Full);
                }
                let m = ThreadMark {
                    stamp: Stamp::after(
                        current.threads.get(tmid).map(|m| &m.stamp),
                        now_ms,
                        device,
                    ),
                    color: color_str(color),
                    rid: Some(rid.clone()),
                };
                Ok(merge_entry(&mut self.threads, tmid, &m))
            }
            FlagEdit::Rename { color, name } => {
                let name = name.as_deref().and_then(clean_flag_name);
                if view.name(*color) == name.as_deref() {
                    return Ok(false);
                }
                let key = color.as_str();
                let m = NameMark {
                    stamp: Stamp::after(current.names.get(key).map(|m| &m.stamp), now_ms, device),
                    name,
                };
                Ok(merge_entry(&mut self.names, key, &m))
            }
            FlagEdit::ClearColor(color) => {
                let mut changed = false;
                for (rid, _) in view.rooms().filter(|(_, c)| c == color) {
                    let m = RoomMark {
                        stamp: Stamp::after(
                            current.rooms.get(rid).map(|m| &m.stamp),
                            now_ms,
                            device,
                        ),
                        color: None,
                    };
                    changed |= merge_entry(&mut self.rooms, rid, &m);
                }
                for (rid, tmid, _) in view.threads().filter(|(_, _, c)| c == color) {
                    let m = ThreadMark {
                        stamp: Stamp::after(
                            current.threads.get(tmid).map(|m| &m.stamp),
                            now_ms,
                            device,
                        ),
                        color: None,
                        rid: Some(rid.to_owned()),
                    };
                    changed |= merge_entry(&mut self.threads, tmid, &m);
                }
                Ok(changed)
            }
        }
    }
}

/// The effective flags: room and thread colors and the colors' names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    rooms: BTreeMap<String, FlagColor>,
    /// tmid -> (rid, color).
    threads: BTreeMap<String, (String, FlagColor)>,
    names: BTreeMap<FlagColor, String>,
}

impl Flags {
    pub fn room(&self, rid: &str) -> Option<FlagColor> {
        self.rooms.get(rid).copied()
    }

    pub fn thread(&self, tmid: &str) -> Option<FlagColor> {
        self.threads.get(tmid).map(|(_, c)| *c)
    }

    /// `(rid, color)`.
    pub fn rooms(&self) -> impl Iterator<Item = (&str, FlagColor)> + '_ {
        self.rooms.iter().map(|(r, c)| (r.as_str(), *c))
    }

    /// `(rid, tmid, color)`.
    pub fn threads(&self) -> impl Iterator<Item = (&str, &str, FlagColor)> + '_ {
        self.threads
            .iter()
            .map(|(t, (r, c))| (r.as_str(), t.as_str(), *c))
    }

    /// The user's name of a color (`None`: the default name).
    pub fn name(&self, c: FlagColor) -> Option<&str> {
        self.names.get(&c).map(String::as_str)
    }

    /// Colors carried by at least one room or thread.
    pub fn used_colors(&self) -> BTreeSet<FlagColor> {
        self.rooms
            .values()
            .chain(self.threads.values().map(|(_, c)| c))
            .copied()
            .collect()
    }

    /// Flagged rooms and threads.
    pub fn len(&self) -> usize {
        self.rooms.len() + self.threads.len()
    }

    /// Nothing is flagged (names may be set).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where the blob is stored (the service room on a server, a fake in
/// tests). Writes replace the whole blob.
pub trait FlagStore {
    type Error;
    /// The stored blob (empty when nothing is stored yet).
    fn read(&mut self) -> impl Future<Output = Result<FlagBlob, Self::Error>> + Send;
    /// Replaces the stored blob.
    fn write(&mut self, blob: &FlagBlob) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum PushError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Store(#[from] E),
    /// Other writers kept replacing the blob.
    #[error("flags were changed elsewhere at the same time")]
    Conflict,
}

/// Writes `pending` into the store: read, merge, write, read again, up to
/// `attempts` times, until a read shows every pending entry (a concurrent
/// whole-object write of another device may drop them). Returns the last
/// blob read, which covers `pending`. Prune the overlay with it
/// ([`FlagBlob::prune_covered`]); a later write of another device that
/// started from an older read can still drop entries, so compare later
/// reads against what was written ([`FlagBlob::uncovered_by`]).
pub async fn push<S: FlagStore>(
    store: &mut S,
    pending: &FlagBlob,
    attempts: usize,
) -> Result<FlagBlob, PushError<S::Error>>
where
    S::Error: std::error::Error + 'static,
{
    let mut stored = store.read().await?;
    for _ in 0..attempts {
        if stored.covers(pending) {
            return Ok(stored);
        }
        let next = stored.merged(pending);
        store.write(&next).await?;
        stored = store.read().await?;
    }
    if stored.covers(pending) {
        Ok(stored)
    } else {
        Err(PushError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use FlagColor::*;

    fn room(rid: &str, c: Option<FlagColor>) -> FlagEdit {
        FlagEdit::Room {
            rid: rid.into(),
            color: c,
        }
    }

    fn thread(rid: &str, tmid: &str, c: Option<FlagColor>) -> FlagEdit {
        FlagEdit::Thread {
            rid: rid.into(),
            tmid: tmid.into(),
            color: c,
        }
    }

    fn edited(stored: &FlagBlob, edits: &[FlagEdit], t: i64, d: &str) -> FlagBlob {
        let mut p = FlagBlob::default();
        for e in edits {
            p.edit(stored, e, t, d).unwrap();
        }
        p
    }

    #[test]
    fn colors_round_trip() {
        for (i, c) in FlagColor::ALL.into_iter().enumerate() {
            assert_eq!(FlagColor::parse(c.as_str()), Some(c));
            assert_eq!(c.index(), i);
            assert_eq!(c.i18n_key(), format!("sidebar.flag.{}", c.as_str()));
        }
        assert_eq!(FlagColor::parse("pink"), None);
        assert_eq!(FlagColor::parse("Red"), None);
    }

    #[test]
    fn names_and_ids_are_cleaned() {
        assert_eq!(clean_flag_name("  Срочно\n "), Some("Срочно".into()));
        assert_eq!(clean_flag_name("a\u{7}b"), Some("ab".into()));
        assert_eq!(clean_flag_name(" \t "), None);
        let long = "я".repeat(50);
        assert_eq!(
            clean_flag_name(&long).unwrap().chars().count(),
            MAX_FLAG_NAME_CHARS
        );
        assert!(valid_id("GENERAL"));
        assert!(valid_id("abcXYZ123"));
        for bad in ["", "a.b", "$set", "a\nb"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(!valid_id(&"x".repeat(MAX_ID_CHARS + 1)));
    }

    #[test]
    fn parse_is_lenient() {
        for v in [json!(null), json!("x"), json!([1]), json!(7)] {
            assert!(FlagBlob::from_value(Some(&v)).is_empty());
        }
        assert!(FlagBlob::from_value(None).is_empty());
        let b = FlagBlob::from_value(Some(&json!({
            "v": 9,
            "future": {"x": 1},
            "rooms": {
                "R1": {"c": "red", "t": 10, "d": "A", "extra": true},
                "R2": "blue",
                "R3": {"c": "pink", "t": 5},
                "R4": {"c": null, "t": 7},
                "R5": 12,
                "a.b": {"c": "red", "t": 1},
            },
            "threads": {
                "T1": {"c": "green", "rid": "R1", "t": 3.0},
                "T2": {"c": "green", "t": 3},
                "T3": "nonsense-but-a-string",
            },
            "names": {
                "red": {"n": "  Срочно ", "t": 2},
                "blue": {"n": "", "t": 2},
                "green": {"n": 5, "t": 2},
            },
            "rooms_extra": 1,
        })));
        assert_eq!(b.rooms.len(), 4);
        assert_eq!(
            b.rooms["R1"].stamp,
            Stamp {
                t: 10,
                d: "A".into()
            }
        );
        assert_eq!(b.rooms["R2"].color.as_deref(), Some("blue"));
        assert_eq!(b.rooms["R2"].stamp.t, 0);
        assert_eq!(b.rooms["R4"].color, None);
        assert_eq!(b.threads["T1"].stamp.t, 3);
        let f = b.view();
        assert_eq!(f.room("R1"), Some(Red));
        assert_eq!(f.room("R2"), Some(Blue));
        // An unknown color is no flag, but kept for writing back.
        assert_eq!(f.room("R3"), None);
        assert_eq!(f.room("R4"), None);
        assert_eq!(f.thread("T1"), Some(Green));
        // No room: not routable.
        assert_eq!(f.thread("T2"), None);
        assert_eq!(f.thread("T3"), None);
        assert_eq!(f.name(Red), Some("Срочно"));
        assert_eq!(f.name(Blue), None);
        assert_eq!(f.name(Green), None);
        assert_eq!(f.len(), 3);
        assert_eq!(f.used_colors(), [Red, Green, Blue].into_iter().collect());

        // Unknown keys and colors survive a write; the version never drops.
        let v = b.to_value();
        assert_eq!(v["v"], json!(9));
        assert_eq!(v["future"], json!({"x": 1}));
        assert_eq!(v["rooms_extra"], json!(1));
        assert_eq!(v["rooms"]["R3"], json!({"c": "pink", "t": 5}));
        assert_eq!(v["rooms"]["R4"], json!({"c": null, "t": 7}));
        assert_eq!(v["rooms"]["R1"], json!({"c": "red", "t": 10, "d": "A"}));
        assert_eq!(
            v["threads"]["T1"],
            json!({"c": "green", "rid": "R1", "t": 3})
        );
        assert_eq!(v["names"]["red"], json!({"n": "Срочно", "t": 2}));
        assert_eq!(FlagBlob::from_value(Some(&v)), b);
        assert_eq!(FlagBlob::default().to_value()["v"], json!(FLAGS_FORMAT));
    }

    #[test]
    fn far_future_stamps_are_dropped() {
        let now = 1_800_000_000_000;
        let v = json!({
            "rooms": {
                "R1": {"c": "red", "t": i64::MAX},
                "R2": {"c": "red", "t": now + MAX_FUTURE_MS},
                "R3": {"c": "red", "t": now + MAX_FUTURE_MS + 1},
            },
            "threads": {"T1": {"c": "red", "rid": "R1", "t": 1e300}},
            "names": {"red": {"n": "x", "t": i64::MAX}},
        });
        let b = FlagBlob::from_value_at(Some(&v), now);
        assert_eq!(b.rooms.keys().collect::<Vec<_>>(), ["R2"]);
        assert!(b.threads.is_empty() && b.names.is_empty());
    }

    #[test]
    fn merge_keeps_the_newest_entry_per_key() {
        let base = FlagBlob::default();
        let a = edited(
            &base,
            &[room("R1", Some(Red)), room("R2", Some(Blue))],
            100,
            "A",
        );
        let b = edited(
            &base,
            &[room("R1", Some(Green)), room("R3", Some(Red))],
            200,
            "B",
        );
        let ab = a.merged(&b);
        let ba = b.merged(&a);
        assert_eq!(ab, ba);
        assert_eq!(ab.merged(&ab), ab);
        let f = ab.view();
        assert_eq!(f.room("R1"), Some(Green));
        assert_eq!(f.room("R2"), Some(Blue));
        assert_eq!(f.room("R3"), Some(Red));
        // Same time: the device id decides, the same on every device.
        let c = edited(&base, &[room("R9", Some(Red))], 300, "A");
        let d = edited(&base, &[room("R9", Some(Blue))], 300, "B");
        assert_eq!(c.merged(&d).view().room("R9"), Some(Blue));
        assert_eq!(d.merged(&c).view().room("R9"), Some(Blue));
        // Merging reports changes.
        let mut x = a.clone();
        assert!(!x.merge(&a));
        assert!(x.merge(&b));
    }

    #[test]
    fn tombstones_are_ordered() {
        let s1 = edited(&FlagBlob::default(), &[room("R1", Some(Red))], 100, "A");
        // Removed later on device B (from a read of s1).
        let s2 = s1.merged(&edited(&s1, &[room("R1", None)], 150, "B"));
        assert_eq!(s2.view().room("R1"), None);
        assert_eq!(s2.rooms["R1"].color, None);
        // A stale copy of the flag does not bring it back.
        let mut stale = s2.clone();
        stale.merge(&s1);
        assert_eq!(stale.view().room("R1"), None);
        // A device with a slow clock still writes after what it saw.
        let again = edited(&s2, &[room("R1", Some(Blue))], 10, "C");
        assert_eq!(again.rooms["R1"].stamp.t, 151);
        assert_eq!(s2.merged(&again).view().room("R1"), Some(Blue));
    }

    #[test]
    fn edits() {
        let stored = edited(
            &FlagBlob::default(),
            &[
                room("R1", Some(Red)),
                thread("R1", "T1", Some(Red)),
                thread("R2", "T2", Some(Blue)),
            ],
            100,
            "A",
        );
        let mut p = FlagBlob::default();
        // No-ops add nothing.
        assert_eq!(p.edit(&stored, &room("R1", Some(Red)), 200, "B"), Ok(false));
        assert_eq!(p.edit(&stored, &room("R5", None), 200, "B"), Ok(false));
        assert!(p.is_empty());
        assert_eq!(
            p.edit(&stored, &room("a.b", Some(Red)), 200, "B"),
            Err(FlagError::InvalidId)
        );
        assert_eq!(
            p.edit(&stored, &thread("R1", "", Some(Red)), 200, "B"),
            Err(FlagError::InvalidId)
        );
        // One flag per item: another color replaces it.
        assert_eq!(
            p.edit(&stored, &room("R1", Some(Green)), 200, "B"),
            Ok(true)
        );
        assert_eq!(stored.merged(&p).view().room("R1"), Some(Green));
        // Names.
        let rename = |n: &str| FlagEdit::Rename {
            color: Red,
            name: Some(n.into()),
        };
        assert_eq!(p.edit(&stored, &rename("  Срочно "), 200, "B"), Ok(true));
        assert_eq!(p.edit(&stored, &rename("Срочно"), 201, "B"), Ok(false));
        assert_eq!(stored.merged(&p).view().name(Red), Some("Срочно"));
        assert_eq!(p.edit(&stored, &rename("  "), 202, "B"), Ok(true));
        assert_eq!(stored.merged(&p).view().name(Red), None);
        assert_eq!(p.names["red"].stamp.t, 202);
        // Clearing a color tombstones its rooms and threads only.
        assert_eq!(
            p.edit(&stored, &FlagEdit::ClearColor(Red), 300, "B"),
            Ok(true)
        );
        let f = stored.merged(&p).view();
        assert_eq!(f.room("R1"), Some(Green));
        assert_eq!(f.thread("T1"), None);
        assert_eq!(f.thread("T2"), Some(Blue));
        assert_eq!(p.threads["T1"].rid.as_deref(), Some("R1"));
        assert_eq!(
            p.edit(&stored, &FlagEdit::ClearColor(Red), 301, "B"),
            Ok(false)
        );
    }

    #[test]
    fn flags_are_capped() {
        let mut stored = FlagBlob::default();
        let mut p = FlagBlob::default();
        for i in 0..MAX_FLAGGED_ITEMS {
            p.edit(&stored, &room(&format!("R{i}"), Some(Red)), 1, "A")
                .unwrap();
        }
        stored.merge(&p);
        let mut q = FlagBlob::default();
        assert_eq!(
            q.edit(&stored, &room("new", Some(Red)), 2, "A"),
            Err(FlagError::Full)
        );
        assert_eq!(
            q.edit(&stored, &thread("R0", "T", Some(Red)), 2, "A"),
            Err(FlagError::Full)
        );
        // Changing or removing a flag still works.
        assert_eq!(q.edit(&stored, &room("R0", Some(Blue)), 2, "A"), Ok(true));
        assert_eq!(q.edit(&stored, &room("R1", None), 2, "A"), Ok(true));
        let mut stored = stored.merged(&q);
        let mut r = FlagBlob::default();
        assert_eq!(r.edit(&stored, &room("new", Some(Red)), 3, "A"), Ok(true));
        stored.merge(&r);
        assert_eq!(stored.view().len(), MAX_FLAGGED_ITEMS);
    }

    #[test]
    fn overlays_leave_once_stored() {
        let stored = edited(&FlagBlob::default(), &[room("R1", Some(Red))], 100, "A");
        let mut p = edited(
            &stored,
            &[room("R1", Some(Blue)), room("R2", Some(Green))],
            200,
            "B",
        );
        // A read from before the write keeps everything pending.
        assert!(!stored.covers(&p));
        assert_eq!(p.uncovered_by(&stored), p);
        assert!(!p.prune_covered(&stored));
        // A read that holds one of them.
        let mut partial = stored.clone();
        partial.rooms.insert("R2".into(), p.rooms["R2"].clone());
        assert!(p.prune_covered(&partial));
        assert_eq!(p.rooms.keys().collect::<Vec<_>>(), ["R1"]);
        // Another device's newer entry covers the pending one too.
        let newer = edited(&stored, &[room("R1", Some(Purple))], 500, "C");
        assert!(p.prune_covered(&stored.merged(&newer)));
        assert!(p.is_empty());
    }

    /// A store that another device writes to between our read and write.
    struct Racy {
        stored: FlagBlob,
        /// Blobs other devices write right after each of our writes (read
        /// before it, so ours are lost).
        clobbers: Vec<FlagBlob>,
        writes: usize,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("store down")]
    struct Down;

    impl FlagStore for Racy {
        type Error = Down;
        async fn read(&mut self) -> Result<FlagBlob, Down> {
            Ok(self.stored.clone())
        }
        async fn write(&mut self, blob: &FlagBlob) -> Result<(), Down> {
            self.writes += 1;
            self.stored = blob.clone();
            if !self.clobbers.is_empty() {
                self.stored = self.clobbers.remove(0);
            }
            Ok(())
        }
    }

    struct Broken;

    impl FlagStore for Broken {
        type Error = Down;
        async fn read(&mut self) -> Result<FlagBlob, Down> {
            Err(Down)
        }
        async fn write(&mut self, _: &FlagBlob) -> Result<(), Down> {
            Err(Down)
        }
    }

    #[tokio::test]
    async fn push_rereads_and_retries() {
        let other = edited(&FlagBlob::default(), &[room("X", Some(Red))], 50, "O");
        let mine = edited(&FlagBlob::default(), &[room("R1", Some(Blue))], 100, "A");
        let mut s = Racy {
            stored: other.clone(),
            clobbers: vec![other.clone()],
            writes: 0,
        };
        let got = push(&mut s, &mine, 3).await.unwrap();
        assert!(got.covers(&mine) && got.covers(&other));
        assert_eq!(s.writes, 2);
        // Nothing to write when the store already holds it.
        let got = push(&mut s, &mine, 3).await.unwrap();
        assert!(got.covers(&mine));
        assert_eq!(s.writes, 2);
        // Retries run out.
        let mut s = Racy {
            stored: FlagBlob::default(),
            clobbers: vec![other.clone(); 5],
            writes: 0,
        };
        assert!(matches!(
            push(&mut s, &mine, 2).await,
            Err(PushError::Conflict)
        ));
        assert_eq!(s.writes, 2);
        assert!(matches!(
            push(&mut Broken, &mine, 2).await,
            Err(PushError::Store(Down))
        ));
    }
}
