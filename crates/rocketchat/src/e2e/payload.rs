//! Decrypted message and file payloads (whitelisted, §3.7, §7.7, §7.8).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::file::AttachmentEncryption;
use super::{E2eError, Result};
use crate::ServerUrl;
use crate::models::{FileInfo, MessageAttachment, lenient};

/// The fields Rustket takes from a decrypted message plaintext. Everything else
/// (`_id`, `rid`, `u`, `ts`, `t`, …) is ignored: the plaintext is controlled by
/// any room member.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecryptedMessage {
    /// `msg`, or the legacy `text` when there is no `msg`.
    pub msg: Option<String>,
    pub attachments: Option<Vec<MessageAttachment>>,
    pub files: Option<Vec<FileInfo>>,
    pub file: Option<FileInfo>,
}

impl DecryptedMessage {
    /// Interprets a decrypted plaintext. Non-objects are an error
    /// (indecipherable); invalid list entries are skipped.
    pub fn from_value(v: &Value) -> Result<Self> {
        let o = v
            .as_object()
            .ok_or(E2eError::Format("decrypted payload is not an object"))?;
        let msg = o
            .get("msg")
            .and_then(Value::as_str)
            .or_else(|| o.get("text").and_then(Value::as_str))
            .map(str::to_owned);
        Ok(Self {
            msg,
            attachments: list(o, "attachments"),
            files: list(o, "files"),
            file: o
                .get("file")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
        })
    }

    /// The plaintext object to encrypt (`attachments`/`files`/`file` omitted when
    /// `None`).
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert(
            "msg".into(),
            Value::String(self.msg.clone().unwrap_or_default()),
        );
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                o.insert(k.into(), v);
            }
        };
        put(
            "attachments",
            self.attachments
                .as_ref()
                .and_then(|a| serde_json::to_value(a).ok()),
        );
        put(
            "files",
            self.files
                .as_ref()
                .and_then(|a| serde_json::to_value(a).ok()),
        );
        put(
            "file",
            self.file
                .as_ref()
                .and_then(|a| serde_json::to_value(a).ok()),
        );
        Value::Object(o)
    }

    pub fn text(&self) -> &str {
        self.msg.as_deref().unwrap_or_default()
    }

    /// Drops attachment URLs that are not server-relative paths or URLs of
    /// `server` (a member could point them anywhere).
    pub fn sanitize_urls(&mut self, server: &ServerUrl) {
        if let Some(atts) = &mut self.attachments {
            for a in atts {
                sanitize_attachment(a, server);
            }
        }
    }

    /// The file encryption parameters of each decrypted attachment that has them.
    pub fn file_keys(&self) -> Vec<(&MessageAttachment, AttachmentEncryption)> {
        self.attachments
            .iter()
            .flatten()
            .filter_map(|a| {
                let enc = a.encryption.as_ref()?;
                Some((a, serde_json::from_value(enc.clone()).ok()?))
            })
            .collect()
    }
}

fn list<T: serde::de::DeserializeOwned>(o: &Map<String, Value>, k: &str) -> Option<Vec<T>> {
    o.get(k)
        .filter(|v| v.is_array())
        .map(|v| lenient::vec_from_value(v.clone()))
}

fn url_allowed(url: &str, server: &ServerUrl) -> bool {
    (url.starts_with('/') && !url.starts_with("//") && !url.starts_with("/\\")) || server.owns(url)
}

fn sanitize_attachment(a: &mut MessageAttachment, server: &ServerUrl) {
    for field in [
        &mut a.title_link,
        &mut a.image_url,
        &mut a.thumb_url,
        &mut a.audio_url,
        &mut a.video_url,
        &mut a.author_icon,
        &mut a.author_link,
        &mut a.message_link,
    ] {
        if field.as_deref().is_some_and(|u| !url_allowed(u, server)) {
            *field = None;
        }
    }
    if let Some(nested) = &mut a.attachments {
        for n in nested {
            sanitize_attachment(n, server);
        }
    }
}

/// `hashes` of an encrypted file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileHashes {
    /// Lowercase hex SHA-256 of the plaintext file.
    pub sha256: String,
}

