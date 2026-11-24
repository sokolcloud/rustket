//! The app icon (`assets/icons`, rendered by `examples/gen_icons.rs`): window
//! icon and tray pixmaps, with unread variants carrying a dot for each
//! [`TrayLevel`] or the Rocket.Chat desktop app's counter ([`TrayArt`]), and
//! the macOS menu bar template glyphs.

use super::badge_art;
use super::tray::{TrayArt, TrayLevel};

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
    decode(png(size, level)?)
}

fn decode(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// Size of the macOS menu bar templates: 22 pt at 2x.
#[cfg(any(target_os = "macos", test))]
pub const MENUBAR_SIZE: u32 = 44;

#[cfg(any(target_os = "macos", test))]
macro_rules! menubar {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/icons/",
            $name,
            ".png"
        ))
        .as_slice()
    };
}

/// PNG bytes of the macOS menu bar template (black with alpha; the menu bar
/// tints it): the plain glyph, a ring for quiet unread, a dot for mentions
/// and `@all`.
#[cfg(any(target_os = "macos", test))]
pub fn menubar_png(level: TrayLevel) -> &'static [u8] {
    match level {
        TrayLevel::None => menubar!("rustket-menubar"),
        TrayLevel::Unread => menubar!("rustket-menubar-ring"),
        TrayLevel::GroupMention | TrayLevel::Mention => menubar!("rustket-menubar-dot"),
    }
}

/// Decoded [`menubar_png`]: `(width, height, rgba)`.
#[cfg(any(target_os = "macos", test))]
pub fn menubar_rgba(level: TrayLevel) -> Option<(u32, u32, Vec<u8>)> {
    decode(menubar_png(level))
}

/// The menu bar template for `art`: the Rocket.Chat counter and bullet show
/// the dot (the number is the menu bar title).
#[cfg(any(target_os = "macos", test))]
pub fn menubar_rgba_art(art: TrayArt) -> Option<(u32, u32, Vec<u8>)> {
    let level = match art {
        TrayArt::Plain => TrayLevel::None,
        TrayArt::Level(level) => level,
        TrayArt::RcCount(_) | TrayArt::RcPlus | TrayArt::RcDot => TrayLevel::Mention,
    };
    menubar_rgba(level)
}

/// The window icon.
pub fn window_icon() -> Option<iced::window::Icon> {
    let (w, h, px) = rgba(256, TrayLevel::None)?;
    iced::window::icon::from_rgba(px, w, h).ok()
}

/// Sizes of the Rocket.Chat counter tray icons (the StatusNotifierItem
/// sizes; Windows uses 32).
pub const RC_SIZES: &[u32] = &[22, 32, 48, 64];

macro_rules! rc_icons {
    ($name:literal) => {
        [
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
        ]
    };
}

/// `rustket-rc-1` … `rustket-rc-9`, `rustket-rc-plus`, `rustket-rc-dot`.
const RC: [[&[u8]; 4]; 11] = [
    rc_icons!("rustket-rc-1"),
    rc_icons!("rustket-rc-2"),
    rc_icons!("rustket-rc-3"),
    rc_icons!("rustket-rc-4"),
    rc_icons!("rustket-rc-5"),
    rc_icons!("rustket-rc-6"),
    rc_icons!("rustket-rc-7"),
    rc_icons!("rustket-rc-8"),
    rc_icons!("rustket-rc-9"),
    rc_icons!("rustket-rc-plus"),
    rc_icons!("rustket-rc-dot"),
];

/// PNG bytes of the tray icon of `size` with `art`: [`SIZES`] for the plain
/// icon and the levels, [`RC_SIZES`] for the Rocket.Chat counters.
pub fn png_art(size: u32, art: TrayArt) -> Option<&'static [u8]> {
    let set = match art {
        TrayArt::Plain => return png(size, TrayLevel::None),
        TrayArt::Level(level) => return png(size, level),
        TrayArt::RcCount(0) => return None,
        TrayArt::RcCount(n @ 1..=9) => &RC[usize::from(n) - 1],
        TrayArt::RcCount(_) | TrayArt::RcPlus => &RC[9],
        TrayArt::RcDot => &RC[10],
    };
    let i = RC_SIZES.iter().position(|s| *s == size)?;
    Some(set[i])
}

