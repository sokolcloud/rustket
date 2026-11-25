//! Secret storage for auth tokens and E2E secrets (the decrypted private key and
//! a generated E2E password not confirmed yet, see `crate::e2e::store`).
//!
//! The OS keyring (Secret Service on Linux, Keychain, Windows Credential Manager)
//! is preferred. When it is unavailable, [`FilePolicy`] decides:
//!
//! - [`FilePolicy::Allowed`] (the default): [`open_default`] falls back to an
//!   unencrypted JSON file in the XDG data directory (mode 0600 on Unix only) and
//!   reports [`SecretBackend::File`] so the UI can warn about it.
//! - [`FilePolicy::Never`] (the user opted out of the file): secrets are kept in
//!   memory for this session only ([`SecretBackend::MemoryByPolicy`]); no file is
//!   created. A `secrets.json` left by an earlier fallback is loaded into memory
//!   and deleted together with any `secrets.json.broken-*`.
//! - [`FilePolicy::Forbidden`] (the Mac App Store flavor, set by the app): as
//!   `Never`, but the keyring failure is reported and the backend is
//!   [`SecretBackend::MemoryForced`], so the UI warns instead of the plaintext
//!   file appearing silently. An unsigned build's keychain error -34018
//!   (`errSecMissingEntitlement`) takes this path like any other failure.
//!
//! The rest:
//!
//! - The keyring is probed with a real write/read/delete round trip, so a keyring
//!   without a usable default collection is detected at startup instead of
//!   silently losing every token.
//! - Keyring calls run on one dedicated worker thread and give up after a timeout
//!   ([`SecretError::Timeout`]): a hung Secret Service or an unanswered unlock
//!   prompt cannot block the caller (or other secret operations) forever.
//! - The chosen backend is recorded next to the file store. When the keyring works,
//!   tokens left in the file by an earlier fallback are moved into it and the
//!   plaintext file is deleted; reads fall back to the file until then.
//! - A corrupted `secrets.json` is moved aside as `secrets.json.broken-<ts>` and
//!   the store continues empty.
//!
//! - [`SwitchableStore`] is the one instance handed to every user of the store;
//!   it swaps the backend at runtime when the policy changes (file ⇄ memory).
//!
//! Token keys are built with [`secret_key`]: `rustket:<server_id>:<user_id>`; E2E
//! keys start with `rustket-e2e:`. Values are never logged.
//!
//! The backends do blocking I/O: call these methods from a blocking context
//! (`tokio::task::spawn_blocking`), not from an async task.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use crate::config::ServerId;

/// Keyring service name.
pub const SERVICE: &str = "rustket";
/// File used by [`FileStore`] inside its directory.
pub const SECRETS_FILE: &str = "secrets.json";
/// Records the backend chosen last time (`keyring` / `file`), next to [`SECRETS_FILE`].
pub const BACKEND_FILE: &str = "secret-backend";
/// Default time limit of a keyring operation (it may wait for an unlock prompt).
pub const KEYRING_TIMEOUT: Duration = Duration::from_secs(60);
/// Time limit of the startup probe.
pub const KEYRING_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
    /// The keyring is locked (and the unlock was refused) or has no collection
    /// to store items in.
    #[error("keyring storage not accessible: {0}")]
    NoStorageAccess(String),
    /// The keyring did not answer in time.
    #[error("keyring did not answer in time")]
    Timeout,
    #[error("secret file error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("secret file {0} is corrupted")]
    Corrupted(PathBuf),
}

pub type Result<T, E = SecretError> = std::result::Result<T, E>;

/// Key under which the auth token of `user_id` on `server` is stored.
pub fn secret_key(server: &ServerId, user_id: &str) -> String {
    format!("{SERVICE}:{server}:{user_id}")
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Deleting a missing key is not an error.
    fn delete(&self, key: &str) -> Result<()>;
    /// Every entry, for backends that can list them (file, memory); `None`
    /// otherwise (the keyring).
    fn list_entries(&self) -> Result<Option<BTreeMap<String, String>>> {
        Ok(None)
    }
}

impl<T: SecretStore + ?Sized> SecretStore for Arc<T> {
    fn get(&self, key: &str) -> Result<Option<String>> {
        (**self).get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        (**self).set(key, value)
    }
    fn delete(&self, key: &str) -> Result<()> {
        (**self).delete(key)
    }
    fn list_entries(&self) -> Result<Option<BTreeMap<String, String>>> {
        (**self).list_entries()
    }
}

/// Whether secrets may be written to a file when the keyring is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilePolicy {
    /// Fall back to the unencrypted file (the default).
    #[default]
    Allowed,
    /// Never write secrets to a file: keep them in memory for this session.
    Never,
    /// This build may never write the file (the user cannot change it): as
    /// [`Self::Never`], and an unusable keyring is reported.
    Forbidden,
}

impl FilePolicy {
    /// From the `secrets_memory_only` preference.
    pub fn memory_only(on: bool) -> Self {
        if on { Self::Never } else { Self::Allowed }
    }
}

/// Which backend [`open_default`] selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    Keyring,
    /// Plain file fallback: the UI should warn the user.
    File,
    /// Neither the keyring nor the data directory is usable; secrets are kept in
    /// memory only and the user has to log in on every start.
    Memory,
    /// The keyring is unusable and [`FilePolicy::Never`] forbids the file:
    /// secrets are kept in memory for this session only.
    MemoryByPolicy,
    /// The keyring is unusable and [`FilePolicy::Forbidden`] rules the file
    /// out: secrets are kept in memory for this session only; the UI warns.
    MemoryForced,
}

impl SecretBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::Memory | Self::MemoryByPolicy | Self::MemoryForced => "memory",
        }
    }

    /// Secrets are lost when the app quits.
    pub fn is_memory(self) -> bool {
        matches!(
            self,
            Self::Memory | Self::MemoryByPolicy | Self::MemoryForced
        )
    }
}

