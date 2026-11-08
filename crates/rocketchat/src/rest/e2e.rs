//! End-to-end encryption endpoints (`e2e.*`, `users.resetE2EKey`, encrypted
//! messages and uploads; `docs/e2e.md` §5).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::files::{ProgressFn, UploadSource};
use super::{Query, RestClient, field};
use crate::auth::TwoFactorCode;
use crate::e2e::E2eMentions;
use crate::error::{Error, Result};
use crate::models::{Message, UploadedFile};

/// MIME type of encrypted uploads (the server stores this for every encrypted
/// file anyway).
pub const ENCRYPTED_UPLOAD_MIME: &str = "application/octet-stream";

/// `e2e.fetchMyKeys`: both `None` when the user has no keys.
#[derive(Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct MyKeys {
    pub public_key: Option<String>,
    pub private_key: Option<String>,
}

impl std::fmt::Debug for MyKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MyKeys")
            .field("public_key", &self.public_key.is_some())
            .field("private_key", &self.private_key.is_some())
            .finish()
    }
}

impl MyKeys {
    /// Both keys present and non-empty.
    pub fn has_keys(&self) -> bool {
        self.public_key.as_deref().is_some_and(|s| !s.is_empty())
            && self.private_key.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// Outcome of `e2e.setRoomKeyID` (atomic: only the first caller wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetKeyIdOutcome {
    Set,
    /// `error-room-e2e-key-already-exists`: another member won the race.
    AlreadyExists,
}

/// A member without the room key and their public key
/// (`e2e.getUsersOfRoomWithoutKey`, `e2e.fetchUsersWaitingForGroupKey`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserPublicKeyEntry {
    pub user_id: String,
    /// Public JWK string.
    pub public_key: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUserWithoutKey {
    #[serde(rename = "_id")]
    id: String,
    e2e: Option<RawE2e>,
    /// `fetchUsersWaitingForGroupKey` puts it at the top level.
    public_key: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawE2e {
    public_key: Option<String>,
}

impl RawUserWithoutKey {
    fn into_entry(self) -> Option<UserPublicKeyEntry> {
        let key = self
            .public_key
            .or_else(|| self.e2e.and_then(|e| e.public_key))
            .filter(|k| !k.is_empty())?;
        (!self.id.is_empty()).then_some(UserPublicKeyEntry {
            user_id: self.id,
            public_key: key,
        })
    }
}

fn entries(v: Value) -> Vec<UserPublicKeyEntry> {
    crate::models::lenient::vec_from_value::<RawUserWithoutKey>(v)
        .into_iter()
        .filter_map(RawUserWithoutKey::into_entry)
        .collect()
}

/// An old room key wrapped for a member (`oldKeys[]` of
/// `e2e.provideUsersSuggestedGroupKeys`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuggestedOldKey {
    #[serde(rename = "e2eKeyId")]
    pub e2e_key_id: String,
    /// ISO-8601 timestamp.
    pub ts: String,
    #[serde(rename = "E2EKey")]
    pub e2e_key: String,
}

/// One entry of `usersSuggestedGroupKeys[rid]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuggestedKey {
    #[serde(rename = "_id")]
    pub user_id: String,
    /// The current room key wrapped for the member.
    pub key: String,
    #[serde(rename = "oldKeys", skip_serializing_if = "Vec::is_empty")]
    pub old_keys: Vec<SuggestedOldKey>,
}

/// JSON body of `rooms.mediaConfirm` for an encrypted upload. Never carries
/// `description`/`fileName` (they would be stored in plaintext, §5.7).
#[derive(Debug, Clone, PartialEq)]
pub struct EncryptedMediaConfirm {
    pub tmid: Option<String>,
    /// v2 envelope of the message payload `{attachments, files, file, msg}`.
    pub content: Value,
    /// v2 envelope of the file metadata.
    pub file_content: Value,
}

fn is_code(e: &Error, code: &str) -> bool {
    e.api_error_code() == Some(code)
        || matches!(e, Error::Api { error: Some(m), .. } if m.contains(code))
}

impl RestClient {
    /// `GET e2e.fetchMyKeys`.
    pub async fn e2e_fetch_my_keys(&self) -> Result<MyKeys> {
        let mut keys: MyKeys = self.get("e2e.fetchMyKeys", &Query::new()).await?;
        keys.public_key = keys.public_key.filter(|s| !s.is_empty());
        keys.private_key = keys.private_key.filter(|s| !s.is_empty());
        Ok(keys)
    }

    /// `POST e2e.setUserPublicAndPrivateKeys`. Without `force` the server refuses
    /// (`error-keys-already-set`) when keys exist.
    pub async fn e2e_set_user_keys(
        &self,
        public_key: &str,
        private_key: &str,
        force: bool,
    ) -> Result<()> {
        let mut body = json!({ "public_key": public_key, "private_key": private_key });
        if force {
            body["force"] = Value::Bool(true);
        }
        self.post_ok("e2e.setUserPublicAndPrivateKeys", &body).await
    }