/// Decoded [`png_art`]: `(width, height, rgba)`.
pub fn rgba_art(size: u32, art: TrayArt) -> Option<(u32, u32, Vec<u8>)> {
    decode(png_art(size, art)?)
}

/// The window icon's pixels with `art` (Linux and Windows, the Rocket.Chat
/// scheme), as the desktop app draws its window icon: a quarter of the
/// size is cleared around the bottom right (a disc of radius `size/4`
/// centred at `size − size/4`, plus the square from that centre to the
/// corner), and the badge, `size/3` wide, is centred in it. Without a badge
/// glyph it is the plain icon.
pub fn window_rgba_art(size: u32, art: TrayArt) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, mut px) = rgba(size, art.level())?;
    let Some(glyph) = art.glyph() else {
        return Some((w, h, px));
    };
    let s = size as f32;
    let (hole, badge) = (s / 2.0, s / 3.0);
    let c = s - hole / 2.0;
    let r = hole / 2.0;
    badge_art::clear(&mut px, size, size / 2, |x, y| {
        (x - c).powi(2) + (y - c).powi(2) <= r * r || (x >= c && y >= c)
    });
    let at = s - hole + (hole - badge) / 2.0;
    badge_art::paint(&mut px, size, (at, at, badge), glyph);
    Some((w, h, px))
}