// --- keyring ----------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// The single thread all keyring calls run on (the platform store is global and
/// serialises calls anyway).
static WORKER: LazyLock<Mutex<Option<mpsc::Sender<Job>>>> = LazyLock::new(|| {
    let (tx, rx) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new()
        .name("rustket-keyring".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                // A panicking call must not take the worker down with it.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    tracing::warn!("keyring call panicked");
                }
            }
        });
    match spawned {
        Ok(_) => Mutex::new(Some(tx)),
        Err(e) => {
            tracing::warn!(error = %e, "cannot start the keyring thread");
            Mutex::new(None)
        }
    }
});

/// Runs `f` on the keyring thread and waits at most `timeout` for its result.
fn on_worker<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::sync_channel(1);
    let job: Job = Box::new(move || {
        let _ = tx.send(f());
    });
    {
        let guard = WORKER.lock().unwrap_or_else(|p| p.into_inner());
        let sender = guard
            .as_ref()
            .ok_or_else(|| SecretError::Keyring("keyring thread unavailable".into()))?;
        sender
            .send(job)
            .map_err(|_| SecretError::Keyring("keyring thread stopped".into()))?;
    }
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!(?timeout, "keyring call timed out");
            Err(SecretError::Timeout)
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(SecretError::Keyring("keyring call aborted".into()))
        }
    }
}

fn kr_err(e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoStorageAccess(_) => SecretError::NoStorageAccess(e.to_string()),
        e => SecretError::Keyring(e.to_string()),
    }
}

fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).map_err(kr_err)
}

/// Windows Credential Manager holds at most 2560 bytes per credential and the
/// password API stores UTF-16, which an E2E private key (JWK) exceeds: values
/// are stored as UTF-8 bytes there.
fn kr_set(entry: &keyring::Entry, value: &str) -> keyring::Result<()> {
    #[cfg(windows)]
    return entry.set_secret(value.as_bytes());
    #[cfg(not(windows))]
    entry.set_password(value)
}

fn kr_get(entry: &keyring::Entry) -> keyring::Result<String> {
    #[cfg(windows)]
    return entry.get_secret().and_then(|bytes| {
        String::from_utf8(bytes).map_err(|e| keyring::Error::BadEncoding(e.into_bytes()))
    });
    #[cfg(not(windows))]
    entry.get_password()
}

