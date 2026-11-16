//! Local matching of a [`SearchQuery`] (offline cache, loaded timelines)
//! and match highlighting.
//!
//! Locally every word of the free text must occur (case-insensitive
//! substring), which is stricter than the server's `$text` search (any
//! word, stemmed): local hits are a fast preview, the server's are the
//! answer. Operators follow the server's semantics; a query with an
//! operator this module cannot evaluate (`has:location`, `label:`,
//! `file-desc:`, `file-title:`) has no local matcher.

use std::ops::Range;

use chrono::{FixedOffset, NaiveDate, TimeZone};
use regex::Regex;
use rocketchat::models::Message;

use super::query::{Has, SearchQuery};

/// Who "me" is, and the day boundaries' time zone.
#[derive(Debug, Clone)]
pub struct MatchCtx {
    pub user_id: String,
    pub username: String,
    pub offset: FixedOffset,
}

/// A query compiled for local matching.
#[derive(Debug, Clone)]
pub struct LocalMatcher {
    words: Vec<String>,
    regex: Option<Regex>,
    from: Vec<String>,
    mentions: Vec<String>,
    has: Vec<Has>,
    /// `ts` bounds in ms: `ts >= min`, `ts <= max`, `ts < below`.
    min: Option<i64>,
    max: Option<i64>,
    below: Option<i64>,
    user_id: String,
}

fn day_start(d: NaiveDate, offset: FixedOffset) -> Option<i64> {
    let t = d.and_hms_opt(0, 0, 0)?;
    offset
        .from_local_datetime(&t)
        .single()
        .map(|t| t.timestamp_millis())
}

impl LocalMatcher {
    /// `None`: the query is empty, or has an operator that cannot be
    /// evaluated here, or an invalid regex.
    pub fn new(q: &SearchQuery, ctx: &MatchCtx) -> Option<Self> {
        if q.is_empty() || q.has(Has::Location) || !q.other.iter().all(|o| is_order(o)) {
            return None;
        }
        let regex = match q.regex() {
            Some((pattern, flags)) => {
                let mut b = regex::RegexBuilder::new(pattern);
                b.case_insensitive(flags.contains('i'))
                    .multi_line(flags.contains('m'))
                    .ignore_whitespace(flags.contains('x'))
                    .dot_matches_new_line(flags.contains('s'))
                    .size_limit(1 << 20);
                Some(b.build().ok()?)
            }
            None => None,
        };
        let me = ctx.username.to_lowercase();
        let lower = |list: &[String]| -> Vec<String> {
            list.iter()
                .map(|u| {
                    if u.eq_ignore_ascii_case("me") && !me.is_empty() {
                        me.clone()
                    } else {
                        u.to_lowercase()
                    }
                })
                .collect()
        };
        let (mut min, mut max, mut below) = (None, None, None);
        if let Some(d) = q.after {
            min = day_start(d.succ_opt()?, ctx.offset);
        }
        if let Some(d) = q.before {
            max = day_start(d, ctx.offset);
        }
        if let Some(d) = q.on {
            // The server's `on:` replaces the other bounds.
            min = day_start(d, ctx.offset);
            max = None;
            below = day_start(d.succ_opt()?, ctx.offset);
        }
        Some(Self {
            words: q.words(),
            regex,
            from: lower(&q.from),
            mentions: lower(&q.mentions),
            has: q.has.clone(),
            min,
            max,
            below,
            user_id: ctx.user_id.clone(),
        })
    }

    /// Whether `m` matches. System messages never do; the caller decides
    /// about encrypted ones (only decrypted text may be searched).
    pub fn matches(&self, m: &Message) -> bool {
        if m.is_system() || m.id.is_empty() {
            return false;
        }
        let ts = m.ts.map_or(0, |d| d.timestamp_millis());
        if self.min.is_some_and(|min| ts < min)
            || self.max.is_some_and(|max| ts > max)
            || self.below.is_some_and(|b| ts >= b)
        {
            return false;
        }
        let contains_any = |names: &[String], value: Option<&str>| {
            let v = value.unwrap_or_default().to_lowercase();
            names.iter().any(|n| v.contains(n.as_str()))
        };
        if !self.from.is_empty() && !contains_any(&self.from, m.u.username.as_deref()) {
            return false;
        }
        if !self.mentions.is_empty()
            && !m
                .mentions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|x| contains_any(&self.mentions, x.username.as_deref()))
        {
            return false;
        }
        for h in &self.has {
            let ok = match h {
                Has::Link => m.urls.as_ref().is_some_and(|u| !u.is_empty()),
                Has::Star => m.is_starred_by(&self.user_id),
                Has::Pin => m.pinned == Some(true),
                Has::Location => false,
            };
            if !ok {
                return false;
            }
        }
        if let Some(r) = &self.regex {
            return r.is_match(&m.msg);
        }
        if self.words.is_empty() {
            return true;
        }
        let text = m.msg.to_lowercase();
        self.words.iter().all(|w| text.contains(w.as_str()))
    }
}

fn is_order(o: &str) -> bool {
    let o = o.to_ascii_lowercase();
    o.starts_with("order:") || o.starts_with("sort:")
}

/// Byte ranges of `text` where any of `words` (lowercase) occurs,
/// case-insensitively, sorted and merged.
pub fn highlight(text: &str, words: &[String]) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char)> = text
        .char_indices()
        .map(|(i, c)| (i, c.to_lowercase().next().unwrap_or(c)))
        .collect();
    let mut found: Vec<Range<usize>> = Vec::new();
    for w in words {
        let w: Vec<char> = w.chars().collect();
        if w.is_empty() || w.len() > chars.len() {
            continue;
        }
        let mut i = 0;
        while i + w.len() <= chars.len() {
            if chars[i..i + w.len()]
                .iter()
                .map(|(_, c)| *c)
                .eq(w.iter().copied())
            {
                let start = chars[i].0;
                let end = chars.get(i + w.len()).map_or(text.len(), |(b, _)| *b);
                found.push(start..end);
                i += w.len();
            } else {
                i += 1;
            }
        }
    }
    found.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in found {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// One line of `text` around its first match, at most about `max` chars,
/// with `…` where it was cut, and the matches inside it.
pub fn snippet(text: &str, words: &[String], max: usize) -> (String, Vec<Range<usize>>) {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let hits = highlight(&flat, words);
    let count = flat.chars().count();
    if count <= max {
        return (flat, hits);
    }
    // Start a little before the first match, on a char boundary.
    let first = hits.first().map_or(0, |r| flat[..r.start].chars().count());
    let start_char = first.saturating_sub(max / 4);
    let start_char = start_char.min(count.saturating_sub(max));
    let byte_at = |n: usize| flat.char_indices().nth(n).map_or(flat.len(), |(b, _)| b);
    let (from, to) = (byte_at(start_char), byte_at(start_char + max));
    let mut out = String::new();
    let mut shift = 0usize;
    if from > 0 {
        out.push('…');
        shift = '…'.len_utf8();
    }
    out.push_str(&flat[from..to]);
    if to < flat.len() {
        out.push('…');
    }
    let ranges = hits
        .into_iter()
        .filter(|r| r.start >= from && r.end <= to)
        .map(|r| r.start - from + shift..r.end - from + shift)
        .collect();
    (out, ranges)
}
