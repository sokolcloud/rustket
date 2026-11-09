//! The Windows pipe name, derived from a per-user secret.
//!
//! Named pipes live in one machine-wide namespace. So that another local user
//! can neither squat the name nor receive the forwarded URLs (they can carry
//! OAuth credential secrets), the name contains 128 random bits created once
//! and stored in the user's local data directory, whose ACL keeps other users
//! out. Platform-neutral so it is tested everywhere.

use std::io;
use std::path::Path;

/// File in the local data directory holding the secret.
pub const SECRET_FILE: &str = "instance-secret";
const PREFIX: &str = r"\\.\pipe\rustket-";
/// Hex digits of the secret (128 bits).
const SECRET_HEX_LEN: usize = 32;

/// The pipe name for a secret read from [`SECRET_FILE`]; `None` when the text
/// is not a well-formed secret.
pub fn from_secret(text: &str) -> Option<String> {
    let secret = text.trim();
    let valid = secret.len() == SECRET_HEX_LEN
        && secret
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    valid.then(|| format!("{PREFIX}{secret}"))
}

/// A new random secret (hex).
pub fn new_secret() -> String {
    use rand::Rng;
    let mut bytes = [0u8; SECRET_HEX_LEN / 2];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The pipe name from `<dir>/instance-secret`, creating the file (and `dir`)
/// when it is missing or malformed.
pub fn load_or_create(dir: &Path) -> io::Result<String> {
    let file = dir.join(SECRET_FILE);
    if let Some(name) = std::fs::read_to_string(&file)
        .ok()
        .as_deref()
        .and_then(from_secret)
    {
        return Ok(name);
    }
    crate::images::private_dir(dir)?;
    let secret = new_secret();
    std::fs::write(&file, &secret)?;
    from_secret(&secret).ok_or_else(|| io::Error::other("generated secret is malformed"))
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
    fn secret_file_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let first = load_or_create(&data).unwrap();
        assert!(first.starts_with(PREFIX));
        assert_eq!(load_or_create(&data).unwrap(), first);
        // A damaged file is replaced.
        std::fs::write(data.join(SECRET_FILE), "garbage").unwrap();
        let second = load_or_create(&data).unwrap();
        assert_ne!(second, first);
        assert_eq!(load_or_create(&data).unwrap(), second);
    }
}
