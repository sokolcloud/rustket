//! Single instance over a Unix socket: a second invocation forwards its URL
//! arguments to the running instance and exits.
//!
//! The socket lives in a directory only the user can access:
//! `$XDG_RUNTIME_DIR` (checked: owned by the user, mode 0700) or a private
//! `<cache dir>/rustket/run` created with mode 0700. There is no fallback to a
//! shared directory; without a private one the app runs without single-instance.
//! Both sides check that the peer is the same user.

use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::Subscription;
use iced::futures::{SinkExt, Stream};

/// Maximum bytes accepted from one client.
const MAX_PAYLOAD: u64 = 64 * 1024;

/// The current user's uid (owner of `/proc/self`).
pub fn current_uid() -> io::Result<u32> {
    std::fs::metadata("/proc/self").map(|m| m.uid())
}

/// A directory owned by `uid` that nobody else can enter.
pub fn is_private_dir(path: &Path, uid: u32) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.permissions().mode() & 0o077 == 0)
}

/// `$XDG_RUNTIME_DIR/rustket.sock` when that directory is private, else
/// `<cache dir>/rustket/run/rustket.sock` (created 0700). `None` when neither
/// is usable.
pub fn socket_path() -> Option<PathBuf> {
    let uid = current_uid().ok()?;
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        if is_private_dir(&dir, uid) {
            return Some(dir.join("rustket.sock"));
        }
        tracing::warn!("XDG_RUNTIME_DIR is not private; not using it");
    }
    let base = directories::ProjectDirs::from("", "", "rustket")?
        .cache_dir()
        .to_owned();
    private_run_dir(&base, uid).map(|d| d.join("rustket.sock"))
}

/// `<base>/run`, created with mode 0700 and checked.
pub fn private_run_dir(base: &Path, uid: u32) -> Option<PathBuf> {
    let dir = base.join("run");
    crate::images::private_dir(&dir).ok()?;
    is_private_dir(&dir, uid).then_some(dir)
}

pub enum Instance {
    /// This process is the primary instance: it owns the listener and holds
    /// the instance lock (keep the `File` alive for the process lifetime; only
    /// the lock holder may remove the socket).
    Primary(UnixListener, std::fs::File),
    /// The arguments were handed to the running instance.
    Forwarded,
}

fn forward(path: &Path, urls: &[String]) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut payload = urls.join("\n");
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)
}

/// Becomes the primary instance or forwards `urls` (one per line) to it.
///
/// An exclusive lock on `rustket.lock` next to the socket decides who is
/// primary, so two simultaneous launches never both run and nobody removes a
/// socket another instance just bound. The directory must be private and the
/// socket owned by this user.
pub fn acquire(path: &Path, urls: &[String]) -> io::Result<Instance> {
    use std::os::unix::fs::OpenOptionsExt;
    let uid = current_uid()?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("socket path has no directory"))?;
    if !is_private_dir(parent, uid) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket directory is not private",
        ));
    }
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && meta.uid() != uid
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket owned by another user",
        ));
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(parent.join("rustket.lock"))?;
    // The primary may still be binding right after taking the lock: retry.
    for _ in 0..30 {
        match lock.try_lock() {
            Ok(()) => {
                // No other primary can exist now: a socket file is stale.
                if std::fs::symlink_metadata(path).is_ok() {
                    std::fs::remove_file(path)?;
                }
                let listener = UnixListener::bind(path)?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                return Ok(Instance::Primary(listener, lock));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if forward(path, urls).is_ok() {
                    return Ok(Instance::Forwarded);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the running instance does not accept connections",
    ))
}

/// Reads the newline-separated arguments a client sent.
pub fn parse_payload(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The listener, handed to the subscription exactly once.
#[derive(Clone)]
pub struct ListenerSlot(pub Arc<Mutex<Option<UnixListener>>>);

impl ListenerSlot {
    pub fn new(listener: UnixListener) -> Self {
        Self(Arc::new(Mutex::new(Some(listener))))
    }
}

impl std::hash::Hash for ListenerSlot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "rustket-single-instance".hash(state);
    }
}

/// Publishes the arguments of every forwarded invocation.
pub fn subscription(slot: ListenerSlot) -> Subscription<Vec<String>> {
    Subscription::run_with(slot, listen)
}

/// Reads one client: same uid required, bounded size and time.
async fn read_client(stream: tokio::net::UnixStream, uid: u32) -> Option<Vec<String>> {
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == uid => {}
        Ok(cred) => {
            tracing::warn!(
                peer_uid = cred.uid(),
                "rejected single-instance client of another user"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot check single-instance client");
            return None;
        }
    }
    let read = tokio::time::timeout(Duration::from_secs(5), async move {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        stream
            .take(MAX_PAYLOAD)
            .read_to_end(&mut buf)
            .await
            .map(|_| buf)
    })
    .await;
    match read {
        // An empty list (a launch without links) raises the window.
        Ok(Ok(buf)) => Some(parse_payload(&buf)),
        _ => None,
    }
}

