//! The Rocket.Chat desktop app's unread badge art (`ui/icons/Badge.tsx`): a
//! `#F5455C` disc filling the 23-unit view box at (36, 33) with a white pixel
//! glyph, "1"…"9", "+9" or a dot. Used for the tray icons
//! (`examples/gen_icons.rs`) and drawn at run time into the window icon.

/// The disc colour.
pub const DISC: (u8, u8, u8) = (0xF5, 0x45, 0x5C);
/// The view box: left, top, side.
pub const VIEW: (f32, f32, f32) = (36.0, 33.0, 23.0);
/// The dot glyph (the bullet "•"): centre and radius in view-box units.
pub const DOT: (f32, f32, f32) = (47.5, 44.5, 3.5);

/// What the badge shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// 1…9.
    Digit(u8),
    /// Above 9: "+9".
    Plus,
    /// The bullet "•".
    Dot,
}

/// The glyph paths of `Badge.tsx` (only `M m h H v V z` commands).
fn path(g: Glyph) -> &'static str {
    match g {
        Glyph::Digit(1) => "M44.4 39h4.4v8.8H51V50h-6.6v-2.2h2.2v-6.6h-2.2z",
        Glyph::Digit(2) => {
            "M43.1 40.1h1.1V39h6.6v1.1h1.1v4.4h-1.1v1.1h-5.5v2.2h6.6V50h-8.8v-5.5h1.1v-1.1h5.5v-2.2h-4.4v1.1h-2.2z"
        }
        Glyph::Digit(3) => {
            "M43.1 40.1h1.1V39h6.6v1.1h1.1v3.3h-1.1v2.2h1.1v3.3h-1.1V50h-6.6v-1.1h-1.1v-2.2h2.2v1.1h4.4v-2.2h-4.4v-2.2h4.4v-2.2h-4.4v1.1h-2.2z"
        }
        Glyph::Digit(4) => "M43.1 39h2.2v4.4h4.4V39h2.2v11h-2.2v-4.4h-6.6z",
        Glyph::Digit(5) => {
            "M42.9 39h8.8v2.2h-6.6v2.2h5.5v1.1h1.1v4.4h-1.1V50H44v-1.1h-1.1v-2.2h2.2v1.1h4.4v-2.2h-6.6z"
        }
        Glyph::Digit(6) => {
            "M43.1 40.1h1.1V39h6.6v1.1h1.1v2.2h-2.2v-1.1h-4.4v2.2h5.5v1.1h1.1v4.4h-1.1V50h-6.6v-1.1h-1.1v-8.8zm2.2 7.7h4.4v-2.2h-4.4v2.2z"
        }
        Glyph::Digit(7) => "M42.8 40.1h1.1V39h6.6v1.1h1.1V50h-2.2v-8.8H45v3.3h-2.2z",
        Glyph::Digit(8) => {
            "M43.1 40.1h1.1V39h6.6v1.1h1.1v3.3h-1.1v2.2h1.1v3.3h-1.1V50h-6.6v-1.1h-1.1v-3.3h1.1v-2.2h-1.1v-3.3zm2.2 7.7h4.4v-2.2h-4.4v2.2zm0-4.4h4.4v-2.2h-4.4v2.2z"
        }
        Glyph::Digit(9) => {
            "M43.1 40.1h1.1V39h6.6v1.1h1.1v8.8h-1.1V50h-6.6v-1.1h-1.1v-2.2h2.2v1.1h4.4v-2.2h-5.5v-1.1h-1.1v-4.4zm2.2 1.1v2.2h4.4v-2.2h-4.4z"
        }
        Glyph::Plus => {
            "M39.3 43.5h2v-2h2v2h2v2h-2v2h-2v-2h-2v-2zm7.68-3h1v-1h6v1h1v8h-1v1h-6v-1h-1v-2h2v1h4v-2h-5v-1h-1v-4zm2 1v2h4v-2h-4z"
        }
        Glyph::Digit(_) | Glyph::Dot => "",
    }
}

/// The glyph as closed polygons in view-box units, filled even-odd (empty
/// for [`Glyph::Dot`], a circle: [`DOT`]).
pub fn polygons(g: Glyph) -> Vec<Vec<(f32, f32)>> {
    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let (mut x, mut y, mut start) = (0.0f32, 0.0f32, (0.0f32, 0.0f32));
    let mut tokens = Tokens(path(g));
    while let Some(cmd) = tokens.command() {
        match cmd {
            'M' | 'm' => {
                if cur.len() > 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                let (dx, dy) = (tokens.number(), tokens.number());
                (x, y) = if cmd == 'M' {
                    (dx, dy)
                } else {
                    (x + dx, y + dy)
                };
                start = (x, y);
                cur.push(start);
            }
            'h' => x += tokens.number(),
            'H' => x = tokens.number(),
            'v' => y += tokens.number(),
            'V' => y = tokens.number(),
            _ => {
                (x, y) = start;
                if cur.len() > 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                continue;
            }
        }
        if !matches!(cmd, 'M' | 'm') {
            cur.push((x, y));
        }
    }
    if cur.len() > 2 {
        out.push(cur);
    }
    out
}

/// A tokenizer of the glyph paths.
struct Tokens<'a>(&'a str);

