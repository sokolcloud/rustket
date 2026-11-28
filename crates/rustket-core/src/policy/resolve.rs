//! Resolution of source snapshots into a [`Policy`] (docs/policy.md section 6).

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use super::registry::{Class, Key, Os, StrictValue, UpdateMode};
use super::servers::{ManagedServer, ServerPattern};
use super::source::{Snapshot, SourceError, SourceId, Trust, TrustProblem};
use super::value::{
    CoerceError, Coerced, Coercion, DISPLAY_MAX_CHARS, DropReason, DroppedItem, EMBED_MAX_CHARS,
    coerce, quote, truncate,
};

/// Longest `OrganizationName`, in characters.
pub const MAX_ORGANIZATION_NAME_CHARS: usize = 80;

/// Dropped list items kept per report row; the rest are only counted.
pub const MAX_DROPPED_REPORTED: usize = 50;

/// What resolution needs to know about the running client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// `CARGO_PKG_VERSION` of the client; compared with `MinimumVersion`.
    pub client_version: String,
    pub os: Os,
    /// Mac App Store flavor.
    pub mas: bool,
    /// Built with the `self-update` feature.
    pub self_update: bool,
}

/// `MAJOR.MINOR.PATCH`: three non-negative decimal integers, no suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let mut next = || {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse::<u64>().ok()
        };
        let v = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A reason that stops the app from connecting (section 9.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocking {
    /// A source exists but could not be read or parsed.
    SourceUnreadable {
        source: SourceId,
        error: SourceError,
    },
    /// `MinimumVersion` is greater than the client version (semver order:
    /// a pre-release is older than its release). `client` is the full
    /// client version string.
    BelowMinimumVersion { required: Version, client: String },
}

/// Why a value is invalid. Typed so the diagnostics page can translate it;
/// `Display` gives the English text of `--dump-policy`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidReason {
    #[error(transparent)]
    Coerce(#[from] CoerceError),
    #[error("must be a single line without control or bidirectional formatting characters")]
    NotSingleLine,
    #[error("longer than {max} characters")]
    TooLong { max: usize },
    #[error("not a URL: {0}")]
    NotUrl(url::ParseError),
    #[error("must be an absolute https:// URL without control characters")]
    NotHttpsUrl,
    #[error("expected MAJOR.MINOR.PATCH, e.g. 0.4.0")]
    BadVersion,
    /// A coerced value of the wrong shape for the key; a registry bug.
    #[error("internal: unexpected value type for {0}")]
    Internal(Key),
}

impl InvalidReason {
    /// Every code [`Self::code`] returns.
    pub const CODES: [&'static str; 10] = [
        "wrong_type",
        "not_bool_integer",
        "unknown_variant",
        "empty",
        "not_single_line",
        "too_long",
        "not_url",
        "not_https_url",
        "bad_version",
        "internal",
    ];

    /// Stable short code, for logs and the diagnostics page (the text can
    /// quote the value).
    pub fn code(&self) -> &'static str {
        match self {
            InvalidReason::Coerce(CoerceError::WrongType { .. }) => "wrong_type",
            InvalidReason::Coerce(CoerceError::NotBoolInteger(_)) => "not_bool_integer",
            InvalidReason::Coerce(CoerceError::UnknownVariant { .. }) => "unknown_variant",
            InvalidReason::Coerce(CoerceError::Empty) => "empty",
            InvalidReason::NotSingleLine => "not_single_line",
            InvalidReason::TooLong { .. } => "too_long",
            InvalidReason::NotUrl(_) => "not_url",
            InvalidReason::NotHttpsUrl => "not_https_url",
            InvalidReason::BadVersion => "bad_version",
            InvalidReason::Internal(_) => "internal",
        }
    }
}

/// Status of one report row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowStatus {
    Applied,
    /// A higher-priority source decided the key. Also used for the lower
    /// sources of a key whose invalid value is ignored (no strict value):
    /// they are not consulted, and the key stays unset.
    Overridden,
    /// The value is invalid; `strict_applied` tells whether the key's strict
    /// value was applied instead (otherwise the key stays unset).
    Invalid {
        reason: InvalidReason,
        strict_applied: bool,
    },
    Unknown,
    /// The key does not apply to this OS or flavor.
    NotApplicable,
    /// A limited source set a trust-expanding key.
    IgnoredLimitedTrust,
}

impl RowStatus {
    /// Every name [`Self::as_str`] returns.
    pub const CODES: [&'static str; 6] = [
        "applied",
        "overridden",
        "invalid",
        "unknown",
        "not_applicable",
        "ignored_limited_trust",
    ];

