//! Image service: avatars (and later attachments) fetched through the server's
//! `RestClient::download`, decoded off the UI thread, kept in a memory LRU and
//! a private per-server disk cache keyed by URL.
//!
//! Safety rules:
//! - SVG is never parsed. Rocket.Chat's default avatars are SVG letter
//!   avatars; for them (and any SVG answer) the local initials avatar is shown
//!   and that decision is cached (memory and disk), so it is not refetched.
//! - Raster images are decoded here with dimension/allocation limits (no
//!   decompression bombs), at most [`MAX_CONCURRENT_DECODES`] at a time, and
//!   avatars are downscaled to [`AVATAR_MAX_PX`] before caching. The disk cache
//!   stores the small decoded image (PNG), not the source.
//! - Downloads are streamed with a byte limit.
//! - The disk cache lives in `0700` directories, one per server, files `0600`.
//!   Every server has a cache generation; purging (logout, removal) bumps it and
//!   results of older generations are dropped by the app, which also does the
//!   disk write, so in-flight fetches cannot repopulate a purged cache.
//! - The memory LRU is bounded by entry count and bytes ([`MEMORY_BUDGET`]);
//!   the initials and failure sets are bounded too ([`MAX_INITIALS`],
//!   [`MAX_FAILED`]), so a long session does not grow without limit.
//! - The disk cache is capped ([`set_disk_cap_mb`], Settings → Storage): when
//!   it grows past the cap the oldest files (by modification time; a hit on a
//!   versioned entry refreshes it) are deleted down to 90% ([`trim_disk`]).
//! - At most [`MAX_FETCHES_PER_SERVER`] fetches of the server's own asset
//!   routes and [`MAX_FOREIGN_FETCHES_PER_SERVER`] of other URLs (message
//!   content) run per server; the rest wait in a queue served newest request
//!   first (what the last frame showed). Every network fetch ends after
//!   [`FETCH_DEADLINE`], so a trickling host cannot hold a slot.
//!
//! Views only read ([`Images::get`], [`Images::avatar`]). Misses are recorded
//! and fetched after the next update, or right after the next frame (the app
//! listens for redraws while [`VIEW_MISSES`] is set). Components should
//! request what they will show from `update`/`on_changes` with [`Images::want`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use iced::futures::StreamExt;
use iced::widget::image;
use rocketchat::{RestClient, ServerUrl};
use rustket_core::ServerId;
use rustket_core::room_list::Avatar;
use rustket_ui::widget::AvatarSource;
use sha2::{Digest, Sha256};

/// Avatar pixel size requested from the server.
pub const AVATAR_PX: u32 = 64;
/// Avatars are downscaled to at most this size (×2 of the largest display size).
pub const AVATAR_MAX_PX: u32 = 128;
/// Other images (attachments) are downscaled to at most this size.
pub const IMAGE_MAX_PX: u32 = 1600;
/// Minimum memory LRU capacity (entries).
pub const MEMORY_CAPACITY: usize = 1024;
/// Memory LRU byte budget (decoded RGBA).
pub const MEMORY_BUDGET: usize = 256 * 1024 * 1024;
/// Most initials decisions kept in memory (they are cheap to restore from the
/// disk markers).
pub const MAX_INITIALS: usize = 50_000;
/// Most recent failures remembered for the backoff.
pub const MAX_FAILED: usize = 4096;
/// One megabyte (decimal, as Settings → Storage labels the cap).
pub const MB: u64 = 1_000_000;
/// Disk entries of URLs without an `etag` are revalidated after this age.
pub const UNVERSIONED_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// A failed URL is not retried for this long.
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(300);
/// Largest download accepted.
pub const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;
/// Largest source dimension accepted.
pub const MAX_DIMENSION: u32 = 10_000;
/// Largest decode allocation accepted.
pub const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
/// Raster decodes running at the same time.
pub const MAX_CONCURRENT_DECODES: usize = 4;
/// Fetches (disk or network) of the server's own asset routes (avatars,
/// emoji, uploads) running at the same time per server.
pub const MAX_FETCHES_PER_SERVER: usize = 12;
/// Fetches of other URLs (attachment `image_url`s on foreign hosts, other
/// paths) per server: a separate pool, so they never take the avatar slots.
pub const MAX_FOREIGN_FETCHES_PER_SERVER: usize = 4;
/// Total time a network download may take (connect, headers and body).
pub const FETCH_DEADLINE: Duration = Duration::from_secs(60);

/// Set when a view recorded a miss; the app flushes it after the next frame.
pub static VIEW_MISSES: AtomicBool = AtomicBool::new(false);

/// A URL to fetch with a given server's client (auth, TLS pins), tagged with
/// the server's cache generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRequest {
    pub server_id: ServerId,
    pub url: String,
}

/// Decoded image ready for widgets.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub handle: image::Handle,
    pub width: u32,
    pub height: u32,
}

impl Loaded {
    fn bytes(&self) -> usize {
        (self.width as usize * self.height as usize * 4).max(64)
    }
}

/// What a fetch produced.
#[derive(Debug, Clone)]
pub enum Fetched {
    Image(Loaded),
    /// The source is SVG (e.g. a default letter avatar): use initials.
    Initials,
}

/// A fetch result plus the disk-cache write the app performs if the server's
/// cache generation is still current.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub fetched: Result<Fetched, String>,
    pub write: Option<(PathBuf, Vec<u8>)>,
}

struct Entry {
    loaded: Loaded,
    used: Cell<u64>,
}

