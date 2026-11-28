//! Raw values as readers produce them and their coercion to a key's type
//! (docs/policy.md section 4).

use std::collections::HashSet;
use std::fmt;

use super::registry::ValueType;
use super::servers::ServerItemError;

/// Longest display form of a value in the report, in characters.
pub const DISPLAY_MAX_CHARS: usize = 200;

/// Longest piece of raw text embedded in an error or a dropped item.
pub(crate) const EMBED_MAX_CHARS: usize = 80;

/// A value as a source wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawValue {
    Bool(bool),
    Integer(i64),
    String(String),
    List(Vec<RawValue>),
    /// A native value the reader cannot map (a JSON float, object or null,
    /// a top-level name a JSON file repeats); the text names its kind.
    /// Always invalid, so the key's strict value applies instead of the
    /// value silently disappearing.
    Unsupported(String),
}

impl RawValue {
    /// Kind of the value for error messages.
    pub fn kind(&self) -> &str {
        match self {
            RawValue::Bool(_) => "boolean",
            RawValue::Integer(_) => "integer",
            RawValue::String(_) => "string",
            RawValue::List(_) => "list",
            RawValue::Unsupported(kind) => kind,
        }
    }

    /// Display form for the report, at most [`DISPLAY_MAX_CHARS`] characters.
    pub fn display(&self) -> String {
        truncate(&self.render(), DISPLAY_MAX_CHARS)
    }

    fn render(&self) -> String {
        match self {
            RawValue::Bool(b) => b.to_string(),
            RawValue::Integer(i) => i.to_string(),
            RawValue::String(s) => quote(s),
            RawValue::List(items) => {
                let items: Vec<String> = items.iter().map(RawValue::render).collect();
                format!("[{}]", items.join(", "))
            }
            RawValue::Unsupported(kind) => format!("<{kind}>"),
        }
    }
}

/// `s` quoted with JSON string escaping: stable across toolchains, and
/// non-ASCII text stays readable. Characters that break a single line
/// (controls, line separators, bidirectional overrides) that JSON would
/// leave raw are escaped as `\uXXXX` as well.
pub(crate) fn quote(s: &str) -> String {
    let json = serde_json::to_string(s).unwrap_or_else(|_| String::from("\"?\""));
    display_text(&json)
}

/// `s` for display on one line, unquoted: the characters [`quote`] escapes
/// beyond JSON (controls, line separators, bidirectional overrides) become
/// `\uXXXX`, so a key or file name cannot span lines or reorder the text
/// around it.
pub fn display_text(s: &str) -> String {
    if !s.chars().any(super::resolve::breaks_single_line) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if super::resolve::breaks_single_line(c) {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// `s` cut to `max` characters, ending with `…` when cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A raw value coerced to its key's type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coerced {
    Bool(bool),
    /// The canonical (lower-case) variant name.
    Enum(&'static str),
    /// Trimmed, not empty.
    String(String),
    /// Trimmed, non-empty items in their order, exact duplicates dropped.
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoerceError {
    #[error("expected {expected}, found {found}")]
    WrongType {
        expected: &'static str,
        found: String,
    },
    #[error("integer {0} is not a boolean (use 0 or 1)")]
    NotBoolInteger(i64),
    #[error("{} is not one of: {}", quote(.value), .variants.join(", "))]
    UnknownVariant {
        /// The value as written, truncated.
        value: String,
        variants: &'static [&'static str],
    },
    #[error("empty string")]
    Empty,
}

/// Why a list item was dropped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DropReason {
    #[error("empty string")]
    Empty,
    #[error("expected a string, found {found}")]
    NotAString { found: String },
    /// A `ManagedServers` or `AllowedServers` item that does not parse.
    #[error(transparent)]
    Server(#[from] ServerItemError),
    /// A `ManagedServers` item whose normalised URL an earlier item has.
    #[error("duplicate of an earlier server")]
    DuplicateServer,
}

impl DropReason {
    /// Every code [`Self::code`] returns.
    pub const CODES: [&'static str; 9] = [
        "empty",
        "not_a_string",
        "missing_scheme",
        "invalid_address",
        "unsupported_scheme",
        "extra_parts",
        "invalid_port",
        "bad_wildcard",
        "duplicate_server",
    ];

    /// Stable short code, for the diagnostics page (the text can quote the
    /// item).
    pub fn code(&self) -> &'static str {
        match self {
            DropReason::Empty => "empty",
            DropReason::NotAString { .. } => "not_a_string",
            DropReason::Server(e) => e.code(),
            DropReason::DuplicateServer => "duplicate_server",
        }
    }
}

/// A list item dropped while applying a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedItem {
    /// 1-based position in the list as written.
    pub position: usize,
    /// Display form of the item, truncated.
    pub item: String,
    pub reason: DropReason,
}

impl fmt::Display for DroppedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "item {}: {}", self.position, self.reason)
    }
}

