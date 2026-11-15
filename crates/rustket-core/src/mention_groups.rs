//! Mention groups: named lists of usernames per account (`@backend`), synced
//! between the user's devices.
//!
//! The synced form is a [`MentionGroupBlob`] stored as JSON under
//! [`MENTION_GROUPS_KEY`](rocketchat::models::MENTION_GROUPS_KEY) in the
//! `customFields` of the user's hidden service room
//! ([`crate::service_sync`]). Groups are keyed by a random id, so a rename
//! stays one entry; each entry is last-writer-wins on the whole group (name
//! and members) and a deleted group is a tombstone that expires after
//! [`TOMBSTONE_TTL_MS`]. Only group names and usernames are stored, never
//! message text.
//!
//! [`expand`] rewrites a message before it is sent: `@backend` becomes
//! `@backend (@alice @bob)`, so readers still see the group and the server
//! notifies every member as an ordinary mention (also in encrypted rooms,
//! whose plaintext mention list is taken from the expanded text).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use serde_json::{Map, Value, json};

use crate::service_sync::{
    EditBlob, Entry, MAX_FUTURE_MS, Stamp, SyncBlob, covers_map, key_bytes, merge_entry, merge_map,
    stamp_of, uncovered_map, valid_id, written,
};

/// Most live groups per account.
pub const MAX_GROUPS: usize = 50;
/// Most members per group: an encrypted room's plaintext mention list stops
/// at 50 entries, and the group's own `@name`, kept in the sent text, takes
/// one of them.
pub const MAX_GROUP_MEMBERS: usize = 49;
pub const MIN_GROUP_NAME_CHARS: usize = 2;
pub const MAX_GROUP_NAME_CHARS: usize = 32;
/// Longest username kept, in bytes.
pub const MAX_USERNAME_BYTES: usize = 64;
/// Serialized size of the groups an edit may grow them to (the shared
/// [`crate::service_sync::BUDGET_BYTES`] applies as well).
pub const MAX_BLOB_BYTES: usize = 24 * 1024;
/// Deleted groups are forgotten after this long (a device offline for longer
/// may bring a deleted group back).
pub const TOMBSTONE_TTL_MS: i64 = 90 * 24 * 3600 * 1000;
/// Format written into the blob's `v`. Readers accept any version.
pub const GROUPS_FORMAT: u64 = 1;
/// Names the server reserves for room-wide mentions.
pub const RESERVED: [&str; 2] = ["all", "here"];

/// A group name as stored: trimmed, without a leading `@`, lowercase, 2–32 of
/// `a-z`, `0-9`, `-`, `_`, starting with a letter or digit (no `.`, so a
/// sentence may end right after `@backend.`). `None` when it does not fit.
pub fn clean_group_name(name: &str) -> Option<String> {
    let n = name.trim();
    let n = n.strip_prefix('@').unwrap_or(n).to_lowercase();
    let len = n.chars().count();
    let ok = (MIN_GROUP_NAME_CHARS..=MAX_GROUP_NAME_CHARS).contains(&len)
        && n.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    ok.then_some(n)
}

/// A username as stored: trimmed, without a leading `@`, no whitespace or
/// control characters, at most [`MAX_USERNAME_BYTES`].
pub fn clean_username(name: &str) -> Option<String> {
    let n = name.trim();
    let n = n.strip_prefix('@').unwrap_or(n);
    let ok = !n.is_empty()
        && n.len() <= MAX_USERNAME_BYTES
        && !n.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then(|| n.to_owned())
}

/// Cleaned usernames, without duplicates (ignoring case), sorted.
fn clean_members<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out: Vec<String> = names
        .into_iter()
        .filter_map(clean_username)
        .filter(|n| seen.insert(n.to_lowercase()))
        .collect();
    out.sort();
    out
}

/// A group's entry. `name: None` is a tombstone (a deleted group).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupMark {
    pub stamp: Stamp,
    pub name: Option<String>,
    /// Sorted, without duplicates.
    pub members: Vec<String>,
    /// Entry fields a newer version added, as JSON object text (empty: none).
    extra: String,
}

impl Entry for GroupMark {
    fn stamp(&self) -> &Stamp {
        &self.stamp
    }
    fn is_tombstone(&self) -> bool {
        self.name.is_none()
    }
}

const ENTRY_KEYS: [&str; 4] = ["n", "u", "t", "d"];

