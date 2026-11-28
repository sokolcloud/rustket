//! The Windows registry source (docs/policy.md sections 2, 4, 5 and 8):
//! `HKLM\Software\Policies\Rustket` with full trust, then
//! `HKCU\Software\Policies\Rustket` with limited trust.
//!
//! The decoding is platform-neutral behind [`RegKey`] and tested with a
//! fake; only the adapter over `windows_registry` is Windows-only. It opens
//! keys read-only and never writes.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use rustket_core::policy::{Key, RawValue, Snapshot, SourceError, SourceErrorKind, SourceId};

/// The policy key under `HKLM` and `HKCU`; a public contract.
pub const POLICY_KEY: &str = r"Software\Policies\Rustket";

/// Reads of a key until two consecutive ones agree (section 8).
pub const MAX_READS: usize = 4;

/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
const HRESULT_FILE_NOT_FOUND: u32 = 0x8007_0002;
/// `HRESULT_FROM_WIN32(ERROR_PATH_NOT_FOUND)`.
const HRESULT_PATH_NOT_FOUND: u32 = 0x8007_0003;

/// Whether an error's HRESULT says that the key or value does not exist.
pub fn is_not_found(hresult: i32) -> bool {
    // The HRESULT bit pattern, as Windows writes it (0x8007....).
    let code = u32::from_ne_bytes(hresult.to_ne_bytes());
    matches!(code, HRESULT_FILE_NOT_FOUND | HRESULT_PATH_NOT_FOUND)
}

/// The type of a registry value as the system reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegType {
    Dword,
    Qword,
    Sz,
    ExpandSz,
    MultiSz,
    Binary,
    /// Any other type, by its number.
    Other(u32),
}

/// A registry value's data, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegData {
    Dword(u32),
    Qword(u64),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    /// Data of another type, or data that does not decode (a DWORD of the
    /// wrong length, a string that is not UTF-16); the text names it.
    Other(String),
}

impl RegData {
    /// Decodes the raw bytes of a value of type `ty` (little-endian, strings
    /// UTF-16). A string ends at its first NUL; variables are not expanded.
    pub fn decode(ty: RegType, bytes: &[u8]) -> RegData {
        match ty {
            RegType::Dword => <[u8; 4]>::try_from(bytes)
                .map(|b| RegData::Dword(u32::from_le_bytes(b)))
                .unwrap_or_else(|_| RegData::Other(format!("REG_DWORD of {} bytes", bytes.len()))),
            RegType::Qword => <[u8; 8]>::try_from(bytes)
                .map(|b| RegData::Qword(u64::from_le_bytes(b)))
                .unwrap_or_else(|_| RegData::Other(format!("REG_QWORD of {} bytes", bytes.len()))),
            RegType::Sz | RegType::ExpandSz => {
                let wide = wide(bytes);
                let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
                match String::from_utf16(&wide[..end]) {
                    Ok(s) if ty == RegType::Sz => RegData::Sz(s),
                    Ok(s) => RegData::ExpandSz(s),
                    Err(_) => RegData::Other(format!("{} that is not UTF-16", type_name(ty))),
                }
            }
            RegType::MultiSz => {
                let wide = wide(bytes);
                let items: Result<Vec<String>, _> =
                    wide.split(|c| *c == 0).map(String::from_utf16).collect();
                match items {
                    // The terminating NULs leave empty pieces; the mapping
                    // drops empty strings anyway.
                    Ok(items) => RegData::MultiSz(items),
                    Err(_) => RegData::Other("REG_MULTI_SZ that is not UTF-16".into()),
                }
            }
            RegType::Binary | RegType::Other(_) => RegData::Other(type_name(ty)),
        }
    }

    fn kind(&self) -> String {
        match self {
            RegData::Dword(_) => type_name(RegType::Dword),
            RegData::Qword(_) => type_name(RegType::Qword),
            RegData::Sz(_) => type_name(RegType::Sz),
            RegData::ExpandSz(_) => type_name(RegType::ExpandSz),
            RegData::MultiSz(_) => type_name(RegType::MultiSz),
            RegData::Other(kind) => kind.clone(),
        }
    }
}