    /// `POST e2e.requestSubscriptionKeys`: broadcasts `{rid}/e2e.keyRequest` for
    /// every room where the caller lacks the key.
    pub async fn e2e_request_subscription_keys(&self) -> Result<()> {
        self.post_ok("e2e.requestSubscriptionKeys", &json!({}))
            .await
    }

    /// `POST e2e.setRoomKeyID`.
    pub async fn e2e_set_room_key_id(&self, rid: &str, key_id: &str) -> Result<SetKeyIdOutcome> {
        match self
            .post_ok("e2e.setRoomKeyID", &json!({ "rid": rid, "keyID": key_id }))
            .await
        {
            Ok(()) => Ok(SetKeyIdOutcome::Set),
            Err(e) if is_code(&e, "error-room-e2e-key-already-exists") => {
                Ok(SetKeyIdOutcome::AlreadyExists)
            }
            Err(e) => Err(e),
        }
    }

    /// `POST e2e.updateGroupKey`: `uid == me` sets my `E2EKey`, otherwise the
    /// member's `E2ESuggestedKey`.
    pub async fn e2e_update_group_key(&self, rid: &str, uid: &str, key: &str) -> Result<()> {
        self.post_ok(
            "e2e.updateGroupKey",
            &json!({ "rid": rid, "uid": uid, "key": key }),
        )
        .await
    }

    /// `GET e2e.getUsersOfRoomWithoutKey`: members without `E2EKey` that have a
    /// public key (may include the caller).
    pub async fn e2e_users_of_room_without_key(
        &self,
        rid: &str,
    ) -> Result<Vec<UserPublicKeyEntry>> {
        let v: Value = self
            .get(
                "e2e.getUsersOfRoomWithoutKey",
                &Query::new().push("rid", rid),
            )
            .await?;
        Ok(v.get("users").cloned().map(entries).unwrap_or_default())
    }

    /// `POST e2e.acceptSuggestedGroupKey`.
    pub async fn e2e_accept_suggested_group_key(&self, rid: &str) -> Result<()> {
        self.post_ok("e2e.acceptSuggestedGroupKey", &json!({ "rid": rid }))
            .await
    }

    /// `POST e2e.rejectSuggestedGroupKey` (re-queues the caller).
    pub async fn e2e_reject_suggested_group_key(&self, rid: &str) -> Result<()> {
        self.post_ok("e2e.rejectSuggestedGroupKey", &json!({ "rid": rid }))
            .await
    }

    /// `GET e2e.fetchUsersWaitingForGroupKey?roomIds[]=…`: per room, up to 50
    /// other members with neither `E2EKey` nor `E2ESuggestedKey`. Empty when
    /// `E2E_Enable` is off.
    pub async fn e2e_users_waiting_for_group_key(
        &self,
        room_ids: &[&str],
    ) -> Result<HashMap<String, Vec<UserPublicKeyEntry>>> {
        if room_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let q = room_ids
            .iter()
            .fold(Query::new(), |q, rid| q.push("roomIds[]", rid));
        let v: Value = self.get("e2e.fetchUsersWaitingForGroupKey", &q).await?;
        let mut out = HashMap::new();
        if let Some(Value::Object(map)) = v.get("usersWaitingForE2EKeys") {
            for (rid, users) in map {
                out.insert(rid.clone(), entries(users.clone()));
            }
        }
        Ok(out)
    }

    /// `POST e2e.provideUsersSuggestedGroupKeys` (no-op when `E2E_Enable` is off).
    pub async fn e2e_provide_users_suggested_group_keys(
        &self,
        keys: &HashMap<String, Vec<SuggestedKey>>,
    ) -> Result<()> {
        self.post_ok(
            "e2e.provideUsersSuggestedGroupKeys",
            &json!({ "usersSuggestedGroupKeys": keys }),
        )
        .await
    }

    /// `POST e2e.resetRoomKey` with the new key wrapped for the caller. Every
    /// internal failure is reported as `error-e2e-key-reset-failed`.
    pub async fn e2e_reset_room_key(
        &self,
        rid: &str,
        e2e_key: &str,
        e2e_key_id: &str,
    ) -> Result<()> {
        self.post_ok(
            "e2e.resetRoomKey",
            &json!({ "rid": rid, "e2eKey": e2e_key, "e2eKeyId": e2e_key_id }),
        )
        .await
    }

    /// `POST users.resetE2EKey {}`: resets the caller's keys and revokes all login
    /// tokens. Expect [`Error::TwoFactorRequired`] (also `method: password` for
    /// users without 2FA) and retry with the code.
    pub async fn e2e_reset_own_key(&self, two_factor: Option<&TwoFactorCode>) -> Result<()> {
        let _: Value = self
            .post_with("users.resetE2EKey", &json!({}), two_factor)
            .await?;
        Ok(())
    }

    /// `POST rooms.saveRoomSettings {rid, encrypted}`.
    pub async fn e2e_set_room_encrypted(&self, rid: &str, encrypted: bool) -> Result<()> {
        self.post_ok(
            "rooms.saveRoomSettings",
            &json!({ "rid": rid, "encrypted": encrypted }),
        )
        .await
    }

