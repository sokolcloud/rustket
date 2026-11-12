//! Persistent offline cache: one SQLite database per server and account
//! (`<data dir>/<server id>/cache.sqlite`, directory 0700, file 0600).
//!
//! It keeps what a workspace needs to be shown before the network sync
//! finishes, and to be read while the server is unreachable: server info,
//! public settings, permissions, roles, the current user (without services
//! or E2E keys), subscriptions and rooms, custom emoji and statuses, the
//! newest [`MESSAGES_PER_TIMELINE`] messages of recently opened rooms and
//! threads (least recently used ones are evicted beyond a total size), the
//! sidebar's thread data ([`CachedThreads`]) and the persistent outbox
//! (pending text messages with their client ids).
//!
//! Tokens and secrets never go here (they stay in the secret store).
//! End-to-end encrypted messages are stored as the server sent them
//! (ciphertext in `content`); every message passes [`sanitize`] on its way
//! into the database, which drops the decrypted fields of a message the app
//! processed, or the whole message when it cannot be reduced to ciphertext.
//!
//! [`CacheDb`] is the synchronous store (tests, the worker); [`CacheHandle`]
//! runs it on a dedicated thread so no IO happens on the UI thread. A
//! database that is corrupt or was written by a newer schema is moved aside
//! and a fresh one is started.

mod db;
pub mod sanitize;
mod worker;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use rocketchat::RcDate;
use rocketchat::models::{
    CustomEmoji, CustomUserStatus, Message, Permission, Role, Room, ServerInfo, Setting,
    Subscription, User,
};

pub use db::{CacheDb, OpenReport, SCHEMA_VERSION, destroy, dir_size};
pub use worker::{CacheHandle, Opened};

use crate::ServerId;

/// Messages kept per room or thread timeline.
pub const MESSAGES_PER_TIMELINE: usize = 100;
/// Total size of cached messages before least recently used timelines are
/// evicted.
pub const DEFAULT_MAX_MESSAGE_BYTES: u64 = 50 * 1024 * 1024;
/// Most timelines kept.
pub const MAX_TIMELINES: usize = 400;
/// Database file name inside the server directory.
pub const FILE_NAME: &str = "cache.sqlite";

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("cache file: {0}")]
    Io(#[from] std::io::Error),
    #[error("cache data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cache schema {found} is newer than {supported}")]
    Incompatible { found: i64, supported: i64 },
    #[error("cache integrity check failed: {0}")]
    Corrupt(String),
    #[error("cache worker stopped")]
    Closed,
}

pub type Result<T> = std::result::Result<T, CacheError>;

/// `$XDG_DATA_HOME/rustket` (platform equivalent elsewhere): the base of
/// every server's cache directory.
pub fn default_base() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustket").map(|d| d.data_dir().to_owned())
}

/// The cache directory of a server under `base`. Server ids are random
/// alphanumerics; anything else (a hand-edited config) is hex-encoded so it
/// can never leave `base`.
pub fn server_dir(base: &Path, server: &ServerId) -> PathBuf {
    let id = server.as_str();
    let safe = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if safe {
        base.join(id)
    } else {
        let hex: String = id.bytes().map(|b| format!("{b:02x}")).collect();
        base.join(format!("x{}", &hex[..hex.len().min(120)]))
    }
}

/// A room timeline (`tmid: None`) or a thread.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimelineKey {
    pub rid: String,
    pub tmid: Option<String>,
}

impl TimelineKey {
    pub fn room(rid: impl Into<String>) -> Self {
        Self {
            rid: rid.into(),
            tmid: None,
        }
    }

    pub fn thread(rid: impl Into<String>, tmid: impl Into<String>) -> Self {
        Self {
            rid: rid.into(),
            tmid: Some(tmid.into()),
        }
    }

    fn db_key(&self) -> String {
        match &self.tmid {
            Some(t) => format!("{}/{t}", self.rid),
            None => self.rid.clone(),
        }
    }
}

/// The cached documents of a workspace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedState {
    pub me: Option<User>,
    pub server_info: Option<ServerInfo>,
    pub settings: Vec<Setting>,
    pub permissions: Vec<Permission>,
    pub roles: Vec<Role>,
    pub subscriptions: Vec<Subscription>,
    pub rooms: Vec<Room>,
    pub custom_emoji: Vec<CustomEmoji>,
    pub custom_statuses: Vec<CustomUserStatus>,
    /// When the state was last written (Unix ms).
    pub saved_at: Option<i64>,
    /// The sidebar's thread data.
    pub threads: CachedThreads,
}

/// What the sidebar's Threads section had loaded: followed sets of crawled
/// rooms and the thread parents the section refers to. A restart crawls
/// again only rooms with replies after `synced_activity`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedThreads {
    pub followed: Vec<CachedFollowed>,
    pub parents: Vec<Message>,
    /// Per room with parents: up to which `lr` they are current. Parents of
    /// a room without one are not known to be current.
    pub parents_synced: Vec<CachedParentsSynced>,
}

impl CachedThreads {
    pub fn is_empty(&self) -> bool {
        self.followed.is_empty() && self.parents.is_empty() && self.parents_synced.is_empty()
    }

    /// Drops what belongs to rooms `keep` rejects.
    pub fn retain_rooms(&mut self, keep: impl Fn(&str) -> bool) {
        self.followed.retain(|f| keep(&f.rid));
        self.parents.retain(|m| keep(&m.rid));
        self.parents_synced.retain(|p| keep(&p.rid));
    }
}