fn wide(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

fn type_name(ty: RegType) -> String {
    match ty {
        RegType::Dword => "REG_DWORD".into(),
        RegType::Qword => "REG_QWORD".into(),
        RegType::Sz => "REG_SZ".into(),
        RegType::ExpandSz => "REG_EXPAND_SZ".into(),
        RegType::MultiSz => "REG_MULTI_SZ".into(),
        RegType::Binary => "REG_BINARY".into(),
        RegType::Other(0) => "REG_NONE".into(),
        RegType::Other(5) => "REG_DWORD_BIG_ENDIAN".into(),
        RegType::Other(6) => "REG_LINK".into(),
        RegType::Other(8) => "REG_RESOURCE_LIST".into(),
        RegType::Other(9) => "REG_FULL_RESOURCE_DESCRIPTOR".into(),
        RegType::Other(10) => "REG_RESOURCE_REQUIREMENTS_LIST".into(),
        RegType::Other(n) => format!("registry type {n}"),
    }
}

/// A failed registry call: whether it said "not found" (by its HRESULT,
/// [`is_not_found`]) and the system's own text, when there is one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("registry call failed: {}", .detail.as_deref().unwrap_or("no detail"))]
pub struct RegError {
    pub not_found: bool,
    pub detail: Option<String>,
}

/// Runs an enumeration of the adapter. `windows_registry`'s iterators
/// `debug_assert` that an enumeration ends with ERROR_NO_MORE_ITEMS, so a key
/// deleted or a value grown during the walk panics in a debug build; such a
/// walk counts as a failed read, which the read-until-stable loop retries.
/// The detail is the panic message itself, if it has one.
#[cfg_attr(not(windows), allow(dead_code))]
fn enumerate<T>(walk: impl FnOnce() -> T) -> Result<T, RegError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(walk)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned());
        RegError {
            not_found: false,
            detail,
        }
    })
}

/// An open registry key, read-only.
pub trait RegKey: Sized {
    /// The values directly under the key, with name and data.
    fn values(&self) -> Result<Vec<(String, RegData)>, RegError>;
    /// The names of the subkeys.
    fn subkeys(&self) -> Result<Vec<String>, RegError>;
    fn open(&self, name: &str) -> Result<Self, RegError>;
}

/// The policy key whose name equals `name` ignoring ASCII case.
fn policy_key(name: &str) -> Option<Key> {
    Key::ALL
        .into_iter()
        .find(|k| k.name().eq_ignore_ascii_case(name))
}

/// One complete read of the policy key, sorted by name so two reads compare
/// equal when the content is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    values: Vec<(String, RegData)>,
    /// Subkeys named like a policy key, with their values.
    lists: Vec<(String, Vec<(String, RegData)>)>,
    /// Every other subkey name.
    other_subkeys: Vec<String>,
}

/// `Ok(None)`: the key does not exist.
type Read = Result<Option<Content>, SourceError>;

fn error(kind: SourceErrorKind, e: RegError) -> SourceError {
    SourceError {
        kind,
        detail: e.detail,
    }
}

fn read_once<K: RegKey>(open: &impl Fn() -> Result<K, RegError>) -> Read {
    let key = match open() {
        Ok(key) => key,
        Err(e) if e.not_found => return Ok(None),
        Err(e) => return Err(error(SourceErrorKind::CannotOpen, e)),
    };
    let mut values = key
        .values()
        .map_err(|e| error(SourceErrorKind::CannotRead, e))?;
    values.sort_by(|a, b| a.0.cmp(&b.0));
    let mut names = key
        .subkeys()
        .map_err(|e| error(SourceErrorKind::CannotRead, e))?;
    names.sort();
    let mut lists = Vec::new();
    let mut other_subkeys = Vec::new();
    for name in names {
        if policy_key(&name).is_none() {
            other_subkeys.push(name);
            continue;
        }
        // A subkey removed since the listing fails here; the next read
        // sees the new state.
        let sub = key
            .open(&name)
            .map_err(|e| error(SourceErrorKind::CannotOpen, e))?;
        let mut items = sub
            .values()
            .map_err(|e| error(SourceErrorKind::CannotRead, e))?;
        items.sort_by(|a, b| a.0.cmp(&b.0));
        lists.push((name, items));
    }
    Ok(Some(Content {
        values,
        lists,
        other_subkeys,
    }))
}

/// Calls `read` until two consecutive results are equal, at most
/// [`MAX_READS`] times; `None` when no two agree.
fn read_stable<T: PartialEq>(mut read: impl FnMut() -> T) -> Option<T> {
    let mut last = read();
    for _ in 1..MAX_READS {
        let next = read();
        if next == last {
            return Some(next);
        }
        last = next;
    }
    None
}

/// The snapshot of source `id` (`WindowsMachine` or `WindowsUser`), whose
/// policy key `open` opens: empty when the key does not exist, unreadable
/// when it exists but cannot be opened or enumerated, or when it changed
/// during every read.
pub fn read_source<K: RegKey>(id: SourceId, open: impl Fn() -> Result<K, RegError>) -> Snapshot {
    match read_stable(|| read_once(&open)) {
        Some(Ok(None)) => {
            let trust = id.default_trust();
            Snapshot::new(id, trust)
        }
        Some(Ok(Some(content))) => decode(id, content),
        Some(Err(e)) => Snapshot::unreadable(id, e),
        None => Snapshot::unreadable(id, SourceErrorKind::Unstable),
    }
}

