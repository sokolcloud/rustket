//! Single instance: a second invocation forwards its URL arguments to the
//! running instance and exits.
//!
//! - Linux and macOS ([`unix`]): a Unix socket in a private directory.
//! - Windows (`windows`): a named pipe whose name holds a per-user secret.
//!
//! Both use the same wire format: the arguments, one per line
//! ([`encode_payload`], [`parse_payload`]); an empty payload (a launch without
//! links) raises the window.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::Subscription;
use iced::futures::Stream;

#[cfg_attr(not(windows), allow(dead_code))]
pub mod pipe_name;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

pub use sys::{Endpoint, Guard, Listener, acquire, endpoint};

/// Maximum bytes accepted from one client.
pub const MAX_PAYLOAD: u64 = 64 * 1024;

pub enum Instance {
    /// This process is the primary instance: it owns the listener. Keep the
    /// guard alive for the process lifetime; dropping it cleans up.
    Primary(Listener, Guard),
    /// The arguments were handed to the running instance.
    Forwarded,
}

/// The bytes a second invocation sends: one argument per line.
pub fn encode_payload(urls: &[String]) -> Vec<u8> {
    let mut payload = urls.join("\n");
    payload.push('\n');
    payload.into_bytes()
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

/// Reads one client's payload: bounded size and time.
pub async fn read_payload<R>(stream: R) -> Option<Vec<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let read = tokio::time::timeout(Duration::from_secs(5), async move {
        let mut buf = Vec::new();
        stream
            .take(MAX_PAYLOAD)
            .read_to_end(&mut buf)
            .await
            .map(|_| buf)
    })
    .await;
    match read {
        Ok(Ok(buf)) => Some(parse_payload(&buf)),
        _ => None,
    }
}

/// The listener, handed to the subscription exactly once.
#[derive(Clone)]
pub struct ListenerSlot(pub Arc<Mutex<Option<Listener>>>);

impl ListenerSlot {
    pub fn new(listener: Listener) -> Self {
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

fn listen(slot: &ListenerSlot) -> impl Stream<Item = Vec<String>> + use<> {
    let taken = slot.0.lock().ok().and_then(|mut g| g.take());
    iced::stream::channel(16, async move |out| {
        if let Some(listener) = taken {
            sys::serve_listener(listener, out).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_format() {
        let urls = vec!["rocketchat://room?host=a&rid=1".to_owned(), "x".to_owned()];
        assert_eq!(
            encode_payload(&urls),
            b"rocketchat://room?host=a&rid=1\nx\n"
        );
        assert_eq!(parse_payload(&encode_payload(&urls)), urls);
        assert_eq!(encode_payload(&[]), b"\n");
        assert!(parse_payload(&encode_payload(&[])).is_empty());
        assert_eq!(parse_payload(b"a\n\n b \r\n"), vec!["a", "b"]);
        assert!(parse_payload(b"").is_empty());
    }

    #[tokio::test]
    async fn payload_is_bounded() {
        let big = vec![b'a'; MAX_PAYLOAD as usize + 10];
        let got = read_payload(&big[..]).await;
        assert_eq!(got.map(|v| v[0].len()), Some(MAX_PAYLOAD as usize));
    }
}
