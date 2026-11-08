//! Rocket.Chat 8.9 end-to-end encryption primitives (see `docs/e2e.md`).
//!
//! Byte-compatible with the 8.9 web client and the React Native client: user RSA
//! key pairs as JWK strings, the private key backup ("keychain") envelopes,
//! room keys (A256GCM and legacy A128CBC), message content envelopes v1/v2,
//! AES-256-CTR file encryption and the plaintext `e2eMentions`.
//!
//! No state and no persistence here. Functions doing RSA key generation, RSA
//! decryption or PBKDF2 are synchronous and **blocking**: call them from a
//! blocking thread. Secrets are held in [`Zeroizing`] buffers and never logged.

mod codec;
mod events;
mod file;
mod keychain;
mod mentions;
mod passphrase;
mod payload;
mod room_key;
mod rsa_backend;
mod user_key;

pub use codec::{
    EncryptedContent, WrappedKey, b64_decode, b64_encode, b64url_decode, b64url_encode,
    latin1_bytes,
};
pub use events::{ROOM_KEY_REQUEST_EVENT, RoomE2eKeyRequest};
pub use file::{
    AttachmentEncryption, FileCipher, FileKey, OctJwk, Sha256Hasher, decrypt_stream,
    encrypt_stream, hashed_upload_name, sha256_hex,
};
pub use keychain::{
    PASSPHRASE_MIN_LENGTH, PrivateKeyEnvelope, encrypt_v2, encrypt_v2_with, validate_new_passphrase,
};
pub use mentions::{DEFAULT_USERNAME_PATTERN, E2eMentions, e2e_mentions};
pub use passphrase::{BIP39_ENGLISH, generate_passphrase, word_list};
pub use payload::{DecryptedMessage, FileHashes, FileMetadata};
pub use room_key::{RoomKey, RoomKeyAlg};
pub use user_key::{
    MAX_OAEP_PLAINTEXT, PrivateJwk, PublicJwk, UserKeyPair, UserPrivateKey, UserPublicKey,
};

pub use zeroize::{Zeroize, Zeroizing};

/// Size of an RSA-2048 block (wrapped room keys).
pub const RSA_BLOCK_LEN: usize = 256;

/// `content.algorithm` of v1 envelopes.
pub const ALGORITHM_V1: &str = "rc.v1.aes-sha2";
/// `content.algorithm` of v2 envelopes (written by Rustket).
pub const ALGORITHM_V2: &str = "rc.v2.aes-sha2";

/// Errors of the E2E primitives. Messages never contain key material or plaintext.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum E2eError {
    #[error("invalid base64")]
    Base64,
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("invalid format: {0}")]
    Format(&'static str),
    #[error("unsupported algorithm: {0}")]
    UnsupportedAlgorithm(String),
    /// The passphrase does not decrypt the private key backup.
    #[error("incorrect encryption password")]
    WrongPassword,
    /// Authenticated decryption or padding check failed (wrong key or corrupted data).
    #[error("decryption failed")]
    Decrypt,
    #[error("RSA error: {0}")]
    Rsa(String),
    /// A new passphrase contains characters other than printable ASCII.
    #[error("the passphrase may only contain printable ASCII characters")]
    PasswordCharset,
    /// A new passphrase does not satisfy the web passphrase policy.
    #[error("the passphrase does not satisfy the policy")]
    PasswordPolicy(PassphrasePolicyReport),
}

impl From<serde_json::Error> for E2eError {
    fn from(e: serde_json::Error) -> Self {
        // serde_json messages quote at most a short token of the input; drop the
        // detail anyway so no plaintext fragment ever ends up in an error.
        Self::Json(format!("{:?} at line {}", e.classify(), e.line()))
    }
}

/// Which rules of the web passphrase policy (`ChangePassphrase.tsx`) a passphrase
/// fails. All `false` means it passes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassphrasePolicyReport {
    pub too_short: bool,
    pub missing_lowercase: bool,
    pub missing_uppercase: bool,
    pub missing_digit: bool,
    pub missing_special: bool,
}

impl PassphrasePolicyReport {
    pub fn is_ok(&self) -> bool {
        *self == Self::default()
    }
}

pub type Result<T, E = E2eError> = std::result::Result<T, E>;

/// `N` bytes from the thread-local CSPRNG (ChaCha, reseeded from the OS).
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::Rng;
    let mut out = [0u8; N];
    rand::rng().fill_bytes(&mut out);
    out
}
