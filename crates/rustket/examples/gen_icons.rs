//! Renders the app icons (`assets/icons/*.png`) with tiny-skia: a blue chat
//! bubble with a white spark, and the tray's unread variants with a dot: red
//! (`rustket-unread`, mentions), orange (`rustket-group`, `@all`) and the
//! quiet unread colour (`rustket-quiet`). Also
//! writes `rustket.ico` (Windows executable and installer) from the same
//! renderer, `rustket.icns` (macOS bundle) from a squircle tile on the macOS
//! icon grid, the macOS menu bar template glyphs (`rustket-menubar*`), and
//! the Rocket.Chat scheme's tray icons (`rustket-rc-{1..9,plus,dot}`): the
//! desktop app's counter badge (`Badge.tsx`) at the bottom right of the logo,
//! in a cut-out, as its `AppIcon.tsx` places it.
//!
//! ```sh
//! cargo run -p rustket --example gen_icons -- crates/rustket/assets/icons
//! # only the rustket-rc-* icons:
//! cargo run -p rustket --example gen_icons -- crates/rustket/assets/icons --rc-only
//! # only the asset catalog icon set (Assets.car, packaging/macos/build.sh):
//! cargo run -p rustket --example gen_icons -- packaging/macos/AppIcon.appiconset --appiconset
//! ```

use std::path::PathBuf;

use rustket::desktop::badge_art::{self, Glyph};

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LinearGradient, Mask, MaskType, Paint, Path,
    PathBuilder, Pixmap, Point, Rect, Shader, SpreadMode, Stroke, Transform,
};

const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 128, 256, 512];
type Rgb = (u8, u8, u8);

/// File name prefixes and dot colours (the theme's `badge_mention`,
/// `badge_group_mention` and light `badge_unread`).
const VARIANTS: &[(&str, Option<Rgb>)] = &[
    ("rustket", None),
    ("rustket-unread", Some((0xEC, 0x0D, 0x2A))),
    ("rustket-group", Some((0xB3, 0x53, 0x00))),
    ("rustket-quiet", Some((0x55, 0x74, 0xA6))),
];
/// macOS menu bar templates (alpha only; the system tints them): 44 px, drawn
/// at 22 pt (2x), with no mark, a ring (quiet unread) or a dot (mentions).
const MENUBAR: &[(&str, Mark)] = &[
    ("rustket-menubar", Mark::None),
    ("rustket-menubar-ring", Mark::Ring),
    ("rustket-menubar-dot", Mark::Dot),
];
const MENUBAR_SIZE: u32 = 44;
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];
/// PNG-backed icns element types with their pixel size (`@2x` types included).
const ICNS_TYPES: &[(&[u8; 4], u32)] = &[
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

fn paint(shader: Shader<'static>) -> Paint<'static> {
    Paint {
        shader,
        anti_alias: true,
        ..Paint::default()
    }
}

fn solid(r: u8, g: u8, b: u8) -> Paint<'static> {
    paint(Shader::SolidColor(Color::from_rgba8(r, g, b, 255)))
}

/// Erases what it paints.
fn clear() -> Paint<'static> {
    Paint {
        blend_mode: BlendMode::Clear,
        anti_alias: true,
        ..Paint::default()
    }
}

/// The icon's blue gradient (#3D8BFF → #0B4FC7), top `y0` to bottom `y1`.
fn blue(y0: f32, y1: f32) -> Shader<'static> {
    LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x3D, 0x8B, 0xFF, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x0B, 0x4F, 0xC7, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("gradient")
}

/// A four-pointed spark centred at `(cx, cy)` with radius `r`.
fn spark(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32) {
    let k = r * 0.16;
    pb.move_to(cx, cy - r);
    pb.quad_to(cx + k, cy - k, cx + r, cy);
    pb.quad_to(cx + k, cy + k, cx, cy + r);
    pb.quad_to(cx - k, cy + k, cx - r, cy);
    pb.quad_to(cx - k, cy - k, cx, cy - r);
    pb.close();
}

