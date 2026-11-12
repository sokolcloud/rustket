//! The SQLite store behind the offline cache (synchronous; see
//! [`super::CacheHandle`] for the worker thread).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rocketchat::models::Message;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    Batch, CacheError, CachedState, CachedThreads, CachedTimeline, DEFAULT_MAX_MESSAGE_BYTES,
    FILE_NAME, MAX_TIMELINES, MESSAGES_PER_TIMELINE, OutboxEntry, Result, TimelineKey, sanitize,
};

/// Current schema (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;
/// `PRAGMA application_id` of a Rustket cache ("RKTC").
const APPLICATION_ID: i64 = 0x524B_5443;

/// Migration `i` brings the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // 1: documents, timelines with their newest messages, outbox.
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE docs (
         kind TEXT NOT NULL,
         id TEXT NOT NULL,
         json TEXT NOT NULL,
         PRIMARY KEY (kind, id)
     ) WITHOUT ROWID;
     CREATE TABLE timelines (
         key TEXT PRIMARY KEY,
         rid TEXT NOT NULL,
         tmid TEXT,
         parent TEXT,
         accessed INTEGER NOT NULL,
         bytes INTEGER NOT NULL
     );
     CREATE INDEX timelines_rid ON timelines (rid);
     CREATE TABLE messages (
         tl TEXT NOT NULL,
         id TEXT NOT NULL,
         ts INTEGER NOT NULL,
         json TEXT NOT NULL,
         PRIMARY KEY (tl, id)
     ) WITHOUT ROWID;
     CREATE INDEX messages_tl_ts ON messages (tl, ts);
     CREATE TABLE outbox (
         id TEXT PRIMARY KEY,
         rid TEXT NOT NULL,
         created INTEGER NOT NULL,
         json TEXT NOT NULL,
         queued INTEGER NOT NULL,
         error TEXT
     );",
];

const KIND_ME: &str = "me";
const KIND_INFO: &str = "info";
const KIND_SETTING: &str = "setting";
const KIND_PERMISSION: &str = "permission";
const KIND_ROLE: &str = "role";
const KIND_SUB: &str = "sub";
const KIND_ROOM: &str = "room";
const KIND_EMOJI: &str = "emoji";
const KIND_STATUS: &str = "status";
/// Sidebar thread data (`CachedThreads`): not part of a full snapshot.
const KIND_THREAD_ROOM: &str = "thread_room";
const KIND_THREAD_PARENT: &str = "thread_parent";
const KIND_THREAD_SYNCED: &str = "thread_synced";
const KIND_THREAD_STARRED: &str = "thread_starred";
const THREAD_KINDS: [&str; 4] = [
    KIND_THREAD_ROOM,
    KIND_THREAD_PARENT,
    KIND_THREAD_SYNCED,
    KIND_THREAD_STARRED,
];

const META_USER: &str = "user_id";
const META_SAVED_AT: &str = "saved_at";
const META_UPLOADS: &str = "interrupted_uploads";
/// `outbox.queued`: 0 failed, 1 queued, 2 queued and possibly delivered.
const QUEUED_MAYBE_SENT: i64 = 2;

/// What happened while opening.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// A corrupt or incompatible database was moved here; a fresh one was
    /// started.
    pub moved_aside: Option<PathBuf>,
    /// The database belonged to another account and was emptied.
    pub user_changed: bool,
}

pub struct CacheDb {
    conn: Connection,
    dir: PathBuf,
    path: PathBuf,
    user_id: String,
    max_bytes: u64,
    max_timelines: usize,
    /// Last `accessed` stamp handed out (strictly increasing).
    last_access: i64,
}

impl std::fmt::Debug for CacheDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheDb")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn to_json<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

/// Parses a stored document; one that no longer parses is skipped.
fn from_json<T: DeserializeOwned>(kind: &str, json: &str) -> Option<T> {
    serde_json::from_str(json)
        .inspect_err(|e| tracing::debug!(kind, error = %e, "skipping unreadable cache entry"))
        .ok()
}

fn ts_ms(m: &Message) -> i64 {
    m.ts.or(m.updated_at).map_or(0, |d| d.timestamp_millis())
}

/// Creates `dir` (and parents) with mode 0700; tightens an existing one.
fn private_dir(dir: &Path) -> std::io::Result<()> {
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
    {
        std::fs::create_dir_all(dir)
    }
}