/// The window icon with `art` (see [`window_rgba_art`]).
pub fn window_icon_rc(art: TrayArt) -> Option<iced::window::Icon> {
    let (w, h, px) = window_rgba_art(256, art)?;
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

#[cfg(all(unix, not(target_os = "macos")))]
/// Tray pixmaps of `art` (several sizes; the host picks the best one).
pub fn tray_pixmaps_art(art: TrayArt) -> Vec<ksni::Icon> {
    [22u32, 32, 48, 64]
        .iter()
        .filter_map(|s| rgba_art(*s, art))
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

    /// The menu bar templates are alpha only (the menu bar tints them): a
    /// bubble with the spark knocked out; at the top right corner, (36, 8),
    /// nothing for no unread, a ring for quiet unread, a dot for mentions and
    /// `@all`, each in a cleared gap.
    #[test]
    fn menubar_templates_are_alpha_only() {
        let alpha = |level| {
            let (w, h, px) = menubar_rgba(level).unwrap();
            assert_eq!((w, h), (MENUBAR_SIZE, MENUBAR_SIZE));
            for p in px.as_chunks::<4>().0 {
                assert!(p[3] == 0 || p[..3] == [0, 0, 0], "{level:?}: {p:?}");
            }
            move |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize]
        };
        for level in LEVELS {
            let a = alpha(level);
            assert_eq!(a(8, 20), 255, "{level:?}: glyph");
            assert_eq!(a(22, 18), 0, "{level:?}: spark");
        }
        let plain = alpha(TrayLevel::None);
        assert_eq!(plain(31, 13), 255);
        assert!(plain(36, 8) < 128);
        let ring = alpha(TrayLevel::Unread);
        assert_eq!((ring(36, 8), ring(31, 13)), (0, 0));
        assert!(ring(41, 8) >= 200);
        for level in [TrayLevel::GroupMention, TrayLevel::Mention] {
            let dot = alpha(level);
            assert_eq!((dot(36, 8), dot(31, 13)), (255, 0), "{level:?}");
        }
    }

    const RC_ARTS: [TrayArt; 11] = [
        TrayArt::RcCount(1),
        TrayArt::RcCount(2),
        TrayArt::RcCount(3),
        TrayArt::RcCount(4),
        TrayArt::RcCount(5),
        TrayArt::RcCount(6),
        TrayArt::RcCount(7),
        TrayArt::RcCount(8),
        TrayArt::RcCount(9),
        TrayArt::RcPlus,
        TrayArt::RcDot,
    ];

    fn pixel(px: &[u8], w: u32, (x, y): (u32, u32)) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    /// The counter icons decode at the tray sizes, each different, with the
    /// red disc and a white glyph at the bottom right; the plain art and
    /// the levels are today's icons.
    #[test]
    fn rc_tray_art() {
        for &s in RC_SIZES {
            let mut seen = std::collections::HashSet::new();
            for art in RC_ARTS {
                let (w, h, px) = rgba_art(s, art).unwrap();
                assert_eq!((w, h), (s, s));
                assert!(seen.insert(px.clone()), "{art:?} {s}");
                // The disc's top edge, inside the badge but above the glyph.
                let top = pixel(&px, w, (s * 3 / 4, s * 3 / 4 - s * 3 / 20));
                let (r, g, b) = badge_art::DISC;
                assert!(
                    top[0].abs_diff(r) < 16 && top[1].abs_diff(g) < 16 && top[2].abs_diff(b) < 16,
                    "{art:?} {s}: {top:?}"
                );
            }
            assert!(png_art(16, TrayArt::RcDot).is_none());
        }
        let (w, _, dot) = rgba_art(64, TrayArt::RcDot).unwrap();
        assert_eq!(pixel(&dot, w, (48, 48)), [255, 255, 255, 255]);
        for &s in SIZES {
            assert_eq!(png_art(s, TrayArt::Plain), png(s, TrayLevel::None));
            assert_eq!(
                png_art(s, TrayArt::Level(TrayLevel::Mention)),
                png(s, TrayLevel::Mention)
            );
        }
        assert!(png_art(32, TrayArt::RcCount(0)).is_none());
        assert_eq!(
            png_art(32, TrayArt::RcCount(10)),
            png_art(32, TrayArt::RcPlus)
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        for art in RC_ARTS {
            assert_eq!(tray_pixmaps_art(art).len(), 4);
        }
    }

    /// The window icon: the desktop app's cut-out (a disc of radius
    /// `size/4` at three quarters, plus the square to the corner) is clear
    /// outside the badge, and the badge disc (`size/3`) sits in its centre.
    #[test]
    fn window_icon_hole_and_disc() {
        let size = 256;
        let (w, _, plain) = rgba(size, TrayLevel::None).unwrap();
        assert_eq!(
            window_rgba_art(size, TrayArt::Plain).unwrap().2,
            plain,
            "no badge"
        );
        for art in RC_ARTS {
            let (_, _, px) = window_rgba_art(size, art).unwrap();
            let at = |x, y| pixel(&px, w, (x, y));
            // The logo under the hole's top edge is opaque in the plain icon.
            assert_eq!(pixel(&plain, w, (192, 140))[3], 255);
            // Hole: inside the cut-out disc, outside the badge disc.
            assert_eq!(at(192, 140)[3], 0, "{art:?}: hole above the badge");
            assert_eq!(at(140, 192)[3], 0, "{art:?}: hole left of the badge");
            assert_eq!(at(250, 250)[3], 0, "{art:?}: the corner square");
            // Disc: the badge's top inside its radius (size/6 ≈ 42.7).
            let top = at(192, 155);
            assert_eq!(top[3], 255, "{art:?}");
            let (r, g, b) = badge_art::DISC;
            assert_eq!((top[0], top[1], top[2]), (r, g, b), "{art:?}");
            // Away from the hole the logo is unchanged.
            assert_eq!(at(128, 40), pixel(&plain, w, (128, 40)));
        }
        let (_, _, dot) = window_rgba_art(size, TrayArt::RcDot).unwrap();
        assert_eq!(pixel(&dot, w, (192, 192)), [255, 255, 255, 255]);
        assert!(window_icon_rc(TrayArt::RcCount(4)).is_some());
    }

    /// The macOS menu bar shows the dot template for every Rocket.Chat
    /// counter and the bullet.
    #[test]
    fn rc_menubar_is_the_dot() {
        let dot = menubar_rgba(TrayLevel::Mention).unwrap();
        for art in RC_ARTS {
            assert_eq!(menubar_rgba_art(art).unwrap(), dot);
        }
        assert_eq!(
            menubar_rgba_art(TrayArt::Plain).unwrap(),
            menubar_rgba(TrayLevel::None).unwrap()
        );
    }
}