/// The chat bubble in 256-unit space: a rounded rectangle `(x0, y0)`-`(x1, y1)`
/// with corner `r`, plus a tail at the bottom left reaching y = 246.
fn bubble_path(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.quad_to(x1, y0, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.quad_to(x1, y1, x1 - r, y1);
    pb.line_to(x0 + 104.0, y1);
    pb.line_to(x0 + 30.0, 246.0);
    pb.line_to(x0 + 44.0, y1);
    pb.line_to(x0 + r * 0.8, y1);
    pb.quad_to(x0, y1, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.quad_to(x0, y0, x0 + r, y0);
    pb.close();
    pb.finish().expect("bubble path")
}

/// The large-size bubble: (16, 22)-(240, 198), 224 units square with its tail.
fn large_bubble() -> Path {
    bubble_path(16.0, 22.0, 240.0, 198.0, 56.0)
}

fn render(size: u32, dot: Option<Rgb>) -> Pixmap {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / 256.0;
    let t = Transform::from_scale(s, s);
    let small = size < 32;

    // Bubble: rounded rectangle plus a tail at the bottom left.
    let (x0, y0, x1, y1, r) = if small {
        (8.0, 12.0, 248.0, 204.0, 60.0)
    } else {
        (16.0, 22.0, 240.0, 198.0, 56.0)
    };
    let bubble = bubble_path(x0, y0, x1, y1, r);
    pm.fill_path(&bubble, &paint(blue(y0, 246.0)), FillRule::Winding, t, None);

    // Spark (and a small companion spark on larger sizes).
    let mut pb = PathBuilder::new();
    let cy = (y0 + y1) / 2.0;
    spark(&mut pb, 128.0, cy, if small { 72.0 } else { 62.0 });
    if !small {
        spark(&mut pb, 186.0, 66.0, 18.0);
    }
    let sparks = pb.finish().expect("spark path");
    pm.fill_path(&sparks, &solid(255, 255, 255), FillRule::Winding, t, None);

    if let Some((red, green, blue)) = dot {
        let (cx, cy, r) = if small {
            (196.0, 60.0, 60.0)
        } else {
            (208.0, 48.0, 44.0)
        };
        let ring =
            PathBuilder::from_circle(cx, cy, r + if small { 14.0 } else { 10.0 }).expect("ring");
        pm.fill_path(&ring, &solid(255, 255, 255), FillRule::Winding, t, None);
        let dot = PathBuilder::from_circle(cx, cy, r).expect("dot");
        pm.fill_path(&dot, &solid(red, green, blue), FillRule::Winding, t, None);
    }
    pm
}

/// The mark on a menu bar template.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    Ring,
    Dot,
}

/// A macOS menu bar template: black with alpha (the menu bar tints it). The
/// bubble is 32 px (16 pt) at (6, 6) with the spark knocked out; the mark sits
/// at the top right corner, (36, 8), in a cleared gap.
fn menubar(mark: Mark) -> Pixmap {
    let mut pm = Pixmap::new(MENUBAR_SIZE, MENUBAR_SIZE).expect("pixmap");
    let s = 32.0 / 224.0;
    let t = Transform::from_row(s, 0.0, 0.0, s, 6.0 - 16.0 * s, 6.0 - 22.0 * s);
    pm.fill_path(&large_bubble(), &solid(0, 0, 0), FillRule::Winding, t, None);
    let mut pb = PathBuilder::new();
    spark(&mut pb, 128.0, 110.0, 66.0);
    let sparks = pb.finish().expect("spark path");
    pm.fill_path(&sparks, &clear(), FillRule::Winding, t, None);

    let (cx, cy) = (36.0, 8.0);
    let id = Transform::identity();
    if mark != Mark::None {
        let gap = PathBuilder::from_circle(cx, cy, 8.5).expect("gap");
        pm.fill_path(&gap, &clear(), FillRule::Winding, id, None);
    }
    match mark {
        Mark::None => {}
        Mark::Dot => {
            let dot = PathBuilder::from_circle(cx, cy, 6.0).expect("dot");
            pm.fill_path(&dot, &solid(0, 0, 0), FillRule::Winding, id, None);
        }
        Mark::Ring => {
            let ring = PathBuilder::from_circle(cx, cy, 4.9).expect("ring");
            let stroke = Stroke {
                width: 2.2,
                ..Stroke::default()
            };
            pm.stroke_path(&ring, &solid(0, 0, 0), &stroke, id, None);
        }
    }
    pm
}

/// A superellipse (n = 5) "squircle" of side `side` at `(x, y)`.
fn squircle(x: f32, y: f32, side: f32) -> Path {
    let (a, n) = (side / 2.0, 5.0f32);
    let (cx, cy) = (x + a, y + a);
    let mut pb = PathBuilder::new();
    let steps = 720;
    for i in 0..=steps {
        let th = i as f32 / steps as f32 * std::f32::consts::TAU;
        let (c, s) = (th.cos(), th.sin());
        let px = cx + a * c.signum() * c.abs().powf(2.0 / n);
        let py = cy + a * s.signum() * s.abs().powf(2.0 / n);
        if i == 0 {
            pb.move_to(px, py);
        } else {
            pb.line_to(px, py);
        }
    }
    pb.close();
    pb.finish().expect("squircle path")
}

/// The macOS app icon: a blue squircle tile on the macOS icon grid (824 of
/// 1024 units at (100, 100), no shadow) with the white bubble; the sparks
/// show the tile's gradient.
fn mac_tile(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let k = size as f32 / 1024.0;
    let t = Transform::from_scale(k, k);
    let tile = || paint(blue(100.0, 924.0));
    pm.fill_path(
        &squircle(100.0, 100.0, 824.0),
        &tile(),
        FillRule::Winding,
        t,
        None,
    );

    // The bubble (224 units) at 540, centred (optically a bit up).
    let s = 540.0 / 224.0;
    let bt =
        Transform::from_row(s, 0.0, 0.0, s, 512.0 - 128.0 * s, 502.0 - 134.0 * s).post_scale(k, k);
    pm.fill_path(
        &large_bubble(),
        &solid(255, 255, 255),
        FillRule::Winding,
        bt,
        None,
    );
    let mut pb = PathBuilder::new();
    spark(&mut pb, 128.0, 110.0, 62.0);
    spark(&mut pb, 186.0, 66.0, 18.0);
    let sparks = pb.finish().expect("spark path");
    let mut layer = Pixmap::new(size, size).expect("pixmap");
    layer.fill_path(&sparks, &solid(0, 0, 0), FillRule::Winding, bt, None);
    let mask = Mask::from_pixmap(layer.as_ref(), MaskType::Alpha);
    let all = Rect::from_xywh(0.0, 0.0, 1024.0, 1024.0).expect("rect");
    pm.fill_rect(all, &tile(), t, Some(&mask));
    pm
}

/// The Rocket.Chat counter icons: name suffix and glyph.
const RC_ART: &[(&str, Glyph)] = &[
    ("1", Glyph::Digit(1)),
    ("2", Glyph::Digit(2)),
    ("3", Glyph::Digit(3)),
    ("4", Glyph::Digit(4)),
    ("5", Glyph::Digit(5)),
    ("6", Glyph::Digit(6)),
    ("7", Glyph::Digit(7)),
    ("8", Glyph::Digit(8)),
    ("9", Glyph::Digit(9)),
    ("plus", Glyph::Plus),
    ("dot", Glyph::Dot),
];
/// The tray sizes (StatusNotifierItem; Windows uses 32).
const RC_SIZES: &[u32] = &[22, 32, 48, 64];

/// The logo with the desktop app's badge (`AppIcon.tsx`): a disc of radius
/// `size/4` centred at three quarters of the size is cut out, and the badge
/// (0.4 of the size wide) is centred there.
fn rc_tray(size: u32, glyph: Glyph) -> Pixmap {
    let mut pm = render(size, None);
    let s = size as f32;
    let (c, id) = (s * 0.75, Transform::identity());
    let cut = PathBuilder::from_circle(c, c, s / 4.0).expect("cut");
    pm.fill_path(&cut, &clear(), FillRule::Winding, id, None);
    let side = s * 0.4;
    let disc = PathBuilder::from_circle(c, c, side / 2.0).expect("disc");
    let (r, g, b) = badge_art::DISC;
    pm.fill_path(&disc, &solid(r, g, b), FillRule::Winding, id, None);
    let (vx, vy, vs) = badge_art::VIEW;
    let k = side / vs;
    let t = Transform::from_row(
        k,
        0.0,
        0.0,
        k,
        c - side / 2.0 - vx * k,
        c - side / 2.0 - vy * k,
    );
    let mut pb = PathBuilder::new();
    if glyph == Glyph::Dot {
        let (x, y, r) = badge_art::DOT;
        pb.push_circle(x, y, r);
    }
    for poly in badge_art::polygons(glyph) {
        for (i, (x, y)) in poly.into_iter().enumerate() {
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
    }
    let mark = pb.finish().expect("glyph path");
    pm.fill_path(&mark, &solid(255, 255, 255), FillRule::EvenOdd, t, None);
    pm
}

fn png(size: u32) -> Vec<u8> {
    render(size, None).encode_png().expect("encode png")
}

/// ICO with 32-bit BMP entries below 256 px (read by every Windows API) and a
/// PNG entry at 256 px, the usual layout of Windows application icons.
fn ico() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&size| {
            let data = if size >= 256 {
                png(size)
            } else {
                bmp(&render(size, None))
            };
            (size, data)
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        out.extend_from_slice(data);
    }
    out
}

/// A BITMAPINFOHEADER image for an ICO entry: bottom-up BGRA rows followed by
/// the AND mask (set where the pixel is fully transparent).
fn bmp(pm: &Pixmap) -> Vec<u8> {
    let (w, h) = (pm.width(), pm.height());
    let mask_stride = w.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40, w, h * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for v in [0, w * h * 4 + mask_stride * h, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let pixels = pm.pixels();
    for y in (0..h).rev() {
        for x in 0..w {
            let c = pixels[(y * w + x) as usize].demultiply();
            out.extend_from_slice(&[c.blue(), c.green(), c.red(), c.alpha()]);
        }
    }
    for y in (0..h).rev() {
        let mut row = vec![0u8; mask_stride as usize];
        for x in 0..w {
            if pixels[(y * w + x) as usize].alpha() == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

/// The `AppIcon.appiconset` entries (point size, scale), from [`mac_tile`]
/// like the icns; the catalog also needs the 16 pt @1x the icns lacks.
const APPICONSET: &[(u32, u32)] = &[
    (16, 1),
    (16, 2),
    (32, 1),
    (32, 2),
    (128, 1),
    (128, 2),
    (256, 1),
    (256, 2),
    (512, 1),
    (512, 2),
];

/// Writes the icon set's PNGs and `Contents.json` into `dir`.
fn appiconset(dir: &std::path::Path) {
    let mut images = Vec::new();
    for &(points, scale) in APPICONSET {
        let suffix = if scale == 1 {
            String::new()
        } else {
            format!("@{scale}x")
        };
        let file = format!("icon_{points}x{points}{suffix}.png");
        let path = dir.join(&file);
        mac_tile(points * scale).save_png(&path).expect("save png");
        println!("wrote {}", path.display());
        images.push(serde_json::json!({
            "filename": file,
            "idiom": "mac",
            "scale": format!("{scale}x"),
            "size": format!("{points}x{points}"),
        }));
    }
    let json = serde_json::json!({
        "images": images,
        "info": { "author": "xcode", "version": 1 },
    });
    let text = serde_json::to_string_pretty(&json).expect("json") + "\n";
    let path = dir.join("Contents.json");
    std::fs::write(&path, text).expect("write Contents.json");
    println!("wrote {}", path.display());
}

/// ICNS made of PNG elements (macOS 10.7+), from [`mac_tile`].
fn icns() -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, size) in ICNS_TYPES {
        let data = mac_tile(*size).encode_png().expect("encode png");
        body.extend_from_slice(*kind);
        body.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&data);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rc_only = args.iter().any(|a| a == "--rc-only");
    let iconset_only = args.iter().any(|a| a == "--appiconset");
    let out: PathBuf = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/rustket/assets/icons"));
    std::fs::create_dir_all(&out).expect("out dir");
    if iconset_only {
        appiconset(&out);
        return;
    }
    for &size in RC_SIZES {
        for (name, glyph) in RC_ART {
            let path = out.join(format!("rustket-rc-{name}-{size}.png"));
            rc_tray(size, *glyph).save_png(&path).expect("save png");
            println!("wrote {}", path.display());
        }
    }
    if rc_only {
        return;
    }
    for &size in SIZES {
        for (name, dot) in VARIANTS {
            let path = out.join(format!("{name}-{size}.png"));
            render(size, *dot).save_png(&path).expect("save png");
            println!("wrote {}", path.display());
        }
    }
    for (name, mark) in MENUBAR {
        let path = out.join(format!("{name}.png"));
        menubar(*mark).save_png(&path).expect("save png");
        println!("wrote {}", path.display());
    }
    for (name, data) in [("rustket.ico", ico()), ("rustket.icns", icns())] {
        let path = out.join(name);
        std::fs::write(&path, data).expect("write icon");
        println!("wrote {}", path.display());
    }
}