impl GroupMark {
    fn from_object(o: &Map<String, Value>) -> Self {
        let name = o
            .get("n")
            .and_then(Value::as_str)
            .and_then(clean_group_name);
        let members = match (&name, o.get("u").and_then(Value::as_array)) {
            (Some(_), Some(list)) => {
                let mut m = clean_members(list.iter().filter_map(Value::as_str));
                m.truncate(MAX_GROUP_MEMBERS);
                m
            }
            _ => Vec::new(),
        };
        let extra: Map<String, Value> = o
            .iter()
            .filter(|(k, _)| !ENTRY_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Tombstones keep no extra fields.
        let extra = if extra.is_empty() || name.is_none() {
            String::new()
        } else {
            Value::Object(extra).to_string()
        };
        Self {
            stamp: stamp_of(o),
            name,
            members,
            extra,
        }
    }

    fn to_value(&self) -> Value {
        let mut o = match serde_json::from_str::<Value>(&self.extra) {
            Ok(Value::Object(o)) => o,
            _ => Map::new(),
        };
        if let Some(n) = &self.name {
            o.insert("n".into(), json!(n));
            o.insert("u".into(), json!(self.members));
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
pub enum MentionGroupEdit {
    /// A new group (a fresh id, `rocketchat::ids::random_id`) or a changed one.
    Upsert {
        id: String,
        name: String,
        members: Vec<String>,
    },
    Delete {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MentionGroupError {
    #[error("invalid group id")]
    InvalidId,
    #[error("invalid group name")]
    InvalidName,
    #[error("reserved group name")]
    Reserved,
    #[error("duplicate group name")]
    Duplicate,
    #[error("a group needs members")]
    Empty,
    #[error("too many members")]
    TooManyMembers,
    #[error("too many groups")]
    Full,
    #[error("the groups are too large to sync")]
    TooLarge,
}

/// The synced groups: a last-writer-wins map by group id plus top-level keys
/// a newer version added (kept on write).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MentionGroupBlob {
    pub groups: BTreeMap<String, GroupMark>,
    v: u64,
    extra: Map<String, Value>,
}

const KNOWN_KEYS: [&str; 2] = ["v", "groups"];

impl MentionGroupBlob {
    /// The effective groups of the blob.
    pub fn view(&self) -> MentionGroups {
        let mut best: BTreeMap<&str, &Stamp> = BTreeMap::new();
        for m in self.groups.values() {
            if let Some(n) = &m.name {
                let b = best.entry(n.as_str()).or_insert(&m.stamp);
                if m.stamp > **b {
                    *b = &m.stamp;
                }
            }
        }
        let mut list: Vec<MentionGroup> = self
            .groups
            .iter()
            .filter_map(|(id, m)| {
                let name = m.name.clone()?;
                let shadowed = best.get(name.as_str()).is_some_and(|b| **b != m.stamp);
                Some(MentionGroup {
                    id: id.clone(),
                    name,
                    members: m.members.clone(),
                    shadowed,
                })
            })
            .collect();
        list.sort_by(|a, b| (&a.name, a.shadowed, &a.id).cmp(&(&b.name, b.shadowed, &b.id)));
        MentionGroups { list }
    }

    fn live(&self) -> usize {
        self.groups.values().filter(|m| !m.is_tombstone()).count()
    }

    /// Records `edit` as pending entries of this blob (the device's overlay)
    /// on top of `stored`, stamped after the entry it replaces. Returns
    /// whether anything changed.
    pub fn edit(
        &mut self,
        stored: &Self,
        edit: &MentionGroupEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, MentionGroupError> {
        let current = stored.merged_with(self);
        match edit {
            MentionGroupEdit::Upsert { id, name, members } => {
                if !valid_id(id) {
                    return Err(MentionGroupError::InvalidId);
                }
                let name = clean_group_name(name).ok_or(MentionGroupError::InvalidName)?;
                if RESERVED.contains(&name.as_str()) {
                    return Err(MentionGroupError::Reserved);
                }
                let members = clean_members(members.iter().map(String::as_str));
                if members.is_empty() {
                    return Err(MentionGroupError::Empty);
                }
                if members.len() > MAX_GROUP_MEMBERS {
                    return Err(MentionGroupError::TooManyMembers);
                }
                let prev = current.groups.get(id);
                // Taking another group's name is refused; a group that
                // already has its name keeps it (two devices may have
                // created the same name, and either copy stays editable).
                let renamed = prev.is_none_or(|m| m.name.as_deref() != Some(name.as_str()));
                let duplicate = current
                    .groups
                    .iter()
                    .any(|(k, m)| k != id && m.name.as_deref() == Some(name.as_str()));
                if renamed && duplicate {
                    return Err(MentionGroupError::Duplicate);
                }
                if prev.is_some_and(|m| m.name.as_deref() == Some(&name) && m.members == members) {
                    return Ok(false);
                }
                if prev.is_none_or(Entry::is_tombstone) && current.live() >= MAX_GROUPS {
                    return Err(MentionGroupError::Full);
                }
                let mark = GroupMark {
                    stamp: Stamp::after(prev.map(|m| &m.stamp), now_ms, device),
                    name: Some(name),
                    members,
                    extra: prev.map(|m| m.extra.clone()).unwrap_or_default(),
                };
                let before = key_bytes(Self::KEY, &current.to_value_at(now_ms));
                let mut next = current;
                next.groups.insert(id.clone(), mark.clone());
                let after = key_bytes(Self::KEY, &next.to_value_at(now_ms));
                if after > MAX_BLOB_BYTES && after > before {
                    return Err(MentionGroupError::TooLarge);
                }
                Ok(merge_entry(&mut self.groups, id, &mark))
            }
            MentionGroupEdit::Delete { id } => {
                let Some(prev) = current.groups.get(id).filter(|m| !m.is_tombstone()) else {
                    return Ok(false);
                };
                let mark = GroupMark {
                    stamp: Stamp::after(Some(&prev.stamp), now_ms, device),
                    name: None,
                    members: Vec::new(),
                    extra: String::new(),
                };
                Ok(merge_entry(&mut self.groups, id, &mark))
            }
        }
    }
}

impl SyncBlob for MentionGroupBlob {
    const KEY: &'static str = rocketchat::models::MENTION_GROUPS_KEY;
    const TOMBSTONE_TTL_MS: Option<i64> = Some(TOMBSTONE_TTL_MS);

    /// Lenient: anything that is not an object is empty, ids that cannot key
    /// an entry and entries stamped more than [`MAX_FUTURE_MS`] ahead are
    /// dropped, an invalid name makes the entry a tombstone, usernames are
    /// cleaned and cut to [`MAX_GROUP_MEMBERS`].
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
        let groups = root.get("groups").and_then(Value::as_object);
        for (id, e) in groups.into_iter().flatten() {
            let empty = Map::new();
            let o = match e {
                Value::Object(o) => o,
                Value::Null => &empty,
                _ => continue,
            };
            if !valid_id(id) || stamp_of(o).t > max_t {
                continue;
            }
            merge_entry(&mut b.groups, id, &GroupMark::from_object(o));
        }
        b
    }

    fn to_value_over_at(&self, copies: &[Self], now_ms: i64) -> Value {
        let copies: Vec<&BTreeMap<String, GroupMark>> = copies.iter().map(|c| &c.groups).collect();
        let groups: Map<String, Value> =
            written(&self.groups, &copies, Self::TOMBSTONE_TTL_MS, now_ms)
                .map(|(k, m)| (k.clone(), m.to_value()))
                .collect();
        let mut root = self.extra.clone();
        root.insert("v".into(), json!(self.v.max(GROUPS_FORMAT)));
        root.insert("groups".into(), Value::Object(groups));
        Value::Object(root)
    }

    fn merge(&mut self, other: &Self) -> bool {
        let changed = merge_map(&mut self.groups, &other.groups);
        self.v = self.v.max(other.v);
        for (k, v) in &other.extra {
            self.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
        changed
    }

    fn covers_at(&self, other: &Self, now_ms: i64) -> bool {
        covers_map(&self.groups, &other.groups, Self::TOMBSTONE_TTL_MS, now_ms)
    }

    fn uncovered_by_at(&self, stored: &Self, now_ms: i64) -> Self {
        Self {
            groups: uncovered_map(&self.groups, &stored.groups, Self::TOMBSTONE_TTL_MS, now_ms),
            ..Self::default()
        }
    }

    fn prune_covered_at(&mut self, stored: &Self, now_ms: i64) -> bool {
        let before = self.groups.len();
        self.groups = uncovered_map(&self.groups, &stored.groups, Self::TOMBSTONE_TTL_MS, now_ms);
        self.groups.len() != before
    }

    fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    fn len(&self) -> usize {
        self.groups.len()
    }
}

impl EditBlob for MentionGroupBlob {
    type Edit = MentionGroupEdit;
    type Error = MentionGroupError;

    fn apply_edit(
        &mut self,
        stored: &Self,
        edit: &MentionGroupEdit,
        now_ms: i64,
        device: &str,
    ) -> Result<bool, MentionGroupError> {
        self.edit(stored, edit, now_ms, device)
    }

    fn too_large() -> MentionGroupError {
        MentionGroupError::TooLarge
    }
}

/// A live group as the account shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionGroup {
    pub id: String,
    pub name: String,
    pub members: Vec<String>,
    /// Another group has the same name and a later edit: this one is not
    /// used (two devices created it at the same time).
    pub shadowed: bool,
}

/// The effective groups, sorted by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionGroups {
    list: Vec<MentionGroup>,
}

impl MentionGroups {
    /// The group used for `@name` (any case).
    pub fn get(&self, name: &str) -> Option<&MentionGroup> {
        self.list
            .iter()
            .find(|g| !g.shadowed && g.name.eq_ignore_ascii_case(name))
    }

    /// The live group with this id, shadowed or not.
    pub fn by_id(&self, id: &str) -> Option<&MentionGroup> {
        self.list.iter().find(|g| g.id == id)
    }

    /// Usable groups whose name starts with `query` (any case).
    pub fn matching<'a>(&'a self, query: &'a str) -> impl Iterator<Item = &'a MentionGroup> + 'a {
        let q = query.to_lowercase();
        self.list
            .iter()
            .filter(move |g| !g.shadowed && g.name.starts_with(&q))
    }

    /// Every live group, shadowed ones included (the editor lists them).
    pub fn all(&self) -> &[MentionGroup] {
        &self.list
    }

    /// No live group.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// What [`expand`] needs to know besides the groups.
pub struct ExpandOptions<'a> {
    /// The sender's username: never added.
    pub me: Option<&'a str>,
    /// Whether a name is known to be a real user (a room member, a direct
    /// message partner): such a token is never expanded.
    pub is_user: &'a dyn Fn(&str) -> bool,
}

/// A text with its group mentions expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// Usernames added.
    pub added: Vec<String>,
    /// `@` mention tokens in the result, as an encrypted room's plaintext
    /// mention list counts them (which stops at 50).
    pub mention_tokens: usize,
}

/// Expands every `@group` token of `text` to `@group (@u1 @u2 …)`. Members
/// are left out when they are the sender or are already mentioned anywhere
/// in the text (by hand, by an earlier group or by an expansion already
/// there); a group with nobody left stays as it is. Tokens inside code
/// (fenced blocks, backtick spans) and markdown links are not touched, nor
/// tokens that name a real user, nor a group already followed by its
/// parenthesised expansion: expanding an expanded text changes nothing.
/// `None` when nothing was expanded.
pub fn expand(text: &str, groups: &MentionGroups, opts: &ExpandOptions<'_>) -> Option<Expanded> {
    if groups.is_empty() || !text.contains('@') {
        return None;
    }
    let skip = skipped(text, true);
    let tokens = tokens(text, &skip);
    let mut mentioned: BTreeSet<String> = tokens.iter().map(|t| t.name.to_lowercase()).collect();
    if let Some(me) = opts.me {
        mentioned.insert(me.to_lowercase());
    }
    let mut out = String::with_capacity(text.len() + 64);
    let mut last = 0;
    let mut added = Vec::new();
    for t in &tokens {
        if t.federated || (opts.is_user)(t.name) {
            continue;
        }
        let Some(g) = groups.get(t.name) else {
            continue;
        };
        if existing_expansion(&text[t.name_end..]).is_some() {
            continue;
        }
        let add: Vec<&str> = g
            .members
            .iter()
            .map(String::as_str)
            .filter(|m| mentioned.insert(m.to_lowercase()))
            .collect();
        if add.is_empty() {
            continue;
        }
        out.push_str(&text[last..t.name_end]);
        out.push_str(" (");
        for (i, m) in add.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push('@');
            out.push_str(m);
        }
        out.push(')');
        last = t.name_end;
        added.extend(add.iter().map(|m| (*m).to_owned()));
    }
    if added.is_empty() {
        return None;
    }
    out.push_str(&text[last..]);
    let mention_tokens = tokens_in(&out);
    Some(Expanded {
        text: out,
        added,
        mention_tokens,
    })
}

/// `@` tokens of `text` outside markdown links (what an encrypted room's
/// plaintext mention list is collected from).
fn tokens_in(text: &str) -> usize {
    tokens(text, &skipped(text, false)).len()
}

/// A `@name` token.
struct Token<'a> {
    /// The name, without trailing `.`s.
    name: &'a str,
    /// Byte offset just after the name.
    name_end: usize,
    /// `@name@server`.
    federated: bool,
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

/// `@` tokens at the start of the text or after whitespace, `>` or a `(`
/// that itself follows one of these (`@backend (@alice`, not `f(@x`), outside
/// `skip`; the same boundaries as the encrypted rooms' plaintext mention
/// list (`rocketchat::e2e::e2e_mentions`).
fn tokens<'a>(text: &'a str, skip: &[Range<usize>]) -> Vec<Token<'a>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let edge = |c: Option<char>| c.is_none_or(|p| p.is_whitespace() || p == '>');
    let mut prev: Option<char> = None;
    let mut before: Option<char> = None;
    for (i, c) in text.char_indices() {
        let boundary = edge(prev) || (prev == Some('(') && edge(before));
        before = prev;
        prev = Some(c);
        if c != '@' || !boundary || skip.iter().any(|r| r.contains(&i)) {
            continue;
        }
        let start = i + 1;
        let run = bytes[start..].iter().take_while(|b| name_byte(**b)).count();
        let name = text[start..start + run].trim_end_matches('.');
        if name.is_empty() {
            continue;
        }
        out.push(Token {
            name,
            name_end: start + name.len(),
            federated: bytes.get(start + run) == Some(&b'@'),
        });
    }
    out
}

