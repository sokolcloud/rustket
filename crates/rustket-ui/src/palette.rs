//! Derives a full token set from a few seed colours (`[light.palette]` /
//! `[dark.palette]` of a theme file).
//!
//! Each seed recomputes the tokens that depend on it; seeds that are not set
//! keep the built-in value of their mode as input. The groups:
//!
//! - `background` + `text` → surfaces, hover/pressed/selected rows, borders,
//!   quote/code backgrounds, text levels, tooltip (and, because their tints
//!   mix into the background, every group below).
//! - `accent` → primary and its states, tints, focus colour, link, selection,
//!   info banner, quiet unread badge.
//! - `danger`, `success`, `warning` → their buttons, text colours, banners,
//!   mention colours, presence dots and mention badge.
//!
//! The formulas are plain sRGB mixes by fixed fractions that roughly
//! reproduce how the built-in palettes relate to their seeds. A final pass
//! nudges derived foregrounds ([`ensure_contrast`]) so that palette-only
//! themes meet WCAG AA in most cases; the seeds themselves are never changed.

use iced::Color;

use crate::theme::{MIN_GRAPHIC_CONTRAST, MIN_TEXT_CONTRAST, Mode, Tokens, contrast_ratio};

/// Seed colours of one variant; `None` keeps the built-in input.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Seeds {
    pub accent: Option<Color>,
    pub danger: Option<Color>,
    pub success: Option<Color>,
    pub warning: Option<Color>,
    /// The room surface.
    pub background: Option<Color>,
    /// Primary text.
    pub text: Option<Color>,
}

impl Seeds {
    /// Seed names as written in theme files.
    pub const NAMES: [&'static str; 6] = [
        "accent",
        "danger",
        "success",
        "warning",
        "background",
        "text",
    ];

