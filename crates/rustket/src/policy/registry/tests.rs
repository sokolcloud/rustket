//! The Windows mapping with a fake registry.

use std::cell::Cell;

use rustket_core::policy::{Context, Os, RowStatus, Trust, resolve};

use super::*;

/// A key of the fake registry; an error field makes that call fail.
#[derive(Debug, Clone, Default)]
struct Fake {
    values: Vec<(String, RegData)>,
    subkeys: Vec<(String, Fake)>,
    values_error: Option<RegError>,
    subkeys_error: Option<RegError>,
    /// Opening this key (as a subkey) fails.
    open_error: Option<RegError>,
}

impl Fake {
    fn value(mut self, name: &str, data: RegData) -> Self {
        self.values.push((name.into(), data));
        self
    }

    fn sub(mut self, name: &str, key: Fake) -> Self {
        self.subkeys.push((name.into(), key));
        self
    }
}

impl RegKey for Fake {
    fn values(&self) -> Result<Vec<(String, RegData)>, RegError> {
        match &self.values_error {
            Some(e) => Err(e.clone()),
            None => Ok(self.values.clone()),
        }
    }

    fn subkeys(&self) -> Result<Vec<String>, RegError> {
        match &self.subkeys_error {
            Some(e) => Err(e.clone()),
            None => Ok(self.subkeys.iter().map(|(n, _)| n.clone()).collect()),
        }
    }

    fn open(&self, name: &str) -> Result<Self, RegError> {
        let sub = self
            .subkeys
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, k)| k)
            .ok_or_else(not_found)?;
        match &sub.open_error {
            Some(e) => Err(e.clone()),
            None => Ok(sub.clone()),
        }
    }
}

fn not_found() -> RegError {
    RegError {
        not_found: true,
        detail: Some("The system cannot find the file specified.".into()),
    }
}

fn denied() -> RegError {
    RegError {
        not_found: false,
        detail: Some("Access is denied. (0x80070005)".into()),
    }
}

fn sz(s: &str) -> RegData {
    RegData::Sz(s.into())
}

fn s(v: &str) -> RawValue {
    RawValue::String(v.into())
}

fn machine(key: Fake) -> Snapshot {
    read_source(SourceId::WindowsMachine, move || Ok(key.clone()))
}

/// The UTF-16LE bytes of `s` with a terminating NUL.
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[test]
fn hresults_of_not_found() {
    let hr = |code: u32| i32::from_ne_bytes(code.to_ne_bytes());
    assert!(is_not_found(hr(0x8007_0002)), "ERROR_FILE_NOT_FOUND");
    assert!(is_not_found(hr(0x8007_0003)), "ERROR_PATH_NOT_FOUND");
    assert!(!is_not_found(hr(0x8007_0005)), "ERROR_ACCESS_DENIED");
    assert!(!is_not_found(2), "a bare Win32 code is not an HRESULT");
    assert!(!is_not_found(0));
}