/// Length of a parenthesised expansion at the start of `rest`: optional
/// blanks, `(`, `@name`s separated by blanks or commas, `)`.
fn existing_expansion(rest: &str) -> Option<usize> {
    let b = rest.as_bytes();
    let blank = |c: u8| c == b' ' || c == b'\t';
    let mut i = b.iter().take_while(|c| blank(**c)).count();
    if b.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    let mut names = 0;
    loop {
        i += b[i..]
            .iter()
            .take_while(|c| blank(**c) || **c == b',')
            .count();
        match b.get(i) {
            Some(b'@') => {
                let n = b[i + 1..].iter().take_while(|c| name_byte(**c)).count();
                if n == 0 {
                    return None;
                }
                names += 1;
                i += 1 + n;
            }
            Some(b')') if names > 0 => return Some(i + 1),
            _ => return None,
        }
    }
}

/// Byte ranges of markdown links `[label](url)` and, with `code`, of fenced
/// code blocks and backtick spans.
fn skipped(text: &str, code: bool) -> Vec<Range<usize>> {
    let mut out = links(text);
    if !code {
        return out;
    }
    let mut fenced = false;
    let mut at = 0;
    for line in text.split('\n') {
        let start = at;
        at += line.len() + 1;
        let fences = line.matches("```").count();
        if fenced || fences > 0 {
            if fences % 2 == 1 {
                fenced = !fenced;
            }
            out.push(start..start + line.len());
            continue;
        }
        // Backtick spans; an unclosed one runs to the end of the line.
        let mut open: Option<usize> = None;
        for (i, b) in line.bytes().enumerate() {
            if b == b'`' {
                match open.take() {
                    Some(o) => out.push(start + o..start + i + 1),
                    None => open = Some(i),
                }
            }
        }
        if let Some(o) = open {
            out.push(start + o..start + line.len());
        }
    }
    out
}