/// Creates the database file with mode 0600 before SQLite opens it (its
/// journal files copy the mode); tightens an existing one.
fn private_file(path: &Path) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        opts.open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        opts.open(path).map(|_| ())
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Moves a broken database (and its journal files) aside; an older broken
/// copy is replaced.
fn move_aside(path: &Path) -> std::io::Result<PathBuf> {
    let target = sidecar(path, ".broken");
    std::fs::rename(path, &target)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let side = sidecar(path, suffix);
        if side.exists() {
            let _ = std::fs::remove_file(side);
        }
    }
    Ok(target)
}

/// Whether `e` means the file is not a usable database.
pub(super) fn is_corruption(e: &CacheError) -> bool {
    match e {
        CacheError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => matches!(
            f.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        CacheError::Corrupt(_) | CacheError::Incompatible { .. } => true,
        _ => false,
    }
}

impl CacheDb {
    /// Opens (or creates) the cache in `dir` for account `user_id`. A corrupt
    /// database, or one written by a newer schema, is moved aside and a fresh
    /// one is started; one of another account is emptied.
    pub fn open(dir: &Path, user_id: &str) -> Result<(Self, OpenReport)> {
        private_dir(dir)?;
        let path = dir.join(FILE_NAME);
        let mut report = OpenReport::default();
        let db = match Self::try_open(dir, &path, user_id) {
            Ok((db, changed)) => {
                report.user_changed = changed;
                db
            }
            Err(e) if is_corruption(&e) => {
                tracing::warn!(error = %e, "offline cache unusable; starting a new one");
                report.moved_aside = Some(move_aside(&path)?);
                Self::try_open(dir, &path, user_id)?.0
            }
            Err(e) => return Err(e),
        };
        Ok((db, report))
    }

    fn try_open(dir: &Path, path: &Path, user_id: &str) -> Result<(Self, bool)> {
        private_file(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(CacheError::Corrupt(check));
        }
        let app_id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app_id != 0 && app_id != APPLICATION_ID {
            return Err(CacheError::Corrupt("not a Rustket cache".into()));
        }
        if version > SCHEMA_VERSION {
            return Err(CacheError::Incompatible {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        // Deleted rows (an outbox entry of a room that turned out encrypted,
        // evicted messages) are overwritten, not left in free pages.
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = OFF; PRAGMA secure_delete = ON;",
        )?;
        let mut db = Self {
            conn,
            dir: dir.to_owned(),
            path: path.to_owned(),
            user_id: user_id.to_owned(),
            max_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_timelines: MAX_TIMELINES,
            last_access: 0,
        };
        db.migrate(version)?;
        let stored: Option<String> = db.meta(META_USER)?;
        let changed = stored.as_deref().is_some_and(|u| u != user_id);
        if changed {
            db.wipe(true)?;
        }
        if stored.as_deref() != Some(user_id) {
            db.set_meta(META_USER, user_id)?;
        }
        db.last_access = db.conn.query_row(
            "SELECT COALESCE(MAX(accessed), 0) FROM timelines",
            [],
            |r| r.get(0),
        )?;
        Ok((db, changed))
    }

    fn migrate(&mut self, from: i64) -> Result<()> {
        let from = usize::try_from(from).unwrap_or(0);
        if from >= MIGRATIONS.len() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(from) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        }
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.commit()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(super) fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Size limits of the message part (tests use small ones).
    pub fn set_limits(&mut self, max_bytes: u64, max_timelines: usize) {
        self.max_bytes = max_bytes;
        self.max_timelines = max_timelines;
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn tick(&mut self) -> i64 {
        self.last_access = now_ms().max(self.last_access + 1);
        self.last_access
    }

    /// Empties the cached data; the outbox too when `outbox`.
    fn wipe(&mut self, outbox: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM docs; DELETE FROM timelines; DELETE FROM messages;
             DELETE FROM meta WHERE key <> 'user_id';",
        )?;
        if outbox {
            tx.execute("DELETE FROM outbox", [])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// "Clear cache": drops every cached document and message (the outbox of
    /// unsent messages stays) and gives the space back.
    pub fn clear(&mut self) -> Result<()> {
        self.wipe(false)?;
        self.conn
            .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        // An unusable database moved aside earlier holds older cached data.
        let broken = sidecar(&self.path, ".broken");
        match std::fs::remove_file(&broken) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        Ok(())
    }

    // --- writing ------------------------------------------------------------------

    /// Writes `batch` in one transaction, then evicts least recently used
    /// timelines beyond the size limits.
    pub fn write(&mut self, batch: Batch) -> Result<()> {
        let mut stamps = Vec::with_capacity(batch.timelines.len());
        for _ in &batch.timelines {
            stamps.push(self.tick());
        }
        let tx = self.conn.transaction()?;
        let touched_docs = batch.full.is_some()
            || batch.me.is_some()
            || batch.server_info.is_some()
            || batch.settings.is_some()
            || batch.permissions.is_some()
            || batch.roles.is_some()
            || batch.custom_emoji.is_some()
            || batch.custom_statuses.is_some()
            || !batch.rooms.is_empty();
        if let Some(full) = batch.full {
            write_full(&tx, &full)?;
        }
        if let Some(me) = &batch.me {
            put_doc(&tx, KIND_ME, "me", &sanitize::user(me))?;
        }
        if let Some(info) = &batch.server_info {
            put_doc(&tx, KIND_INFO, "info", info)?;
        }
        if let Some(list) = &batch.settings {
            replace_kind(&tx, KIND_SETTING, list.iter().map(|s| (s.id.as_str(), s)))?;
        }
        if let Some(list) = &batch.permissions {
            replace_kind(
                &tx,
                KIND_PERMISSION,
                list.iter().map(|p| (p.id.as_str(), p)),
            )?;
        }
        if let Some(list) = &batch.roles {
            replace_kind(&tx, KIND_ROLE, list.iter().map(|r| (r.id.as_str(), r)))?;
        }
        if let Some(list) = &batch.custom_emoji {
            replace_kind(&tx, KIND_EMOJI, list.iter().map(|e| (e.id.as_str(), e)))?;
        }
        if let Some(list) = &batch.custom_statuses {
            replace_kind(&tx, KIND_STATUS, list.iter().map(|s| (s.id.as_str(), s)))?;
        }
        for (rid, sub, room) in &batch.rooms {
            match sub {
                Some(s) => put_doc(&tx, KIND_SUB, rid, &sanitize::subscription(s))?,
                None => delete_doc(&tx, KIND_SUB, rid)?,
            }
            match room {
                Some(r) => put_doc(&tx, KIND_ROOM, rid, &sanitize::room(r))?,
                None => delete_doc(&tx, KIND_ROOM, rid)?,
            }
            if sub.is_none() && room.is_none() {
                drop_room_timelines(&tx, rid)?;
                drop_room_threads(&tx, rid)?;
            }
        }
        if let Some(threads) = &batch.threads {
            write_threads(&tx, threads)?;
        }
        for (t, stamp) in batch.timelines.iter().zip(stamps) {
            write_timeline(&tx, t, stamp)?;
        }
        if let Some(outbox) = &batch.outbox {
            write_outbox(&tx, outbox)?;
        }
        if let Some(names) = &batch.interrupted_uploads {
            if names.is_empty() {
                tx.execute("DELETE FROM meta WHERE key = ?1", [META_UPLOADS])?;
            } else {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![META_UPLOADS, to_json(names)?],
                )?;
            }
        }
        if touched_docs {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![META_SAVED_AT, now_ms().to_string()],
            )?;
        }
        evict(&tx, self.max_bytes, self.max_timelines)?;
        tx.commit()?;
        Ok(())
    }

    // --- reading ------------------------------------------------------------------

    fn docs<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT json FROM docs WHERE kind = ?1")?;
        let rows = st.query_map([kind], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            if let Some(v) = from_json(kind, &json?) {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// The cached documents.
    pub fn load_state(&self) -> Result<CachedState> {
        Ok(CachedState {
            me: self.docs(KIND_ME)?.into_iter().next(),
            server_info: self.docs(KIND_INFO)?.into_iter().next(),
            settings: self.docs(KIND_SETTING)?,
            permissions: self.docs(KIND_PERMISSION)?,
            roles: self.docs(KIND_ROLE)?,
            subscriptions: self.docs(KIND_SUB)?,
            rooms: self.docs(KIND_ROOM)?,
            custom_emoji: self.docs(KIND_EMOJI)?,
            custom_statuses: self.docs(KIND_STATUS)?,
            saved_at: self.meta(META_SAVED_AT)?.and_then(|s| s.parse().ok()),
            threads: CachedThreads {
                followed: self.docs(KIND_THREAD_ROOM)?,
                parents: self.docs(KIND_THREAD_PARENT)?,
                parents_synced: self.docs(KIND_THREAD_SYNCED)?,
                starred: self.docs(KIND_THREAD_STARRED)?,
            },
        })
    }

    /// The cached messages of a timeline (oldest first); marks it used.
    pub fn load_timeline(&mut self, key: &TimelineKey) -> Result<Option<CachedTimeline>> {
        let k = key.db_key();
        let parent: Option<Option<String>> = self
            .conn
            .query_row("SELECT parent FROM timelines WHERE key = ?1", [&k], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(parent) = parent else {
            return Ok(None);
        };
        let mut messages = Vec::new();
        {
            let mut st = self
                .conn
                .prepare_cached("SELECT json FROM messages WHERE tl = ?1 ORDER BY ts, id")?;
            let rows = st.query_map([&k], |r| r.get::<_, String>(0))?;
            for json in rows {
                if let Some(m) = from_json::<Message>("message", &json?) {
                    messages.push(m);
                }
            }
        }
        let stamp = self.tick();
        self.conn.execute(
            "UPDATE timelines SET accessed = ?1 WHERE key = ?2",
            params![stamp, k],
        )?;
        Ok(Some(CachedTimeline {
            key: key.clone(),
            parent: parent.and_then(|p| from_json("parent", &p)),
            messages,
        }))
    }

    /// Timelines in the cache, most recently used first.
    pub fn timeline_keys(&self) -> Result<Vec<TimelineKey>> {
        let mut st = self
            .conn
            .prepare("SELECT rid, tmid FROM timelines ORDER BY accessed DESC")?;
        let rows = st.query_map([], |r| {
            Ok(TimelineKey {
                rid: r.get(0)?,
                tmid: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The persisted outbox, oldest first.
    pub fn load_outbox(&self) -> Result<Vec<OutboxEntry>> {
        let mut st = self
            .conn
            .prepare("SELECT json, queued, error FROM outbox ORDER BY created, id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (json, queued, error) = row?;
            if let Some(message) = from_json::<Message>("outbox", &json) {
                out.push(OutboxEntry {
                    message,
                    queued: queued != 0,
                    maybe_sent: queued == QUEUED_MAYBE_SENT,
                    error,
                });
            }
        }
        Ok(out)
    }

    /// Uploads interrupted by quitting, reported once.
    pub fn take_interrupted_uploads(&mut self) -> Result<Vec<String>> {
        let names: Vec<String> = self
            .meta(META_UPLOADS)?
            .and_then(|j| from_json("uploads", &j))
            .unwrap_or_default();
        if !names.is_empty() {
            self.conn
                .execute("DELETE FROM meta WHERE key = ?1", [META_UPLOADS])?;
        }
        Ok(names)
    }

    /// Bytes of cached messages (what the eviction limit counts).
    pub fn message_bytes(&self) -> Result<u64> {
        let n: i64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM timelines", [], |r| {
                    r.get(0)
                })?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Bytes on disk (database and journal files).
    pub fn disk_size(&self) -> u64 {
        dir_size(&self.dir)
    }
}

fn put_doc<T: Serialize>(tx: &Transaction<'_>, kind: &str, id: &str, v: &T) -> Result<()> {
    put_json(tx, kind, id, &to_json(v)?)
}

fn put_json(tx: &Transaction<'_>, kind: &str, id: &str, json: &str) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO docs (kind, id, json) VALUES (?1, ?2, ?3)
         ON CONFLICT (kind, id) DO UPDATE SET json = excluded.json",
    )?
    .execute(params![kind, id, json])?;
    Ok(())
}

fn delete_doc(tx: &Transaction<'_>, kind: &str, id: &str) -> Result<()> {
    tx.prepare_cached("DELETE FROM docs WHERE kind = ?1 AND id = ?2")?
        .execute(params![kind, id])?;
    Ok(())
}

fn replace_kind<'a, T: Serialize + 'a>(
    tx: &Transaction<'_>,
    kind: &str,
    items: impl Iterator<Item = (&'a str, &'a T)>,
) -> Result<()> {
    tx.execute("DELETE FROM docs WHERE kind = ?1", [kind])?;
    for (id, v) in items {
        put_doc(tx, kind, id, v)?;
    }
    Ok(())
}

fn write_full(tx: &Transaction<'_>, s: &CachedState) -> Result<()> {
    tx.execute(
        "DELETE FROM docs WHERE kind NOT IN (?1, ?2, ?3, ?4)",
        THREAD_KINDS,
    )?;
    if let Some(me) = &s.me {
        put_doc(tx, KIND_ME, "me", &sanitize::user(me))?;
    }
    if let Some(info) = &s.server_info {
        put_doc(tx, KIND_INFO, "info", info)?;
    }
    for x in &s.settings {
        put_doc(tx, KIND_SETTING, &x.id, x)?;
    }
    for x in &s.permissions {
        put_doc(tx, KIND_PERMISSION, &x.id, x)?;
    }
    for x in &s.roles {
        put_doc(tx, KIND_ROLE, &x.id, x)?;
    }
    for x in &s.subscriptions {
        put_doc(tx, KIND_SUB, &x.rid, &sanitize::subscription(x))?;
    }
    for x in &s.rooms {
        put_doc(tx, KIND_ROOM, &x.id, &sanitize::room(x))?;
    }
    for x in &s.custom_emoji {
        put_doc(tx, KIND_EMOJI, &x.id, x)?;
    }
    for x in &s.custom_statuses {
        put_doc(tx, KIND_STATUS, &x.id, x)?;
    }
    // Messages of rooms the account no longer has.
    let known: HashSet<&str> = s
        .subscriptions
        .iter()
        .map(|x| x.rid.as_str())
        .chain(s.rooms.iter().map(|r| r.id.as_str()))
        .collect();
    let rids: Vec<String> = {
        let mut st = tx.prepare("SELECT DISTINCT rid FROM timelines")?;
        st.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for rid in rids.iter().filter(|r| !known.contains(r.as_str())) {
        drop_room_timelines(tx, rid)?;
    }
    let rids: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT DISTINCT id FROM docs WHERE kind IN (?1, ?3, ?4)
             UNION SELECT DISTINCT json_extract(json, '$.rid') FROM docs WHERE kind = ?2",
        )?;
        st.query_map(THREAD_KINDS, |r| r.get::<_, Option<String>>(0))?
            .filter_map(|r| r.transpose())
            .collect::<rusqlite::Result<_>>()?
    };
    for rid in rids.iter().filter(|r| !known.contains(r.as_str())) {
        drop_room_threads(tx, rid)?;
    }
    Ok(())
}

fn drop_room_threads(tx: &Transaction<'_>, rid: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM docs WHERE (kind IN (?1, ?3, ?4) AND id = ?5)
             OR (kind = ?2 AND json_extract(json, '$.rid') = ?5)",
        params![
            KIND_THREAD_ROOM,
            KIND_THREAD_PARENT,
            KIND_THREAD_SYNCED,
            KIND_THREAD_STARRED,
            rid
        ],
    )?;
    Ok(())
}

/// Replaces the sidebar's thread data; parents pass [`sanitize`] like
/// every message. Only documents that changed are written.
fn write_threads(tx: &Transaction<'_>, t: &CachedThreads) -> Result<()> {
    let mut stored: HashMap<(String, String), String> = {
        let mut st =
            tx.prepare("SELECT kind, id, json FROM docs WHERE kind IN (?1, ?2, ?3, ?4)")?;
        st.query_map(THREAD_KINDS, |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut put = |kind: &str, id: &str, json: String| -> Result<()> {
        if stored.remove(&(kind.to_owned(), id.to_owned())).as_ref() != Some(&json) {
            put_json(tx, kind, id, &json)?;
        }
        Ok(())
    };
    for f in &t.followed {
        put(KIND_THREAD_ROOM, &f.rid, to_json(f)?)?;
    }
    for m in t.parents.iter().filter_map(sanitize::message) {
        put(KIND_THREAD_PARENT, &m.id, to_json(&m)?)?;
    }
    for p in &t.parents_synced {
        put(KIND_THREAD_SYNCED, &p.rid, to_json(p)?)?;
    }
    for s in &t.starred {
        put(KIND_THREAD_STARRED, &s.rid, to_json(s)?)?;
    }
    for (kind, id) in stored.into_keys() {
        delete_doc(tx, &kind, &id)?;
    }
    Ok(())
}

fn drop_room_timelines(tx: &Transaction<'_>, rid: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM messages WHERE tl IN (SELECT key FROM timelines WHERE rid = ?1)",
        [rid],
    )?;
    tx.execute("DELETE FROM timelines WHERE rid = ?1", [rid])?;
    Ok(())
}

fn write_timeline(tx: &Transaction<'_>, t: &CachedTimeline, stamp: i64) -> Result<()> {
    let key = t.key.db_key();
    let mut messages: Vec<Message> = t
        .messages
        .iter()
        .filter(|m| m.rid == t.key.rid)
        .filter_map(sanitize::message)
        .collect();
    messages.sort_by(|a, b| ts_ms(a).cmp(&ts_ms(b)).then_with(|| a.id.cmp(&b.id)));
    messages.dedup_by(|a, b| a.id == b.id);
    let skip = messages.len().saturating_sub(MESSAGES_PER_TIMELINE);
    let parent = match t.parent.as_ref().and_then(sanitize::message) {
        Some(p) => Some(to_json(&p)?),
        None => None,
    };
    tx.execute("DELETE FROM messages WHERE tl = ?1", [&key])?;
    let mut bytes = parent.as_ref().map_or(0, String::len);
    let mut kept = 0usize;
    {
        let mut st = tx.prepare_cached(
            "INSERT OR REPLACE INTO messages (tl, id, ts, json) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for m in messages.iter().skip(skip) {
            let json = to_json(m)?;
            bytes += json.len();
            kept += 1;
            st.execute(params![key, m.id, ts_ms(m), json])?;
        }
    }
    if kept == 0 && parent.is_none() {
        tx.execute("DELETE FROM timelines WHERE key = ?1", [&key])?;
        return Ok(());
    }
    tx.execute(
        "INSERT INTO timelines (key, rid, tmid, parent, accessed, bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (key) DO UPDATE SET parent = excluded.parent,
             accessed = excluded.accessed, bytes = excluded.bytes",
        params![key, t.key.rid, t.key.tmid, parent, stamp, bytes as i64],
    )?;
    Ok(())
}

/// Encrypted messages are never persisted in the outbox (see
/// `docs/e2e.md`, "Offline cache"); the caller filters them, this is the
/// last check: encrypted shapes, and messages of a room stored as
/// encrypted, are skipped.
fn write_outbox(tx: &Transaction<'_>, outbox: &[OutboxEntry]) -> Result<()> {
    tx.execute("DELETE FROM outbox", [])?;
    let mut st = tx.prepare_cached(
        "INSERT OR REPLACE INTO outbox (id, rid, created, json, queued, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut encrypted_room = tx.prepare_cached(
        "SELECT 1 FROM docs WHERE kind = ?1 AND id = ?2
             AND json_extract(json, '$.encrypted') = 1",
    )?;
    for e in outbox {
        let m = &e.message;
        if m.is_encrypted() || m.e2e.is_some() || m.id.is_empty() {
            continue;
        }
        if encrypted_room.exists(params![KIND_ROOM, m.rid])? {
            continue;
        }
        let queued: i64 = match (e.queued, e.maybe_sent) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => QUEUED_MAYBE_SENT,
        };
        st.execute(params![m.id, m.rid, ts_ms(m), to_json(m)?, queued, e.error])?;
    }
    Ok(())
}

/// Drops least recently used timelines until the message bytes and the
/// timeline count are within the limits.
fn evict(tx: &Transaction<'_>, max_bytes: u64, max_timelines: usize) -> Result<()> {
    let rows: Vec<(String, i64)> = {
        let mut st =
            tx.prepare_cached("SELECT key, bytes FROM timelines ORDER BY accessed DESC")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut total: u64 = 0;
    for (i, (key, bytes)) in rows.iter().enumerate() {
        total += u64::try_from(*bytes).unwrap_or(0);
        if i >= max_timelines || total > max_bytes {
            tx.execute("DELETE FROM messages WHERE tl = ?1", [key])?;
            tx.execute("DELETE FROM timelines WHERE key = ?1", [key])?;
        }
    }
    Ok(())
}

/// Bytes of the files in `dir` (not recursive).
pub fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()?.metadata().ok())
                .filter(std::fs::Metadata::is_file)
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Deletes a server's cache directory (logout, server removal).
pub fn destroy(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}
