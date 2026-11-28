//! Enterprise policy in the app (docs/policy.md sections 8 and 12): the
//! sources of this OS (the Windows registry and macOS managed preferences
//! readers; the Linux directory is read by the core), start-up loading, the
//! refresh, its log summary and `--dump-policy`. Resolution and the overlay
//! live in `rustket_core::policy`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use rust_i18n::t;
use rustket_core::policy::{self, Context, Os, Policy, RowStatus, Snapshot, SourceId};

pub mod managed;
pub mod page;
pub mod refresh;
pub mod registry;
pub mod servers;

/// The resolution context of this build.
pub fn context() -> Context {
    Context {
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: Os::current(),
        mas: cfg!(feature = "mas"),
        self_update: cfg!(feature = "self-update"),
    }
}

/// The snapshots of this OS's sources, highest priority first.
pub fn read_sources() -> Vec<Snapshot> {
    #[cfg(windows)]
    {
        registry::read_windows()
    }
    #[cfg(target_os = "macos")]
    {
        vec![managed::read_macos()]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        policy::read_managed_dir()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

/// Reads and resolves the policy. Runs before logging exists and twice when
/// the renderer restart happens: no logging, no side effects.
pub fn load() -> Arc<Policy> {
    load_with_sources().1
}

/// [`load`] with the snapshots it resolved (the refresh compares the next
/// read with them).
pub fn load_with_sources() -> (Vec<Snapshot>, Arc<Policy>) {
    let snapshots = read_sources();
    let policy = Arc::new(policy::resolve(&snapshots, &context()));
    (snapshots, policy)
}

/// The empty policy, for code that needs a `&Policy` without an app
/// (tests, tools).
pub fn empty() -> &'static Policy {
    static EMPTY: LazyLock<Policy> = LazyLock::new(Policy::default);
    &EMPTY
}

/// Logs what [`load`] found, once logging exists: key names, statuses,
/// codes and source ids, never values or source texts (they can quote
/// values).
pub fn log_summary(p: &Policy) {
    let (applied, strict) = summary_counts(p);
    let sources: Vec<String> = p.sources().iter().map(|s| log_id(&s.id)).collect();
    if sources.is_empty() {
        tracing::info!("policy: no sources");
    } else {
        tracing::info!(applied, strict, sources = %sources.join(", "), "policy loaded");
    }
    for s in p.sources() {
        if let Some(e) = &s.error {
            tracing::warn!(source = %log_id(&s.id), code = e.kind.code(), "policy source unreadable");
        }
        for problem in &s.trust_problems {
            tracing::warn!(
                source = %log_id(&s.id),
                code = problem.code(),
                "policy source has limited trust"
            );
        }
    }
    for r in p.rows() {
        if let RowStatus::Invalid {
            reason,
            strict_applied,
        } = &r.status
        {
            tracing::warn!(
                key = %r.key,
                source = %log_id(&r.source),
                reason = reason.code(),
                strict_applied,
                "invalid policy value"
            );
        }
    }
    if p.is_blocked() {
        tracing::warn!(reasons = p.blocking().len(), "policy blocks connections");
    }
}

/// A source id for the log: quoted with Rust's escaping, so a file name
/// with a line break or a bidirectional control cannot forge log lines.
fn log_id(id: &SourceId) -> String {
    format!("{:?}", id.to_string())
}

/// The keys applied with a valid value, and the keys whose strict value is
/// in force (an invalid value).
fn summary_counts(p: &Policy) -> (usize, usize) {
    let count = |f: fn(&RowStatus) -> bool| p.rows().iter().filter(|r| f(&r.status)).count();
    (
        count(|s| *s == RowStatus::Applied),
        count(|s| {
            matches!(
                s,
                RowStatus::Invalid {
                    strict_applied: true,
                    ..
                }
            )
        }),
    )
}

/// Logs a policy the refresh put in force: the changed keys by name (never
/// values), then the summary.
pub fn log_replaced(old: &Policy, new: &Policy) {
    let changed: Vec<&str> = old.changed_keys(new).iter().map(|k| k.name()).collect();
    tracing::info!(changed = %changed.join(", "), "policy changed");
    log_summary(new);
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("cannot serialise the policy: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("cannot write {}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write to standard output: {0}")]
    Stdout(std::io::Error),
}

impl DumpError {
    /// The message `main` prints, in the UI language (the OS or parser text
    /// stays untranslated).
    pub fn localized(&self) -> String {
        match self {
            Self::Serialize(e) => t!("cli.dump_policy_failed", error = e.to_string()).into_owned(),
            Self::File { path, source } => t!(
                "cli.dump_policy_file_failed",
                path = path.display().to_string(),
                error = source.to_string()
            )
            .into_owned(),
            Self::Stdout(e) => {
                t!("cli.dump_policy_stdout_failed", error = e.to_string()).into_owned()
            }
        }
    }
}

/// `--dump-policy [FILE]`: the JSON of `p` with a final newline, written to
/// `out` (created or truncated) or to standard output.
pub fn dump(p: &Policy, out: Option<&Path>) -> Result<(), DumpError> {
    let mut json = policy::dump_json(p)?;
    json.push('\n');
    match out {
        Some(path) => std::fs::write(path, json).map_err(|source| DumpError::File {
            path: path.to_owned(),
            source,
        }),
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(json.as_bytes())
                .and_then(|()| stdout.flush())
                .map_err(DumpError::Stdout)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn context_of_this_build() {
        let c = context();
        assert_eq!(c.client_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(c.os, Os::current());
        assert_eq!(c.mas, cfg!(feature = "mas"));
        assert_eq!(c.self_update, cfg!(feature = "self-update"));
    }

    #[test]
    fn empty_policy_sets_nothing() {
        assert!(empty().has_no_settings());
        assert!(!empty().is_blocked());
    }

    #[test]
    fn dump_writes_and_truncates_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        std::fs::write(&path, "x".repeat(10_000)).unwrap();
        dump(&Policy::default(), Some(&path)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["format"], policy::DUMP_FORMAT);
        let missing = dir.path().join("no/such/dir/p.json");
        let e = dump(&Policy::default(), Some(&missing)).unwrap_err();
        assert!(matches!(e, DumpError::File { .. }), "{e}");
        let text = e.localized();
        assert!(text.contains("p.json"), "{text}");
        assert!(!text.contains("cli."), "a missing key: {text}");
    }

    #[test]
    fn source_ids_in_the_log_stay_on_one_line() {
        let id = SourceId::LinuxFile("a\n\r\u{202E}\u{2028}b.json".into());
        let logged = log_id(&id);
        for c in ['\n', '\r', '\u{202E}', '\u{2028}'] {
            assert!(!logged.contains(c), "{logged:?}");
        }
        assert!(logged.starts_with("\"linux:a"), "{logged}");
    }

    /// Section 11: the Mac App Store build fails on these substrings, and
    /// the registry and the locale files are in every flavor.
    #[test]
    fn no_store_flavor_markers_in_names_or_strings() {
        const MARKERS: [&str; 4] = [
            "api.github.com",
            "install-kind",
            "SHA256SUMS",
            "didDismissAlert",
        ];
        let mut checked: Vec<String> = Vec::new();
        for spec in policy::specs() {
            checked.push(spec.name.to_owned());
            if let policy::ValueType::Enum(variants) = spec.value_type {
                checked.extend(variants.iter().map(|v| (*v).to_owned()));
            }
        }
        checked.extend(policy::UpdateMode::NAMES.iter().map(|v| (*v).to_owned()));
        let yml = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("locales/policy.yml"),
        )
        .unwrap();
        assert!(yml.lines().count() > 100, "the policy strings are read");
        checked.extend(yml.lines().map(str::to_owned));
        for s in &checked {
            for m in MARKERS {
                assert!(!s.contains(m), "{m} in {s:?}");
            }
        }
        assert!(checked.iter().any(|s| s == "prompt"), "enum values checked");
    }

    #[test]
    fn summary_counts_applied_and_strict_keys() {
        let snap = Snapshot::new(SourceId::Test("t".into()), policy::Trust::Full)
            .with("RequireHttps", policy::RawValue::Bool(true))
            .with("NotificationPreview", policy::RawValue::Bool(false))
            .with("AllowUserServers", policy::RawValue::Integer(7))
            .with("MinimumVersion", policy::RawValue::String("x".into()))
            .with("Unknown", policy::RawValue::Bool(true));
        let p = policy::resolve(&[snap], &context());
        // MinimumVersion is invalid without a strict value.
        assert_eq!(summary_counts(&p), (2, 1));
        assert_eq!(summary_counts(&Policy::default()), (0, 0));
    }
}