    /// Sets seed `name`; `false` for an unknown name.
    pub fn set(&mut self, name: &str, c: Color) -> bool {
        let slot = match name {
            "accent" => &mut self.accent,
            "danger" => &mut self.danger,
            "success" => &mut self.success,
            "warning" => &mut self.warning,
            "background" => &mut self.background,
            "text" => &mut self.text,
            _ => return false,
        };
        *slot = Some(c);
        true
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The seed values the built-in tokens of a mode correspond to.
    pub fn of(t: &Tokens) -> [(&'static str, Color); 6] {
        [
            ("accent", t.primary),
            ("danger", t.danger),
            ("success", t.success),
            ("warning", t.warning),
            ("background", t.surface_room),
            ("text", t.text_primary),
        ]
    }
}

/// `a` moved toward `b` by `t` (0 = `a`, 1 = `b`), per sRGB channel and alpha.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: f32, y: f32| x + (y - x) * t;
    Color {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    }
}

fn worst_contrast(fg: Color, bgs: &[Color]) -> f32 {
    bgs.iter()
        .map(|bg| contrast_ratio(fg, *bg))
        .fold(f32::INFINITY, f32::min)
}

/// `fg`, moved in 5 % steps toward black (light backgrounds) or white (dark
/// ones) until it reaches `min` against every colour in `bgs`. Returns `fg`
/// unchanged when it already does, black/white when nothing short of it does.
pub fn ensure_contrast(fg: Color, bgs: &[Color], min: f32) -> Color {
    if bgs.is_empty() || worst_contrast(fg, bgs) >= min {
        return fg;
    }
    let lum = bgs.iter().map(|c| c.relative_luminance()).sum::<f32>() / bgs.len() as f32;
    // Above ~0.18 black contrasts more than white.
    let target = if lum > 0.18 {
        Color::BLACK
    } else {
        Color::WHITE
    };
    ensure_contrast_toward(fg, bgs, min, target)
}

/// [`ensure_contrast`] toward a given colour.
fn ensure_contrast_toward(fg: Color, bgs: &[Color], min: f32, target: Color) -> Color {
    if worst_contrast(fg, bgs) >= min {
        return fg;
    }
    let target = Color { a: fg.a, ..target };
    for step in 1..=20 {
        let c = mix(fg, target, step as f32 * 0.05);
        if worst_contrast(c, bgs) >= min {
            return c;
        }
    }
    target
}

/// White when it reaches 4.5:1 on all of `bgs` (the usual look of filled
/// buttons), else white or black, whichever contrasts more.
pub fn on_color(bgs: &[Color]) -> Color {
    let white = worst_contrast(Color::WHITE, bgs);
    if white >= MIN_TEXT_CONTRAST || white >= worst_contrast(Color::BLACK, bgs) {
        Color::WHITE
    } else {
        Color::BLACK
    }
}

fn with_alpha(c: Color, a: u8) -> Color {
    Color {
        a: f32::from(a) / 255.0,
        ..c
    }
}

/// A greyer version of `c` (half way to the grey of its channel mean).
fn desaturate(c: Color, amount: f32) -> Color {
    let m = (c.r + c.g + c.b) / 3.0;
    mix(c, Color::from_rgb(m, m, m), amount)
}

/// Applies the seeds to `t` (the built-in tokens of its mode, usually).
pub fn apply_seeds(t: &mut Tokens, s: &Seeds) {
    if s.is_empty() {
        return;
    }
    let dark = t.mode == Mode::Dark;
    let neutral = s.background.is_some() || s.text.is_some();
    if neutral {
        neutrals(
            t,
            s.background.unwrap_or(t.surface_room),
            s.text.unwrap_or(t.text_primary),
            dark,
        );
    }
    if s.accent.is_some() || neutral {
        accent(t, s.accent.unwrap_or(t.primary), dark);
    }
    if s.warning.is_some() || neutral {
        warning(t, s.warning.unwrap_or(t.warning), dark);
    }
    if s.danger.is_some() || neutral {
        danger(t, s.danger.unwrap_or(t.danger), dark);
    }
    if s.success.is_some() || neutral {
        success(t, s.success.unwrap_or(t.success), dark);
    }
    fix_contrast(t);
}

/// Background mixed toward text by fixed fractions (light / dark).
fn neutrals(t: &mut Tokens, bg: Color, text: Color, dark: bool) {
    let f = |light: f32, dark_f: f32| mix(bg, text, if dark { dark_f } else { light });
    // In the dark palette the rail and sunken wells are darker than the room
    // (toward black), in the light one toward the text.
    let deeper = |light: f32| {
        if dark {
            mix(bg, Color::BLACK, 0.16)
        } else {
            mix(bg, text, light)
        }
    };
    t.surface_room = bg;
    t.surface_sidebar = f(0.03, 0.034);
    t.surface_rail = deeper(0.07);
    t.surface_raised = f(0.0, 0.063);
    t.surface_sunken = deeper(0.055);
    t.hover_bg = f(0.055, 0.055);
    t.pressed_bg = f(0.11, 0.082);
    t.sidebar_hover_bg = f(0.085, 0.058);
    t.sidebar_selected_bg = f(0.10, 0.082);
    t.border = f(0.11, 0.10);
    t.border_strong = f(0.22, 0.19);
    t.quote_bg = t.surface_sidebar;
    t.quote_bar = f(0.22, 0.23);
    t.code_bg = f(0.055, 0.063);
    t.badge_muted = t.border_strong;
    t.focus_ring_inner = if dark { t.surface_sunken } else { bg };

    let toward_bg = |light: f32, dark_f: f32| mix(text, bg, if dark { dark_f } else { light });
    t.text_primary = text;
    t.text_secondary = toward_bg(0.21, 0.28);
    t.text_hint = toward_bg(0.30, 0.41);
    t.text_disabled = toward_bg(0.67, 0.70);
    t.presence_offline = toward_bg(0.55, 0.45);
    t.code_fg = text;
    t.badge_muted_fg = text;
    t.tooltip_bg = mix(text, bg, 0.07);
    t.tooltip_fg = bg;
}

fn accent(t: &mut Tokens, accent: Color, dark: bool) {
    let bg = t.surface_room;
    t.primary = accent;
    if dark {
        t.primary_pressed = mix(accent, Color::BLACK, 0.12);
        // Hover lightens, as far as the button text keeps 4.5:1.
        let on = on_color(&[accent, t.primary_pressed]);
        t.primary_hover = [0.12, 0.08, 0.05]
            .into_iter()
            .map(|f| mix(accent, Color::WHITE, f))
            .find(|c| contrast_ratio(on, *c) >= MIN_TEXT_CONTRAST)
            .unwrap_or_else(|| mix(accent, Color::WHITE, 0.03));
        t.border_focus = mix(accent, Color::WHITE, 0.2);
        t.link = mix(accent, Color::WHITE, 0.45);
        t.info_fg = mix(accent, Color::WHITE, 0.55);
        t.badge_unread = mix(desaturate(accent, 0.45), Color::BLACK, 0.3);
    } else {
        t.primary_hover = mix(accent, Color::BLACK, 0.15);
        t.primary_pressed = mix(accent, Color::BLACK, 0.28);
        t.border_focus = accent;
        t.link = t.primary_hover;
        t.info_fg = t.primary_pressed;
        t.badge_unread = desaturate(accent, 0.45);
    }
    t.on_primary = on_color(&[t.primary, t.primary_hover, t.primary_pressed]);
    let tint = mix(bg, accent, if dark { 0.16 } else { 0.10 });
    t.primary_tint = tint;
    t.selected_bg = tint;
    t.info_bg = tint;
    t.own_message_bg = mix(bg, accent, if dark { 0.06 } else { 0.04 });
    t.selection = with_alpha(t.border_focus, if dark { 89 } else { 64 });
}

fn warning(t: &mut Tokens, w: Color, dark: bool) {
    let bg = t.surface_room;
    t.warning = w;
    t.presence_away = w;
    if dark {
        t.warning_icon = w;
        t.warning_fg = mix(w, Color::WHITE, 0.3);
        t.mention_fg = mix(w, Color::WHITE, 0.35);
    } else {
        t.warning_icon = mix(w, Color::BLACK, 0.3);
        t.warning_fg = mix(w, Color::BLACK, 0.5);
        t.mention_fg = mix(w, Color::BLACK, 0.45);
    }
    t.presence_away_ring = t.warning_icon;
    t.warning_bg = mix(bg, w, if dark { 0.2 } else { 0.13 });
    t.mention_bg = mix(bg, w, if dark { 0.25 } else { 0.18 });
    t.highlight_bg = mix(bg, w, if dark { 0.12 } else { 0.1 });
}

fn danger(t: &mut Tokens, d: Color, dark: bool) {
    let bg = t.surface_room;
    t.danger = d;
    t.danger_hover = mix(d, Color::BLACK, 0.1);
    t.danger_pressed = mix(d, Color::BLACK, if dark { 0.17 } else { 0.2 });
    t.on_danger = on_color(&[t.danger, t.danger_hover, t.danger_pressed]);
    t.badge_mention = d;
    if dark {
        t.danger_text = mix(d, Color::WHITE, 0.45);
        t.error_fg = mix(d, Color::WHITE, 0.55);
        t.mention_me_fg = mix(d, Color::WHITE, 0.5);
        t.presence_busy = mix(d, Color::WHITE, 0.3);
    } else {
        t.danger_text = mix(d, Color::BLACK, 0.15);
        t.error_fg = mix(d, Color::BLACK, 0.25);
        t.mention_me_fg = mix(d, Color::BLACK, 0.25);
        t.presence_busy = d;
    }
    t.error_bg = mix(bg, d, if dark { 0.2 } else { 0.09 });
    t.mention_me_bg = mix(bg, d, if dark { 0.25 } else { 0.12 });
}

fn success(t: &mut Tokens, s: Color, dark: bool) {
    let bg = t.surface_room;
    t.success = s;
    t.presence_online = s;
    if dark {
        t.success_text = s;
        t.success_fg = mix(s, Color::WHITE, 0.45);
    } else {
        t.success_text = mix(s, Color::BLACK, 0.25);
        t.success_fg = mix(s, Color::BLACK, 0.25);
    }
    t.success_bg = mix(bg, s, if dark { 0.2 } else { 0.1 });
}

/// Nudges derived foregrounds (never seeds) to WCAG AA on what they are
/// drawn on: the pairs of [`crate::theme::contrast_report`].
fn fix_contrast(t: &mut Tokens) {
    let text_bgs = [
        t.surface_room,
        t.surface_sidebar,
        t.surface_raised,
        t.surface_sunken,
        t.hover_bg,
        t.pressed_bg,
        t.selected_bg,
        t.sidebar_hover_bg,
        t.sidebar_selected_bg,
        t.highlight_bg,
        t.own_message_bg,
        t.quote_bg,
        t.primary_tint,
    ];
    let main = [t.surface_room, t.surface_sidebar, t.surface_raised];
    let (text, graphic) = (MIN_TEXT_CONTRAST, MIN_GRAPHIC_CONTRAST);
    t.text_secondary = ensure_contrast(t.text_secondary, &text_bgs, text);
    t.text_hint = ensure_contrast(t.text_hint, &text_bgs, text);
    t.link = ensure_contrast(t.link, &text_bgs, text);
    t.danger_text = ensure_contrast(t.danger_text, &text_bgs, text);
    t.warning_icon = ensure_contrast(t.warning_icon, &text_bgs, graphic);
    t.presence_online = ensure_contrast(t.presence_online, &text_bgs, graphic);
    t.presence_busy = ensure_contrast(t.presence_busy, &text_bgs, graphic);
    t.success_text = ensure_contrast(t.success_text, &main, text);
    t.presence_away_ring = ensure_contrast(t.presence_away_ring, &main, graphic);
    t.border_focus = ensure_contrast(
        t.border_focus,
        &[
            t.surface_room,
            t.surface_sidebar,
            t.surface_raised,
            t.surface_sunken,
        ],
        graphic,
    );
    t.code_fg = ensure_contrast(t.code_fg, &[t.code_bg], text);
    t.mention_fg = ensure_contrast(t.mention_fg, &[t.mention_bg], text);
    t.mention_me_fg = ensure_contrast(t.mention_me_fg, &[t.mention_me_bg, t.highlight_bg], text);
    t.tooltip_fg = ensure_contrast(t.tooltip_fg, &[t.tooltip_bg], text);
    t.info_fg = ensure_contrast(t.info_fg, &[t.info_bg], text);
    t.warning_fg = ensure_contrast(t.warning_fg, &[t.warning_bg], text);
    t.error_fg = ensure_contrast(t.error_fg, &[t.error_bg], text);
    t.success_fg = ensure_contrast(t.success_fg, &[t.success_bg], text);
    t.badge_muted_fg = ensure_contrast(t.badge_muted_fg, &[t.badge_muted], text);
    // Badge backgrounds move away from the badge text instead.
    for badge in [
        &mut t.badge_unread,
        &mut t.badge_mention,
        &mut t.badge_group_mention,
        &mut t.badge_thread,
    ] {
        *badge = ensure_contrast(*badge, &[t.badge_fg], text);
    }
    // The inner line of the focus ring carries it on filled widgets.
    t.focus_ring_inner = ensure_contrast_toward(
        t.focus_ring_inner,
        &[t.primary, t.primary_hover, t.danger],
        graphic,
        if t.mode == Mode::Dark {
            Color::BLACK
        } else {
            Color::WHITE
        },
    );
    let flag_bgs = [
        t.surface_sidebar,
        t.sidebar_hover_bg,
        t.sidebar_selected_bg,
        t.surface_room,
        t.surface_raised,
        t.hover_bg,
        t.pressed_bg,
        t.selected_bg,
    ];
    for f in &mut t.flag {
        *f = ensure_contrast(*f, &flag_bgs, graphic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT, contrast_report};

    fn rgb(hex: u32) -> Color {
        Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    fn close(a: Color, b: Color, tol: f32) -> bool {
        (a.r - b.r).abs() <= tol && (a.g - b.g).abs() <= tol && (a.b - b.b).abs() <= tol
    }

    #[test]
    fn mix_and_on_color() {
        assert_eq!(mix(Color::BLACK, Color::WHITE, 0.0), Color::BLACK);
        assert_eq!(mix(Color::BLACK, Color::WHITE, 1.0), Color::WHITE);
        assert!(close(
            mix(Color::BLACK, Color::WHITE, 0.5),
            Color::from_rgb(0.5, 0.5, 0.5),
            1e-6
        ));
        assert_eq!(on_color(&[rgb(0x156FF5)]), Color::WHITE);
        assert_eq!(on_color(&[rgb(0xF3BE08)]), Color::BLACK);
    }

    #[test]
    fn ensure_contrast_nudges_only_when_needed() {
        let white = Color::WHITE;
        let ok = rgb(0x1A1D22);
        assert_eq!(ensure_contrast(ok, &[white], 4.5), ok);
        let pale = rgb(0xAAAAAA);
        let fixed = ensure_contrast(pale, &[white], 4.5);
        assert!(contrast_ratio(fixed, white) >= 4.5);
        assert!(fixed.relative_luminance() < pale.relative_luminance());
        // On dark backgrounds it lightens.
        let dim = rgb(0x555555);
        let bg = rgb(0x1F2329);
        let fixed = ensure_contrast(dim, &[bg], 4.5);
        assert!(contrast_ratio(fixed, bg) >= 4.5);
        assert!(fixed.relative_luminance() > dim.relative_luminance());
    }

    /// Hover darker than the base in light, lighter in dark; pressed darker.
    #[test]
    fn accent_states_move_the_right_way() {
        let a = rgb(0x8A3FD1);
        for base in [LIGHT, DARK] {
            let mut t = base;
            apply_seeds(
                &mut t,
                &Seeds {
                    accent: Some(a),
                    ..Seeds::default()
                },
            );
            let lum = |c: Color| c.relative_luminance();
            assert_eq!(t.primary, a);
            assert!(lum(t.primary_pressed) < lum(a));
            if t.mode == Mode::Dark {
                assert!(lum(t.primary_hover) > lum(a));
            } else {
                assert!(lum(t.primary_hover) < lum(a));
            }
            assert!(contrast_report(&t).is_empty(), "{:#?}", contrast_report(&t));
        }
    }

    /// Seeding the built-in values reproduces the built-in neutrals closely.
    #[test]
    fn builtin_seeds_roughly_reproduce_builtins() {
        for base in [LIGHT, DARK] {
            let mut t = base;
            let mut s = Seeds::default();
            for (name, c) in Seeds::of(&base) {
                assert!(s.set(name, c));
            }
            apply_seeds(&mut t, &s);
            for (name, a, b) in [
                ("sidebar", t.surface_sidebar, base.surface_sidebar),
                ("rail", t.surface_rail, base.surface_rail),
                ("sunken", t.surface_sunken, base.surface_sunken),
                ("hover", t.hover_bg, base.hover_bg),
                ("pressed", t.pressed_bg, base.pressed_bg),
                ("border", t.border, base.border),
                ("border_strong", t.border_strong, base.border_strong),
                ("secondary", t.text_secondary, base.text_secondary),
                ("hint", t.text_hint, base.text_hint),
                ("tint", t.primary_tint, base.primary_tint),
            ] {
                assert!(close(a, b, 0.04), "{:?} {name}: {a:?} vs {b:?}", base.mode);
            }
            assert!(contrast_report(&t).is_empty(), "{:#?}", contrast_report(&t));
        }
    }

    /// A few palette-only themes pass the contrast report without overrides.
    #[test]
    fn palette_only_themes_pass_aa() {
        let cases = [
            (
                LIGHT,
                Some(rgb(0xFBF7EF)),
                Some(rgb(0x2B2620)),
                rgb(0xB0466A),
            ),
            (LIGHT, Some(rgb(0xEEF3F7)), None, rgb(0x0B7070)),
            (
                DARK,
                Some(rgb(0x14181C)),
                Some(rgb(0xE6E2DA)),
                rgb(0xD08A2E),
            ),
            (DARK, Some(rgb(0x2A2633)), None, rgb(0x7FA7E0)),
        ];
        for (base, background, text, accent) in cases {
            let mut t = base;
            apply_seeds(
                &mut t,
                &Seeds {
                    background,
                    text,
                    accent: Some(accent),
                    ..Seeds::default()
                },
            );
            let report = contrast_report(&t);
            assert!(report.is_empty(), "{:?} {accent:?}: {report:#?}", base.mode);
        }
    }

    #[test]
    fn empty_seeds_change_nothing() {
        let mut t = DARK;
        apply_seeds(&mut t, &Seeds::default());
        assert_eq!(t, DARK);
        let mut s = Seeds::default();
        assert!(!s.set("nope", Color::BLACK));
        assert!(s.is_empty());
    }
}
