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

use serde::{Deserialize, Serialize};

/// Name of the configuration file inside the config directory.
pub const CONFIG_FILE: &str = "config.toml";
/// Current schema version written by [`AppConfig::save`].
pub const CONFIG_VERSION: u32 = 1;
/// Maximum length of [`AppConfig::recent_emoji`].
pub const MAX_RECENT_EMOJI: usize = 36;
/// Maximum length of [`AppConfig::recent_status_texts`].
pub const MAX_RECENT_STATUS_TEXTS: usize = 5;
/// Accepted range of [`UiPrefs::zoom`].
pub const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.5..=3.0;
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
    /// Unix seconds.
    pub added_at: i64,
    pub sort_order: i32,
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
    let v = Option::<toml::Value>::deserialize(d)?;
    Ok(v.and_then(|v| v.try_into().ok()))
}

impl ServerEntry {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    System,
    En,
    Ru,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarSort {
    #[default]
    Activity,
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
    pub theme: Theme,
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
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            language: Language::System,
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
];
const UI_KEYS: &[&str] = &[
    "theme",
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
];
const SERVER_KEYS: &[&str] = &[
    "id",
    "url",
    "display_name",
    "user_id",
    "username",
    "last_room",
    "pinned_certificates",
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
        if self.window.is_some_and(|w| {
            w.width == 0 || w.height == 0 || w.width > MAX_WINDOW_SIDE || w.height > MAX_WINDOW_SIDE
        }) {
            tracing::warn!("config: invalid window geometry ignored");
            self.window = None;
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
fn write_target(path: &Path) -> PathBuf {
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
        c.ui.language = Language::Ru;
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
            ("language = \"system\"", "language = \"de\""),
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
        c.add_server(s);
        c.ui.use_real_names = Some(true);
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
    fn loaded_values_are_sanitised() {
        let c = AppConfig::parse("[ui]\nzoom = nan\n[window]\nwidth = 0\nheight = 0").unwrap();
        assert_eq!(c.ui.zoom, 1.0);
        assert_eq!(c.window, None);
        let c = AppConfig::parse("[ui]\nzoom = -inf\n[window]\nmaximized = true").unwrap();
        assert_eq!(c.ui.zoom, 1.0);
        assert_eq!(c.window, None, "no size");
        let c = AppConfig::parse("[ui]\nzoom = 40.0").unwrap();
        assert_eq!(c.ui.zoom, *ZOOM_RANGE.end());
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
