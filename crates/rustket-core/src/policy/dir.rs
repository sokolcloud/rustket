//! The Linux JSON directory source (docs/policy.md section 5): regular files
//! `*.json` directly in the directory. Full-trust files outrank limited-trust
//! files; within a trust level the later file name (byte order) wins.
//! [`read_policy_dir`] returns the snapshots in exactly that priority order,
//! which [`super::resolve`] relies on: do not re-sort them by name.
//! Compiles everywhere; ownership and mode checks are unix-only.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use serde::de::{IgnoredAny, MapAccess, Visitor};

use super::source::{
    Snapshot, SourceError, SourceErrorKind, SourceId, Trust, TrustProblem, TrustSubject,
};
use super::value::RawValue;

/// The release policy directory. Release code never takes another one.
pub const MANAGED_DIR: &str = "/etc/rustket/policies/managed";

/// Largest policy file read.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Kind of [`RawValue::Unsupported`] for a top-level name that a file
/// repeats.
pub(crate) const REPEATED_NAME: &str = "repeated name";

/// The release source: [`MANAGED_DIR`], files and directory owned by root.
/// Only on Linux and the other free unixes, where this directory is the
/// policy channel; Windows and macOS have their own readers.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn read_managed_dir() -> Vec<Snapshot> {
    read_policy_dir(Path::new(MANAGED_DIR), Some(0))
}

/// Reads the policy files of `dir`, **highest priority first**, ready for
/// [`super::resolve`]: files with full trust before files with limited trust
/// (a limited file never outranks a trusted one, as `HKCU` ranks below
/// `HKLM`), and within each trust level in descending byte order of the file
/// name, so the later name wins.
///
/// `owner`: the uid files and directory must belong to for full trust (0 in
/// release); `None` skips the ownership and mode checks and is for tests and
/// tools only. A missing directory gives no snapshots; one that cannot be
/// listed gives a [`SourceId::LinuxDirectory`] snapshot carrying the error.
pub fn read_policy_dir(dir: &Path, owner: Option<u32>) -> Vec<Snapshot> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            return vec![Snapshot::unreadable(
                SourceId::LinuxDirectory,
                io_error(SourceErrorKind::CannotListDirectory, &e),
            )];
        }
    };
    let dir_before = owner.map(|_| fs::metadata(dir));
    let dir_problem = match (&dir_before, owner) {
        (Some(Ok(meta)), Some(uid)) => untrusted_owner_or_mode(meta, uid, TrustSubject::Directory),
        (Some(Err(e)), _) => Some(TrustProblem::NotChecked {
            subject: TrustSubject::Directory,
            detail: Some(e.to_string()),
        }),
        _ => None,
    };
    let mut out = Vec::new();
    let mut names = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let name = entry.file_name();
                if is_policy_file_name(&name) {
                    names.push(name);
                }
            }
            Err(e) => out.push(Snapshot::unreadable(
                SourceId::LinuxDirectory,
                io_error(SourceErrorKind::CannotListDirectory, &e),
            )),
        }
    }
    names.sort_unstable_by(|a, b| b.as_encoded_bytes().cmp(a.as_encoded_bytes()));
    let mut files: Vec<Snapshot> = names
        .iter()
        .filter_map(|name| read_file(dir, name, owner, dir_problem.as_ref()))
        .collect();
    // The directory verdict comes from a second lookup of the path; if the
    // path now names another directory, the verdict proves nothing.
    if let Some(Ok(before)) = &dir_before
        && fs::metadata(dir).map_or(true, |after| !same_file(before, &after))
    {
        for snap in &mut files {
            snap.trust = Trust::Limited;
            snap.trust_problems.push(TrustProblem::DirectoryChanged);
        }
    }
    // Stable: each trust level keeps the descending name order.
    let (full, limited): (Vec<_>, Vec<_>) = files.into_iter().partition(|s| s.trust == Trust::Full);
    out.extend(full);
    out.extend(limited);
    out
}

/// `*.json`, not starting with `.`; compared as bytes, so a name that is not
/// UTF-8 still counts.
fn is_policy_file_name(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    !bytes.starts_with(b".") && bytes.ends_with(b".json")
}

