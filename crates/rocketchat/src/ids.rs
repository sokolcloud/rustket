//! Random identifiers compatible with Meteor's `Random.id()` / `Random.secret()`.

use rand::RngExt;

/// Meteor's unmistakable-characters alphabet used by `Random.id()`.
pub const METEOR_ID_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTWXYZabcdefghijkmnopqrstuvwxyz";

/// Alphabet of `Random.secret()` (base64url characters).
pub const SECRET_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn random_string(alphabet: &[u8], len: usize) -> String {
    // ThreadRng is a CSPRNG (ChaCha) reseeded from the OS.
    let mut rng = rand::rng();
    (0..len)
        .map(|_| char::from(alphabet[rng.random_range(0..alphabet.len())]))
        .collect()
}

/// A 17-character Meteor id, e.g. for client-generated message `_id`s.
pub fn random_id() -> String {
    random_string(METEOR_ID_ALPHABET, 17)
}

/// A Meteor-style id of arbitrary length.
pub fn random_id_len(len: usize) -> String {
    random_string(METEOR_ID_ALPHABET, len)
}

/// A random secret from `[A-Za-z0-9_-]`, e.g. a 43-character OAuth `credentialToken`.
pub fn random_secret(len: usize) -> String {
    random_string(SECRET_ALPHABET, len)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn id_shape() {
        let id = random_id();
        assert_eq!(id.len(), 17);
        assert!(id.bytes().all(|b| METEOR_ID_ALPHABET.contains(&b)));
        assert_ne!(random_id(), random_id());
    }

    #[test]
    fn secret_shape() {
        let s = random_secret(43);
        assert_eq!(s.len(), 43);
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }
}
