//! `image_preview` placeholders (tiny base64 JPEGs sent with image uploads).
//! Handles are cached so the renderer does not decode them again on every
//! rebuild (a new `Handle` means a new texture).

use std::cell::RefCell;
use std::collections::HashMap;

use iced::widget::image;

/// Most cached preview handles.
const CAPACITY: usize = 256;
/// Largest base64 preview accepted (the server sends ~1–3 KB).
const MAX_LEN: usize = 64 * 1024;

thread_local! {
    static CACHE: RefCell<HashMap<String, image::Handle>> = RefCell::new(HashMap::new());
}

/// Handle for the preview of message `mid` (decoded once).
pub fn preview_handle(mid: &str, b64: &str) -> Option<image::Handle> {
    if b64.len() > MAX_LEN {
        return None;
    }
    let key = format!("{mid}:{}", b64.len());
    CACHE.with(|c| {
        if let Some(h) = c.borrow().get(&key) {
            return Some(h.clone());
        }
        let bytes = decode_base64(b64)?;
        let handle = image::Handle::from_bytes(bytes);
        let mut c = c.borrow_mut();
        if c.len() >= CAPACITY {
            c.clear();
        }
        c.insert(key, handle.clone());
        Some(handle)
    })
}

/// Standard or URL-safe base64, padding optional, whitespace ignored.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVs\nbG8h").unwrap(), b"hello!");
        assert_eq!(decode_base64("_-8=").unwrap(), [0xff, 0xef]);
        assert!(decode_base64("a*b").is_none());
        assert!(decode_base64("").is_none());
    }
}
