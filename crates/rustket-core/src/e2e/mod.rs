//! End-to-end encryption state for one account (`docs/e2e.md` §7, §14.3).
//!
//! [`E2eManager`] owns the account key lifecycle (load, unlock with the E2E
//! passphrase, first-time key creation, passphrase change, reset), one
//! [`RoomCrypto`](room) per room (handshake, suggested key accept/reject, room
//! key creation and reset, old keys), the key distribution loop and a decrypt
//! cache. Feed it rooms and subscriptions ([`E2eManager::update_room`],
//! [`E2eManager::update_subscription`] or [`E2eManager::handle_event`]) and call
//! [`E2eManager::decrypt_message`] / [`E2eManager::encrypt_outgoing`].
//!
//! Blocking crypto (RSA, PBKDF2) runs on `spawn_blocking`. The synchronous file
//! helpers ([`E2eManager::prepare_upload`], [`decrypt_download`]) process the whole
//! file on the calling thread; use their `_async` variants from UI code. Key
//! material is never logged; logs carry rid, kid, message id and state names only
//! (debug level).

mod account;
mod api;
mod cache;
mod crypt;
mod distribution;
mod files;
mod room;
mod room_keys;
pub mod shown;
mod store;
#[cfg(test)]
mod tests;
mod types;

pub use api::E2eApi;
pub use cache::DecryptCache;
pub use files::{decrypt_download, decrypt_download_async, upload_payload};
pub use room::RoomE2eState;
pub use store::{E2E_SECRET_PREFIX, E2eSecrets, pending_passphrase_secret, private_key_secret};
pub use types::{
    AccountE2eState, DecryptFailure, DecryptResult, DecryptedContent, E2E_SETTING_IDS, E2eConfig,
    E2eEvent, E2eManagerError, E2eSettings, OutgoingEncrypted, PreparedUpload, Result,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rocketchat::e2e::{RoomKey, UserPrivateKey, UserPublicKey, Zeroizing};
use rocketchat::rest::{SuggestedKey, SuggestedOldKey, UserPublicKeyEntry};
use tokio::sync::broadcast;

use crate::config::ServerId;
use crate::secrets::SecretStore;
use room::RoomCrypto;

#[cfg(test)]
use crate::secrets::SecretError;
#[cfg(test)]
use rocketchat::models::MessageAttachment;

/// Room keys usable for synchronous decryption.
#[derive(Debug, Default, Clone)]
struct RoomKeySet {
    current: Option<RoomKey>,
    old: Vec<RoomKey>,
}

impl RoomKeySet {
    fn by_kid(&self, kid: &str) -> Option<&RoomKey> {
        self.current
            .iter()
            .chain(self.old.iter())
            .find(|k| k.kid == kid)
    }
}

/// Synchronous view of a room for getters and the distribution loop.
#[derive(Debug, Default, Clone)]
struct RoomSummary {
    state: RoomE2eState,
    waiting: Option<Vec<String>>,
    has_key: bool,
    encrypted: bool,
}

struct Account {
    state: AccountE2eState,
    public_key: Option<UserPublicKey>,
    private_key: Option<Arc<UserPrivateKey>>,
    /// `users.e2e.private_key` while waiting for the passphrase.
    server_envelope: Option<String>,
    server_public: Option<String>,
    pending_passphrase: Option<Zeroizing<String>>,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            state: AccountE2eState::Disabled,
            public_key: None,
            private_key: None,
            server_envelope: None,
            server_public: None,
            pending_passphrase: None,
        }
    }
}

/// One room's crypto state behind its async mutex.
type RoomCell = Arc<tokio::sync::Mutex<RoomCrypto>>;