/// OS keyring through the `keyring` 4 crate (`v1` API), with a time limit per call.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    timeout: Duration,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringStore {
    pub fn new() -> Self {
        Self::with_timeout(KEYRING_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// Whether the platform store can actually store a secret: writes, reads back
    /// and deletes a probe entry (this may show an unlock prompt).
    pub fn probe() -> Result<()> {
        on_worker(KEYRING_PROBE_TIMEOUT, || {
            if let Err(e) = keyring::Entry::store_status() {
                return Err(SecretError::Keyring(e.to_string()));
            }
            let probe = entry(&format!("{SERVICE}:probe"))?;
            let value = rocketchat::ids::random_id();
            kr_set(&probe, &value).map_err(kr_err)?;
            let read = kr_get(&probe).map_err(kr_err);
            let _ = probe.delete_credential();
            if read? != value {
                return Err(SecretError::Keyring(
                    "probe value did not round-trip".into(),
                ));
            }
            Ok(())
        })
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        on_worker(self.timeout, move || match kr_get(&entry(&key)?) {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(kr_err(e)),
        })
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        on_worker(self.timeout, move || {
            kr_set(&entry(&key)?, &value).map_err(kr_err)
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        on_worker(self.timeout, move || {
            match entry(&key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(kr_err(e)),
            }
        })
    }
}

// --- file -------------------------------------------------------------------------

/// Unencrypted JSON map `{key: value}` in a file with mode 0600.
#[derive(Debug)]
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
    recovered: Mutex<Option<PathBuf>>,
}

impl FileStore {
    /// Store in `dir/secrets.json` (the directory is created on first write).
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(SECRETS_FILE),
            lock: Mutex::new(()),
            recovered: Mutex::new(None),
        }
    }

    /// `$XDG_DATA_HOME/rustket` (platform equivalent elsewhere).
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "rustket").map(|d| d.data_dir().to_owned())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where a corrupted file was moved to, if that happened (once).
    pub fn take_recovery(&self) -> Option<PathBuf> {
        self.recovered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    fn io_err(&self) -> impl Fn(io::Error) -> SecretError + '_ {
        move |source| SecretError::Io {
            path: self.path.clone(),
            source,
        }
    }

    /// Reads the map; a file that is not a JSON string map is moved aside.
    fn read(&self) -> Result<BTreeMap<String, String>> {
        match fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(map) => Ok(map),
                Err(_) => {
                    let ts = chrono::Utc::now().timestamp();
                    let mut broken = self.path.as_os_str().to_owned();
                    broken.push(format!(".broken-{ts}"));
                    let broken = PathBuf::from(broken);
                    tracing::warn!(path = %self.path.display(), moved_to = %broken.display(), "secret file is corrupted; starting empty");
                    fs::rename(&self.path, &broken).map_err(self.io_err())?;
                    *self.recovered.lock().unwrap_or_else(|p| p.into_inner()) = Some(broken);
                    Ok(BTreeMap::new())
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(self.io_err()(e)),
        }
    }

    fn write(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(self.io_err())?;
        }
        let data =
            serde_json::to_vec_pretty(map).map_err(|e| self.io_err()(io::Error::other(e)))?;
        crate::config::write_atomic(&self.path, &data).map_err(|e| match e {
            crate::config::ConfigError::Io { source, .. } => self.io_err()(source),
            other => self.io_err()(io::Error::other(other.to_string())),
        })
    }

    fn with_lock<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        f()
    }

    /// Whether the directory is writable (creates it if needed).
    pub fn probe(&self) -> Result<()> {
        self.with_lock(|| {
            let map = self.read()?;
            self.write(&map)
        })
    }

    /// Every stored entry.
    pub fn entries(&self) -> Result<BTreeMap<String, String>> {
        self.with_lock(|| self.read())
    }

    /// Adds `entries` (replacing equal keys) in one write.
    pub fn merge(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        self.with_lock(|| {
            let mut map = self.read()?;
            map.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone())));
            self.write(&map)
        })
    }

    /// Deletes the file when it holds no entries.
    fn remove_if_empty(&self) -> Result<()> {
        self.with_lock(|| {
            if self.read()?.is_empty() {
                match fs::remove_file(&self.path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(self.io_err()(e)),
                }
            }
            Ok(())
        })
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.with_lock(|| Ok(self.read()?.get(key).cloned()))
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.with_lock(|| {
            let mut map = self.read()?;
            map.insert(key.to_owned(), value.to_owned());
            self.write(&map)
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.with_lock(|| {
            let mut map = self.read()?;
            if map.remove(key).is_some() {
                self.write(&map)?;
            }
            Ok(())
        })
    }

    fn list_entries(&self) -> Result<Option<BTreeMap<String, String>>> {
        self.entries().map(Some)
    }
}

/// The `secrets.json.broken-*` copies in `dir`.
fn broken_files(dir: &Path) -> Vec<PathBuf> {
    files_with_prefix(dir, &format!("{SECRETS_FILE}.broken-"))
}

/// Deletes `paths`; missing files are not an error. Every file is tried, the
/// first error is returned.
fn remove_files(paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
    let mut first = None;
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "deleted a secret file"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                tracing::warn!(path = %path.display(), error = %source, "cannot delete a secret file");
                first.get_or_insert(SecretError::Io { path, source });
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// Files in `dir` whose name starts with `prefix`.
fn files_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|list| {
            list.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

/// Temporary files of an interrupted atomic write of `path` (they hold a full
/// copy of the secrets).
fn temp_files(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    files_with_prefix(dir, &format!(".{}.tmp-", name.to_string_lossy()))
}

/// Deletes `dir/secrets.json` (and the file it links to, when it is a symlink:
/// writes go to the target), every `dir/secrets.json.broken-*` and leftover
/// temporary files of interrupted writes.
pub fn remove_secret_files(dir: &Path) -> Result<()> {
    let path = dir.join(SECRETS_FILE);
    let target = crate::config::write_target(&path);
    let mut paths = Vec::new();
    if target != path {
        paths.push(target.clone());
        paths.extend(temp_files(&target));
    }
    paths.push(path.clone());
    paths.extend(temp_files(&path));
    paths.extend(broken_files(dir));
    remove_files(paths)
}

// --- memory -----------------------------------------------------------------------

/// In-memory store (tests, last-resort fallback).
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore")
            .field("entries", &self.map().len())
            .finish()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: BTreeMap<String, String>) -> Self {
        Self {
            map: Mutex::new(entries),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.map().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.map().insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.map().remove(key);
        Ok(())
    }

    fn list_entries(&self) -> Result<Option<BTreeMap<String, String>>> {
        Ok(Some(self.map().clone()))
    }
}

// --- keyring with the file as legacy source ---------------------------------------

/// The keyring, plus the file store as a read-only legacy source: tokens written
/// there by an earlier fallback are still found; writing or deleting a key also
/// removes it from the file (deleted once empty).
pub struct KeyringWithLegacyFile {
    keyring: Box<dyn SecretStore>,
    file: FileStore,
}

impl KeyringWithLegacyFile {
    fn forget_legacy(&self, key: &str) {
        let r = self
            .file
            .delete(key)
            .and_then(|()| self.file.remove_if_empty());
        if let Err(e) = r {
            tracing::warn!(error = %e, "cannot remove a token from the secret file");
        }
    }
}

impl SecretStore for KeyringWithLegacyFile {
    fn get(&self, key: &str) -> Result<Option<String>> {
        match self.keyring.get(key)? {
            Some(v) => Ok(Some(v)),
            None => Ok(self.file.get(key).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot read the legacy secret file");
                None
            })),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.keyring.set(key, value)?;
        self.forget_legacy(key);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.keyring.delete(key)?;
        self.forget_legacy(key);
        Ok(())
    }
}

// --- selection ----------------------------------------------------------------------

/// Something the UI may want to tell the user about the secret store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretNotice {
    /// The keyring failed; why.
    KeyringUnavailable(String),
    /// The keyring was used last time but is unavailable now: tokens stored there
    /// cannot be read in this session.
    KeyringTokensUnreachable,
    /// Tokens from an earlier file fallback were moved into the keyring.
    MigratedToKeyring { count: usize },
    /// Secrets are still in the plaintext file (moving or deleting them failed).
    PlaintextTokensLeft { count: usize },
    /// [`FilePolicy::Never`]: the file left by an earlier fallback was loaded
    /// into memory and deleted (only reported when it held entries).
    FileRemoved { count: usize },
    /// [`FilePolicy::Never`]: the secret file could not be read or deleted and
    /// is still on disk.
    FileNotRemoved { path: PathBuf },
    /// A corrupted secret file was moved aside.
    FileRecovered { moved_to: PathBuf },
}

/// Result of [`open_default_report`].
pub struct OpenedSecrets {
    pub store: Box<dyn SecretStore>,
    pub backend: SecretBackend,
    pub notices: Vec<SecretNotice>,
}

fn read_backend_record(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(BACKEND_FILE))
        .ok()
        .map(|s| s.trim().to_owned())
}

fn write_backend_record(dir: &Path, backend: SecretBackend) {
    if read_backend_record(dir).as_deref() == Some(backend.as_str()) {
        return;
    }
    let r = fs::create_dir_all(dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            crate::config::write_atomic(&dir.join(BACKEND_FILE), backend.as_str().as_bytes())
                .map_err(|e| e.to_string())
        });
    if let Err(e) = r {
        tracing::warn!(error = %e, "cannot record the secret backend");
    }
}

/// Moves every token from `file` into `keyring`. Returns (moved, left).
fn migrate(file: &FileStore, keyring: &dyn SecretStore) -> (usize, usize) {
    let entries = match file.entries() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the secret file for migration");
            return (0, 0);
        }
    };
    let mut moved = 0;
    for (k, v) in &entries {
        let ok = keyring
            .set(k, v)
            .and_then(|()| keyring.get(k))
            .is_ok_and(|read| read.as_deref() == Some(v.as_str()));
        if ok && file.delete(k).is_ok() {
            moved += 1;
        }
    }
    if let Err(e) = file.remove_if_empty() {
        tracing::warn!(error = %e, "cannot delete the emptied secret file");
    }
    (moved, entries.len() - moved)
}

