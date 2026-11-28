//! Source snapshots (docs/policy.md section 5): what one reader read from
//! one source.

use std::collections::BTreeMap;
use std::fmt;

use super::value::RawValue;

/// Where a snapshot comes from. Its display form is stable (diagnostics and
/// `--dump-policy`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceId {
    /// `HKLM\Software\Policies\Rustket`.
    WindowsMachine,
    /// `HKCU\Software\Policies\Rustket`.
    WindowsUser,
    /// Forced values of the managed preference domain.
    MacManaged,
    /// The Linux policy directory itself, when it exists but cannot be listed.
    LinuxDirectory,
    /// One file of the Linux policy directory, by file name: a backslash is
    /// written as two backslashes and each byte that is not UTF-8 as `\xNN`,
    /// so the rendering is reversible.
    LinuxFile(String),
    /// Snapshots built by tests.
    Test(String),
}

impl SourceId {
    /// The fixed trust of section 5: `HKCU` is limited, every other source
    /// full. A Linux file can still be limited by its ownership check.
    pub fn default_trust(&self) -> Trust {
        match self {
            SourceId::WindowsUser => Trust::Limited,
            SourceId::WindowsMachine
            | SourceId::MacManaged
            | SourceId::LinuxDirectory
            | SourceId::LinuxFile(_)
            | SourceId::Test(_) => Trust::Full,
        }
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceId::WindowsMachine => f.write_str("windows:HKLM"),
            SourceId::WindowsUser => f.write_str("windows:HKCU"),
            SourceId::MacManaged => f.write_str("macos:managed"),
            SourceId::LinuxDirectory => f.write_str("linux:directory"),
            SourceId::LinuxFile(name) => write!(f, "linux:{name}"),
            SourceId::Test(name) => write!(f, "test:{name}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trust {
    Full,
    /// May not set trust-expanding (class T) keys.
    Limited,
}

impl Trust {
    /// Every level (the diagnostics page translates each by its code).
    pub const ALL: [Trust; 2] = [Trust::Full, Trust::Limited];

    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Full => "full",
            Trust::Limited => "limited",
        }
    }
}

/// Why a source that exists could not be read (section 6). Typed so the UI
/// can translate it; [`SourceError::detail`] keeps the untranslated text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceErrorKind {
    /// The source (a file, a registry key) cannot be opened.
    CannotOpen,
    CannotReadMetadata,
    CannotRead,
    /// Larger than [`super::MAX_FILE_BYTES`].
    TooLarge,
    InvalidJson,
    /// The top-level JSON value is not an object.
    NotAnObject,
    /// The policy directory exists but cannot be listed.
    CannotListDirectory,
    /// The source changed during every read (section 8).
    Unstable,
}

impl SourceErrorKind {
    /// Every kind (the diagnostics page translates each by its code).
    pub const ALL: [SourceErrorKind; 8] = [
        SourceErrorKind::CannotOpen,
        SourceErrorKind::CannotReadMetadata,
        SourceErrorKind::CannotRead,
        SourceErrorKind::TooLarge,
        SourceErrorKind::InvalidJson,
        SourceErrorKind::NotAnObject,
        SourceErrorKind::CannotListDirectory,
        SourceErrorKind::Unstable,
    ];

    /// Stable short code (`--dump-policy`).
    pub fn code(self) -> &'static str {
        match self {
            SourceErrorKind::CannotOpen => "cannot_open",
            SourceErrorKind::CannotReadMetadata => "cannot_read_metadata",
            SourceErrorKind::CannotRead => "cannot_read",
            SourceErrorKind::TooLarge => "too_large",
            SourceErrorKind::InvalidJson => "invalid_json",
            SourceErrorKind::NotAnObject => "not_an_object",
            SourceErrorKind::CannotListDirectory => "cannot_list_directory",
            SourceErrorKind::Unstable => "unstable",
        }
    }
}

impl fmt::Display for SourceErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SourceErrorKind::CannotOpen => "cannot open",
            SourceErrorKind::CannotReadMetadata => "cannot read metadata",
            SourceErrorKind::CannotRead => "cannot read",
            SourceErrorKind::TooLarge => "larger than 1 MiB",
            SourceErrorKind::InvalidJson => "invalid JSON",
            SourceErrorKind::NotAnObject => "the top-level value is not a JSON object",
            SourceErrorKind::CannotListDirectory => "cannot list the directory",
            SourceErrorKind::Unstable => "changed during every read",
        })
    }
}