/// The file name for [`SourceId::LinuxFile`]: valid UTF-8 unchanged except
/// that a backslash is doubled, each byte that is not part of valid UTF-8 as
/// `\xNN`. The rendering is reversible, so distinct names stay distinct.
fn file_name_text(name: &OsStr) -> String {
    let bytes = name.as_encoded_bytes();
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(&chunk.valid().replace('\\', "\\\\"));
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn io_error(kind: SourceErrorKind, e: &io::Error) -> SourceError {
    SourceError::new(kind).with_detail(e.to_string())
}

/// The trust problems of a file whose metadata is `file` (or whose stat
/// failed), given the directory's problem. Empty without `owner`.
fn trust_problems(
    owner: Option<u32>,
    dir_problem: Option<&TrustProblem>,
    file: Result<&fs::Metadata, &io::Error>,
) -> Vec<TrustProblem> {
    let Some(uid) = owner else {
        return Vec::new();
    };
    let mut out: Vec<TrustProblem> = dir_problem.cloned().into_iter().collect();
    match file {
        Ok(meta) => out.extend(untrusted_owner_or_mode(meta, uid, TrustSubject::File)),
        Err(e) => out.push(TrustProblem::NotChecked {
            subject: TrustSubject::File,
            detail: Some(e.to_string()),
        }),
    }
    out
}

/// `snap` with limited trust when there are `problems`.
fn with_problems(mut snap: Snapshot, problems: Vec<TrustProblem>) -> Snapshot {
    if !problems.is_empty() {
        snap.trust = Trust::Limited;
        snap.trust_problems = problems;
    }
    snap
}

/// One file; `None` when it is not a regular file (a directory named
/// `x.json`, a FIFO, a socket) or was removed since the directory was listed.
fn read_file(
    dir: &Path,
    name: &OsStr,
    owner: Option<u32>,
    dir_problem: Option<&TrustProblem>,
) -> Option<Snapshot> {
    let id = SourceId::LinuxFile(file_name_text(name));
    let path = dir.join(name);
    let mut file = match open(&path) {
        Ok(f) => f,
        Err(e) => {
            let unreadable = Snapshot::unreadable(id, io_error(SourceErrorKind::CannotOpen, &e));
            // A file that exists but cannot be opened (no read permission)
            // is judged by its stat like an opened one.
            let problems = match fs::metadata(&path) {
                Ok(meta) if meta.is_file() => trust_problems(owner, dir_problem, Ok(&meta)),
                // A socket cannot be opened; it is not a policy file either.
                Ok(_) => return None,
                Err(_)
                    if fs::symlink_metadata(&path)
                        .is_err_and(|m| m.kind() == io::ErrorKind::NotFound) =>
                {
                    return None;
                }
                Err(stat) => trust_problems(owner, dir_problem, Err(&stat)),
            };
            return Some(with_problems(unreadable, problems));
        }
    };
    // Every check below uses the opened handle, so the file cannot be
    // swapped between the checks and the read.
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => {
            let unreadable =
                Snapshot::unreadable(id, io_error(SourceErrorKind::CannotReadMetadata, &e));
            return Some(with_problems(
                unreadable,
                trust_problems(owner, dir_problem, Err(&e)),
            ));
        }
    };
    if !meta.is_file() {
        return None;
    }
    let problems = trust_problems(owner, dir_problem, Ok(&meta));
    let unreadable = |error: SourceError| {
        Some(with_problems(
            Snapshot::unreadable(id.clone(), error),
            problems.clone(),
        ))
    };
    if meta.len() > MAX_FILE_BYTES {
        return unreadable(SourceErrorKind::TooLarge.into());
    }
    let mut buf = Vec::new();
    if let Err(e) = (&mut file).take(MAX_FILE_BYTES + 1).read_to_end(&mut buf) {
        return unreadable(io_error(SourceErrorKind::CannotRead, &e));
    }
    if buf.len() as u64 > MAX_FILE_BYTES {
        return unreadable(SourceErrorKind::TooLarge.into());
    }
    let json: serde_json::Value = match serde_json::from_slice(&buf) {
        Ok(v) => v,
        Err(e) => {
            return unreadable(
                SourceError::new(SourceErrorKind::InvalidJson).with_detail(e.to_string()),
            );
        }
    };
    let serde_json::Value::Object(map) = json else {
        return unreadable(SourceErrorKind::NotAnObject.into());
    };
    // serde_json keeps the last of repeated names; a repeated key is invalid
    // instead of one of its values silently winning.
    let repeated = repeated_names(&buf);
    let mut snap = Snapshot::new(id.clone(), Trust::Full);
    snap.values = map
        .iter()
        .map(|(k, v)| {
            let value = if repeated.contains(k) {
                RawValue::Unsupported(REPEATED_NAME.into())
            } else {
                raw(v)
            };
            (k.clone(), value)
        })
        .collect();
    Some(with_problems(snap, problems))
}