    /// Stable name (`--dump-policy`).
    pub fn as_str(&self) -> &'static str {
        match self {
            RowStatus::Applied => "applied",
            RowStatus::Overridden => "overridden",
            RowStatus::Invalid { .. } => "invalid",
            RowStatus::Unknown => "unknown",
            RowStatus::NotApplicable => "not_applicable",
            RowStatus::IgnoredLimitedTrust => "ignored_limited_trust",
        }
    }
}

/// One (source, key name) seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// Key name as written (an unknown name truncated).
    pub key: String,
    pub source: SourceId,
    pub status: RowStatus,
    /// Display form of the raw value, truncated.
    pub value: String,
    /// List items dropped while applying (bad type, empty, unparsable), at
    /// most [`MAX_DROPPED_REPORTED`].
    pub dropped: Vec<DroppedItem>,
    /// Dropped items beyond those in `dropped`.
    pub dropped_omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub id: SourceId,
    pub trust: Trust,
    pub trust_problems: Vec<TrustProblem>,
    pub error: Option<SourceError>,
}

/// Values of the applied keys; `None` is "not set".
#[derive(Debug, Clone, PartialEq, Default)]
struct Settings {
    managed_servers: Option<Vec<ManagedServer>>,
    allow_user_servers: Option<bool>,
    allowed_servers: Option<Vec<ServerPattern>>,
    require_https: Option<bool>,
    allow_certificate_exceptions: Option<bool>,
    update_mode: Option<UpdateMode>,
    allow_password_login: Option<bool>,
    allow_token_login: Option<bool>,
    allow_secrets_file: Option<bool>,
    notification_preview: Option<bool>,
    hardware_acceleration: Option<bool>,
    launch_at_login: Option<bool>,
    organization_name: Option<String>,
    support_url: Option<String>,
    minimum_version: Option<Version>,
}

impl Settings {
    fn bool_slot(&mut self, key: Key) -> Option<&mut Option<bool>> {
        Some(match key {
            Key::AllowUserServers => &mut self.allow_user_servers,
            Key::RequireHttps => &mut self.require_https,
            Key::AllowCertificateExceptions => &mut self.allow_certificate_exceptions,
            Key::AllowPasswordLogin => &mut self.allow_password_login,
            Key::AllowTokenLogin => &mut self.allow_token_login,
            Key::AllowSecretsFile => &mut self.allow_secrets_file,
            Key::NotificationPreview => &mut self.notification_preview,
            Key::HardwareAcceleration => &mut self.hardware_acceleration,
            Key::LaunchAtLogin => &mut self.launch_at_login,
            _ => return None,
        })
    }

    fn is_set(&self, key: Key) -> bool {
        match key {
            Key::ManagedServers => self.managed_servers.is_some(),
            Key::AllowedServers => self.allowed_servers.is_some(),
            Key::UpdateMode => self.update_mode.is_some(),
            Key::OrganizationName => self.organization_name.is_some(),
            Key::SupportUrl => self.support_url.is_some(),
            Key::MinimumVersion => self.minimum_version.is_some(),
            Key::AllowUserServers => self.allow_user_servers.is_some(),
            Key::RequireHttps => self.require_https.is_some(),
            Key::AllowCertificateExceptions => self.allow_certificate_exceptions.is_some(),
            Key::AllowPasswordLogin => self.allow_password_login.is_some(),
            Key::AllowTokenLogin => self.allow_token_login.is_some(),
            Key::AllowSecretsFile => self.allow_secrets_file.is_some(),
            Key::NotificationPreview => self.notification_preview.is_some(),
            Key::HardwareAcceleration => self.hardware_acceleration.is_some(),
            Key::LaunchAtLogin => self.launch_at_login.is_some(),
        }
    }

    /// Whether `key` has the same value (or is unset) in both.
    fn key_eq(&self, other: &Settings, key: Key) -> bool {
        match key {
            Key::ManagedServers => self.managed_servers == other.managed_servers,
            Key::AllowUserServers => self.allow_user_servers == other.allow_user_servers,
            Key::AllowedServers => self.allowed_servers == other.allowed_servers,
            Key::RequireHttps => self.require_https == other.require_https,
            Key::AllowCertificateExceptions => {
                self.allow_certificate_exceptions == other.allow_certificate_exceptions
            }
            Key::UpdateMode => self.update_mode == other.update_mode,
            Key::AllowPasswordLogin => self.allow_password_login == other.allow_password_login,
            Key::AllowTokenLogin => self.allow_token_login == other.allow_token_login,
            Key::AllowSecretsFile => self.allow_secrets_file == other.allow_secrets_file,
            Key::NotificationPreview => self.notification_preview == other.notification_preview,
            Key::HardwareAcceleration => self.hardware_acceleration == other.hardware_acceleration,
            Key::LaunchAtLogin => self.launch_at_login == other.launch_at_login,
            Key::OrganizationName => self.organization_name == other.organization_name,
            Key::SupportUrl => self.support_url == other.support_url,
            Key::MinimumVersion => self.minimum_version == other.minimum_version,
        }
    }

