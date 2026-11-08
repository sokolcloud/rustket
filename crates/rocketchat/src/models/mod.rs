//! Tolerant serde models of Rocket.Chat payloads.

pub mod admin;
mod apps;
pub mod lenient;
pub mod md;
mod message;
mod misc;
mod omnichannel;
mod room;
mod settings;
mod subscription;
mod user;

pub use apps::*;
pub use message::*;
pub use misc::*;
pub use omnichannel::*;
pub use room::*;
pub use settings::*;
pub use subscription::*;
pub use user::*;

use serde::{Deserialize, Serialize};

use crate::RcDate;

/// `{_id, username, name}` as embedded in messages, rooms and subscriptions.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserRef {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Room type `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RoomType {
    /// Public channel.
    #[serde(rename = "c")]
    #[default]
    Channel,
    /// Private group.
    #[serde(rename = "p")]
    Private,
    /// Direct message (1:1 or group DM).
    #[serde(rename = "d")]
    Direct,
    /// Omnichannel (livechat).
    #[serde(rename = "l")]
    Livechat,
    /// Omnichannel VoIP.
    #[serde(rename = "v")]
    Voip,
    #[serde(other)]
    Unknown,
}

impl RoomType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Channel => "c",
            Self::Private => "p",
            Self::Direct => "d",
            Self::Livechat => "l",
            Self::Voip => "v",
            Self::Unknown => "",
        }
    }
}

/// Paging fields returned by list endpoints, flattened next to the item array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Paging {
    pub count: u64,
    pub offset: u64,
    pub total: u64,
}

/// A page of items. `items` holds the endpoint-specific array (`messages`, `members`, …).
/// Invalid elements are skipped: decide on more pages with `raw_len`, not `items.len()`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub count: u64,
    pub offset: u64,
    pub total: u64,
    /// Elements the server returned (including dropped ones).
    pub raw_len: usize,
    /// Elements skipped because they failed to parse.
    pub dropped: usize,
}

impl<T> Paginated<T> {
    pub fn has_more(&self) -> bool {
        self.offset + (self.raw_len as u64) < self.total
    }
}

/// `{update: [...], remove: [...]}` delta responses (`updatedSince`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    from = "RawUpdatedSince",
    bound(deserialize = "T: serde::de::DeserializeOwned, R: serde::de::DeserializeOwned")
)]
pub struct UpdatedSince<T, R = Removed> {
    /// Invalid entries are skipped (logged at warn), so one bad room never fails the list.
    pub update: Vec<T>,
    pub remove: Vec<R>,
    /// Number of `update`/`remove` entries skipped because they failed to parse.
    #[serde(skip)]
    pub dropped: usize,
}

impl<T, R> Default for UpdatedSince<T, R> {
    fn default() -> Self {
        Self {
            update: Vec::new(),
            remove: Vec::new(),
            dropped: 0,
        }
    }
}

#[derive(Deserialize)]
#[doc(hidden)]
pub struct RawUpdatedSince {
    #[serde(default)]
    update: serde_json::Value,
    #[serde(default)]
    remove: serde_json::Value,
}

impl<T: serde::de::DeserializeOwned, R: serde::de::DeserializeOwned> From<RawUpdatedSince>
    for UpdatedSince<T, R>
{
    fn from(raw: RawUpdatedSince) -> Self {
        let update = lenient::counted(raw.update);
        let remove = lenient::counted(raw.remove);
        Self {
            dropped: update.dropped + remove.dropped,
            update: update.items,
            remove: remove.items,
        }
    }
}

/// An entry of a `remove` list: at least `_id`, often `_deletedAt`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Removed {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    #[serde(
        rename = "_deletedAt",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::opt"
    )]
    pub deleted_at: Option<RcDate>,
}

/// Deserialises a `null` as the default value.
pub(crate) fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
