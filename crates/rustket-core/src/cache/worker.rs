//! The cache worker: one thread per open cache owns the [`CacheDb`]; the UI
//! sends it batches (fire and forget, coalesced while it is busy) and awaits
//! reads through oneshot channels.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use tokio::sync::oneshot;

use super::db::destroy;
use super::{
    Batch, CacheDb, CacheError, CachedState, CachedTimeline, OpenReport, OutboxEntry, Result,
    TimelineKey,
};

enum Cmd {
    Write(Box<Batch>),
    Timeline(TimelineKey, oneshot::Sender<Option<CachedTimeline>>),
    Clear(oneshot::Sender<Result<()>>),
    Size(oneshot::Sender<u64>),
    /// Answered once everything sent before it is written.
    Flush(mpsc::Sender<()>),
    Destroy(oneshot::Sender<std::io::Result<()>>),
}

/// A cache running on its worker thread. Cloning shares the worker; it
/// stops when the last handle is dropped.
#[derive(Clone)]
pub struct CacheHandle {
    tx: mpsc::Sender<Cmd>,
    dir: PathBuf,
}

impl std::fmt::Debug for CacheHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheHandle")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// What opening a cache found.
#[derive(Debug, Clone)]
pub struct Opened {
    pub handle: CacheHandle,
    pub state: CachedState,
    pub outbox: Vec<OutboxEntry>,
    /// Uploads interrupted when the app quit last time (reported once).
    pub interrupted_uploads: Vec<String>,
    /// The timeline asked for with the open (the room shown first).
    pub preloaded: Option<CachedTimeline>,
    pub report: OpenReport,
}

impl CacheHandle {
    /// Opens the cache of `user_id` in `dir` on a new worker thread and
    /// reads what the first frame needs.
    pub async fn open(
        dir: PathBuf,
        user_id: String,
        preload: Option<TimelineKey>,
    ) -> Result<Opened> {
        let (reply, opened) = oneshot::channel();
        std::thread::Builder::new()
            .name("rustket-cache".into())
            .spawn(move || run(dir, user_id, preload, reply))?;
        opened.await.map_err(|_| CacheError::Closed)?
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Queues a batch (empty batches are dropped).
    pub fn write(&self, batch: Batch) {
        if !batch.is_empty() {
            let _ = self.tx.send(Cmd::Write(Box::new(batch)));
        }
    }

    /// The cached messages of a timeline.
    pub async fn timeline(&self, key: TimelineKey) -> Option<CachedTimeline> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Timeline(key, reply)).ok()?;
        rx.await.ok().flatten()
    }

    /// Drops every cached document and message (the outbox stays).
    pub async fn clear(&self) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Clear(reply))
            .map_err(|_| CacheError::Closed)?;
        rx.await.map_err(|_| CacheError::Closed)?
    }

    /// Bytes on disk.
    pub async fn size(&self) -> u64 {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Size(reply)).is_err() {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    /// Blocks until everything queued so far is written, at most `timeout`
    /// (quitting). Returns whether it finished.
    pub fn flush_blocking(&self, timeout: Duration) -> bool {
        let (reply, rx) = mpsc::channel();
        if self.tx.send(Cmd::Flush(reply)).is_err() {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }

    /// Stops the worker after the queued writes and deletes the cache
    /// directory (logout, server removal). Other clones stop working.
    pub async fn destroy(self) -> std::io::Result<()> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Destroy(reply)).is_err() {
            let dir = self.dir.clone();
            return tokio::task::spawn_blocking(move || destroy(&dir))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)));
        }
        rx.await
            .unwrap_or_else(|_| Err(std::io::Error::other("cache worker stopped")))
    }
}

/// What [`open_db`] reads besides the database itself.
type Loaded = (
    CachedState,
    Vec<OutboxEntry>,
    Vec<String>,
    Option<CachedTimeline>,
    OpenReport,
);

fn open_db(dir: &Path, user_id: &str, preload: Option<TimelineKey>) -> Result<(CacheDb, Loaded)> {
    let (mut db, report) = CacheDb::open(dir, user_id)?;
    let state = db.load_state()?;
    let outbox = db.load_outbox()?;
    let uploads = db.take_interrupted_uploads()?;
    let preloaded = match preload {
        Some(k) => db.load_timeline(&k)?,
        None => None,
    };
    Ok((db, (state, outbox, uploads, preloaded, report)))
}

fn run(
    dir: PathBuf,
    user_id: String,
    preload: Option<TimelineKey>,
    reply: oneshot::Sender<Result<Opened>>,
) {
    let (mut db, (state, outbox, interrupted_uploads, preloaded, report)) =
        match open_db(&dir, &user_id, preload) {
            Ok(v) => v,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
    let (tx, rx) = mpsc::channel();
    let opened = Opened {
        handle: CacheHandle {
            tx,
            dir: dir.clone(),
        },
        state,
        outbox,
        interrupted_uploads,
        preloaded,
        report,
    };
    if reply.send(Ok(opened)).is_err() {
        return;
    }
    let mut next: Option<Cmd> = None;
    loop {
        let cmd = match next.take() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            },
        };
        match cmd {
            Cmd::Write(b) => {
                // Coalesce what queued up meanwhile; other commands keep
                // their order.
                let mut batch = *b;
                while let Ok(c) = rx.try_recv() {
                    match c {
                        Cmd::Write(more) => batch.merge(*more),
                        other => {
                            next = Some(other);
                            break;
                        }
                    }
                }
                if let Err(e) = db.write(batch) {
                    tracing::warn!(error = %e, "offline cache write failed");
                    // A database that broke while running starts over.
                    if let Ok((fresh, report)) = CacheDb::open(&dir, &user_id)
                        && report.moved_aside.is_some()
                    {
                        db = fresh;
                    }
                }
            }
            Cmd::Timeline(key, reply) => {
                let t = db
                    .load_timeline(&key)
                    .inspect_err(|e| tracing::warn!(error = %e, "offline cache read failed"))
                    .ok()
                    .flatten();
                let _ = reply.send(t);
            }
            Cmd::Clear(reply) => {
                let _ = reply.send(db.clear());
            }
            Cmd::Size(reply) => {
                let _ = reply.send(db.disk_size());
            }
            Cmd::Flush(reply) => {
                let _ = reply.send(());
            }
            Cmd::Destroy(reply) => {
                drop(db);
                let _ = reply.send(destroy(&dir));
                return;
            }
        }
    }
}