struct Inner<A: E2eApi> {
    api: A,
    user_id: String,
    secrets: E2eSecrets,
    config: E2eConfig,
    settings: RwLock<E2eSettings>,
    account: Mutex<Account>,
    /// Serializes account operations (start, unlock, change, reset).
    account_op: tokio::sync::Mutex<()>,
    /// Incremented by [`E2eManager::stop`]: in-flight operations started before
    /// drop their result.
    stops: AtomicU64,
    /// Incremented by [`E2eManager::logout`]: secrets written by an operation
    /// that raced a logout are deleted again.
    logouts: AtomicU64,
    next_cell: AtomicU64,
    rooms: Mutex<HashMap<String, (u64, RoomCell)>>,
    /// Last `e2e.keyRequest` handled per room (debounce).
    key_requests: Mutex<HashMap<String, Instant>>,
    summaries: RwLock<HashMap<String, RoomSummary>>,
    keys: RwLock<HashMap<String, Arc<RoomKeySet>>>,
    cache: Mutex<DecryptCache>,
    events: broadcast::Sender<E2eEvent>,
    distribution: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// E2E manager of one account. Cheap to clone (shared state).
pub struct E2eManager<A: E2eApi = rocketchat::RestClient> {
    inner: Arc<Inner<A>>,
}

impl<A: E2eApi> Clone for E2eManager<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<A: E2eApi> std::fmt::Debug for E2eManager<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eManager")
            .field("user_id", &self.inner.user_id)
            .field("state", &self.account_state())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read<T>(m: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(m: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs blocking crypto off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| E2eManagerError::Task(e.to_string()))
}

/// Minimum time between two distributions triggered by `e2e.keyRequest` for the
/// same room.
const KEY_REQUEST_DEBOUNCE: Duration = Duration::from_secs(3);

/// Snapshot of the stop/logout counters taken when an operation starts.
#[derive(Debug, Clone, Copy)]
struct Token {
    stops: u64,
    logouts: u64,
}

/// A random `u64` (std's per-process random hasher keys mixed with a counter).
pub(crate) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.finish()
}

