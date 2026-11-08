//! Encrypted file uploads: AES-256-CTR with a 64-bit big-endian counter
//! (WebCrypto `{name: 'AES-CTR', length: 64}`), SHA-256 of the plaintext and the
//! hashed upload name (§3.8).

use std::fmt;

use aes::cipher::{KeyIvInit, StreamCipher};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use zeroize::{Zeroize, Zeroizing};

use super::codec::{b64_decode, b64_encode, b64url_decode, b64url_encode};
use super::{E2eError, Result, random_bytes};

type Aes256Ctr64 = ctr::Ctr64BE<aes::Aes256>;

/// The JWK of a file key (`encryption.key`).
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OctJwk {
    #[serde(default)]
    pub alg: String,
    #[serde(default)]
    pub ext: bool,
    #[serde(default)]
    pub k: String,
    #[serde(default)]
    pub key_ops: Vec<String>,
    #[serde(default)]
    pub kty: String,
}

impl Drop for OctJwk {
    fn drop(&mut self) {
        self.k.zeroize();
    }
}

impl fmt::Debug for OctJwk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OctJwk")
            .field("alg", &self.alg)
            .finish_non_exhaustive()
    }
}

/// `encryption` of an encrypted attachment / upload metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentEncryption {
    pub key: OctJwk,
    /// Base64 (padded) initial counter block.
    pub iv: String,
}

impl AttachmentEncryption {
    pub fn new(key: &FileKey) -> Self {
        Self {
            key: OctJwk {
                alg: "A256CTR".into(),
                ext: true,
                k: b64url_encode(&*key.key),
                key_ops: vec!["encrypt".into(), "decrypt".into()],
                kty: "oct".into(),
            },
            iv: b64_encode(&key.iv),
        }
    }
}

/// A per-file AES-256-CTR key and initial counter block.
#[derive(Clone)]
pub struct FileKey {
    key: Zeroizing<[u8; 32]>,
    iv: [u8; 16],
}

impl fmt::Debug for FileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileKey").finish_non_exhaustive()
    }
}

impl FileKey {
    pub fn generate() -> Self {
        Self {
            key: Zeroizing::new(random_bytes::<32>()),
            iv: random_bytes::<16>(),
        }
    }

    pub fn from_parts(key: [u8; 32], iv: [u8; 16]) -> Self {
        Self {
            key: Zeroizing::new(key),
            iv,
        }
    }

    /// From a decrypted attachment's `encryption` (`alg` must be `A256CTR` when
    /// present).
    pub fn from_attachment(enc: &AttachmentEncryption) -> Result<Self> {
        if !enc.key.alg.is_empty() && enc.key.alg != "A256CTR" {
            return Err(E2eError::UnsupportedAlgorithm(enc.key.alg.clone()));
        }
        let raw = Zeroizing::new(b64url_decode(&enc.key.k)?);
        let key: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| E2eError::Format("file key is not 32 bytes"))?;
        let iv: [u8; 16] = b64_decode(&enc.iv)?
            .try_into()
            .map_err(|_| E2eError::Format("file iv is not 16 bytes"))?;
        Ok(Self::from_parts(key, iv))
    }

    pub fn to_attachment(&self) -> AttachmentEncryption {
        AttachmentEncryption::new(self)
    }

    /// A keystream positioned at the start of the file. Encryption and
    /// decryption are the same operation.
    pub fn cipher(&self) -> FileCipher {
        FileCipher(Aes256Ctr64::new((&*self.key).into(), (&self.iv).into()))
    }

    /// Encrypts or decrypts a whole buffer in place.
    pub fn apply(&self, data: &mut [u8]) {
        self.cipher().apply(data);
    }
}

/// Streaming AES-256-CTR state.
pub struct FileCipher(Aes256Ctr64);

impl FileCipher {
    pub fn apply(&mut self, data: &mut [u8]) {
        self.0.apply_keystream(data);
    }
}

/// Encrypts a byte stream chunk by chunk (any chunk sizes).
pub fn encrypt_stream<S, E>(key: &FileKey, stream: S) -> impl Stream<Item = Result<Bytes, E>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send,
    E: Send,
{
    let mut cipher = key.cipher();
    stream.map(move |chunk| {
        chunk.map(|b| {
            let mut v = b.to_vec();
            cipher.apply(&mut v);
            Bytes::from(v)
        })
    })
}

/// Decrypts a byte stream (CTR: same as [`encrypt_stream`]). Verify the result
/// with [`Sha256Hasher`] against `hashes.sha256`.
pub fn decrypt_stream<S, E>(key: &FileKey, stream: S) -> impl Stream<Item = Result<Bytes, E>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send,
    E: Send,
{
    encrypt_stream(key, stream)
}

