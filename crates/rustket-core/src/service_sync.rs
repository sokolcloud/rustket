//! Shared machinery of the blobs Rustket syncs through the user's hidden
//! service room (`rocketchat::models::service_room`).
//!
//! Each synced feature stores one blob under its own `customFields` key
//! ([`SyncBlob::KEY`]; flags are [`crate::flags::FlagBlob`]). The server
//! replaces the whole `customFields` object on every write, so every blob is
//! a set of last-writer-wins entries: each entry carries a [`Stamp`] (time,
//! then device), merging keeps the larger entry per key and removals are
//! tombstones. A blob may let its tombstones expire
//! ([`SyncBlob::TOMBSTONE_TTL_MS`]): writes drop them, and covering checks
//! treat them as stored, so pushes still finish.
//!
//! One client writes every key of a server in one read-merge-write loop
//! ([`push_all`] over a [`FieldsStore`]): the keys never overwrite each other
//! inside the client and a burst of edits on several keys costs one write.
//! All keys together stay within [`BUDGET_BYTES`] (checked at edit time,
//! [`fits_budget`]): the server copies the fields into the subscription on
//! every change and streams them to every device.

use std::collections::BTreeMap;
use std::future::Future;

use rocketchat::models::ServiceFields;
use serde_json::{Map, Value};

/// How far ahead of the local clock a stored stamp may be (a year: clocks
/// differ, but not by that much).
pub const MAX_FUTURE_MS: i64 = 365 * 24 * 3600 * 1000;
/// Longest room or message id accepted as a key.
pub const MAX_ID_CHARS: usize = 128;
/// Serialized size all synced keys of an account together may grow to.
pub const BUDGET_BYTES: usize = 64 * 1024;

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

/// An entry of a last-writer-wins map. `Ord` must order by the stamp first.
pub trait Entry: Ord + Clone {
    fn stamp(&self) -> &Stamp;
    /// A removal.
    fn is_tombstone(&self) -> bool;
}

/// A tombstone older than `ttl` (`None`: tombstones never expire).
pub fn expired<M: Entry>(m: &M, ttl: Option<i64>, now_ms: i64) -> bool {
    m.is_tombstone() && ttl.is_some_and(|ttl| m.stamp().t < now_ms.saturating_sub(ttl))
}

/// Whether `have` (one key of the other side) leaves nothing of `m` to store:
/// it is at least as new, or `m` is an expired tombstone that a write would
/// drop anyway and `have` holds nothing live for the key.
fn covered<M: Entry>(have: Option<&M>, m: &M, ttl: Option<i64>, now_ms: i64) -> bool {
    match have {
        Some(h) if h >= m => true,
        h => expired(m, ttl, now_ms) && h.is_none_or(Entry::is_tombstone),
    }
}

/// Every entry of `want` is in `have`, at least as new (see [`expired`] for
/// tombstones past `ttl`).
pub fn covers_map<M: Entry>(
    have: &BTreeMap<String, M>,
    want: &BTreeMap<String, M>,
    ttl: Option<i64>,
    now_ms: i64,
) -> bool {
    want.iter()
        .all(|(k, m)| covered(have.get(k), m, ttl, now_ms))
}

/// The entries of `mine` that `have` does not cover ([`covers_map`]).
pub fn uncovered_map<M: Entry>(
    mine: &BTreeMap<String, M>,
    have: &BTreeMap<String, M>,
    ttl: Option<i64>,
    now_ms: i64,
) -> BTreeMap<String, M> {
    mine.iter()
        .filter(|(k, m)| !covered(have.get(*k), *m, ttl, now_ms))
        .map(|(k, m)| (k.clone(), m.clone()))
        .collect()
}

/// The entries a write over `copies` (the same map in every stored copy)
/// keeps: all but the expired tombstones. An expired tombstone stays while
/// a copy holds a live entry for its key: only the written room loses that
/// entry, and one left in another service room would come back.
pub fn written<'a, M: Entry>(
    map: &'a BTreeMap<String, M>,
    copies: &'a [&'a BTreeMap<String, M>],
    ttl: Option<i64>,
    now_ms: i64,
) -> impl Iterator<Item = (&'a String, &'a M)> + 'a {
    map.iter().filter(move |(k, m)| {
        !expired(*m, ttl, now_ms)
            || copies
                .iter()
                .any(|c| c.get(*k).is_some_and(|h| !h.is_tombstone()))
    })
}

