//! The `--dump-policy` JSON (docs/policy.md section 10).
//!
//! Shape, format 1 (fields are only ever added; a change of meaning raises
//! [`DUMP_FORMAT`]):
//!
//! ```text
//! {
//!   "format": 1,
//!   "client_version": "0.4.0",
//!   "blocked": true,
//!   "blocking": [
//!     {"kind": "source_unreadable", "source": "linux:x.json",
//!      "error": {"code": "invalid_json", "detail": "EOF while parsing…",
//!                "text": "invalid JSON: EOF while parsing…"}},
//!     {"kind": "below_minimum_version", "required": "1.0.0", "client": "0.4.0"}
//!   ],
//!   "sources": [
//!     {"id": "linux:x.json", "trust": "limited",
//!      "trust_problems": [{"code": "file_writable",
//!                          "text": "the file is writable by group or others"}],
//!      "error": null}
//!   ],
//!   "rows": [
//!     {"key": "AllowedServers", "source": "windows:HKLM", "status": "applied",
//!      "reason": null, "strict_applied": false, "value": "[\"a.com\", 7]",
//!      "dropped": [{"position": 2, "item": "7",
//!                   "reason": "expected a string, found integer"}],
//!      "dropped_omitted": 0}
//!   ]
//! }
//! ```
//!
//! `error` is `null` or an object: `code` is one of `cannot_open`,
//! `cannot_read_metadata`, `cannot_read`, `too_large`, `invalid_json`,
//! `not_an_object`, `cannot_list_directory`, `unstable`; `detail` is the operating
//! system's or parser's own text (or `null`); `text` is the English message.
//! `trust_problems` is empty for full trust and for a source that is limited
//! by itself (`HKCU`); `code` is one of `file_wrong_owner`,
//! `directory_wrong_owner`, `file_writable`, `directory_writable`,
//! `directory_changed`, `file_not_checked`, `directory_not_checked`, and
//! `text` the English message (with the uids for a wrong owner).
//!
//! `status` is one of `applied`, `overridden`, `invalid`, `unknown`,
//! `not_applicable`, `ignored_limited_trust`; `reason` (English text) and
//! `strict_applied` are meaningful for `invalid` only. `value` is the display
//! form of the raw value, truncated to 200 characters: strings are quoted
//! with JSON escaping, and the characters JSON leaves raw that break a line
//! or reorder it are escaped as `\uXXXX` too: the controls JSON does not
//! escape (DEL and the C1 controls U+0080 to U+009F), the line and paragraph
//! separators U+2028 and U+2029, and the bidirectional formatting characters
//! U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. `dropped`
//! lists at most 50 dropped list items, each with its 1-based `position` in
//! the list as written, the `item` in display form (a
//! string quoted with the same escaping, any other value bare: `7`, `true`,
//! `[…]`, `<float>`), the text cut to 80 characters, and the `reason` in
//! English; `dropped_omitted` counts the rest.

use serde::Serialize;

use super::resolve::{Blocking, Policy, RowStatus};
use super::source::{SourceError, TrustProblem};
use super::value::DroppedItem;

/// Version of the dump's shape.
pub const DUMP_FORMAT: u32 = 1;

#[derive(Serialize)]
struct Dump<'a> {
    format: u32,
    client_version: &'a str,
    blocked: bool,
    blocking: Vec<DumpBlocking<'a>>,
    sources: Vec<DumpSource<'a>>,
    rows: Vec<DumpRow<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DumpBlocking<'a> {
    SourceUnreadable {
        source: String,
        error: DumpError<'a>,
    },
    BelowMinimumVersion {
        required: String,
        client: String,
    },
}

#[derive(Serialize)]
struct DumpError<'a> {
    code: &'static str,
    detail: Option<&'a str>,
    text: String,
}

impl<'a> DumpError<'a> {
    fn new(e: &'a SourceError) -> Self {
        Self {
            code: e.kind.code(),
            detail: e.detail.as_deref(),
            text: e.to_string(),
        }
    }
}

#[derive(Serialize)]
struct DumpTrustProblem {
    code: &'static str,
    text: String,
}

impl DumpTrustProblem {
    fn new(p: &TrustProblem) -> Self {
        Self {
            code: p.code(),
            text: p.to_string(),
        }
    }
}

#[derive(Serialize)]
struct DumpSource<'a> {
    id: String,
    trust: &'static str,
    trust_problems: Vec<DumpTrustProblem>,
    error: Option<DumpError<'a>>,
}

#[derive(Serialize)]
struct DumpDropped<'a> {
    position: usize,
    item: &'a str,
    reason: String,
}

impl<'a> DumpDropped<'a> {
    fn new(d: &'a DroppedItem) -> Self {
        Self {
            position: d.position,
            item: &d.item,
            reason: d.reason.to_string(),
        }
    }
}