fn listen(slot: &ListenerSlot) -> impl Stream<Item = Vec<String>> + use<> {
    let taken = slot.0.lock().ok().and_then(|mut g| g.take());
    iced::stream::channel(16, async move |out| {
        let Some(std_listener) = taken else { return };
        let Ok(uid) = current_uid() else { return };
        if std_listener.set_nonblocking(true).is_err() {
            return;
        }
        let Ok(listener) = tokio::net::UnixListener::from_std(std_listener) else {
            return;
        };
        serve(listener, uid, out).await;
    })
}

/// Accept loop: clients are handled concurrently; accept errors back off.
pub async fn serve(
    listener: tokio::net::UnixListener,
    uid: u32,
    out: iced::futures::channel::mpsc::Sender<Vec<String>>,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::warn!(error = %e, "single-instance accept failed");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        if out.is_closed() {
            return;
        }
        let mut out = out.clone();
        tokio::spawn(async move {
            if let Some(args) = read_client(stream, uid).await {
                let _ = out.send(args).await;
            }
        });
    }
}

/// Blocking read of one client (tests).
pub fn read_one(listener: &UnixListener) -> io::Result<Vec<String>> {
    let (stream, _) = listener.accept()?;
    let mut buf = Vec::new();
    stream.take(MAX_PAYLOAD).read_to_end(&mut buf)?;
    Ok(parse_payload(&buf))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn private_tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let run = private_run_dir(dir.path(), current_uid().unwrap()).unwrap();
        (dir, run)
    }

    #[test]
    fn forwards_to_primary() {
        let (_d, run) = private_tmp();
        let path = run.join("rustket.sock");
        let Instance::Primary(listener, _lock) = acquire(&path, &[]).unwrap() else {
            panic!("first instance must be primary");
        };
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let p2 = path.clone();
        let client = std::thread::spawn(move || {
            matches!(
                acquire(&p2, &["rocketchat://room?host=a&rid=1".into(), "x".into()]).unwrap(),
                Instance::Forwarded
            )
        });
        let got = read_one(&listener).unwrap();
        assert!(client.join().unwrap());
        assert_eq!(got, vec!["rocketchat://room?host=a&rid=1", "x"]);
    }

    #[test]
    fn replaces_stale_socket() {
        let (_d, run) = private_tmp();
        let path = run.join("rustket.sock");
        {
            let _l = UnixListener::bind(&path).unwrap();
        } // dropped: file stays, nobody listens
        assert!(path.exists());
        assert!(matches!(
            acquire(&path, &[]).unwrap(),
            Instance::Primary(..)
        ));
    }

    /// Review #11: never use a directory others can enter.
    #[test]
    fn refuses_shared_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("rustket.sock");
        assert!(acquire(&path, &[]).is_err());
        assert!(acquire(Path::new("/tmp/rustket-test.sock"), &[]).is_err());
        let uid = current_uid().unwrap();
        assert!(!is_private_dir(Path::new("/tmp"), uid));
        let (_d, run) = private_tmp();
        assert!(is_private_dir(&run, uid));
        assert_eq!(
            std::fs::metadata(&run).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// The production async listener: peer check, concurrent clients.
    #[tokio::test]
    async fn async_listener_receives_arguments() {
        let (_d, run) = private_tmp();
        let path = run.join("rustket.sock");
        let Instance::Primary(listener, _lock) = acquire(&path, &[]).unwrap() else {
            panic!("primary expected");
        };
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let (tx, mut rx) = iced::futures::channel::mpsc::channel(4);
        let server = tokio::spawn(serve(listener, current_uid().unwrap(), tx));
        // A silent client must not block the next one.
        let _idle = tokio::net::UnixStream::connect(&path).await.unwrap();
        let p = path.clone();
        tokio::task::spawn_blocking(move || acquire(&p, &["rocketchat://a".into()]).unwrap())
            .await
            .unwrap();
        let got = tokio::time::timeout(
            Duration::from_secs(3),
            iced::futures::StreamExt::next(&mut rx),
        )
        .await
        .unwrap();
        assert_eq!(got, Some(vec!["rocketchat://a".to_owned()]));
        server.abort();
    }

    /// Re-review #15: simultaneous launches elect exactly one primary.
    #[test]
    fn simultaneous_launches_elect_one_primary() {
        let (_d, run) = private_tmp();
        let path = run.join("rustket.sock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let (p, b) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    match acquire(&p, &[format!("arg{i}")]).unwrap() {
                        Instance::Primary(l, lock) => {
                            // Serve the others, then report.
                            let mut got = Vec::new();
                            for _ in 0..3 {
                                got.extend(read_one(&l).unwrap());
                            }
                            drop(lock);
                            Some(got)
                        }
                        Instance::Forwarded => None,
                    }
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let primaries: Vec<_> = results.iter().flatten().collect();
        assert_eq!(primaries.len(), 1);
        assert_eq!(primaries[0].len(), 3);
    }

    #[test]
    fn payload_parsing() {
        assert_eq!(parse_payload(b"a\n\n b \r\n"), vec!["a", "b"]);
        assert!(parse_payload(b"").is_empty());
    }
}