/// Section 4's mapping of one read into a snapshot.
fn decode(id: SourceId, content: Content) -> Snapshot {
    let trust = id.default_trust();
    let mut snap = Snapshot::new(id, trust);
    let mut values: BTreeMap<Key, Vec<RegData>> = BTreeMap::new();
    let mut lists: BTreeMap<Key, Vec<Vec<(String, RegData)>>> = BTreeMap::new();
    for (name, data) in content.values {
        match policy_key(&name) {
            Some(key) => values.entry(key).or_default().push(data),
            None => {
                snap.values.insert(name, value(&data));
            }
        }
    }
    // After the values: an unknown subkey wins over a value of its name.
    for name in content.other_subkeys {
        snap.values
            .insert(name, RawValue::Unsupported("registry key".into()));
    }
    for (name, items) in content.lists {
        // A list subkey without values is absent (section 4): a disabled
        // ADMX list policy leaves one behind.
        if items.is_empty() {
            continue;
        }
        if let Some(key) = policy_key(&name) {
            lists.entry(key).or_default().push(items);
        }
    }
    for key in Key::ALL {
        let raw = match (values.get(&key).map(Vec::as_slice), lists.get(&key)) {
            // The subkey wins over a value of the same name.
            (_, Some(subs)) => match subs.as_slice() {
                [items] => list(items),
                _ => RawValue::Unsupported("several registry keys with this name".into()),
            },
            (Some([data]), None) => value(data),
            (Some(_), None) => {
                RawValue::Unsupported("several registry values with this name".into())
            }
            (None, None) => continue,
        };
        snap.values.insert(key.name().to_owned(), raw);
    }
    snap
}

/// A value directly under the policy key.
fn value(data: &RegData) -> RawValue {
    match data {
        RegData::Dword(v) => RawValue::Integer(i64::from(*v)),
        RegData::Qword(v) => i64::try_from(*v).map_or_else(
            |_| RawValue::Unsupported("REG_QWORD above the signed 64-bit range".into()),
            RawValue::Integer,
        ),
        RegData::Sz(s) | RegData::ExpandSz(s) => RawValue::String(s.clone()),
        RegData::MultiSz(items) => RawValue::List(
            items
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| RawValue::String(s.clone()))
                .collect(),
        ),
        RegData::Other(kind) => RawValue::Unsupported(kind.clone()),
    }
}

/// A list subkey: its string values in value-name order; any other value
/// is an unsupported item (the core drops and reports it).
fn list(items: &[(String, RegData)]) -> RawValue {
    let mut items: Vec<&(String, RegData)> = items.iter().collect();
    items.sort_by(|a, b| value_name_order(&a.0, &b.0));
    RawValue::List(
        items
            .into_iter()
            .map(|(_, data)| match data {
                RegData::Sz(s) | RegData::ExpandSz(s) => RawValue::String(s.clone()),
                other => RawValue::Unsupported(other.kind()),
            })
            .collect(),
    )
}

/// Numeric names (ASCII digits) first, in numeric order, then the others
/// in byte order.
fn value_name_order(a: &str, b: &str) -> Ordering {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    match (numeric(a), numeric(b)) {
        (true, true) => {
            let (ta, tb) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
            ta.len()
                .cmp(&tb.len())
                .then_with(|| ta.cmp(tb))
                .then_with(|| a.cmp(b))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.as_bytes().cmp(b.as_bytes()),
    }
}

/// The Windows snapshots, highest priority first.
#[cfg(windows)]
pub fn read_windows() -> Vec<Snapshot> {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
    vec![
        read_source(SourceId::WindowsMachine, || os::open(LOCAL_MACHINE)),
        read_source(SourceId::WindowsUser, || os::open(CURRENT_USER)),
    ]
}

/// The thin adapter over `windows_registry`.
#[cfg(windows)]
mod os {
    use windows_registry::{Key, Type};

    use super::{POLICY_KEY, RegData, RegError, RegKey, RegType, enumerate, is_not_found};

    pub struct WinKey(Key);

    pub fn open(root: &Key) -> Result<WinKey, RegError> {
        check(root.open(POLICY_KEY)).map(WinKey)
    }

    /// The result with the error classified by its HRESULT.
    fn check<T>(r: windows_registry::Result<T>) -> Result<T, RegError> {
        r.map_err(|e| {
            let code = e.code().0;
            RegError {
                not_found: is_not_found(code),
                detail: Some(format!("{} (0x{code:08X})", e.message())),
            }
        })
    }

    impl RegKey for WinKey {
        fn values(&self) -> Result<Vec<(String, RegData)>, RegError> {
            let values = check(self.0.values())?;
            enumerate(|| {
                values
                    .map(|(name, v)| {
                        let ty = match v.ty() {
                            Type::U32 => RegType::Dword,
                            Type::U64 => RegType::Qword,
                            Type::String => RegType::Sz,
                            Type::ExpandString => RegType::ExpandSz,
                            Type::MultiString => RegType::MultiSz,
                            Type::Bytes => RegType::Binary,
                            Type::Other(n) => RegType::Other(n),
                        };
                        (name, RegData::decode(ty, &v))
                    })
                    .collect()
            })
        }

        fn subkeys(&self) -> Result<Vec<String>, RegError> {
            let keys = check(self.0.keys())?;
            enumerate(|| keys.collect())
        }

        fn open(&self, name: &str) -> Result<Self, RegError> {
            check(self.0.open(name)).map(WinKey)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
