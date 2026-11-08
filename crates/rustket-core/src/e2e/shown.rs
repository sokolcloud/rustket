//! How an end-to-end encrypted message is shown after the app processed it.
//!
//! The app decrypts `t: 'e2e'` messages in place (`Workspace::rewrite_messages`):
//! `msg`, `md`, `attachments`, `file` and `files` are replaced by the decrypted
//! payload while `content` keeps the envelope, and `Message::e2e` gets one of
//! the markers below. The server's own `e2e` value is never trusted (§4): the
//! app overwrites it on every message it processes, before anything is drawn,
//! and checks its own record of processed messages before believing a marker.

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

/// Whether the app shows `m` decrypted (its `msg`/attachments are plaintext).
pub fn is_decrypted(m: &Message) -> bool {
    matches!(m.e2e.as_deref(), Some(DECRYPTED | DECRYPTED_PLAINTEXT))
}

/// The marker of `m`, if the app wrote one.
pub fn marker(m: &Message) -> Option<&str> {
    m.e2e.as_deref().filter(|s| s.starts_with(PREFIX))
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
        m.e2e = Some(DECRYPTED_PLAINTEXT.into());
        assert!(is_decrypted(&m));
        m.e2e = Some(WAITING.into());
        assert!(!is_decrypted(&m));
        assert_eq!(marker(&m), Some(WAITING));
    }
}