/// Incremental SHA-256 producing lowercase hex.
#[derive(Clone, Default)]
pub struct Sha256Hasher(sha2::Sha256);

impl Sha256Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish_hex(self) -> String {
        hex::encode(self.0.finalize())
    }
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

/// The upload file name of an encrypted file: hex SHA-256 of the UTF-8 name.
pub fn hashed_upload_name(name: &str) -> String {
    sha256_hex(name.as_bytes())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const PLAIN: &[u8] = b"0123456789abcdef0123456789abcdef0123456789abcdeftail";
    const CT: &str = "232636a6270bb2add7899c6c17e5971a1241b249201d3c216446084c91cf37f42596457bd8510c2d120c730325276cc1ab3a7c56";

    fn vector_key() -> FileKey {
        let enc: AttachmentEncryption = serde_json::from_value(serde_json::json!({
            "key": {"alg":"A256CTR","ext":true,"k":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","key_ops":["encrypt","decrypt"],"kty":"oct"},
            "iv": "AAECAwQFBgf//////////g=="
        }))
        .unwrap();
        FileKey::from_attachment(&enc).unwrap()
    }

    #[test]
    fn ctr64_vector_with_low_half_wrap() {
        let key = vector_key();
        assert_eq!(hex::encode(key.iv), "0001020304050607fffffffffffffffe");
        assert_eq!(
            sha256_hex(PLAIN),
            "27ba3f32aa22e58169e21a5ccee9ef04a6a8aafb9e610897ebd1186c05f24fd4"
        );
        let mut buf = PLAIN.to_vec();
        key.apply(&mut buf);
        assert_eq!(hex::encode(&buf), CT);
        key.apply(&mut buf);
        assert_eq!(buf, PLAIN);

        // A 128-bit counter agrees on the first two blocks only.
        let mut c128 = ctr::Ctr128BE::<aes::Aes256>::new((&*key.key).into(), (&key.iv).into());
        let mut b128 = PLAIN.to_vec();
        c128.apply_keystream(&mut b128);
        assert_eq!(b128[..32], hex::decode(CT).unwrap()[..32]);
        assert_ne!(b128[32..48], hex::decode(CT).unwrap()[32..48]);
    }

    #[test]
    fn streaming_in_odd_chunks_matches() {
        let key = vector_key();
        let chunks: Vec<Result<Bytes, ()>> = PLAIN
            .chunks(7)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let out: Vec<Result<Bytes, ()>> = futures::executor::block_on(
            encrypt_stream(&key, futures::stream::iter(chunks)).collect(),
        );
        let joined: Vec<u8> = out.into_iter().flat_map(|r| r.unwrap().to_vec()).collect();
        assert_eq!(hex::encode(&joined), CT);
        let back: Vec<Result<Bytes, ()>> = futures::executor::block_on(
            decrypt_stream(&key, futures::stream::iter(vec![Ok(Bytes::from(joined))])).collect(),
        );
        let mut h = Sha256Hasher::new();
        for c in &back {
            h.update(c.as_ref().unwrap());
        }
        assert_eq!(h.finish_hex(), sha256_hex(PLAIN));
    }

    #[test]
    fn hashed_names() {
        assert_eq!(
            hashed_upload_name("notes.txt"),
            "e39538e7f27a7bf579cd9b85a103c0f0b86b60b788534295538d0301a9c5dce6"
        );
        assert_eq!(
            hashed_upload_name("Отчёт.pdf"),
            "2e2bc3fdfa1c462abcadcd3ca0ac2065699f7aa10076e3f9a71c485224c84e78"
        );
    }

    #[test]
    fn attachment_encryption_roundtrip_and_errors() {
        let k = FileKey::generate();
        let enc = k.to_attachment();
        assert_eq!(enc.key.alg, "A256CTR");
        assert!(!enc.key.k.contains('='));
        let back = FileKey::from_attachment(&enc).unwrap();
        assert_eq!(*back.key, *k.key);
        assert_eq!(back.iv, k.iv);
        let mut bad = enc.clone();
        bad.key.alg = "A128CBC".into();
        assert!(FileKey::from_attachment(&bad).is_err());
        let mut bad = enc.clone();
        bad.iv = "AAAA".into();
        assert!(FileKey::from_attachment(&bad).is_err());
        let mut bad = enc.clone();
        bad.key.k = "AAAA".into();
        assert!(FileKey::from_attachment(&bad).is_err());
        let mut bad = enc;
        bad.key.k = "!!".into();
        assert!(FileKey::from_attachment(&bad).is_err());
        assert!(!format!("{k:?}").contains(&b64url_encode(&*k.key)));
    }
}