/// The thread parents of a room, current up to the room's `lr` of
/// `synced_activity`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CachedParentsSynced {
    #[serde(default)]
    pub rid: String,
    #[serde(default)]
    pub synced_activity: Option<RcDate>,
}

/// A room's followed threads, current up to the room's `lr` of
/// `synced_activity`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CachedFollowed {
    #[serde(default)]
    pub rid: String,
    #[serde(default)]
    pub tmids: Vec<String>,
    #[serde(default)]
    pub synced_activity: Option<RcDate>,
    /// The crawl stopped at a page while the room had more (a row limit).
    #[serde(default)]
    pub short: Option<crate::thread_index::ShortCrawl>,
}

impl CachedState {
    pub fn is_empty(&self) -> bool {
        self.me.is_none() && self.subscriptions.is_empty() && self.rooms.is_empty()
    }
}

/// The cached newest messages of a timeline (oldest first).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedTimeline {
    pub key: TimelineKey,
    /// Thread parent (threads only).
    pub parent: Option<Message>,
    pub messages: Vec<Message>,
}

/// A pending outgoing text message.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    /// The local message (client-generated `_id`, room, thread, text).
    pub message: Message,
    /// Sent automatically once the server is reachable again (written
    /// offline, or still sending when the app quit). Otherwise it failed
    /// and waits for the user's retry or discard.
    pub queued: bool,
    /// Queued while a send was in flight (the app quit before the answer):
    /// the server may have it, and it may have been deleted since.
    pub maybe_sent: bool,
    /// The last error of a failed send.
    pub error: Option<String>,
}

/// One set of changes, written in one transaction. `None` / empty fields
/// leave the stored data alone.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    /// Replaces every document (an initial load): rooms not in it are
    /// forgotten together with their messages and thread data. Its
    /// `threads` are not written (see [`Self::threads`]).
    pub full: Option<Box<CachedState>>,
    pub me: Option<User>,
    pub server_info: Option<ServerInfo>,
    pub settings: Option<Vec<Setting>>,
    pub permissions: Option<Vec<Permission>>,
    pub roles: Option<Vec<Role>>,
    pub custom_emoji: Option<Vec<CustomEmoji>>,
    pub custom_statuses: Option<Vec<CustomUserStatus>>,
    /// Room entries to store; both `None`: the room is gone (its messages
    /// are dropped too).
    pub rooms: Vec<(String, Option<Subscription>, Option<Room>)>,
    /// Newest messages of timelines (each replaces what is stored for it).
    pub timelines: Vec<CachedTimeline>,
    /// Replaces the whole outbox.
    pub outbox: Option<Vec<OutboxEntry>>,
    /// Names of uploads interrupted by quitting (reported on the next start).
    pub interrupted_uploads: Option<Vec<String>>,
    /// Replaces the sidebar's thread data.
    pub threads: Option<CachedThreads>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.full.is_none()
            && self.me.is_none()
            && self.server_info.is_none()
            && self.settings.is_none()
            && self.permissions.is_none()
            && self.roles.is_none()
            && self.custom_emoji.is_none()
            && self.custom_statuses.is_none()
            && self.rooms.is_empty()
            && self.timelines.is_empty()
            && self.outbox.is_none()
            && self.interrupted_uploads.is_none()
            && self.threads.is_none()
    }

    /// Folds a later batch into this one (later values win).
    pub fn merge(&mut self, later: Batch) {
        if later.full.is_some() {
            // A full snapshot supersedes every earlier document change.
            self.me = None;
            self.server_info = None;
            self.settings = None;
            self.permissions = None;
            self.roles = None;
            self.custom_emoji = None;
            self.custom_statuses = None;
            self.rooms.clear();
            // Timelines of rooms the snapshot no longer lists are dropped.
            if let Some(full) = &later.full {
                let listed: std::collections::HashSet<&str> = full
                    .subscriptions
                    .iter()
                    .map(|s| s.rid.as_str())
                    .chain(full.rooms.iter().map(|r| r.id.as_str()))
                    .collect();
                self.timelines
                    .retain(|t| listed.contains(t.key.rid.as_str()));
                if let Some(t) = self.threads.as_mut() {
                    t.retain_rooms(|rid| listed.contains(rid));
                }
            }
            self.full = later.full;
        }
        macro_rules! take {
            ($($f:ident),*) => { $( if later.$f.is_some() { self.$f = later.$f; } )* };
        }
        take!(
            me,
            server_info,
            settings,
            permissions,
            roles,
            custom_emoji,
            custom_statuses,
            outbox,
            interrupted_uploads,
            threads
        );
        for r in later.rooms {
            if r.1.is_none() && r.2.is_none() {
                // The room is gone: an earlier timeline or thread data must
                // not bring its messages back.
                self.timelines.retain(|t| t.key.rid != r.0);
                if let Some(t) = self.threads.as_mut() {
                    t.retain_rooms(|rid| rid != r.0);
                }
            }
            self.rooms.retain(|(rid, _, _)| *rid != r.0);
            self.rooms.push(r);
        }
        for t in later.timelines {
            self.timelines.retain(|x| x.key != t.key);
            self.timelines.push(t);
        }
    }
}