/// The top-level names that occur more than once in the JSON object `buf`.
fn repeated_names(buf: &[u8]) -> HashSet<String> {
    struct Names;
    impl<'de> Visitor<'de> for Names {
        type Value = HashSet<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a JSON object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut seen = HashSet::new();
            let mut repeated = HashSet::new();
            while let Some(name) = map.next_key::<String>()? {
                map.next_value::<IgnoredAny>()?;
                if seen.contains(&name) {
                    repeated.insert(name);
                } else {
                    seen.insert(name);
                }
            }
            Ok(repeated)
        }
    }
    let mut de = serde_json::Deserializer::from_slice(buf);
    serde::Deserializer::deserialize_map(&mut de, Names).unwrap_or_default()
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

/// Opens for reading without blocking on a FIFO.
#[cfg(unix)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn untrusted_owner_or_mode(
    meta: &fs::Metadata,
    uid: u32,
    subject: TrustSubject,
) -> Option<TrustProblem> {
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != uid {
        Some(TrustProblem::WrongOwner {
            subject,
            found: meta.uid(),
            expected: uid,
        })
    } else if meta.mode() & 0o022 != 0 {
        Some(TrustProblem::Writable { subject })
    } else {
        None
    }
}

#[cfg(not(unix))]
fn untrusted_owner_or_mode(
    _: &fs::Metadata,
    _: u32,
    subject: TrustSubject,
) -> Option<TrustProblem> {
    Some(TrustProblem::NotChecked {
        subject,
        detail: None,
    })
}