    /// `POST chat.sendMessage` of an encrypted message: `t: 'e2e'`, `content`,
    /// optional `e2eMentions`; no `msg`, `md` or `e2e`.
    pub async fn e2e_send_message(
        &self,
        rid: &str,
        id: &str,
        tmid: Option<&str>,
        tshow: bool,
        content: &Value,
        mentions: Option<&E2eMentions>,
    ) -> Result<Message> {
        let mut message = json!({ "_id": id, "rid": rid, "t": "e2e", "content": content });
        if let Some(t) = tmid {
            message["tmid"] = Value::String(t.into());
            if tshow {
                message["tshow"] = Value::Bool(true);
            }
        }
        if let Some(m) = mentions {
            message["e2eMentions"] = serde_json::to_value(m)?;
        }
        let v: Value = self
            .post("chat.sendMessage", &json!({ "message": message }))
            .await?;
        field(v, "message")
    }

    /// `POST chat.update {roomId, msgId, content, e2eMentions?}` (never `text`).
    pub async fn e2e_update_message(
        &self,
        rid: &str,
        id: &str,
        content: &Value,
        mentions: Option<&E2eMentions>,
    ) -> Result<Message> {
        let mut body = json!({ "roomId": rid, "msgId": id, "content": content });
        if let Some(m) = mentions {
            body["e2eMentions"] = serde_json::to_value(m)?;
        }
        let v: Value = self.post("chat.update", &body).await?;
        field(v, "message")
    }

    /// `POST rooms.media/{rid}` of encrypted bytes: file name = hashed name,
    /// MIME `application/octet-stream`, text part `content` = JSON of the v2
    /// envelope of the file metadata.
    pub async fn e2e_upload_media(
        &self,
        rid: &str,
        hashed_name: String,
        source: UploadSource,
        file_content: &Value,
        progress: Option<ProgressFn>,
    ) -> Result<UploadedFile> {
        self.upload_media_with_fields(
            rid,
            hashed_name,
            ENCRYPTED_UPLOAD_MIME.to_owned(),
            source,
            progress,
            vec![("content".to_owned(), file_content.to_string())],
        )
        .await
    }

    /// `POST rooms.mediaConfirm/{rid}/{fileId}` with `{tmid?, t: 'e2e', msg: '',
    /// content, fileContent}`.
    pub async fn e2e_confirm_media(
        &self,
        rid: &str,
        file_id: &str,
        confirm: &EncryptedMediaConfirm,
    ) -> Result<Message> {
        let mut body = json!({
            "t": "e2e",
            "msg": "",
            "content": confirm.content,
            "fileContent": confirm.file_content,
        });
        if let Some(t) = &confirm.tmid {
            body["tmid"] = Value::String(t.clone());
        }
        let v: Value = self
            .post(
                &format!(
                    "rooms.mediaConfirm/{}/{}",
                    crate::server_url::encode_segment(rid),
                    crate::server_url::encode_segment(file_id)
                ),
                &body,
            )
            .await?;
        field(v, "message")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn user_entries_from_both_shapes() {
        let v = json!([
            {"_id": "a", "e2e": {"public_key": "{\"kty\":\"RSA\"}"}},
            {"_id": "b", "public_key": "pk"},
            {"_id": "c"},
            {"_id": "", "public_key": "pk"},
            {"_id": "d", "e2e": {"public_key": ""}},
            7
        ]);
        let e = entries(v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].user_id, "a");
        assert_eq!(e[1].public_key, "pk");
    }

    #[test]
    fn suggested_keys_serialize_like_the_web() {
        let mut map = HashMap::new();
        map.insert(
            "rid".to_owned(),
            vec![
                SuggestedKey {
                    user_id: "u".into(),
                    key: "k".into(),
                    old_keys: vec![SuggestedOldKey {
                        e2e_key_id: "old".into(),
                        ts: "2026-11-07T21:55:22.123Z".into(),
                        e2e_key: "oldk".into(),
                    }],
                },
                SuggestedKey {
                    user_id: "v".into(),
                    key: "k2".into(),
                    old_keys: vec![],
                },
            ],
        );
        assert_eq!(
            json!({ "usersSuggestedGroupKeys": map }),
            json!({"usersSuggestedGroupKeys": {"rid": [
                {"_id": "u", "key": "k", "oldKeys": [{"e2eKeyId": "old", "ts": "2026-11-07T21:55:22.123Z", "E2EKey": "oldk"}]},
                {"_id": "v", "key": "k2"}
            ]}})
        );
    }

    #[test]
    fn my_keys_debug_hides_keys() {
        let k: MyKeys = serde_json::from_value(
            json!({"public_key": "pub", "private_key": "secret", "success": true}),
        )
        .unwrap();
        assert!(k.has_keys());
        assert!(!format!("{k:?}").contains("secret"));
        let none: MyKeys = serde_json::from_value(json!({"success": true})).unwrap();
        assert!(!none.has_keys());
    }
}
