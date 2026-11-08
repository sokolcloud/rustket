use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::md::Root;
use super::{UserRef, lenient, null_default};
use crate::RcDate;

/// IMessage.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Message {
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    #[serde(deserialize_with = "null_default")]
    pub msg: String,
    #[serde(deserialize_with = "lenient::opt")]
    pub ts: Option<RcDate>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub u: UserRef,
    /// System message type (`uj`, `ul`, `r`, `room_changed_topic`, `discussion-created`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::md::opt_root"
    )]
    pub md: Option<Root>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub attachments: Option<Vec<MessageAttachment>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub file: Option<FileInfo>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub files: Option<Vec<FileInfo>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_map"
    )]
    pub reactions: Option<BTreeMap<String, Reaction>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub mentions: Option<Vec<Mention>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub channels: Option<Vec<ChannelRef>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub urls: Option<Vec<MessageUrl>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub tshow: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub tcount: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub tlm: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub replies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub dcount: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub dlm: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub pinned: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub pinned_at: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub pinned_by: Option<UserRef>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub starred: Option<Vec<IdRef>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub edited_at: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub edited_by: Option<UserRef>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub groupable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<Value>,
    /// UIKit blocks (rendered in a later phase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Value>,
    /// `pending` / `done` for encrypted messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2e: Option<String>,
    /// Encrypted payload (`{algorithm, ciphertext, iv?, kid?}`); see
    /// [`Message::encrypted_content`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    /// Plaintext mentions of an encrypted message (sent by the client).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub e2e_mentions: Option<crate::e2e::E2eMentions>,
    #[serde(rename = "_hidden", skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(
        rename = "_updatedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub updated_at: Option<RcDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<bool>,
    // Omnichannel (docs/omnichannel.md §18).
    /// Visitor token: present on messages sent by an omnichannel visitor.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub token: Option<String>,
    /// `livechat_transfer_history`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub transfer_data: Option<super::TransferData>,
    /// `omnichannel_placed_chat_on_hold` / `omnichannel_on_hold_chat_resumed`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub comment: Option<String>,
    /// `livechat-close`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub transcript_requested: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sla_data: Option<Value>,
    /// `livechat_navigation_history`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation: Option<Value>,
    /// `livechat_transcript_history`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_data: Option<Value>,
}

impl Message {
    pub fn is_system(&self) -> bool {
        self.t
            .as_deref()
            .is_some_and(|t| !t.is_empty() && t != "e2e")
    }

    pub fn is_thread_reply(&self) -> bool {
        self.tmid.is_some()
    }

    pub fn is_edited(&self) -> bool {
        self.edited_at.is_some() && self.t.as_deref() != Some("rm")
    }

    pub fn is_encrypted(&self) -> bool {
        self.t.as_deref() == Some("e2e") || self.content.is_some()
    }

    /// The decoded encrypted part of an `e2e` message: `content`, else a legacy
    /// v1 ciphertext in `msg`. `None` for other messages and for an `e2e`
    /// message with neither (e.g. an upload confirmed without content).
    pub fn encrypted_content(&self) -> Option<crate::e2e::Result<crate::e2e::EncryptedContent>> {
        if self.t.as_deref() != Some("e2e") {
            return None;
        }
        crate::e2e::EncryptedContent::from_message(self.content.as_ref(), &self.msg)
    }

    pub fn is_starred_by(&self, user_id: &str) -> bool {
        self.starred
            .as_ref()
            .is_some_and(|s| s.iter().any(|r| r.id == user_id))
    }

    /// Text to show when the message has no usable `md`.
    pub fn plain_text(&self) -> String {
        match &self.md {
            Some(md) if !md.is_empty() => super::md::to_plain_text(md),
            _ => self.msg.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IdRef {
    #[serde(rename = "_id")]
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub usernames: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
}

/// Entry of `mentions`; `_id` is `all` / `here` for group mentions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Mention {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fname: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelRef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fname: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MessageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// OpenGraph/oEmbed metadata (`pageTitle`, `ogImage`, `description`, …).
    #[serde(deserialize_with = "null_default")]
    pub meta: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<UrlHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_url: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_parse: Option<bool>,
}

impl MessageUrl {
    pub fn meta_str(&self, key: &str) -> Option<&str> {
        self.meta.get(key).and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UrlHeaders {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_length: Option<String>,
}

/// `file` / `files[]` of an upload message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileInfo {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// MessageAttachment: file attachments, quotes (`message_link`), pins, link
/// attachments of integrations, removed files. All fields optional.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageAttachment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `file`, `removed-file`, … (absent for quotes and integrations).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_link: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub title_link_download: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pretext: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        rename = "descriptionMd",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::md::opt_root"
    )]
    pub description_md: Option<Root>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::md::opt_root"
    )]
    pub md: Option<Root>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub image_dimensions: Option<Dimensions>,
    /// Base64 JPEG thumbnail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub image_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub audio_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub video_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_link: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub ts: Option<RcDate>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub fields: Option<Vec<AttachmentField>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub collapsed: Option<bool>,
    /// Nested attachments (quotes of quotes).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt_vec"
    )]
    pub attachments: Option<Vec<MessageAttachment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub size: Option<u64>,
    #[serde(rename = "fileId", skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Value>,
    /// E2E file encryption parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashes: Option<Value>,
    /// Encrypted content of the pinned message (`message_pinned_e2e`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

impl MessageAttachment {
    pub fn is_quote(&self) -> bool {
        self.message_link.is_some()
    }

    pub fn is_image(&self) -> bool {
        self.image_url.is_some()
    }

    /// Typed `encryption` of a decrypted encrypted-file attachment.
    pub fn e2e_encryption(&self) -> Option<crate::e2e::AttachmentEncryption> {
        serde_json::from_value(self.encryption.clone()?).ok()
    }

    /// `hashes.sha256` of a decrypted encrypted-file attachment.
    pub fn e2e_sha256(&self) -> Option<&str> {
        self.hashes.as_ref()?.get("sha256")?.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentField {
    pub title: String,
    /// String, number or bool.
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<bool>,
}

/// Body of `chat.sendMessage`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingMessage {
    /// Client-generated id (see [`crate::ids::random_id`]) so the echo can be matched.
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub rid: String,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tshow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<MessageAttachment>>,
}

impl OutgoingMessage {
    pub fn new(rid: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            id: Some(crate::ids::random_id()),
            rid: rid.into(),
            msg: msg.into(),
            ..Default::default()
        }
    }

    pub fn in_thread(mut self, tmid: impl Into<String>, also_send_to_channel: bool) -> Self {
        self.tmid = Some(tmid.into());
        self.tshow = also_send_to_channel.then_some(true);
        self
    }
}
