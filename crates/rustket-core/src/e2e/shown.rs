//! How an end-to-end encrypted message is shown after the app processed it.
//!
//! The app decrypts `t: 'e2e'` messages in place (`Workspace::rewrite_messages`):
//! `msg`, `md`, `attachments`, `file` and `files` are replaced by the decrypted
//! payload while `content` keeps the envelope, and `Message::e2e` gets one of
//! the markers below. The server's own `e2e` value is never trusted (§4): the
//! app overwrites it on every message it processes, before anything is drawn,
//! and checks its own record of processed messages before believing a marker.
//! Stored markers carry a random per-process nonce, so a marker the server
//! stored never matches (see [`stored`]).

use std::sync::OnceLock;

use rocketchat::models::Message;

/// Common prefix of every marker.
pub const PREFIX: &str = "rustket:";
/// Decrypted; `msg` and the attachments are the decrypted values.
pub const DECRYPTED: &str = "rustket:ok";
/// Decrypted, but the server message also carried plaintext `msg` (§11.14).
pub const DECRYPTED_PLAINTEXT: &str = "rustket:ok+plain";
/// The account keys are not loaded (passphrase not entered, E2E off).
pub const LOCKED: &str = "rustket:locked";
/// The account is ready but the room key has not arrived yet.
pub const WAITING: &str = "rustket:waiting";
/// No key for this message (`E2E_indecipherable`).
pub const NO_KEY: &str = "rustket:nokey";
/// A key was found but decryption failed (`E2E_Key_Error`).
pub const KEY_ERROR: &str = "rustket:keyerror";
/// An envelope that cannot be decrypted (e.g. megolm, malformed).
pub const UNSUPPORTED: &str = "rustket:unsupported";

const KINDS: [&str; 7] = [
    DECRYPTED,
    DECRYPTED_PLAINTEXT,
    LOCKED,
    WAITING,
    NO_KEY,
    KEY_ERROR,
    UNSUPPORTED,
];

/// A random tag of this process. Stored markers carry it
/// (`rustket:<nonce>:ok`), so a marker the server stored (any member can
/// send one) never matches.
fn nonce() -> &'static str {
    static N: OnceLock<String> = OnceLock::new();
    N.get_or_init(|| rocketchat::ids::random_id_len(16))
}

/// The stored form of a marker kind (one of the constants above).
pub fn stored(kind: &str) -> String {
    let suffix = kind.strip_prefix(PREFIX).unwrap_or(kind);
    format!("{PREFIX}{}:{suffix}", nonce())
}

/// Sets the marker of `m` (one of the constants above).
pub fn set_marker(m: &mut Message, kind: &str) {
    m.e2e = Some(stored(kind));
}

/// Whether the app shows `m` decrypted (its `msg`/attachments are plaintext).
pub fn is_decrypted(m: &Message) -> bool {
    matches!(marker(m), Some(DECRYPTED | DECRYPTED_PLAINTEXT))
}

/// The marker kind of `m` (one of the constants above), if this process
/// wrote it.
pub fn marker(m: &Message) -> Option<&'static str> {
    let rest = m
        .e2e
        .as_deref()?
        .strip_prefix(PREFIX)?
        .strip_prefix(nonce())?
        .strip_prefix(':')?;
    KINDS
        .iter()
        .copied()
        .find(|k| k.strip_prefix(PREFIX) == Some(rest))
}

/// Whether `m` carries anything that looks like a marker (ours or forged).
pub fn has_marker_prefix(m: &Message) -> bool {
    m.e2e.as_deref().is_some_and(|s| s.starts_with(PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        let mut m = Message::default();
        assert!(!is_decrypted(&m));
        m.e2e = Some("done".into());
        assert!(!is_decrypted(&m));
        assert_eq!(marker(&m), None);
        // A marker without this process's nonce (e.g. sent by a member).
        m.e2e = Some(DECRYPTED.into());
        assert!(!is_decrypted(&m));
        assert!(has_marker_prefix(&m));
        set_marker(&mut m, DECRYPTED_PLAINTEXT);
        assert!(is_decrypted(&m));
        assert_eq!(marker(&m), Some(DECRYPTED_PLAINTEXT));
        set_marker(&mut m, WAITING);
        assert!(!is_decrypted(&m));
        assert_eq!(marker(&m), Some(WAITING));
    }
}
