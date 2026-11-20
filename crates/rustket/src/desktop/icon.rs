//! The app icon (`assets/icons`, rendered by `examples/gen_icons.rs`): window
//! icon and tray pixmaps, with unread variants carrying a dot for each
//! [`TrayLevel`].

use super::tray::TrayLevel;

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
/// Red dot: mentions.
const MENTION: [&[u8]; 8] = icons!("rustket-unread");
/// Orange dot: `@all` / `@here`.
const GROUP: [&[u8]; 8] = icons!("rustket-group");
/// Quiet dot (the unread colour): any other unread.
const QUIET: [&[u8]; 8] = icons!("rustket-quiet");

/// PNG bytes of the icon of `size` (one of [`SIZES`]) with the dot of `level`.
pub fn png(size: u32, level: TrayLevel) -> Option<&'static [u8]> {
    let i = SIZES.iter().position(|s| *s == size)?;
    let set = match level {
        TrayLevel::None => &NORMAL,
        TrayLevel::Unread => &QUIET,
        TrayLevel::GroupMention => &GROUP,
        TrayLevel::Mention => &MENTION,
    };
    Some(set[i])
}

/// Decoded RGBA pixels: `(width, height, rgba)`.
pub fn rgba(size: u32, level: TrayLevel) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory_with_format(png(size, level)?, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// The window icon.
pub fn window_icon() -> Option<iced::window::Icon> {
    let (w, h, px) = rgba(256, TrayLevel::None)?;
    iced::window::icon::from_rgba(px, w, h).ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
/// RGBA → ARGB32 in network byte order (StatusNotifierItem pixmaps).
pub fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let (pixels, _) = rgba.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
/// Tray pixmaps (several sizes; the host picks the best one).
pub fn tray_pixmaps(level: TrayLevel) -> Vec<ksni::Icon> {
    [22u32, 32, 48, 64]
        .iter()
        .filter_map(|s| rgba(*s, level))
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

    const LEVELS: [TrayLevel; 4] = [
        TrayLevel::None,
        TrayLevel::Unread,
        TrayLevel::GroupMention,
        TrayLevel::Mention,
    ];

    #[test]
    fn every_size_decodes() {
        for &s in SIZES {
            for level in LEVELS {
                let (w, h, px) = rgba(s, level).unwrap();
                assert_eq!((w, h), (s, s));
                assert_eq!(px.len(), (s * s * 4) as usize);
            }
        }
        assert!(png(17, TrayLevel::None).is_none());
        assert!(window_icon().is_some());
        #[cfg(all(unix, not(target_os = "macos")))]
        for level in LEVELS {
            assert_eq!(tray_pixmaps(level).len(), 4);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn argb_order() {
        assert_eq!(
            rgba_to_argb(&[1, 2, 3, 4, 5, 6, 7, 8]),
            [4, 1, 2, 3, 8, 5, 6, 7]
        );
    }

    /// Each level has its dot colour at the dot's centre, (52, 12) in a
    /// 64 px icon and (26, 6) in a 32 px one (the sizes the trays use).
    #[test]
    fn unread_variants_have_their_dot() {
        let rgb = |level, size: u32, (x, y): (u32, u32)| {
            let (w, _, px) = rgba(size, level).unwrap();
            let i = ((y * w + x) * 4) as usize;
            assert_eq!(px[i + 3], 255, "{level:?} {size}");
            (px[i], px[i + 1], px[i + 2])
        };
        let close = |a: (u8, u8, u8), b: (u8, u8, u8)| {
            a.0.abs_diff(b.0) < 12 && a.1.abs_diff(b.1) < 12 && a.2.abs_diff(b.2) < 12
        };
        for (size, at) in [(64, (52, 12)), (32, (26, 6))] {
            let plain = rgb(TrayLevel::None, size, at);
            for (level, want) in [
                (TrayLevel::Mention, (0xEC, 0x0D, 0x2A)),
                (TrayLevel::GroupMention, (0xB3, 0x53, 0x00)),
                (TrayLevel::Unread, (0x55, 0x74, 0xA6)),
            ] {
                let got = rgb(level, size, at);
                assert!(close(got, want), "{level:?} {size}: {got:?}");
                assert_ne!(got, plain);
            }
        }
    }
}