pub struct Images {
    cache: HashMap<String, Entry>,
    /// URLs whose answer was SVG: shown as initials, not refetched.
    initials: HashSet<String>,
    bytes: usize,
    clock: Cell<u64>,
    capacity: usize,
    budget: usize,
    /// URL → running fetch.
    in_flight: HashMap<String, InFlight>,
    failed: HashMap<String, Instant>,
    /// Queued requests → (batch, order): later batches are served first.
    wanted: RefCell<HashMap<ImageRequest, (u64, u64)>>,
    /// Current batch (bumped by every [`Self::take_wanted`]) and order counter.
    want_batch: Cell<u64>,
    want_seq: Cell<u64>,
    generations: HashMap<ServerId, u64>,
    disk_dir: Option<PathBuf>,
    /// Bumped whenever what [`Self::get`] returns may have changed.
    revision: u64,
    /// Per server: bumped when one of its URLs got an image or an initials
    /// decision, or its entries were dropped (not on failures).
    server_revisions: HashMap<ServerId, u64>,
}

struct InFlight {
    server_id: ServerId,
    generation: u64,
    /// Counted in the server's own pool ([`MAX_FETCHES_PER_SERVER`]).
    own: bool,
}

impl Default for Images {
    fn default() -> Self {
        Self::new(default_cache_dir())
    }
}

pub fn default_cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustket").map(|d| d.cache_dir().join("images"))
}

impl Images {
    pub fn new(disk_dir: Option<PathBuf>) -> Self {
        Self {
            cache: HashMap::new(),
            initials: HashSet::new(),
            bytes: 0,
            clock: Cell::new(0),
            capacity: MEMORY_CAPACITY,
            budget: MEMORY_BUDGET,
            in_flight: HashMap::new(),
            server_revisions: HashMap::new(),
            failed: HashMap::new(),
            wanted: RefCell::new(HashMap::new()),
            want_batch: Cell::new(0),
            want_seq: Cell::new(0),
            generations: HashMap::new(),
            disk_dir,
            revision: 0,
        }
    }

    /// Avatar URL for a room-list avatar (with `etag` when known).
    pub fn avatar_url(server: &ServerUrl, avatar: &Avatar) -> String {
        match avatar {
            Avatar::User { username } => server.avatar_user(username, Some(AVATAR_PX)),
            Avatar::Room { rid, etag } => {
                let mut url = server.avatar_room(rid, Some(AVATAR_PX));
                if let Some(etag) = etag.as_deref().filter(|e| !e.is_empty()) {
                    url.push_str("&etag=");
                    url.push_str(&encode_query(etag));
                }
                url
            }
        }
    }

    /// The loaded image or `None` (also for initials); a miss is recorded for
    /// [`Self::take_wanted`].
    pub fn get(&self, server_id: &ServerId, url: &str) -> Option<&Loaded> {
        match self.cache.get(url) {
            Some(e) => {
                self.touch(e);
                Some(&e.loaded)
            }
            None => {
                if !self.initials.contains(url) {
                    self.want(ImageRequest {
                        server_id: server_id.clone(),
                        url: url.to_owned(),
                    });
                    VIEW_MISSES.store(true, Ordering::Relaxed);
                }
                None
            }
        }
    }

    fn touch(&self, e: &Entry) {
        self.clock.set(self.clock.get() + 1);
        e.used.set(self.clock.get());
    }

    pub fn contains(&self, url: &str) -> bool {
        self.cache.contains_key(url)
    }

    /// Changes whenever a cached image or initials decision was added or
    /// dropped: a key for memoised views that show images.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Like [`Self::revision`] but only for one server's URLs: a key for
    /// views that show only that server's images (the sidebar).
    pub fn server_revision(&self, server_id: &ServerId) -> u64 {
        self.server_revisions.get(server_id).copied().unwrap_or(0)
    }

    fn bump(&mut self, server_id: &ServerId) {
        self.revision = self.revision.wrapping_add(1);
        let r = self.server_revisions.entry(server_id.clone()).or_default();
        *r = r.wrapping_add(1);
    }

    pub fn shows_initials(&self, url: &str) -> bool {
        self.initials.contains(url)
    }

    /// Avatar for views: image when loaded, initials otherwise.
    pub fn avatar(
        &self,
        server_id: &ServerId,
        server: &ServerUrl,
        avatar: &Avatar,
        name: &str,
    ) -> AvatarSource {
        let key = match avatar {
            Avatar::User { username } => username.clone(),
            Avatar::Room { rid, .. } => rid.clone(),
        };
        let url = Self::avatar_url(server, avatar);
        AvatarSource::initials(name, key)
            .with_image(self.get(server_id, &url).map(|l| l.handle.clone()))
    }

    /// Records a request (deduplicated; skips cached, initials, in-flight and
    /// recently failed URLs). A cached entry counts as used (LRU).
    pub fn want(&self, req: ImageRequest) {
        if let Some(e) = self.cache.get(&req.url) {
            self.touch(e);
            return;
        }
        if self.initials.contains(&req.url) || self.in_flight.contains_key(&req.url) {
            return;
        }
        if let Some(t) = self.failed.get(&req.url)
            && t.elapsed() < FAILURE_BACKOFF
        {
            return;
        }
        let batch = self.want_batch.get();
        let seq = self.want_seq.get();
        self.want_seq.set(seq.wrapping_add(1));
        let mut w = self.wanted.borrow_mut();
        match w.get_mut(&req) {
            // Asked again in a later frame: it is still on screen.
            Some(at) if at.0 != batch => *at = (batch, seq),
            Some(_) => {}
            None => {
                w.insert(req, (batch, seq));
            }
        }
    }

