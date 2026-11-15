//! Application configuration persisted as TOML in the XDG config directory
//! (`~/.config/rustket/config.toml` on Linux).
//!
//! Loading is tolerant field by field: a missing or invalid value (wrong type,
//! unknown enum variant from a newer version, hand-edit mistake) takes its
//! default with a warning while everything else — in particular the server list —
//! is kept; out-of-range values are sanitised. Only a file that is not TOML at
//! all is moved aside as `config.toml.broken-<unix-ts>`. Keys this version does
//! not know (written by a newer one) are preserved on save, and `version` is
//! never lowered. Saving writes a temporary file (mode 0600) next to the real
//! file (through a symlink), renames it over the old one and syncs the
//! directory. Secrets are never stored here (see [`crate::secrets`]).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::sidebar_layout::{
    GroupType, KeptThread, SectionId, SectionPrefs, SidebarGroup, ThreadsMode,
};

/// Name of the configuration file inside the config directory.
pub const CONFIG_FILE: &str = "config.toml";
/// Current schema version written by [`AppConfig::save`].
pub const CONFIG_VERSION: u32 = 1;
/// Maximum length of [`AppConfig::recent_emoji`].
pub const MAX_RECENT_EMOJI: usize = 36;
/// Maximum length of [`AppConfig::pending_secret_deletions`].
const MAX_PENDING_SECRET_DELETIONS: usize = 256;
/// Maximum length of [`AppConfig::recent_status_texts`].
pub const MAX_RECENT_STATUS_TEXTS: usize = 5;
/// Accepted range of [`UiPrefs::zoom`].
pub const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.5..=3.0;
/// Accepted range of [`UiPrefs::image_cache_mb`].
pub const IMAGE_CACHE_MB_RANGE: std::ops::RangeInclusive<u32> = 50..=20_000;
/// Default of [`UiPrefs::image_cache_mb`].
pub const DEFAULT_IMAGE_CACHE_MB: u32 = 500;
/// Default of [`UiPrefs::sidebar_threads_limit`].
pub const DEFAULT_SIDEBAR_THREADS_LIMIT: u32 = 20;
/// Largest accepted [`UiPrefs::sidebar_threads_limit`] (0, no limit, is accepted too).
pub const MAX_SIDEBAR_THREADS_LIMIT: u32 = 500;
/// Largest accepted window width/height.
const MAX_WINDOW_SIDE: u32 = 32_768;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("no home directory to place the config in")]
    NoConfigDir,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> ConfigError + '_ {
    move |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Stable, random, local identifier of a configured server. It never changes when
/// the URL or display name changes and is used in secret-store keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerId(pub String);

impl ServerId {
    pub fn new_random() -> Self {
        Self(rocketchat::ids::random_id())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A user-approved certificate pin: host and SHA-256 fingerprint (hex).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PinnedCertificate {
    pub host: String,
    pub sha256: String,
}

/// How the stored token of a server was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    /// A login token: logging out may revoke it on the server.
    Login,
    /// A personal access token: logging out only forgets it locally (revoking
    /// it would break every other place the user configured it).
    Pat,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerEntry {
    pub id: ServerId,
    /// Normalised server URL (as produced by `rocketchat::ServerUrl`).
    pub url: String,
    /// Shown in the server switcher; usually `Site_Name`.
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Kind of the stored token; `None` (unknown, older configs) means: never
    /// revoke it on the server without asking.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_token_kind"
    )]
    pub token_kind: Option<TokenKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_room: Option<String>,
    pub pinned_certificates: Vec<PinnedCertificate>,
    /// Threads the user keeps in the sidebar's Threads section, oldest first.
    /// Entries of every account that signed in here; read them through
    /// [`Self::kept_threads()`] (signed-in account only).
    #[serde(
        rename = "kept_threads",
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_vec"
    )]
    pub kept_threads_all_accounts: Vec<KeptThread>,
    /// Flag edits not known to be stored on the server yet (see
    /// [`crate::flags`]), per account; read them through
    /// [`Self::pending_flags()`].
    #[serde(
        rename = "pending_flags",
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_vec"
    )]
    pub pending_flags_all_accounts: Vec<PendingFlags>,
    /// Edits of the other synced blobs (`crate::service_sync`) not known to
    /// be stored yet, per account and key; read them through
    /// [`Self::pending_blob()`]. Flags keep `pending_flags`, which older
    /// versions read.
    #[serde(
        rename = "pending_service",
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_vec"
    )]
    pub pending_service_all_accounts: Vec<PendingBlob>,
    /// Unix seconds.
    pub added_at: i64,
    pub sort_order: i32,
}

/// An account's pending flag edits: a [`crate::flags::FlagBlob`] as JSON
/// (TOML has no `null` for its tombstones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFlags {
    pub user_id: String,
    pub blob: String,
}

/// An account's pending edits of one synced blob: its `customFields` key
/// and the blob as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBlob {
    pub user_id: String,
    pub key: String,
    pub blob: String,
}

/// A pending-edits list entry (`PendingFlags`, `PendingBlob`).
trait PendingSlot {
    fn blob_mut(&mut self) -> &mut String;
}

impl PendingSlot for PendingFlags {
    fn blob_mut(&mut self) -> &mut String {
        &mut self.blob
    }
}

impl PendingSlot for PendingBlob {
    fn blob_mut(&mut self) -> &mut String {
        &mut self.blob
    }
}

/// Puts `json` into the entry at `pos` (a new one from `new` when there is
/// none), or removes the entry when `json` is `None`. Returns whether the
/// list changed.
fn set_pending<T: PendingSlot>(
    list: &mut Vec<T>,
    pos: Option<usize>,
    json: Option<String>,
    new: impl FnOnce() -> T,
) -> bool {
    match (pos, json) {
        (Some(i), None) => {
            list.remove(i);
            true
        }
        (None, None) => false,
        (Some(i), Some(json)) => {
            let slot = list[i].blob_mut();
            if *slot == json {
                return false;
            }
            *slot = json;
            true
        }
        (None, Some(json)) => {
            let mut entry = new();
            *entry.blob_mut() = json;
            list.push(entry);
            true
        }
    }
}

impl Default for ServerId {
    fn default() -> Self {
        Self::new_random()
    }
}

/// An unknown or invalid `token_kind` reads as `None` instead of failing the
/// whole config.
fn lenient_token_kind<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<TokenKind>, D::Error> {
    lenient_opt(d)
}

/// An unknown or invalid optional value reads as `None`.
pub(crate) fn lenient_opt<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = Option::<toml::Value>::deserialize(d)?;
    Ok(v.and_then(|v| v.try_into().ok()))
}

/// A list whose invalid elements are skipped (with a warning) instead of
/// failing the whole list. A value that is not a list fails as usual.
pub(crate) fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let list = Vec::<toml::Value>::deserialize(d)?;
    Ok(list
        .into_iter()
        .filter_map(|v| {
            v.try_into()
                .map_err(|e| tracing::warn!(error = %e, "config: invalid list element ignored"))
                .ok()
        })
        .collect())
}

/// A table whose invalid values are skipped (with a warning) instead of
/// failing the whole table.
pub(crate) fn lenient_map<'de, D, T>(d: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let table = toml::Table::deserialize(d)?;
    Ok(table
        .into_iter()
        .filter_map(|(k, v)| match v.try_into() {
            Ok(v) => Some((k, v)),
            Err(e) => {
                tracing::warn!(key = %k, error = %e, "config: invalid table entry ignored");
                None
            }
        })
        .collect())
}

impl ServerEntry {
    /// Kept threads of the signed-in account (none while signed out).
    pub fn kept_threads(&self) -> impl Iterator<Item = &KeptThread> {
        let uid = self.user_id.as_deref();
        self.kept_threads_all_accounts
            .iter()
            .filter(move |k| uid.is_some_and(|u| k.user_id == u))
    }

    /// Keeps or un-keeps a thread for the signed-in account. Returns whether
    /// the list changed. Bounded by [`crate::sidebar_layout::MAX_KEPT_THREADS`]
    /// (oldest dropped first).
    pub fn set_thread_kept(&mut self, rid: &str, tmid: &str, keep: bool) -> bool {
        let Some(uid) = self.user_id.clone() else {
            return false;
        };
        let pos = self
            .kept_threads_all_accounts
            .iter()
            .position(|k| k.tmid == tmid && k.user_id == uid);
        match (pos, keep) {
            (Some(_), true) | (None, false) => false,
            (Some(i), false) => {
                self.kept_threads_all_accounts.remove(i);
                true
            }
            (None, true) => {
                self.kept_threads_all_accounts.push(KeptThread {
                    rid: rid.to_owned(),
                    tmid: tmid.to_owned(),
                    user_id: uid,
                });
                crate::sidebar_layout::sanitize_kept_threads(&mut self.kept_threads_all_accounts);
                true
            }
        }
    }

