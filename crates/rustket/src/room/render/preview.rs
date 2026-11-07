//! `image_preview` placeholders (tiny base64 JPEGs sent with image uploads).
//! Handles are cached so the renderer does not decode them again on every
//! rebuild (a new `Handle` means a new texture).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use iced::widget::image;

/// Most cached preview handles.
const CAPACITY: usize = 256;
/// Largest base64 preview accepted (the server sends ~1–3 KB).
const MAX_LEN: usize = 64 * 1024;
/// Largest preview dimension accepted (the server makes 32 px previews).
const MAX_DIMENSION: u32 = 512;
/// Most memory one preview decode may allocate.
const MAX_ALLOC: u64 = 4 * 1024 * 1024;

thread_local! {
    /// Content hash → decoded handle (`None`: invalid, not decoded again).
    static CACHE: RefCell<HashMap<u64, Option<image::Handle>>> = RefCell::new(HashMap::new());
}

/// Handle for a base64 preview (decoded once, with size limits: the bytes
/// come from any room member).
pub fn preview_handle(b64: &str) -> Option<image::Handle> {
    if b64.len() > MAX_LEN {
        return None;
    }
    let mut h = DefaultHasher::new();
    b64.hash(&mut h);
    let key = h.finish();
    CACHE.with(|c| {
        if let Some(h) = c.borrow().get(&key) {
            return h.clone();
        }
        let handle = decode_base64(b64).and_then(|bytes| decode_limited(&bytes));
        let mut c = c.borrow_mut();
        if c.len() >= CAPACITY {
            c.clear();
        }
        c.insert(key, handle.clone());
        handle
    })
}

/// Decodes a raster image no larger than [`MAX_DIMENSION`] into RGBA.
fn decode_limited(bytes: &[u8]) -> Option<image::Handle> {
    if crate::images::looks_like_svg(bytes) {
        return None;
    }
    let mut reader = ::image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some(image::Handle::from_rgba(w, h, rgba.into_raw()))
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

    fn png(w: u32, h: u32) -> String {
        let img = ::image::GrayImage::new(w, h);
        let mut out = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut out),
            ::image::ImageFormat::Png,
        )
        .unwrap();
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in out.chunks(3) {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..=c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        s
    }

    #[test]
    fn previews_are_size_limited() {
        assert!(preview_handle(&png(32, 24)).is_some());
        // A tiny PNG that would decode to a huge bitmap is refused.
        let bomb = png(1000, 1000);
        assert!(bomb.len() < MAX_LEN);
        assert!(preview_handle(&bomb).is_none());
        assert!(preview_handle("bm90IGFuIGltYWdl").is_none());
    }
}
