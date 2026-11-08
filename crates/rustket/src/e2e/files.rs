//! Encrypted files (§3.8, §7.8): decrypt-on-download with hash verification.
//!
//! When a message is decrypted, the URLs of its encrypted attachments are
//! rewritten to `<absolute url>#rustket-e2e-<token>` and the file key is
//! recorded here under the token. The token is derived from the server, the
//! room, the message and the attachment slot, so one message can never
//! replace the key another message registered for the same upload. The image
//! loader and the download path recognise the marker, fetch the ciphertext,
//! decrypt it (AES-256-CTR, 64-bit counter) and check `hashes.sha256`.
//! Decrypted bytes are never written to the image disk cache; only the
//! user's own saves and media opened with the system player reach the disk
//! (the latter in a private directory that is purged, see
//! `room::timeline::viewer`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rocketchat::ServerUrl;
use rocketchat::e2e::{FileCipher, FileKey, Sha256Hasher};
use rocketchat::models::MessageAttachment;
use rust_i18n::t;
use rustket_core::Workspace;

/// Starts the marker appended to the URLs of encrypted files.
pub const FRAGMENT: &str = "#rustket-e2e-";

/// Most files remembered (oldest dropped first).
const MAX_FILES: usize = 4096;

#[derive(Clone)]
struct Entry {
    /// The file URL without the marker.
    url: String,
    attachment: MessageAttachment,
    seq: u64,
}

#[derive(Default)]
struct Registry {
    files: HashMap<String, Entry>,
    seq: u64,
    /// Entries were dropped since the shown messages were last re-marked.
    evicted: bool,
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
    url.rsplit_once(FRAGMENT).map(|(base, _)| base)
}

/// Whether `url` carries the encrypted-file marker.
pub fn is_marked(url: &str) -> bool {
    strip(url).is_some()
}

/// The key material of an encrypted file URL (with the marker).
fn lookup(url: &str) -> Option<MessageAttachment> {
    let (base, token) = url.rsplit_once(FRAGMENT)?;
    let reg = lock();
    let e = reg.files.get(token)?;
    (e.url == base).then(|| e.attachment.clone())
}

fn token(server: &ServerUrl, rid: &str, owner: &str, slot: usize) -> String {
    let mut h = Sha256Hasher::new();
    for part in [server.as_str(), rid, owner, &slot.to_string()] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    let mut hex = h.finish_hex();
    hex.truncate(32);
    hex
}

/// Records `keys` for `url` (any fragment dropped) in the slot `slot` of
/// `owner` (a message or upload id) and returns the marked absolute URL.
fn register(
    server: &ServerUrl,
    rid: &str,
    owner: &str,
    slot: usize,
    url: &str,
    keys: &MessageAttachment,
) -> String {
    let abs = server.absolute(url);
    let abs = abs.split('#').next().unwrap_or_default().to_owned();
    let token = token(server, rid, owner, slot);
    let mut reg = lock();
    reg.seq += 1;
    let seq = reg.seq;
    reg.files.insert(
        token.clone(),
        Entry {
            url: abs.clone(),
            attachment: keys.clone(),
            seq,
        },
    );
    if reg.files.len() > MAX_FILES {
        let mut by_age: Vec<(u64, String)> =
            reg.files.iter().map(|(k, e)| (e.seq, k.clone())).collect();
        by_age.sort();
        let excess = reg.files.len() - MAX_FILES;
        for (_, k) in by_age.into_iter().take(excess) {
            reg.files.remove(&k);
        }
        reg.evicted = true;
    }
    format!("{abs}{FRAGMENT}{token}")
}

/// Records the key of `a` (attachment `index` of the decrypted message `mid`
/// in `rid`) and rewrites its file URLs to absolute marked URLs. Attachments
/// without encryption are left alone. A URL that already carries a marker
/// (from the payload) is re-marked for this message.
pub fn mark_attachment(
    a: &mut MessageAttachment,
    server: &ServerUrl,
    rid: &str,
    mid: &str,
    index: usize,
) {
    if a.e2e_encryption().is_none() {
        return;
    }
    if a.e2e_sha256().is_none() {
        tracing::warn!("encrypted attachment without hashes.sha256: not verified");
    }
    let keys = MessageAttachment {
        encryption: a.encryption.clone(),
        hashes: a.hashes.clone(),
        ..Default::default()
    };
    for (n, field) in [
        &mut a.title_link,
        &mut a.image_url,
        &mut a.audio_url,
        &mut a.video_url,
    ]
    .into_iter()
    .enumerate()
    {
        let Some(u) = field.as_deref() else {
            continue;
        };
        let base = strip(u).unwrap_or(u);
        *field = Some(register(server, rid, mid, index * 4 + n, base, &keys));
    }
}

/// An encrypted URL could not be resolved: when entries were evicted, the
/// shown messages are marked again (their keys come back).
fn missing_key() {
    let evicted = std::mem::take(&mut lock().evicted);
    if evicted {
        super::shown::forget_files();
    }
}