/// Puts `m` unless the stored entry is at least as large.
pub fn merge_entry<M: Ord + Clone>(map: &mut BTreeMap<String, M>, k: &str, m: &M) -> bool {
    match map.get(k) {
        Some(cur) if cur >= m => false,
        _ => {
            map.insert(k.to_owned(), m.clone());
            true
        }
    }
}

pub fn merge_map<M: Ord + Clone>(dst: &mut BTreeMap<String, M>, src: &BTreeMap<String, M>) -> bool {
    let mut changed = false;
    for (k, m) in src {
        changed |= merge_entry(dst, k, m);
    }
    changed
}

/// The `t`/`d` stamp of a stored entry (missing parts: `0` / empty).
pub fn stamp_of(o: &Map<String, Value>) -> Stamp {
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

/// A non-empty string member.
pub fn str_of(o: &Map<String, Value>, k: &str) -> Option<String> {
    o.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Entries of a map member, leniently: a bare string is an unstamped value
/// (`t` 0, so any stamped entry wins) under both `c` and `n`; `null` is an
/// empty entry; other non-objects are skipped.
pub fn entries<'a>(
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

/// A blob synced under one `customFields` key of the service room.
pub trait SyncBlob: Clone + Default + PartialEq + std::fmt::Debug + Send + Sync + 'static {
    /// The `customFields` key.
    const KEY: &'static str;
    /// Tombstones older than this are dropped when writing (`None`: kept).
    const TOMBSTONE_TTL_MS: Option<i64> = None;

    /// Reads a stored value leniently (anything malformed is skipped).
    fn from_value_at(v: Option<&Value>, now_ms: i64) -> Self;
    /// The stored form written over `copies` (this key's blob in every
    /// stored copy of the fields): without expired tombstones, except those
    /// still hiding a live entry of a copy ([`written`]).
    fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value;
    /// Takes every entry of `other` that is larger than this blob's own.
    /// Returns whether an entry changed.
    fn merge(&mut self, other: &Self) -> bool;
    /// Every entry of `other` is here, at least as new.
    fn covers_at(&self, other: &Self, now_ms: i64) -> bool;
    /// The entries of this blob that `stored` does not cover.
    fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self;
    /// Drops the entries `stored` covers. Returns whether any were dropped.
    fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool;
    /// No entries (tombstones count as entries).
    fn is_empty(&self) -> bool;
    /// Number of entries, tombstones included.
    fn len(&self) -> usize;

    /// `self` merged with `other`.
    fn merged_with(&self, other: &Self) -> Self {
        let mut b = self.clone();
        SyncBlob::merge(&mut b, other);
        b
    }

    /// The stored form, without expired tombstones.
    fn to_value_at(&self, now_ms: i64) -> Value {
        self.to_value_over_at(&[], now_ms)
    }

    /// What this blob charges against [`BUDGET_BYTES`]: its stored form. A
    /// blob whose tombstones never expire leaves them out (removing an entry
    /// could never free its space).
    fn budget_bytes(&self, now_ms: i64) -> usize {
        key_bytes(Self::KEY, &self.to_value_at(now_ms))
    }
}

/// A [`SyncBlob`] the user edits.
pub trait EditBlob: SyncBlob {
    type Edit: std::fmt::Debug;
    type Error: std::error::Error;

    /// Records `edit` as pending entries of this blob (the device's overlay)
    /// on top of `stored`. Returns whether anything changed.
    fn apply_edit(
        &mut self,
        stored: &Self,
        edit: &Self::Edit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, Self::Error>;

    /// The refusal of an edit that would grow the synced keys past
    /// [`BUDGET_BYTES`].
    fn too_large() -> Self::Error;
}

/// What one key adds to the serialized `customFields` object.
pub fn key_bytes(key: &str, value: &Value) -> usize {
    // `"key":value,`
    key.len() + 4 + serde_json::to_vec(value).map_or(0, |v| v.len())
}

/// Whether an edit that takes the synced keys from `before` to `after`
/// bytes is allowed: within the budget, or not growing (so a user over it,
/// e.g. after a newer version wrote more, can still remove things).
pub fn fits_budget(before: usize, after: usize) -> bool {
    after <= BUDGET_BYTES || after <= before
}

/// The blob of key `B` across every copy of the service fields (several
/// service rooms: their blobs merged).
pub fn stored<B: SyncBlob>(copies: &[ServiceFields], now_ms: i64) -> B {
    let mut blob = B::default();
    for f in copies {
        blob.merge(&B::from_value_at(f.get(B::KEY), now_ms));
    }
    blob
}

/// One key's pending entries in a combined push ([`push_all`]), whatever
/// its blob type.
pub trait Part: Send + Sync {
    fn key(&self) -> &'static str;
    /// The stored copies hold every pending entry.
    fn covered(&self, copies: &[ServiceFields], now_ms: i64) -> bool;
    /// The value to write: the stored copies merged with the pending entries.
    fn next(&self, copies: &[ServiceFields], now_ms: i64) -> Value;
}

impl<B: SyncBlob> Part for B {
    fn key(&self) -> &'static str {
        B::KEY
    }

    fn covered(&self, copies: &[ServiceFields], now_ms: i64) -> bool {
        stored::<B>(copies, now_ms).covers_at(self, now_ms)
    }

    fn next(&self, copies: &[ServiceFields], now_ms: i64) -> Value {
        let blobs: Vec<B> = copies
            .iter()
            .map(|f| B::from_value_at(f.get(B::KEY), now_ms))
            .collect();
        let mut next = B::default();
        for b in &blobs {
            next.merge(b);
        }
        next.merge(self);
        next.to_value_over_at(&blobs, now_ms)
    }
}

/// Where the service fields are stored (the service rooms on a server, a
/// fake in tests). Writes replace the whole object.
pub trait FieldsStore {
    type Error;
    /// Every stored copy of the fields (several service rooms: one each;
    /// empty when nothing is stored yet).
    fn read(&mut self) -> impl Future<Output = Result<Vec<ServiceFields>, Self::Error>> + Send;
    /// Sets `values` on the fields as last read (other keys are kept) and
    /// stores them, creating the store when there is none.
    fn write(
        &mut self,
        values: &[(&'static str, Value)],
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Writes the pending entries of every part in one loop: read, write every
/// key whose part the copies do not cover yet, read again, up to `attempts`
/// writes. Returns the last copies read; a part they do not cover lost to
/// concurrent writes of other devices (check with [`stored`] and
/// [`SyncBlob::covers_at`]).
pub async fn push_all<S: FieldsStore>(
    store: &mut S,
    parts: &[Box<dyn Part>],
    attempts: usize,
    now_ms: i64,
) -> Result<Vec<ServiceFields>, S::Error> {
    let mut copies = store.read().await?;
    for _ in 0..attempts {
        let values: Vec<(&'static str, Value)> = parts
            .iter()
            .filter(|p| !p.covered(&copies, now_ms))
            .map(|p| (p.key(), p.next(&copies, now_ms)))
            .collect();
        if values.is_empty() {
            break;
        }
        store.write(&values).await?;
        copies = store.read().await?;
    }
    Ok(copies)
}

/// A [`FieldsStore`] that knows what this client last wrote to it
/// ([`LastWrite`]), for [`push_locked`].
pub trait Remembering: FieldsStore {
    /// What this client wrote last, before this store's first read.
    fn remember(&mut self, last: LastWrite);
    /// What this client wrote last, after this store's writes.
    fn last(&self) -> &LastWrite;
}

/// [`push_all`] under the server's lock, which holds what this client last
/// wrote there: the store gets it before its first read and the lock gets
/// the store's after the push, so a push waiting for another one never
/// starts from fields older than that one's write.
pub async fn push_locked<S: Remembering>(
    lock: &tokio::sync::Mutex<LastWrite>,
    store: &mut S,
    parts: &[Box<dyn Part>],
    attempts: usize,
    now_ms: i64,
) -> Result<Vec<ServiceFields>, S::Error> {
    let mut last = lock.lock().await;
    store.remember(last.clone());
    let res = push_all(store, parts, attempts, now_ms).await;
    *last = store.last().clone();
    res
}

#[derive(Debug, thiserror::Error)]
pub enum PushError<E: std::error::Error + 'static> {
    #[error(transparent)]
    Store(#[from] E),
    /// Other writers kept replacing the blob.
    #[error("the synced data was changed elsewhere at the same time")]
    Conflict,
}

/// What this client last wrote to a server's service fields. A copy of the
/// fields known without a request (a subscription's, which the server
/// updates after the room) is only used while it shows that write: else a
/// write right after another one, before its echo arrived, would start from
/// the older fields and put back the old value of a key the first write
/// changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum LastWrite {
    /// Nothing written yet.
    #[default]
    Nothing,
    /// The fields room `rid` held after the write.
    Wrote { rid: String, fields: ServiceFields },
    /// A write failed and may or may not have been stored.
    Unknown,
}

impl LastWrite {
    /// Whether `seed`, a copy of room `rid`'s fields known without a
    /// request, can stand in for a read: it shows the last write, or a
    /// later state (another device's write on top of it). `keys` are the
    /// keys this client syncs; any other key must be as written.
    pub fn trusts(&self, rid: &str, seed: &ServiceFields, keys: &[KeyCheck], now_ms: i64) -> bool {
        match self {
            Self::Nothing => true,
            Self::Wrote { rid: r, fields } => {
                r != rid || fields == seed || shows(seed, fields, keys, now_ms)
            }
            Self::Unknown => false,
        }
    }
}

/// Every synced key of `seed` covers the one in `written`, and every other
/// key is the same.
fn shows(seed: &ServiceFields, written: &ServiceFields, keys: &[KeyCheck], now_ms: i64) -> bool {
    let (mut rest_seed, mut rest_written) = (seed.clone(), written.clone());
    for k in keys {
        if !(k.covers)(seed.get(k.key), written.get(k.key), now_ms) {
            return false;
        }
        rest_seed.remove(k.key);
        rest_written.remove(k.key);
    }
    rest_seed == rest_written
}

/// One synced key as [`LastWrite::trusts`] compares it.
#[derive(Debug, Clone, Copy)]
pub struct KeyCheck {
    key: &'static str,
    /// The first stored value holds every entry of the second.
    covers: fn(Option<&Value>, Option<&Value>, i64) -> bool,
}

impl KeyCheck {
    /// The `customFields` key checked.
    pub fn key(&self) -> &'static str {
        self.key
    }

    pub fn of<B: SyncBlob>() -> Self {
        Self {
            key: B::KEY,
            covers: |have, want, now_ms| {
                B::from_value_at(have, now_ms).covers_at(&B::from_value_at(want, now_ms), now_ms)
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::flags::{FlagBlob, FlagColor, FlagEdit};
    use serde_json::json;

    /// A toy blob with expiring tombstones: `items` id -> value.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub(crate) struct Toy {
        items: BTreeMap<String, Mark>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) struct Mark {
        stamp: Stamp,
        value: Option<String>,
    }

    impl Entry for Mark {
        fn stamp(&self) -> &Stamp {
            &self.stamp
        }
        fn is_tombstone(&self) -> bool {
            self.value.is_none()
        }
    }

    const TTL: i64 = 1_000;

    impl SyncBlob for Toy {
        const KEY: &'static str = "rustketToy";
        const TOMBSTONE_TTL_MS: Option<i64> = Some(TTL);

        fn from_value_at(v: Option<&Value>, _now_ms: i64) -> Self {
            let mut b = Self::default();
            if let Some(root) = v.and_then(Value::as_object) {
                for (k, o) in entries(root, "items").filter(|(k, _)| valid_id(k)) {
                    let m = Mark {
                        stamp: stamp_of(&o),
                        value: str_of(&o, "v"),
                    };
                    merge_entry(&mut b.items, k, &m);
                }
            }
            b
        }

        fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value {
            let copies: Vec<&BTreeMap<String, Mark>> = copies.iter().map(|c| &c.items).collect();
            let items: Map<String, Value> =
                written(&self.items, &copies, Self::TOMBSTONE_TTL_MS, now_ms)
                    .map(|(k, m)| {
                        (
                            k.clone(),
                            json!({"v": m.value, "t": m.stamp.t, "d": m.stamp.d}),
                        )
                    })
                    .collect();
            json!({ "items": items })
        }

        fn merge(&mut self, other: &Self) -> bool {
            merge_map(&mut self.items, &other.items)
        }

        fn covers_at(&self, other: &Self, now_ms: i64) -> bool {
            covers_map(&self.items, &other.items, Self::TOMBSTONE_TTL_MS, now_ms)
        }

        fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self {
            Self {
                items: uncovered_map(&self.items, &stored.items, Self::TOMBSTONE_TTL_MS, now_ms),
            }
        }

        fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool {
            let before = self.items.len();
            *self = self.uncovered_by_at(stored, now_ms);
            self.items.len() != before
        }

        fn is_empty(&self) -> bool {
            self.items.is_empty()
        }

        fn len(&self) -> usize {
            self.items.len()
        }
    }

    pub(crate) fn toy(entries: &[(&str, Option<&str>, i64)]) -> Toy {
        let mut b = Toy::default();
        for (k, v, t) in entries {
            b.items.insert(
                (*k).into(),
                Mark {
                    stamp: Stamp {
                        t: *t,
                        d: "dev".into(),
                    },
                    value: v.map(str::to_owned),
                },
            );
        }
        b
    }

    fn flag(rid: &str, c: FlagColor, t: i64) -> FlagBlob {
        let mut b = FlagBlob::default();
        let edit = FlagEdit::Room {
            rid: rid.into(),
            color: Some(c),
        };
        b.edit(&FlagBlob::default(), &edit, t, "dev").unwrap();
        b
    }

    fn fields_with(keys: &[(&str, Value)]) -> ServiceFields {
        let mut f = ServiceFields::new("me");
        for (k, v) in keys {
            f.set(k, v.clone());
        }
        f
    }

    #[derive(Debug, thiserror::Error)]
    #[error("store down")]
    struct Down;

    /// One service room in memory, written like the server does it
    /// (whole-object replacement), read through a copy known without a
    /// request (a subscription) for the first read, as the app's store does.
    #[derive(Default)]
    struct Room {
        stored: Option<ServiceFields>,
        writes: usize,
        /// Reads that went to the room (not to a seed).
        reads: usize,
    }

    fn keys() -> [KeyCheck; 2] {
        [KeyCheck::of::<FlagBlob>(), KeyCheck::of::<Toy>()]
    }

    struct Store<'a> {
        room: &'a mut Room,
        /// The fields a subscription showed when the push was planned.
        seed: Option<ServiceFields>,
        last_read: Option<ServiceFields>,
        last: LastWrite,
        /// Writes of other devices right after each of ours (read before
        /// it, so ours are lost).
        clobbers: Vec<ServiceFields>,
        /// Another service room of the user: read, never written.
        extra: Option<ServiceFields>,
        fail: bool,
    }

    impl<'a> Store<'a> {
        fn new(room: &'a mut Room, seed: Option<ServiceFields>) -> Self {
            Self {
                room,
                seed,
                last_read: None,
                last: LastWrite::Nothing,
                clobbers: Vec::new(),
                extra: None,
                fail: false,
            }
        }
    }

    impl Remembering for Store<'_> {
        fn remember(&mut self, last: LastWrite) {
            self.last = last;
        }

        fn last(&self) -> &LastWrite {
            &self.last
        }
    }

    impl FieldsStore for Store<'_> {
        type Error = Down;

        async fn read(&mut self) -> Result<Vec<ServiceFields>, Down> {
            if self.fail {
                return Err(Down);
            }
            let read = match self.seed.take() {
                Some(seed) if self.last.trusts("svc", &seed, &keys(), 100) => Some(seed),
                _ => {
                    self.room.reads += 1;
                    self.room.stored.clone()
                }
            };
            self.last_read = read.clone();
            Ok(read.into_iter().chain(self.extra.clone()).collect())
        }

        async fn write(&mut self, values: &[(&'static str, Value)]) -> Result<(), Down> {
            let mut f = self
                .last_read
                .take()
                .unwrap_or_else(|| ServiceFields::new("me"));
            for (k, v) in values {
                f.set(k, v.clone());
            }
            self.room.writes += 1;
            self.room.stored = Some(f.clone());
            self.last = LastWrite::Wrote {
                rid: "svc".into(),
                fields: f,
            };
            if !self.clobbers.is_empty() {
                self.room.stored = Some(self.clobbers.remove(0));
            }
            Ok(())
        }
    }

    fn boxed<B: SyncBlob>(b: &B) -> Box<dyn Part> {
        Box::new(b.clone())
    }

    #[tokio::test]
    async fn one_push_writes_every_key_and_keeps_unknown_ones() {
        let mut room = Room {
            stored: Some(fields_with(&[("other", json!({"x": 1}))])),
            writes: 0,
            reads: 0,
        };
        let flags = flag("R1", FlagColor::Red, 10);
        let items = toy(&[("a", Some("1"), 10)]);
        let mut s = Store::new(&mut room, None);
        let copies = push_all(&mut s, &[boxed(&flags), boxed(&items)], 3, 100)
            .await
            .unwrap();
        assert!(stored::<FlagBlob>(&copies, 100).covers(&flags));
        assert!(stored::<Toy>(&copies, 100).covers_at(&items, 100));
        assert_eq!(room.writes, 1, "both keys in one write");
        let f = room.stored.as_ref().unwrap();
        assert_eq!(f.get("other"), Some(&json!({"x": 1})));
        assert!(f.is_owned_by("me"));
        // Nothing left: no write.
        let mut s = Store::new(&mut room, None);
        push_all(&mut s, &[boxed(&flags), boxed(&items)], 3, 100)
            .await
            .unwrap();
        assert_eq!(room.writes, 1);
    }

    #[tokio::test]
    async fn back_to_back_pushes_of_two_keys_keep_both() {
        // Both pushes were planned from the same subscription copy, before
        // either wrote: the second must not start from it. Only the server's
        // lock tells it so.
        let fresh = || Room {
            stored: Some(fields_with(&[])),
            writes: 0,
            reads: 0,
        };
        let mut room = fresh();
        let seed = room.stored.clone();
        let flags = flag("R1", FlagColor::Red, 10);
        let items = toy(&[("a", Some("1"), 10)]);
        let lock = tokio::sync::Mutex::new(LastWrite::Nothing);
        let mut first = Store::new(&mut room, seed.clone());
        push_locked(&lock, &mut first, &[boxed(&flags)], 3, 100)
            .await
            .unwrap();
        let last = lock.lock().await.clone();
        assert!(matches!(&last, LastWrite::Wrote { rid, .. } if rid == "svc"));
        let mut second = Store::new(&mut room, seed.clone());
        let copies = push_locked(&lock, &mut second, &[boxed(&items)], 3, 100)
            .await
            .unwrap();
        assert!(stored::<FlagBlob>(&copies, 100).covers(&flags));
        assert!(stored::<Toy>(&copies, 100).covers_at(&items, 100));
        let f = room.stored.clone().unwrap();
        assert!(FlagBlob::from_value(f.get(FlagBlob::KEY)).covers(&flags));
        assert!(Toy::from_value_at(f.get(Toy::KEY), 100).covers_at(&items, 100));
        assert_eq!(room.writes, 2);
        assert_eq!(
            *lock.lock().await,
            LastWrite::Wrote {
                rid: "svc".into(),
                fields: f.clone()
            }
        );
        // A copy that shows the last write is used as it is.
        let last = lock.lock().await.clone();
        let old = seed.as_ref().unwrap();
        assert!(last.trusts("svc", &f, &keys(), 100));
        assert!(!last.trusts("svc", old, &keys(), 100));
        assert!(last.trusts("other-room", old, &keys(), 100));
        assert!(!LastWrite::Unknown.trusts("svc", old, &keys(), 100));

        // Without the lock's record the second push trusts the old copy
        // and puts the flags' old value back: what the lock prevents.
        let mut room = fresh();
        let mut first = Store::new(&mut room, seed.clone());
        push_all(&mut first, &[boxed(&flags)], 3, 100)
            .await
            .unwrap();
        let mut second = Store::new(&mut room, seed.clone());
        push_all(&mut second, &[boxed(&items)], 3, 100)
            .await
            .unwrap();
        let f = room.stored.clone().unwrap();
        assert!(!FlagBlob::from_value(f.get(FlagBlob::KEY)).covers(&flags));
    }

    #[tokio::test]
    async fn copies_newer_than_the_last_write_are_used() {
        let mut room = Room {
            stored: Some(fields_with(&[("other", json!(1))])),
            writes: 0,
            reads: 0,
        };
        let lock = tokio::sync::Mutex::new(LastWrite::Nothing);
        let r1 = flag("R1", FlagColor::Red, 10);
        let mut s = Store::new(&mut room, None);
        push_locked(&lock, &mut s, &[boxed(&r1)], 3, 100)
            .await
            .unwrap();
        let ours = room.stored.clone().unwrap();
        // Another device adds a flag and a toy entry on top of our write.
        let mut theirs = ours.clone();
        let both = r1.merged_with(&flag("R2", FlagColor::Blue, 20));
        theirs.set(FlagBlob::KEY, both.to_value());
        theirs.set(Toy::KEY, toy(&[("a", Some("1"), 20)]).to_value_at(100));
        room.stored = Some(theirs.clone());
        let last = lock.lock().await.clone();
        assert!(last.trusts("svc", &theirs, &keys(), 100));
        // Our next push starts from the subscription's copy of their write:
        // no read before the write, theirs kept.
        let r3 = flag("R3", FlagColor::Green, 30);
        let reads = room.reads;
        let mut s = Store::new(&mut room, Some(theirs.clone()));
        let copies = push_locked(&lock, &mut s, &[boxed(&r3)], 3, 100)
            .await
            .unwrap();
        assert_eq!(room.reads, reads + 1, "only the read after the write");
        let flags = stored::<FlagBlob>(&copies, 100);
        assert!(flags.covers(&both) && flags.covers(&r3));
        assert!(stored::<Toy>(&copies, 100).covers_at(&toy(&[("a", Some("1"), 20)]), 100));
        // Not trusted: a copy missing an entry of the last write, or one
        // where a key this client does not sync differs.
        let last = lock.lock().await.clone();
        let LastWrite::Wrote { fields, .. } = &last else {
            unreachable!()
        };
        let mut older = fields.clone();
        older.set(FlagBlob::KEY, both.to_value());
        assert!(!last.trusts("svc", &older, &keys(), 100));
        let mut changed = fields.clone();
        changed.set("other", json!(2));
        assert!(!last.trusts("svc", &changed, &keys(), 100));
        let mut newer = fields.clone();
        newer.set(Toy::KEY, toy(&[("a", Some("2"), 40)]).to_value_at(100));
        assert!(last.trusts("svc", &newer, &keys(), 100));
    }

    #[tokio::test]
    async fn lost_parts_are_retried_then_reported_per_key() {
        let mut room = Room {
            stored: Some(fields_with(&[])),
            writes: 0,
            reads: 0,
        };
        let flags = flag("R1", FlagColor::Red, 10);
        let items = toy(&[("a", Some("1"), 10)]);
        // Another device keeps writing the fields back with our other key
        // but without our flags.
        let theirs = fields_with(&[
            (FlagBlob::KEY, flag("X", FlagColor::Blue, 5).to_value()),
            (Toy::KEY, items.to_value_at(100)),
        ]);
        let mut s = Store::new(&mut room, None);
        s.clobbers = vec![theirs.clone(), theirs.clone(), theirs.clone()];
        let copies = push_all(&mut s, &[boxed(&flags), boxed(&items)], 2, 100)
            .await
            .unwrap();
        assert_eq!(room.writes, 2);
        // Only the flags are lost.
        assert!(!stored::<FlagBlob>(&copies, 100).covers(&flags));
        assert!(stored::<Toy>(&copies, 100).covers_at(&items, 100));
        let mut s = Store::new(&mut room, None);
        s.fail = true;
        assert!(push_all(&mut s, &[boxed(&flags)], 2, 100).await.is_err());
    }

    #[tokio::test]
    async fn expired_tombstones_converge() {
        let now = 10_000;
        // Removals pending since long ago, one over an older live entry.
        let pending = toy(&[("a", None, 100), ("b", None, 200)]);
        let mut room = Room {
            stored: Some(fields_with(&[(
                Toy::KEY,
                toy(&[("a", Some("1"), 50)]).to_value_at(now),
            )])),
            writes: 0,
            reads: 0,
        };
        let mut s = Store::new(&mut room, None);
        let copies = push_all(&mut s, &[boxed(&pending)], 4, now).await.unwrap();
        assert_eq!(room.writes, 1, "one write, then covered");
        let left = stored::<Toy>(&copies, now);
        // `b` was dropped; `a` stays once, as the written room held it.
        assert_eq!(left, toy(&[("a", None, 100)]));
        assert!(left.covers_at(&pending, now));
        let mut overlay = pending.clone();
        assert!(overlay.prune_covered_at(&left, now));
        assert!(overlay.is_empty());
        // The next write of the key drops it.
        let fresh = toy(&[("c", None, now - 10)]);
        let mut s = Store::new(&mut room, None);
        let copies = push_all(&mut s, &[boxed(&fresh)], 4, now).await.unwrap();
        assert_eq!(room.writes, 2);
        // A fresh tombstone is written and kept.
        assert_eq!(stored::<Toy>(&copies, now), fresh);
        // A newer live entry is not covered by an expired tombstone.
        let live = toy(&[("a", Some("2"), 50)]);
        let tomb = toy(&[("a", None, 100)]);
        assert!(!live.covers_at(&tomb, now));
        assert!(!covers_map(&live.items, &tomb.items, None, now));
        assert!(toy(&[]).covers_at(&tomb, now));
        assert!(!covers_map(&toy(&[]).items, &tomb.items, None, now));
    }

    #[tokio::test]
    async fn expired_tombstones_converge_over_another_service_room() {
        let now = 10_000;
        // Another service room (never written) still holds the entry the
        // removal hides.
        let other = fields_with(&[(Toy::KEY, toy(&[("a", Some("1"), 50)]).to_value_at(now))]);
        let pending = toy(&[("a", None, 100)]);
        let mut room = Room {
            stored: Some(fields_with(&[])),
            writes: 0,
            reads: 0,
        };
        let mut s = Store::new(&mut room, None);
        s.extra = Some(other.clone());
        let copies = push_all(&mut s, &[boxed(&pending)], 4, now).await.unwrap();
        assert_eq!(room.writes, 1, "one write, then covered");
        let shown = stored::<Toy>(&copies, now);
        assert_eq!(shown, pending, "the removal is kept and wins");
        assert!(shown.covers_at(&pending, now));
        // Later writes keep it while the other room holds the entry.
        let next = toy(&[("b", Some("2"), now)]);
        let mut s = Store::new(&mut room, None);
        s.extra = Some(other);
        let copies = push_all(&mut s, &[boxed(&next)], 4, now).await.unwrap();
        assert_eq!(room.writes, 2);
        assert_eq!(
            stored::<Toy>(&copies, now),
            toy(&[("a", None, 100), ("b", Some("2"), now)])
        );
        // Once it is gone there, the next write drops the removal.
        let last = toy(&[("b", Some("3"), now + 1)]);
        let mut s = Store::new(&mut room, None);
        let copies = push_all(&mut s, &[boxed(&last)], 4, now).await.unwrap();
        assert_eq!(stored::<Toy>(&copies, now), last);
    }

    #[test]
    fn budget() {
        assert!(fits_budget(0, BUDGET_BYTES));
        assert!(!fits_budget(0, BUDGET_BYTES + 1));
        // Over it already: shrinking or equal is fine, growing is not.
        assert!(fits_budget(BUDGET_BYTES + 10, BUDGET_BYTES + 5));
        assert!(fits_budget(BUDGET_BYTES + 10, BUDGET_BYTES + 10));
        assert!(!fits_budget(BUDGET_BYTES + 10, BUDGET_BYTES + 11));
        assert_eq!(key_bytes("k", &json!({})), 1 + 4 + 2);
    }

    #[test]
    fn budget_bytes_leave_out_tombstones_that_never_expire() {
        let now = 10_000;
        // Flags: removed flags cost nothing (they can never be freed).
        let mut flags = FlagBlob::default();
        for n in 0..50 {
            let rid = format!("R{n}");
            let on = FlagEdit::Room {
                rid: rid.clone(),
                color: Some(FlagColor::Red),
            };
            flags.edit(&FlagBlob::default(), &on, 10, "dev").unwrap();
            let off = FlagEdit::Room { rid, color: None };
            flags.edit(&FlagBlob::default(), &off, 20, "dev").unwrap();
        }
        let empty = FlagBlob::default().budget_bytes(now);
        assert_eq!(flags.budget_bytes(now), empty);
        assert!(key_bytes(FlagBlob::KEY, &flags.to_value()) > empty + 50 * 20);
        flags.merge(&flag("R1", FlagColor::Blue, 30));
        assert!(flags.budget_bytes(now) > empty);
        // Expiring tombstones count until they expire.
        let tomb = toy(&[("a", None, now - 10)]);
        assert!(tomb.budget_bytes(now) > toy(&[]).budget_bytes(now));
        assert_eq!(
            tomb.budget_bytes(now + 2 * TTL),
            toy(&[]).budget_bytes(now + 2 * TTL)
        );
    }

    #[test]
    fn ids_and_stamps() {
        assert!(valid_id("GENERAL"));
        assert!(!valid_id("a.b") && !valid_id("$x") && !valid_id(""));
        let s = Stamp::after(
            Some(&Stamp {
                t: 50,
                d: "A".into(),
            }),
            10,
            "B",
        );
        assert_eq!((s.t, s.d.as_str()), (51, "B"));
        assert_eq!(Stamp::after(None, 10, "B").t, 10);
    }
}