/// A coerced value and the list items that were dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coercion {
    pub value: Coerced,
    pub dropped: Vec<DroppedItem>,
    /// For a list: the 1-based position in the list as written of each kept
    /// item, parallel to [`Coerced::List`]. Empty otherwise.
    pub positions: Vec<usize>,
}

/// Coerces `raw` to `ty` following section 4.
pub fn coerce(ty: ValueType, raw: &RawValue) -> Result<Coercion, CoerceError> {
    let plain = |value| Coercion {
        value,
        dropped: Vec::new(),
        positions: Vec::new(),
    };
    let wrong = |expected| CoerceError::WrongType {
        expected,
        found: raw.kind().to_owned(),
    };
    match (ty, raw) {
        (ValueType::Bool, RawValue::Bool(b)) => Ok(plain(Coerced::Bool(*b))),
        (ValueType::Bool, RawValue::Integer(0)) => Ok(plain(Coerced::Bool(false))),
        (ValueType::Bool, RawValue::Integer(1)) => Ok(plain(Coerced::Bool(true))),
        (ValueType::Bool, RawValue::Integer(i)) => Err(CoerceError::NotBoolInteger(*i)),
        (ValueType::Bool, _) => Err(wrong("a boolean")),
        (ValueType::Enum(variants), RawValue::String(s)) => variants
            .iter()
            .find(|v| v.eq_ignore_ascii_case(s))
            .map(|v| plain(Coerced::Enum(v)))
            .ok_or_else(|| CoerceError::UnknownVariant {
                value: truncate(s, EMBED_MAX_CHARS),
                variants,
            }),
        (ValueType::Enum(_), _) => Err(wrong("a string")),
        (ValueType::String, RawValue::String(s)) => match s.trim() {
            "" => Err(CoerceError::Empty),
            t => Ok(plain(Coerced::String(t.to_owned()))),
        },
        (ValueType::String, _) => Err(wrong("a string")),
        (ValueType::StringList, RawValue::String(_)) => Ok(list(std::iter::once((0, raw)), 1)),
        (ValueType::StringList, RawValue::List(items)) => {
            Ok(list(items.iter().enumerate(), items.len()))
        }
        (ValueType::StringList, _) => Err(wrong("a list of strings")),
    }
}