    /// Pending edits of `uid` of the synced blob `B` (empty when there are
    /// none or they do not parse).
    pub fn pending_blob<B: crate::service_sync::SyncBlob>(&self, uid: &str) -> B {
        let json = if B::KEY == rocketchat::models::FLAGS_KEY {
            self.pending_flags_all_accounts
                .iter()
                .find(|p| p.user_id == uid)
                .map(|p| p.blob.as_str())
        } else {
            self.pending_service_all_accounts
                .iter()
                .find(|p| p.user_id == uid && p.key == B::KEY)
                .map(|p| p.blob.as_str())
        };
        json.and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .map(|v| B::from_value_at(Some(&v), chrono::Utc::now().timestamp_millis()))
            .unwrap_or_default()
    }

    /// Stores the pending edits of `uid` of the synced blob `B` (an empty
    /// blob removes them). Returns whether the entry changed.
    pub fn set_pending_blob<B: crate::service_sync::SyncBlob>(
        &mut self,
        uid: &str,
        blob: &B,
    ) -> bool {
        let json = (!blob.is_empty()).then(|| {
            blob.to_value_at(chrono::Utc::now().timestamp_millis())
                .to_string()
        });
        if B::KEY == rocketchat::models::FLAGS_KEY {
            let pos = self
                .pending_flags_all_accounts
                .iter()
                .position(|p| p.user_id == uid);
            return set_pending(&mut self.pending_flags_all_accounts, pos, json, || {
                PendingFlags {
                    user_id: uid.to_owned(),
                    blob: String::new(),
                }
            });
        }
        let pos = self
            .pending_service_all_accounts
            .iter()
            .position(|p| p.user_id == uid && p.key == B::KEY);
        set_pending(&mut self.pending_service_all_accounts, pos, json, || {
            PendingBlob {
                user_id: uid.to_owned(),
                key: B::KEY.to_owned(),
                blob: String::new(),
            }
        })
    }

    /// Pending flag edits of `uid` (empty when there are none or they do
    /// not parse).
    pub fn pending_flags(&self, uid: &str) -> crate::flags::FlagBlob {
        self.pending_blob(uid)
    }

    /// Stores the pending flag edits of `uid` (an empty blob removes them).
    /// Returns whether the entry changed.
    pub fn set_pending_flags(&mut self, uid: &str, blob: &crate::flags::FlagBlob) -> bool {
        self.set_pending_blob(uid, blob)
    }

    pub fn new(url: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: ServerId::new_random(),
            url: url.into(),
            display_name: display_name.into(),
            added_at: chrono::Utc::now().timestamp(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// UI language: `"system"` (follow the environment) or a locale code — a
/// bundled one (`en`, `ru`, …) or the file stem of an external locale
/// (`de`, `pt_BR`). A code that is not available at runtime falls back to
/// the system language; a value that is not a plausible code reads as
/// `"system"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Language(String);

impl Language {
    pub const SYSTEM: &'static str = "system";
    /// Longest accepted code.
    pub const MAX_LEN: usize = 32;

    /// Follow the environment.
    pub fn system() -> Self {
        Self(Self::SYSTEM.to_owned())
    }

    /// `code` if it is `"system"` or a plausible locale code: 1..=32 ASCII
    /// letters, digits, `_` or `-`.
    pub fn new(code: &str) -> Option<Self> {
        Self::valid_code(code).then(|| Self(code.to_owned()))
    }

    pub fn valid_code(code: &str) -> bool {
        !code.is_empty()
            && code.len() <= Self::MAX_LEN
            && code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    pub fn is_system(&self) -> bool {
        self.0 == Self::SYSTEM
    }

    /// The stored value (`"system"` or a locale code).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Language {
    fn default() -> Self {
        Self::system()
    }
}

impl TryFrom<String> for Language {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s).ok_or_else(|| format!("invalid language code {s:?}"))
    }
}

impl From<Language> for String {
    fn from(l: Language) -> Self {
        l.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSort {
    /// Newest activity first.
    #[default]
    Activity,
    /// Oldest activity first; rooms without activity last.
    #[serde(rename = "activity_asc")]
    ActivityAsc,
    Alphabetical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Extended,
    Condensed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    /// Light/dark mode (or follow the system).
    pub theme: Theme,
    /// External colour theme: file stem in `<config dir>/themes/`; `None`
    /// is the built-in Rustket theme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme_name: Option<String>,
    pub language: Language,
    pub sidebar_sort: SidebarSort,
    pub group_by_type: bool,
    pub unread_on_top: bool,
    pub show_avatars: bool,
    pub density: Density,
    /// Overrides the server's `UI_Use_Real_Name` when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_real_names: Option<bool>,
    pub clock_24h: bool,
    /// Show message text in desktop notifications.
    pub notification_preview: bool,
    pub desktop_notifications: bool,
    pub sound: bool,
    pub minimize_to_tray: bool,
    /// Closing the window hides it to the tray (when a tray is available).
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub zoom: f32,
    /// Render with the GPU (wgpu); `false` restarts with the CPU renderer.
    pub hardware_acceleration: bool,
    /// Use the window manager's title bar; `false` draws Rustket's own title
    /// bar with the workspace switcher in it.
    pub system_title_bar: bool,
    /// Size cap of the image disk cache in MiB (oldest files are evicted).
    pub image_cache_mb: u32,
    /// Stored sidebar section order; empty means the default order. Missing
    /// built-ins are filled in when resolved (`sidebar_layout::effective_order`).
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_vec"
    )]
    pub sidebar_order: Vec<SectionId>,
    /// User-defined sidebar groups (supergroups of room types).
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_vec"
    )]
    pub sidebar_groups: Vec<SidebarGroup>,
    /// Per-section overrides, keyed by the `SectionId` config string.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "lenient_map"
    )]
    pub sidebar_section_prefs: BTreeMap<String, SectionPrefs>,
    pub sidebar_threads: ThreadsMode,
    /// Most threads the sidebar's Threads section shows (and loads); 0 means
    /// no limit. Clamped to [`MAX_SIDEBAR_THREADS_LIMIT`].
    pub sidebar_threads_limit: u32,
    /// Threads whose parent message the user starred show in Favorites
    /// (whatever the Threads mode is).
    pub sidebar_starred_threads: bool,
    /// Look for new releases automatically (builds with the updater only).
    pub check_updates: bool,
    /// Offer pre-releases as updates.
    pub update_prereleases: bool,
    /// Never write secrets (tokens, E2E keys) to a file: without a usable
    /// keyring they are kept in memory for the session only
    /// (`crate::secrets::FilePolicy::Never`).
    pub secrets_memory_only: bool,
    /// Escape with nothing else to close marks the open room as read (with
    /// its threads), like Slack.
    pub escape_marks_read: bool,
    /// Ask before sending `@all` / `@here` to a room with several members
    /// (also an upload's message).
    pub confirm_broad_mentions: bool,
    /// Publish the local UTC offset to the server after sign-in (as the web
    /// client does), so others see the user's local time.
    pub share_utc_offset: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            theme_name: None,
            language: Language::system(),
            sidebar_sort: SidebarSort::Activity,
            group_by_type: true,
            unread_on_top: false,
            show_avatars: true,
            density: Density::Extended,
            use_real_names: None,
            clock_24h: true,
            notification_preview: true,
            desktop_notifications: true,
            sound: true,
            minimize_to_tray: false,
            close_to_tray: true,
            start_minimized: false,
            zoom: 1.0,
            hardware_acceleration: true,
            system_title_bar: false,
            image_cache_mb: DEFAULT_IMAGE_CACHE_MB,
            sidebar_order: Vec::new(),
            sidebar_groups: Vec::new(),
            sidebar_section_prefs: BTreeMap::new(),
            sidebar_threads: ThreadsMode::Off,
            sidebar_threads_limit: DEFAULT_SIDEBAR_THREADS_LIMIT,
            sidebar_starred_threads: true,
            check_updates: true,
            update_prereleases: false,
            secrets_memory_only: false,
            escape_marks_read: true,
            confirm_broad_mentions: true,
            share_utc_offset: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowGeometry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// Parts of the loaded document this version does not understand, written back