    /// Validates a coerced value for `key` and stores it. Returns the
    /// dropped list items, or why the whole value is invalid (nothing stored).
    fn set(&mut self, key: Key, coercion: Coercion) -> Result<Vec<DroppedItem>, InvalidReason> {
        let Coercion {
            value,
            mut dropped,
            positions,
        } = coercion;
        let mismatch = || InvalidReason::Internal(key);
        let dropped_item = |position: usize, item: &str, reason: DropReason| DroppedItem {
            position,
            item: truncate(&quote(item), EMBED_MAX_CHARS),
            reason,
        };
        match (key, &value) {
            (Key::ManagedServers, Coerced::List(items)) => {
                let mut servers: Vec<ManagedServer> = Vec::new();
                let mut seen: HashSet<String> = HashSet::new();
                for (item, pos) in items.iter().zip(positions.iter().copied()) {
                    match ManagedServer::parse(item) {
                        Ok(m) if !seen.insert(m.url.as_str().to_owned()) => {
                            dropped.push(dropped_item(pos, item, DropReason::DuplicateServer))
                        }
                        Ok(m) => servers.push(m),
                        Err(e) => dropped.push(dropped_item(pos, item, e.into())),
                    }
                }
                dropped.sort_by_key(|d| d.position);
                self.managed_servers = Some(servers);
                Ok(dropped)
            }
            (Key::AllowedServers, Coerced::List(items)) => {
                let mut patterns = Vec::new();
                for (item, pos) in items.iter().zip(positions.iter().copied()) {
                    match ServerPattern::parse(item) {
                        Ok(p) => patterns.push(p),
                        Err(e) => dropped.push(dropped_item(pos, item, e.into())),
                    }
                }
                dropped.sort_by_key(|d| d.position);
                self.allowed_servers = Some(patterns);
                Ok(dropped)
            }
            (Key::UpdateMode, Coerced::Enum(name)) => {
                self.update_mode = Some(UpdateMode::from_name(name).ok_or_else(mismatch)?);
                Ok(dropped)
            }
            (Key::OrganizationName, Coerced::String(s)) => {
                if s.chars().any(breaks_single_line) {
                    return Err(InvalidReason::NotSingleLine);
                }
                if s.chars().count() > MAX_ORGANIZATION_NAME_CHARS {
                    return Err(InvalidReason::TooLong {
                        max: MAX_ORGANIZATION_NAME_CHARS,
                    });
                }
                self.organization_name = Some(s.clone());
                Ok(dropped)
            }
            (Key::SupportUrl, Coerced::String(s)) => {
                // The parser silently strips tabs and newlines; refuse them so
                // the shown text is the link that opens.
                if s.chars().any(char::is_control) {
                    return Err(InvalidReason::NotHttpsUrl);
                }
                let url = url::Url::parse(s).map_err(InvalidReason::NotUrl)?;
                if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
                    return Err(InvalidReason::NotHttpsUrl);
                }
                self.support_url = Some(url.as_str().to_owned());
                Ok(dropped)
            }
            (Key::MinimumVersion, Coerced::String(s)) => {
                let v = Version::parse(s).ok_or(InvalidReason::BadVersion)?;
                self.minimum_version = Some(v);
                Ok(dropped)
            }
            (_, Coerced::Bool(b)) => {
                *self.bool_slot(key).ok_or_else(mismatch)? = Some(*b);
                Ok(dropped)
            }
            _ => Err(mismatch()),
        }
    }

    fn set_strict(&mut self, key: Key, strict: StrictValue) {
        match (strict, key) {
            (StrictValue::EmptyList, Key::AllowedServers) => {
                self.allowed_servers = Some(Vec::new())
            }
            (StrictValue::UpdateMode(m), _) => self.update_mode = Some(m),
            (StrictValue::Bool(b), _) => {
                if let Some(slot) = self.bool_slot(key) {
                    *slot = Some(b);
                }
            }
            (StrictValue::EmptyList, _) => {}
        }
    }
}