fn list<'a>(items: impl Iterator<Item = (usize, &'a RawValue)>, capacity: usize) -> Coercion {
    let mut out: Vec<String> = Vec::with_capacity(capacity.min(64));
    let mut positions = Vec::with_capacity(capacity.min(64));
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dropped = Vec::new();
    for (i, item) in items {
        let reason = match item {
            RawValue::String(s) if !s.trim().is_empty() => {
                let t = s.trim();
                if seen.insert(t) {
                    out.push(t.to_owned());
                    positions.push(i + 1);
                }
                continue;
            }
            RawValue::String(_) => DropReason::Empty,
            other => DropReason::NotAString {
                found: other.kind().to_owned(),
            },
        };
        dropped.push(DroppedItem {
            position: i + 1,
            item: truncate(&item.render(), EMBED_MAX_CHARS),
            reason,
        });
    }
    Coercion {
        value: Coerced::List(out),
        dropped,
        positions,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::registry::UpdateMode;

    fn s(v: &str) -> RawValue {
        RawValue::String(v.to_owned())
    }

    fn ok(ty: ValueType, raw: RawValue) -> Coerced {
        coerce(ty, &raw).unwrap().value
    }

    #[test]
    fn bool_rules() {
        let b = ValueType::Bool;
        assert_eq!(ok(b, RawValue::Bool(true)), Coerced::Bool(true));
        assert_eq!(ok(b, RawValue::Bool(false)), Coerced::Bool(false));
        assert_eq!(ok(b, RawValue::Integer(0)), Coerced::Bool(false));
        assert_eq!(ok(b, RawValue::Integer(1)), Coerced::Bool(true));
        for bad in [
            RawValue::Integer(2),
            RawValue::Integer(-1),
            s("true"),
            s("1"),
            RawValue::List(vec![RawValue::Bool(true)]),
            RawValue::Unsupported("float".into()),
        ] {
            assert!(coerce(b, &bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            coerce(b, &RawValue::Integer(2)),
            Err(CoerceError::NotBoolInteger(2))
        );
    }

    #[test]
    fn enum_rules() {
        let e = ValueType::Enum(UpdateMode::NAMES);
        assert_eq!(ok(e, s("off")), Coerced::Enum("off"));
        assert_eq!(ok(e, s("Notify")), Coerced::Enum("notify"));
        assert_eq!(ok(e, s("PROMPT")), Coerced::Enum("prompt"));
        for bad in [
            s(" off"),
            s("of"),
            s(""),
            s("offf"),
            RawValue::Integer(0),
            RawValue::Bool(false),
            RawValue::List(vec![s("off")]),
        ] {
            assert!(coerce(e, &bad).is_err(), "{bad:?}");
        }
        // Only ASCII case folds: the dotless i is not `i`.
        assert!(coerce(e, &s("notıfy")).is_err());
    }

    #[test]
    fn string_rules() {
        let t = ValueType::String;
        assert_eq!(ok(t, s("  Acme  ")), Coerced::String("Acme".into()));
        assert_eq!(coerce(t, &s("   ")), Err(CoerceError::Empty));
        assert_eq!(coerce(t, &s("")), Err(CoerceError::Empty));
        assert!(coerce(t, &RawValue::Integer(1)).is_err());
        assert!(coerce(t, &RawValue::List(vec![s("x")])).is_err());
        assert!(coerce(t, &RawValue::Bool(true)).is_err());
    }

    #[test]
    fn list_rules() {
        let l = ValueType::StringList;
        // A single string is a one-item list.
        assert_eq!(ok(l, s(" a ")), Coerced::List(vec!["a".into()]));
        assert_eq!(ok(l, RawValue::List(vec![])), Coerced::List(vec![]));
        // Order kept, exact duplicates dropped (after trimming), case kept.
        let c = coerce(
            l,
            &RawValue::List(vec![s("b"), s("a"), s("b"), s(" a"), s("A")]),
        )
        .unwrap();
        assert_eq!(
            c.value,
            Coerced::List(vec!["b".into(), "a".into(), "A".into()])
        );
        assert!(c.dropped.is_empty());
        assert_eq!(c.positions, [1, 2, 5]);
        // Bad items are dropped and reported, the rest stays.
        let c = coerce(
            l,
            &RawValue::List(vec![
                s("x"),
                RawValue::Integer(3),
                s("  "),
                RawValue::List(vec![s("nested")]),
                s("y"),
            ]),
        )
        .unwrap();
        assert_eq!(c.value, Coerced::List(vec!["x".into(), "y".into()]));
        let dropped: Vec<String> = c.dropped.iter().map(ToString::to_string).collect();
        assert_eq!(
            dropped,
            [
                "item 2: expected a string, found integer",
                "item 3: empty string",
                "item 4: expected a string, found list",
            ]
        );
        assert_eq!(c.dropped[0].item, "3");
        assert_eq!(c.dropped[1].item, "\"  \"");
        assert_eq!(c.positions, [1, 5]);
        let c = coerce(l, &s("")).unwrap();
        assert_eq!(c.value, Coerced::List(vec![]));
        assert_eq!(c.dropped.len(), 1);
        for bad in [
            RawValue::Integer(1),
            RawValue::Bool(true),
            RawValue::Unsupported("object".into()),
        ] {
            assert!(coerce(l, &bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn long_lists_are_fast() {
        // About what fits in a 1 MiB file; quadratic deduplication would take
        // seconds here.
        let items: Vec<RawValue> = (0..120_000).map(|i| s(&format!("a{i:06}"))).collect();
        let c = coerce(ValueType::StringList, &RawValue::List(items)).unwrap();
        assert!(matches!(c.value, Coerced::List(ref l) if l.len() == 120_000));
    }

    #[test]
    fn errors_embed_truncated_text() {
        let e = ValueType::Enum(UpdateMode::NAMES);
        let err = coerce(e, &s(&"x".repeat(10_000))).unwrap_err();
        assert!(err.to_string().chars().count() < 200, "{err}");
    }

    #[test]
    fn unknown_variant_is_quoted_stably() {
        let e = ValueType::Enum(UpdateMode::NAMES);
        let err = coerce(e, &s("выкл\u{7}")).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#""выкл\u0007" is not one of: prompt, notify, off"#
        );
    }

    #[test]
    fn display_escapes_strings_stably() {
        assert_eq!(s("Привет \"x\"\n\t").display(), r#""Привет \"x\"\n\t""#);
        assert_eq!(s("\u{1}").display(), r#""\u0001""#);
    }

    /// The characters the `--dump-policy` doc lists as escaped beyond JSON.
    #[test]
    fn display_escapes_what_json_leaves_raw() {
        let raw = "\u{7f}\u{80}\u{85}\u{9f}\u{2028}\u{2029}\u{61c}\u{200e}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}";
        assert_eq!(
            s(raw).display(),
            r#""\u007f\u0080\u0085\u009f\u2028\u2029\u061c\u200e\u200f\u202a\u202e\u2066\u2069""#
        );
        assert_eq!(s("\u{a0}\u{200b}é").display(), "\"\u{a0}\u{200b}é\"");
    }

    #[test]
    fn display_is_truncated() {
        assert_eq!(s("a").display(), "\"a\"");
        assert_eq!(RawValue::Integer(-5).display(), "-5");
        assert_eq!(
            RawValue::List(vec![s("a"), RawValue::Bool(true)]).display(),
            "[\"a\", true]"
        );
        assert_eq!(RawValue::Unsupported("null".into()).display(), "<null>");
        let long = s(&"x".repeat(500)).display();
        assert_eq!(long.chars().count(), DISPLAY_MAX_CHARS);
        assert!(long.ends_with('…'));
    }
}
