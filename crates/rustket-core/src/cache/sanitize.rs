//! What may be written to the offline cache (fail closed).
//!
//! The app decrypts end-to-end encrypted messages in place
//! ([`crate::e2e::shown`]): `msg`, `md`, attachments and files hold the
//! plaintext, `content` the envelope and `e2e` a marker. The cache must only
//! ever see ciphertext, so:
//! - a message the app processed (any `rustket:` marker) keeps its envelope
//!   and loses every field that can hold decrypted data; without an
//!   envelope it is not stored at all;
//! - `message_pinned_e2e` messages the app processed (the pinned text is
//!   decrypted into an attachment) are not stored;
//! - an encrypted message the app has not processed is the server's form
//!   (ciphertext) and is stored as is, minus any `e2e` value;
//! - the current user loses `services` and `e2e` (key material), a
//!   subscription its (encrypted) room keys;
//! - a room's `lastMessage` follows the same message rules.

use rocketchat::models::{Message, Room, Subscription, User};

use crate::e2e::shown::has_marker_prefix;

/// The form of `m` the cache may store, or `None` when it must not be
/// stored.
pub fn message(m: &Message) -> Option<Message> {
    let processed = has_marker_prefix(m);
    let encrypted = m.is_encrypted() || m.t.as_deref() == Some("message_pinned_e2e");
    if !processed && !encrypted {
        return Some(m.clone());
    }
    if m.t.as_deref() == Some("message_pinned_e2e") {
        return (!processed).then(|| {
            let mut c = m.clone();
            c.e2e = None;
            c
        });
    }
    let mut c = m.clone();
    c.e2e = None;
    if processed {
        // Decrypted in place: only the envelope may stay.
        c.content.as_ref()?;
        c.msg.clear();
        c.md = None;
        c.attachments = None;
        c.file = None;
        c.files = None;
        c.urls = None;
        c.blocks = None;
        c.mentions = None;
        c.channels = None;
    }
    Some(c)
}

/// A room without decrypted data in its `lastMessage`.
pub fn room(r: &Room) -> Room {
    let mut c = r.clone();
    c.last_message = r.last_message.as_deref().and_then(message).map(Box::new);
    c
}

/// The current user without services or E2E key material.
pub fn user(u: &User) -> User {
    let mut c = u.clone();
    c.services = None;
    c.e2e = None;
    c
}

/// A subscription without its room keys (the E2E manager only works with
/// the server's copy).
pub fn subscription(s: &Subscription) -> Subscription {
    let mut c = s.clone();
    c.e2e_key = None;
    c.e2e_suggested_key = None;
    c.old_room_keys = None;
    c.suggested_old_room_keys = None;
    c
}