#[test]
fn raw_data_decoding() {
    use RegType as T;
    assert_eq!(
        RegData::decode(T::Dword, &7u32.to_le_bytes()),
        RegData::Dword(7)
    );
    assert_eq!(
        RegData::decode(T::Qword, &u64::MAX.to_le_bytes()),
        RegData::Qword(u64::MAX)
    );
    assert_eq!(
        RegData::decode(T::Dword, &[1, 0]),
        RegData::Other("REG_DWORD of 2 bytes".into())
    );
    assert_eq!(
        RegData::decode(T::Qword, &[1, 0, 0, 0]),
        RegData::Other("REG_QWORD of 4 bytes".into())
    );
    assert_eq!(RegData::decode(T::Sz, &utf16("Café")), sz("Café"));
    assert_eq!(
        RegData::decode(T::ExpandSz, &utf16("%APPDATA%\\x")),
        RegData::ExpandSz("%APPDATA%\\x".into()),
        "no expansion"
    );
    // Without a terminator, and ending at an embedded NUL.
    assert_eq!(RegData::decode(T::Sz, &utf16("ab")[..4]), sz("ab"));
    let mut embedded = utf16("ab");
    embedded.extend(utf16("cd"));
    assert_eq!(RegData::decode(T::Sz, &embedded), sz("ab"));
    assert_eq!(RegData::decode(T::Sz, &[]), sz(""));
    // An unpaired surrogate.
    assert_eq!(
        RegData::decode(T::Sz, &[0x00, 0xD8, 0, 0]),
        RegData::Other("REG_SZ that is not UTF-16".into())
    );
    let mut multi = utf16("a.com");
    multi.extend(utf16(""));
    multi.extend(utf16("b.com"));
    multi.extend([0, 0]);
    let RegData::MultiSz(items) = RegData::decode(T::MultiSz, &multi) else {
        panic!("not a multi-string");
    };
    let items: Vec<&str> = items
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(items, ["a.com", "b.com"]);
    assert_eq!(
        RegData::decode(T::MultiSz, &[0x00, 0xDC, 0, 0]),
        RegData::Other("REG_MULTI_SZ that is not UTF-16".into())
    );
    assert_eq!(
        RegData::decode(T::Binary, &[1, 2]),
        RegData::Other("REG_BINARY".into())
    );
    assert_eq!(
        RegData::decode(T::Other(0), &[]),
        RegData::Other("REG_NONE".into())
    );
    assert_eq!(
        RegData::decode(T::Other(5), &[0, 0, 0, 1]),
        RegData::Other("REG_DWORD_BIG_ENDIAN".into())
    );
    assert_eq!(
        RegData::decode(T::Other(42), &[]),
        RegData::Other("registry type 42".into())
    );
}