/// Decrypted upload metadata (`rocketchat_uploads.content`, `fileContent` of
/// `rooms.mediaConfirm`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileMetadata {
    #[serde(rename = "type")]
    pub type_: String,
    pub type_group: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<AttachmentEncryption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashes: Option<FileHashes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
}

impl FileMetadata {
    /// Metadata as the web builds it: `typeGroup` = MIME type before `/`.
    pub fn new(name: &str, mime: &str, encryption: AttachmentEncryption, sha256: String) -> Self {
        Self {
            type_: mime.to_owned(),
            type_group: mime.split('/').next().unwrap_or_default().to_owned(),
            name: name.to_owned(),
            encryption: Some(encryption),
            hashes: Some(FileHashes { sha256 }),
            alt_text: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whitelist_and_legacy_text() {
        let d = DecryptedMessage::from_value(&json!({
            "_id": "spoof", "rid": "other", "u": {"_id": "admin"}, "t": "uj",
            "text": "world", "ts": {"$date": 1}
        }))
        .unwrap();
        assert_eq!(d.msg.as_deref(), Some("world"));
        assert!(d.attachments.is_none() && d.file.is_none());
        let d = DecryptedMessage::from_value(&json!({"msg": "a", "text": "b"})).unwrap();
        assert_eq!(d.text(), "a");
        assert!(DecryptedMessage::from_value(&json!("x")).is_err());
        assert!(DecryptedMessage::from_value(&json!([1])).is_err());
        let d =
            DecryptedMessage::from_value(&json!({"attachments": [5, {"title": "t"}], "file": 3}))
                .unwrap();
        assert_eq!(d.attachments.unwrap().len(), 1);
        assert!(d.file.is_none());
    }

    #[test]
    fn upload_payload_roundtrip_and_keys() {
        let v = json!({
            "attachments": [{"title": "notes.txt", "type": "file", "title_link": "/file-upload/f1/abc",
                "title_link_download": true,
                "encryption": {"key": {"alg":"A256CTR","ext":true,"k":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","key_ops":["encrypt","decrypt"],"kty":"oct"}, "iv": "AAECAwQFBgf//////////g=="},
                "hashes": {"sha256": "27ba"}, "fileId": "f1"}],
            "files": [{"_id": "f1", "name": "notes.txt", "type": "text/plain", "size": 52, "format": "TXT"}],
            "file": {"_id": "f1", "name": "notes.txt", "type": "text/plain", "size": 52, "format": "TXT"},
            "msg": "caption"
        });
        let d = DecryptedMessage::from_value(&v).unwrap();
        assert_eq!(d.file.as_ref().unwrap().name, "notes.txt");
        let keys = d.file_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].1.iv, "AAECAwQFBgf//////////g==");
        let back = DecryptedMessage::from_value(&d.to_value()).unwrap();
        assert_eq!(back, d);
        assert_eq!(DecryptedMessage::default().to_value(), json!({"msg": ""}));
    }

    #[test]
    fn foreign_urls_are_dropped() {
        let server = ServerUrl::parse("https://chat.example.com").unwrap();
        let mut d = DecryptedMessage::from_value(&json!({"attachments": [
            {"title_link": "/file-upload/x", "image_url": "https://evil.example/x.png",
             "message_link": "https://chat.example.com/group/x?msg=1",
             "author_icon": "//evil.example/a.png",
             "attachments": [{"thumb_url": "http://chat.example.com/t"}]}
        ]}))
        .unwrap();
        d.sanitize_urls(&server);
        let a = &d.attachments.as_ref().unwrap()[0];
        assert_eq!(a.title_link.as_deref(), Some("/file-upload/x"));
        assert!(a.image_url.is_none());
        assert!(a.author_icon.is_none());
        assert!(a.message_link.is_some());
        assert!(a.attachments.as_ref().unwrap()[0].thumb_url.is_none());
    }

    #[test]
    fn file_metadata_shape() {
        let enc = AttachmentEncryption::new(&crate::e2e::FileKey::generate());
        let m = FileMetadata::new("Отчёт.pdf", "application/pdf", enc, "ab".into());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["type"], "application/pdf");
        assert_eq!(v["typeGroup"], "application");
        assert_eq!(v["hashes"]["sha256"], "ab");
        assert_eq!(v["encryption"]["key"]["alg"], "A256CTR");
        assert!(v.get("altText").is_none());
        let back: FileMetadata = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }
}
