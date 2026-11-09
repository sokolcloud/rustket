//! Renders the app icons (`assets/icons/*.png`) with tiny-skia: a blue chat
//! bubble with a white spark, and an "unread" variant with a red dot. Also
//! writes `rustket.ico` (Windows executable and installer) and `rustket.icns`
//! (macOS bundle) from the same renderer.
//!
//! ```sh
//! cargo run -p rustket --example gen_icons -- crates/rustket/assets/icons
//! ```

use std::path::PathBuf;

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Shader,
    SpreadMode, Transform,
};

const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 128, 256, 512];
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

fn render(size: u32, unread: bool) -> Pixmap {
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
    let bubble = pb.finish().expect("bubble path");
    let gradient = LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, 246.0),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x3D, 0x8B, 0xFF, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x0B, 0x4F, 0xC7, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("gradient");
    pm.fill_path(&bubble, &paint(gradient), FillRule::Winding, t, None);

    // Spark (and a small companion spark on larger sizes).
    let mut pb = PathBuilder::new();
    let cy = (y0 + y1) / 2.0;
    spark(&mut pb, 128.0, cy, if small { 72.0 } else { 62.0 });
    if !small {
        spark(&mut pb, 186.0, 66.0, 18.0);
    }
    let sparks = pb.finish().expect("spark path");
    pm.fill_path(&sparks, &solid(255, 255, 255), FillRule::Winding, t, None);

    if unread {
        let (cx, cy, r) = if small {
            (196.0, 60.0, 60.0)
        } else {
            (208.0, 48.0, 44.0)
        };
        let ring =
            PathBuilder::from_circle(cx, cy, r + if small { 14.0 } else { 10.0 }).expect("ring");
        pm.fill_path(&ring, &solid(255, 255, 255), FillRule::Winding, t, None);
        let dot = PathBuilder::from_circle(cx, cy, r).expect("dot");
        pm.fill_path(&dot, &solid(0xEC, 0x0D, 0x2A), FillRule::Winding, t, None);
    }
    pm
}

fn png(size: u32) -> Vec<u8> {
    render(size, false).encode_png().expect("encode png")
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
                bmp(&render(size, false))
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

/// ICNS made of PNG elements (macOS 10.7+).
fn icns() -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, size) in ICNS_TYPES {
        let data = png(*size);
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
    let out: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/rustket/assets/icons"));
    std::fs::create_dir_all(&out).expect("out dir");
    for &size in SIZES {
        for (unread, name) in [(false, "rustket"), (true, "rustket-unread")] {
            let path = out.join(format!("{name}-{size}.png"));
            render(size, unread).save_png(&path).expect("save png");
            println!("wrote {}", path.display());
        }
    }
    for (name, data) in [("rustket.ico", ico()), ("rustket.icns", icns())] {
        let path = out.join(name);
        std::fs::write(&path, data).expect("write icon");
        println!("wrote {}", path.display());
    }
}