/// `[label](url)` with a label without `]` and a non-empty url without `)`.
fn links(text: &str) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'[' {
            i += 1;
            continue;
        }
        let Some(close) = b[i + 1..].iter().position(|c| *c == b']') else {
            break;
        };
        let close = i + 1 + close;
        if b.get(close + 1) == Some(&b'(')
            && let Some(end) = b[close + 2..].iter().position(|c| *c == b')')
            && end > 0
        {
            let end = close + 2 + end;
            out.push(i..end + 1);
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn upsert(id: &str, name: &str, members: &[&str]) -> MentionGroupEdit {
        MentionGroupEdit::Upsert {
            id: id.into(),
            name: name.into(),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    fn blob(edits: &[MentionGroupEdit], t: i64, device: &str) -> MentionGroupBlob {
        let mut b = MentionGroupBlob::default();
        for e in edits {
            b.edit(&MentionGroupBlob::default(), e, t, device).unwrap();
        }
        b
    }

    fn groups(list: &[(&str, &[&str])]) -> MentionGroups {
        let edits: Vec<_> = list
            .iter()
            .enumerate()
            .map(|(i, (n, m))| upsert(&format!("g{i}"), n, m))
            .collect();
        blob(&edits, NOW, "dev").view()
    }

    fn no_users(_: &str) -> bool {
        false
    }

    fn run(text: &str, g: &MentionGroups, me: Option<&str>) -> Option<String> {
        let opts = ExpandOptions {
            me,
            is_user: &no_users,
        };
        expand(text, g, &opts).map(|e| e.text)
    }

    #[test]
    fn names_are_cleaned_and_validated() {
        assert_eq!(clean_group_name(" @Backend ").as_deref(), Some("backend"));
        assert_eq!(clean_group_name("qa_team-2").as_deref(), Some("qa_team-2"));
        for bad in ["a", "-x", "_x", "a.b", "привет", "a b", "", "@"] {
            assert_eq!(clean_group_name(bad), None, "{bad}");
        }
        assert_eq!(clean_group_name(&"a".repeat(33)), None);
        assert!(clean_group_name(&"a".repeat(32)).is_some());
        assert_eq!(clean_username(" @bob ").as_deref(), Some("bob"));
        assert_eq!(clean_username("a b"), None);
        assert_eq!(clean_username(&"x".repeat(65)), None);
        assert_eq!(
            clean_members(["bob", "@alice", "Bob", "", "carol"]),
            ["alice", "bob", "carol"]
        );
    }

    #[test]
    fn edits_check_names_and_limits() {
        let stored = MentionGroupBlob::default();
        let mut b = MentionGroupBlob::default();
        let e = |b: &mut MentionGroupBlob, edit| b.edit(&stored, &edit, NOW, "dev");
        assert_eq!(
            e(&mut b, upsert("g1", "all", &["bob"])),
            Err(MentionGroupError::Reserved)
        );
        assert_eq!(
            e(&mut b, upsert("g1", "x", &["bob"])),
            Err(MentionGroupError::InvalidName)
        );
        assert_eq!(
            e(&mut b, upsert("a.b", "devs", &["bob"])),
            Err(MentionGroupError::InvalidId)
        );
        assert_eq!(
            e(&mut b, upsert("g1", "devs", &[" "])),
            Err(MentionGroupError::Empty)
        );
        let many: Vec<String> = (0..51).map(|i| format!("u{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            e(&mut b, upsert("g1", "devs", &many)),
            Err(MentionGroupError::TooManyMembers)
        );
        assert_eq!(e(&mut b, upsert("g1", "Devs", &["bob", "alice"])), Ok(true));
        assert_eq!(b.view().get("devs").unwrap().members, ["alice", "bob"]);
        // Same content: no change; another id with the name: refused.
        assert_eq!(
            e(&mut b, upsert("g1", "devs", &["alice", "bob"])),
            Ok(false)
        );
        assert_eq!(
            e(&mut b, upsert("g2", "devs", &["carol"])),
            Err(MentionGroupError::Duplicate)
        );
        // Rename keeps the id.
        assert_eq!(e(&mut b, upsert("g1", "backend", &["bob"])), Ok(true));
        let v = b.view();
        assert!(v.get("devs").is_none());
        assert_eq!(v.get("BACKEND").unwrap().id, "g1");
        // The freed name can be taken.
        assert_eq!(e(&mut b, upsert("g2", "devs", &["carol"])), Ok(true));
        // Delete.
        assert_eq!(
            e(&mut b, MentionGroupEdit::Delete { id: "g2".into() }),
            Ok(true)
        );
        assert_eq!(
            e(&mut b, MentionGroupEdit::Delete { id: "g2".into() }),
            Ok(false)
        );
        assert!(b.view().get("devs").is_none());
        assert!(b.groups["g2"].is_tombstone());
    }

    #[test]
    fn edits_refuse_too_many_or_too_large_groups() {
        let stored = MentionGroupBlob::default();
        let mut b = MentionGroupBlob::default();
        for i in 0..MAX_GROUPS {
            let e = upsert(&format!("g{i}"), &format!("group{i}"), &["bob"]);
            assert_eq!(b.edit(&stored, &e, NOW, "dev"), Ok(true));
        }
        let e = upsert("new", "another", &["bob"]);
        assert_eq!(
            b.edit(&stored, &e, NOW, "dev"),
            Err(MentionGroupError::Full)
        );
        // Changing an existing group is still fine.
        let e = upsert("g0", "group0", &["bob", "carol"]);
        assert_eq!(b.edit(&stored, &e, NOW, "dev"), Ok(true));
        // Long usernames fill the size cap before the group cap.
        let mut b = MentionGroupBlob::default();
        let names: Vec<String> = (0..MAX_GROUP_MEMBERS)
            .map(|i| format!("{i:0>60}"))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let refused = (0..MAX_GROUPS).find_map(|i| {
            let e = upsert(&format!("g{i}"), &format!("group{i}"), &names);
            b.edit(&stored, &e, NOW, "dev").err()
        });
        assert_eq!(refused, Some(MentionGroupError::TooLarge));
        assert!(key_bytes(MentionGroupBlob::KEY, &b.to_value_at(NOW)) <= MAX_BLOB_BYTES);
    }

    #[test]
    fn blob_round_trip_keeps_unknown_keys() {
        let v = json!({
            "v": 3,
            "future": {"x": 1},
            "groups": {
                "Kq3xY7pLmN2aB9cDe": {"n": "backend", "u": ["bob", "carol"], "t": 10, "d": "dev-a", "color": "red"},
                "Zz9yX8wV7uT6sR5qP": {"t": NOW - 10, "d": "dev-b"}
            }
        });
        let b = MentionGroupBlob::from_value_at(Some(&v), NOW);
        assert_eq!(b.to_value_at(NOW), v);
        // The format never goes down.
        let old = MentionGroupBlob::from_value_at(Some(&json!({"groups": {}})), NOW);
        assert_eq!(old.to_value_at(NOW)["v"], 1);
    }

    #[test]
    fn reads_leniently() {
        let v = json!({
            "groups": {
                "a.b": {"n": "x1", "u": ["bob"], "t": 1},
                "s": "backend",
                "g1": {"n": "Bad Name", "u": ["bob"], "t": 1, "x": 1},
                "g2": {"n": "ok", "u": "bob", "t": 1},
                "g3": {"n": "devs", "u": ["@bob", "a b", "", "bob", 3, "carol"], "t": 1},
                "g4": {"n": "future", "u": ["bob"], "t": NOW + MAX_FUTURE_MS + 1},
                "g5": null
            }
        });
        let b = MentionGroupBlob::from_value_at(Some(&v), NOW);
        let ids: Vec<&str> = b.groups.keys().map(String::as_str).collect();
        assert_eq!(ids, ["g1", "g2", "g3", "g5"]);
        assert!(b.groups["g1"].is_tombstone(), "invalid name");
        assert!(b.groups["g1"].to_value().get("x").is_none());
        assert!(b.groups["g2"].members.is_empty());
        assert_eq!(b.groups["g3"].members, ["bob", "carol"]);
        assert!(b.groups["g5"].is_tombstone());
        let many: Vec<String> = (0..80).map(|i| format!("u{i:02}")).collect();
        let v = json!({"groups": {"g": {"n": "big", "u": many, "t": 1}}});
        let b = MentionGroupBlob::from_value_at(Some(&v), NOW);
        assert_eq!(b.groups["g"].members.len(), MAX_GROUP_MEMBERS);
        assert!(MentionGroupBlob::from_value_at(Some(&json!([1])), NOW).is_empty());
        assert!(MentionGroupBlob::from_value_at(None, NOW).is_empty());
    }

    #[test]
    fn last_writer_wins_per_group() {
        let a = blob(&[upsert("g1", "devs", &["bob"])], 10, "A");
        let b = blob(&[upsert("g1", "devs", &["carol"])], 10, "B");
        let c = blob(&[upsert("g2", "qa", &["dave"])], 5, "A");
        let all = [&a, &b, &c];
        let mut results = Vec::new();
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
            let mut m = MentionGroupBlob::default();
            for i in order {
                m.merge(all[i]);
                m.merge(all[i]);
            }
            results.push(m);
        }
        assert!(results.windows(2).all(|w| w[0] == w[1]));
        let v = results[0].view();
        assert_eq!(
            v.get("devs").unwrap().members,
            ["carol"],
            "device B wins the tie"
        );
        assert!(v.get("qa").is_some());
        // A delete stamped later wins over an older edit.
        let mut d = a.clone();
        d.edit(
            &MentionGroupBlob::default(),
            &MentionGroupEdit::Delete { id: "g1".into() },
            20,
            "A",
        )
        .unwrap();
        let mut m = d.clone();
        m.merge(&b);
        assert!(m.view().get("devs").is_none());
    }

    #[test]
    fn duplicate_names_shadow_the_older() {
        let a = blob(&[upsert("g1", "devs", &["bob"])], 10, "A");
        let b = blob(&[upsert("g2", "devs", &["carol"])], 20, "B");
        let v = a.merged_with(&b).view();
        assert_eq!(v.get("devs").unwrap().id, "g2");
        assert_eq!(v.all().len(), 2);
        assert!(v.by_id("g1").unwrap().shadowed);
        assert_eq!(v.matching("de").count(), 1);
        // Either copy stays editable under its name; neither may take the
        // other's name by a rename.
        let stored = a.merged_with(&b);
        let mut o = MentionGroupBlob::default();
        let e = upsert("g2", "devs", &["carol", "dave"]);
        assert_eq!(o.edit(&stored, &e, 30, "A"), Ok(true));
        let e = upsert("g1", "devs", &["bob", "erin"]);
        assert_eq!(o.edit(&stored, &e, 40, "A"), Ok(true));
        let e = upsert("g3", "qa", &["bob"]);
        assert_eq!(o.edit(&stored, &e, 50, "A"), Ok(true));
        let e = upsert("g3", "devs", &["bob"]);
        assert_eq!(
            o.edit(&stored, &e, 60, "A"),
            Err(MentionGroupError::Duplicate)
        );
    }

    #[test]
    fn tombstones_expire_when_written() {
        let mut b = blob(&[upsert("g1", "devs", &["bob"])], 10, "A");
        b.edit(
            &MentionGroupBlob::default(),
            &MentionGroupEdit::Delete { id: "g1".into() },
            20,
            "A",
        )
        .unwrap();
        assert!(b.to_value_at(30)["groups"].get("g1").is_some());
        let later = 20 + TOMBSTONE_TTL_MS + 1;
        assert!(b.to_value_at(later)["groups"].get("g1").is_none());
        // An expired pending removal counts as stored once nothing holds it.
        assert!(MentionGroupBlob::default().covers_at(&b, later));
        assert!(!MentionGroupBlob::default().covers_at(&b, 30));
        let mut overlay = b.clone();
        assert!(overlay.prune_covered_at(&MentionGroupBlob::default(), later));
        assert!(overlay.is_empty());
        let stored = blob(&[upsert("g1", "devs", &["bob"])], 10, "A");
        assert_eq!(b.uncovered_by_at(&stored, 30).len(), 1);
    }

    #[test]
    fn expands_to_the_group_name_and_its_members() {
        let g = groups(&[("backend", &["alice", "bob"]), ("qa", &["bob", "carol"])]);
        assert_eq!(
            run("hi @backend", &g, None).as_deref(),
            Some("hi @backend (@alice @bob)")
        );
        assert_eq!(
            run("@Backend.", &g, None).as_deref(),
            Some("@Backend (@alice @bob).")
        );
        assert_eq!(
            run("@backend: see this", &g, None).as_deref(),
            Some("@backend (@alice @bob): see this")
        );
        // Quotes and parentheses are boundaries.
        assert_eq!(
            run(">@backend", &g, None).as_deref(),
            Some(">@backend (@alice @bob)")
        );
        assert_eq!(
            run("(@qa)", &g, None).as_deref(),
            Some("(@qa (@bob @carol))")
        );
        // Overlapping groups and explicit mentions: nobody twice.
        assert_eq!(
            run("@backend @qa", &g, None).as_deref(),
            Some("@backend (@alice @bob) @qa (@carol)")
        );
        assert_eq!(
            run("@backend and @alice", &g, None).as_deref(),
            Some("@backend (@bob) and @alice")
        );
        // The sender is dropped; nobody left: unchanged.
        assert_eq!(
            run("@backend", &g, Some("Alice")).as_deref(),
            Some("@backend (@bob)")
        );
        assert_eq!(run("@backend @bob", &g, Some("alice")), None);
        let solo = groups(&[("me-only", &["alice"])]);
        assert_eq!(run("@me-only", &solo, Some("alice")), None);
    }

    #[test]
    fn expansion_is_idempotent() {
        let g = groups(&[("backend", &["alice", "bob"]), ("qa", &["carol"])]);
        let once = run("ping @backend and @qa.", &g, None).unwrap();
        assert_eq!(once, "ping @backend (@alice @bob) and @qa (@carol).");
        assert_eq!(run(&once, &g, None), None);
        // An edit adding a group expands only the new one.
        let edited = format!("{once} cc @qa");
        assert_eq!(run(&edited, &g, None), None, "carol is mentioned already");
        let g2 = groups(&[("backend", &["alice", "bob", "dave"]), ("qa", &["carol"])]);
        assert_eq!(run(&once, &g2, None), None, "an existing list is kept");
        assert_eq!(
            existing_expansion(" ( @a, @b )!"),
            Some(" ( @a, @b )".len())
        );
        assert_eq!(existing_expansion(" (note)"), None);
        assert_eq!(existing_expansion(" ()"), None);
        assert_eq!(existing_expansion("\n(@a)"), None);
    }

    #[test]
    fn code_links_users_and_non_boundaries_are_left_alone() {
        let g = groups(&[("backend", &["alice", "bob"])]);
        for text in [
            "`@backend`",
            "```\n@backend\n```",
            "[@backend](http://x)",
            "[see](http://x/@backend)",
            "mail a@backend.com",
            "x`y @backend",
            "@backend@remote.org",
            "@backends",
            "f(@backend x)",
        ] {
            assert_eq!(run(text, &g, None), None, "{text}");
        }
        assert_eq!(
            run("`code` @backend", &g, None).as_deref(),
            Some("`code` @backend (@alice @bob)")
        );
        assert_eq!(
            run("```\nx\n```\n@backend", &g, None).as_deref(),
            Some("```\nx\n```\n@backend (@alice @bob)")
        );
        // A real user with the group's name wins.
        let is_user = |u: &str| u == "backend";
        let opts = ExpandOptions {
            me: None,
            is_user: &is_user,
        };
        assert_eq!(expand("@backend", &g, &opts), None);
        // No groups or no token: nothing to do.
        assert_eq!(run("hello", &g, None), None);
        assert_eq!(run("@backend", &MentionGroups::default(), None), None);
    }

    #[test]
    fn reports_what_was_added() {
        let g = groups(&[("backend", &["alice", "bob"])]);
        let opts = ExpandOptions {
            me: None,
            is_user: &no_users,
        };
        let e = expand("@backend [x](http://y/@z) @all", &g, &opts).unwrap();
        assert_eq!(e.added, ["alice", "bob"]);
        // @backend, @alice, @bob, @all (not the one in the link).
        assert_eq!(e.mention_tokens, 4);
    }
}
