//! The Windows pipe name: new and random for every primary instance.
//!
//! Named pipes live in one machine-wide namespace that every local user can
//! list, so the name is not a secret and the peers authenticate each other
//! ([`super::pipe_auth`]). It is still unpredictable: the primary picks 128
//! random bits per run and publishes the name in the user's local data
//! directory (whose ACL keeps other users out) before it creates the pipe, so
//! nobody can occupy the name in advance. Platform-neutral so it is tested
//! everywhere.

use std::io;
use std::path::Path;

/// File in the local data directory holding the current name's random part.
pub const NAME_FILE: &str = "instance-pipe";
const PREFIX: &str = r"\\.\pipe\rustket-";
/// Hex digits of the random part (128 bits).
const SECRET_HEX_LEN: usize = 32;

/// The pipe name for the text of [`NAME_FILE`]; `None` when the text is not a
/// well-formed random part.
pub fn from_secret(text: &str) -> Option<String> {
    let secret = text.trim();
    let valid = secret.len() == SECRET_HEX_LEN
        && secret
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    valid.then(|| format!("{PREFIX}{secret}"))
}

/// A new random part (hex).
pub fn new_secret() -> String {
    use rand::Rng;
    let mut bytes = [0u8; SECRET_HEX_LEN / 2];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The name the primary instance published in `<dir>/instance-pipe`; `None`
/// when the file is missing or malformed (also while it is being written).
pub fn current(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(NAME_FILE)).ok()?;
    from_secret(&text)
}

/// Publishes a new random name in `<dir>/instance-pipe` (creating `dir`) and
/// returns it. Only the holder of the instance lock may call this.
pub fn rotate(dir: &Path) -> io::Result<String> {
    crate::images::private_dir(dir)?;
    let secret = new_secret();
    std::fs::write(dir.join(NAME_FILE), &secret)?;
    from_secret(&secret).ok_or_else(|| io::Error::other("generated pipe name is malformed"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn name_from_secret() {
        let s = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            from_secret(s).as_deref(),
            Some(r"\\.\pipe\rustket-0123456789abcdef0123456789abcdef")
        );
        assert_eq!(from_secret(&format!("{s}\r\n")), from_secret(s));
        assert_eq!(from_secret(""), None);
        assert_eq!(from_secret(&s[1..]), None, "too short");
        assert_eq!(from_secret(&format!("{s}0")), None, "too long");
        assert_eq!(from_secret(&s.to_uppercase()), None);
        assert_eq!(from_secret(&s.replace('0', "\\")), None);
        let fresh = new_secret();
        assert_ne!(fresh, new_secret());
        assert!(from_secret(&fresh).is_some());
    }

    #[test]
    fn name_changes_with_every_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        assert_eq!(current(&data), None);
        let first = rotate(&data).unwrap();
        assert!(first.starts_with(PREFIX));
        assert_eq!(current(&data), Some(first.clone()));
        // F1: a name seen during an earlier run is never served again.
        let second = rotate(&data).unwrap();
        assert_ne!(second, first);
        assert_eq!(current(&data), Some(second));
        // A damaged file names no pipe.
        std::fs::write(data.join(NAME_FILE), "garbage").unwrap();
        assert_eq!(current(&data), None);
        std::fs::write(data.join(NAME_FILE), r"..\\..\\pipe\\other").unwrap();
        assert_eq!(current(&data), None);
    }
}