/// on save. Not part of equality.
#[derive(Debug, Clone, Default)]
struct Unknown(Option<toml::Table>);

impl PartialEq for Unknown {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Schema version; a file from a newer version keeps its number.
    pub version: u32,
    pub servers: Vec<ServerEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_server: Option<ServerId>,
    pub ui: UiPrefs,
    /// Reaction/emoji keys (`:shortcode:`), most recent first.
    pub recent_emoji: Vec<String>,
    /// Custom status texts used recently (status menu), most recent first.
    pub recent_status_texts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowGeometry>,
    /// This installation's id, set on first use ([`Self::ensure_device_id`]):
    /// tells its synced flag edits from other devices' (`crate::flags::Stamp`).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_opt"
    )]
    pub device_id: Option<String>,
    /// Secret-store keys (names, not values) of signed-out accounts that are
    /// still to be deleted from the store: it was unreachable, the delete
    /// failed, or another backend may hold a copy. A listed key is never
    /// read back as a login ([`Self::mark_secret_deletion`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending_secret_deletions: Vec<String>,
    #[serde(skip)]
    unknown: Unknown,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            servers: Vec::new(),
            active_server: None,
            ui: UiPrefs::default(),
            recent_emoji: Vec::new(),
            recent_status_texts: Vec::new(),
            window: None,
            device_id: None,
            pending_secret_deletions: Vec::new(),
            unknown: Unknown::default(),
        }
    }
}

/// Keys of each table this version understands (the rest is preserved).
const APP_KEYS: &[&str] = &[
    "version",
    "servers",
    "active_server",
    "ui",
    "recent_emoji",
    "recent_status_texts",
    "window",
    "device_id",
    "pending_secret_deletions",
];
const UI_KEYS: &[&str] = &[
    "theme",
    "theme_name",
    "language",
    "sidebar_sort",
    "group_by_type",
    "unread_on_top",
    "show_avatars",
    "density",
    "use_real_names",
    "clock_24h",
    "notification_preview",
    "desktop_notifications",
    "sound",
    "minimize_to_tray",
    "close_to_tray",
    "start_minimized",
    "zoom",
    "hardware_acceleration",
    "system_title_bar",
    "image_cache_mb",
    "sidebar_order",
    "sidebar_groups",
    "sidebar_section_prefs",
    "sidebar_threads",
    "sidebar_threads_limit",
    "sidebar_starred_threads",
    "check_updates",
    "update_prereleases",
    "secrets_memory_only",
    "escape_marks_read",
    "confirm_broad_mentions",
    "share_utc_offset",
];
const SERVER_KEYS: &[&str] = &[
    "id",
    "url",
    "display_name",
    "user_id",
    "username",
    "token_kind",
    "last_room",
    "pinned_certificates",
    "kept_threads",
    "pending_flags",
    "pending_service",
    "added_at",
    "sort_order",
];
const WINDOW_KEYS: &[&str] = &["x", "y", "width", "height", "maximized"];

/// Deserializes a `#[serde(default)]` struct key by key: a key whose value does
/// not fit keeps the default (with a warning) instead of failing the whole table.
fn lenient<T: serde::de::DeserializeOwned + Default>(t: &toml::Table, what: &str) -> T {
    if let Ok(v) = t.clone().try_into() {
        return v;
    }
    let mut good = toml::Table::new();
    for (k, v) in t {
        let mut probe = good.clone();
        probe.insert(k.clone(), v.clone());
        match probe.try_into::<T>() {
            Ok(_) => {
                good.insert(k.clone(), v.clone());
            }
            Err(e) => {
                tracing::warn!(key = %format!("{what}.{k}"), error = %e, "config: invalid value ignored")
            }
        }
    }
    good.try_into().unwrap_or_default()
}