    /// Number of queued requests.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.wanted.borrow().len()
    }

    pub fn has_wanted(&self) -> bool {
        !self.wanted.borrow().is_empty()
    }

    /// Requests to start now, marked in flight: per server at most
    /// [`MAX_FETCHES_PER_SERVER`] running for which `own` holds (the server's
    /// asset routes, `ServerUrl::is_credentialed_asset`) and
    /// [`MAX_FOREIGN_FETCHES_PER_SERVER`] for the rest; the most recently
    /// requested first (in request order within a batch). The rest stay queued.
    pub fn take_wanted(&mut self, own: impl Fn(&ImageRequest) -> bool) -> Vec<ImageRequest> {
        self.want_batch.set(self.want_batch.get().wrapping_add(1));
        let mut running: HashMap<(ServerId, bool), usize> = HashMap::new();
        for f in self.in_flight.values() {
            *running.entry((f.server_id.clone(), f.own)).or_default() += 1;
        }
        let full = |key: &(ServerId, bool), running: &HashMap<(ServerId, bool), usize>| {
            let cap = if key.1 {
                MAX_FETCHES_PER_SERVER
            } else {
                MAX_FOREIGN_FETCHES_PER_SERVER
            };
            running.get(key).copied().unwrap_or(0) >= cap
        };
        let mut w = self.wanted.borrow_mut();
        let both_full = |sid: &ServerId| {
            full(&(sid.clone(), true), &running) && full(&(sid.clone(), false), &running)
        };
        if w.keys().all(|r| both_full(&r.server_id)) {
            return Vec::new();
        }
        let mut queued: Vec<(ImageRequest, (u64, u64))> = w.drain().collect();
        queued.sort_unstable_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
        let mut out = Vec::new();
        for (req, at) in queued {
            if self.in_flight.contains_key(&req.url) {
                continue;
            }
            let key = (req.server_id.clone(), own(&req));
            if full(&key, &running) {
                w.insert(req, at);
                continue;
            }
            *running.entry(key.clone()).or_default() += 1;
            self.in_flight.insert(
                req.url.clone(),
                InFlight {
                    server_id: key.0,
                    generation: self.generations.get(&req.server_id).copied().unwrap_or(0),
                    own: key.1,
                },
            );
            out.push(req);
        }
        out
    }

    pub fn disk_dir(&self) -> Option<&Path> {
        self.disk_dir.as_deref()
    }

    /// Per-server disk directory.
    pub fn server_dir(&self, server_id: &ServerId) -> Option<PathBuf> {
        self.disk_dir
            .as_ref()
            .map(|d| d.join(sanitize_component(server_id.as_str())))
    }

    /// Current cache generation of a server (bumped by [`Self::purge_server`]).
    pub fn generation(&self, server_id: &ServerId) -> u64 {
        self.generations.get(server_id).copied().unwrap_or(0)
    }

    /// Entry capacity for the avatar working set of all servers.
    pub fn set_working_set(&mut self, entries: usize) {
        self.capacity = entries.saturating_add(256).max(MEMORY_CAPACITY);
        self.evict();
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn memory_bytes(&self) -> usize {
        self.bytes
    }

    /// Applies a fetch result of generation `generation` for `server_id`.
    /// Returns `false` (and drops the result) when the cache was purged since.
    pub fn finish(
        &mut self,
        server_id: &ServerId,
        generation: u64,
        url: String,
        result: Result<Fetched, String>,
    ) -> bool {
        // A result of an older generation must not free a newer fetch's slot.
        if self
            .in_flight
            .get(&url)
            .is_some_and(|f| &f.server_id == server_id && f.generation == generation)
        {
            self.in_flight.remove(&url);
        }
        if generation != self.generation(server_id) {
            return false;
        }
        if result.is_ok() {
            self.bump(server_id);
        }
        match result {
            Ok(Fetched::Image(img)) => {
                self.failed.remove(&url);
                self.initials.remove(&url);
                self.insert(url, img);
            }
            Ok(Fetched::Initials) => {
                self.failed.remove(&url);
                if self.initials.len() >= MAX_INITIALS {
                    // Restored from the disk markers when shown again.
                    self.initials.clear();
                }
                self.initials.insert(url);
            }
            Err(e) => {
                tracing::debug!(error = %e, "image fetch failed");
                if self.failed.len() >= MAX_FAILED {
                    self.failed.retain(|_, t| t.elapsed() < FAILURE_BACKOFF);
                    if self.failed.len() >= MAX_FAILED {
                        self.failed.clear();
                    }
                }
                self.failed.insert(url, Instant::now());
            }
        }
        true
    }

    fn insert(&mut self, url: String, img: Loaded) {
        self.revision = self.revision.wrapping_add(1);
        self.clock.set(self.clock.get() + 1);
        self.bytes += img.bytes();
        if let Some(old) = self.cache.insert(
            url,
            Entry {
                loaded: img,
                used: Cell::new(self.clock.get()),
            },
        ) {
            self.bytes -= old.loaded.bytes();
        }
        self.evict();
    }

    /// Evicts least recently used entries down to 90% of the limits.
    fn evict(&mut self) {
        if self.cache.len() <= self.capacity && self.bytes <= self.budget {
            return;
        }
        let max_entries = self.capacity - self.capacity / 10;
        let max_bytes = self.budget - self.budget / 10;
        let mut by_age: Vec<(u64, String)> = self
            .cache
            .iter()
            .map(|(k, e)| (e.used.get(), k.clone()))
            .collect();
        by_age.sort_unstable();
        for (_, k) in by_age {
            if self.cache.len() <= max_entries && self.bytes <= max_bytes {
                break;
            }
            if let Some(e) = self.cache.remove(&k) {
                self.bytes -= e.loaded.bytes();
            }
        }
    }

    /// Drops cached avatars of a user / room (memory and disk) after an
    /// `AvatarUpdated` event and requests them again right away.
    pub fn invalidate_avatar(
        &mut self,
        server_id: &ServerId,
        server: &ServerUrl,
        username: Option<&str>,
        rid: Option<&str>,
    ) -> Vec<String> {
        let mut prefixes = Vec::new();
        if let Some(u) = username {
            prefixes.push(server.avatar_user(u, None));
        }
        if let Some(r) = rid {
            prefixes.push(server.avatar_room(r, None));
        }
        let matches = |url: &str| {
            prefixes.iter().any(|p| {
                url == p || url.starts_with(&format!("{p}?")) || url.starts_with(&format!("{p}&"))
            })
        };
        let mut dropped: Vec<String> = self.cache.keys().filter(|u| matches(u)).cloned().collect();
        dropped.extend(self.initials.iter().filter(|u| matches(u)).cloned());
        if !dropped.is_empty() {
            self.bump(server_id);
        }
        let dir = self.server_dir(server_id);
        for url in &dropped {
            if let Some(e) = self.cache.remove(url) {
                self.bytes -= e.loaded.bytes();
            }
            self.initials.remove(url);
            self.failed.remove(url);
            if let Some(dir) = &dir {
                for p in disk_paths(dir, url) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        for url in &dropped {
            self.want(ImageRequest {
                server_id: server_id.clone(),
                url: url.clone(),
            });
        }
        dropped
    }

    /// Forgets everything of a server (logout, removal) and bumps its cache
    /// generation. Returns the disk directory to delete (off the UI thread).
    pub fn purge_server(&mut self, server_id: &ServerId, server: &ServerUrl) -> Option<PathBuf> {
        *self.generations.entry(server_id.clone()).or_default() += 1;
        self.bump(server_id);
        let prefix = format!("{}/", server.as_str());
        let mut freed = 0;
        self.cache.retain(|k, e| {
            let keep = !k.starts_with(&prefix);
            if !keep {
                freed += e.loaded.bytes();
            }
            keep
        });
        self.bytes -= freed;
        self.initials.retain(|k| !k.starts_with(&prefix));
        self.failed.retain(|k, _| !k.starts_with(&prefix));
        self.in_flight
            .retain(|k, f| !k.starts_with(&prefix) && &f.server_id != server_id);
        self.wanted
            .borrow_mut()
            .retain(|r, _| &r.server_id != server_id);
        PURGES.fetch_add(1, Ordering::AcqRel);
        DISK_USAGE.store(USAGE_UNKNOWN, Ordering::Relaxed);
        self.server_dir(server_id)
    }

    /// Entries in the initials and failure sets (for the bound tests).
    #[cfg(test)]
    pub(crate) fn side_sets(&self) -> (usize, usize) {
        (self.initials.len(), self.failed.len())
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

pub(crate) fn encode_query(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn hash(url: &str) -> String {
    hex::encode(Sha256::digest(url.as_bytes()))
}

/// `(initials marker, decoded PNG)` cache file names of `url` in a server directory.
pub fn disk_paths(dir: &Path, url: &str) -> [PathBuf; 2] {
    let h = hash(url);
    [
        dir.join(format!("{h}.initials")),
        dir.join(format!("{h}.png")),
    ]
}

fn has_version(url: &str) -> bool {
    url.contains("etag=")
}

/// Whether `url` is one of `server`'s own avatar URLs.
pub fn is_server_avatar(server: &ServerUrl, url: &str) -> bool {
    url.starts_with(&server.join("avatar/"))
}

/// Creates `dir` (and parents) private to the user. On Windows the ACL of the
/// per-user profile directory is the protection; nothing is changed.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options.open(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(tmp, path)
}

/// Writes a cache entry (called by the app when the generation is current).
/// Trims the cache when enough was written since the last trim.
pub fn write_cache_entry(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no dir"))?;
    if let Some(root) = dir.parent() {
        private_dir(root)?;
    }
    private_dir(dir)?;
    write_private(path, bytes)?;
    let n = bytes.len() as u64;
    let mut usage = DISK_USAGE.load(Ordering::Relaxed);
    while usage != USAGE_UNKNOWN {
        match DISK_USAGE.compare_exchange_weak(
            usage,
            usage.saturating_add(n).min(USAGE_UNKNOWN - 1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(now) => usage = now,
        }
    }
    let written = DISK_WRITTEN.fetch_add(n, Ordering::Relaxed) + n;
    let cap = disk_cap();
    let usage = DISK_USAGE.load(Ordering::Relaxed);
    if (written >= cap / 20 || (usage != USAGE_UNKNOWN && usage > cap))
        && let Some(root) = dir.parent()
    {
        trim_disk(root, cap);
    }
    Ok(())
}

/// Default disk cache cap.
pub const DEFAULT_DISK_CAP: u64 = rustket_core::config::DEFAULT_IMAGE_CACHE_MB as u64 * MB;
/// Unfinished temporary files older than this are removed by [`trim_disk`].
const STALE_TMP: Duration = Duration::from_secs(3600);
const USAGE_UNKNOWN: u64 = u64::MAX;
/// Bumped by every purge: a trim scan that started before one does not
/// report its (stale) total.
static PURGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DISK_CAP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(DEFAULT_DISK_CAP);
/// Bytes written since the last trim.
static DISK_WRITTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Size of the disk cache as of the last trim plus later writes.
static DISK_USAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(USAGE_UNKNOWN);
static TRIMMING: AtomicBool = AtomicBool::new(false);

/// Sets the disk cache cap (`UiPrefs::image_cache_mb`). Returns whether it
/// went down (the caller then runs [`trim_task`]).
pub fn set_disk_cap_mb(mb: u32) -> bool {
    let cap = u64::from(mb.max(1)) * MB;
    DISK_CAP.swap(cap, Ordering::Relaxed) > cap
}

pub fn disk_cap() -> u64 {
    DISK_CAP.load(Ordering::Relaxed)
}

/// Size of the disk cache in bytes, when known (after a trim scan).
pub fn disk_usage() -> Option<u64> {
    Some(DISK_USAGE.load(Ordering::Relaxed)).filter(|u| *u != USAGE_UNKNOWN)
}

/// Deletes the oldest files of the disk cache under `root` (one directory
/// per server) until it is at most 90% of `cap` when it is over `cap`, and
/// leftover temporary files. Returns the remaining size. Blocking: run it
/// off the UI thread. Concurrent calls return right away.
pub fn trim_disk(root: &Path, cap: u64) -> u64 {
    if TRIMMING.swap(true, Ordering::AcqRel) {
        return DISK_USAGE.load(Ordering::Relaxed);
    }
    DISK_WRITTEN.store(0, Ordering::Relaxed);
    let purges = PURGES.load(Ordering::Acquire);
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let now = SystemTime::now();
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let Ok(kind) = dir.file_type() else { continue };
        if !kind.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let path = f.path();
            let tmp = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.starts_with("tmp"));
            if tmp {
                if now.duration_since(mtime).unwrap_or_default() > STALE_TMP {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            files.push((mtime, meta.len(), path));
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total > cap {
        let target = cap - cap / 10;
        files.sort_unstable_by_key(|f| f.0);
        let mut removed = 0usize;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                removed += 1;
            }
        }
        tracing::debug!(removed, total, cap, "image disk cache trimmed");
    }
    if PURGES.load(Ordering::Acquire) == purges {
        DISK_USAGE.store(total, Ordering::Relaxed);
    }
    TRIMMING.store(false, Ordering::Release);
    total
}

/// [`trim_disk`] of `root` with the current cap, in a task.
pub fn trim_task<M: Send + 'static>(root: Option<&Path>, done: M) -> iced::Task<M> {
    let Some(root) = root.map(Path::to_path_buf) else {
        return iced::Task::none();
    };
    iced::Task::perform(
        async move {
            let _ = tokio::task::spawn_blocking(move || trim_disk(&root, disk_cap())).await;
        },
        move |()| done,
    )
}

/// Marks a versioned disk entry as recently used (its freshness does not
/// depend on the modification time), so trimming evicts the least recently
/// used first.
fn touch_file(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn decode_permits() -> &'static tokio::sync::Semaphore {
    static S: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    S.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_DECODES))
}

/// Loads `url` from the disk cache (`server_dir`) or the network and decodes
/// it. Never call from `update`/`view`; run it in a task. The disk write is
/// returned to the caller (see [`FetchResult`]).
pub async fn fetch(rest: RestClient, server_dir: Option<PathBuf>, url: String) -> FetchResult {
    let max_px = if is_server_avatar(rest.server(), &url) {
        AVATAR_MAX_PX
    } else {
        IMAGE_MAX_PX
    };
    // End-to-end encrypted files: decrypted in memory, never cached on disk.
    let e2e = tokio::time::timeout(
        FETCH_DEADLINE,
        crate::e2e::files::fetch_image(&rest, &url, MAX_DOWNLOAD_BYTES),
    )
    .await
    .unwrap_or_else(|_| Some(Err("image fetch timed out".into())));
    if let Some(r) = e2e {
        let fetched = match r {
            Ok(bytes) => decode_limited(bytes, max_px)
                .await
                .map(|(img, _)| Fetched::Image(img)),
            Err(e) => Err(e),
        };
        return FetchResult {
            fetched,
            write: None,
        };
    }
    if let Some(dir) = &server_dir {
        let [marker, png] = disk_paths(dir, &url);
        let versioned = has_version(&url);
        let cached = tokio::task::spawn_blocking(move || {
            if read_fresh(&marker, versioned).is_some() {
                if versioned {
                    touch_file(&marker);
                }
                return Some(None);
            }
            let bytes = read_fresh(&png, versioned)?;
            if versioned {
                touch_file(&png);
            }
            Some(Some(bytes))
        })
        .await
        .ok()
        .flatten();
        match cached {
            Some(None) => {
                return FetchResult {
                    fetched: Ok(Fetched::Initials),
                    write: None,
                };
            }
            Some(Some(bytes)) => {
                if let Ok(img) = decode_limited(bytes, max_px).await {
                    return FetchResult {
                        fetched: Ok(Fetched::Image(img.0)),
                        write: None,
                    };
                }
            }
            None => {}
        }
    }
    let fetched = download(&rest, &url, FETCH_DEADLINE).await;
    let (fetched, store) = match fetched {
        Err(e) => (Err(e), None),
        Ok(Body::Svg) => (Ok(Fetched::Initials), Some(Vec::new())),
        Ok(Body::Raster(bytes)) => match decode_limited(bytes, max_px).await {
            Ok((img, png)) => (Ok(Fetched::Image(img)), Some(png)),
            Err(e) => (Err(e), None),
        },
    };
    let write = match (server_dir, store) {
        (Some(dir), Some(bytes)) => {
            let [marker, png] = disk_paths(&dir, &url);
            Some(if bytes.is_empty() {
                (marker, bytes)
            } else {
                (png, bytes)
            })
        }
        _ => None,
    };
    FetchResult { fetched, write }
}

enum Body {
    Svg,
    Raster(Vec<u8>),
}

/// Downloads `url` within `deadline` (connect, headers and body together).
async fn download(rest: &RestClient, url: &str, deadline: Duration) -> Result<Body, String> {
    tokio::time::timeout(deadline, download_body(rest, url))
        .await
        .unwrap_or_else(|_| Err("image fetch timed out".into()))
}

async fn download_body(rest: &RestClient, url: &str) -> Result<Body, String> {
    let dl = rest.download(url).await.map_err(|e| e.to_string())?;
    let svg_type = dl
        .content_type
        .as_deref()
        .is_some_and(|t| t.to_ascii_lowercase().contains("svg"));
    if svg_type {
        // Not parsed at all; the body is not even read.
        return Ok(Body::Svg);
    }
    if dl.content_length.is_some_and(|n| n > MAX_DOWNLOAD_BYTES) {
        return Err("image too large".into());
    }
    let mut body = Vec::new();
    let mut stream = Box::pin(dl.stream());
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if body.len() as u64 + chunk.len() as u64 > MAX_DOWNLOAD_BYTES {
            return Err("image too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    if looks_like_svg(&body) {
        return Ok(Body::Svg);
    }
    Ok(Body::Raster(body))
}

fn read_fresh(path: &Path, versioned: bool) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_DOWNLOAD_BYTES {
        return None;
    }
    if !versioned {
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or(Duration::MAX);
        if age > UNVERSIONED_MAX_AGE {
            return None;
        }
    }
    std::fs::read(path).ok()
}

async fn decode_limited(bytes: Vec<u8>, max_px: u32) -> Result<(Loaded, Vec<u8>), String> {
    let _permit = decode_permits()
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || decode_and_encode(&bytes, max_px))
        .await
        .map_err(|e| e.to_string())?
}