/// Picks the backend given the keyring probe result, the file directory and
/// the file policy.
fn select(
    dir: Option<PathBuf>,
    keyring: Result<Box<dyn SecretStore>>,
    policy: FilePolicy,
) -> OpenedSecrets {
    let mut notices = Vec::new();
    let previous = dir.as_deref().and_then(read_backend_record);
    match keyring {
        Ok(keyring) => {
            let Some(dir) = dir else {
                return OpenedSecrets {
                    store: keyring,
                    backend: SecretBackend::Keyring,
                    notices,
                };
            };
            let file = FileStore::new(&dir);
            let (moved, left) = migrate(&file, keyring.as_ref());
            if moved > 0 {
                tracing::info!(moved, "moved secrets from the secret file into the keyring");
                notices.push(SecretNotice::MigratedToKeyring { count: moved });
            }
            if left > 0 {
                notices.push(SecretNotice::PlaintextTokensLeft { count: left });
            }
            if let Some(moved_to) = file.take_recovery()
                && policy == FilePolicy::Allowed
            {
                notices.push(SecretNotice::FileRecovered { moved_to });
            }
            if policy != FilePolicy::Allowed {
                let _ = remove_files(leftover_files(&dir));
            }
            write_backend_record(&dir, SecretBackend::Keyring);
            OpenedSecrets {
                store: Box::new(KeyringWithLegacyFile { keyring, file }),
                backend: SecretBackend::Keyring,
                notices,
            }
        }
        Err(e) if policy != FilePolicy::Allowed => {
            let backend = if policy == FilePolicy::Forbidden {
                tracing::warn!(error = %e, "OS keyring unavailable; keeping secrets in memory (no file in this build)");
                notices.push(SecretNotice::KeyringUnavailable(e.to_string()));
                SecretBackend::MemoryForced
            } else {
                tracing::warn!(error = %e, "OS keyring unavailable; keeping secrets in memory (no file by preference)");
                SecretBackend::MemoryByPolicy
            };
            if previous.as_deref() == Some(SecretBackend::Keyring.as_str()) {
                notices.push(SecretNotice::KeyringTokensUnreachable);
            }
            let memory = MemoryStore::new();
            if let Some(dir) = &dir {
                load_and_remove_file(dir, &memory, &mut notices);
            }
            OpenedSecrets {
                store: Box::new(memory),
                backend,
                notices,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "OS keyring unavailable; falling back to a file");
            notices.push(SecretNotice::KeyringUnavailable(e.to_string()));
            if previous.as_deref() == Some(SecretBackend::Keyring.as_str()) {
                notices.push(SecretNotice::KeyringTokensUnreachable);
            }
            if let Some(dir) = dir {
                let store = FileStore::new(&dir);
                let probed = store.probe();
                if let Some(moved_to) = store.take_recovery() {
                    notices.push(SecretNotice::FileRecovered { moved_to });
                }
                match probed {
                    Ok(()) => {
                        // Keep the "keyring" record while its tokens are unreachable,
                        // so the next start still reports it.
                        if previous.as_deref() != Some(SecretBackend::Keyring.as_str()) {
                            write_backend_record(&dir, SecretBackend::File);
                        }
                        return OpenedSecrets {
                            store: Box::new(store),
                            backend: SecretBackend::File,
                            notices,
                        };
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "secret file unusable; keeping tokens in memory");
                    }
                }
            }
            OpenedSecrets {
                store: Box::new(MemoryStore::new()),
                backend: SecretBackend::Memory,
                notices,
            }
        }
    }
}

/// [`FilePolicy::Never`] without a keyring: the entries of a `secrets.json`
/// left by an earlier fallback go into `memory` (no forced logout), then the
/// file and its `.broken-*` copies are deleted. Nothing is created.
fn load_and_remove_file(dir: &Path, memory: &MemoryStore, notices: &mut Vec<SecretNotice>) {
    let path = dir.join(SECRETS_FILE);
    let mut count = 0;
    if fs::symlink_metadata(&path).is_ok() {
        // A corrupted file is moved aside by the read (and deleted below).
        match FileStore::new(dir).entries() {
            Ok(entries) => {
                count = entries.len();
                memory.map().extend(entries);
            }
            Err(e) => {
                // Unreadable: keep it, its secrets would be lost otherwise.
                tracing::warn!(error = %e, "cannot read the secret file; leaving it in place");
                let _ = remove_files(leftover_files(dir));
                notices.push(SecretNotice::FileNotRemoved { path });
                return;
            }
        }
    }
    match remove_secret_files(dir) {
        Ok(()) if count > 0 => notices.push(SecretNotice::FileRemoved { count }),
        Ok(()) => {}
        Err(_) if count > 0 => notices.push(SecretNotice::PlaintextTokensLeft { count }),
        Err(SecretError::Io { path, .. }) => notices.push(SecretNotice::FileNotRemoved { path }),
        Err(_) => {}
    }
}

/// The `.broken-*` copies and temporary files next to `dir/secrets.json`.
fn leftover_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths = broken_files(dir);
    paths.extend(temp_files(&dir.join(SECRETS_FILE)));
    paths
}

/// Probes the OS keyring and, by `policy`, falls back to the file store or to
/// memory (then to memory when the directory is unusable), migrating file
/// secrets into a working keyring. Blocking: call from `spawn_blocking`.
pub fn open_default_report(policy: FilePolicy) -> OpenedSecrets {
    let keyring =
        KeyringStore::probe().map(|()| Box::new(KeyringStore::new()) as Box<dyn SecretStore>);
    select(FileStore::default_dir(), keyring, policy)
}

/// [`open_default_report`] without the notices.
pub fn open_default(policy: FilePolicy) -> (Box<dyn SecretStore>, SecretBackend) {
    let o = open_default_report(policy);
    (o.store, o.backend)
}

// --- switchable ---------------------------------------------------------------------