/// Characters that break "one line" in `OrganizationName` and in managed
/// server display names: controls, the Unicode line and paragraph
/// separators, and bidirectional formatting characters (which can reorder
/// the surrounding text).
pub(super) fn breaks_single_line(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{061C}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// The resolved policy: typed values for enforcement code, the blocking
/// states and the report. `Default` is the empty policy (nothing set).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Policy {
    settings: Settings,
    blocking: Vec<Blocking>,
    rows: Vec<ReportRow>,
    sources: Vec<SourceInfo>,
    client_version: String,
}

/// Resolves `snapshots`, given **highest priority first**, into a
/// [`Policy`]. The order is the caller's: Windows machine before user; Linux
/// files with full trust before files with limited trust and, within a trust
/// level, the later file name first, exactly as [`super::read_policy_dir`]
/// returns them (do not re-sort them by name).
///
/// Report rows are ordered by key (registry order, then unknown names in
/// byte order) and, within a key, by source priority.
pub fn resolve(snapshots: &[Snapshot], ctx: &Context) -> Policy {
    let mut policy = Policy {
        client_version: ctx.client_version.clone(),
        ..Policy::default()
    };
    for snap in snapshots {
        policy.sources.push(SourceInfo {
            id: snap.id.clone(),
            trust: snap.trust,
            trust_problems: snap.trust_problems.clone(),
            error: snap.error.clone(),
        });
        if let Some(error) = &snap.error {
            policy.blocking.push(Blocking::SourceUnreadable {
                source: snap.id.clone(),
                error: error.clone(),
            });
        }
    }

    let names: BTreeSet<&str> = snapshots
        .iter()
        .flat_map(|s| s.values.keys().map(String::as_str))
        .collect();
    let mut unknown_rows = Vec::new();
    for key in Key::ALL {
        if names.contains(key.name()) {
            resolve_key(&mut policy, key, snapshots, ctx);
        }
    }
    for name in names.iter().filter(|n| Key::from_name(n).is_none()) {
        for snap in snapshots {
            if let Some(raw) = snap.values.get(*name) {
                let name = truncate(name, DISPLAY_MAX_CHARS);
                unknown_rows.push(row(&name, snap, RowStatus::Unknown, raw.display()));
            }
        }
    }
    policy.rows.extend(unknown_rows);

    if let (Some(required), Some((core, pre))) = (
        policy.settings.minimum_version,
        client_core(&ctx.client_version),
    ) && (required > core || (required == core && pre))
    {
        policy.blocking.push(Blocking::BelowMinimumVersion {
            required,
            client: ctx.client_version.clone(),
        });
    }
    policy
}

/// The `MAJOR.MINOR.PATCH` core of a client version and whether it is a
/// pre-release. `CARGO_PKG_VERSION` is always semver (Cargo enforces it);
/// build metadata does not affect precedence.
fn client_core(s: &str) -> Option<(Version, bool)> {
    let s = s.split_once('+').map_or(s, |(v, _)| v);
    let (core, pre) = s.split_once('-').map_or((s, false), |(c, _)| (c, true));
    Some((Version::parse(core)?, pre))
}

fn row(name: &str, snap: &Snapshot, status: RowStatus, value: String) -> ReportRow {
    ReportRow {
        key: name.to_owned(),
        source: snap.id.clone(),
        status,
        value,
        dropped: Vec::new(),
        dropped_omitted: 0,
    }
}

fn resolve_key(policy: &mut Policy, key: Key, snapshots: &[Snapshot], ctx: &Context) {
    let spec = key.spec();
    let applicable = spec.applicability.applies(ctx);
    let mut decided = false;
    for snap in snapshots {
        let Some(raw) = snap.values.get(spec.name) else {
            continue;
        };
        let mut r = row(spec.name, snap, RowStatus::Applied, raw.display());
        if !applicable {
            r.status = RowStatus::NotApplicable;
        } else if decided {
            r.status = RowStatus::Overridden;
        } else if snap.trust == Trust::Limited && spec.class == Class::TrustExpanding {
            r.status = RowStatus::IgnoredLimitedTrust;
        } else {
            decided = true;
            let outcome = coerce(spec.value_type, raw)
                .map_err(InvalidReason::from)
                .and_then(|c| policy.settings.set(key, c));
            match outcome {
                Ok(mut dropped) => {
                    r.dropped_omitted = dropped.len().saturating_sub(MAX_DROPPED_REPORTED);
                    dropped.truncate(MAX_DROPPED_REPORTED);
                    r.dropped = dropped;
                }
                Err(reason) => {
                    if let Some(strict) = spec.strict {
                        policy.settings.set_strict(key, strict);
                    }
                    r.status = RowStatus::Invalid {
                        reason,
                        strict_applied: spec.strict.is_some(),
                    };
                }
            }
        }
        policy.rows.push(r);
    }
}

