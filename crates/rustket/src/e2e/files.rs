//! Encrypted files (§3.8, §7.8): decrypt-on-download with hash verification.
//!
//! When a message is decrypted, the URLs of its encrypted attachments are
//! rewritten to `<absolute url>#rustket-e2e` and the file key is recorded here
//! under the absolute URL. The image loader and the download path recognise
//! the marker, fetch the ciphertext, decrypt it (AES-256-CTR, 64-bit counter)
//! and check `hashes.sha256`. Decrypted bytes are never written to the image
//! disk cache; only the user's own saves and media opened with the system
//! player reach the disk.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rocketchat::ServerUrl;
use rocketchat::e2e::{FileCipher, FileKey, Sha256Hasher};
use rocketchat::models::MessageAttachment;

/// Appended to the URLs of encrypted files.
pub const FRAGMENT: &str = "#rustket-e2e";

/// Most files remembered (oldest dropped first).
const MAX_FILES: usize = 4096;

#[derive(Clone)]
struct Entry {
    attachment: MessageAttachment,
    seq: u64,
}

#[derive(Default)]
struct Registry {
    files: HashMap<String, Entry>,
    seq: u64,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The URL without the marker, if `url` is an encrypted file URL.
pub fn strip(url: &str) -> Option<&str> {
    url.strip_suffix(FRAGMENT)
}

/// The key material of an encrypted file URL (with the marker).
fn lookup(url: &str) -> Option<MessageAttachment> {
    let base = strip(url)?;
    lock().files.get(base).map(|e| e.attachment.clone())
}

/// Records the key of `a` (a decrypted attachment) and rewrites its file URLs
/// to absolute marked URLs. Attachments without encryption are left alone.
pub fn mark_attachment(a: &mut MessageAttachment, server: &ServerUrl) {
    if a.e2e_encryption().is_none() {
        return;
    }
    let keys = MessageAttachment {
        encryption: a.encryption.clone(),
        hashes: a.hashes.clone(),
        ..Default::default()
    };
    let mut reg = lock();
    for field in [
        &mut a.title_link,
        &mut a.image_url,
        &mut a.audio_url,
        &mut a.video_url,
    ] {
        let Some(u) = field.as_deref() else {
            continue;
        };
        if strip(u).is_some() {
            continue;
        }
        let abs = server.absolute(u);
        let abs = abs.split('#').next().unwrap_or_default().to_owned();
        reg.seq += 1;
        let seq = reg.seq;
        reg.files.insert(
            abs.clone(),
            Entry {
                attachment: keys.clone(),
                seq,
            },
        );
        *field = Some(format!("{abs}{FRAGMENT}"));
    }
    if reg.files.len() > MAX_FILES {
        let mut by_age: Vec<(u64, String)> =
            reg.files.iter().map(|(k, e)| (e.seq, k.clone())).collect();
        by_age.sort();
        let excess = reg.files.len() - MAX_FILES;
        for (_, k) in by_age.into_iter().take(excess) {
            reg.files.remove(&k);
        }
    }
}

/// Streaming decryption of a download (§3.8): feed the chunks in order, then
/// [`Decryptor::finish`] checks the plaintext hash.
pub struct Decryptor {
    cipher: FileCipher,
    hasher: Sha256Hasher,
    expected: Option<String>,
}

impl Decryptor {
    /// A decryptor for `url` if it is a known encrypted file URL.
    pub fn for_url(url: &str) -> Option<Self> {
        let a = lookup(url)?;
        let enc = a.e2e_encryption()?;
        let key = FileKey::from_attachment(&enc).ok()?;
        Some(Self {
            cipher: key.cipher(),
            hasher: Sha256Hasher::new(),
            expected: a.e2e_sha256().map(str::to_owned),
        })
    }

    pub fn apply(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = chunk.to_vec();
        self.cipher.apply(&mut out);
        self.hasher.update(&out);
        out
    }

    /// Whether the decrypted bytes match `hashes.sha256` (true when the
    /// sender gave no hash).
    pub fn finish(self) -> bool {
        match self.expected {
            Some(e) => self.hasher.finish_hex().eq_ignore_ascii_case(&e),
            None => true,
        }
    }
}

/// The text shown when a downloaded file does not match its hash.
pub fn hash_mismatch_text() -> String {
    rust_i18n::t!("e2e.file_hash_mismatch").into_owned()
}

/// Downloads and decrypts an encrypted image URL for the image loader:
/// `Some(result)` for a marked URL, `None` for any other URL. Nothing is
/// written to the disk cache.
pub async fn fetch_image(
    rest: &rocketchat::RestClient,
    url: &str,
    max_bytes: u64,
) -> Option<Result<Vec<u8>, String>> {
    use iced::futures::StreamExt;
    let base = strip(url)?;
    let Some(mut dec) = Decryptor::for_url(url) else {
        return Some(Err("unknown encrypted file".into()));
    };
    let r = async {
        let dl = rest.download(base).await.map_err(|e| e.to_string())?;
        if dl.content_length.is_some_and(|n| n > max_bytes) {
            return Err("image too large".to_owned());
        }
        let mut body = Vec::new();
        let mut stream = Box::pin(dl.stream());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            if body.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err("image too large".to_owned());
            }
            body.extend(dec.apply(&chunk));
        }
        if !dec.finish() {
            tracing::warn!("encrypted image does not match its hash");
            return Err(hash_mismatch_text());
        }
        Ok(body)
    }
    .await;
    Some(r)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::e2e::FileKey;

    fn encrypted_attachment(plain: &[u8]) -> (MessageAttachment, Vec<u8>) {
        let key = FileKey::generate();
        let mut data = plain.to_vec();
        key.apply(&mut data);
        let mut h = Sha256Hasher::new();
        h.update(plain);
        let a = MessageAttachment {
            title_link: Some("/file-upload/f1/abc".into()),
            image_url: Some("/file-upload/f1/abc".into()),
            encryption: serde_json::to_value(key.to_attachment()).ok(),
            hashes: Some(serde_json::json!({"sha256": h.finish_hex()})),
            ..Default::default()
        };
        (a, data)
    }

    #[test]
    fn marks_urls_and_decrypts_with_hash_check() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let (mut a, cipher) = encrypted_attachment(b"hello encrypted world");
        mark_attachment(&mut a, &server);
        let url = a.image_url.clone().unwrap();
        assert_eq!(
            url,
            "http://localhost:3000/file-upload/f1/abc#rustket-e2e".to_owned()
        );
        assert_eq!(a.title_link.as_deref(), Some(url.as_str()));
        // Marking twice keeps the URL.
        mark_attachment(&mut a, &server);
        assert_eq!(a.image_url.as_deref(), Some(url.as_str()));

        let mut d = Decryptor::for_url(&url).unwrap();
        let (x, y) = cipher.split_at(5);
        let mut out = d.apply(x);
        out.extend(d.apply(y));
        assert_eq!(out, b"hello encrypted world");
        assert!(d.finish());

        let mut d = Decryptor::for_url(&url).unwrap();
        let _ = d.apply(b"tampered bytes here!!");
        assert!(!d.finish());

        assert!(Decryptor::for_url("http://localhost:3000/file-upload/f1/abc").is_none());
        assert!(Decryptor::for_url("http://localhost:3000/other#rustket-e2e").is_none());
    }

    #[test]
    fn plain_attachments_are_untouched() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let mut a = MessageAttachment {
            image_url: Some("/file-upload/x/y.png".into()),
            ..Default::default()
        };
        mark_attachment(&mut a, &server);
        assert_eq!(a.image_url.as_deref(), Some("/file-upload/x/y.png"));
    }
}
