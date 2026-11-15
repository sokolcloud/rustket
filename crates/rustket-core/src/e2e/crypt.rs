//! Message encryption and decryption with room keys.

use std::collections::HashSet;
use std::sync::Arc;

use rocketchat::RcDate;
use rocketchat::e2e::{
    DecryptedMessage, EncryptedContent, FileMetadata, RoomKey, e2e_mentions_with_groups,
};
use rocketchat::models::{Message, MessageAttachment, UploadedFile};
use serde_json::Value;

use super::files::encrypt_file;
use super::{
    DecryptFailure, DecryptResult, DecryptedContent, E2eApi, E2eManager, E2eManagerError,
    OutgoingEncrypted, PreparedUpload, Result, RoomE2eState, RoomKeySet, blocking, lock, read,
    upload_payload,
};

impl<A: E2eApi> E2eManager<A> {
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
        self.encrypt_edit(rid, text, None, &|_| false)
    }

    /// Re-encrypts an edited message with the current key: the original
    /// decrypted `attachments`/`files`/`file` are kept, `msg` replaced.
    /// `is_group` tells the sender's mention-group names: their expansions
    /// (`@backend (@alice @bob)`) list the users, not the group, in the
    /// plaintext mentions.
    pub fn encrypt_edit(
        &self,
        rid: &str,
        text: &str,
        original: Option<&DecryptedMessage>,
        is_group: &dyn Fn(&str) -> bool,
    ) -> Result<OutgoingEncrypted> {
        let mut payload = original.cloned().unwrap_or_default();
        payload.msg = Some(text.to_owned());
        let content = self.encrypt_payload(rid, &payload)?;
        let settings = self.settings();
        let mentions = settings
            .mentions
            .then(|| e2e_mentions_with_groups(text, &settings.username_pattern, is_group));
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
    /// [`E2eConfig::server_url`](super::E2eConfig::server_url) is set.
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