impl Tokens<'_> {
    fn command(&mut self) -> Option<char> {
        self.0 = self.0.trim_start();
        let c = self.0.chars().next()?;
        self.0 = &self.0[c.len_utf8()..];
        Some(c)
    }

    fn number(&mut self) -> f32 {
        self.0 = self.0.trim_start();
        let mut end = 0;
        let mut dot = false;
        for (i, c) in self.0.char_indices() {
            let ok = c.is_ascii_digit() || (c == '-' && i == 0) || (c == '.' && !dot);
            if !ok {
                break;
            }
            dot |= c == '.';
            end = i + c.len_utf8();
        }
        let (n, rest) = self.0.split_at(end);
        self.0 = rest;
        n.parse().unwrap_or(0.0)
    }
}

fn inside(polys: &[Vec<(f32, f32)>], px: f32, py: f32) -> bool {
    let mut odd = false;
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            let (x0, y0) = poly[i];
            let (x1, y1) = poly[(i + 1) % n];
            if (y0 > py) != (y1 > py) && px < x0 + (py - y0) / (y1 - y0) * (x1 - x0) {
                odd = !odd;
            }
        }
    }
    odd
}

/// Sub-samples per pixel side.
const SS: u32 = 4;

/// Source-over of a straight-alpha colour onto an RGBA pixel.
fn over(dst: &mut [u8], (r, g, b): (u8, u8, u8), a: f32) {
    if a <= 0.0 {
        return;
    }
    let da = f32::from(dst[3]) / 255.0;
    let oa = a + da * (1.0 - a);
    let mix = |s: u8, d: u8| {
        let v = (f32::from(s) * a + f32::from(d) * da * (1.0 - a)) / oa;
        v.round().clamp(0.0, 255.0) as u8
    };
    dst[0] = mix(r, dst[0]);
    dst[1] = mix(g, dst[1]);
    dst[2] = mix(b, dst[2]);
    dst[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Draws the badge of `glyph` into the square RGBA image `px` (side `size`)
/// with the disc's box at `(left, top)`, `side` pixels wide.
pub fn paint(px: &mut [u8], size: u32, (left, top, side): (f32, f32, f32), glyph: Glyph) {
    let polys = polygons(glyph);
    let scale = VIEW.2 / side;
    let (cx, cy, r) = (VIEW.0 + VIEW.2 / 2.0, VIEW.1 + VIEW.2 / 2.0, VIEW.2 / 2.0);
    let x0 = left.floor().max(0.0) as u32;
    let y0 = top.floor().max(0.0) as u32;
    let x1 = ((left + side).ceil() as u32).min(size);
    let y1 = ((top + side).ceil() as u32).min(size);
    let n = (SS * SS) as f32;
    for y in y0..y1 {
        for x in x0..x1 {
            let (mut disc, mut mark) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let ux = VIEW.0 + (fx - left) * scale;
                    let uy = VIEW.1 + (fy - top) * scale;
                    if (ux - cx).powi(2) + (uy - cy).powi(2) > r * r {
                        continue;
                    }
                    disc += 1;
                    let on = match glyph {
                        Glyph::Dot => (ux - DOT.0).powi(2) + (uy - DOT.1).powi(2) <= DOT.2 * DOT.2,
                        _ => inside(&polys, ux, uy),
                    };
                    mark += u32::from(on);
                }
            }
            let i = ((y * size + x) * 4) as usize;
            let Some(dst) = px.get_mut(i..i + 4) else {
                continue;
            };
            over(dst, DISC, disc as f32 / n);
            over(dst, (255, 255, 255), mark as f32 / n);
        }
    }
}

/// Clears (with anti-aliased edges) what `hole(x, y)` covers, sampled per
/// pixel at sub-pixel positions; the hole lies within `[from, size)²`.
pub fn clear(px: &mut [u8], size: u32, from: u32, hole: impl Fn(f32, f32) -> bool) {
    let n = (SS * SS) as f32;
    for y in from..size {
        for x in from..size {
            let mut covered = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    covered += u32::from(hole(fx, fy));
                }
            }
            if covered == 0 {
                continue;
            }
            let i = ((y * size + x) * 4 + 3) as usize;
            if let Some(a) = px.get_mut(i) {
                *a = (f32::from(*a) * (1.0 - covered as f32 / n)).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_paths_parse() {
        for d in 1..=9 {
            let p = polygons(Glyph::Digit(d));
            assert!(!p.is_empty(), "{d}");
            for poly in &p {
                for &(x, y) in poly {
                    assert!((36.0..=59.0).contains(&x) && (33.0..=56.0).contains(&y));
                }
            }
        }
        assert_eq!(polygons(Glyph::Digit(1)).len(), 1);
        assert_eq!(polygons(Glyph::Digit(8)).len(), 3);
        assert_eq!(polygons(Glyph::Plus).len(), 3);
        assert!(polygons(Glyph::Dot).is_empty());
        // "1": the stem's inside and the gap left of it.
        let one = polygons(Glyph::Digit(1));
        assert!(inside(&one, 47.5, 44.0));
        assert!(!inside(&one, 45.0, 44.0));
        // "8": its two holes stay empty.
        let eight = polygons(Glyph::Digit(8));
        assert!(!inside(&eight, 47.5, 46.7));
        assert!(!inside(&eight, 47.5, 42.3));
        assert!(inside(&eight, 44.0, 42.0));
    }

    #[test]
    fn paints_disc_and_glyph() {
        let size = 46;
        let mut px = vec![0u8; (size * size * 4) as usize];
        paint(&mut px, size, (0.0, 0.0, 46.0), Glyph::Dot);
        let at = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(23, 23), [255, 255, 255, 255], "the dot");
        assert_eq!(at(23, 5), [DISC.0, DISC.1, DISC.2, 255], "the disc");
        assert_eq!(at(1, 1)[3], 0, "outside");
    }
}