/// Copies keys of `raw` that are not in `known` (and not already present) into `out`.
fn merge_unknown(out: &mut toml::Table, raw: &toml::Table, known: &[&str]) {
    for (k, v) in raw {
        if !known.contains(&k.as_str()) && !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
}

/// Upgrades an older document in place. Each step bumps `version` by one.
/// Version 0 (no `version` key) has the same layout as version 1.
fn migrate(doc: &mut toml::Table) {
    let mut v = doc
        .get("version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(0);
    while v < i64::from(CONFIG_VERSION) {
        // Future steps: `if v == 1 { rename fields ... }`
        v += 1;
    }
    doc.insert("version".into(), toml::Value::Integer(v));
}

impl AppConfig {
    /// `$XDG_CONFIG_HOME/rustket` (platform equivalent elsewhere).
    pub fn default_dir() -> Result<PathBuf, ConfigError> {
        directories::ProjectDirs::from("", "", "rustket")
            .map(|d| d.config_dir().to_owned())
            .ok_or(ConfigError::NoConfigDir)
    }

    /// Loads `dir/config.toml`. A missing file yields defaults; an unparsable one
    /// is renamed to `config.toml.broken-<ts>` and defaults are returned.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                Self::move_aside(&path, "not UTF-8")?;
                return Ok(Self::default());
            }
            Err(e) => return Err(io_err(&path)(e)),
        };
        match Self::parse_inner(&text) {
            Ok((c, dirty)) => {
                if dirty && let Err(e) = c.save(dir) {
                    tracing::warn!(error = %e, "config: cannot write back repaired server ids");
                }
                Ok(c)
            }
            Err(reason) => {
                Self::move_aside(&path, &reason)?;
                Ok(Self::default())
            }
        }
    }

    /// Parses a TOML document, running migrations first. Fails only when the
    /// text is not TOML; invalid values fall back to their defaults.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::parse_inner(text).map(|(cfg, _)| cfg)
    }

    /// Like [`Self::parse`]; also reports whether sanitising changed something
    /// that must be written back (generated server ids).
    fn parse_inner(text: &str) -> Result<(Self, bool), String> {
        let mut doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        if doc
            .get("version")
            .and_then(toml::Value::as_integer)
            .is_some_and(|v| v > i64::from(CONFIG_VERSION))
        {
            tracing::warn!(
                "config was written by a newer Rustket; unknown fields are kept but not used"
            );
        }
        migrate(&mut doc);
        Ok(Self::from_table(doc))
    }

    fn from_table(doc: toml::Table) -> (Self, bool) {
        let mut cfg = Self::default();
        let mut dirty = false;
        cfg.version = doc
            .get("version")
            .and_then(toml::Value::as_integer)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(CONFIG_VERSION)
            .max(CONFIG_VERSION);
        match doc.get("servers") {
            Some(toml::Value::Array(list)) => {
                for v in list {
                    let Some(t) = v.as_table() else {
                        tracing::warn!("config: ignoring a server entry that is not a table");
                        continue;
                    };
                    let mut entry: ServerEntry = lenient(t, "servers");
                    let has_id = t
                        .get("id")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|id| !id.trim().is_empty());
                    if !has_id {
                        entry.id = ServerId::new_random();
                        dirty = true;
                    }
                    cfg.servers.push(entry);
                }
            }
            Some(_) => tracing::warn!("config: `servers` is not a list; ignored"),
            None => {}
        }
        cfg.active_server = doc
            .get("active_server")
            .and_then(toml::Value::as_str)
            .map(|s| ServerId(s.to_owned()));
        cfg.device_id = doc
            .get("device_id")
            .and_then(toml::Value::as_str)
            .filter(|d| crate::flags::valid_id(d))
            .map(str::to_owned);
        if let Some(t) = doc.get("ui").and_then(toml::Value::as_table) {
            cfg.ui = lenient(t, "ui");
        }
        if let Some(list) = doc.get("recent_emoji").and_then(toml::Value::as_array) {
            cfg.recent_emoji = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(list) = doc
            .get("recent_status_texts")
            .and_then(toml::Value::as_array)
        {
            cfg.recent_status_texts = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(t) = doc.get("window").and_then(toml::Value::as_table) {
            cfg.window = Some(lenient(t, "window"));
        }
        if let Some(list) = doc
            .get("pending_secret_deletions")
            .and_then(toml::Value::as_array)
        {
            for key in list.iter().filter_map(toml::Value::as_str) {
                cfg.mark_secret_deletion(key);
            }
        }
        dirty |= cfg.sanitize();
        cfg.unknown = Unknown(Some(doc));
        (cfg, dirty)
    }

    /// Repairs out-of-range values. Returns whether server ids changed (the file
    /// must then be saved so the ids stay stable).
    fn sanitize(&mut self) -> bool {
        let zoom = self.ui.zoom;
        if !zoom.is_finite() || zoom <= 0.0 {
            tracing::warn!(zoom, "config: invalid zoom reset");
            self.ui.zoom = UiPrefs::default().zoom;
        } else {
            self.ui.zoom = zoom.clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
        }
        self.ui.image_cache_mb = self
            .ui
            .image_cache_mb
            .clamp(*IMAGE_CACHE_MB_RANGE.start(), *IMAGE_CACHE_MB_RANGE.end());
        if self.window.is_some_and(|w| {
            w.width == 0 || w.height == 0 || w.width > MAX_WINDOW_SIDE || w.height > MAX_WINDOW_SIDE
        }) {
            tracing::warn!("config: invalid window geometry ignored");
            self.window = None;
        }
        if self.ui.sidebar_threads_limit > MAX_SIDEBAR_THREADS_LIMIT {
            self.ui.sidebar_threads_limit = MAX_SIDEBAR_THREADS_LIMIT;
        }
        crate::sidebar_layout::sanitize_prefs(&mut self.ui);
        for s in &mut self.servers {
            crate::sidebar_layout::sanitize_kept_threads(&mut s.kept_threads_all_accounts);
        }
        self.recent_emoji.truncate(MAX_RECENT_EMOJI);
        let texts = std::mem::take(&mut self.recent_status_texts);
        self.set_recent_status_texts(texts);
        let mut changed = false;
        let mut seen = std::collections::HashSet::new();
        for s in &mut self.servers {
            while !seen.insert(s.id.clone()) {
                tracing::warn!(id = %s.id, "config: duplicate server id replaced");
                s.id = ServerId::new_random();
                changed = true;
            }
        }
        if self
            .active_server
            .as_ref()
            .is_some_and(|id| self.server(id).is_none())
        {
            self.active_server = self.sorted_servers().first().map(|s| s.id.clone());
        }
        changed
    }

    fn move_aside(path: &Path, reason: &str) -> Result<(), ConfigError> {
        let ts = chrono::Utc::now().timestamp();
        let mut broken = path.as_os_str().to_owned();
        broken.push(format!(".broken-{ts}"));
        let broken = PathBuf::from(broken);
        tracing::warn!(path = %path.display(), moved_to = %broken.display(), %reason, "config file is corrupted; using defaults");
        fs::rename(path, &broken).map_err(io_err(path))
    }

    /// Writes `dir/config.toml` atomically (temp file + rename, mode 0600),
    /// keeping keys a newer version wrote.
    pub fn save(&self, dir: &Path) -> Result<(), ConfigError> {
        let mut out = toml::Table::try_from(self)?;
        if let Some(raw) = self.unknown.0.as_ref() {
            merge_unknown(&mut out, raw, APP_KEYS);
            for (key, known) in [("ui", UI_KEYS), ("window", WINDOW_KEYS)] {
                if let (Some(toml::Value::Table(o)), Some(toml::Value::Table(r))) =
                    (out.get_mut(key), raw.get(key))
                {
                    merge_unknown(o, r, known);
                }
            }
            if let (Some(toml::Value::Array(o)), Some(toml::Value::Array(r))) =
                (out.get_mut("servers"), raw.get("servers"))
            {
                for entry in o.iter_mut().filter_map(toml::Value::as_table_mut) {
                    let id = entry.get("id").cloned();
                    if let Some(raw_entry) = r
                        .iter()
                        .filter_map(toml::Value::as_table)
                        .find(|t| t.get("id") == id.as_ref())
                    {
                        merge_unknown(entry, raw_entry, SERVER_KEYS);
                    }
                }
            }
        }
        let text = toml::to_string_pretty(&out)?;
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        write_atomic(&dir.join(CONFIG_FILE), text.as_bytes())
    }

    /// This installation's device id, created on first use. Returns it and
    /// whether it was just created (the config needs saving).
    pub fn ensure_device_id(&mut self) -> (String, bool) {
        match &self.device_id {
            Some(d) if crate::flags::valid_id(d) => (d.clone(), false),
            _ => {
                let d = rocketchat::ids::random_id();
                self.device_id = Some(d.clone());
                (d, true)
            }
        }
    }

    pub fn server(&self, id: &ServerId) -> Option<&ServerEntry> {
        self.servers.iter().find(|s| &s.id == id)
    }

    pub fn server_mut(&mut self, id: &ServerId) -> Option<&mut ServerEntry> {
        self.servers.iter_mut().find(|s| &s.id == id)
    }

    /// Adds a server at the end of the list and returns its id.
    pub fn add_server(&mut self, mut entry: ServerEntry) -> ServerId {
        entry.sort_order = self
            .servers
            .iter()
            .map(|s| s.sort_order)
            .max()
            .map_or(0, |m| m + 1);
        let id = entry.id.clone();
        self.servers.push(entry);
        if self.active_server.is_none() {
            self.active_server = Some(id.clone());
        }
        id
    }

    /// Records that secret `key` must be deleted from the secret store and
    /// never be restored. Returns whether the list changed.
    pub fn mark_secret_deletion(&mut self, key: &str) -> bool {
        if key.is_empty() || self.pending_secret_deletions.iter().any(|k| k == key) {
            return false;
        }
        // Bounded: the oldest entry gives way.
        if self.pending_secret_deletions.len() >= MAX_PENDING_SECRET_DELETIONS {
            self.pending_secret_deletions.remove(0);
        }
        self.pending_secret_deletions.push(key.to_owned());
        true
    }

    /// Drops `keys` from the pending deletions (deleted for good, or written
    /// again by a new login). Returns whether the list changed.
    pub fn clear_secret_deletions(&mut self, keys: &[String]) -> bool {
        let before = self.pending_secret_deletions.len();
        self.pending_secret_deletions.retain(|k| !keys.contains(k));
        self.pending_secret_deletions.len() != before
    }

    pub fn is_secret_deletion_pending(&self, key: &str) -> bool {
        self.pending_secret_deletions.iter().any(|k| k == key)
    }

    pub fn remove_server(&mut self, id: &ServerId) -> Option<ServerEntry> {
        let pos = self.servers.iter().position(|s| &s.id == id)?;
        let removed = self.servers.remove(pos);
        if self.active_server.as_ref() == Some(id) {
            self.active_server = self.sorted_servers().first().map(|s| s.id.clone());
        }
        Some(removed)
    }

    /// Servers in display order.
    pub fn sorted_servers(&self) -> Vec<&ServerEntry> {
        let mut v: Vec<&ServerEntry> = self.servers.iter().collect();
        v.sort_by_key(|s| (s.sort_order, s.added_at));
        v
    }

    /// Records a used emoji (most recent first, deduplicated, bounded).
    pub fn push_recent_emoji(&mut self, key: &str) {
        crate::emoji::push_recent(&mut self.recent_emoji, key, MAX_RECENT_EMOJI);
    }

    /// Replaces the recent status texts: trimmed, without blanks and
    /// duplicates, at most [`MAX_RECENT_STATUS_TEXTS`].
    pub fn set_recent_status_texts(&mut self, texts: Vec<String>) {
        let mut out: Vec<String> = Vec::new();
        for t in texts {
            let t = t.trim();
            if !t.is_empty() && !out.iter().any(|o| o == t) {
                out.push(t.to_owned());
            }
        }
        out.truncate(MAX_RECENT_STATUS_TEXTS);
        self.recent_status_texts = out;
    }
}

/// Where a write to `path` must land: the target of a symlink (so the link
/// survives), else `path` itself.
pub(crate) fn write_target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| {
            // Dangling link: follow it by hand.
            match fs::read_link(path) {
                Ok(t) if t.is_absolute() => t,
                Ok(t) => path.parent().unwrap_or(Path::new(".")).join(t),
                Err(_) => path.to_owned(),
            }
        }),
        _ => path.to_owned(),
    }
}

