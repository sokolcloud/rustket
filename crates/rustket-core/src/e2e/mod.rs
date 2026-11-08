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

mod api;
mod cache;
mod distribution;
mod room;
pub mod shown;
mod store;
#[cfg(test)]
mod tests;

pub use api::E2eApi;
pub use cache::DecryptCache;
pub use room::RoomE2eState;
pub use store::{E2E_SECRET_PREFIX, E2eSecrets, pending_passphrase_secret, private_key_secret};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rocketchat::auth::TwoFactorCode;
use rocketchat::e2e::{
    DEFAULT_USERNAME_PATTERN, DecryptedMessage, E2eError, E2eMentions, EncryptedContent, FileKey,
    FileMetadata, PrivateKeyEnvelope, RoomE2eKeyRequest, RoomKey, Sha256Hasher, UserKeyPair,
    UserPrivateKey, UserPublicKey, Zeroizing, e2e_mentions, encrypt_v2, generate_passphrase,
    hashed_upload_name, new_key_id, validate_new_passphrase,
};
use rocketchat::models::{
    FileInfo, Message, MessageAttachment, OldRoomKey, Room, Subscription, UploadedFile,
};
use rocketchat::rest::{SetKeyIdOutcome, SuggestedKey, SuggestedOldKey, UserPublicKeyEntry};
use rocketchat::session::{ChangeKind, SessionEvent};
use rocketchat::{RcDate, ServerUrl};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::ServerId;
use crate::secrets::{SecretError, SecretStore};
use room::{OldKey, RoomCrypto, prefix_kid};

/// Account E2E state (web `E2EEState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AccountE2eState {
    /// Not started, stopped, or `E2E_Enable` is off at start without cached keys.
    #[default]
    Disabled,
    /// `E2E_Enable` is off but the cached private key is loaded: existing room
    /// keys decrypt, nothing is encrypted, created, accepted or distributed
    /// (§11.12). Call [`E2eManager::start`] again when `E2E_Enable` turns on.
    DisabledReadOnly,
    LoadingKeys,
    /// The server has a key backup; the user must enter the E2E passphrase.
    /// `failed`: the last attempt was wrong.
    EnterPassword {
        failed: bool,
    },
    /// Ready, but a generated passphrase is still waiting to be saved by the user.
    SavePassword,
    Ready,
    Error(String),
}

impl AccountE2eState {
    /// `Ready` and `SavePassword` both have the private key loaded.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready | Self::SavePassword)
    }

    /// The private key is loaded (ready, or read-only).
    pub fn has_keys(&self) -> bool {
        self.is_ready() || *self == Self::DisabledReadOnly
    }
}

/// Notifications for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E2eEvent {
    AccountState(AccountE2eState),
    RoomState {
        rid: String,
        state: RoomE2eState,
    },
    /// Room keys changed: messages of `rid` may decrypt now. `pending` lists the
    /// ids that previously came back as [`DecryptResult::Pending`].
    KeysChanged {
        rid: String,
        pending: Vec<String>,
    },
}

/// Ids of the public settings [`E2eSettings`] reads.
pub const E2E_SETTING_IDS: [&str; 5] = [
    "E2E_Enable",
    "E2E_Enable_Encrypt_Files",
    "E2E_Enabled_Mentions",
    "E2E_Allow_Unencrypted_Messages",
    "UTF8_User_Names_Validation",
];

/// E2E-related public settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2eSettings {
    /// `E2E_Enable`.
    pub enabled: bool,
    /// `E2E_Enable_Encrypt_Files`.
    pub encrypt_files: bool,
    /// `E2E_Enabled_Mentions`.
    pub mentions: bool,
    /// `E2E_Allow_Unencrypted_Messages`.
    pub allow_unencrypted: bool,
    /// `UTF8_User_Names_Validation`.
    pub username_pattern: String,
}

impl Default for E2eSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            encrypt_files: true,
            mentions: true,
            allow_unencrypted: false,
            username_pattern: DEFAULT_USERNAME_PATTERN.to_owned(),
        }
    }
}

impl E2eSettings {
    /// Whether the server's E2E settings are known: `E2E_Enable` is a public
    /// setting on every supported server, so a settings map without it was not
    /// loaded (e.g. `settings.public` failed). Encrypted rooms must fail closed
    /// until then.
    pub fn loaded(s: &crate::settings::Settings) -> bool {
        s.get("E2E_Enable").is_some()
    }

    pub fn from_settings(s: &crate::settings::Settings) -> Self {
        let d = Self::default();
        Self {
            enabled: s.bool_or("E2E_Enable", d.enabled),
            encrypt_files: s.bool_or("E2E_Enable_Encrypt_Files", d.encrypt_files),
            mentions: s.bool_or("E2E_Enabled_Mentions", d.mentions),
            allow_unencrypted: s.bool_or("E2E_Allow_Unencrypted_Messages", d.allow_unencrypted),
            username_pattern: s
                .str("UTF8_User_Names_Validation")
                .filter(|p| !p.is_empty())
                .unwrap_or(DEFAULT_USERNAME_PATTERN)
                .to_owned(),
        }
    }

    /// Applies one changed setting; `true` if it is an E2E setting.
    pub fn apply(&mut self, id: &str, value: &Value) -> bool {
        let b = value.as_bool();
        match (id, b) {
            ("E2E_Enable", Some(v)) => self.enabled = v,
            ("E2E_Enable_Encrypt_Files", Some(v)) => self.encrypt_files = v,
            ("E2E_Enabled_Mentions", Some(v)) => self.mentions = v,
            ("E2E_Allow_Unencrypted_Messages", Some(v)) => self.allow_unencrypted = v,
            ("UTF8_User_Names_Validation", _) => {
                self.username_pattern = value
                    .as_str()
                    .filter(|p| !p.is_empty())
                    .unwrap_or(DEFAULT_USERNAME_PATTERN)
                    .to_owned();
            }
            _ => return false,
        }
        true
    }
}

/// Tuning of an [`E2eManager`].
#[derive(Debug, Clone)]
pub struct E2eConfig {
    /// Period of the key distribution loop (jittered by ±20 %).
    pub distribution_interval: Duration,
    /// Start the loop automatically when the account becomes ready.
    pub auto_distribution: bool,
    /// Decrypted attachment URLs must be relative or on this server.
    pub server_url: Option<ServerUrl>,
    pub cache_capacity: usize,
}

