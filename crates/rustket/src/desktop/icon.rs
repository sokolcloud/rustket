//! The app icon (`assets/icons`, rendered by `examples/gen_icons.rs`): window
//! icon and tray pixmaps, with an "unread" variant carrying a red dot.

/// Sizes shipped in `assets/icons`.
pub const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 128, 256];

macro_rules! icons {
    ($name:literal) => {
        [
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-16.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-22.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-24.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-32.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-48.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-64.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-128.png"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/",
                $name,
                "-256.png"
            ))
            .as_slice(),
        ]
    };
}

const NORMAL: [&[u8]; 8] = icons!("rustket");
const UNREAD: [&[u8]; 8] = icons!("rustket-unread");

/// PNG bytes of the icon of `size` (one of [`SIZES`]).
pub fn png(size: u32, unread: bool) -> Option<&'static [u8]> {
    let i = SIZES.iter().position(|s| *s == size)?;
    Some(if unread { UNREAD[i] } else { NORMAL[i] })
}

/// Decoded RGBA pixels: `(width, height, rgba)`.
pub fn rgba(size: u32, unread: bool) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory_with_format(png(size, unread)?, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// The window icon.
pub fn window_icon() -> Option<iced::window::Icon> {
    let (w, h, px) = rgba(256, false)?;
    iced::window::icon::from_rgba(px, w, h).ok()
}

/// RGBA → ARGB32 in network byte order (StatusNotifierItem pixmaps).
pub fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let (pixels, _) = rgba.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}

/// Tray pixmaps (several sizes; the host picks the best one).
pub fn tray_pixmaps(unread: bool) -> Vec<ksni::Icon> {
    [22u32, 32, 48, 64]
        .iter()
        .filter_map(|s| rgba(*s, unread))
        .map(|(w, h, px)| ksni::Icon {
            width: w as i32,
            height: h as i32,
            data: rgba_to_argb(&px),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_size_decodes() {
        for &s in SIZES {
            for unread in [false, true] {
                let (w, h, px) = rgba(s, unread).unwrap();
                assert_eq!((w, h), (s, s));
                assert_eq!(px.len(), (s * s * 4) as usize);
            }
        }
        assert!(png(17, false).is_none());
        assert!(window_icon().is_some());
        assert_eq!(tray_pixmaps(true).len(), 4);
    }

    #[test]
    fn argb_order() {
        assert_eq!(
            rgba_to_argb(&[1, 2, 3, 4, 5, 6, 7, 8]),
            [4, 1, 2, 3, 8, 5, 6, 7]
        );
    }

    #[test]
    fn unread_variant_has_a_red_dot() {
        let (w, _, px) = rgba(64, true).unwrap();
        // Centre of the dot at (52, 12) in a 64 px icon.
        let i = ((12 * w + 52) * 4) as usize;
        assert!(
            px[i] > 200 && px[i + 1] < 60 && px[i + 3] == 255,
            "{:?}",
            &px[i..i + 4]
        );
        let (_, _, plain) = rgba(64, false).unwrap();
        assert_ne!(plain[i..i + 4], px[i..i + 4]);
    }
}