/// Text that looks like SVG/XML (never parsed; answered with initials).
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let s = String::from_utf8_lossy(head);
    let s = s.trim_start_matches('\u{feff}').trim_start();
    s.starts_with('<') && (s.contains("<svg") || s.starts_with("<?xml"))
}

/// Decodes raster `bytes` with limits and downscales to `max_px`.
pub fn decode(bytes: &[u8], max_px: u32) -> Result<Loaded, String> {
    decode_and_encode(bytes, max_px).map(|(l, _)| l)
}

/// [`decode`] plus the PNG encoding of the (downscaled) result for the disk cache.
fn decode_and_encode(bytes: &[u8], max_px: u32) -> Result<(Loaded, Vec<u8>), String> {
    if bytes.is_empty() {
        return Err("empty image".into());
    }
    if looks_like_svg(bytes) {
        return Err("SVG is not decoded".into());
    }
    let mut reader = ::image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| e.to_string())?;
    let img = if img.width() > max_px || img.height() > max_px {
        img.thumbnail(max_px, max_px)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut png = Vec::new();
    rgba.write_to(
        &mut std::io::Cursor::new(&mut png),
        ::image::ImageFormat::Png,
    )
    .map_err(|e| e.to_string())?;
    Ok((
        Loaded {
            handle: image::Handle::from_rgba(w, h, rgba.into_raw()),
            width: w,
            height: h,
        },
        png,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200"><rect width="100%" height="100%" fill="#2F343D"/><text x="50%" y="50%">A</text></svg>"##;

    pub(crate) fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&[255, 0, 0, 255].repeat((w * h) as usize))
            .unwrap();
        drop(wr);
        out
    }

    /// Review re-review #1/#2: SVG is never decoded.
    #[test]
    fn svg_is_never_decoded() {
        assert!(looks_like_svg(SVG.as_bytes()));
        assert!(looks_like_svg(
            format!("<?xml version=\"1.0\"?>\n{SVG}").as_bytes()
        ));
        assert!(!looks_like_svg(&png_bytes(1, 1)));
        // A `<use>` fan-out bomb returns immediately.
        let bomb = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><defs><g id="a"><use xlink:href="#b"/><use xlink:href="#b"/></g></defs></svg>"##;
        let t = Instant::now();
        assert!(decode(bomb.as_bytes(), AVATAR_MAX_PX).is_err());
        assert!(t.elapsed() < Duration::from_millis(50));
        assert!(decode(b"", AVATAR_MAX_PX).is_err());
    }

    #[test]
    fn raster_decode_limits_and_downscale() {
        let img = decode(&png_bytes(4, 3), AVATAR_MAX_PX).unwrap();
        assert_eq!((img.width, img.height), (4, 3));
        // Large avatars are downscaled before caching.
        let big = decode(&png_bytes(1000, 500), AVATAR_MAX_PX).unwrap();
        assert!(big.width <= AVATAR_MAX_PX && big.height <= AVATAR_MAX_PX);
        assert_eq!(big.width, AVATAR_MAX_PX);
        // A PNG header claiming a huge size is rejected before allocation.
        let mut bomb = png_bytes(1, 1);
        bomb[16..20].copy_from_slice(&50_000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&50_000u32.to_be_bytes());
        assert!(decode(&bomb, AVATAR_MAX_PX).is_err());
        assert!(decode(b"garbage", AVATAR_MAX_PX).is_err());
        // The disk copy is the small decoded image.
        let (_, png) = decode_and_encode(&png_bytes(1000, 500), AVATAR_MAX_PX).unwrap();
        let again = decode(&png, AVATAR_MAX_PX).unwrap();
        assert_eq!(again.width, AVATAR_MAX_PX);
    }

    fn img(w: u32) -> Loaded {
        decode(&png_bytes(w, w), IMAGE_MAX_PX).unwrap()
    }

    #[test]
    fn wanted_dedup_failures_and_initials() {
        let mut images = Images::new(None);
        let sid = ServerId("s".into());
        assert!(images.get(&sid, "u1").is_none());
        assert!(images.get(&sid, "u1").is_none());
        assert!(images.has_wanted());
        assert_eq!(images.take_wanted(|_| true).len(), 1);
        assert!(images.get(&sid, "u1").is_none());
        assert!(images.take_wanted(|_| true).is_empty(), "in flight");
        assert!(images.finish(&sid, 0, "u1".into(), Ok(Fetched::Image(img(1)))));
        assert!(images.get(&sid, "u1").is_some());
        // Failures back off.
        images.get(&sid, "bad");
        images.take_wanted(|_| true);
        images.finish(&sid, 0, "bad".into(), Err("x".into()));
        assert!(images.get(&sid, "bad").is_none());
        assert!(images.take_wanted(|_| true).is_empty());
        // SVG answers become initials and are not requested again.
        images.get(&sid, "svg");
        images.take_wanted(|_| true);
        images.finish(&sid, 0, "svg".into(), Ok(Fetched::Initials));
        assert!(images.get(&sid, "svg").is_none());
        assert!(images.shows_initials("svg"));
        assert!(images.take_wanted(|_| true).is_empty());
    }

    /// App review #57: a sidebar of 2000 rows does not start 2000 fetches at
    /// once; what the latest frame asked for goes first.
    #[test]
    fn fetches_are_bounded_per_server_newest_first() {
        let mut images = Images::new(None);
        let (a, b) = (ServerId("a".into()), ServerId("b".into()));
        for i in 0..2000 {
            images.get(&a, &format!("a{i}"));
        }
        images.get(&b, "b0");
        let first = images.take_wanted(|_| true);
        assert_eq!(first.len(), MAX_FETCHES_PER_SERVER + 1);
        assert_eq!(first[0].url, "a0", "request order within a frame");
        assert!(
            first.iter().any(|r| r.url == "b0"),
            "servers are independent"
        );
        assert!(images.take_wanted(|_| true).is_empty(), "all slots busy");
        // The next frame shows a room: its image jumps the queue.
        images.get(&a, "room-image");
        images.finish(&a, 0, "a0".into(), Err("x".into()));
        let next = images.take_wanted(|_| true);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].url, "room-image");
        // Duplicates are not queued twice.
        let queued = images.wanted.borrow().len();
        images.get(&a, "a1999");
        assert_eq!(images.wanted.borrow().len(), queued);
        let rev = images.revision();
        images.finish(&a, 0, "a1".into(), Ok(Fetched::Image(img(1))));
        assert_ne!(images.revision(), rev);
    }

    /// Re-review #12/#7: LRU bounded by entries and bytes; used entries stay.
    #[test]
    fn lru_entries_and_bytes() {
        let mut images = Images::new(None);
        let sid = ServerId("s".into());
        let small = img(1);
        images.insert("hot".into(), small.clone());
        for i in 0..MEMORY_CAPACITY + 50 {
            images.insert(format!("u{i}"), small.clone());
            if i % 100 == 0 {
                assert!(images.get(&sid, "hot").is_some());
            }
        }
        assert!(images.len() <= MEMORY_CAPACITY);
        assert!(images.contains("hot"));
        images.set_working_set(5000);
        assert!(images.capacity() >= 5000);
        let mut images = Images::new(None);
        images.budget = 10 * 128 * 128 * 4;
        for i in 0..50 {
            images.insert(format!("b{i}"), img(128));
        }
        assert!(images.memory_bytes() <= images.budget);
        assert!(images.len() <= 10);
    }

    #[test]
    fn avatar_urls_invalidation_and_generations() {
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let sid = ServerId("srv1".into());
        let u = Images::avatar_url(
            &server,
            &Avatar::User {
                username: "bob".into(),
            },
        );
        assert_eq!(u, "http://localhost:3000/avatar/bob?size=64");
        assert!(is_server_avatar(&server, &u));
        assert!(!is_server_avatar(
            &server,
            "http://localhost:3000/file-upload/x.png"
        ));
        let r = Images::avatar_url(
            &server,
            &Avatar::Room {
                rid: "R".into(),
                etag: Some("e1".into()),
            },
        );
        assert_eq!(r, "http://localhost:3000/avatar/room/R?size=64&etag=e1");
        let dir = tempfile::tempdir().unwrap();
        let mut images = Images::new(Some(dir.path().to_owned()));
        let sdir = images.server_dir(&sid).unwrap();
        write_cache_entry(&disk_paths(&sdir, &u)[1], &png_bytes(1, 1)).unwrap();
        images.insert(u.clone(), img(1));
        images.insert(r.clone(), img(1));
        let dropped = images.invalidate_avatar(&sid, &server, Some("bob"), None);
        assert_eq!(dropped, vec![u.clone()]);
        assert!(!images.contains(&u));
        assert!(images.contains(&r));
        assert!(!disk_paths(&sdir, &u)[1].exists());
        let w = images.take_wanted(|_| true);
        assert_eq!(w.len(), 1, "re-requested right away");
        // Purge bumps the generation: an in-flight result is dropped.
        let generation = images.generation(&sid);
        assert_eq!(images.purge_server(&sid, &server), Some(sdir));
        assert!(!images.contains(&r));
        assert!(!images.finish(&sid, generation, u.clone(), Ok(Fetched::Image(img(1)))));
        assert!(!images.contains(&u));
        assert_eq!(images.memory_bytes(), 0);
    }

    /// Review: untrusted image URLs (message content) used to share the
    /// server's slots, so a dozen slow foreign hosts starved every avatar.
    #[test]
    fn foreign_urls_never_take_the_servers_slots() {
        let mut images = Images::new(None);
        let sid = ServerId("s".into());
        let own = |r: &ImageRequest| r.url.starts_with("http://srv/avatar/");
        for i in 0..50 {
            images.get(&sid, &format!("http://evil{i}/x.png"));
        }
        let first = images.take_wanted(own);
        assert_eq!(first.len(), MAX_FOREIGN_FETCHES_PER_SERVER);
        assert!(images.take_wanted(own).is_empty());
        for i in 0..20 {
            images.get(&sid, &format!("http://srv/avatar/u{i}"));
        }
        let avatars = images.take_wanted(own);
        assert_eq!(avatars.len(), MAX_FETCHES_PER_SERVER);
        assert!(avatars.iter().all(own));
    }

    /// Review: a result of a purged generation removed the in-flight marker
    /// of the newer fetch of the same URL (duplicate downloads, slots
    /// undercounted). Failures change nothing a view shows.
    #[test]
    fn stale_results_keep_newer_fetches_in_flight() {
        let mut images = Images::new(None);
        let sid = ServerId("s".into());
        let server = ServerUrl::parse("http://localhost:3000").unwrap();
        let url = "http://localhost:3000/avatar/bob?size=64".to_owned();
        images.get(&sid, &url);
        assert_eq!(images.take_wanted(|_| true).len(), 1);
        images.purge_server(&sid, &server);
        images.get(&sid, &url);
        assert_eq!(images.take_wanted(|_| true).len(), 1);
        assert!(!images.finish(&sid, 0, url.clone(), Err("old".into())));
        images.get(&sid, &url);
        assert!(images.take_wanted(|_| true).is_empty(), "still in flight");
        let rev = images.server_revision(&sid);
        let other = ServerId("o".into());
        images.get(&other, "http://o/x");
        images.take_wanted(|_| true);
        images.finish(&other, 0, "http://o/x".into(), Ok(Fetched::Image(img(1))));
        assert!(images.finish(&sid, 1, url.clone(), Err("x".into())));
        assert_eq!(images.server_revision(&sid), rev, "other server, failure");
        images.get(&sid, &url);
        assert!(images.take_wanted(|_| true).is_empty(), "backing off");
    }

    /// Review: a host that sends headers and then one byte at a time held a
    /// fetch (and its slot) forever; the read timeout resets on every byte.
    #[tokio::test]
    async fn trickling_host_hits_the_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 1000000\r\n\r\n",
                        )
                        .await;
                    for _ in 0..1000 {
                        if sock.write_all(b"x").await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                });
            }
        });
        let server = ServerUrl::parse(&format!("http://{addr}")).unwrap();
        let tls = crate::session::tls_for(&[]).unwrap();
        let rest = RestClient::new(server, &tls).unwrap();
        let started = Instant::now();
        let r = download(
            &rest,
            &format!("http://{addr}/elsewhere/x.png"),
            Duration::from_millis(600),
        )
        .await;
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Phase 13: a long session with many large attachments, SVG avatars
    /// and failing URLs stays within the memory budget and the set bounds.
    #[test]
    fn long_sessions_stay_bounded() {
        let mut images = Images::new(None);
        images.set_working_set(20_000);
        let sid = ServerId("s".into());
        let tiny = img(1);
        // 1600x1600 RGBA claims ~10 MB each (synthetic: the handle is tiny).
        for i in 0..200 {
            let big = Loaded {
                handle: tiny.handle.clone(),
                width: IMAGE_MAX_PX,
                height: IMAGE_MAX_PX,
            };
            assert!(images.finish(&sid, 0, format!("http://h/big{i}"), Ok(Fetched::Image(big))));
            assert!(images.memory_bytes() <= MEMORY_BUDGET);
        }
        assert!(images.len() < 30, "{}", images.len());
        // The newest images are the ones kept.
        assert!(images.contains("http://h/big199"));
        assert!(!images.contains("http://h/big0"));
        for i in 0..MAX_INITIALS + 10 {
            images.finish(&sid, 0, format!("http://h/svg{i}"), Ok(Fetched::Initials));
        }
        for i in 0..MAX_FAILED * 3 {
            images.finish(&sid, 0, format!("http://h/bad{i}"), Err("x".into()));
        }
        let (initials, failed) = images.side_sets();
        assert!(initials <= MAX_INITIALS, "{initials}");
        assert!(failed <= MAX_FAILED, "{failed}");
        // The latest decisions survive the bound.
        assert!(images.shows_initials(&format!("http://h/svg{}", MAX_INITIALS + 9)));
    }

    /// Phase 13: the disk cache is trimmed oldest first down to 90% of the
    /// cap; stale temporary files go too.
    #[test]
    fn disk_cache_is_trimmed_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("images");
        let now = SystemTime::now();
        let mut paths = Vec::new();
        for (server, n) in [("a", 30u64), ("b", 30)] {
            for i in 0..n {
                let p = root.join(server).join(format!("{i:03}.png"));
                write_private_entry(&p, &[0u8; 1024]);
                // a/000 is the oldest, b/029 the newest.
                let age = Duration::from_secs(if server == "a" { 10_000 } else { 5_000 } - i * 10);
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(now - age)
                    .unwrap();
                paths.push(p);
            }
        }
        let tmp = root.join("a").join("x.tmp123-4");
        write_private_entry(&tmp, b"partial");
        std::fs::File::options()
            .write(true)
            .open(&tmp)
            .unwrap()
            .set_modified(now - Duration::from_secs(7200))
            .unwrap();
        // Under the cap: nothing but the stale temporary file goes.
        assert_eq!(trim_disk(&root, 100 * 1024), 60 * 1024);
        assert!(!tmp.exists());
        // Cap 40 KiB: down to 36 KiB, the oldest 24 files removed.
        let left = trim_disk(&root, 40 * 1024);
        assert_eq!(left, 36 * 1024);
        let gone: Vec<bool> = paths.iter().map(|p| !p.exists()).collect();
        assert!(gone[..24].iter().all(|g| *g), "oldest removed");
        assert!(gone[24..].iter().all(|g| !*g), "newest kept");
        // The cap setting: lowering it asks for a trim.
        assert!(!set_disk_cap_mb(
            rustket_core::config::DEFAULT_IMAGE_CACHE_MB
        ));
        assert!(set_disk_cap_mb(100));
        assert_eq!(disk_cap(), 100 * MB);
        assert!(!set_disk_cap_mb(
            rustket_core::config::DEFAULT_IMAGE_CACHE_MB
        ));
        assert_eq!(disk_cap(), DEFAULT_DISK_CAP);
    }

    fn write_private_entry(p: &Path, bytes: &[u8]) {
        private_dir(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("images").join("s").join("x.png");
        write_cache_entry(&f, b"x").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(f.parent().unwrap()), 0o700);
        assert_eq!(mode(f.parent().unwrap().parent().unwrap()), 0o700);
        assert_eq!(mode(&f), 0o600);
        assert!(read_fresh(&f, true).is_some());
    }
}