#[derive(Serialize)]
struct DumpRow<'a> {
    key: &'a str,
    source: String,
    status: &'static str,
    reason: Option<String>,
    strict_applied: bool,
    value: &'a str,
    dropped: Vec<DumpDropped<'a>>,
    dropped_omitted: usize,
}

/// The report of `policy` as pretty-printed JSON (shape in the module docs).
pub fn dump_json(policy: &Policy) -> Result<String, serde_json::Error> {
    let dump = Dump {
        format: DUMP_FORMAT,
        client_version: policy.client_version(),
        blocked: policy.is_blocked(),
        blocking: policy
            .blocking()
            .iter()
            .map(|b| match b {
                Blocking::SourceUnreadable { source, error } => DumpBlocking::SourceUnreadable {
                    source: source.to_string(),
                    error: DumpError::new(error),
                },
                Blocking::BelowMinimumVersion { required, client } => {
                    DumpBlocking::BelowMinimumVersion {
                        required: required.to_string(),
                        client: client.clone(),
                    }
                }
            })
            .collect(),
        sources: policy
            .sources()
            .iter()
            .map(|s| DumpSource {
                id: s.id.to_string(),
                trust: s.trust.as_str(),
                trust_problems: s.trust_problems.iter().map(DumpTrustProblem::new).collect(),
                error: s.error.as_ref().map(DumpError::new),
            })
            .collect(),
        rows: policy
            .rows()
            .iter()
            .map(|r| {
                let (reason, strict_applied) = match &r.status {
                    RowStatus::Invalid {
                        reason,
                        strict_applied,
                    } => (Some(reason.to_string()), *strict_applied),
                    _ => (None, false),
                };
                DumpRow {
                    key: &r.key,
                    source: r.source.to_string(),
                    status: r.status.as_str(),
                    reason,
                    strict_applied,
                    value: &r.value,
                    dropped: r.dropped.iter().map(DumpDropped::new).collect(),
                    dropped_omitted: r.dropped_omitted,
                }
            })
            .collect(),
    };
    serde_json::to_string_pretty(&dump)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::policy::{
        Context, Os, RawValue, Snapshot, SourceError, SourceErrorKind, SourceId, Trust,
        TrustProblem, TrustSubject, resolve,
    };

    /// Golden fixture: the shape is a contract with scripts that read it.
    #[test]
    fn golden() {
        let s = |v: &str| RawValue::String(v.into());
        let machine = Snapshot::new(SourceId::WindowsMachine, Trust::Full)
            .with("RequireHttps", RawValue::Integer(1))
            .with("UpdateMode", s("sometimes"))
            .with("MinimumVersion", s("0.5.0"))
            .with(
                "AllowedServers",
                RawValue::List(vec![s("*.corp.com"), RawValue::Integer(7), s("ftp://x")]),
            )
            .with("FutureKey", RawValue::Bool(true))
            .with("Комментарий", s("Привет, \"мир\""));
        let user = Snapshot::new(SourceId::WindowsUser, Trust::Limited)
            .with("RequireHttps", RawValue::Bool(false))
            .with("SupportUrl", s("https://help.corp.com"))
            .with("OrganizationName", s(&"Acme ".repeat(60)));
        let mut team = Snapshot::new(SourceId::Test("team".into()), Trust::Limited)
            .with("ManagedServers", s("https://team.example Team"));
        team.trust_problems = vec![
            TrustProblem::WrongOwner {
                subject: TrustSubject::File,
                found: 1000,
                expected: 0,
            },
            TrustProblem::Writable {
                subject: TrustSubject::Directory,
            },
        ];
        let broken = Snapshot::unreadable(
            SourceId::Test("broken".into()),
            SourceError::new(SourceErrorKind::CannotOpen)
                .with_detail("Access is denied. (os error 5)"),
        );
        let big = Snapshot::unreadable(SourceId::Test("big".into()), SourceErrorKind::TooLarge);
        let ctx = Context {
            client_version: "0.4.0".into(),
            os: Os::Windows,
            mas: false,
            self_update: true,
        };
        let policy = resolve(&[machine, user, team, broken, big], &ctx);
        let actual = dump_json(&policy).unwrap();
        // Windows checkouts may convert the fixture to CRLF.
        let expected = include_str!("../../tests/fixtures/policy/dump.json").replace("\r\n", "\n");
        assert_eq!(actual.trim_end(), expected.trim_end(), "actual:\n{actual}");
    }

    #[test]
    fn empty_policy() {
        let v: serde_json::Value =
            serde_json::from_str(&dump_json(&Policy::default()).unwrap()).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["blocked"], false);
        assert_eq!(v["rows"], serde_json::json!([]));
    }
}