/// A source error: its kind and the operating system's or parser's own
/// text, untranslated. `Display` is English, for logs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
#[error("{kind}{}", .detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
pub struct SourceError {
    pub kind: SourceErrorKind,
    pub detail: Option<String>,
}

impl SourceError {
    pub fn new(kind: SourceErrorKind) -> Self {
        Self { kind, detail: None }
    }

    /// Builder: adds the untranslated detail.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl From<SourceErrorKind> for SourceError {
    fn from(kind: SourceErrorKind) -> Self {
        Self::new(kind)
    }
}

/// What a trust problem is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustSubject {
    File,
    Directory,
}

impl fmt::Display for TrustSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrustSubject::File => "the file",
            TrustSubject::Directory => "the directory",
        })
    }
}

/// Why a Linux file has limited trust (section 5). `Display` is English,
/// for logs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrustProblem {
    WrongOwner {
        subject: TrustSubject,
        found: u32,
        expected: u32,
    },
    /// Writable by group or others.
    Writable { subject: TrustSubject },
    /// The directory path named another directory after the files were read.
    DirectoryChanged,
    /// Ownership and mode could not be checked; `detail` is the operating
    /// system's text, untranslated.
    NotChecked {
        subject: TrustSubject,
        detail: Option<String>,
    },
}

impl TrustProblem {
    /// Every code [`Self::code`] returns.
    pub const CODES: [&'static str; 7] = [
        "file_wrong_owner",
        "directory_wrong_owner",
        "file_writable",
        "directory_writable",
        "directory_changed",
        "file_not_checked",
        "directory_not_checked",
    ];

    /// Stable short code (`--dump-policy`), e.g. `file_wrong_owner`.
    pub fn code(&self) -> &'static str {
        use TrustSubject::{Directory, File};
        match self {
            TrustProblem::WrongOwner { subject: File, .. } => "file_wrong_owner",
            TrustProblem::WrongOwner {
                subject: Directory, ..
            } => "directory_wrong_owner",
            TrustProblem::Writable { subject: File } => "file_writable",
            TrustProblem::Writable { subject: Directory } => "directory_writable",
            TrustProblem::DirectoryChanged => "directory_changed",
            TrustProblem::NotChecked { subject: File, .. } => "file_not_checked",
            TrustProblem::NotChecked {
                subject: Directory, ..
            } => "directory_not_checked",
        }
    }
}

impl fmt::Display for TrustProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustProblem::WrongOwner {
                subject,
                found,
                expected,
            } => write!(f, "{subject} is owned by uid {found}, not {expected}"),
            TrustProblem::Writable { subject } => {
                write!(f, "{subject} is writable by group or others")
            }
            TrustProblem::DirectoryChanged => {
                f.write_str("the directory changed while it was read")
            }
            TrustProblem::NotChecked {
                subject,
                detail: Some(detail),
            } => write!(f, "{subject} could not be checked: {detail}"),
            TrustProblem::NotChecked {
                subject,
                detail: None,
            } => write!(f, "{subject} could not be checked"),
        }
    }
}

/// What a reader read from one source.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: SourceId,
    pub trust: Trust,
    /// Why the trust is limited (shown in diagnostics); empty when the trust
    /// is full or limited by the source itself (`HKCU`).
    pub trust_problems: Vec<TrustProblem>,
    /// Raw values by key name as written in the source.
    pub values: BTreeMap<String, RawValue>,
    /// The source exists but could not be read; a blocking state. Values it
    /// still carries (a partial read) are resolved like any others.
    pub error: Option<SourceError>,
}

impl Snapshot {
    pub fn new(id: SourceId, trust: Trust) -> Self {
        Self {
            id,
            trust,
            trust_problems: Vec::new(),
            values: BTreeMap::new(),
            error: None,
        }
    }

    /// A source that exists but could not be read, with the source's
    /// [`SourceId::default_trust`].
    pub fn unreadable(id: SourceId, error: impl Into<SourceError>) -> Self {
        let trust = id.default_trust();
        Self {
            error: Some(error.into()),
            ..Self::new(id, trust)
        }
    }

    /// Builder: adds a value.
    pub fn with(mut self, name: &str, value: RawValue) -> Self {
        self.values.insert(name.to_owned(), value);
        self
    }
}
