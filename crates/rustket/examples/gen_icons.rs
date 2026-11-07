//! Renders the app icons (`assets/icons/*.png`) with tiny-skia: a blue chat
//! bubble with a white spark, and an "unread" variant with a red dot.
//!
//! ```sh
//! cargo run -p rustket --example gen_icons -- crates/rustket/assets/icons
//! ```

use std::path::PathBuf;

use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Shader,
    SpreadMode, Transform,
};

const SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 128, 256];

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
}