/// Writes `data` to `path` through a temporary sibling file and a rename, then
/// syncs the directory. A symlinked `path` is written through to its target. On
/// Unix the file is created with mode 0600.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ConfigError> {
    let target = write_target(path);
    let path = target.as_path();
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", rocketchat::ids::random_id_len(8)));
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        // Make the rename itself durable.
        #[cfg(unix)]
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_err(path))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn token_kind_is_lenient() {
        let mut cfg = AppConfig::default();
        let mut e = ServerEntry::new("https://a.example", "A");
        e.token_kind = Some(TokenKind::Pat);
        cfg.add_server(e);
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("token_kind = \"pat\""));
        assert_eq!(
            AppConfig::parse(&text).unwrap().servers[0].token_kind,
            Some(TokenKind::Pat)
        );
        let bad = text.replace("token_kind = \"pat\"", "token_kind = \"weird\"");
        let parsed = AppConfig::parse(&bad).unwrap();
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].token_kind, None);
        let none = text.replace("token_kind = \"pat\"\n", "");
        assert_eq!(AppConfig::parse(&none).unwrap().servers[0].token_kind, None);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = AppConfig::load(dir.path()).unwrap();
        assert_eq!(c, AppConfig::default());
    }

    #[test]
    fn close_to_tray_defaults_on_and_parses() {
        assert!(AppConfig::default().ui.close_to_tray);
        let c = AppConfig::parse("[ui]\nclose_to_tray = false\n").unwrap();
        assert!(!c.ui.close_to_tray);
        let c = AppConfig::parse("[ui]\nclose_to_tray = \"maybe\"\n").unwrap();
        assert!(c.ui.close_to_tray, "invalid value keeps the default");
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = AppConfig::default();
        let mut s = ServerEntry::new("https://chat.example.com", "Example");
        s.user_id = Some("u1".into());
        s.username = Some("alice".into());
        s.pinned_certificates.push(PinnedCertificate {
            host: "chat.example.com".into(),
            sha256: "ab".repeat(32),
        });
        let id = c.add_server(s);
        c.ui.theme = Theme::Dark;
        c.ui.language = Language::new("ru").unwrap();
        c.ui.use_real_names = Some(true);
        c.ui.zoom = 1.25;
        c.push_recent_emoji(":thumbsup:");
        c.window = Some(WindowGeometry {
            x: Some(10),
            y: None,
            width: 1200,
            height: 800,
            maximized: false,
        });
        c.save(dir.path()).unwrap();
        let loaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(loaded.active_server, Some(id));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(CONFIG_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temp files left behind.
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn tolerant_of_missing_and_unknown_fields() {
        let c = AppConfig::parse(
            r#"
            future_thing = 3
            [ui]
            theme = "dark"
            something_new = true
            [[servers]]
            id = "abc"
            url = "https://x.example"
            extra = 1
            "#,
        )
        .unwrap();
        assert_eq!(c.ui.theme, Theme::Dark);
        assert!(c.ui.desktop_notifications);
        assert_eq!(c.servers[0].id, ServerId("abc".into()));
        assert_eq!(c.servers[0].url, "https://x.example");
        assert_eq!(c.version, CONFIG_VERSION);
    }

    #[test]
    fn theme_name_is_optional() {
        let c = AppConfig::parse("[ui]\ntheme = \"dark\"\n").unwrap();
        assert_eq!(c.ui.theme_name, None);
        assert!(!toml::to_string(&c).unwrap().contains("theme_name"));
        let c = AppConfig::parse("[ui]\ntheme = \"light\"\ntheme_name = \"forest\"\n").unwrap();
        assert_eq!(c.ui.theme, Theme::Light);
        assert_eq!(c.ui.theme_name.as_deref(), Some("forest"));
        let again = AppConfig::parse(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(again, c);
        // A wrong type keeps the built-in theme.
        let c = AppConfig::parse("[ui]\ntheme_name = 3\n").unwrap();
        assert_eq!(c.ui.theme_name, None);
    }

    #[test]
    fn corrupted_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONFIG_FILE), "this is [not toml").unwrap();
        let c = AppConfig::load(dir.path()).unwrap();
        assert_eq!(c, AppConfig::default());
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].starts_with("config.toml.broken-"), "{names:?}");
    }

    #[test]
    fn wrong_types_fall_back_per_field() {
        let c = AppConfig::parse("servers = 5\nrecent_emoji = [1, \":joy:\"]").unwrap();
        assert!(c.servers.is_empty());
        assert_eq!(c.recent_emoji, [":joy:"]);
        assert!(AppConfig::parse("not = [toml").is_err());
    }

    #[test]
    fn language_codes() {
        let lang = |text: &str| AppConfig::parse(text).unwrap().ui.language;
        // Values of the old enum read the same.
        assert!(lang("").is_system());
        assert!(lang("[ui]\nlanguage = \"system\"").is_system());
        assert_eq!(lang("[ui]\nlanguage = \"en\"").as_str(), "en");
        assert_eq!(lang("[ui]\nlanguage = \"ru\"").as_str(), "ru");
        // Any plausible code is kept (external locales).
        assert_eq!(lang("[ui]\nlanguage = \"pt_BR\"").as_str(), "pt_BR");
        assert_eq!(lang("[ui]\nlanguage = \"zh-Hant\"").as_str(), "zh-Hant");
        // Not a code: the default, the rest of [ui] kept.
        for bad in [
            "\"\"",
            "\"../de\"",
            "\"a b\"",
            "5",
            &format!("\"{}\"", "x".repeat(33)),
        ] {
            let c = AppConfig::parse(&format!("[ui]\nlanguage = {bad}\nzoom = 1.5")).unwrap();
            assert!(c.ui.language.is_system(), "{bad}");
            assert!((c.ui.zoom - 1.5).abs() < 1e-6);
        }
        // Saved as a plain string.
        let mut c = AppConfig::default();
        c.ui.language = Language::new("pt_BR").unwrap();
        let doc: toml::Table = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(doc["ui"]["language"].as_str(), Some("pt_BR"));
        assert_eq!(
            AppConfig::default().ui.language,
            Language::new("system").unwrap()
        );
    }

    fn saved_with_two_servers(dir: &Path) -> AppConfig {
        let mut c = AppConfig::default();
        c.add_server(ServerEntry::new("https://a.example", "A"));
        c.add_server(ServerEntry::new("https://b.example", "B"));
        c.push_recent_emoji(":joy:");
        c.save(dir).unwrap();
        c
    }

    #[test]
    fn one_bad_value_keeps_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let orig = saved_with_two_servers(dir.path());
        let path = dir.path().join(CONFIG_FILE);
        let text = fs::read_to_string(&path).unwrap();
        for (from, to) in [
            ("theme = \"system\"", "theme = \"high-contrast\""),
            ("language = \"system\"", "language = \"../de\""),
            ("sound = true", "sound = \"yes\""),
            ("added_at = ", "added_at = \"soon\"\nx_old_added_at = "),
        ] {
            assert!(text.contains(from), "{from}");
            fs::write(&path, text.replacen(from, to, 1)).unwrap();
            let c = AppConfig::load(dir.path()).unwrap();
            assert_eq!(c.servers.len(), 2, "{to}");
            assert_eq!(c.servers[0].id, orig.servers[0].id);
            assert_eq!(c.recent_emoji, [":joy:"]);
            assert_eq!(c.ui.theme, Theme::System);
            assert_eq!(c.active_server, orig.active_server);
        }
        // Nothing was moved aside.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn newer_fields_and_version_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        saved_with_two_servers(dir.path());
        let path = dir.path().join(CONFIG_FILE);
        let text = fs::read_to_string(&path).unwrap().replace(
            "version = 1",
            "version = 2\nproxy = \"socks5://x\"\n[future]\nflag = true",
        );
        let text = text.replacen("zoom = 1.0", "zoom = 1.0\naccent = \"teal\"", 1);
        let text = text.replacen("sort_order = 0", "sort_order = 0\nnickname = \"work\"", 1);
        fs::write(&path, text).unwrap();
        let mut c = AppConfig::load(dir.path()).unwrap();
        assert_eq!(c.version, 2);
        c.ui.zoom = 1.5;
        c.servers[0].user_id = Some("u1".into());
        c.save(dir.path()).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let doc: toml::Table = toml::from_str(&saved).unwrap();
        assert_eq!(doc["version"].as_integer(), Some(2));
        assert_eq!(doc["proxy"].as_str(), Some("socks5://x"));
        assert_eq!(doc["future"]["flag"].as_bool(), Some(true));
        assert_eq!(doc["ui"]["accent"].as_str(), Some("teal"));
        assert_eq!(doc["ui"]["zoom"].as_float(), Some(1.5));
        assert_eq!(doc["servers"][0]["nickname"].as_str(), Some("work"));
        assert!(doc["servers"][1].get("nickname").is_none());
        // Known keys removed in memory are not resurrected.
        c.servers[0].user_id = None;
        c.save(dir.path()).unwrap();
        let doc: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(doc["servers"][0].get("user_id").is_none());
    }

    #[test]
    fn known_key_lists_match_the_structs() {
        let mut c = AppConfig::default();
        let mut s = ServerEntry::new("https://a", "A");
        s.user_id = Some("u".into());
        s.username = Some("n".into());
        s.last_room = Some("r".into());
        s.token_kind = Some(TokenKind::Login);
        s.set_thread_kept("r", "t", true);
        let mut pending = crate::flags::FlagBlob::default();
        pending
            .edit(
                &crate::flags::FlagBlob::default(),
                &crate::flags::FlagEdit::Room {
                    rid: "r".into(),
                    color: Some(crate::flags::FlagColor::Red),
                },
                1,
                "d",
            )
            .unwrap();
        s.set_pending_flags("u", &pending);
        s.set_pending_blob(
            "u",
            &crate::service_sync::tests::toy(&[("a", Some("1"), 1)]),
        );
        c.add_server(s);
        c.ensure_device_id();
        c.mark_secret_deletion("rustket:s:u");
        c.ui.use_real_names = Some(true);
        c.ui.theme_name = Some("forest".into());
        crate::sidebar_layout::apply(
            &mut c.ui,
            crate::sidebar_layout::LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels],
            },
        );
        c.ui.sidebar_section_prefs.insert(
            "channels".into(),
            SectionPrefs {
                sort: Some(SidebarSort::Alphabetical),
                unread_first: None,
            },
        );
        assert!(!c.ui.sidebar_order.is_empty());
        c.window = Some(WindowGeometry {
            x: Some(1),
            y: Some(2),
            width: 3,
            height: 4,
            maximized: true,
        });
        let t = toml::Table::try_from(&c).unwrap();
        let keys = |t: &toml::Table| {
            let mut k: Vec<String> = t.keys().cloned().collect();
            k.sort();
            k
        };
        let sorted = |l: &[&str]| {
            let mut k: Vec<String> = l.iter().map(|s| (*s).to_owned()).collect();
            k.sort();
            k
        };
        assert_eq!(keys(&t), sorted(APP_KEYS));
        assert_eq!(keys(t["ui"].as_table().unwrap()), sorted(UI_KEYS));
        assert_eq!(keys(t["window"].as_table().unwrap()), sorted(WINDOW_KEYS));
        assert_eq!(
            keys(t["servers"][0].as_table().unwrap()),
            sorted(SERVER_KEYS)
        );
    }

    #[test]
    fn pending_flags_are_per_account_and_survive_a_save() {
        use crate::flags::{FlagBlob, FlagColor, FlagEdit};
        let mut s = ServerEntry::new("https://a", "A");
        let mut b = FlagBlob::default();
        let room = |c| FlagEdit::Room {
            rid: "r1".into(),
            color: c,
        };
        b.edit(&FlagBlob::default(), &room(Some(FlagColor::Blue)), 5, "d")
            .unwrap();
        // A removal is a tombstone (null), which TOML cannot hold directly.
        let mut gone = FlagBlob::default();
        gone.edit(&b, &room(None), 6, "d").unwrap();
        assert!(s.set_pending_flags("u1", &b));
        assert!(!s.set_pending_flags("u1", &b));
        assert!(s.set_pending_flags("u2", &gone));
        let mut c = AppConfig::default();
        c.add_server(s);
        let (dev, created) = c.ensure_device_id();
        assert!(created && crate::flags::valid_id(&dev));
        assert_eq!(c.ensure_device_id(), (dev.clone(), false));
        let text = toml::to_string(&c).unwrap();
        let mut c = AppConfig::parse(&text).unwrap();
        assert_eq!(c.device_id.as_deref(), Some(dev.as_str()));
        let s = &mut c.servers[0];
        assert_eq!(s.pending_flags("u1").to_value(), b.to_value());
        assert_eq!(s.pending_flags("u2").to_value(), gone.to_value());
        assert!(s.pending_flags("u2").view().room("r1").is_none());
        assert!(s.pending_flags("u3").is_empty());
        assert!(s.set_pending_flags("u1", &FlagBlob::default()));
        assert!(!s.set_pending_flags("u1", &FlagBlob::default()));
        assert_eq!(s.pending_flags_all_accounts.len(), 1);
        // Junk entries and an unparsable blob are ignored.
        let c = AppConfig::parse(
            r#"
            device_id = 5
            [[servers]]
            id = "s"
            pending_flags = [{ user_id = "u", blob = "not json" }, "junk"]
            "#,
        )
        .unwrap();
        assert_eq!(c.device_id, None);
        assert!(c.servers[0].pending_flags("u").is_empty());
    }

    #[test]
    fn pending_service_blobs_are_per_account_and_key() {
        use crate::service_sync::SyncBlob;
        use crate::service_sync::tests::{Toy, toy};
        let mut s = ServerEntry::new("https://a", "A");
        // Recent enough for the toy's tombstone TTL; older ones are not saved.
        let now = chrono::Utc::now().timestamp_millis();
        let a = toy(&[("a", Some("1"), 5), ("gone", None, now)]);
        let b = toy(&[("b", Some("2"), 7)]);
        assert!(s.set_pending_blob("u1", &a));
        assert!(!s.set_pending_blob("u1", &a));
        assert!(s.set_pending_blob("u2", &b));
        // Flags stay in their own list, other keys do not touch it.
        let mut flags = crate::flags::FlagBlob::default();
        flags
            .edit(
                &crate::flags::FlagBlob::default(),
                &crate::flags::FlagEdit::Room {
                    rid: "r1".into(),
                    color: Some(crate::flags::FlagColor::Red),
                },
                1,
                "d",
            )
            .unwrap();
        assert!(s.set_pending_blob("u1", &flags));
        assert_eq!(s.pending_flags_all_accounts.len(), 1);
        assert_eq!(s.pending_service_all_accounts.len(), 2);
        let mut c = AppConfig::default();
        c.add_server(s);
        let mut c = AppConfig::parse(&toml::to_string(&c).unwrap()).unwrap();
        let s = &mut c.servers[0];
        assert_eq!(s.pending_blob::<Toy>("u1"), a, "tombstones survive TOML");
        let mut stale = ServerEntry::new("https://a", "A");
        stale.set_pending_blob("u1", &toy(&[("a", Some("1"), 5), ("gone", None, 6)]));
        assert_eq!(stale.pending_blob::<Toy>("u1"), toy(&[("a", Some("1"), 5)]));
        assert_eq!(s.pending_blob::<Toy>("u2"), b);
        assert!(s.pending_blob::<Toy>("u3").is_empty());
        assert_eq!(s.pending_flags("u1").to_value(), flags.to_value());
        assert!(s.set_pending_blob("u1", &Toy::default()));
        assert!(!s.set_pending_blob("u1", &Toy::default()));
        assert_eq!(s.pending_service_all_accounts.len(), 1);
        assert_eq!(s.pending_flags_all_accounts.len(), 1);
        // Junk is ignored.
        let c = AppConfig::parse(
            r#"
            [[servers]]
            id = "s"
            pending_service = [{ user_id = "u", key = "rustketToy", blob = "not json" }, "junk", { user_id = "u" }]
            "#,
        )
        .unwrap();
        assert!(c.servers[0].pending_blob::<Toy>("u").is_empty());
        assert_eq!(c.servers[0].pending_service_all_accounts.len(), 1);
    }

    #[test]
    fn pending_mention_groups_survive_toml() {
        use crate::mention_groups::{MentionGroupBlob, MentionGroupEdit};
        let mut blob = MentionGroupBlob::default();
        let now = chrono::Utc::now().timestamp_millis();
        let edit = MentionGroupEdit::Upsert {
            id: "g1".into(),
            name: "backend".into(),
            members: vec!["alice".into(), "bob".into()],
        };
        blob.edit(&MentionGroupBlob::default(), &edit, now, "d")
            .unwrap();
        let gone = MentionGroupEdit::Delete { id: "g1".into() };
        let mut deleted = blob.clone();
        deleted
            .edit(&MentionGroupBlob::default(), &gone, now, "d")
            .unwrap();
        let mut s = ServerEntry::new("https://a", "A");
        assert!(s.set_pending_blob("u1", &deleted));
        assert!(s.pending_flags_all_accounts.is_empty());
        let mut c = AppConfig::default();
        c.add_server(s);
        let c = AppConfig::parse(&toml::to_string(&c).unwrap()).unwrap();
        let read: MentionGroupBlob = c.servers[0].pending_blob("u1");
        assert_eq!(read.groups, deleted.groups, "the removal survives");
    }

    fn with_sidebar_layout() -> AppConfig {
        use crate::sidebar_layout::{LayoutEdit, apply};
        let mut c = AppConfig::default();
        let mut s = ServerEntry::new("https://a", "A");
        s.user_id = Some("u".into());
        s.set_thread_kept("r1", "t1", true);
        c.add_server(s);
        apply(
            &mut c.ui,
            LayoutEdit::AddGroup {
                name: "Work".into(),
                types: vec![GroupType::Channels, GroupType::DirectMessages],
            },
        );
        apply(
            &mut c.ui,
            LayoutEdit::Move {
                id: SectionId::Builtin(crate::room_list::SectionKind::Threads),
                delta: -2,
            },
        );
        apply(
            &mut c.ui,
            LayoutEdit::SetSort {
                id: SectionId::Builtin(crate::room_list::SectionKind::Teams),
                sort: Some(SidebarSort::ActivityAsc),
            },
        );
        apply(
            &mut c.ui,
            LayoutEdit::SetThreadsMode(ThreadsMode::UnreadKept),
        );
        apply(
            &mut c.ui,
            LayoutEdit::SetDefaultSort(SidebarSort::ActivityAsc),
        );
        apply(&mut c.ui, LayoutEdit::SetThreadsLimit(50));
        c
    }

    #[test]
    fn sidebar_layout_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let c = with_sidebar_layout();
        c.save(dir.path()).unwrap();
        let text = fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("sidebar_sort = \"activity_asc\""), "{text}");
        assert!(text.contains("sidebar_threads = \"unread_kept\""), "{text}");
        assert!(text.contains("sidebar_threads_limit = 50"), "{text}");
        let loaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(loaded, c);
        assert_eq!(loaded.servers[0].kept_threads().count(), 1);
        // Defaults write none of the optional sidebar keys.
        let text = toml::to_string(&AppConfig::default()).unwrap();
        for k in ["sidebar_order", "sidebar_groups", "sidebar_section_prefs"] {
            assert!(!text.contains(k), "{k}");
        }
    }

    #[test]
    fn sidebar_lists_skip_bad_elements() {
        let c = AppConfig::parse(
            r#"
            [ui]
            sidebar_sort = "weird"
            unread_on_top = true
            sidebar_threads = "everything"
            sidebar_order = ["future_section", "channels", 3, "group:b"]
            sidebar_groups = [
                { id = "a", name = "A", types = ["teams"] },
                5,
                { id = "b", name = "B", types = ["channels", "omnichannel", "future"] },
                { id = "c", name = 7 },
            ]
            [ui.sidebar_section_prefs]
            channels = { sort = "weird", unread_first = true }
            teams = { sort = "alphabetical" }
            broken = 4
            [[servers]]
            id = "s"
            user_id = "u"
            kept_threads = [{ rid = "r", tmid = "t", user_id = "u" }, "junk"]
            "#,
        )
        .unwrap();
        let ui = &c.ui;
        assert_eq!(
            ui.sidebar_sort,
            SidebarSort::Activity,
            "only that key falls back"
        );
        assert!(ui.unread_on_top);
        assert_eq!(ui.sidebar_threads, ThreadsMode::Off);
        let groups: Vec<(&str, &[GroupType])> = ui
            .sidebar_groups
            .iter()
            .map(|g| (g.id.as_str(), g.types.as_slice()))
            .collect();
        assert_eq!(
            groups,
            [
                ("a", &[GroupType::Teams][..]),
                ("b", &[GroupType::Channels][..])
            ]
        );
        assert_eq!(
            ui.sidebar_order,
            [
                SectionId::Unknown("future_section".into()),
                SectionId::Builtin(crate::room_list::SectionKind::Channels),
                SectionId::group("b"),
            ]
        );
        assert!(
            !crate::sidebar_layout::effective_order(ui)
                .iter()
                .any(SectionId::is_unknown)
        );
        assert_eq!(ui.sidebar_section_prefs.len(), 2);
        assert_eq!(ui.sidebar_section_prefs["channels"].sort, None);
        assert_eq!(
            ui.sidebar_section_prefs["channels"].unread_first,
            Some(true)
        );
        assert_eq!(
            ui.sidebar_section_prefs["teams"].sort,
            Some(SidebarSort::Alphabetical)
        );
        assert_eq!(c.servers[0].kept_threads().count(), 1);
        // The unknown order id survives a save.
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("future_section"));
        let c = AppConfig::parse("[ui]\nsidebar_sort = \"activity_asc\"").unwrap();
        assert_eq!(c.ui.sidebar_sort, SidebarSort::ActivityAsc);
    }

    #[test]
    fn sidebar_starred_threads_defaults_on() {
        assert!(UiPrefs::default().sidebar_starred_threads);
        let c = AppConfig::parse("[ui]\nunread_on_top = true").unwrap();
        assert!(c.ui.sidebar_starred_threads);
        let mut c = AppConfig::default();
        c.ui.sidebar_starred_threads = false;
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("sidebar_starred_threads = false"), "{text}");
        assert!(!AppConfig::parse(&text).unwrap().ui.sidebar_starred_threads);
    }

    #[test]
    fn sidebar_threads_limit_defaults_and_bounds() {
        assert_eq!(
            UiPrefs::default().sidebar_threads_limit,
            DEFAULT_SIDEBAR_THREADS_LIMIT
        );
        let limit = |v: &str| {
            AppConfig::parse(&format!(
                "[ui]\nunread_on_top = true\nsidebar_threads_limit = {v}"
            ))
            .unwrap()
            .ui
        };
        for (v, want) in [
            ("0", 0),
            ("7", 7),
            ("9999", MAX_SIDEBAR_THREADS_LIMIT),
            ("-3", DEFAULT_SIDEBAR_THREADS_LIMIT),
            ("\"many\"", DEFAULT_SIDEBAR_THREADS_LIMIT),
            ("1.5", DEFAULT_SIDEBAR_THREADS_LIMIT),
        ] {
            let ui = limit(v);
            assert_eq!(ui.sidebar_threads_limit, want, "{v}");
            assert!(ui.unread_on_top, "only that key falls back ({v})");
        }
        let c = AppConfig::parse("[ui]\nzoom = 1.0").unwrap();
        assert_eq!(c.ui.sidebar_threads_limit, DEFAULT_SIDEBAR_THREADS_LIMIT);
        let mut c = AppConfig::default();
        c.ui.sidebar_threads_limit = 0;
        let text = toml::to_string(&c).unwrap();
        assert_eq!(AppConfig::parse(&text).unwrap().ui.sidebar_threads_limit, 0);
    }

    #[test]
    fn hardware_acceleration_defaults_on() {
        assert!(UiPrefs::default().hardware_acceleration);
        assert!(
            AppConfig::parse("[ui]\nzoom = 1.0")
                .unwrap()
                .ui
                .hardware_acceleration
        );
        let c = AppConfig::parse("[ui]\nhardware_acceleration = false").unwrap();
        assert!(!c.ui.hardware_acceleration);
        let text = toml::to_string(&c).unwrap();
        assert!(!AppConfig::parse(&text).unwrap().ui.hardware_acceleration);
        let c = AppConfig::parse("[ui]\nhardware_acceleration = \"no\"").unwrap();
        assert!(c.ui.hardware_acceleration, "invalid value falls back");
    }

    #[test]
    fn system_title_bar_defaults_off() {
        assert!(!UiPrefs::default().system_title_bar);
        assert!(
            !AppConfig::parse("[ui]\nzoom = 1.0")
                .unwrap()
                .ui
                .system_title_bar
        );
        let c = AppConfig::parse("[ui]\nsystem_title_bar = true").unwrap();
        assert!(c.ui.system_title_bar);
        let text = toml::to_string(&c).unwrap();
        assert!(AppConfig::parse(&text).unwrap().ui.system_title_bar);
        let c = AppConfig::parse("[ui]\nsystem_title_bar = 1").unwrap();
        assert!(!c.ui.system_title_bar, "invalid value falls back");
    }

    #[test]
    fn update_prefs_default_and_parse() {
        let c = AppConfig::parse("[ui]\nzoom = 1.0").unwrap();
        assert!(c.ui.check_updates && !c.ui.update_prereleases);
        let c = AppConfig::parse("[ui]\ncheck_updates = false\nupdate_prereleases = true").unwrap();
        assert!(!c.ui.check_updates && c.ui.update_prereleases);
        let text = toml::to_string(&c).unwrap();
        assert_eq!(AppConfig::parse(&text).unwrap().ui, c.ui);
        let c = AppConfig::parse("[ui]\ncheck_updates = \"no\"").unwrap();
        assert!(c.ui.check_updates, "invalid value falls back");
    }

    #[test]
    fn escape_marks_read_defaults_on() {
        let c = AppConfig::parse("[ui]\nzoom = 1.0").unwrap();
        assert!(c.ui.escape_marks_read);
        let c = AppConfig::parse("[ui]\nescape_marks_read = false").unwrap();
        assert!(!c.ui.escape_marks_read);
    }

    #[test]
    fn broad_confirm_and_utc_offset_default_on() {
        let c = AppConfig::parse("[ui]\nzoom = 1.0").unwrap();
        assert!(c.ui.confirm_broad_mentions && c.ui.share_utc_offset);
        let c = AppConfig::parse("[ui]\nconfirm_broad_mentions = false\nshare_utc_offset = false")
            .unwrap();
        assert!(!c.ui.confirm_broad_mentions && !c.ui.share_utc_offset);
    }

    #[test]
    fn loaded_values_are_sanitised() {
        let c = AppConfig::parse("[ui]\nzoom = nan\n[window]\nwidth = 0\nheight = 0").unwrap();
        assert_eq!(c.ui.zoom, 1.0);
        assert_eq!(c.window, None);
        let c = AppConfig::parse("[ui]\nzoom = -inf\n[window]\nmaximized = true").unwrap();
        assert_eq!(c.ui.zoom, 1.0);
        assert_eq!(c.window, None, "no size");
        let c = AppConfig::parse("[ui]\nzoom = 40.0").unwrap();
        assert_eq!(c.ui.zoom, *ZOOM_RANGE.end());
        let c = AppConfig::parse("[ui]\nimage_cache_mb = 1").unwrap();
        assert_eq!(c.ui.image_cache_mb, *IMAGE_CACHE_MB_RANGE.start());
        let c = AppConfig::parse("[ui]\nzoom = 1.0").unwrap();
        assert_eq!(c.ui.image_cache_mb, DEFAULT_IMAGE_CACHE_MB);
        let c = AppConfig::parse(
            "active_server = \"gone\"\n[[servers]]\nid = \"a\"\nurl = \"https://1\"\n[[servers]]\nid = \"a\"\nurl = \"https://2\"\n[[servers]]\nid = \"\"\nurl = \"https://3\"",
        )
        .unwrap();
        assert_eq!(c.servers.len(), 3);
        let ids: std::collections::HashSet<_> = c.servers.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids.len(), 3, "duplicate and empty ids replaced");
        assert_eq!(c.servers[0].id, ServerId("a".into()));
        assert_eq!(c.active_server, Some(ServerId("a".into())));
    }

    #[test]
    fn generated_server_ids_are_written_back() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE),
            "[[servers]]\nurl = \"https://x.example\"\n",
        )
        .unwrap();
        let a = AppConfig::load(dir.path()).unwrap();
        let b = AppConfig::load(dir.path()).unwrap();
        assert_eq!(a.servers[0].id, b.servers[0].id, "stable across loads");
    }

    #[test]
    fn save_twice() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = AppConfig::default();
        c.save(dir.path()).unwrap();
        c.ui.zoom = 2.0;
        c.save(dir.path()).unwrap();
        assert_eq!(AppConfig::load(dir.path()).unwrap().ui.zoom, 2.0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_is_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let dots = tempfile::tempdir().unwrap();
        let target = dots.path().join("rustket.toml");
        fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(CONFIG_FILE)).unwrap();
        let mut c = AppConfig::default();
        c.ui.zoom = 1.25;
        c.save(dir.path()).unwrap();
        let link = dir.path().join(CONFIG_FILE);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(AppConfig::load(dir.path()).unwrap().ui.zoom, 1.25);
        assert!(fs::read_to_string(&target).unwrap().contains("zoom = 1.25"));
        // No temp files left next to the link or the target.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(dots.path()).unwrap().count(), 1);
    }

    #[test]
    fn remove_server_moves_active() {
        let mut c = AppConfig::default();
        let a = c.add_server(ServerEntry::new("https://a", "A"));
        let b = c.add_server(ServerEntry::new("https://b", "B"));
        assert_eq!(c.active_server, Some(a.clone()));
        c.remove_server(&a);
        assert_eq!(c.active_server, Some(b.clone()));
        c.remove_server(&b);
        assert_eq!(c.active_server, None);
    }

    #[test]
    fn recent_status_texts_round_trip_and_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = AppConfig::default();
        c.set_recent_status_texts(
            ["a", " b ", "a", "", "c", "d", "e", "f", "g"]
                .map(String::from)
                .to_vec(),
        );
        assert_eq!(c.recent_status_texts, ["a", "b", "c", "d", "e"]);
        c.save(dir.path()).unwrap();
        let back = AppConfig::load(dir.path()).unwrap();
        assert_eq!(back.recent_status_texts, ["a", "b", "c", "d", "e"]);
        // Older files without the key, and too long or invalid lists.
        assert!(AppConfig::parse("").unwrap().recent_status_texts.is_empty());
        let c = AppConfig::parse(
            "recent_status_texts = [1, \"x\", \"x\", \"2\", \"3\", \"4\", \"5\", \"6\"]",
        )
        .unwrap();
        assert_eq!(c.recent_status_texts, ["x", "2", "3", "4", "5"]);
    }

    #[test]
    fn pending_secret_deletions_round_trip_and_survive_server_removal() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = AppConfig::default();
        let sid = c.add_server(ServerEntry::new("https://chat.example.com", "Chat"));
        assert!(c.mark_secret_deletion("rustket:s:u"));
        assert!(!c.mark_secret_deletion("rustket:s:u"), "no duplicates");
        assert!(!c.mark_secret_deletion(""));
        assert!(c.mark_secret_deletion("rustket-e2e:s:u:private_key"));
        c.remove_server(&sid);
        c.save(dir.path()).unwrap();
        let mut back = AppConfig::load(dir.path()).unwrap();
        assert_eq!(
            back.pending_secret_deletions,
            ["rustket:s:u", "rustket-e2e:s:u:private_key"]
        );
        assert!(back.is_secret_deletion_pending("rustket:s:u"));
        assert!(back.clear_secret_deletions(&["rustket:s:u".to_owned()]));
        assert!(!back.clear_secret_deletions(&["rustket:s:u".to_owned()]));
        assert!(!back.is_secret_deletion_pending("rustket:s:u"));
        // Older files without the key; invalid and repeated elements.
        assert!(
            AppConfig::parse("")
                .unwrap()
                .pending_secret_deletions
                .is_empty()
        );
        let c = AppConfig::parse("pending_secret_deletions = [1, \"a\", \"a\", \"b\"]").unwrap();
        assert_eq!(c.pending_secret_deletions, ["a", "b"]);
        // An emptied list leaves the file.
        back.clear_secret_deletions(&["rustket-e2e:s:u:private_key".to_owned()]);
        back.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(!text.contains("pending_secret_deletions"), "{text}");
    }

    #[test]
    fn pending_secret_deletions_are_bounded() {
        let mut c = AppConfig::default();
        for i in 0..MAX_PENDING_SECRET_DELETIONS + 3 {
            c.mark_secret_deletion(&format!("k{i}"));
        }
        assert_eq!(
            c.pending_secret_deletions.len(),
            MAX_PENDING_SECRET_DELETIONS
        );
        let newest = format!("k{}", MAX_PENDING_SECRET_DELETIONS + 2);
        assert!(c.is_secret_deletion_pending(&newest));
        assert!(!c.is_secret_deletion_pending("k0"));
    }

    #[test]
    fn recent_emoji_bounded() {
        let mut c = AppConfig::default();
        for i in 0..50 {
            c.push_recent_emoji(&format!(":e{i}:"));
        }
        c.push_recent_emoji(":e10:");
        assert_eq!(c.recent_emoji.len(), MAX_RECENT_EMOJI);
        assert_eq!(c.recent_emoji[0], ":e10:");
        assert_eq!(c.recent_emoji.iter().filter(|e| *e == ":e10:").count(), 1);
    }
}