struct Current {
    store: Box<dyn SecretStore>,
    backend: SecretBackend,
}

/// The store handed to every user (sessions, E2E managers): delegates to the
/// backend in use, which [`Self::apply_policy`] can swap at runtime without
/// anyone holding a stale store.
pub struct SwitchableStore {
    current: RwLock<Current>,
    /// Directory of the file store; `None`: no file backend possible.
    dir: Option<PathBuf>,
}

impl std::fmt::Debug for SwitchableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitchableStore")
            .field("backend", &self.backend())
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// Result of [`SwitchableStore::apply_policy`].
#[derive(Debug)]
pub struct PolicySwitch {
    /// The backend in use afterwards.
    pub backend: SecretBackend,
    /// What failed: before the swap (nothing changed) or after it (e.g. the
    /// old file could not be deleted).
    pub error: Option<SecretError>,
}

impl SwitchableStore {
    /// `store` is `backend`; `dir` is where the file store lives (or would).
    pub fn new(store: Box<dyn SecretStore>, backend: SecretBackend, dir: Option<PathBuf>) -> Self {
        Self {
            current: RwLock::new(Current { store, backend }),
            dir,
        }
    }

    /// The result of [`open_default_report`] (whose file directory is
    /// [`FileStore::default_dir`]).
    pub fn opened(o: OpenedSecrets) -> (Self, Vec<SecretNotice>) {
        (
            Self::new(o.store, o.backend, FileStore::default_dir()),
            o.notices,
        )
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Current> {
        self.current.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Current> {
        self.current.write().unwrap_or_else(|p| p.into_inner())
    }

    pub fn backend(&self) -> SecretBackend {
        self.read().backend
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Applies a changed [`FilePolicy`] (blocking).
    /// - `Never` while on the file: every entry moves into memory, then
    ///   `secrets.json` and its `.broken-*` copies are deleted.
    /// - `Never` with the keyring: entries a startup migration left in the file
    ///   are moved into the keyring again (the file goes once empty).
    /// - `Allowed` while in memory by policy: the entries are written to the
    ///   file store (probed first), which then takes over. A keyring that became
    ///   usable meanwhile is only picked up at the next start.
    ///
    /// A failure before the swap leaves everything as it was.
    pub fn apply_policy(&self, policy: FilePolicy) -> PolicySwitch {
        let backend = self.backend();
        match (policy, backend) {
            (FilePolicy::Never | FilePolicy::Forbidden, SecretBackend::File)
            | (FilePolicy::Allowed, SecretBackend::MemoryByPolicy) => {}
            (FilePolicy::Never | FilePolicy::Forbidden, SecretBackend::Keyring) => {
                // Readers only: secret access goes on while the keyring is slow.
                let cur = self.read();
                let error = self
                    .dir
                    .as_deref()
                    .and_then(|dir| migrate_leftovers(dir, cur.store.as_ref()).err());
                return PolicySwitch {
                    backend: cur.backend,
                    error,
                };
            }
            _ => {
                return PolicySwitch {
                    backend,
                    error: None,
                };
            }
        }
        let mut cur = self.write();
        let error = match (policy, cur.backend) {
            (FilePolicy::Never | FilePolicy::Forbidden, SecretBackend::File) => {
                let entries = match cur.store.list_entries() {
                    Ok(Some(e)) => e,
                    Ok(None) => BTreeMap::new(),
                    Err(e) => {
                        return PolicySwitch {
                            backend: cur.backend,
                            error: Some(e),
                        };
                    }
                };
                cur.store = Box::new(MemoryStore::from_entries(entries));
                cur.backend = if policy == FilePolicy::Forbidden {
                    SecretBackend::MemoryForced
                } else {
                    SecretBackend::MemoryByPolicy
                };
                self.dir
                    .as_deref()
                    .and_then(|d| remove_secret_files(d).err())
            }
            (FilePolicy::Allowed, SecretBackend::MemoryByPolicy) => {
                let Some(dir) = &self.dir else {
                    cur.backend = SecretBackend::Memory;
                    return PolicySwitch {
                        backend: cur.backend,
                        error: None,
                    };
                };
                let file = FileStore::new(dir);
                let r = cur.store.list_entries().and_then(|entries| {
                    file.probe()?;
                    file.merge(&entries.unwrap_or_default())
                });
                if let Err(e) = r {
                    return PolicySwitch {
                        backend: cur.backend,
                        error: Some(e),
                    };
                }
                if read_backend_record(dir).as_deref() != Some(SecretBackend::Keyring.as_str()) {
                    write_backend_record(dir, SecretBackend::File);
                }
                cur.store = Box::new(file);
                cur.backend = SecretBackend::File;
                None
            }
            _ => None,
        };
        PolicySwitch {
            backend: cur.backend,
            error,
        }
    }
}

/// [`FilePolicy::Never`] with the keyring: moves the entries still in
/// `dir/secrets.json` through `store` (a [`KeyringWithLegacyFile`], whose
/// writes drop the file copy only once the keyring took the value), then
/// deletes the `.broken-*` copies. The first failure is returned.
fn migrate_leftovers(dir: &Path, store: &dyn SecretStore) -> Result<()> {
    if fs::symlink_metadata(dir.join(SECRETS_FILE)).is_err() {
        let _ = remove_files(leftover_files(dir));
        return Ok(());
    }
    // A corrupted file is moved aside by the read (and deleted below).
    let entries = FileStore::new(dir).entries();
    let _ = remove_files(leftover_files(dir));
    let entries = entries?;
    let mut first = None;
    for (k, v) in &entries {
        if let Err(e) = store.set(k, v) {
            first.get_or_insert(e);
        }
    }
    if first.is_none() {
        tracing::info!(
            moved = entries.len(),
            "moved leftover file secrets into the keyring"
        );
    }
    first.map_or(Ok(()), Err)
}

impl SecretStore for SwitchableStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.read().store.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.read().store.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.read().store.delete(key)
    }
    fn list_entries(&self) -> Result<Option<BTreeMap<String, String>>> {
        self.read().store.list_entries()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn exercise(store: &dyn SecretStore) {
        let k = secret_key(&ServerId("srv".into()), "uid");
        assert_eq!(k, "rustket:srv:uid");
        assert_eq!(store.get(&k).unwrap(), None);
        store.set(&k, "token-1").unwrap();
        assert_eq!(store.get(&k).unwrap().as_deref(), Some("token-1"));
        store.set(&k, "token-2").unwrap();
        assert_eq!(store.get(&k).unwrap().as_deref(), Some("token-2"));
        store.delete(&k).unwrap();
        assert_eq!(store.get(&k).unwrap(), None);
        store.delete(&k).unwrap();
    }

    #[test]
    fn memory_store() {
        let m = MemoryStore::new();
        exercise(&m);
        m.set("k", "secret-token").unwrap();
        assert!(!format!("{m:?}").contains("secret-token"));
    }

    #[test]
    fn file_store() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        let store = FileStore::new(&sub);
        exercise(&store);
        store.set("a", "b").unwrap();
        // Persisted and readable by a new instance.
        assert_eq!(FileStore::new(&sub).get("a").unwrap().as_deref(), Some("b"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn corrupted_file_is_moved_aside_and_store_recovers() {
        for content in ["{nope", "", "\0\0\0\0", "[]", "{\"a\": 1}"] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(SECRETS_FILE), content).unwrap();
            let store = FileStore::new(dir.path());
            assert!(store.probe().is_ok(), "{content:?}");
            let moved = store.take_recovery().unwrap();
            assert!(moved.to_string_lossy().contains("secrets.json.broken-"));
            assert!(moved.exists());
            store.set("k", "v").unwrap();
            assert_eq!(
                FileStore::new(dir.path()).get("k").unwrap().as_deref(),
                Some("v")
            );
        }
    }

    #[test]
    fn corrupted_file_does_not_degrade_to_memory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(SECRETS_FILE),
            "{\"rustket:srv:uid\": \"tok\"",
        )
        .unwrap();
        let o = select(
            Some(dir.path().to_owned()),
            Err(SecretError::Keyring("no bus".into())),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::File);
        assert!(
            o.notices
                .iter()
                .any(|n| matches!(n, SecretNotice::FileRecovered { .. }))
        );
        o.store.set("k", "v").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_secret_file_is_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join("tokens.json");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(SECRETS_FILE)).unwrap();
        let store = FileStore::new(dir.path());
        store.set("a", "b").unwrap();
        assert!(
            fs::symlink_metadata(store.path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_to_string(&target).unwrap().contains("\"a\""));
    }

