//! Enterprise policy core (docs/policy.md): the key registry, raw values and
//! their coercion, resolution of source snapshots into an immutable
//! [`Policy`] with its report, the preferences overlay, server items and
//! patterns, the Linux JSON directory source and the `--dump-policy` JSON.
//!
//! Pure data and `std`; the only OS access is reading the policy directory.
//! The Windows and macOS readers live in the app and produce [`Snapshot`]s.

mod dir;
mod dump;
mod prefs;
mod registry;
mod resolve;
mod servers;
mod source;
mod value;

#[cfg(test)]
mod tests;

#[cfg(all(unix, not(target_os = "macos")))]
pub use dir::read_managed_dir;
pub use dir::{MANAGED_DIR, MAX_FILE_BYTES, read_policy_dir};
pub use dump::{DUMP_FORMAT, dump_json};
pub use prefs::LockedPref;
pub use registry::{
    Applicability, Class, Key, KeySpec, Os, RefreshClass, StrictValue, UpdateMode, ValueType, specs,
};
pub use resolve::{
    Blocking, Context, InvalidReason, MAX_DROPPED_REPORTED, MAX_ORGANIZATION_NAME_CHARS, Policy,
    ReportRow, RowStatus, SourceInfo, Version, resolve,
};
pub use servers::{
    MAX_DISPLAY_NAME_CHARS, ManagedServer, Scheme, ServerAccess, ServerItemError, ServerPattern,
};
pub use source::{
    Snapshot, SourceError, SourceErrorKind, SourceId, Trust, TrustProblem, TrustSubject,
};
pub use value::{
    CoerceError, Coerced, Coercion, DISPLAY_MAX_CHARS, DropReason, DroppedItem, RawValue, coerce,
    display_text,
};
