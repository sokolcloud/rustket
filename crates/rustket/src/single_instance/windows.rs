//! Single instance over a named pipe (Windows).
//!
//! Everything that decides whom to trust lives in the user's local data
//! directory, whose ACL keeps other users out:
//!
//! - `instance.lock`: an exclusive lock held by the primary instance. A launch
//!   that gets the lock is primary and never forwards; only a launch that
//!   finds it held looks for a pipe.
//! - `instance-pipe` ([`super::pipe_name`]): the pipe name of this run.
//! - `instance-key` ([`super::pipe_auth`]): the key both ends prove before
//!   any argument crosses the pipe.
//!
//! Pipe names are machine-wide and listable, and another user can create a
//! pipe under a name they saw, so the name is trusted by neither side: a
//! second invocation sends its arguments (they can carry OAuth credential
//! secrets) only to a server that answered its challenge, and the primary
//! accepts them only from a client that answered its own. Remote clients are
//! rejected and the client lets the server identify, not impersonate, it.
//!
//! A second invocation opens the pipe as a plain file (no async runtime exists
//! yet in `main`). The pipe server is created later, inside the iced
//! subscription, because a tokio pipe server is bound to the runtime that
//! created it; until then second launches wait and retry.

use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::futures::SinkExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

use super::pipe_auth::{self, Key};
use super::{Instance, encode_payload, pipe_name};

/// The local data directory holding the lock, the key and the pipe name.
pub type Endpoint = PathBuf;

/// What the subscription serves.
pub struct Listener {
    dir: PathBuf,
    name: String,
    key: Key,
}

/// Held by the primary instance: the instance lock. The pipe disappears with
/// its last server handle.
pub struct Guard {
    _lock: std::fs::File,
}

const LOCK_FILE: &str = "instance.lock";
/// `SECURITY_IDENTIFICATION`: the pipe server may query who the client is but
/// cannot act as that user.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
/// How long a second launch waits for the primary to serve its pipe.
const FORWARD_DEADLINE: Duration = Duration::from_secs(5);
/// Bound for one authenticated exchange with the pipe server.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_millis(100);

/// The local data directory; `None` when the system reports no profile.
pub fn endpoint() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "rustket")?;
    Some(dirs.data_local_dir().to_owned())
}

/// Becomes the primary instance or forwards `urls` to it.
///
/// An exclusive lock on `instance.lock` decides who is primary, never the
/// existence of a pipe: a pipe created by somebody else while no instance
/// runs receives nothing.
pub fn acquire(dir: &Path, urls: &[String]) -> io::Result<Instance> {
    acquire_within(dir, urls, FORWARD_DEADLINE)
}

fn acquire_within(dir: &Path, urls: &[String], wait: Duration) -> io::Result<Instance> {
    crate::images::private_dir(dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    let payload = encode_payload(urls);
    let deadline = Instant::now() + wait;
    // The primary may still be starting (its pipe is created by the UI
    // runtime) or be between two clients: retry.
    loop {
        match lock.try_lock() {
            Ok(()) => {
                let key = pipe_auth::load_or_create(dir)?;
                // A name nobody has seen yet, published before the pipe exists.
                let name = pipe_name::rotate(dir)?;
                let listener = Listener {
                    dir: dir.to_owned(),
                    name,
                    key,
                };
                return Ok(Instance::Primary(listener, Guard { _lock: lock }));
            }
            Err(std::fs::TryLockError::WouldBlock) => match forward(dir, &payload) {
                Ok(()) => return Ok(Instance::Forwarded),
                Err(e) => tracing::debug!(error = %e, "single-instance pipe not usable yet"),
            },
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the running instance does not accept connections",
            ));
        }
        std::thread::sleep(RETRY_DELAY);
    }
}

/// One attempt to hand `payload` to the primary's pipe. Nothing but a random
/// nonce is written until the server proved it has the key.
fn forward(dir: &Path, payload: &[u8]) -> io::Result<()> {
    let key = pipe_auth::load(dir).ok_or_else(|| io::Error::other("no single-instance key yet"))?;
    let name = pipe_name::current(dir)
        .ok_or_else(|| io::Error::other("no single-instance pipe name yet"))?;
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(&name)?;
    // Pipe reads have no timeout: a server that never answers must not hang
    // this process, so the exchange runs on a helper thread.
    let payload = payload.to_vec();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(pipe_auth::forward(&mut pipe, &key, &payload));
    });
    result.recv_timeout(EXCHANGE_TIMEOUT).map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "the single-instance pipe server does not answer",
        )
    })?
}

fn server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .access_inbound(true)
        .access_outbound(true)
        .pipe_mode(PipeMode::Byte)
        .create(name)
}