impl Default for E2eConfig {
    fn default() -> Self {
        Self {
            distribution_interval: Duration::from_secs(10),
            auto_distribution: true,
            server_url: None,
            cache_capacity: cache::DEFAULT_CAPACITY,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum E2eManagerError {
    /// `E2E_Enable` is off: nothing may be encrypted.
    #[error("end-to-end encryption is disabled on the server")]
    Disabled,
    /// The account keys are not loaded.
    #[error("end-to-end encryption keys are not loaded")]
    NotReady,
    /// The room key is missing (waiting for another member or a reset).
    #[error("the room encryption key is not available")]
    RoomNotReady,
    /// Encryption is turned off in this room (old keys still decrypt).
    #[error("encryption is turned off in this room")]
    RoomNotEncrypted,
    /// The account was logged out or stopped while the operation ran.
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("incorrect encryption password")]
    WrongPassword,
    /// The room has no key id yet (nothing to reset).
    #[error("the room has no encryption key yet")]
    NoRoomKeyId,
    #[error(transparent)]
    Crypto(#[from] E2eError),
    #[error(transparent)]
    Rest(#[from] rocketchat::Error),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("background task failed: {0}")]
    Task(String),
}

pub type Result<T, E = E2eManagerError> = std::result::Result<T, E>;

/// A decrypted message payload.
#[derive(Debug, Clone, PartialEq)]
pub struct DecryptedContent {
    pub message: DecryptedMessage,
    /// Kid of the envelope (`None` for an `e2e` message without content).
    pub kid: Option<String>,
    /// The server message also carries plaintext `msg` next to `content` (a
    /// plaintext edit, §11.14): the decrypted content wins, but the UI should
    /// mark the message.
    pub plaintext_edit: bool,
}

/// Why a message could not be decrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecryptFailure {
    /// No key for this message (`E2E_indecipherable`, e.g. after resets).
    NoKey,
    /// A key was found but decryption or parsing failed (`E2E_Key_Error`).
    KeyError,
    /// An envelope algorithm that cannot be decrypted (e.g. megolm).
    Unsupported(String),
    /// Malformed envelope.
    Malformed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecryptResult {
    /// Not an `e2e` message: render as is.
    NotEncrypted,
    Decrypted(Arc<DecryptedContent>),
    /// The room key is not available yet; retry on [`E2eEvent::KeysChanged`].
    Pending,
    Failed(DecryptFailure),
}

/// The encrypted part of an outgoing message.
#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingEncrypted {
    /// v2 envelope (`content` of `chat.sendMessage` / `chat.update`).
    pub content: Value,
    /// Plaintext mentions to send next to it (`None` when disabled).
    pub mentions: Option<E2eMentions>,
}

/// An encrypted file ready for `rooms.media`.
pub struct PreparedUpload {
    /// Upload file name: hex SHA-256 of the real name.
    pub hashed_name: String,
    pub encrypted: rocketchat::rest::Bytes,
    pub key: FileKey,
    pub metadata: FileMetadata,
    /// v2 envelope of `metadata` (`content` part of `rooms.media`,
    /// `fileContent` of `rooms.mediaConfirm`).
    pub file_content: Value,
    pub size: u64,
}

impl std::fmt::Debug for PreparedUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedUpload")
            .field("hashed_name", &self.hashed_name)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

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

    // ----- account lifecycle -------------------------------------------------

    /// Loads or creates the account keys (§7.1). Never generates keys when the
    /// server could not be asked. On success the rooms known so far are
    /// handshaken and the distribution loop starts. With `E2E_Enable` off only a
    /// cached key is loaded, read-only ([`AccountE2eState::DisabledReadOnly`]).
    /// Failures leave the account in [`AccountE2eState::Error`].
    pub async fn start(&self) -> Result<()> {
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let r = if self.settings().enabled {
            self.set_account_state(AccountE2eState::LoadingKeys);
            self.start_enabled(t).await
        } else {
            self.start_read_only(t).await
        };
        if let Err(e) = &r
            && !matches!(e, E2eManagerError::Cancelled)
            && self.alive(t)
        {
            self.set_account_state(AccountE2eState::Error(e.to_string()));
        }
        r
    }

    /// The cached private key: `Ok(None)` when there is none (or it does not
    /// parse), `Err` when the secret store could not be read (nothing may be
    /// deleted then).
    async fn cached_private_key(&self) -> Result<Option<UserPrivateKey>, SecretError> {
        let Some(cached) = self.inner.secrets.private_key().await? else {
            return Ok(None);
        };
        Ok(blocking(move || UserPrivateKey::from_jwk_str(&cached).ok())
            .await
            .ok()
            .flatten())
    }

    /// `E2E_Enable` off: load the cached key (if it still matches the server's
    /// public key) for decryption only. Nothing is created or uploaded.
    async fn start_read_only(&self, t: Token) -> Result<()> {
        let Some(key) = self.cached_private_key().await? else {
            self.check(t)?;
            self.set_account_state(AccountE2eState::Disabled);
            return Ok(());
        };
        let matches = match self.inner.api.fetch_my_keys().await {
            Ok(server) => server
                .public_key
                .and_then(|p| UserPublicKey::from_jwk_str(&p).ok())
                .is_some_and(|p| p == key.public_key()),
            // Offline: decrypting with the cached key is harmless.
            Err(e) => {
                tracing::debug!(error = %e, "cannot reconcile the cached E2E key");
                true
            }
        };
        self.check(t)?;
        if !matches {
            self.set_account_state(AccountE2eState::Disabled);
            return Ok(());
        }
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(key.public_key());
            acc.private_key = Some(Arc::new(key));
        }
        self.set_account_state(AccountE2eState::DisabledReadOnly);
        self.handshake_all().await;
        Ok(())
    }

    async fn start_enabled(&self, t: Token) -> Result<()> {
        // A secret store read error is not "no cached key": fail (retried
        // later) and delete nothing (§7.1).
        let cached_key = self.cached_private_key().await.inspect_err(|e| {
            tracing::warn!(error = %e, "cannot read the cached E2E key");
        })?;
        let server = self.inner.api.fetch_my_keys().await?;
        self.check(t)?;
        let pending = self.inner.secrets.pending_passphrase().await.inspect_err(
            |e| tracing::warn!(error = %e, "cannot read the pending E2E passphrase"),
        )?;

        if let (Some(server_pub), Some(envelope)) = (server.public_key, server.private_key) {
            let server_public = UserPublicKey::from_jwk_str(&server_pub).ok();
            let stale_key = cached_key.is_some();
            let usable = cached_key.filter(|k| server_public.as_ref() == Some(&k.public_key()));
            self.check(t)?;
            if let Some(key) = usable {
                {
                    let mut acc = lock(&self.inner.account);
                    acc.public_key = Some(key.public_key());
                    acc.private_key = Some(Arc::new(key));
                    acc.server_public = Some(server_pub);
                    acc.server_envelope = Some(envelope);
                    acc.pending_passphrase = pending.clone();
                }
                self.became_ready(t, pending.is_some()).await;
                return Ok(());
            }
            // No usable cached key. A generated passphrase the user has not
            // saved yet survives a forced logout: it unlocks the backup again.
            if let Some(pw) = pending.clone() {
                let restored = self
                    .unlock_envelope(&envelope, server_public.as_ref(), &pw)
                    .await?;
                self.check(t)?;
                if let Some((key, jwk)) = restored {
                    if let Err(e) = self.inner.secrets.set_private_key(&jwk).await {
                        tracing::warn!(error = %e, "cannot cache the E2E key");
                    }
                    self.check_after_secret_write(t).await?;
                    {
                        let mut acc = lock(&self.inner.account);
                        acc.public_key = Some(key.public_key());
                        acc.private_key = Some(Arc::new(key));
                        acc.server_public = Some(server_pub);
                        acc.server_envelope = Some(envelope);
                        acc.pending_passphrase = Some(pw);
                    }
                    self.became_ready(t, true).await;
                    return Ok(());
                }
            }
            // Keys were created or reset elsewhere: drop the stale local
            // secrets (only what was read and found not to match).
            if stale_key && let Err(e) = self.inner.secrets.delete_private_key().await {
                tracing::warn!(error = %e, "cannot delete the stale E2E key");
            }
            if pending.is_some()
                && let Err(e) = self.inner.secrets.delete_pending_passphrase().await
            {
                tracing::warn!(error = %e, "cannot delete the stale E2E passphrase");
            }
            self.check(t)?;
            {
                let mut acc = lock(&self.inner.account);
                acc.public_key = None;
                acc.private_key = None;
                acc.server_public = Some(server_pub);
                acc.server_envelope = Some(envelope);
                acc.pending_passphrase = None;
            }
            self.set_account_state(AccountE2eState::EnterPassword { failed: false });
            return Ok(());
        }

        // No keys on the server: reuse a cached pair or create one, upload it
        // with a fresh generated passphrase.
        let pair = match cached_key {
            Some(k) => UserKeyPair::from_private(k),
            None => blocking(UserKeyPair::generate).await??,
        };
        let private_jwk = pair.private.to_jwk_string()?;
        let public_jwk = pair.public.to_jwk_string();
        let passphrase = generate_passphrase();
        let envelope = {
            let (jwk, pw, uid) = (
                private_jwk.clone(),
                passphrase.clone(),
                self.inner.user_id.clone(),
            );
            blocking(move || encrypt_v2(&jwk, &pw, &uid)).await??
        };
        self.check(t)?;
        // A secret store failure is not fatal: the key and the passphrase stay in
        // memory for this session (the user is asked again next start).
        if let Err(e) = self.inner.secrets.set_private_key(&private_jwk).await {
            tracing::warn!(error = %e, "cannot cache the E2E key");
        }
        if let Err(e) = self.inner.secrets.set_pending_passphrase(&passphrase).await {
            tracing::warn!(error = %e, "cannot store the generated E2E passphrase");
        }
        self.check_after_secret_write(t).await?;
        match self
            .inner
            .api
            .set_user_keys(&public_jwk, &envelope, false)
            .await
        {
            Ok(()) => {}
            Err(e) if e.api_error_code() == Some("error-keys-already-set") => {
                // Another device uploaded keys meanwhile: unlock those instead.
                let _ = self.inner.secrets.clear().await;
                let theirs = self.inner.api.fetch_my_keys().await?;
                self.check(t)?;
                {
                    let mut acc = lock(&self.inner.account);
                    acc.server_public = theirs.public_key;
                    acc.server_envelope = theirs.private_key;
                    acc.pending_passphrase = None;
                }
                self.set_account_state(AccountE2eState::EnterPassword { failed: false });
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        self.check(t)?;
        if let Err(e) = self.inner.api.request_subscription_keys().await {
            tracing::debug!(error = %e, "e2e.requestSubscriptionKeys failed");
        }
        self.check(t)?;
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(pair.public.clone());
            acc.private_key = Some(Arc::new(pair.private));
            acc.server_public = Some(public_jwk);
            acc.server_envelope = Some(envelope);
            acc.pending_passphrase = Some(passphrase);
        }
        self.became_ready(t, true).await;
        Ok(())
    }

    async fn became_ready(&self, t: Token, save_password: bool) {
        if !self.alive(t) {
            return;
        }
        self.set_account_state(if save_password {
            AccountE2eState::SavePassword
        } else {
            AccountE2eState::Ready
        });
        self.handshake_all().await;
        if self.inner.config.auto_distribution && self.alive(t) {
            self.start_distribution();
        }
    }

    /// Decrypts the key backup `envelope` with `password`: `Ok(None)` when the
    /// password is wrong or the key does not match `server_public`.
    async fn unlock_envelope(
        &self,
        envelope: &str,
        server_public: Option<&UserPublicKey>,
        password: &str,
    ) -> Result<Option<(UserPrivateKey, Zeroizing<String>)>> {
        let (envelope, uid) = (envelope.to_owned(), self.inner.user_id.clone());
        let pw = Zeroizing::new(password.to_owned());
        let decrypted = blocking(move || -> Result<(UserPrivateKey, Zeroizing<String>)> {
            let env = PrivateKeyEnvelope::parse(&envelope, &uid)?;
            let jwk = env.decrypt(&pw)?;
            let key = UserPrivateKey::from_jwk_str(&jwk).map_err(|_| E2eError::WrongPassword)?;
            Ok((key, jwk))
        })
        .await?;
        match decrypted {
            Ok((key, jwk)) if server_public.is_none_or(|p| *p == key.public_key()) => {
                Ok(Some((key, jwk)))
            }
            Ok(_) | Err(E2eManagerError::Crypto(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Unlocks the key backup with the E2E passphrase (state `EnterPassword`).
    pub async fn submit_password(&self, password: &str) -> Result<()> {
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let (envelope, server_public) = {
            let acc = lock(&self.inner.account);
            if !matches!(acc.state, AccountE2eState::EnterPassword { .. }) {
                return Err(E2eManagerError::NotReady);
            }
            (acc.server_envelope.clone(), acc.server_public.clone())
        };
        let envelope = envelope.ok_or(E2eManagerError::NotReady)?;
        let uid = self.inner.user_id.clone();
        let pw = Zeroizing::new(password.to_owned());
        let decrypted = blocking(move || -> Result<(UserPrivateKey, Zeroizing<String>)> {
            let env = PrivateKeyEnvelope::parse(&envelope, &uid)?;
            let jwk = env.decrypt(&pw)?;
            // Garbage that happens to parse (CBC) still fails the import.
            let key = UserPrivateKey::from_jwk_str(&jwk).map_err(|_| E2eError::WrongPassword)?;
            Ok((key, jwk))
        })
        .await?;
        let (key, jwk) = match decrypted {
            Ok(v) => v,
            Err(E2eManagerError::Crypto(E2eError::WrongPassword)) => {
                self.check(t)?;
                self.set_account_state(AccountE2eState::EnterPassword { failed: true });
                return Err(E2eManagerError::WrongPassword);
            }
            Err(e) => return Err(e),
        };
        self.check(t)?;
        if let Some(public) = server_public.and_then(|p| UserPublicKey::from_jwk_str(&p).ok())
            && public != key.public_key()
        {
            tracing::warn!("decrypted E2E key does not match the server public key");
            self.set_account_state(AccountE2eState::EnterPassword { failed: true });
            return Err(E2eManagerError::WrongPassword);
        }
        // Unlocking works without the secret store (asked again next start).
        if let Err(e) = self.inner.secrets.set_private_key(&jwk).await {
            tracing::warn!(error = %e, "cannot cache the E2E key");
        }
        self.check_after_secret_write(t).await?;
        {
            let mut acc = lock(&self.inner.account);
            acc.public_key = Some(key.public_key());
            acc.private_key = Some(Arc::new(key));
            acc.pending_passphrase = None;
        }
        self.became_ready(t, false).await;
        Ok(())
    }

    /// "I saved my password": forgets the generated passphrase.
    pub async fn confirm_passphrase_saved(&self) -> Result<()> {
        self.inner.secrets.delete_pending_passphrase().await?;
        lock(&self.inner.account).pending_passphrase = None;
        if self.account_state() == AccountE2eState::SavePassword {
            self.set_account_state(AccountE2eState::Ready);
        }
        Ok(())
    }

    /// Re-encrypts the key backup with a new passphrase (web policy enforced) and
    /// uploads it with `force: true`.
    pub async fn change_passphrase(&self, new_passphrase: &str) -> Result<()> {
        validate_new_passphrase(new_passphrase)?;
        let _op = self.inner.account_op.lock().await;
        let t = self.token();
        let (private, public) = {
            let acc = lock(&self.inner.account);
            if !acc.state.is_ready() {
                return Err(E2eManagerError::NotReady);
            }
            (acc.private_key.clone(), acc.public_key.clone())
        };
        let (private, public) = private.zip(public).ok_or(E2eManagerError::NotReady)?;
        let jwk = private.to_jwk_string()?;
        let (pw, uid) = (
            Zeroizing::new(new_passphrase.to_owned()),
            self.inner.user_id.clone(),
        );
        let envelope = blocking(move || encrypt_v2(&jwk, &pw, &uid)).await??;
        self.check(t)?;
        self.inner
            .api
            .set_user_keys(&public.to_jwk_string(), &envelope, true)
            .await?;
        self.check(t)?;
        let _ = self.inner.secrets.delete_pending_passphrase().await;
        {
            let mut acc = lock(&self.inner.account);
            acc.server_envelope = Some(envelope);
            acc.pending_passphrase = None;
        }
        self.set_account_state(AccountE2eState::Ready);
        Ok(())
    }

    /// `users.resetE2EKey {}`: the server revokes all login tokens, so the caller
    /// must log out afterwards. Local E2E secrets are deleted on success. A
    /// [`rocketchat::Error::TwoFactorRequired`] asks for a code (often
    /// `method: password`).
    pub async fn reset_own_keys(&self, two_factor: Option<&TwoFactorCode>) -> Result<()> {
        self.inner.api.reset_own_key(two_factor).await?;
        self.logout().await;
        Ok(())
    }

    /// Stops the manager and forgets keys in memory (keeps the secret store).
    /// Operations still in flight drop their results.
    pub fn stop(&self) {
        self.inner.stops.fetch_add(1, Ordering::SeqCst);
        if let Some(h) = lock(&self.inner.distribution).take() {
            h.abort();
        }
        *lock(&self.inner.account) = Account::default();
        lock(&self.inner.rooms).clear();
        lock(&self.inner.key_requests).clear();
        write(&self.inner.summaries).clear();
        write(&self.inner.keys).clear();
        lock(&self.inner.cache).clear();
        self.emit(E2eEvent::AccountState(AccountE2eState::Disabled));
    }

    /// Explicit logout / key reset: stop and delete the E2E secrets of this
    /// account (the cached key and a pending generated passphrase).
    pub async fn logout(&self) {
        self.inner.logouts.fetch_add(1, Ordering::SeqCst);
        self.stop();
        if let Err(e) = self.inner.secrets.clear().await {
            tracing::warn!(error = %e, "cannot delete E2E secrets");
        }
    }

    /// A logout the user did not choose (`force_logout`): stop and delete the
    /// cached key, but keep a generated passphrase the user has not saved yet.
    /// It is the only copy; the next start unlocks the key backup with it and
    /// asks the user to save it again (web: the stored `e2e.randomPassword`
    /// survives logout).
    pub async fn forced_logout(&self) {
        self.inner.logouts.fetch_add(1, Ordering::SeqCst);
        self.stop();
        if let Err(e) = self.inner.secrets.delete_private_key().await {
            tracing::warn!(error = %e, "cannot delete the cached E2E key");
        }
    }

    // ----- settings and events ------------------------------------------------

    /// Replaces the settings. Turning `E2E_Enable` off keeps decrypting with the
    /// loaded keys but refuses to encrypt and pauses distribution.
    pub fn set_settings(&self, settings: E2eSettings) {
        *write(&self.inner.settings) = settings;
    }

    /// Applies a realtime event (rooms, subscriptions, settings, `force_logout`,
    /// room key requests). Returns `true` if it was relevant.
    pub async fn handle_event(&self, ev: &SessionEvent) -> bool {
        match ev {
            SessionEvent::RoomChanged { kind, room } => {
                if *kind == ChangeKind::Removed {
                    self.forget_room(&room.id);
                } else {
                    self.update_room(room).await;
                }
                true
            }
            SessionEvent::SubscriptionChanged { kind, sub } => {
                if *kind == ChangeKind::Removed {
                    self.forget_room(&sub.rid);
                } else {
                    self.update_subscription(sub).await;
                }
                true
            }
            SessionEvent::SettingChanged { setting, .. } => {
                write(&self.inner.settings).apply(&setting.id, &setting.value)
            }
            SessionEvent::ForceLogout { .. } => {
                self.forced_logout().await;
                true
            }
            other => match RoomE2eKeyRequest::from_event(other) {
                Some(req) => {
                    if !self.should_answer_key_request(&req.rid) {
                        return true;
                    }
                    if let Err(e) = self.distribute_rooms(vec![req.rid]).await {
                        tracing::debug!(error = %e, "key distribution after a request failed");
                    }
                    true
                }
                None => false,
            },
        }
    }

    /// Only rooms whose key this client holds, at most once per
    /// [`KEY_REQUEST_DEBOUNCE`] (the server emits a request on every join).
    fn should_answer_key_request(&self, rid: &str) -> bool {
        let ready = read(&self.inner.summaries)
            .get(rid)
            .is_some_and(|s| s.has_key && s.state == RoomE2eState::Ready);
        if !ready {
            return false;
        }
        let now = Instant::now();
        let mut last = lock(&self.inner.key_requests);
        if last
            .get(rid)
            .is_some_and(|t| now.duration_since(*t) < KEY_REQUEST_DEBOUNCE)
        {
            return false;
        }
        last.insert(rid.to_owned(), now);
        true
    }

    fn forget_room(&self, rid: &str) {
        lock(&self.inner.rooms).remove(rid);
        write(&self.inner.summaries).remove(rid);
        write(&self.inner.keys).remove(rid);
    }

    fn room_cell(&self, rid: &str) -> Arc<tokio::sync::Mutex<RoomCrypto>> {
        lock(&self.inner.rooms)
            .entry(rid.to_owned())
            .or_insert_with(|| {
                let id = self.inner.next_cell.fetch_add(1, Ordering::SeqCst) + 1;
                let rc = RoomCrypto {
                    cell_id: id,
                    ..Default::default()
                };
                (id, Arc::new(tokio::sync::Mutex::new(rc)))
            })
            .1
            .clone()
    }

    /// `rc` is still the manager's cell for `rid` (not dropped by a logout,
    /// `stop` or room removal while it was being worked on).
    fn attached(&self, rid: &str, rc: &RoomCrypto) -> bool {
        lock(&self.inner.rooms)
            .get(rid)
            .is_some_and(|(id, _)| *id == rc.cell_id)
    }

    /// Records rooms and subscriptions without handshaking (initial load before
    /// [`Self::start`]).
    pub async fn load(&self, rooms: &[Room], subscriptions: &[Subscription]) {
        for r in rooms {
            let cell = self.room_cell(&r.id);
            let mut rc = cell.lock().await;
            rc.apply_room(r);
            self.publish(&r.id, &rc);
        }
        for s in subscriptions {
            let cell = self.room_cell(&s.rid);
            let mut rc = cell.lock().await;
            rc.apply_subscription(s);
            self.publish(&s.rid, &rc);
        }
    }

    /// A room arrived or changed (`rooms.get`, `rooms-changed`).
    pub async fn update_room(&self, room: &Room) {
        let cell = self.room_cell(&room.id);
        let mut rc = cell.lock().await;
        let fx = rc.apply_room(room);
        if fx.reset {
            tracing::debug!(rid = %room.id, kid = ?rc.info.e2e_key_id, "room key reset detected");
        }
        self.handshake(&room.id, &mut rc).await;
    }

    /// A subscription arrived or changed (`subscriptions.get`,
    /// `subscriptions-changed`).
    pub async fn update_subscription(&self, sub: &Subscription) {
        let cell = self.room_cell(&sub.rid);
        let mut rc = cell.lock().await;
        rc.apply_subscription(sub);
        self.handshake(&sub.rid, &mut rc).await;
    }

    async fn handshake_all(&self) {
        let rids: Vec<String> = lock(&self.inner.rooms).keys().cloned().collect();
        for rid in rids {
            let cell = self.room_cell(&rid);
            let mut rc = cell.lock().await;
            self.handshake(&rid, &mut rc).await;
        }
    }

    /// Publishes the room's state and keys for the synchronous getters.
    fn publish(&self, rid: &str, rc: &RoomCrypto) {
        if !self.attached(rid, rc) {
            return;
        }
        let set = RoomKeySet {
            current: rc.current.clone(),
            old: rc
                .old_keys()
                .into_iter()
                .filter_map(|o| o.key.clone())
                .collect(),
        };
        let kids = |s: &RoomKeySet| -> Vec<String> {
            s.current
                .iter()
                .chain(s.old.iter())
                .map(|k| k.kid.clone())
                .collect()
        };
        let keys_changed = {
            let mut keys = write(&self.inner.keys);
            let before = keys.get(rid).map(|s| kids(s)).unwrap_or_default();
            let changed = before != kids(&set)
                || match (
                    keys.get(rid).and_then(|s| s.current.as_ref()),
                    set.current.as_ref(),
                ) {
                    (Some(a), Some(b)) => !a.same_key(b),
                    _ => false,
                };
            keys.insert(rid.to_owned(), Arc::new(set.clone()));
            changed
        };
        let state_changed = {
            let mut sums = write(&self.inner.summaries);
            let entry = sums.entry(rid.to_owned()).or_default();
            let changed = entry.state != rc.state;
            *entry = RoomSummary {
                state: rc.state.clone(),
                waiting: rc.info.waiting.clone(),
                has_key: set.current.is_some(),
                encrypted: rc.info.encrypted(),
            };
            changed
        };
        if state_changed {
            tracing::debug!(%rid, state = ?room_state_name(&rc.state), "e2e room state");
            self.emit(E2eEvent::RoomState {
                rid: rid.to_owned(),
                state: rc.state.clone(),
            });
        }
        if keys_changed && (set.current.is_some() || !set.old.is_empty()) {
            let pending = lock(&self.inner.cache).take_pending(rid);
            self.emit(E2eEvent::KeysChanged {
                rid: rid.to_owned(),
                pending,
            });
        }
    }

    fn set_room_state(&self, rid: &str, rc: &mut RoomCrypto, state: RoomE2eState) {
        rc.state = state;
        self.publish(rid, rc);
    }

    /// §7.2 handshake, re-run after every update. Acts only when the account
    /// has keys; creating, accepting and rejecting keys only when it is writable
    /// (E2E on, not read-only).
    async fn handshake(&self, rid: &str, rc: &mut RoomCrypto) {
        let Some(private) = self.private_key() else {
            self.publish(rid, rc);
            return;
        };
        if !self.attached(rid, rc) || (!rc.info.have_room && !rc.info.have_sub) {
            return;
        }
        if !rc.info.encryptable_type() {
            self.set_room_state(rid, rc, RoomE2eState::Disabled);
            return;
        }
        self.refresh_old_keys(rid, rc, &private).await;
        self.load_own_key(rid, rc, &private).await;
        if !self.attached(rid, rc) {
            return;
        }
        let writable = self.writable();

        if writable
            && let Some(suggested) = rc.info.sub_suggested.clone()
            && rc.rejected_suggested.as_ref() != Some(&suggested)
        {
            self.process_suggested(rid, rc, &private, suggested).await;
        }

        if let Some((pending, _)) = &rc.pending_created
            && (rc.current.is_some() || rc.info.e2e_key_id.as_ref() != Some(&pending.kid))
        {
            rc.pending_created = None;
        }
        if writable
            && rc.info.encrypted()
            && let Some((key, wrapped)) = rc.pending_created.take()
        {
            self.store_created_key(rid, rc, key, wrapped).await;
            return;
        }

        let next = if !rc.info.encrypted() {
            // Keys stay loaded for decrypting the history; nothing is encrypted.
            RoomE2eState::Disabled
        } else if rc.current.is_some() {
            RoomE2eState::Ready
        } else if writable && rc.info.have_room && rc.info.e2e_key_id.is_none() {
            self.create_room_key(rid, rc).await;
            return;
        } else {
            RoomE2eState::WaitingKeys
        };
        self.set_room_state(rid, rc, next);
    }

    /// Unwraps the subscription's `E2EKey` into the current key. Its kid is the
    /// wrapped prefix (the room's `e2eKeyId` for legacy keys without one, else a
    /// random id like the web). A prefix that differs from the room's kid is a
    /// reset whose subscription update came first: the key is kept as an old key
    /// until the room update arrives.
    async fn load_own_key(&self, rid: &str, rc: &mut RoomCrypto, private: &Arc<UserPrivateKey>) {
        if rc.current.is_some() {
            return;
        }
        let Some(wrapped) = rc.info.sub_e2e_key.clone() else {
            return;
        };
        if rc.stale_e2e_key.as_ref() == Some(&wrapped)
            || rc.failed_e2e_key.as_ref() == Some(&wrapped)
        {
            return;
        }
        let prefix = prefix_kid(&wrapped).unwrap_or_default();
        let room_kid = rc.info.e2e_key_id.clone();
        let mismatch = !prefix.is_empty() && room_kid.as_ref().is_some_and(|k| *k != prefix);
        if mismatch && rc.deferred_e2e_key.as_ref() == Some(&wrapped) {
            return;
        }
        if rc.state != RoomE2eState::Ready {
            self.set_room_state(rid, rc, RoomE2eState::Establishing);
        }
        let (p, w) = (private.clone(), wrapped.clone());
        match blocking(move || RoomKey::unwrap(&p, &w)).await {
            Ok(Ok(key)) if mismatch => {
                tracing::debug!(%rid, kid = %prefix, "own room key does not match the room key id yet");
                rc.push_local_old(key);
                rc.deferred_e2e_key = Some(wrapped);
            }
            Ok(Ok(key)) => {
                let kid = if prefix.is_empty() {
                    room_kid.unwrap_or_else(new_key_id)
                } else {
                    prefix
                };
                rc.current = Some(key.with_kid(kid));
                rc.current_source = Some(wrapped);
                rc.deferred_e2e_key = None;
            }
            Ok(Err(e)) => {
                tracing::warn!(%rid, error = %e, "cannot decrypt the own room key; waiting for a new one");
                rc.failed_e2e_key = Some(wrapped);
            }
            Err(e) => tracing::warn!(%rid, error = %e, "room key task failed"),
        }
    }

    /// Re-derives old keys when `oldRoomKeys` changed.
    async fn refresh_old_keys(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        private: &Arc<UserPrivateKey>,
    ) {
        let fp = rc.old_fingerprint();
        if fp == rc.server_old_fingerprint {
            return;
        }
        let entries: Vec<OldRoomKey> = rc.info.old_room_keys.clone().unwrap_or_default();
        let p = private.clone();
        let derived = blocking(move || {
            entries
                .into_iter()
                .map(|o| OldKey {
                    key: RoomKey::unwrap(&p, &o.e2e_key)
                        .ok()
                        .map(|k| k.with_kid(o.e2e_key_id.clone())),
                    kid: o.e2e_key_id,
                    ts: o.ts,
                })
                .collect::<Vec<_>>()
        })
        .await;
        match derived {
            Ok(old) => {
                let failed = old.iter().filter(|o| o.key.is_none()).count();
                if failed > 0 {
                    tracing::debug!(%rid, failed, "some old room keys cannot be decrypted");
                }
                rc.server_old = old;
                rc.server_old_fingerprint = fp;
            }
            Err(e) => tracing::warn!(%rid, error = %e, "old room key task failed"),
        }
    }

    /// Accepts a suggested key that decrypts and matches the room's key id,
    /// otherwise rejects it (§7.2, §11.1, §11.2).
    async fn process_suggested(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        private: &Arc<UserPrivateKey>,
        suggested: String,
    ) {
        let (p, s) = (private.clone(), suggested.clone());
        let unwrapped = match blocking(move || RoomKey::unwrap(&p, &s)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(%rid, error = %e, "suggested key task failed");
                return;
            }
        };
        let accept = match &unwrapped {
            Err(_) => {
                tracing::debug!(%rid, "suggested room key does not decrypt");
                None
            }
            Ok(key) => {
                if rc.info.e2e_key_id.as_ref().is_some_and(|k| *k != key.kid) {
                    tracing::debug!(%rid, kid = %key.kid, "suggested room key has a stale kid");
                    None
                } else if rc
                    .current
                    .as_ref()
                    .is_some_and(|c| c.kid == key.kid && !c.same_key(key))
                {
                    tracing::warn!(%rid, kid = %key.kid, "suggested room key differs from the working key");
                    None
                } else {
                    Some(key.clone())
                }
            }
        };
        if !self.attached(rid, rc) {
            return;
        }
        match accept {
            Some(key) => match self.inner.api.accept_suggested_group_key(rid).await {
                Ok(()) => {
                    tracing::debug!(%rid, kid = %key.kid, "accepted suggested room key");
                    rc.current = Some(key);
                    rc.current_source = Some(suggested.clone());
                    rc.info.sub_e2e_key = Some(suggested);
                    rc.info.sub_suggested = None;
                    rc.stale_e2e_key = None;
                    if rc.info.old_room_keys.is_none() && rc.info.suggested_old.is_some() {
                        rc.info.old_room_keys = rc.info.suggested_old.take();
                        self.refresh_old_keys(rid, rc, private).await;
                    }
                    if rc.state == RoomE2eState::WaitingKeys {
                        self.set_room_state(rid, rc, RoomE2eState::KeysReceived);
                    }
                }
                Err(e) if e.api_error_code() == Some("error-no-suggested-key-available") => {
                    rc.info.sub_suggested = None;
                }
                Err(e) => tracing::debug!(%rid, error = %e, "accepting the suggested key failed"),
            },
            None => {
                rc.rejected_suggested = Some(suggested);
                rc.info.sub_suggested = None;
                if let Err(e) = self.inner.api.reject_suggested_group_key(rid).await {
                    tracing::debug!(%rid, error = %e, "rejecting the suggested key failed");
                }
            }
        }
    }

    /// First room key (§7.3): claim the kid, then publish wrapped keys.
    async fn create_room_key(&self, rid: &str, rc: &mut RoomCrypto) {
        let Some(public) = self.public_key() else {
            return;
        };
        self.set_room_state(rid, rc, RoomE2eState::CreatingKeys);
        let key = RoomKey::generate();
        let wrapped = match key.wrap_for(&public) {
            Ok(w) => w,
            Err(e) => {
                self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
                return;
            }
        };
        if !self.attached(rid, rc) {
            return;
        }
        match self.inner.api.set_room_key_id(rid, &key.kid).await {
            Ok(SetKeyIdOutcome::Set) => {}
            Ok(SetKeyIdOutcome::AlreadyExists) => {
                tracing::debug!(%rid, "another member created the room key");
                self.set_room_state(rid, rc, RoomE2eState::WaitingKeys);
                return;
            }
            Err(e) => {
                self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
                return;
            }
        }
        rc.info.e2e_key_id = Some(key.kid.clone());
        self.store_created_key(rid, rc, key, wrapped).await;
    }

    /// Stores a created key in the own subscription, then makes it current and
    /// provides it to the other members. Until the own copy is stored the room
    /// stays in `Error` and nothing is encrypted (a key only in RAM would make
    /// every message unreadable after a restart); the next handshake retries.
    async fn store_created_key(
        &self,
        rid: &str,
        rc: &mut RoomCrypto,
        key: RoomKey,
        wrapped: String,
    ) {
        if !self.attached(rid, rc) {
            return;
        }
        let me = self.inner.user_id.clone();
        if let Err(e) = self.inner.api.update_group_key(rid, &me, &wrapped).await {
            tracing::warn!(%rid, error = %e, "storing the own room key failed");
            rc.pending_created = Some((key, wrapped));
            self.set_room_state(rid, rc, RoomE2eState::Error(e.to_string()));
            return;
        }
        rc.pending_created = None;
        rc.current_source = Some(wrapped);
        rc.current = Some(key.clone());
        self.set_room_state(rid, rc, RoomE2eState::Ready);
        if !self.attached(rid, rc) {
            return;
        }
        match self.inner.api.users_of_room_without_key(rid).await {
            Ok(users) => {
                let users: Vec<_> = users.into_iter().filter(|u| u.user_id != me).collect();
                let entries = wrap_for_users(key, Vec::new(), users).await;
                if !entries.is_empty() && self.attached(rid, rc) {
                    let map = HashMap::from([(rid.to_owned(), entries)]);
                    if let Err(e) = self
                        .inner
                        .api
                        .provide_users_suggested_group_keys(&map)
                        .await
                    {
                        tracing::debug!(%rid, error = %e, "providing room keys failed");
                    }
                }
            }
            Err(e) => tracing::debug!(%rid, error = %e, "listing members without key failed"),
        }
    }

    // ----- room operations ------------------------------------------------------

    /// `rooms.saveRoomSettings {encrypted}`.
    pub async fn toggle_room(&self, rid: &str, encrypted: bool) -> Result<()> {
        Ok(self.inner.api.set_room_encrypted(rid, encrypted).await?)
    }

    /// Resets the room key (§7.5): the server call first, then the local switch;
    /// the previous key stays usable as an old key.
    pub async fn reset_room_key(&self, rid: &str) -> Result<()> {
        if !self.settings().enabled {
            return Err(E2eManagerError::Disabled);
        }
        let public = self.public_key().ok_or(E2eManagerError::NotReady)?;
        if !self.account_state().is_ready() {
            return Err(E2eManagerError::NotReady);
        }
        let cell = self.room_cell(rid);
        let mut rc = cell.lock().await;
        if rc.info.e2e_key_id.is_none() {
            return Err(E2eManagerError::NoRoomKeyId);
        }
        let key = RoomKey::generate();
        let wrapped = key.wrap_for(&public)?;
        self.inner
            .api
            .reset_room_key(rid, &wrapped, &key.kid)
            .await?;
        if let Some(old) = rc.current.take() {
            let old_kid = rc
                .info
                .e2e_key_id
                .clone()
                .unwrap_or_else(|| old.kid.clone());
            rc.push_local_old(old.with_kid(old_kid));
        }
        rc.info.e2e_key_id = Some(key.kid.clone());
        rc.info.sub_e2e_key = Some(wrapped.clone());
        rc.current = Some(key);
        rc.current_source = Some(wrapped);
        rc.stale_e2e_key = None;
        rc.deferred_e2e_key = None;
        rc.pending_created = None;
        self.set_room_state(rid, &mut rc, RoomE2eState::Ready);
        Ok(())
    }

    // ----- encryption -----------------------------------------------------------

    fn current_key(&self, rid: &str) -> Result<RoomKey> {
        if !self.settings().enabled {
            return Err(E2eManagerError::Disabled);
        }
        if !self.account_state().is_ready() {
            return Err(E2eManagerError::NotReady);
        }
        if read(&self.inner.summaries)
            .get(rid)
            .is_some_and(|s| !s.encrypted)
        {
            return Err(E2eManagerError::RoomNotEncrypted);
        }
        read(&self.inner.keys)
            .get(rid)
            .and_then(|k| k.current.clone())
            .ok_or(E2eManagerError::RoomNotReady)
    }

    /// Encrypts any payload with the room's current key.
    pub fn encrypt_payload(&self, rid: &str, payload: &DecryptedMessage) -> Result<Value> {
        let key = self.current_key(rid)?;
        Ok(key.encrypt_json(&payload.to_value())?.to_json())
    }

    /// Encrypts any JSON object with the room's current key (e.g. edited
    /// upload metadata for `fileContent`). Top-level keys must not start with
    /// `$` (§11.15).
    pub fn encrypt_value(&self, rid: &str, value: &Value) -> Result<Value> {
        let key = self.current_key(rid)?;
        Ok(key.encrypt_json(value)?.to_json())
    }

    /// Encrypts a text message: `{msg}` + plaintext mentions (when enabled).
    /// Slash commands must be handled by the caller before.
    pub fn encrypt_outgoing(&self, rid: &str, text: &str) -> Result<OutgoingEncrypted> {
        self.encrypt_edit(rid, text, None)
    }

    /// Re-encrypts an edited message with the current key: the original
    /// decrypted `attachments`/`files`/`file` are kept, `msg` replaced.
    pub fn encrypt_edit(
        &self,
        rid: &str,
        text: &str,
        original: Option<&DecryptedMessage>,
    ) -> Result<OutgoingEncrypted> {
        let mut payload = original.cloned().unwrap_or_default();
        payload.msg = Some(text.to_owned());
        let content = self.encrypt_payload(rid, &payload)?;
        let settings = self.settings();
        let mentions = settings
            .mentions
            .then(|| e2e_mentions(text, &settings.username_pattern));
        Ok(OutgoingEncrypted { content, mentions })
    }

    /// Encrypts a file for upload (§7.8 steps 2–4). **Blocking**: hashes and
    /// encrypts the whole file on the calling thread (and copies it); prefer
    /// [`Self::prepare_upload_async`] outside tests.
    pub fn prepare_upload(
        &self,
        rid: &str,
        name: &str,
        mime: &str,
        plaintext: &[u8],
    ) -> Result<PreparedUpload> {
        let room_key = self.current_key(rid)?;
        encrypt_file(&room_key, name, mime, plaintext.to_vec())
    }

    /// [`Self::prepare_upload`] on a blocking thread, encrypting `data` in place.
    pub async fn prepare_upload_async(
        &self,
        rid: &str,
        name: String,
        mime: String,
        data: Vec<u8>,
    ) -> Result<PreparedUpload> {
        let room_key = self.current_key(rid)?;
        blocking(move || encrypt_file(&room_key, &name, &mime, data)).await?
    }

    /// The `content` of `rooms.mediaConfirm` for an uploaded encrypted file
    /// (`{attachments, files, file, msg}`, web shape).
    pub fn upload_message_content(
        &self,
        rid: &str,
        upload: &PreparedUpload,
        uploaded: &UploadedFile,
        caption: &str,
        image_dimensions: Option<(u32, u32)>,
    ) -> Result<Value> {
        let payload = upload_payload(upload, uploaded, caption, image_dimensions);
        self.encrypt_payload(rid, &payload)
    }

    // ----- decryption -----------------------------------------------------------

    /// Decrypts an `e2e` message (cached by id + `_updatedAt`). The server's `e2e`
    /// and `md` fields are ignored; decrypted URLs are sanitized when
    /// [`E2eConfig::server_url`] is set.
    pub fn decrypt_message(&self, msg: &Message) -> DecryptResult {
        if msg.t.as_deref() != Some("e2e") {
            return DecryptResult::NotEncrypted;
        }
        let updated = msg.updated_at.as_ref().map(RcDate::timestamp_millis);
        if let Some(hit) = lock(&self.inner.cache).get(&msg.rid, &msg.id, updated) {
            return DecryptResult::Decrypted(hit);
        }
        let content = match msg.encrypted_content() {
            None => {
                return DecryptResult::Decrypted(Arc::new(DecryptedContent {
                    message: DecryptedMessage::default(),
                    kid: None,
                    plaintext_edit: false,
                }));
            }
            Some(Err(_)) => return DecryptResult::Failed(DecryptFailure::Malformed),
            Some(Ok(c)) => c,
        };
        // Legacy v1 senders put the same ciphertext into `msg` as well.
        let plaintext_edit = !msg.msg.is_empty()
            && msg
                .content
                .as_ref()
                .and_then(|c| c.get("ciphertext"))
                .and_then(Value::as_str)
                != Some(msg.msg.as_str());
        let mut result = self.decrypt_envelope(&msg.rid, &content, plaintext_edit);
        if let DecryptResult::Decrypted(d) = &result
            && let Some(server) = msg.attachments.as_deref()
            && server
                .iter()
                .any(|a| a.kind.as_deref() == Some(REMOVED_FILE))
        {
            let mut c = (**d).clone();
            substitute_removed_files(&mut c.message, server);
            result = DecryptResult::Decrypted(Arc::new(c));
        }
        match &result {
            DecryptResult::Decrypted(d) => {
                lock(&self.inner.cache).insert(&msg.rid, &msg.id, updated, d.clone())
            }
            DecryptResult::Pending => lock(&self.inner.cache).mark_pending(&msg.rid, &msg.id),
            _ => {}
        }
        result
    }

    /// Decrypts a `content` object of room `rid` (pinned message attachments,
    /// notification payloads, …).
    pub fn decrypt_content(&self, rid: &str, content: &Value) -> DecryptResult {
        match EncryptedContent::from_value(content) {
            Ok(c) => self.decrypt_envelope(rid, &c, false),
            Err(_) => DecryptResult::Failed(DecryptFailure::Malformed),
        }
    }

    /// `message_pinned_e2e`: decrypts `attachments[0].content`.
    pub fn decrypt_pinned(&self, msg: &Message) -> DecryptResult {
        if msg.t.as_deref() != Some("message_pinned_e2e") {
            return DecryptResult::NotEncrypted;
        }
        let content = msg
            .attachments
            .as_ref()
            .and_then(|a| a.first())
            .and_then(|a| a.content.clone());
        match content {
            Some(c) => self.decrypt_content(&msg.rid, &c),
            None => DecryptResult::Failed(DecryptFailure::Malformed),
        }
    }

    /// Text for a desktop notification of an encrypted message
    /// (`payload.message.content`); `None` if it cannot be decrypted.
    pub fn decrypt_notification(&self, rid: &str, payload_message: &Value) -> Option<String> {
        let content = payload_message.get("content")?;
        match self.decrypt_content(rid, content) {
            DecryptResult::Decrypted(d) => Some(d.message.text().to_owned()),
            _ => None,
        }
    }

    /// Decrypts upload metadata (`rooms.files` entries' `content`).
    pub fn decrypt_file_metadata(
        &self,
        rid: &str,
        content: &Value,
    ) -> Result<FileMetadata, DecryptFailure> {
        let env = EncryptedContent::from_value(content).map_err(|_| DecryptFailure::Malformed)?;
        let keys = read(&self.inner.keys)
            .get(rid)
            .cloned()
            .ok_or(DecryptFailure::NoKey)?;
        let key = select_key(&keys, &env).ok_or(DecryptFailure::NoKey)?;
        let v = key
            .decrypt_json(&env)
            .map_err(|_| DecryptFailure::KeyError)?;
        serde_json::from_value(v).map_err(|_| DecryptFailure::KeyError)
    }

    fn decrypt_envelope(
        &self,
        rid: &str,
        content: &EncryptedContent,
        plaintext_edit: bool,
    ) -> DecryptResult {
        if let EncryptedContent::Unsupported { algorithm } = content {
            return DecryptResult::Failed(DecryptFailure::Unsupported(algorithm.clone()));
        }
        let keys = read(&self.inner.keys).get(rid).cloned();
        let Some(keys) = keys.filter(|k| k.current.is_some() || !k.old.is_empty()) else {
            return DecryptResult::Pending;
        };
        let kid = content.kid().map(str::to_owned);
        let exact = kid.as_deref().and_then(|k| keys.by_kid(k));
        let key = match (exact, &keys.current) {
            (Some(k), _) => k,
            (None, Some(cur)) => cur,
            (None, None) => {
                return if self.room_state(rid) == RoomE2eState::Ready {
                    DecryptResult::Failed(DecryptFailure::NoKey)
                } else {
                    DecryptResult::Pending
                };
            }
        };
        let parsed = key
            .decrypt_json(content)
            .ok()
            .and_then(|v| DecryptedMessage::from_value(&v).ok());
        match parsed {
            Some(mut message) => {
                if let Some(server) = &self.inner.config.server_url {
                    message.sanitize_urls(server);
                }
                DecryptResult::Decrypted(Arc::new(DecryptedContent {
                    message,
                    kid,
                    plaintext_edit,
                }))
            }
            None if exact.is_some() => DecryptResult::Failed(DecryptFailure::KeyError),
            None => DecryptResult::Failed(DecryptFailure::NoKey),
        }
    }

    // ----- distribution -----------------------------------------------------------

    fn start_distribution(&self) {
        let mut slot = lock(&self.inner.distribution);
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let period = self.inner.config.distribution_interval;
        *slot = Some(tokio::spawn(distribution::run_loop(weak, period)));
    }

    /// One distribution round over up to 10 random candidate rooms (§7.4).
    /// Returns the number of keys provided.
    pub async fn run_distribution_once(&self) -> Result<usize> {
        if !self.writable() || self.private_key().is_none() {
            return Ok(0);
        }
        let me = &self.inner.user_id;
        let mut candidates: Vec<String> = read(&self.inner.summaries)
            .iter()
            .filter(|(_, s)| {
                s.has_key
                    && s.state == RoomE2eState::Ready
                    && s.waiting.as_ref().is_some_and(|w| !w.contains(me))
            })
            .map(|(rid, _)| rid.clone())
            .collect();
        distribution::sample(&mut candidates, distribution::ROOM_KEY_EXCHANGE_SIZE);
        self.distribute_rooms(candidates).await
    }

    /// Provides keys to the waiting members of `rids` (rooms without a loaded key
    /// are skipped).
    pub async fn distribute_rooms(&self, rids: Vec<String>) -> Result<usize> {
        if rids.is_empty() || !self.writable() || self.private_key().is_none() {
            return Ok(0);
        }
        let t = self.token();
        let waiting = self.inner.api.users_waiting_for_group_key(&rids).await?;
        self.check(t)?;
        let mut map: HashMap<String, Vec<SuggestedKey>> = HashMap::new();
        for (rid, users) in waiting {
            let users: Vec<_> = users
                .into_iter()
                .filter(|u| u.user_id != self.inner.user_id)
                .collect();
            if users.is_empty() {
                continue;
            }
            let (current, old) = {
                let cell = self.room_cell(&rid);
                let rc = cell.lock().await;
                let Some(cur) = rc
                    .current
                    .clone()
                    .filter(|_| rc.state == RoomE2eState::Ready && self.attached(&rid, &rc))
                else {
                    continue;
                };
                let old: Vec<(RoomKey, String)> = rc
                    .old_keys()
                    .into_iter()
                    .filter_map(|o| {
                        let ts = o.ts.unwrap_or_else(RcDate::now).to_iso();
                        o.key.clone().map(|k| (k, ts))
                    })
                    .collect();
                (cur, old)
            };
            let entries = wrap_for_users(current, old, users).await;
            if !entries.is_empty() {
                map.insert(rid, entries);
            }
        }
        let count = map.values().map(Vec::len).sum();
        if count > 0 {
            self.check(t)?;
            self.inner
                .api
                .provide_users_suggested_group_keys(&map)
                .await?;
            tracing::debug!(rooms = map.len(), count, "provided room keys");
        }
        Ok(count)
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

/// Exact kid match (current, then old), else the current key.
fn select_key<'a>(keys: &'a RoomKeySet, env: &EncryptedContent) -> Option<&'a RoomKey> {
    env.kid()
        .and_then(|k| keys.by_kid(k))
        .or(keys.current.as_ref())
}

/// `type` of the placeholder the server puts in place of a deleted file.
const REMOVED_FILE: &str = "removed-file";

/// Deleted files (§7.7): the server replaces file attachments with
/// `removed-file` placeholders but keeps the encrypted content, so the decrypted
/// file attachments are swapped for the placeholders (web
/// `decryptMessageContent`) and the deleted files dropped from `files`/`file`.
fn substitute_removed_files(message: &mut DecryptedMessage, server: &[MessageAttachment]) {
    let removed: Vec<&MessageAttachment> = server
        .iter()
        .filter(|a| a.kind.as_deref() == Some(REMOVED_FILE))
        .collect();
    let all = removed.iter().find(|a| a.file_id.is_none()).copied();
    let removed_ids: HashSet<&str> = removed
        .iter()
        .filter_map(|a| a.file_id.as_deref())
        .collect();
    let main_file = message.file.as_ref().map(|f| f.id.clone());
    if let Some(atts) = &mut message.attachments {
        for att in atts.iter_mut() {
            if att.kind.as_deref() != Some("file") {
                continue;
            }
            if let Some(placeholder) = all {
                *att = placeholder.clone();
                continue;
            }
            let Some(fid) = att.file_id.clone().or_else(|| main_file.clone()) else {
                continue;
            };
            if let Some(placeholder) = removed.iter().find(|r| r.file_id.as_deref() == Some(&fid)) {
                *att = (*placeholder).clone();
            }
        }
    }
    let gone = |id: &str| all.is_some() || removed_ids.contains(id);
    if let Some(files) = &mut message.files {
        files.retain(|f| !gone(&f.id));
    }
    if message.file.as_ref().is_some_and(|f| gone(&f.id)) {
        message.file = message.files.as_ref().and_then(|f| f.first().cloned());
    }
}

/// Hashes and encrypts a file in place (blocking).
fn encrypt_file(
    room_key: &RoomKey,
    name: &str,
    mime: &str,
    mut data: Vec<u8>,
) -> Result<PreparedUpload> {
    let key = FileKey::generate();
    let mut hasher = Sha256Hasher::new();
    let mut cipher = key.cipher();
    for chunk in data.chunks_mut(FILE_CHUNK) {
        hasher.update(chunk);
        cipher.apply(chunk);
    }
    let size = data.len() as u64;
    let metadata = FileMetadata::new(name, mime, key.to_attachment(), hasher.finish_hex());
    let file_content = room_key
        .encrypt_json(&serde_json::to_value(&metadata).map_err(E2eError::from)?)?
        .to_json();
    Ok(PreparedUpload {
        hashed_name: hashed_upload_name(name),
        size,
        encrypted: data.into(),
        key,
        metadata,
        file_content,
    })
}

/// Chunk size of in-memory file encryption/decryption.
const FILE_CHUNK: usize = 64 * 1024;

/// The decrypted payload of an upload message (web shape, §7.8 step 5).
pub fn upload_payload(
    upload: &PreparedUpload,
    uploaded: &UploadedFile,
    caption: &str,
    image_dimensions: Option<(u32, u32)>,
) -> DecryptedMessage {
    let meta = &upload.metadata;
    let mime = meta.type_.as_str();
    let format = std::path::Path::new(&meta.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_uppercase)
        .unwrap_or_else(|| "file".to_owned());
    let mut att = MessageAttachment {
        title: Some(meta.name.clone()),
        kind: Some("file".into()),
        title_link: Some(uploaded.url.clone()),
        title_link_download: Some(true),
        file_id: Some(uploaded.id.clone()),
        encryption: serde_json::to_value(upload.key.to_attachment()).ok(),
        hashes: Some(
            serde_json::json!({ "sha256": meta.hashes.as_ref().map(|h| h.sha256.clone()).unwrap_or_default() }),
        ),
        ..Default::default()
    };
    match mime.split('/').next() {
        Some("image") => {
            att.image_url = Some(uploaded.url.clone());
            att.image_type = Some(mime.to_owned());
            att.image_size = Some(upload.size);
            att.image_dimensions = image_dimensions
                .map(|(width, height)| rocketchat::models::Dimensions { width, height });
        }
        Some("audio") => {
            att.audio_url = Some(uploaded.url.clone());
            att.audio_type = Some(mime.to_owned());
            att.audio_size = Some(upload.size);
        }
        Some("video") => {
            att.video_url = Some(uploaded.url.clone());
            att.video_type = Some(mime.to_owned());
            att.video_size = Some(upload.size);
        }
        _ => {
            att.size = Some(upload.size);
            att.format = Some(format.clone());
        }
    }
    let file = FileInfo {
        id: uploaded.id.clone(),
        name: meta.name.clone(),
        mime: Some(mime.to_owned()),
        size: Some(upload.size),
        format: Some(format),
        type_group: None,
    };
    DecryptedMessage {
        msg: Some(caption.to_owned()),
        attachments: Some(vec![att]),
        files: Some(vec![file.clone()]),
        file: Some(file),
    }
}

/// Decrypts a downloaded encrypted file and verifies `hashes.sha256` (when the
/// attachment has one). **Blocking** (whole file, copied); prefer
/// [`decrypt_download_async`] outside tests.
pub fn decrypt_download(attachment: &MessageAttachment, ciphertext: &[u8]) -> Result<Vec<u8>> {
    decrypt_download_owned(attachment, ciphertext.to_vec())
}

/// [`decrypt_download`] on a blocking thread, decrypting `ciphertext` in place.
pub async fn decrypt_download_async(
    attachment: MessageAttachment,
    ciphertext: Vec<u8>,
) -> Result<Vec<u8>> {
    blocking(move || decrypt_download_owned(&attachment, ciphertext)).await?
}

fn decrypt_download_owned(attachment: &MessageAttachment, mut data: Vec<u8>) -> Result<Vec<u8>> {
    let enc = attachment
        .e2e_encryption()
        .ok_or(E2eError::Format("attachment has no encryption parameters"))?;
    let key = FileKey::from_attachment(&enc)?;
    let mut cipher = key.cipher();
    let mut hasher = Sha256Hasher::new();
    for chunk in data.chunks_mut(FILE_CHUNK) {
        cipher.apply(chunk);
        hasher.update(chunk);
    }
    if let Some(expected) = attachment.e2e_sha256()
        && !hasher.finish_hex().eq_ignore_ascii_case(expected)
    {
        return Err(E2eError::Format("file hash mismatch").into());
    }
    Ok(data)
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