#[test]
fn every_type_mapping() {
    let snap = machine(
        Fake::default()
            .value("AllowUserServers", RegData::Dword(0))
            .value("RequireHttps", RegData::Qword(1))
            .value("OrganizationName", sz("Example Corp"))
            .value(
                "SupportUrl",
                RegData::ExpandSz("https://help/%USER%".into()),
            )
            .value(
                "AllowedServers",
                RegData::MultiSz(vec![
                    "a.com".into(),
                    String::new(),
                    "b.com".into(),
                    String::new(),
                ]),
            )
            .value("UpdateMode", RegData::Other("REG_BINARY".into())),
    );
    assert_eq!(snap.id, SourceId::WindowsMachine);
    assert_eq!(snap.trust, Trust::Full);
    assert_eq!(snap.error, None);
    let want: BTreeMap<String, RawValue> = [
        ("AllowUserServers", RawValue::Integer(0)),
        ("RequireHttps", RawValue::Integer(1)),
        ("OrganizationName", s("Example Corp")),
        ("SupportUrl", s("https://help/%USER%")),
        (
            "AllowedServers",
            RawValue::List(vec![s("a.com"), s("b.com")]),
        ),
        ("UpdateMode", RawValue::Unsupported("REG_BINARY".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(snap.values, want);
}

#[test]
fn dword_and_qword_ranges() {
    let snap = machine(
        Fake::default()
            .value("AllowTokenLogin", RegData::Dword(u32::MAX))
            .value("AllowPasswordLogin", RegData::Qword(i64::MAX as u64))
            .value("LaunchAtLogin", RegData::Qword(i64::MAX as u64 + 1)),
    );
    assert_eq!(
        snap.values["AllowTokenLogin"],
        RawValue::Integer(i64::from(u32::MAX))
    );
    assert_eq!(
        snap.values["AllowPasswordLogin"],
        RawValue::Integer(i64::MAX)
    );
    assert!(
        matches!(&snap.values["LaunchAtLogin"], RawValue::Unsupported(k) if k.contains("REG_QWORD")),
        "{:?}",
        snap.values["LaunchAtLogin"]
    );
}

#[test]
fn names_differing_in_case_use_the_key_spelling() {
    let snap = machine(
        Fake::default()
            .value("requirehttps", RegData::Dword(1))
            .value("ALLOWUSERSERVERS", RegData::Dword(0))
            .sub("allowedservers", Fake::default().value("1", sz("a.com"))),
    );
    let names: Vec<&str> = snap.values.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["AllowUserServers", "AllowedServers", "RequireHttps"]
    );
}

#[test]
fn names_collapsing_to_one_key_are_unsupported() {
    let snap = machine(
        Fake::default()
            .value("RequireHttps", RegData::Dword(0))
            .value("requirehttps", RegData::Dword(1))
            .sub("AllowedServers", Fake::default().value("1", sz("a")))
            .sub("ALLOWEDSERVERS", Fake::default().value("1", sz("b"))),
    );
    assert!(matches!(
        snap.values["RequireHttps"],
        RawValue::Unsupported(_)
    ));
    assert!(matches!(
        snap.values["AllowedServers"],
        RawValue::Unsupported(_)
    ));
    assert_eq!(snap.values.len(), 2);
}

#[test]
fn unknown_value_and_subkey_are_passed_through() {
    let snap = machine(
        Fake::default()
            .value("RequireHtps", RegData::Dword(1))
            .value("Extra", sz("x"))
            .sub("Servers", Fake::default().value("1", sz("a.com")))
            .sub("Extra", Fake::default()),
    );
    assert_eq!(snap.values["RequireHtps"], RawValue::Integer(1));
    assert_eq!(
        snap.values["Servers"],
        RawValue::Unsupported("registry key".into())
    );
    assert_eq!(
        snap.values["Extra"],
        RawValue::Unsupported("registry key".into()),
        "the subkey wins"
    );
    let p = resolve(&[snap], &ctx());
    assert!(
        p.rows().iter().all(|r| r.status == RowStatus::Unknown),
        "{:?}",
        p.rows()
    );
    assert_eq!(p.rows().len(), 3);
}

fn ctx() -> Context {
    Context {
        client_version: "0.4.0".into(),
        os: Os::Windows,
        mas: false,
        self_update: true,
    }
}

#[test]
fn list_subkey_order() {
    let snap = machine(
        Fake::default().sub(
            "AllowedServers",
            Fake::default()
                .value("b", sz("b"))
                .value("10", sz("ten"))
                .value("a", sz("a"))
                .value("2", sz("two"))
                .value("1", sz("one"))
                .value("B", sz("B"))
                .value("02", sz("zero-two")),
        ),
    );
    assert_eq!(
        snap.values["AllowedServers"],
        RawValue::List(
            ["one", "zero-two", "two", "ten", "B", "a", "b"]
                .map(s)
                .to_vec()
        )
    );
    assert_eq!(value_name_order("2", "10"), Ordering::Less);
    assert_eq!(value_name_order("99", "a"), Ordering::Less);
    assert_eq!(value_name_order("", "1"), Ordering::Greater);
}

#[test]
fn subkey_beats_value() {
    let snap = machine(
        Fake::default()
            .value("AllowedServers", sz("value.com"))
            .sub("allowedServers", Fake::default().value("1", sz("sub.com"))),
    );
    assert_eq!(
        snap.values["AllowedServers"],
        RawValue::List(vec![s("sub.com")])
    );
}

#[test]
fn non_string_inside_a_list_subkey() {
    let snap = machine(
        Fake::default().sub(
            "ManagedServers",
            Fake::default()
                .value("1", sz("https://a.com"))
                .value("2", RegData::Dword(5))
                .value("3", RegData::ExpandSz("https://b.com".into()))
                .value("4", RegData::MultiSz(vec!["x".into()])),
        ),
    );
    assert_eq!(
        snap.values["ManagedServers"],
        RawValue::List(vec![
            s("https://a.com"),
            RawValue::Unsupported("REG_DWORD".into()),
            s("https://b.com"),
            RawValue::Unsupported("REG_MULTI_SZ".into()),
        ])
    );
    let p = resolve(&[snap], &ctx());
    assert_eq!(p.managed_servers().len(), 2);
    assert_eq!(p.rows()[0].status, RowStatus::Applied);
    assert_eq!(p.rows()[0].dropped.len(), 2);
}

#[test]
fn missing_key_is_an_empty_source() {
    let snap = read_source::<Fake>(SourceId::WindowsUser, || Err(not_found()));
    assert_eq!(snap.id, SourceId::WindowsUser);
    assert_eq!(snap.trust, Trust::Limited);
    assert!(snap.values.is_empty());
    assert_eq!(snap.error, None);
}

#[test]
fn access_denied_on_open() {
    let snap = read_source::<Fake>(SourceId::WindowsMachine, || Err(denied()));
    let e = snap.error.unwrap();
    assert_eq!(e.kind, SourceErrorKind::CannotOpen);
    assert_eq!(e.detail.as_deref(), Some("Access is denied. (0x80070005)"));
    assert_eq!(snap.trust, Trust::Full);
}

#[test]
fn access_denied_on_enumeration() {
    let values = Fake {
        values_error: Some(denied()),
        ..Fake::default()
    };
    let e = machine(values).error.unwrap();
    assert_eq!(e.kind, SourceErrorKind::CannotRead);
    assert_eq!(e.detail.as_deref(), Some("Access is denied. (0x80070005)"));

    let subkeys = Fake {
        subkeys_error: Some(denied()),
        ..Fake::default()
    };
    assert_eq!(
        machine(subkeys).error.unwrap().kind,
        SourceErrorKind::CannotRead
    );

    let list_values = Fake::default().sub(
        "AllowedServers",
        Fake {
            values_error: Some(denied()),
            ..Fake::default()
        },
    );
    assert_eq!(
        machine(list_values).error.unwrap().kind,
        SourceErrorKind::CannotRead
    );

    let list_open = Fake::default().sub(
        "AllowedServers",
        Fake {
            open_error: Some(denied()),
            ..Fake::default()
        },
    );
    assert_eq!(
        machine(list_open).error.unwrap().kind,
        SourceErrorKind::CannotOpen
    );

    // An unknown subkey is never opened.
    let unknown = Fake::default().sub(
        "Other",
        Fake {
            open_error: Some(denied()),
            ..Fake::default()
        },
    );
    assert_eq!(machine(unknown).error, None);
}

/// A registry whose key is `states[i]` at the i-th read (the last one
/// after that); returns the snapshot and the number of reads.
fn read_changing(states: Vec<Fake>) -> (Snapshot, usize) {
    let reads = Cell::new(0);
    let snap = read_source(SourceId::WindowsMachine, || {
        let i = reads.get();
        reads.set(i + 1);
        Ok(states[i.min(states.len() - 1)].clone())
    });
    (snap, reads.get())
}

fn https(on: u32) -> Fake {
    Fake::default().value("RequireHttps", RegData::Dword(on))
}

#[test]
fn stable_at_once() {
    let (snap, reads) = read_changing(vec![https(1)]);
    assert_eq!(reads, 2);
    assert_eq!(snap.values["RequireHttps"], RawValue::Integer(1));
}

#[test]
fn changes_once_then_stable() {
    let (snap, reads) = read_changing(vec![Fake::default(), https(1)]);
    assert_eq!(reads, 3);
    assert_eq!(snap.error, None);
    assert_eq!(snap.values["RequireHttps"], RawValue::Integer(1));

    // A list subkey whose item changes counts as a change too.
    let list = |v: &str| Fake::default().sub("AllowedServers", Fake::default().value("1", sz(v)));
    let (snap, reads) = read_changing(vec![list("a"), list("b"), list("c"), list("c")]);
    assert_eq!(reads, 4);
    assert_eq!(snap.values["AllowedServers"], RawValue::List(vec![s("c")]));
}

#[test]
fn never_stable() {
    let (snap, reads) = read_changing(vec![https(0), https(1), https(0), https(1), https(1)]);
    assert_eq!(reads, MAX_READS);
    let e = snap.error.unwrap();
    assert_eq!(e.kind, SourceErrorKind::Unstable);
    assert_eq!(e.detail, None, "no text of the app's own in the detail");
    assert!(snap.values.is_empty());

    // Alternating between missing and present never agrees either.
    let reads = Cell::new(0);
    let snap = read_source(SourceId::WindowsMachine, || {
        reads.set(reads.get() + 1);
        if reads.get() % 2 == 0 {
            Ok(https(1))
        } else {
            Err(not_found())
        }
    });
    assert!(snap.error.is_some());
}

#[test]
fn value_order_does_not_matter_between_reads() {
    let a = https(1).value("AllowUserServers", RegData::Dword(0));
    let b = Fake::default()
        .value("AllowUserServers", RegData::Dword(0))
        .value("RequireHttps", RegData::Dword(1));
    let (snap, reads) = read_changing(vec![a, b]);
    assert_eq!(reads, 2);
    assert_eq!(snap.values.len(), 2);
}

#[test]
fn a_panicking_enumeration_is_a_failed_read() {
    assert_eq!(enumerate(|| 7), Ok(7));
    let e = enumerate::<()>(|| panic!("debug_assert in the iterator")).unwrap_err();
    assert!(!e.not_found);
    // The detail is the panic's own text, never one the app writes.
    assert_eq!(e.detail.as_deref(), Some("debug_assert in the iterator"));
    let n = 3;
    let formatted = enumerate::<()>(|| panic!("left {n}")).unwrap_err();
    assert_eq!(formatted.detail.as_deref(), Some("left 3"));
    let silent = enumerate::<()>(|| std::panic::panic_any(7u8)).unwrap_err();
    assert_eq!(silent.detail, None);
    let snap = machine(Fake {
        values_error: Some(silent.clone()),
        ..Fake::default()
    });
    assert_eq!(snap.error.unwrap().detail, None);
    let _: &dyn std::error::Error = &silent;
    // A failed enumeration followed by two equal reads is a stable read.
    let reads = Cell::new(0);
    let snap = read_source(SourceId::WindowsMachine, || {
        reads.set(reads.get() + 1);
        Ok(if reads.get() == 1 {
            Fake {
                values_error: Some(e.clone()),
                ..Fake::default()
            }
        } else {
            https(1)
        })
    });
    assert_eq!(reads.get(), 3);
    assert_eq!(snap.error, None);
    assert_eq!(snap.values["RequireHttps"], RawValue::Integer(1));
}

/// A list subkey without values is absent (section 4): a disabled ADMX
/// list policy leaves one, meaning "no restriction". An explicitly empty
/// `REG_MULTI_SZ` value stays an empty list.
#[test]
fn empty_list_subkey_is_absent() {
    let snap = machine(Fake::default().sub("AllowedServers", Fake::default()));
    assert!(snap.values.is_empty(), "{:?}", snap.values);
    let p = resolve(&[snap], &ctx());
    assert_eq!(p.allowed_servers(), None);
    assert!(p.rows().is_empty());

    // Only subkeys below it count as no values too.
    let nested = Fake::default().sub(
        "AllowedServers",
        Fake::default().sub("1", Fake::default().value("1", sz("a.com"))),
    );
    assert!(machine(nested).values.is_empty());

    // The value of the same name applies then.
    let snap = machine(
        Fake::default()
            .value("AllowedServers", sz("value.com"))
            .sub("AllowedServers", Fake::default()),
    );
    assert_eq!(snap.values["AllowedServers"], s("value.com"));

    // An unknown empty subkey is still reported.
    let snap = machine(Fake::default().sub("Servers", Fake::default()));
    assert_eq!(
        snap.values["Servers"],
        RawValue::Unsupported("registry key".into())
    );
}

#[test]
fn empty_multi_string_is_an_empty_list() {
    let snap = machine(Fake::default().value("AllowedServers", RegData::MultiSz(vec![])));
    assert_eq!(snap.values["AllowedServers"], RawValue::List(vec![]));
    // Only empty strings: dropped, so an empty list too.
    let snap =
        machine(Fake::default().value("AllowedServers", RegData::MultiSz(vec![String::new()])));
    assert_eq!(snap.values["AllowedServers"], RawValue::List(vec![]));
    let p = resolve(&[snap], &ctx());
    assert_eq!(p.allowed_servers(), Some(&[][..]));
}

/// Subkeys below a list subkey are ignored.
#[test]
fn subkeys_below_a_list_subkey_are_ignored() {
    let snap = machine(
        Fake::default().sub(
            "AllowedServers",
            Fake::default()
                .value("1", sz("a.com"))
                .sub("2", Fake::default().value("1", sz("b.com"))),
        ),
    );
    assert_eq!(
        snap.values["AllowedServers"],
        RawValue::List(vec![s("a.com")])
    );
}

/// The real reader on a Windows machine (CI runs it on the Windows jobs).
#[cfg(windows)]
mod real {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, MutexGuard};

    use rustket_core::policy::{
        Key, RawValue, RowStatus, SourceErrorKind, SourceId, Trust, resolve,
    };
    use windows_registry::CURRENT_USER;

    use super::super::{POLICY_KEY, is_not_found, read_windows};
    use super::{ctx, s};

    /// Opt-in: the round trip writes real policy values under `HKCU`.
    const ROUND_TRIP_ENV: &str = "RUSTKET_TEST_POLICY_REGISTRY";
    const POLICIES: &str = r"Software\Policies";

    /// The round trip writes the key the read-only test reads.
    static REGISTRY: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, ()> {
        REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read-only: whatever this machine's policy holds, both hives are read
    /// and a missing key is a readable, empty source. A machine's own key may
    /// deny access or change meanwhile: the error-free check is opt-in.
    #[test]
    fn reads_both_hives() {
        let _guard = lock();
        let snaps = read_windows();
        let ids: Vec<(SourceId, Trust)> = snaps.iter().map(|s| (s.id.clone(), s.trust)).collect();
        assert_eq!(
            ids,
            [
                (SourceId::WindowsMachine, Trust::Full),
                (SourceId::WindowsUser, Trust::Limited),
            ]
        );
        let strict = std::env::var_os(ROUND_TRIP_ENV).is_some_and(|v| v == "1");
        for snap in &snaps {
            if strict {
                assert!(snap.error.is_none(), "{}: {:?}", snap.id, snap.error);
                assert!(snap.trust_problems.is_empty(), "{}", snap.id);
            } else if let Some(e) = &snap.error {
                assert!(
                    matches!(
                        e.kind,
                        SourceErrorKind::CannotOpen
                            | SourceErrorKind::CannotRead
                            | SourceErrorKind::Unstable
                    ),
                    "{}: {e:?}",
                    snap.id
                );
            }
        }
        // Resolving the machine's own policy never panics.
        let _ = resolve(&snaps, &ctx());
    }

    /// Whether `HKCU\path` exists; any other failure ends the test.
    fn exists(path: &str) -> bool {
        match CURRENT_USER.open(path) {
            Ok(_) => true,
            Err(e) if is_not_found(e.code().0) => false,
            Err(e) => panic!("cannot open HKCU\\{path}: {e}"),
        }
    }

    /// Removes what the round trip created, also when an assertion fails.
    struct Cleanup {
        /// `Software\Policies` did not exist before.
        policies_created: bool,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let mut remove = vec![POLICY_KEY];
            if self.policies_created {
                remove.push(POLICIES);
            }
            for path in remove {
                let empty_parent = path == POLICIES
                    && CURRENT_USER.open(path).is_ok_and(|k| {
                        k.keys().is_ok_and(|mut i| i.next().is_none())
                            && k.values().is_ok_and(|mut i| i.next().is_none())
                    });
                if (path == POLICY_KEY || empty_parent)
                    && let Err(e) = CURRENT_USER.remove_tree(path)
                {
                    eprintln!("could not remove HKCU\\{path}: {e}");
                }
            }
        }
    }

    fn write_failed(what: &str, e: impl std::fmt::Display) -> ! {
        panic!(
            "cannot write {what} under HKCU\\{POLICY_KEY}: {e}. The round trip needs write \
             access to the user's policy key, which standard users do not have."
        )
    }

    /// Writes a DWORD, a string, a multi-string and a list subkey under
    /// `HKCU\Software\Policies\Rustket`, reads them through the real reader
    /// and removes everything it created.
    #[test]
    fn round_trip_through_hkcu() {
        if std::env::var_os(ROUND_TRIP_ENV).is_none_or(|v| v != "1") {
            eprintln!("skipped: set {ROUND_TRIP_ENV}=1 to write HKCU\\{POLICY_KEY}");
            return;
        }
        let _guard = lock();
        assert!(
            !exists(POLICY_KEY),
            "HKCU\\{POLICY_KEY} already exists; the round trip only writes a key it creates"
        );
        let cleanup = Cleanup {
            policies_created: !exists(POLICIES),
        };
        let key = CURRENT_USER
            .create(POLICY_KEY)
            .unwrap_or_else(|e| write_failed("the key", e));
        key.set_u32("RequireHttps", 1)
            .unwrap_or_else(|e| write_failed("RequireHttps", e));
        key.set_string("OrganizationName", "Rustket CI")
            .unwrap_or_else(|e| write_failed("OrganizationName", e));
        key.set_multi_string("AllowedServers", &["chat.example.com", "*.example.org"])
            .unwrap_or_else(|e| write_failed("AllowedServers", e));
        let list = key
            .create("ManagedServers")
            .unwrap_or_else(|e| write_failed("ManagedServers", e));
        for (name, value) in [
            ("10", "https://c.example.com"),
            ("2", "https://b.example.com"),
            ("1", "https://a.example.com Example A"),
        ] {
            list.set_string(name, value)
                .unwrap_or_else(|e| write_failed("ManagedServers", e));
        }

        let snaps = read_windows();
        let user = snaps
            .iter()
            .find(|s| s.id == SourceId::WindowsUser)
            .expect("the HKCU snapshot");
        assert_eq!(user.error, None);
        assert_eq!(user.trust, Trust::Limited);
        let want: BTreeMap<String, RawValue> = [
            ("RequireHttps", RawValue::Integer(1)),
            ("OrganizationName", s("Rustket CI")),
            (
                "AllowedServers",
                RawValue::List(vec![s("chat.example.com"), s("*.example.org")]),
            ),
            (
                "ManagedServers",
                RawValue::List(vec![
                    s("https://a.example.com Example A"),
                    s("https://b.example.com"),
                    s("https://c.example.com"),
                ]),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        assert_eq!(user.values, want);

        // A limited source cannot set a trust-expanding key; the others
        // apply unless the machine's own policy sets them.
        let policy = resolve(&snaps, &ctx());
        let machine = &snaps[0];
        for (key, status) in [
            (Key::ManagedServers, RowStatus::IgnoredLimitedTrust),
            (Key::RequireHttps, RowStatus::Applied),
            (Key::OrganizationName, RowStatus::Applied),
            (Key::AllowedServers, RowStatus::Applied),
        ] {
            if machine.values.contains_key(key.name()) {
                continue;
            }
            let row = policy
                .rows()
                .iter()
                .find(|r| r.key == key.name() && r.source == SourceId::WindowsUser)
                .unwrap_or_else(|| panic!("no HKCU row for {}", key.name()));
            assert_eq!(row.status, status, "{}", key.name());
        }

        // The guard also runs on a panic; here a failed removal fails the test.
        drop(list);
        drop(key);
        drop(cleanup);
        assert!(!exists(POLICY_KEY), "HKCU\\{POLICY_KEY} was not removed");
    }
}
