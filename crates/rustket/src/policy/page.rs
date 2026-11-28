//! The Settings → Policies page (docs/policy.md sections 6 and 10): what it
//! shows of a [`Policy`], in order. Pure: no iced; the texts are i18n keys
//! computed from the core's stable codes (`locales/policy.yml`), so a code
//! the core adds needs only a translation.

use rustket_core::policy::{
    Blocking, DroppedItem, InvalidReason, Key, Policy, ReportRow, RowStatus, SourceErrorKind,
    SourceId, SourceInfo, Trust, TrustProblem,
};

/// What the page shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<'a> {
    pub blocking: &'a [Blocking],
    /// The report rows in the report's own order.
    pub rows: Vec<&'a ReportRow>,
    /// Every source in priority order.
    pub sources: &'a [SourceInfo],
}

impl<'a> Page<'a> {
    /// The page of `policy`. `mas`: the Mac App Store flavor, which never
    /// shows the rows of keys that do not apply to it (sections 3 and 10: no
    /// updater, no renderer switch, no secrets file); `--dump-policy` keeps
    /// them.
    pub fn new(policy: &'a Policy, mas: bool) -> Self {
        Page {
            blocking: policy.blocking(),
            rows: policy
                .rows()
                .iter()
                .filter(|r| !(mas && hidden_in_mas(r)))
                .collect(),
            sources: policy.sources(),
        }
    }

    /// The page of this build's flavor.
    pub fn of_build(policy: &'a Policy) -> Self {
        Self::new(policy, cfg!(feature = "mas"))
    }

    /// Whether there is anything to show: a report row, a source with an
    /// error or a trust problem, or a blocking state. Without one the
    /// Policies tab is not offered.
    pub fn has_content(&self) -> bool {
        !self.rows.is_empty()
            || !self.blocking.is_empty()
            || self
                .sources
                .iter()
                .any(|s| s.error.is_some() || !s.trust_problems.is_empty())
    }
}

/// A row of a key the Mac App Store flavor does not have.
fn hidden_in_mas(r: &ReportRow) -> bool {
    Key::from_name(&r.key).is_some_and(|k| !k.spec().applicability.mas)
}

/// The label of a source: an i18n key and the file name for Linux files
/// (the `name` placeholder).
pub fn source_label(id: &SourceId) -> (&'static str, Option<&str>) {
    match id {
        SourceId::WindowsMachine => ("policy.source.windows_machine", None),
        SourceId::WindowsUser => ("policy.source.windows_user", None),
        SourceId::MacManaged => ("policy.source.macos_managed", None),
        SourceId::LinuxDirectory => ("policy.source.linux_directory", None),
        SourceId::LinuxFile(name) => ("policy.source.linux_file", Some(name)),
        SourceId::Test(name) => ("policy.source.test", Some(name)),
    }
}

/// One source of every kind (keep in step with `source_kind_index` in the
/// tests).
#[cfg(test)]
fn source_samples() -> [SourceId; 6] {
    [
        SourceId::WindowsMachine,
        SourceId::WindowsUser,
        SourceId::MacManaged,
        SourceId::LinuxDirectory,
        SourceId::LinuxFile(String::new()),
        SourceId::Test(String::new()),
    ]
}

pub fn status_key(s: &RowStatus) -> String {
    format!("policy.status.{}", s.as_str())
}

pub fn error_key(kind: SourceErrorKind) -> String {
    format!("policy.error.{}", kind.code())
}

pub fn trust_problem_key(p: &TrustProblem) -> String {
    format!("policy.trust_problem.{}", p.code())
}

pub fn reason_key(r: &InvalidReason) -> String {
    format!("policy.reason.{}", r.code())
}

pub fn drop_key(d: &DroppedItem) -> String {
    format!("policy.drop.{}", d.reason.code())
}

pub fn trust_key(t: Trust) -> String {
    format!("policy.trust.{}", t.as_str())
}

