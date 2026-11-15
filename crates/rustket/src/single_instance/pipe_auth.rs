//! Mutual authentication over the Windows single-instance pipe.
//!
//! Pipe names are listable by every local user and another user can create a
//! pipe under a name they have seen, so neither end trusts the name. Both
//! prove knowledge of a per-user key kept in the local data directory (whose
//! ACL keeps other users out) before any argument is sent or accepted:
//!
//! 1. client → server: `client_nonce`
//! 2. server → client: `HMAC(key, "server" ‖ client_nonce)`, `server_nonce`
//! 3. client → server: `HMAC(key, "client" ‖ client_nonce ‖ server_nonce)`,
//!    then the payload
//!
//! The key never appears in a pipe name. Platform-neutral so it is tested
//! everywhere.

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// File in the local data directory holding the key (hex).
pub const KEY_FILE: &str = "instance-key";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 32;
const TAG_LEN: usize = 32;
const SERVER_LABEL: &[u8] = b"rustket single-instance v1 server";
const CLIENT_LABEL: &[u8] = b"rustket single-instance v1 client";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// The per-user key. Deliberately not `Debug`.
#[derive(Clone)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    /// A new random key.
    pub fn generate() -> Self {
        Self(random_bytes())
    }

    fn from_hex(text: &str) -> Option<Self> {
        let bytes = hex::decode(text.trim()).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    fn tag(&self, label: &[u8], parts: &[&[u8]]) -> [u8; TAG_LEN] {
        let mut message = Vec::with_capacity(parts.len() + 1);
        message.push(label);
        message.extend_from_slice(parts);
        hmac_sha256(&self.0, &message)
    }
}

/// The key from `<dir>/instance-key`; `None` when the file is missing or
/// malformed.
pub fn load(dir: &Path) -> Option<Key> {
    let text = std::fs::read_to_string(dir.join(KEY_FILE)).ok()?;
    Key::from_hex(&text)
}

/// [`load`], creating the file (and `dir`) when it is missing or malformed.
/// Only the holder of the instance lock may call this.
pub fn load_or_create(dir: &Path) -> io::Result<Key> {
    if let Some(key) = load(dir) {
        return Ok(key);
    }
    crate::images::private_dir(dir)?;
    let key = Key::generate();
    std::fs::write(dir.join(KEY_FILE), hex::encode(key.0))?;
    Ok(key)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::Rng;
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// HMAC-SHA256 (RFC 2104) over the concatenation of `parts`.
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; TAG_LEN] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..TAG_LEN].copy_from_slice(Sha256::digest(key).as_slice());
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize().as_slice());
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(outer.finalize().as_slice());
    tag
}

/// Comparison whose duration does not depend on where the inputs differ.
fn tags_match(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Client side: sends `payload` only after the server proved it has `key`.
/// Blocking; the caller bounds the time.
pub fn forward<S: Read + Write>(stream: &mut S, key: &Key, payload: &[u8]) -> io::Result<()> {
    let nonce: [u8; NONCE_LEN] = random_bytes();
    stream.write_all(&nonce)?;
    let mut reply = [0u8; TAG_LEN + NONCE_LEN];
    stream.read_exact(&mut reply)?;
    let (tag, server_nonce) = reply.split_at(TAG_LEN);
    if !tags_match(tag, &key.tag(SERVER_LABEL, &[&nonce])) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the single-instance pipe server failed authentication",
        ));
    }
    let mut message = key.tag(CLIENT_LABEL, &[&nonce, server_nonce]).to_vec();
    message.extend_from_slice(payload);
    stream.write_all(&message)
}