/// The first pipe instance. Creation fails when the name already exists:
/// somebody else serves it, so a new name is published and tried instead.
fn first_server(dir: &Path, name: &mut String) -> io::Result<NamedPipeServer> {
    let mut retries = 2;
    loop {
        match server(name, true) {
            Ok(s) => return Ok(s),
            Err(e) if retries > 0 => {
                retries -= 1;
                tracing::warn!(error = %e, "single-instance pipe name is taken; trying a new one");
                *name = pipe_name::rotate(dir)?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Accept loop: a new pipe instance is created before each client is read,
/// clients are handled concurrently.
pub(super) async fn serve_listener(
    listener: Listener,
    out: iced::futures::channel::mpsc::Sender<Vec<String>>,
) {
    let Listener { dir, mut name, key } = listener;
    let mut next = match first_server(&dir, &mut name) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot create the single-instance pipe");
            return;
        }
    };
    loop {
        if let Err(e) = next.connect().await {
            tracing::warn!(error = %e, "single-instance pipe connect failed");
            tokio::time::sleep(Duration::from_millis(500)).await;
            match server(&name, false) {
                Ok(s) => next = s,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot recreate the single-instance pipe");
                    return;
                }
            }
            continue;
        }
        let connected = match server(&name, false) {
            Ok(s) => std::mem::replace(&mut next, s),
            Err(e) => {
                tracing::warn!(error = %e, "cannot create the next single-instance pipe");
                return;
            }
        };
        if out.is_closed() {
            return;
        }
        let mut out = out.clone();
        let key = key.clone();
        tokio::spawn(async move {
            if let Some(args) = pipe_auth::accept(connected, &key).await {
                let _ = out.send(args).await;
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const AUTH_LINK: &str =
        "rocketchat://auth?host=chat.example&credentialToken=tok&credentialSecret=sec";

    /// A pipe server of somebody without the key: answers the challenge with
    /// zeros and returns every byte it received.
    async fn rogue(mut server: NamedPipeServer) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut seen = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            server.connect().await?;
            let mut nonce = [0u8; 32];
            server.read_exact(&mut nonce).await?;
            seen.extend_from_slice(&nonce);
            server.write_all(&[0u8; 64]).await?;
            server.read_to_end(&mut seen).await?;
            Ok::<_, io::Error>(())
        })
        .await;
        seen
    }

    fn primary(dir: &Path) -> (Listener, Guard) {
        match acquire(dir, &[]).unwrap() {
            Instance::Primary(listener, guard) => (listener, guard),
            Instance::Forwarded => panic!("first instance must be primary"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forwards_to_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let (listener, _guard) = primary(&dir);
        let (tx, mut rx) = iced::futures::channel::mpsc::channel(4);
        let serving = tokio::spawn(serve_listener(listener, tx));
        let d = dir.clone();
        let second = tokio::task::spawn_blocking(move || acquire(&d, &[AUTH_LINK.to_owned()]))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(second, Instance::Forwarded));
        let got = tokio::time::timeout(
            Duration::from_secs(3),
            iced::futures::StreamExt::next(&mut rx),
        )
        .await
        .unwrap();
        assert_eq!(got, Some(vec![AUTH_LINK.to_owned()]));
        serving.abort();
    }

    /// F1: a pipe somebody else created under a known name, while no
    /// instance runs, is never written to: the launch becomes primary under
    /// a new name.
    #[test]
    fn squatted_stale_name_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        // What an earlier run left behind and exposed in the pipe namespace.
        let stale = pipe_name::rotate(&dir).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let _squat = ServerOptions::new().create(&stale).unwrap();
        let (listener, _guard) = match acquire(&dir, &[AUTH_LINK.to_owned()]).unwrap() {
            Instance::Primary(listener, guard) => (listener, guard),
            Instance::Forwarded => panic!("forwarded to a pipe without a primary"),
        };
        assert_ne!(listener.name, stale);
        assert_eq!(pipe_name::current(&dir), Some(listener.name.clone()));
    }

    /// F1: while the primary runs, a server on the published name that does
    /// not have the key gets no auth link.
    #[tokio::test(flavor = "multi_thread")]
    async fn rogue_server_gets_no_auth_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let (listener, _guard) = primary(&dir);
        // The primary's own server is not up; somebody else serves the name.
        let squat = ServerOptions::new().create(&listener.name).unwrap();
        let seen = tokio::spawn(rogue(squat));
        let d = dir.clone();
        let second = tokio::task::spawn_blocking(move || {
            acquire_within(&d, &[AUTH_LINK.to_owned()], Duration::from_millis(500))
        })
        .await
        .unwrap();
        assert!(second.is_err(), "nothing to forward to");
        let seen = seen.await.unwrap();
        assert_eq!(seen.len(), 32, "only the nonce");
        assert!(!seen.windows(6).any(|w| w == b"secret" || w == b"Secret"));
    }

    /// F1: a taken name does not leave the primary without a listener.
    #[tokio::test(flavor = "multi_thread")]
    async fn taken_name_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let (listener, _guard) = primary(&dir);
        let taken = listener.name.clone();
        let _squat = ServerOptions::new().create(&taken).unwrap();
        let (tx, mut rx) = iced::futures::channel::mpsc::channel(4);
        let serving = tokio::spawn(serve_listener(listener, tx));
        for _ in 0..50 {
            if pipe_name::current(&dir).is_some_and(|name| name != taken) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_ne!(pipe_name::current(&dir), Some(taken));
        let d = dir.clone();
        let second = tokio::task::spawn_blocking(move || acquire(&d, &[AUTH_LINK.to_owned()]))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(second, Instance::Forwarded));
        let got = tokio::time::timeout(
            Duration::from_secs(3),
            iced::futures::StreamExt::next(&mut rx),
        )
        .await
        .unwrap();
        assert_eq!(got, Some(vec![AUTH_LINK.to_owned()]));
        serving.abort();
    }
}
