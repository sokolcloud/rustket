//! The search query string: Rocket.Chat's default provider operators
//! (`apps/meteor/server/lib/parseMessageSearchQuery.ts`, 8.9.1) plus the
//! client-side `in:` room filter.
//!
//! Operators the server parses (anywhere in the text, consumed in this
//! order; what is left is the free text):
//! - `from:<username>` (several: any of them; `from:me`), a
//!   case-insensitive regex on the author's username;
//! - `mention:<username>`, likewise on the mentioned usernames;
//! - `has:star` (starred by me), `has:url` / `has:link`, `is:pinned` /
//!   `has:pin`, `has:location` / `has:map`;
//! - `label:"…"`, `file-desc:"…"`, `file-title:"…"` (first attachment's
//!   labels, attachment description / title);
//! - `before:D/M/YYYY`, `after:D/M/YYYY`, `on:D/M/YYYY` (separators `/`,
//!   `.` or `-`; day first; the user's UTC offset). Both `before` and
//!   `after` exclude the day itself: `before:` is "until the start of that
//!   day", `after:` "from the start of the next day";
//! - `order:asc|desc` (`sort:`).
//!
//! The free text is a MongoDB `$text` search (whole words, any of them),
//! or a regex when written `/…/flags` (or when `Message_AlwaysSearchRegExp`
//! is on). There is no `has:file` or `in:` on the server: `in:` is applied
//! here by choosing the rooms searched.

use chrono::NaiveDate;

/// A `has:` / `is:` filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Has {
    /// `has:link` (`has:url`).
    Link,
    /// `has:star`: starred by the current user.
    Star,
    /// `is:pinned` (`has:pin`).
    Pin,
    /// `has:location` (`has:map`).
    Location,
}

impl Has {
    pub const ALL: [Has; 4] = [Has::Link, Has::Star, Has::Pin, Has::Location];

    /// The operator as written by [`SearchQuery::to_query`].
    pub fn operator(self) -> &'static str {
        match self {
            Has::Link => "has:link",
            Has::Star => "has:star",
            Has::Pin => "is:pinned",
            Has::Location => "has:location",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "has:link" | "has:url" => Has::Link,
            "has:star" => Has::Star,
            "is:pinned" | "has:pin" => Has::Pin,
            "has:location" | "has:map" => Has::Location,
            _ => return None,
        })
    }
}

/// A parsed search: what the filter builder edits. The query string is the
/// source of truth; the builder parses it, changes one part and writes it
/// back with [`SearchQuery::to_query`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// Free text: words, or a `/regex/flags`.
    pub text: String,
    /// `from:` usernames (`me` allowed).
    pub from: Vec<String>,
    /// `mention:` usernames.
    pub mentions: Vec<String>,
    /// `in:` room names (client-side; without `#`).
    pub rooms: Vec<String>,
    pub has: Vec<Has>,
    pub after: Option<NaiveDate>,
    pub before: Option<NaiveDate>,
    pub on: Option<NaiveDate>,
    /// Server operators kept verbatim (`label:`, `file-desc:`,
    /// `file-title:`, `order:`).
    pub other: Vec<String>,
}

/// A username as the server's `from:` / `mention:` accepts it.
fn is_username(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `D/M/YYYY` with `/`, `.` or `-` (the server's date operators).
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let parts: Vec<&str> = s.split(['/', '.', '-']).collect();
    let [d, m, y] = parts.as_slice() else {
        return None;
    };
    let ok = |p: &str, n: std::ops::RangeInclusive<usize>| {
        n.contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit())
    };
    if !ok(d, 1..=2) || !ok(m, 1..=2) || !ok(y, 4..=4) {
        return None;
    }
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
}

/// A date as the date operators take it (`16/11/2026`).
pub fn format_date(d: NaiveDate) -> String {
    d.format("%d/%m/%Y").to_string()
}

/// Splits on whitespace, keeping `"…"` runs (with spaces) inside a token.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn push_unique(list: &mut Vec<String>, v: &str) {
    if !list.iter().any(|x| x.eq_ignore_ascii_case(v)) {
        list.push(v.to_owned());
    }
}

/// A token written with quotes when it has spaces.
fn quote(prefix: &str, v: &str) -> String {
    if v.contains(char::is_whitespace) {
        format!("{prefix}\"{v}\"")
    } else {
        format!("{prefix}{v}")
    }
}