impl<A: E2eApi> E2eManager<A> {
    /// A manager for `user_id` on `server`; nothing happens until [`Self::start`].
    pub fn new(
        api: A,
        store: Arc<dyn SecretStore>,
        server: &ServerId,
        user_id: &str,
        settings: E2eSettings,
        config: E2eConfig,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let cache = DecryptCache::new(config.cache_capacity);
        Self {
            inner: Arc::new(Inner {
                api,
                user_id: user_id.to_owned(),
                secrets: E2eSecrets::new(store, server, user_id),
                config,
                settings: RwLock::new(settings),
                account: Mutex::new(Account::default()),
                account_op: tokio::sync::Mutex::new(()),
                stops: AtomicU64::new(0),
                logouts: AtomicU64::new(0),
                next_cell: AtomicU64::new(0),
                rooms: Mutex::new(HashMap::new()),
                key_requests: Mutex::new(HashMap::new()),
                summaries: RwLock::new(HashMap::new()),
                keys: RwLock::new(HashMap::new()),
                cache: Mutex::new(cache),
                events,
                distribution: Mutex::new(None),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<E2eEvent> {
        self.inner.events.subscribe()
    }

    pub fn user_id(&self) -> &str {
        &self.inner.user_id
    }

    pub fn api(&self) -> &A {
        &self.inner.api
    }

    pub fn account_state(&self) -> AccountE2eState {
        lock(&self.inner.account).state.clone()
    }

    pub fn settings(&self) -> E2eSettings {
        read(&self.inner.settings).clone()
    }

    /// The generated passphrase the user still has to save (state `SavePassword`).
    pub fn pending_passphrase(&self) -> Option<Zeroizing<String>> {
        lock(&self.inner.account).pending_passphrase.clone()
    }

    pub fn room_state(&self, rid: &str) -> RoomE2eState {
        read(&self.inner.summaries)
            .get(rid)
            .map(|s| s.state.clone())
            .unwrap_or_default()
    }

    /// Whether messages to `rid` can be encrypted now (E2E on, account ready,
    /// room encrypted and its key loaded).
    pub fn can_encrypt(&self, rid: &str) -> bool {
        self.settings().enabled
            && self.account_state().is_ready()
            && read(&self.inner.summaries)
                .get(rid)
                .is_some_and(|s| s.encrypted)
            && read(&self.inner.keys)
                .get(rid)
                .is_some_and(|k| k.current.is_some())
    }

    fn emit(&self, ev: E2eEvent) {
        let _ = self.inner.events.send(ev);
    }

    fn set_account_state(&self, state: AccountE2eState) {
        let changed = {
            let mut acc = lock(&self.inner.account);
            let changed = acc.state != state;
            acc.state = state.clone();
            changed
        };
        if changed {
            tracing::debug!(state = ?state_name(&state), "e2e account state");
            self.emit(E2eEvent::AccountState(state));
        }
    }

    fn token(&self) -> Token {
        Token {
            stops: self.inner.stops.load(Ordering::SeqCst),
            logouts: self.inner.logouts.load(Ordering::SeqCst),
        }
    }

    fn alive(&self, t: Token) -> bool {
        self.inner.stops.load(Ordering::SeqCst) == t.stops
    }

    fn check(&self, t: Token) -> Result<()> {
        if self.alive(t) {
            Ok(())
        } else {
            Err(E2eManagerError::Cancelled)
        }
    }

    /// After writing secrets: if a logout happened meanwhile, delete them again.
    async fn check_after_secret_write(&self, t: Token) -> Result<()> {
        if self.inner.logouts.load(Ordering::SeqCst) != t.logouts
            && let Err(e) = self.inner.secrets.clear().await
        {
            tracing::warn!(error = %e, "cannot delete E2E secrets after logout");
        }
        self.check(t)
    }

    /// Mutations of private-key state while `E2E_Enable` is on and the account
    /// is ready (not read-only).
    fn writable(&self) -> bool {
        self.settings().enabled && self.account_state().is_ready()
    }

    fn private_key(&self) -> Option<Arc<UserPrivateKey>> {
        let acc = lock(&self.inner.account);
        if acc.state.has_keys() {
            acc.private_key.clone()
        } else {
            None
        }
    }

    fn public_key(&self) -> Option<UserPublicKey> {
        lock(&self.inner.account).public_key.clone()
    }
}

/// Wraps the current (and old) room keys for each member's public key on a
/// blocking thread. Members whose key cannot be used are skipped.
async fn wrap_for_users(
    current: RoomKey,
    old: Vec<(RoomKey, String)>,
    users: Vec<UserPublicKeyEntry>,
) -> Vec<SuggestedKey> {
    let r = blocking(move || {
        users
            .into_iter()
            .filter_map(|u| {
                let public = UserPublicKey::from_jwk_str(&u.public_key).ok()?;
                let key = current.wrap_for(&public).ok()?;
                let old_keys = old
                    .iter()
                    .filter_map(|(k, ts)| {
                        Some(SuggestedOldKey {
                            e2e_key_id: k.kid.clone(),
                            ts: ts.clone(),
                            e2e_key: k.wrap_for(&public).ok()?,
                        })
                    })
                    .collect();
                Some(SuggestedKey {
                    user_id: u.user_id,
                    key,
                    old_keys,
                })
            })
            .collect::<Vec<_>>()
    })
    .await;
    r.unwrap_or_default()
}

fn state_name(s: &AccountE2eState) -> &'static str {
    match s {
        AccountE2eState::Disabled => "disabled",
        AccountE2eState::DisabledReadOnly => "disabled_read_only",
        AccountE2eState::LoadingKeys => "loading_keys",
        AccountE2eState::EnterPassword { .. } => "enter_password",
        AccountE2eState::SavePassword => "save_password",
        AccountE2eState::Ready => "ready",
        AccountE2eState::Error(_) => "error",
    }
}

fn room_state_name(s: &RoomE2eState) -> &'static str {
    match s {
        RoomE2eState::NotStarted => "not_started",
        RoomE2eState::Establishing => "establishing",
        RoomE2eState::CreatingKeys => "creating_keys",
        RoomE2eState::WaitingKeys => "waiting_keys",
        RoomE2eState::KeysReceived => "keys_received",
        RoomE2eState::Ready => "ready",
        RoomE2eState::Disabled => "disabled",
        RoomE2eState::Error(_) => "error",
    }
}
