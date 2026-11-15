//! Encrypted file uploads and downloads (§7.8).

use rocketchat::e2e::{
    DecryptedMessage, E2eError, FileKey, FileMetadata, RoomKey, Sha256Hasher, hashed_upload_name,
};
use rocketchat::models::{FileInfo, MessageAttachment, UploadedFile};

use super::{PreparedUpload, Result, blocking};

/// Hashes and encrypts a file in place (blocking).
pub(super) fn encrypt_file(
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