impl SearchQuery {
    /// Parses a query string. Tokens that look like operators but do not
    /// parse (`from:` with no name, a bad date) stay in the free text, as
    /// on the server.
    pub fn parse(s: &str) -> Self {
        let mut q = SearchQuery::default();
        let mut words: Vec<String> = Vec::new();
        let trimmed = s.trim();
        // A whole-query regex is free text as is.
        if trimmed.starts_with('/') && trimmed.len() > 1 && is_regex(trimmed) {
            q.text = trimmed.to_owned();
            return q;
        }
        for tok in tokens(s) {
            let lower = tok.to_ascii_lowercase();
            let value = |p: &str| tok.get(p.len()..).filter(|_| lower.starts_with(p));
            if let Some(h) = Has::parse(&lower) {
                if !q.has.contains(&h) {
                    q.has.push(h);
                }
            } else if let Some(u) = value("from:").filter(|u| is_username(u)) {
                push_unique(&mut q.from, u);
            } else if let Some(u) = value("mention:").filter(|u| is_username(u)) {
                push_unique(&mut q.mentions, u);
            } else if let Some(r) = value("in:") {
                let r = r.trim_start_matches('#').trim_matches('"');
                if r.is_empty() {
                    words.push(tok);
                } else {
                    push_unique(&mut q.rooms, r);
                }
            } else if let Some(d) = value("after:").and_then(parse_date) {
                q.after = Some(d);
            } else if let Some(d) = value("before:").and_then(parse_date) {
                q.before = Some(d);
            } else if let Some(d) = value("on:").and_then(parse_date) {
                q.on = Some(d);
            } else if ["label:", "file-desc:", "file-title:", "order:", "sort:"]
                .iter()
                .any(|p| lower.starts_with(p) && tok.len() > p.len())
            {
                q.other.push(tok);
            } else {
                words.push(tok);
            }
        }
        q.text = words.join(" ");
        q
    }

    /// Nothing to search for (the server answers an empty query with
    /// nothing).
    pub fn is_empty(&self) -> bool {
        self.server_query().trim().is_empty()
    }

    /// The query string with every part (what the search field shows).
    pub fn to_query(&self) -> String {
        let mut parts = self.operator_parts();
        parts.extend(self.rooms.iter().map(|r| quote("in:#", r)));
        if !self.text.trim().is_empty() {
            parts.push(self.text.trim().to_owned());
        }
        parts.join(" ")
    }

    /// The string sent to the server: everything but `in:`.
    pub fn server_query(&self) -> String {
        let mut parts = self.operator_parts();
        if !self.text.trim().is_empty() {
            parts.push(self.text.trim().to_owned());
        }
        parts.join(" ")
    }

    fn operator_parts(&self) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.from.iter().map(|u| format!("from:{u}")));
        parts.extend(self.mentions.iter().map(|u| format!("mention:{u}")));
        parts.extend(self.has.iter().map(|h| h.operator().to_owned()));
        if let Some(d) = self.after {
            parts.push(format!("after:{}", format_date(d)));
        }
        if let Some(d) = self.before {
            parts.push(format!("before:{}", format_date(d)));
        }
        if let Some(d) = self.on {
            parts.push(format!("on:{}", format_date(d)));
        }
        parts.extend(self.other.iter().cloned());
        parts
    }

    /// Words of the free text to highlight (none for a regex).
    pub fn words(&self) -> Vec<String> {
        if is_regex(self.text.trim()) {
            return Vec::new();
        }
        self.text
            .split_whitespace()
            .map(|w| w.trim_matches('"').to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// The free text's regex and flags when written `/…/flags`.
    pub fn regex(&self) -> Option<(&str, &str)> {
        let t = self.text.trim();
        if !is_regex(t) {
            return None;
        }
        let end = t.rfind('/')?;
        Some((&t[1..end], &t[end + 1..]))
    }

    pub fn has(&self, h: Has) -> bool {
        self.has.contains(&h)
    }

    pub fn set_has(&mut self, h: Has, on: bool) {
        self.has.retain(|x| *x != h);
        if on {
            self.has.push(h);
            self.has.sort();
        }
    }
}

/// `/…/flags` with flags among `imxs` (the server's test).
fn is_regex(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('/') else {
        return false;
    };
    let Some(end) = rest.rfind('/') else {
        return false;
    };
    end > 0 && rest[end + 1..].chars().all(|c| "imxs".contains(c))
}
