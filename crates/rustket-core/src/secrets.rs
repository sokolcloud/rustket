//! Secret storage for auth tokens.
//!
//! The OS keyring (Secret Service on Linux, Keychain, Windows Credential Manager)
//! is preferred; when it is unavailable [`open_default`] falls back to a JSON file
//! in the XDG data directory (mode 0600) and reports
//! [`SecretBackend::File`] so the UI can warn that tokens are stored unencrypted.
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
//! Keys are built with [`secret_key`]: `rustket:<server_id>:<user_id>`; values are
//! auth tokens. Values are never logged.
//!
//! The backends do blocking I/O: call these methods from a blocking context
//! (`tokio::task::spawn_blocking`), not from an async task.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex};
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
}

/// Which backend [`open_default`] selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBackend {
    Keyring,
    /// Plain file fallback: the UI should warn the user.
    File,
    /// Neither the keyring nor the data directory is usable; tokens are kept in
    /// memory only and the user has to log in on every start.
    Memory,
}

impl SecretBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::Memory => "memory",
        }
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
            probe.set_password(&value).map_err(kr_err)?;
            let read = probe.get_password().map_err(kr_err);
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
        on_worker(self.timeout, move || match entry(&key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(kr_err(e)),
        })
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        on_worker(self.timeout, move || {
            entry(&key)?.set_password(&value).map_err(kr_err)
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
    /// Tokens are still in the plaintext file (moving them failed).
    PlaintextTokensLeft { count: usize },
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

/// Picks the backend given the keyring probe result and the file directory.
fn select(dir: Option<PathBuf>, keyring: Result<Box<dyn SecretStore>>) -> OpenedSecrets {
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
                tracing::info!(moved, "moved tokens from the secret file into the keyring");
                notices.push(SecretNotice::MigratedToKeyring { count: moved });
            }
            if left > 0 {
                notices.push(SecretNotice::PlaintextTokensLeft { count: left });
            }
            if let Some(moved_to) = file.take_recovery() {
                notices.push(SecretNotice::FileRecovered { moved_to });
            }
            write_backend_record(&dir, SecretBackend::Keyring);
            OpenedSecrets {
                store: Box::new(KeyringWithLegacyFile { keyring, file }),
                backend: SecretBackend::Keyring,
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

/// Probes the OS keyring and falls back to the file store, then to memory,
/// migrating file tokens into a working keyring. Blocking: call from
/// `spawn_blocking`.
pub fn open_default_report() -> OpenedSecrets {
    let keyring =
        KeyringStore::probe().map(|()| Box::new(KeyringStore::new()) as Box<dyn SecretStore>);
    select(FileStore::default_dir(), keyring)
}

/// [`open_default_report`] without the notices.
pub fn open_default() -> (Box<dyn SecretStore>, SecretBackend) {
    let o = open_default_report();
    (o.store, o.backend)
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
        let o = select(Some(dir.path().to_owned()), Err(SecretError::Timeout));
        assert_eq!(o.backend, SecretBackend::File);
        o.store.set("rustket:a:u", "T-file").unwrap();
        assert_eq!(read_backend_record(dir.path()).as_deref(), Some("file"));
        // Start 2: the keyring works → the token moves, the plaintext file goes.
        let o = select(Some(dir.path().to_owned()), keyring(&kr));
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
        let o = select(Some(dir.path().to_owned()), Err(SecretError::Timeout));
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
