//! Single instance over a named pipe (Windows).
//!
//! The pipe name holds a per-user secret ([`super::pipe_name`]); remote
//! clients are rejected. A second invocation opens the pipe as a plain file
//! (no async runtime exists yet in `main`), writes the payload and exits. When
//! nobody listens, the process becomes primary; the pipe server is created
//! later, inside the iced subscription, because a tokio pipe server is bound
//! to the runtime that created it. Until then, and if two instances start at
//! the same moment, a second launch can also become primary: an accepted,
//! rare race (both windows then run side by side).

use std::io::{self, Write};
use std::time::Duration;

use iced::futures::SinkExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

use super::{Instance, encode_payload, read_payload};

/// The pipe name.
pub type Endpoint = String;

/// The pipe name the subscription serves.
pub struct Listener(String);

/// Nothing to clean up: the pipe disappears with its last server handle.
pub struct Guard;

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// The pipe name from the secret in the local data directory; `None` (with a
/// warning) when it cannot be read or created.
pub fn endpoint() -> Option<String> {
    let dirs = directories::ProjectDirs::from("", "", "rustket")?;
    match super::pipe_name::load_or_create(dirs.data_local_dir()) {
        Ok(name) => Some(name),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read or create the single-instance secret");
            None
        }
    }
}

/// Forwards `urls` to the running instance or becomes primary.
pub fn acquire(name: &str, urls: &[String]) -> io::Result<Instance> {
    // Busy: the primary is between two clients; it creates the next pipe
    // instance right after accepting one.
    for _ in 0..30 {
        match std::fs::OpenOptions::new().write(true).open(name) {
            Ok(mut pipe) => {
                pipe.write_all(&encode_payload(urls))?;
                return Ok(Instance::Forwarded);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Ok(Instance::Primary(Listener(name.to_owned()), Guard));
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "the running instance does not accept connections",
    ))
}

fn server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .access_inbound(true)
        .access_outbound(false)
        .pipe_mode(PipeMode::Byte)
        .create(name)
}

/// Accept loop: a new pipe instance is created before each client is read,
/// clients are handled concurrently.
pub(super) async fn serve_listener(
    listener: Listener,
    out: iced::futures::channel::mpsc::Sender<Vec<String>>,
) {
    let name = listener.0;
    // First instance: fails if anybody else already owns the name.
    let mut next = match server(&name, true) {
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
        tokio::spawn(async move {
            if let Some(args) = read_payload(connected).await {
                let _ = out.send(args).await;
            }
        });
    }
}