/// Every key the page can compute from a core code, with the codes the
/// core can produce.
#[cfg(test)]
fn code_keys() -> Vec<String> {
    let mut keys: Vec<String> = source_samples()
        .iter()
        .map(|id| source_label(id).0.to_owned())
        .collect();
    keys.extend(RowStatus::CODES.map(|c| format!("policy.status.{c}")));
    keys.extend(SourceErrorKind::ALL.map(error_key));
    keys.extend(TrustProblem::CODES.map(|c| format!("policy.trust_problem.{c}")));
    keys.extend(InvalidReason::CODES.map(|c| format!("policy.reason.{c}")));
    keys.extend(rustket_core::policy::DropReason::CODES.map(|c| format!("policy.drop.{c}")));
    keys.extend(Trust::ALL.map(trust_key));
    keys
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rustket_core::policy::{Context, Os, RawValue, Snapshot, SourceError, resolve};

    use super::*;

    fn ctx(mas: bool) -> Context {
        Context {
            client_version: "0.4.0".into(),
            os: if mas { Os::MacOs } else { Os::Linux },
            mas,
            self_update: !mas,
        }
    }

    /// Position of a source's kind in `source_samples`. An exhaustive match,
    /// so a new kind does not compile until it gets an arm here. The test
    /// below checks that the samples hit the indices 0..n in order; it cannot
    /// see a kind that has an arm but no sample, so add both together.
    fn source_kind_index(id: &SourceId) -> usize {
        match id {
            SourceId::WindowsMachine => 0,
            SourceId::WindowsUser => 1,
            SourceId::MacManaged => 2,
            SourceId::LinuxDirectory => 3,
            SourceId::LinuxFile(_) => 4,
            SourceId::Test(_) => 5,
        }
    }

    fn file(name: &str) -> Snapshot {
        Snapshot::new(SourceId::LinuxFile(name.into()), Trust::Full)
    }

    fn keys(p: &Page<'_>) -> Vec<(String, String, &'static str)> {
        p.rows
            .iter()
            .map(|r| (r.key.clone(), r.source.to_string(), r.status.as_str()))
            .collect()
    }

    #[test]
    fn rows_keep_the_report_order() {
        let high = file("b.json")
            .with("RequireHttps", RawValue::Bool(true))
            .with("Typo", RawValue::Bool(true));
        let low = file("a.json")
            .with("RequireHttps", RawValue::Bool(false))
            .with("AllowUserServers", RawValue::Bool(false));
        let policy = resolve(&[high, low], &ctx(false));
        let page = Page::new(&policy, false);
        assert_eq!(page.rows.len(), policy.rows().len());
        for (a, b) in page.rows.iter().zip(policy.rows()) {
            assert!(std::ptr::eq(*a, b), "reordered");
        }
        assert_eq!(
            keys(&page),
            [
                ("AllowUserServers".into(), "linux:a.json".into(), "applied"),
                ("RequireHttps".into(), "linux:b.json".into(), "applied"),
                ("RequireHttps".into(), "linux:a.json".into(), "overridden"),
                ("Typo".into(), "linux:b.json".into(), "unknown"),
            ]
        );
        assert!(page.has_content());
        assert_eq!(page.sources.len(), 2);
    }

    #[test]
    fn invalid_row_with_dropped_items_and_more() {
        let mut items: Vec<RawValue> = vec![RawValue::String("https://ok.example".into())];
        items.extend((0..60).map(RawValue::Integer));
        let snap = file("a.json")
            .with("ManagedServers", RawValue::List(items))
            .with("AllowUserServers", RawValue::Integer(7));
        let policy = resolve(&[snap], &ctx(false));
        let page = Page::new(&policy, false);
        let servers = page
            .rows
            .iter()
            .find(|r| r.key == "ManagedServers")
            .unwrap();
        assert_eq!(servers.status, RowStatus::Applied);
        assert_eq!(servers.dropped.len(), 50);
        assert_eq!(servers.dropped_omitted, 10);
        assert_eq!(servers.dropped[0].position, 2);
        assert_eq!(drop_key(&servers.dropped[0]), "policy.drop.not_a_string");
        let invalid = page
            .rows
            .iter()
            .find(|r| r.key == "AllowUserServers")
            .unwrap();
        let RowStatus::Invalid {
            reason,
            strict_applied,
        } = &invalid.status
        else {
            panic!("{:?}", invalid.status);
        };
        assert!(*strict_applied);
        assert_eq!(reason_key(reason), "policy.reason.not_bool_integer");
        assert_eq!(status_key(&invalid.status), "policy.status.invalid");
    }

    #[test]
    fn blocking_states_of_each_kind() {
        let broken = Snapshot::unreadable(
            SourceId::LinuxFile("x.json".into()),
            SourceError::new(SourceErrorKind::InvalidJson).with_detail("EOF at line 1"),
        );
        let min = file("a.json").with("MinimumVersion", RawValue::String("9.0.0".into()));
        let policy = resolve(&[broken, min], &ctx(false));
        let page = Page::new(&policy, false);
        assert!(page.has_content());
        assert_eq!(page.blocking.len(), 2);
        assert!(page.blocking.iter().any(|b| matches!(
            b,
            Blocking::SourceUnreadable { source, error }
                if *source == SourceId::LinuxFile("x.json".into())
                    && error.kind == SourceErrorKind::InvalidJson
                    && error.detail.as_deref() == Some("EOF at line 1")
        )));
        assert!(page.blocking.iter().any(|b| matches!(
            b,
            Blocking::BelowMinimumVersion { required, client }
                if required.to_string() == "9.0.0" && client == "0.4.0"
        )));

        // A source that cannot be read is enough on its own.
        let only = resolve(
            &[Snapshot::unreadable(
                SourceId::WindowsMachine,
                SourceErrorKind::Unstable,
            )],
            &ctx(false),
        );
        let page = Page::new(&only, false);
        assert!(page.rows.is_empty());
        assert!(page.has_content());
        assert_eq!(
            error_key(SourceErrorKind::Unstable),
            "policy.error.unstable"
        );
    }

    #[test]
    fn sources_with_trust_problems() {
        let mut limited = file("a.json").with("SupportUrl", RawValue::String("https://h".into()));
        limited.trust = Trust::Limited;
        limited.trust_problems = vec![
            TrustProblem::WrongOwner {
                subject: rustket_core::policy::TrustSubject::File,
                found: 1000,
                expected: 0,
            },
            TrustProblem::Writable {
                subject: rustket_core::policy::TrustSubject::Directory,
            },
        ];
        let policy = resolve(&[limited], &ctx(false));
        let page = Page::new(&policy, false);
        assert_eq!(page.sources.len(), 1);
        let s = &page.sources[0];
        assert_eq!(trust_key(s.trust), "policy.trust.limited");
        assert_eq!(
            s.trust_problems
                .iter()
                .map(trust_problem_key)
                .collect::<Vec<_>>(),
            [
                "policy.trust_problem.file_wrong_owner",
                "policy.trust_problem.directory_writable"
            ]
        );
        assert_eq!(page.rows[0].status, RowStatus::IgnoredLimitedTrust);
        assert_eq!(
            source_label(&s.id),
            ("policy.source.linux_file", Some("a.json"))
        );

        // A trust problem alone (no row) still makes a page.
        let mut quiet = file("b.json");
        quiet.trust = Trust::Limited;
        quiet.trust_problems = vec![TrustProblem::DirectoryChanged];
        let policy = resolve(&[quiet], &ctx(false));
        assert!(Page::new(&policy, false).has_content());
    }

    #[test]
    fn nothing_to_show() {
        let empty = Policy::default();
        assert!(!Page::new(&empty, false).has_content());
        // A readable source without values or problems.
        let policy = resolve(&[file("a.json")], &ctx(false));
        assert!(!Page::new(&policy, false).has_content());
    }

    #[test]
    fn mac_app_store_hides_the_keys_it_does_not_have() {
        let snap = Snapshot::new(SourceId::MacManaged, Trust::Full)
            .with("UpdateMode", RawValue::String("off".into()))
            .with("HardwareAcceleration", RawValue::Bool(false))
            .with("AllowSecretsFile", RawValue::Bool(false))
            .with("NotificationPreview", RawValue::Bool(false))
            .with("Typo", RawValue::Bool(true));
        let policy = resolve(&[snap], &ctx(true));
        assert_eq!(policy.rows().len(), 5, "the report keeps every row");
        let mas = Page::new(&policy, true);
        assert_eq!(
            mas.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["NotificationPreview", "Typo"]
        );
        // The dump keeps them.
        let dump = rustket_core::policy::dump_json(&policy).unwrap();
        assert!(dump.contains("UpdateMode") && dump.contains("HardwareAcceleration"));
        // Other flavors show every row, also the not applicable ones.
        let other = Page::new(&policy, false);
        assert_eq!(other.rows.len(), 5);

        // Only hidden rows: nothing to show there.
        let snap = Snapshot::new(SourceId::MacManaged, Trust::Full)
            .with("UpdateMode", RawValue::String("off".into()));
        let policy = resolve(&[snap], &ctx(true));
        assert!(!Page::new(&policy, true).has_content());
        assert!(Page::new(&policy, false).has_content());
    }

    /// Every code the core can produce has its key in every complete locale.
    #[test]
    fn every_core_code_is_translated() {
        let entries = crate::i18n::tests::locale_entries();
        let keys = code_keys();
        assert_eq!(
            keys.len(),
            source_samples().len()
                + RowStatus::CODES.len()
                + SourceErrorKind::ALL.len()
                + TrustProblem::CODES.len()
                + InvalidReason::CODES.len()
                + rustket_core::policy::DropReason::CODES.len()
                + Trust::ALL.len()
        );
        for key in &keys {
            let texts = entries
                .get(key)
                .unwrap_or_else(|| panic!("{key} is not translated"));
            for loc in crate::i18n::tests::BUNDLED_COMPLETE {
                assert!(
                    texts.get(*loc).is_some_and(|t| !t.is_empty()),
                    "{key}: missing {loc}"
                );
            }
        }
        // The samples hit each index of `source_kind_index` once, in order (a
        // kind with an arm but no sample is not seen).
        let samples = source_samples();
        let indices: Vec<usize> = samples.iter().map(source_kind_index).collect();
        assert_eq!(indices, (0..samples.len()).collect::<Vec<_>>());
    }
}
