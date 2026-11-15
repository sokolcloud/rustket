//! Public E2E types: states, events, settings, errors and results.

use std::sync::Arc;
use std::time::Duration;

use rocketchat::ServerUrl;
use rocketchat::e2e::{
    DEFAULT_USERNAME_PATTERN, DecryptedMessage, E2eError, E2eMentions, FileKey, FileMetadata,
};
use serde_json::Value;

use crate::secrets::SecretError;

use super::{RoomE2eState, cache};

/// Account E2E state (web `E2EEState`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AccountE2eState {
    /// Not started, stopped, or `E2E_Enable` is off at start without cached keys.
    #[default]
    Disabled,
    /// `E2E_Enable` is off but the cached private key is loaded: existing room
    /// keys decrypt, nothing is encrypted, created, accepted or distributed
    /// (§11.12). Call [`E2eManager::start`](super::E2eManager::start) again when `E2E_Enable` turns on.
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

/// Tuning of an [`E2eManager`](super::E2eManager).
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
    /// Upload file name: random hex, unrelated to the real name.
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