/// Server side: answers the client's challenge, then reads the payload of a
/// client that proved it has `key`. Bounded size and time.
pub async fn accept<S>(mut stream: S, key: &Key) -> Option<Vec<String>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut client_nonce = [0u8; NONCE_LEN];
        stream.read_exact(&mut client_nonce).await?;
        let nonce: [u8; NONCE_LEN] = random_bytes();
        let mut reply = key.tag(SERVER_LABEL, &[&client_nonce]).to_vec();
        reply.extend_from_slice(&nonce);
        stream.write_all(&reply).await?;
        let mut tag = [0u8; TAG_LEN];
        stream.read_exact(&mut tag).await?;
        Ok::<_, io::Error>(tags_match(
            &tag,
            &key.tag(CLIENT_LABEL, &[&client_nonce, &nonce]),
        ))
    })
    .await;
    match handshake {
        Ok(Ok(true)) => super::read_payload(stream).await,
        Ok(Ok(false)) => {
            tracing::warn!("rejected a single-instance client that failed authentication");
            None
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    const AUTH_LINK: &str =
        "rocketchat://auth?host=chat.example&credentialToken=tok&credentialSecret=sec";

    fn payload() -> Vec<u8> {
        super::super::encode_payload(&[AUTH_LINK.to_owned()])
    }

    /// A connected pair: the blocking client end and the server end.
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn async_end(stream: TcpStream) -> tokio::net::TcpStream {
        stream.set_nonblocking(true).unwrap();
        tokio::net::TcpStream::from_std(stream).unwrap()
    }

    fn with_timeouts(stream: &TcpStream) {
        let limit = Some(Duration::from_secs(5));
        stream.set_read_timeout(limit).unwrap();
        stream.set_write_timeout(limit).unwrap();
    }

    /// Everything a rogue server receives until the client hangs up.
    fn drain(mut stream: TcpStream) -> Vec<u8> {
        let mut seen = Vec::new();
        let _ = stream.read_to_end(&mut seen);
        seen
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        let tag = hmac_sha256(&[0x0b; 20], &[b"Hi There"]);
        assert_eq!(
            hex::encode(tag),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Split input hashes like the concatenation.
        let tag = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(
            hex::encode(tag),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block is hashed first (test case 6).
        let tag = hmac_sha256(
            &[0xaa; 131],
            &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
        );
        assert_eq!(
            hex::encode(tag),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn tags_differ_by_label_and_input() {
        let key = Key::generate();
        let n = [7u8; NONCE_LEN];
        assert_eq!(key.tag(SERVER_LABEL, &[&n]), key.tag(SERVER_LABEL, &[&n]));
        assert_ne!(key.tag(SERVER_LABEL, &[&n]), key.tag(CLIENT_LABEL, &[&n]));
        assert_ne!(
            key.tag(SERVER_LABEL, &[&n]),
            key.tag(SERVER_LABEL, &[&[8u8; NONCE_LEN]])
        );
        assert_ne!(
            key.tag(SERVER_LABEL, &[&n]),
            Key::generate().tag(SERVER_LABEL, &[&n])
        );
        assert!(tags_match(&[1, 2], &[1, 2]));
        assert!(!tags_match(&[1, 2], &[1, 3]));
        assert!(!tags_match(&[1, 2], &[1]));
    }

    #[test]
    fn key_file_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        assert!(load(&data).is_none());
        let first = load_or_create(&data).unwrap();
        assert_eq!(load(&data).unwrap().0, first.0);
        assert_eq!(load_or_create(&data).unwrap().0, first.0);
        // A damaged file is not a key; the lock holder replaces it.
        std::fs::write(data.join(KEY_FILE), "garbage").unwrap();
        assert!(load(&data).is_none());
        std::fs::write(data.join(KEY_FILE), "ab".repeat(KEY_LEN - 1)).unwrap();
        assert!(load(&data).is_none());
        let second = load_or_create(&data).unwrap();
        assert_ne!(second.0, first.0);
        assert_eq!(load(&data).unwrap().0, second.0);
    }

    #[tokio::test]
    async fn authenticated_peers_exchange_the_payload() {
        let key = Key::generate();
        let (mut client, server) = pair();
        with_timeouts(&client);
        let client_key = key.clone();
        let sent = tokio::task::spawn_blocking(move || {
            forward(&mut client, &client_key, &payload())
            // `client` drops here: end of payload.
        });
        let got = accept(async_end(server), &key).await;
        sent.await.unwrap().unwrap();
        assert_eq!(got, Some(vec![AUTH_LINK.to_owned()]));
    }

    /// F1: a pipe squatted by another user gets no auth link.
    #[test]
    fn rogue_server_without_the_key_gets_nothing() {
        let key = Key::generate();
        // Answers with a tag made with another key.
        let (mut client, mut server) = pair();
        with_timeouts(&client);
        let rogue = std::thread::spawn(move || {
            let mut nonce = [0u8; NONCE_LEN];
            server.read_exact(&mut nonce).unwrap();
            let mut reply = Key::generate().tag(SERVER_LABEL, &[&nonce]).to_vec();
            reply.extend_from_slice(&[0u8; NONCE_LEN]);
            server.write_all(&reply).unwrap();
            drain(server)
        });
        let err = forward(&mut client, &key, &payload()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        drop(client);
        let seen = rogue.join().unwrap();
        assert!(seen.is_empty(), "nothing follows the nonce");

        // Just reads: the client never gets a reply and sends nothing more.
        let (mut client, server) = pair();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let rogue = std::thread::spawn(move || drain(server));
        assert!(forward(&mut client, &key, &payload()).is_err());
        drop(client);
        let seen = rogue.join().unwrap();
        assert_eq!(seen.len(), NONCE_LEN);
        assert!(!contains(&seen, b"credentialSecret"));
    }

    /// F1: a reply recorded from the real server does not fit another nonce.
    #[tokio::test]
    async fn replayed_server_reply_is_rejected() {
        let key = Key::generate();
        // Record a genuine reply.
        let (mut probe, server) = pair();
        with_timeouts(&probe);
        let server_key = key.clone();
        let real = tokio::spawn(async move { accept(async_end(server), &server_key).await });
        let recorded = tokio::task::spawn_blocking(move || {
            probe.write_all(&[1u8; NONCE_LEN]).unwrap();
            let mut reply = [0u8; TAG_LEN + NONCE_LEN];
            probe.read_exact(&mut reply).unwrap();
            reply
        })
        .await
        .unwrap();
        // The probe hung up without a client tag: nothing is accepted.
        assert_eq!(real.await.unwrap(), None);

        let (mut client, mut server) = pair();
        with_timeouts(&client);
        let rogue = std::thread::spawn(move || {
            let mut nonce = [0u8; NONCE_LEN];
            server.read_exact(&mut nonce).unwrap();
            server.write_all(&recorded).unwrap();
            drain(server)
        });
        let sent = tokio::task::spawn_blocking(move || {
            let result = forward(&mut client, &key, &payload());
            drop(client);
            result
        })
        .await
        .unwrap();
        assert_eq!(sent.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(rogue.join().unwrap().is_empty());
    }

    /// A client without the key cannot inject arguments.
    #[tokio::test]
    async fn client_without_the_key_is_rejected() {
        let key = Key::generate();
        // Speaks the protocol with another key.
        let (mut client, server) = pair();
        with_timeouts(&client);
        let stranger = tokio::task::spawn_blocking(move || {
            let mut nonce = [0u8; NONCE_LEN];
            client.write_all(&nonce).unwrap();
            let mut reply = [0u8; TAG_LEN + NONCE_LEN];
            client.read_exact(&mut reply).unwrap();
            nonce.copy_from_slice(&reply[TAG_LEN..]);
            let mut message = Key::generate()
                .tag(CLIENT_LABEL, &[&[0u8; NONCE_LEN], &nonce])
                .to_vec();
            message.extend_from_slice(&payload());
            let _ = client.write_all(&message);
        });
        assert_eq!(accept(async_end(server), &key).await, None);
        stranger.await.unwrap();

        // The old wire format (payload only) is not accepted either.
        let (mut client, server) = pair();
        let old = tokio::task::spawn_blocking(move || {
            let _ = client.write_all(&payload());
        });
        assert_eq!(accept(async_end(server), &key).await, None);
        old.await.unwrap();
    }
}
