//! The generated 12-word passphrase (§3.4), identical to the web and RN clients.

use zeroize::Zeroizing;

use super::random_bytes;

/// The BIP-39 English word list, one word per line (2048 words).
pub const BIP39_ENGLISH: &str = include_str!("bip39_english.txt");

const WORD_COUNT: usize = 12;

/// The 2048 words in order.
pub fn word_list() -> Vec<&'static str> {
    BIP39_ENGLISH.lines().collect()
}

/// Index of a word drawn with the web client's rejection sampling: a uniform
/// 32-bit value, rejected while `v >= floor(0xFFFFFFFF / 2048) * 2048`.
fn sample(range: u32, mut next: impl FnMut() -> u32) -> u32 {
    let threshold = (u32::MAX / range) * range;
    loop {
        let v = next();
        if v < threshold {
            return v % range;
        }
    }
}

/// A fresh 12-word passphrase (132 bits of entropy).
pub fn generate_passphrase() -> Zeroizing<String> {
    let words = word_list();
    let range = u32::try_from(words.len()).unwrap_or(u32::MAX);
    let mut out = Zeroizing::new(String::with_capacity(WORD_COUNT * 9));
    for i in 0..WORD_COUNT {
        if i > 0 {
            out.push(' ');
        }
        let idx = sample(range, || u32::from_be_bytes(random_bytes::<4>()));
        out.push_str(words.get(idx as usize).copied().unwrap_or_default());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn word_list_matches_upstream() {
        let words = word_list();
        assert_eq!(words.len(), 2048);
        assert_eq!(words[0], "abandon");
        assert_eq!(words[2047], "zoo");
        assert!(
            words
                .iter()
                .all(|w| (3..=8).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase()))
        );
        let hash = sha2::Sha256::digest(words.join("\n").as_bytes());
        assert_eq!(
            hex::encode(hash),
            "187db04a869dd9bc7be80d21a86497d692c0db6abd3aa8cb6be5d618ff757fae"
        );
    }

    #[test]
    fn rejection_sampling() {
        let threshold = (u32::MAX / 2048) * 2048;
        let mut values = vec![u32::MAX, threshold, threshold - 1].into_iter();
        assert_eq!(
            sample(2048, || values.next().unwrap()),
            (threshold - 1) % 2048
        );
        let mut values = vec![2049].into_iter();
        assert_eq!(sample(2048, || values.next().unwrap()), 1);
    }

    #[test]
    fn generated_passphrases() {
        let words = word_list();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let p = generate_passphrase();
            let parts: Vec<&str> = p.split(' ').collect();
            assert_eq!(parts.len(), 12);
            assert!(parts.iter().all(|w| words.contains(w)));
            seen.extend(parts.iter().map(|w| w.to_string()));
        }
        // 2400 draws from 2048 words: expect well over a thousand distinct ones.
        assert!(seen.len() > 1000, "{}", seen.len());
    }
}