impl Policy {
    /// Whether `key` has a value (valid or strict).
    pub fn is_set(&self, key: Key) -> bool {
        self.settings.is_set(key)
    }

    /// No key is set. Says nothing about the blocking states or the report:
    /// a policy whose only source is unreadable has no settings but is blocked.
    pub fn has_no_settings(&self) -> bool {
        Key::ALL.iter().all(|k| !self.is_set(*k))
    }

    /// The keys whose value differs between `self` and `other` (set versus
    /// unset counts), in registry order. The refresh applies these by their
    /// [`super::RefreshClass`].
    pub fn changed_keys(&self, other: &Policy) -> Vec<Key> {
        Key::ALL
            .into_iter()
            .filter(|k| !self.settings.key_eq(&other.settings, *k))
            .collect()
    }

    /// Whether every key has the same value in both; unlike `==` this ignores
    /// the report, the sources and the blocking states.
    #[cfg(test)]
    pub(crate) fn settings_eq(&self, other: &Policy) -> bool {
        self.settings == other.settings
    }

    /// `ManagedServers`; empty when not set.
    pub fn managed_servers(&self) -> &[ManagedServer] {
        self.settings.managed_servers.as_deref().unwrap_or_default()
    }

    /// `AllowUserServers`; `true` when not set.
    pub fn allow_user_servers(&self) -> bool {
        self.settings.allow_user_servers.unwrap_or(true)
    }

    /// `AllowedServers`; `None` when not set (every server is allowed),
    /// `Some(&[])` allows managed servers only.
    pub fn allowed_servers(&self) -> Option<&[ServerPattern]> {
        self.settings.allowed_servers.as_deref()
    }

    /// `RequireHttps`; `false` when not set.
    pub fn require_https(&self) -> bool {
        self.settings.require_https.unwrap_or(false)
    }

    /// `AllowCertificateExceptions`; `true` when not set.
    pub fn allow_certificate_exceptions(&self) -> bool {
        self.settings.allow_certificate_exceptions.unwrap_or(true)
    }

    /// `UpdateMode`; `None` when not set or not applicable (the app picks
    /// its own default, section 9.3).
    pub fn update_mode(&self) -> Option<UpdateMode> {
        self.settings.update_mode
    }

    /// `AllowPasswordLogin`; `true` when not set.
    pub fn allow_password_login(&self) -> bool {
        self.settings.allow_password_login.unwrap_or(true)
    }

    /// `AllowTokenLogin`; `true` when not set.
    pub fn allow_token_login(&self) -> bool {
        self.settings.allow_token_login.unwrap_or(true)
    }

    /// `AllowSecretsFile`; `None` when not set.
    pub fn allow_secrets_file(&self) -> Option<bool> {
        self.settings.allow_secrets_file
    }

    /// `NotificationPreview`; `None` when not set.
    pub fn notification_preview(&self) -> Option<bool> {
        self.settings.notification_preview
    }

    /// `HardwareAcceleration`; `None` when not set or not applicable.
    pub fn hardware_acceleration(&self) -> Option<bool> {
        self.settings.hardware_acceleration
    }

    /// `LaunchAtLogin`; `None` when not set.
    pub fn launch_at_login(&self) -> Option<bool> {
        self.settings.launch_at_login
    }

    /// `OrganizationName`: one line, at most 80 characters.
    pub fn organization_name(&self) -> Option<&str> {
        self.settings.organization_name.as_deref()
    }

    /// `SupportUrl`: an absolute https URL in the url crate's normalised form.
    pub fn support_url(&self) -> Option<&str> {
        self.settings.support_url.as_deref()
    }

    pub fn minimum_version(&self) -> Option<Version> {
        self.settings.minimum_version
    }

    /// The reasons the app must not connect; empty normally.
    pub fn blocking(&self) -> &[Blocking] {
        &self.blocking
    }

    pub fn is_blocked(&self) -> bool {
        !self.blocking.is_empty()
    }

    /// One row per (source, key name) seen; see [`resolve`] for the order.
    pub fn rows(&self) -> &[ReportRow] {
        &self.rows
    }

    /// Every source in priority order, with trust and error.
    pub fn sources(&self) -> &[SourceInfo] {
        &self.sources
    }

    pub fn client_version(&self) -> &str {
        &self.client_version
    }
}