/// Decrypts the metadata (`content`) of the encrypted uploads in a files
/// list (§7.7): name, type and type group are replaced and the download URL
/// is marked for decryption. Files that cannot be decrypted get a
/// placeholder name and no URL (nothing to download).
pub fn process_file_items(
    ws: &Workspace,
    server: &ServerUrl,
    rid: &str,
    items: &mut [crate::room::panel::api::FileItem],
) {
    let mgr = ws.e2e();
    for f in items.iter_mut() {
        let Some(content) = f.content.take() else {
            continue;
        };
        let meta = mgr.and_then(|m| m.decrypt_file_metadata(rid, &content).ok());
        let url = f.download_path().map(str::to_owned);
        match (meta, url) {
            (Some(meta), Some(url)) if meta.encryption.is_some() => {
                let keys = MessageAttachment {
                    encryption: meta
                        .encryption
                        .as_ref()
                        .and_then(|e| serde_json::to_value(e).ok()),
                    hashes: meta
                        .hashes
                        .as_ref()
                        .and_then(|h| serde_json::to_value(h).ok()),
                    ..Default::default()
                };
                f.path = Some(register(
                    server,
                    rid,
                    &format!("upload:{}", f.id),
                    0,
                    &url,
                    &keys,
                ));
                f.url = None;
                f.name = meta.name;
                f.mime = Some(meta.type_).filter(|t| !t.is_empty());
                f.type_group = Some(meta.type_group).filter(|t| !t.is_empty());
            }
            _ => {
                f.name = t!("e2e.encrypted_file").into_owned();
                f.mime = None;
                f.type_group = None;
                f.path = None;
                f.url = None;
            }
        }
    }
}

/// How long a decrypted media file opened with the system player is kept
/// (the player has opened it by then; on Unix it keeps reading the unlinked
/// file).
const PRIVATE_MEDIA_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn private_dir_slot() -> &'static Mutex<Option<std::path::PathBuf>> {
    static D: OnceLock<Mutex<Option<std::path::PathBuf>>> = OnceLock::new();
    D.get_or_init(Default::default)
}

/// The directory for decrypted media opened with the system player:
/// `e2e` inside the private media directory `media_dir`.
pub fn private_media_dir(media_dir: &std::path::Path) -> std::path::PathBuf {
    let dir = media_dir.join("e2e");
    *private_dir_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dir.clone());
    dir
}

/// Deletes the decrypted media files (all of them, or those older than
/// [`PRIVATE_MEDIA_TTL`] when `stale_only`). Without a known directory the
/// default cache location is cleaned.
pub async fn purge_private_media(stale_only: bool) {
    let dir = private_dir_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .or_else(|| {
            crate::images::default_cache_dir()
                .and_then(|d| d.parent().map(|p| p.join("media").join("e2e")))
        });
    let Some(dir) = dir else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten() {
            let stale = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age >= PRIVATE_MEDIA_TTL);
            if !stale_only || stale {
                let _ = std::fs::remove_file(e.path());
            }
        }
    })
    .await;
}

/// Deletes a decrypted media file opened with the system player after
/// [`PRIVATE_MEDIA_TTL`]. Call from inside the runtime.
pub fn delete_later(path: std::path::PathBuf) {
    tokio::spawn(async move {
        tokio::time::sleep(PRIVATE_MEDIA_TTL).await;
        let _ = tokio::fs::remove_file(&path).await;
    });
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
        let Some(a) = lookup(url) else {
            if is_marked(url) {
                missing_key();
            }
            return None;
        };
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
    t!("e2e.file_hash_mismatch").into_owned()
}

/// The text shown when the key of an encrypted file is not known.
pub fn key_missing_text() -> String {
    t!("e2e.file_key_missing").into_owned()
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
        return Some(Err(key_missing_text()));
    };
    let too_large = || t!("e2e.image_too_large").into_owned();
    let r = async {
        let dl = rest.download(base).await.map_err(|e| {
            tracing::debug!(error = %e, "encrypted image download failed");
            crate::error::user_text(&e)
        })?;
        if dl.content_length.is_some_and(|n| n > max_bytes) {
            return Err(too_large());
        }
        let mut body = Vec::new();
        let mut stream = Box::pin(dl.stream());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                tracing::debug!(error = %e, "encrypted image download failed");
                crate::error::user_text(&e)
            })?;
            if body.len() as u64 + chunk.len() as u64 > max_bytes {
                return Err(too_large());
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
        let original = a.clone();
        mark_attachment(&mut a, &server, "r", "m1", 0);
        let url = a.image_url.clone().unwrap();
        assert!(url.starts_with("http://localhost:3000/file-upload/f1/abc#rustket-e2e-"));
        assert_eq!(
            strip(&url),
            Some("http://localhost:3000/file-upload/f1/abc")
        );
        assert!(a.title_link.as_deref().is_some_and(is_marked));
        // Marking twice keeps the URL.
        mark_attachment(&mut a, &server, "r", "m1", 0);
        assert_eq!(a.image_url.as_deref(), Some(url.as_str()));

        // Another message pointing at the same upload with another key (and
        // even the first message's marked URL) does not replace that key.
        let (mut forged, _) = encrypted_attachment(b"garbage");
        forged.image_url = Some(url.clone());
        mark_attachment(&mut forged, &server, "r", "evil", 0);
        assert_ne!(forged.image_url.as_deref(), Some(url.as_str()));
        let mut again = original.clone();
        mark_attachment(&mut again, &server, "r", "m1", 0);
        assert_eq!(again.image_url.as_deref(), Some(url.as_str()));

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
        // A known token on another URL is not accepted.
        let token = url.rsplit_once(FRAGMENT).unwrap().1;
        assert!(
            Decryptor::for_url(&format!("http://localhost:3000/other{FRAGMENT}{token}")).is_none()
        );
    }

    #[test]
    fn plain_attachments_are_untouched() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let mut a = MessageAttachment {
            image_url: Some("/file-upload/x/y.png".into()),
            ..Default::default()
        };
        mark_attachment(&mut a, &server, "r", "m", 0);
        assert_eq!(a.image_url.as_deref(), Some("/file-upload/x/y.png"));
    }
}