    /// A stand-in keyring shared between "starts".
    fn keyring(m: &Arc<MemoryStore>) -> Result<Box<dyn SecretStore>> {
        Ok(Box::new(m.clone()))
    }

    #[test]
    fn file_tokens_move_into_a_working_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let kr = Arc::new(MemoryStore::new());
        // Start 1: no keyring → file.
        let o = select(
            Some(dir.path().to_owned()),
            Err(SecretError::Timeout),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::File);
        o.store.set("rustket:a:u", "T-file").unwrap();
        assert_eq!(read_backend_record(dir.path()).as_deref(), Some("file"));
        // Start 2: the keyring works → the token moves, the plaintext file goes.
        let o = select(
            Some(dir.path().to_owned()),
            keyring(&kr),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::Keyring);
        assert_eq!(o.notices, [SecretNotice::MigratedToKeyring { count: 1 }]);
        assert_eq!(kr.get("rustket:a:u").unwrap().as_deref(), Some("T-file"));
        assert!(!dir.path().join(SECRETS_FILE).exists());
        assert_eq!(
            o.store.get("rustket:a:u").unwrap().as_deref(),
            Some("T-file")
        );
        assert_eq!(read_backend_record(dir.path()).as_deref(), Some("keyring"));
        // Start 3: keyring gone again → the user is told its tokens are unreachable.
        let o = select(
            Some(dir.path().to_owned()),
            Err(SecretError::Timeout),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::File);
        assert!(o.notices.contains(&SecretNotice::KeyringTokensUnreachable));
    }

    #[test]
    fn legacy_file_is_read_and_cleaned_through_the_keyring_store() {
        let dir = tempfile::tempdir().unwrap();
        FileStore::new(dir.path()).set("k", "old").unwrap();
        let store = KeyringWithLegacyFile {
            keyring: Box::new(MemoryStore::new()),
            file: FileStore::new(dir.path()),
        };
        assert_eq!(store.get("k").unwrap().as_deref(), Some("old"));
        store.set("k", "new").unwrap();
        assert!(!dir.path().join(SECRETS_FILE).exists(), "no plaintext left");
        assert_eq!(store.get("k").unwrap().as_deref(), Some("new"));
        FileStore::new(dir.path()).set("k2", "x").unwrap();
        store.delete("k2").unwrap();
        assert_eq!(store.get("k2").unwrap(), None);
    }

    fn no_keyring() -> Result<Box<dyn SecretStore>> {
        Err(SecretError::Keyring("no bus".into()))
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The keychain error of an unsigned sandboxed build (-34018,
    /// `errSecMissingEntitlement`), as the keyring crate reports it.
    fn missing_entitlement() -> Result<Box<dyn SecretStore>> {
        Err(SecretError::Keyring(
            "Platform secure storage failure: A required entitlement isn't present. (-34018)"
                .into(),
        ))
    }

    #[test]
    fn forbidden_file_goes_to_memory_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("data");
        let o = select(
            Some(sub.clone()),
            missing_entitlement(),
            FilePolicy::Forbidden,
        );
        assert_eq!(o.backend, SecretBackend::MemoryForced);
        assert!(o.backend.is_memory());
        assert!(
            matches!(o.notices.as_slice(), [SecretNotice::KeyringUnavailable(why)] if why.contains("-34018")),
            "{:?}",
            o.notices
        );
        o.store.set("rustket:a:u", "T").unwrap();
        assert_eq!(o.store.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert!(!sub.exists(), "nothing written");
        // The same failure in the default policy still falls back to the file.
        let o = select(
            Some(sub.clone()),
            missing_entitlement(),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::File);
        // A left-over file is read into memory and deleted.
        FileStore::new(&sub).set("rustket:a:u", "T-file").unwrap();
        let o = select(
            Some(sub.clone()),
            missing_entitlement(),
            FilePolicy::Forbidden,
        );
        assert_eq!(o.backend, SecretBackend::MemoryForced);
        assert!(o.notices.contains(&SecretNotice::FileRemoved { count: 1 }));
        assert_eq!(
            o.store.get("rustket:a:u").unwrap().as_deref(),
            Some("T-file")
        );
        assert!(!sub.join(SECRETS_FILE).exists());
    }

    #[test]
    fn forbidden_policy_with_a_working_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let kr = Arc::new(MemoryStore::new());
        let o = select(
            Some(dir.path().to_owned()),
            keyring(&kr),
            FilePolicy::Forbidden,
        );
        assert_eq!(o.backend, SecretBackend::Keyring);
        assert!(o.notices.is_empty(), "{:?}", o.notices);
        let sw = SwitchableStore::new(o.store, o.backend, Some(dir.path().to_owned()));
        assert_eq!(
            sw.apply_policy(FilePolicy::Forbidden).backend,
            SecretBackend::Keyring
        );
    }

    #[test]
    fn memory_only_policy_creates_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("data");
        let o = select(Some(sub.clone()), no_keyring(), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::MemoryByPolicy);
        assert!(o.notices.is_empty(), "{:?}", o.notices);
        o.store.set("rustket:a:u", "T").unwrap();
        assert_eq!(o.store.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert!(!sub.exists(), "nothing written");
        // An existing empty directory stays empty too.
        let o = select(Some(dir.path().to_owned()), no_keyring(), FilePolicy::Never);
        o.store.set("k", "v").unwrap();
        assert!(files_in(dir.path()).is_empty());
        // Without a directory the reason is different.
        let o = select(None, no_keyring(), FilePolicy::Allowed);
        assert_eq!(o.backend, SecretBackend::Memory);
        let o = select(None, no_keyring(), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::MemoryByPolicy);
    }

    #[test]
    fn memory_only_policy_loads_and_deletes_a_legacy_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = FileStore::new(dir.path());
        file.set("rustket:a:u", "T-file").unwrap();
        file.set("rustket-e2e:a:u:private_key", "JWK").unwrap();
        fs::write(dir.path().join("secrets.json.broken-1"), "{old").unwrap();
        fs::write(dir.path().join("secrets.json.broken-2"), "{old").unwrap();
        fs::write(dir.path().join("config.toml"), "").unwrap();
        let o = select(Some(dir.path().to_owned()), no_keyring(), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::MemoryByPolicy);
        assert_eq!(o.notices, [SecretNotice::FileRemoved { count: 2 }]);
        // Still signed in for this session.
        assert_eq!(
            o.store.get("rustket:a:u").unwrap().as_deref(),
            Some("T-file")
        );
        assert_eq!(
            o.store
                .get("rustket-e2e:a:u:private_key")
                .unwrap()
                .as_deref(),
            Some("JWK")
        );
        assert_eq!(files_in(dir.path()), ["config.toml"]);
        o.store.set("rustket:b:u", "T2").unwrap();
        assert!(!dir.path().join(SECRETS_FILE).exists());
    }

    #[test]
    fn memory_only_policy_deletes_a_corrupted_legacy_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SECRETS_FILE), "{nope").unwrap();
        let o = select(Some(dir.path().to_owned()), no_keyring(), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::MemoryByPolicy);
        // Nothing kept: no "0 entries kept in memory" notice.
        assert!(o.notices.is_empty(), "{:?}", o.notices);
        assert!(files_in(dir.path()).is_empty());
    }

    #[test]
    fn memory_only_policy_reports_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        // A directory in place of the file: it cannot be read.
        fs::create_dir(dir.path().join(SECRETS_FILE)).unwrap();
        let o = select(Some(dir.path().to_owned()), no_keyring(), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::MemoryByPolicy);
        assert_eq!(
            o.notices,
            [SecretNotice::FileNotRemoved {
                path: dir.path().join(SECRETS_FILE)
            }]
        );
        assert!(dir.path().join(SECRETS_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn removing_secret_files_follows_a_symlink_and_drops_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let dotfiles = dir.path().join("dotfiles");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&dotfiles).unwrap();
        let target = dotfiles.join("secrets.json");
        std::os::unix::fs::symlink(&target, data.join(SECRETS_FILE)).unwrap();
        // Written through the link.
        FileStore::new(&data).set("rustket:a:u", "T").unwrap();
        assert!(target.exists());
        fs::write(data.join(".secrets.json.tmp-abc"), "{}").unwrap();
        fs::write(dotfiles.join(".secrets.json.tmp-def"), "{}").unwrap();
        fs::write(dotfiles.join("other"), "").unwrap();
        let o = select(Some(data.clone()), no_keyring(), FilePolicy::Never);
        assert_eq!(o.notices, [SecretNotice::FileRemoved { count: 1 }]);
        assert_eq!(o.store.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert!(files_in(&data).is_empty());
        assert_eq!(files_in(&dotfiles), ["other"]);
    }

    #[test]
    fn memory_only_policy_keeps_the_keyring_migration() {
        let dir = tempfile::tempdir().unwrap();
        FileStore::new(dir.path()).set("rustket:a:u", "T").unwrap();
        fs::write(dir.path().join("secrets.json.broken-1"), "{old").unwrap();
        let kr = Arc::new(MemoryStore::new());
        let o = select(Some(dir.path().to_owned()), keyring(&kr), FilePolicy::Never);
        assert_eq!(o.backend, SecretBackend::Keyring);
        assert_eq!(o.notices, [SecretNotice::MigratedToKeyring { count: 1 }]);
        assert_eq!(kr.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert_eq!(files_in(dir.path()), ["secret-backend"]);
    }

    #[test]
    fn switchable_store_moves_between_file_and_memory() {
        let dir = tempfile::tempdir().unwrap();
        let o = select(
            Some(dir.path().to_owned()),
            no_keyring(),
            FilePolicy::Allowed,
        );
        assert_eq!(o.backend, SecretBackend::File);
        let sw = Arc::new(SwitchableStore::new(
            o.store,
            o.backend,
            Some(dir.path().to_owned()),
        ));
        // E2E managers get the same instance.
        let e2e: Arc<dyn SecretStore> = sw.clone();
        e2e.set("rustket-e2e:a:u:pending_passphrase", "pass")
            .unwrap();
        sw.set("rustket:a:u", "T").unwrap();
        fs::write(dir.path().join("secrets.json.broken-9"), "{x").unwrap();
        // On: everything moves to memory, the files go.
        let r = sw.apply_policy(FilePolicy::Never);
        assert!(r.error.is_none());
        assert_eq!(r.backend, SecretBackend::MemoryByPolicy);
        assert_eq!(sw.backend(), SecretBackend::MemoryByPolicy);
        assert_eq!(files_in(dir.path()), ["secret-backend"]);
        assert_eq!(
            e2e.get("rustket-e2e:a:u:pending_passphrase")
                .unwrap()
                .as_deref(),
            Some("pass")
        );
        e2e.set("rustket-e2e:a:u:private_key", "JWK").unwrap();
        assert!(!dir.path().join(SECRETS_FILE).exists());
        // Off: persisted to the file.
        let r = sw.apply_policy(FilePolicy::Allowed);
        assert!(r.error.is_none());
        assert_eq!(r.backend, SecretBackend::File);
        let on_disk = FileStore::new(dir.path()).entries().unwrap();
        assert_eq!(on_disk.len(), 3);
        assert_eq!(on_disk["rustket-e2e:a:u:private_key"], "JWK");
        // On again: nothing lost.
        sw.apply_policy(FilePolicy::Never);
        assert_eq!(sw.list_entries().unwrap().unwrap(), on_disk);
        assert_eq!(e2e.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert!(!dir.path().join(SECRETS_FILE).exists());
    }

    #[test]
    fn switchable_store_through_e2e_secrets() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sw = Arc::new(SwitchableStore::new(
            Box::new(FileStore::new(dir.path())),
            SecretBackend::File,
            Some(dir.path().to_owned()),
        ));
        let secrets = crate::e2e::E2eSecrets::new(sw.clone(), &ServerId("srv".into()), "uid");
        rt.block_on(secrets.set_private_key("JWK")).unwrap();
        assert!(dir.path().join(SECRETS_FILE).exists());
        sw.apply_policy(FilePolicy::Never);
        assert!(!dir.path().join(SECRETS_FILE).exists());
        let key = rt.block_on(secrets.private_key()).unwrap();
        assert_eq!(key.as_deref().map(String::as_str), Some("JWK"));
    }

    #[test]
    fn switchable_store_keeps_memory_when_the_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the directory should be: the file store fails.
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, "").unwrap();
        let sw = SwitchableStore::new(
            Box::new(MemoryStore::new()),
            SecretBackend::MemoryByPolicy,
            Some(blocked.join("sub")),
        );
        sw.set("k", "v").unwrap();
        let r = sw.apply_policy(FilePolicy::Allowed);
        assert!(r.error.is_some());
        assert_eq!(r.backend, SecretBackend::MemoryByPolicy);
        assert_eq!(sw.get("k").unwrap().as_deref(), Some("v"));
        // A keyring backend keeps its backend.
        let sw = SwitchableStore::new(
            Box::new(MemoryStore::new()),
            SecretBackend::Keyring,
            Some(dir.path().to_owned()),
        );
        assert_eq!(
            sw.apply_policy(FilePolicy::Never).backend,
            SecretBackend::Keyring
        );
        assert_eq!(
            sw.apply_policy(FilePolicy::Allowed).backend,
            SecretBackend::Keyring
        );
    }

    /// A keyring that refuses every write.
    struct BrokenKeyring;

    impl SecretStore for BrokenKeyring {
        fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Err(SecretError::Keyring("locked".into()))
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn memory_only_policy_retries_leftovers_into_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        FileStore::new(dir.path()).set("rustket:a:u", "T").unwrap();
        // The keyring refuses the startup migration: the entry stays in the file.
        let o = select(
            Some(dir.path().to_owned()),
            Ok(Box::new(BrokenKeyring)),
            FilePolicy::Allowed,
        );
        assert_eq!(o.notices, [SecretNotice::PlaintextTokensLeft { count: 1 }]);
        let sw = SwitchableStore::new(o.store, o.backend, Some(dir.path().to_owned()));
        let r = sw.apply_policy(FilePolicy::Never);
        assert_eq!(r.backend, SecretBackend::Keyring);
        assert!(r.error.is_some(), "still refused: reported");
        assert!(dir.path().join(SECRETS_FILE).exists());
        // Later the keyring works: turning the switch on moves the entry.
        let kr = Arc::new(MemoryStore::new());
        let store = KeyringWithLegacyFile {
            keyring: Box::new(kr.clone()),
            file: FileStore::new(dir.path()),
        };
        fs::write(dir.path().join("secrets.json.broken-1"), "{x").unwrap();
        let sw = SwitchableStore::new(
            Box::new(store),
            SecretBackend::Keyring,
            Some(dir.path().to_owned()),
        );
        let r = sw.apply_policy(FilePolicy::Never);
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(kr.get("rustket:a:u").unwrap().as_deref(), Some("T"));
        assert_eq!(files_in(dir.path()), ["secret-backend"]);
    }

    #[test]
    fn worker_calls_time_out() {
        let r: Result<()> = on_worker(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Ok(())
        });
        assert!(matches!(r, Err(SecretError::Timeout)));
        // Later calls wait behind the stuck one and time out too, without hanging.
        let r: Result<u8> = on_worker(Duration::from_millis(10), || Ok(1));
        assert!(matches!(r, Err(SecretError::Timeout)));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(on_worker(Duration::from_secs(5), || Ok(7)).unwrap(), 7);
    }

    /// Talks to the real Secret Service; opt-in because it may prompt to unlock.
    #[test]
    #[ignore]
    fn keyring_store() {
        KeyringStore::probe().unwrap();
        exercise(&KeyringStore::new());
    }
}