/// JSON to raw value; floats, objects, nulls and integers outside `i64`
/// are unsupported (invalid).
fn raw(v: &serde_json::Value) -> RawValue {
    use serde_json::Value;
    match v {
        Value::Bool(b) => RawValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => RawValue::Integer(i),
            None if n.is_f64() => RawValue::Unsupported("float".into()),
            None => RawValue::Unsupported("integer out of range".into()),
        },
        Value::String(s) => RawValue::String(s.clone()),
        Value::Array(items) => RawValue::List(items.iter().map(raw).collect()),
        Value::Object(_) => RawValue::Unsupported("object".into()),
        Value::Null => RawValue::Unsupported("null".into()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn setup() -> (tempfile::TempDir, u32) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let uid = fs::metadata(dir.path()).unwrap().uid();
        (dir, uid)
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn ids(snaps: &[Snapshot]) -> Vec<String> {
        snaps.iter().map(|s| s.id.to_string()).collect()
    }

    #[test]
    fn missing_directory_is_no_policy() {
        let (dir, uid) = setup();
        assert!(read_policy_dir(&dir.path().join("nope"), Some(uid)).is_empty());
        assert!(read_policy_dir(&dir.path().join("nope"), None).is_empty());
    }

    #[test]
    fn not_a_directory_is_an_error() {
        let (dir, uid) = setup();
        write(dir.path(), "file", "{}");
        let snaps = read_policy_dir(&dir.path().join("file"), Some(uid));
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, SourceId::LinuxDirectory);
        assert_eq!(
            snaps[0].error.as_ref().unwrap().kind,
            SourceErrorKind::CannotListDirectory
        );
        assert!(snaps[0].error.as_ref().unwrap().detail.is_some());
    }

    #[test]
    fn order_names_and_values() {
        let (dir, uid) = setup();
        let d = dir.path();
        write(
            d,
            "10-base.json",
            r#"{"RequireHttps": false, "UpdateMode": "notify"}"#,
        );
        write(d, "20-site.json", r#"{"RequireHttps": true}"#);
        write(d, "9-late.json", r#"{"OrganizationName": "Acme"}"#);
        write(d, ".hidden.json", r#"{"RequireHttps": false}"#);
        write(d, "notes.txt", "{}");
        write(d, "x.json.bak", "{}");
        write(d, "upper.JSON", "{}");
        fs::create_dir(d.join("sub.json")).unwrap();
        let snaps = read_policy_dir(d, Some(uid));
        // Byte order: "10-" < "20-" < "9-"; highest priority (last) first.
        assert_eq!(
            ids(&snaps),
            [
                "linux:9-late.json",
                "linux:20-site.json",
                "linux:10-base.json"
            ]
        );
        assert!(
            snaps
                .iter()
                .all(|s| s.trust == Trust::Full && s.error.is_none())
        );
        assert_eq!(snaps[1].values["RequireHttps"], RawValue::Bool(true));

        let policy = crate::policy::resolve(
            &snaps,
            &crate::policy::Context {
                client_version: "0.4.0".into(),
                os: crate::policy::Os::Linux,
                mas: false,
                self_update: true,
            },
        );
        assert!(policy.require_https(), "the later file wins");
        assert_eq!(
            policy.update_mode(),
            Some(crate::policy::UpdateMode::Notify)
        );
        assert_eq!(policy.organization_name(), Some("Acme"));
    }

    #[test]
    fn json_types() {
        let (dir, uid) = setup();
        write(
            dir.path(),
            "a.json",
            r#"{"b": true, "i": -3, "s": "x", "l": ["a", 1, null], "f": 1.5,
                "o": {}, "n": null, "big": 18446744073709551615}"#,
        );
        let snaps = read_policy_dir(dir.path(), Some(uid));
        let v = &snaps[0].values;
        assert_eq!(v["b"], RawValue::Bool(true));
        assert_eq!(v["i"], RawValue::Integer(-3));
        assert_eq!(v["s"], RawValue::String("x".into()));
        assert_eq!(
            v["l"],
            RawValue::List(vec![
                RawValue::String("a".into()),
                RawValue::Integer(1),
                RawValue::Unsupported("null".into()),
            ])
        );
        assert_eq!(v["f"], RawValue::Unsupported("float".into()));
        assert_eq!(v["o"], RawValue::Unsupported("object".into()));
        assert_eq!(v["n"], RawValue::Unsupported("null".into()));
        assert_eq!(
            v["big"],
            RawValue::Unsupported("integer out of range".into())
        );
    }

    #[test]
    fn unreadable_files() {
        let (dir, uid) = setup();
        let d = dir.path();
        write(d, "a-syntax.json", r#"{"RequireHttps": true,"#);
        write(d, "b-array.json", r#"[{"RequireHttps": true}]"#);
        write(d, "c-string.json", r#""text""#);
        let big = format!(
            r#"{{"OrganizationName": "{}"}}"#,
            "x".repeat(MAX_FILE_BYTES as usize)
        );
        write(d, "d-big.json", &big);
        write(d, "e-empty.json", "");
        write(d, "f-ok.json", r#"{"RequireHttps": true}"#);
        let exact = format!(r#"{{"x": "{}"}}"#, "y".repeat(MAX_FILE_BYTES as usize - 9));
        assert_eq!(exact.len() as u64, MAX_FILE_BYTES);
        write(d, "g-exact.json", &exact);
        let snaps = read_policy_dir(d, Some(uid));
        let by_name = |n: &str| {
            snaps
                .iter()
                .find(|s| s.id == SourceId::LinuxFile(n.into()))
                .unwrap()
        };
        for (n, kind) in [
            ("a-syntax.json", SourceErrorKind::InvalidJson),
            ("b-array.json", SourceErrorKind::NotAnObject),
            ("c-string.json", SourceErrorKind::NotAnObject),
            ("d-big.json", SourceErrorKind::TooLarge),
            ("e-empty.json", SourceErrorKind::InvalidJson),
        ] {
            let s = by_name(n);
            assert_eq!(s.error.as_ref().unwrap().kind, kind, "{n}");
            assert!(s.values.is_empty(), "{n}");
            assert_eq!(s.trust, Trust::Full, "{n}");
        }
        // serde_json's own text is kept as the untranslated detail.
        assert!(
            by_name("a-syntax.json")
                .error
                .as_ref()
                .unwrap()
                .detail
                .is_some()
        );
        assert_eq!(
            by_name("d-big.json").error.as_ref().unwrap().to_string(),
            "larger than 1 MiB"
        );
        assert!(by_name("f-ok.json").error.is_none());
        assert!(by_name("g-exact.json").error.is_none());

        // Unreadable sources block, readable ones are still resolved.
        let policy = crate::policy::resolve(
            &snaps,
            &crate::policy::Context {
                client_version: "0.4.0".into(),
                os: crate::policy::Os::Linux,
                mas: false,
                self_update: true,
            },
        );
        assert_eq!(policy.blocking().len(), 5);
        assert!(policy.require_https());
    }

    #[test]
    fn symlink_to_a_file_is_followed() {
        let (dir, uid) = setup();
        let target = tempfile::tempdir().unwrap();
        write(target.path(), "real", r#"{"RequireHttps": true}"#);
        write(target.path(), "shared", r#"{"RequireHttps": true}"#);
        fs::set_permissions(
            target.path().join("shared"),
            fs::Permissions::from_mode(0o664),
        )
        .unwrap();
        std::os::unix::fs::symlink(target.path().join("real"), dir.path().join("link.json"))
            .unwrap();
        std::os::unix::fs::symlink(target.path().join("shared"), dir.path().join("gw.json"))
            .unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join("dangling.json"))
            .unwrap();
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(
            ids(&snaps),
            ["linux:link.json", "linux:gw.json", "linux:dangling.json"]
        );
        assert_eq!(snaps[0].values["RequireHttps"], RawValue::Bool(true));
        assert_eq!(snaps[0].trust, Trust::Full);
        // The checks apply to the target, not to the link.
        assert_eq!(snaps[1].trust, Trust::Limited);
        assert_eq!(
            snaps[1].trust_problems,
            [TrustProblem::Writable {
                subject: TrustSubject::File
            }]
        );
        assert_eq!(snaps[1].values["RequireHttps"], RawValue::Bool(true));
        assert_eq!(
            snaps[2].error.as_ref().map(|e| e.kind),
            Some(SourceErrorKind::CannotOpen),
            "a dangling link is unreadable"
        );
        assert_eq!(snaps[2].trust, Trust::Limited);
        assert!(matches!(
            snaps[2].trust_problems[..],
            [TrustProblem::NotChecked {
                subject: TrustSubject::File,
                ..
            }]
        ));
    }

    #[test]
    fn trust_checks() {
        let (dir, uid) = setup();
        let d = dir.path();
        write(d, "a.json", r#"{"SupportUrl": "https://help.example"}"#);
        write(d, "b.json", r#"{"SupportUrl": "https://other.example"}"#);
        fs::set_permissions(d.join("b.json"), fs::Permissions::from_mode(0o664)).unwrap();
        write(d, "c.json", "{}");
        fs::set_permissions(d.join("c.json"), fs::Permissions::from_mode(0o646)).unwrap();
        let snaps = read_policy_dir(d, Some(uid));
        // Full files first, then limited ones, each by descending name.
        assert_eq!(
            ids(&snaps),
            ["linux:a.json", "linux:c.json", "linux:b.json"]
        );
        assert_eq!(snaps[0].trust, Trust::Full);
        assert!(snaps[0].trust_problems.is_empty());
        for s in &snaps[1..] {
            assert_eq!(s.trust, Trust::Limited, "{}", s.id);
            assert_eq!(
                s.trust_problems,
                [TrustProblem::Writable {
                    subject: TrustSubject::File
                }]
            );
        }
        let policy = crate::policy::resolve(
            &snaps,
            &crate::policy::Context {
                client_version: "0.4.0".into(),
                os: crate::policy::Os::Linux,
                mas: false,
                self_update: true,
            },
        );
        // The group-writable file may not set a class T key.
        assert_eq!(policy.support_url(), Some("https://help.example/"));

        // Wrong owner: everything limited.
        let snaps = read_policy_dir(d, Some(uid.wrapping_add(1)));
        assert!(snaps.iter().all(|s| s.trust == Trust::Limited));
        let wrong_owner = |subject| TrustProblem::WrongOwner {
            subject,
            found: uid,
            expected: uid.wrapping_add(1),
        };
        assert_eq!(
            snaps[2].trust_problems,
            [
                wrong_owner(TrustSubject::Directory),
                wrong_owner(TrustSubject::File)
            ]
        );
        assert_eq!(
            snaps[2].trust_problems[1].to_string(),
            format!(
                "the file is owned by uid {uid}, not {}",
                uid.wrapping_add(1)
            )
        );
        assert_eq!(snaps[2].trust_problems[1].code(), "file_wrong_owner");
        // No owner: no checks.
        let snaps = read_policy_dir(d, None);
        assert!(snaps.iter().all(|s| s.trust == Trust::Full));

        // A group-writable directory limits every file.
        fs::set_permissions(d, fs::Permissions::from_mode(0o775)).unwrap();
        let snaps = read_policy_dir(d, Some(uid));
        assert!(snaps.iter().all(|s| s.trust == Trust::Limited));
        assert_eq!(
            ids(&snaps),
            ["linux:c.json", "linux:b.json", "linux:a.json"]
        );
        let writable = |subject| TrustProblem::Writable { subject };
        assert_eq!(
            snaps[0].trust_problems,
            [
                writable(TrustSubject::Directory),
                writable(TrustSubject::File)
            ]
        );
        assert_eq!(snaps[2].trust_problems, [writable(TrustSubject::Directory)]);
        assert_eq!(snaps[2].trust_problems[0].code(), "directory_writable");
        assert_eq!(
            snaps[2].trust_problems[0].to_string(),
            "the directory is writable by group or others"
        );
    }

    #[test]
    fn fifo_is_skipped_without_blocking() {
        let (dir, uid) = setup();
        let fifo = dir.path().join("pipe.json");
        // std cannot create a FIFO; use mkfifo(1) when it is there.
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("fifo_is_skipped_without_blocking: mkfifo unavailable, test not run");
            return;
        }
        write(dir.path(), "a.json", "{}");
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(ids(&snaps), ["linux:a.json"]);
    }

    fn ctx() -> crate::policy::Context {
        crate::policy::Context {
            client_version: "0.4.0".into(),
            os: crate::policy::Os::Linux,
            mas: false,
            self_update: true,
        }
    }

    #[test]
    fn socket_is_skipped_without_blocking() {
        let (dir, uid) = setup();
        let _listener =
            std::os::unix::net::UnixListener::bind(dir.path().join("sock.json")).unwrap();
        write(dir.path(), "a.json", "{}");
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(ids(&snaps), ["linux:a.json"]);
    }

    #[test]
    fn non_utf8_names_are_read() {
        use std::os::unix::ffi::OsStrExt;
        let (dir, uid) = setup();
        let name = std::ffi::OsStr::from_bytes(b"\xff-strict.json");
        let p = dir.path().join(name);
        if fs::write(&p, r#"{"RequireHttps": true}"#).is_err() {
            eprintln!("non_utf8_names_are_read: file system refuses the name, test not run");
            return;
        }
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        // A second invalid byte must not collide with the first.
        let other = dir
            .path()
            .join(std::ffi::OsStr::from_bytes(b"\xfe-strict.json"));
        fs::write(&other, "{}").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o644)).unwrap();
        // Valid UTF-8 stays as it is.
        write(dir.path(), "ü.json", "{}");
        write(dir.path(), "a.json", r#"{"RequireHttps": false}"#);
        let snaps = read_policy_dir(dir.path(), Some(uid));
        // 0xff sorts after "a": highest priority.
        assert_eq!(
            ids(&snaps),
            [
                "linux:\\xff-strict.json",
                "linux:\\xfe-strict.json",
                "linux:ü.json",
                "linux:a.json"
            ]
        );
        assert!(crate::policy::resolve(&snaps, &ctx()).require_https());
    }

    /// A literal backslash is doubled, so the rendering can be reversed and
    /// a name spelled `\xff` never reads like the byte 0xff.
    #[test]
    fn file_names_render_reversibly() {
        use std::os::unix::ffi::OsStrExt;
        let text = |b: &[u8]| file_name_text(std::ffi::OsStr::from_bytes(b));
        assert_eq!(text(b"a.json"), "a.json");
        assert_eq!(text("ü.json".as_bytes()), "ü.json");
        // The byte 0xff and the four characters `\xff`.
        assert_eq!(text(b"\xff.json"), "\\xff.json");
        assert_eq!(text(b"\\xff.json"), "\\\\xff.json");
        assert_ne!(text(b"\xff.json"), text(b"\\xff.json"));
        assert_eq!(text(b"a\\b\xfe"), "a\\\\b\\xfe");
        assert_eq!(
            SourceId::LinuxFile(text(b"x\\y.json")).to_string(),
            "linux:x\\\\y.json"
        );
    }

    #[test]
    fn repeated_names_are_invalid_keys() {
        let (dir, uid) = setup();
        write(
            dir.path(),
            "a.json",
            r#"{"RequireHttps": false, "UpdateMode": "notify", "RequireHttps": false,
                "OrganizationName": "A", "OrganizationName": "B", "NotificationPreview": true}"#,
        );
        write(dir.path(), "b.json", r#"{"x": {"y": 1, "y": 2}}"#);
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(ids(&snaps), ["linux:b.json", "linux:a.json"]);
        assert!(snaps[0].error.is_none(), "only top-level names count");
        assert!(snaps[1].error.is_none(), "the file stays readable");
        let repeated = RawValue::Unsupported(REPEATED_NAME.into());
        assert_eq!(snaps[1].values["RequireHttps"], repeated);
        assert_eq!(snaps[1].values["OrganizationName"], repeated);
        assert_eq!(
            snaps[1].values["UpdateMode"],
            RawValue::String("notify".into())
        );

        let p = crate::policy::resolve(&snaps, &ctx());
        assert!(!p.is_blocked());
        // A repeated restrictive key gets its strict value...
        assert!(p.require_https());
        let row = p.rows().iter().find(|r| r.key == "RequireHttps").unwrap();
        assert!(matches!(
            row.status,
            crate::policy::RowStatus::Invalid {
                strict_applied: true,
                ..
            }
        ));
        // ...one without a strict value is ignored...
        assert_eq!(p.organization_name(), None);
        // ...and the other keys of the file apply.
        assert_eq!(p.update_mode(), Some(crate::policy::UpdateMode::Notify));
        assert_eq!(p.notification_preview(), Some(true));
    }

    #[test]
    fn limited_file_never_outranks_a_full_one() {
        let (dir, uid) = setup();
        let d = dir.path();
        write(
            d,
            "50-corp.json",
            r#"{"RequireHttps": true, "AllowedServers": ["chat.corp.com"],
                "AllowCertificateExceptions": false}"#,
        );
        write(
            d,
            "90-team.json",
            r#"{"RequireHttps": false, "AllowedServers": ["*.com"],
                "AllowCertificateExceptions": true, "LaunchAtLogin": true}"#,
        );
        fs::set_permissions(d.join("90-team.json"), fs::Permissions::from_mode(0o664)).unwrap();
        let snaps = read_policy_dir(d, Some(uid));
        assert_eq!(ids(&snaps), ["linux:50-corp.json", "linux:90-team.json"]);
        assert_eq!(snaps[1].trust, Trust::Limited);
        let p = crate::policy::resolve(&snaps, &ctx());
        assert!(p.require_https());
        assert!(!p.allow_certificate_exceptions());
        assert_eq!(p.allowed_servers().unwrap()[0].host, "chat.corp.com");
        // Keys the trusted file leaves unset still come from the limited one.
        assert_eq!(p.launch_at_login(), Some(true));
        // Without ownership checks both are full and the later name wins.
        let snaps = read_policy_dir(d, None);
        assert_eq!(ids(&snaps), ["linux:90-team.json", "linux:50-corp.json"]);
    }

    #[test]
    fn unreadable_file_keeps_its_trust_verdict() {
        let (dir, uid) = setup();
        write(dir.path(), "zz.json", "{ broken");
        fs::set_permissions(
            dir.path().join("zz.json"),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(
            snaps[0].error.as_ref().map(|e| e.kind),
            Some(SourceErrorKind::InvalidJson)
        );
        assert_eq!(snaps[0].trust, Trust::Limited);
        assert_eq!(
            snaps[0].trust_problems,
            [TrustProblem::Writable {
                subject: TrustSubject::File
            }]
        );
    }

    #[test]
    fn permission_denied() {
        let (dir, uid) = setup();
        if uid == 0 {
            eprintln!("permission_denied: running as root, test not run");
            return;
        }
        write(dir.path(), "locked.json", r#"{"RequireHttps": true}"#);
        fs::set_permissions(
            dir.path().join("locked.json"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(ids(&snaps), ["linux:locked.json"]);
        let error = snaps[0].error.as_ref().unwrap();
        assert_eq!(error.kind, SourceErrorKind::CannotOpen);
        assert!(error.detail.is_some());
        // The stat still works: owned by the expected uid, not writable by
        // others, so the file keeps full trust and still blocks.
        assert_eq!(snaps[0].trust, Trust::Full);
        assert!(snaps[0].trust_problems.is_empty());
        assert!(crate::policy::resolve(&snaps, &ctx()).is_blocked());
        // The same file judged against another owner is limited by its stat.
        let snaps = read_policy_dir(dir.path(), Some(uid.wrapping_add(1)));
        assert_eq!(snaps[0].trust, Trust::Limited);
        assert!(matches!(
            snaps[0].trust_problems[..],
            [
                TrustProblem::WrongOwner {
                    subject: TrustSubject::Directory,
                    ..
                },
                TrustProblem::WrongOwner {
                    subject: TrustSubject::File,
                    ..
                }
            ]
        ));

        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
        let snaps = read_policy_dir(&sub, Some(uid));
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, SourceId::LinuxDirectory);
        assert_eq!(
            snaps[0].error.as_ref().map(|e| e.kind),
            Some(SourceErrorKind::CannotListDirectory)
        );
    }

    const CI_FIXTURE: &str = include_str!("../../tests/fixtures/policy/ci-managed.json");

    /// The values of `tests/fixtures/policy/ci-managed.json`.
    fn ci_fixture_values() -> std::collections::BTreeMap<String, RawValue> {
        let s = |v: &str| RawValue::String(v.into());
        [
            ("OrganizationName", s("Rustket CI")),
            ("SupportUrl", s("https://help.example.com/chat")),
            ("NotificationPreview", RawValue::Bool(false)),
            (
                "AllowedServers",
                RawValue::List(vec![s("chat.example.com"), s("*.example.org")]),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }

    #[test]
    fn ci_fixture_reads_as_expected() {
        let (dir, uid) = setup();
        write(dir.path(), "50-rustket-ci.json", CI_FIXTURE);
        let snaps = read_policy_dir(dir.path(), Some(uid));
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].error, None);
        assert_eq!(snaps[0].values, ci_fixture_values());
    }

    /// Opt-in: the release source reads the file that the Linux CI job
    /// installs as root (`.github/workflows/ci.yml`, from
    /// `tests/fixtures/policy/ci-managed.json`) with full trust.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn managed_dir_round_trip() {
        const ENV: &str = "RUSTKET_TEST_POLICY_ETC";
        const NAME: &str = "50-rustket-ci.json";
        if std::env::var_os(ENV).is_none_or(|v| v != "1") {
            eprintln!("skipped: set {ENV}=1 after installing {MANAGED_DIR}/{NAME} as root");
            return;
        }
        let path = Path::new(MANAGED_DIR).join(NAME);
        let installed = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("{}: {e}; the CI job installs it with sudo", path.display())
        });
        assert_eq!(
            installed,
            CI_FIXTURE,
            "{} is not the fixture",
            path.display()
        );

        let snaps = read_managed_dir();
        let snap = snaps
            .iter()
            .find(|s| s.id == SourceId::LinuxFile(NAME.into()))
            .unwrap_or_else(|| panic!("no snapshot of {NAME}: {snaps:?}"));
        assert_eq!(snap.error, None);
        assert_eq!(snap.trust, Trust::Full, "{:?}", snap.trust_problems);
        assert!(snap.trust_problems.is_empty());
        assert_eq!(snap.values, ci_fixture_values());

        // A trust-expanding key applies only from a trusted file.
        let policy = crate::policy::resolve(&snaps, &ctx());
        assert_eq!(policy.support_url(), Some("https://help.example.com/chat"));
        assert_eq!(policy.organization_name(), Some("Rustket CI"));
    }
}
