//! Rustket palette, design tokens, spacing/typography scales and the iced theme.

use std::borrow::Cow;

use iced::font::Weight;
use iced::theme::Palette;
use iced::{Color, Font, Theme};

/// Light or dark appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Light,
    Dark,
}

impl Mode {
    pub fn toggle(self) -> Self {
        match self {
            Mode::Light => Mode::Dark,
            Mode::Dark => Mode::Light,
        }
    }

    pub fn tokens(self) -> &'static Tokens {
        match self {
            Mode::Light => &LIGHT,
            Mode::Dark => &DARK,
        }
    }
}

// Spacing scale (logical pixels).
pub const SPACE_0: f32 = 2.0;
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_5: f32 = 20.0;
pub const SPACE_6: f32 = 24.0;
pub const SPACE_8: f32 = 32.0;
pub const SPACE_10: f32 = 40.0;

// Corner radii.
pub const RADIUS_XS: f32 = 2.0;
pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 8.0;
pub const RADIUS_XL: f32 = 12.0;
pub const RADIUS_PILL: f32 = 999.0;

// Font size scale.
pub const TEXT_MICRO: f32 = 10.0;
pub const TEXT_CAPTION: f32 = 12.0;
pub const TEXT_SMALL: f32 = 13.0;
pub const TEXT_BODY: f32 = 14.0;
pub const TEXT_SUBTITLE: f32 = 16.0;
pub const TEXT_TITLE: f32 = 18.0;
pub const TEXT_HEADLINE: f32 = 22.0;
pub const TEXT_DISPLAY: f32 = 28.0;
/// Size used for "big emoji" messages.
pub const TEXT_EMOJI_LARGE: f32 = 32.0;

// Layout sizes.
pub const RAIL_WIDTH: f32 = 64.0;
pub const SIDEBAR_WIDTH: f32 = 280.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 200.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 480.0;
pub const HEADER_HEIGHT: f32 = 56.0;

/// Default UI font. Noto Sans covers Latin, Cyrillic and Greek; other scripts and
/// colour emoji (Noto Color Emoji) are picked up through font fallback. Regular,
/// Medium, SemiBold and Bold are bundled ([`fonts`]): many distributions ship only
/// Regular/Bold, and cosmic-text takes a face of the default family only on an
/// exact weight match, so a missing weight would fall back to another family.
pub const DEFAULT_FONT: Font = Font::with_name("Noto Sans");
pub const FONT_MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..DEFAULT_FONT
};
pub const FONT_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..DEFAULT_FONT
};
pub const FONT_BOLD: Font = Font {
    weight: Weight::Bold,
    ..DEFAULT_FONT
};
/// Monospace font for inline code and code blocks (system monospace family).
pub const MONOSPACE_FONT: Font = Font::MONOSPACE;

/// Fonts that must be loaded at startup: pass each to `iced::application(..).font(..)`.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(lucide_icons::LUCIDE_FONT_BYTES),
        Cow::Borrowed(NOTO_SANS_REGULAR),
        Cow::Borrowed(NOTO_SANS_MEDIUM),
        Cow::Borrowed(NOTO_SANS_SEMIBOLD),
        Cow::Borrowed(NOTO_SANS_BOLD),
    ]
}

// Noto Sans (SIL Open Font License 1.1, see assets/fonts/OFL.txt).
const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");
const NOTO_SANS_MEDIUM: &[u8] = include_bytes!("../assets/fonts/NotoSans-Medium.ttf");
const NOTO_SANS_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/NotoSans-SemiBold.ttf");
const NOTO_SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/NotoSans-Bold.ttf");

/// Application settings with the Rustket default font, 14px body text and the
/// bundled fonts: `iced::application(..).settings(rustket_ui::theme::settings())`.
pub fn settings() -> iced::Settings {
    iced::Settings {
        fonts: fonts(),
        default_font: DEFAULT_FONT,
        default_text_size: iced::Pixels(TEXT_BODY),
        ..iced::Settings::default()
    }
}

/// Design tokens for one appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub mode: Mode,

    // Brand and semantic colours.
    pub primary: Color,
    pub primary_hover: Color,
    pub primary_pressed: Color,
    pub on_primary: Color,
    pub primary_tint: Color,
    pub danger: Color,
    pub danger_hover: Color,
    pub danger_pressed: Color,
    pub on_danger: Color,
    pub warning: Color,
    pub success: Color,
    /// `danger` for text and small icons (meets 4.5:1 on every surface).
    pub danger_text: Color,
    /// `success` for text.
    pub success_text: Color,
    /// `warning` for icons on light surfaces (3:1).
    pub warning_icon: Color,

    // Surfaces.
    pub surface_room: Color,
    pub surface_sidebar: Color,
    pub surface_rail: Color,
    pub surface_raised: Color,
    pub surface_sunken: Color,
    pub backdrop: Color,
    pub shadow: Color,

    // Text.
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_hint: Color,
    pub text_disabled: Color,

    // Lines.
    pub border: Color,
    pub border_strong: Color,
    pub border_focus: Color,
    /// Inner line of the keyboard focus ring (contrast on filled widgets).
    pub focus_ring_inner: Color,

    // Interactive backgrounds.
    pub hover_bg: Color,
    pub pressed_bg: Color,
    pub selected_bg: Color,
    pub sidebar_hover_bg: Color,
    pub sidebar_selected_bg: Color,

    // Message content.
    pub mention_bg: Color,
    pub mention_fg: Color,
    pub mention_me_bg: Color,
    pub mention_me_fg: Color,
    pub own_message_bg: Color,
    pub highlight_bg: Color,
    pub link: Color,
    pub code_bg: Color,
    pub code_fg: Color,
    pub quote_bar: Color,
    pub quote_bg: Color,
    pub selection: Color,

    // Badges.
    pub badge_unread: Color,
    pub badge_mention: Color,
    pub badge_group_mention: Color,
    pub badge_thread: Color,
    pub badge_muted: Color,
    pub badge_fg: Color,
    pub badge_muted_fg: Color,

    // Presence.
    pub presence_online: Color,
    pub presence_away: Color,
    /// 1px ring around the away dot so the yellow keeps 3:1 against light surfaces.
    pub presence_away_ring: Color,
    pub presence_busy: Color,
    pub presence_offline: Color,

    // Banners/toasts: background, foreground (text/icon), border.
    pub info_bg: Color,
    pub info_fg: Color,
    pub warning_bg: Color,
    pub warning_fg: Color,
    pub error_bg: Color,
    pub error_fg: Color,
    pub success_bg: Color,
    pub success_fg: Color,

    pub tooltip_bg: Color,
    pub tooltip_fg: Color,

    /// Flag colours in palette order (red, orange, yellow, green, blue,
    /// purple): marks and swatches, 3:1 on sidebar rows and menus.
    pub flag: [Color; FLAG_COUNT],
}

/// Number of flag colours in [`Tokens::flag`].
pub const FLAG_COUNT: usize = 6;

impl Tokens {
    /// Flag colour `index` (palette order); an unknown index falls back to
    /// the hint colour.
    pub fn flag_color(&self, index: usize) -> Color {
        self.flag.get(index).copied().unwrap_or(self.text_hint)
    }
}

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

const fn hexa(rgb: u32, a: f32) -> Color {
    Color::from_rgba8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
        a,
    )
}

pub const BRAND_BLUE: Color = hex(0x156FF5);
pub const BRAND_DANGER: Color = hex(0xEC0D2A);
pub const BRAND_WARNING: Color = hex(0xF3BE08);
pub const BRAND_SUCCESS: Color = hex(0x158D65);

pub static LIGHT: Tokens = Tokens {
    mode: Mode::Light,

    primary: BRAND_BLUE,
    primary_hover: hex(0x0D5BD1),
    primary_pressed: hex(0x0A4BB0),
    on_primary: Color::WHITE,
    primary_tint: hex(0xE8F2FF),
    danger: BRAND_DANGER,
    danger_hover: hex(0xD40C26),
    danger_pressed: hex(0xBB0B21),
    on_danger: Color::WHITE,
    warning: BRAND_WARNING,
    success: BRAND_SUCCESS,
    danger_text: hex(0xC40B23),
    success_text: hex(0x0E6B4C),
    warning_icon: hex(0xB45309),

    surface_room: Color::WHITE,
    surface_sidebar: hex(0xF7F8FA),
    surface_rail: hex(0xEEEFF1),
    surface_raised: Color::WHITE,
    surface_sunken: hex(0xF2F3F5),
    backdrop: hexa(0x0B0D10, 0.6),
    shadow: hexa(0x000000, 0.14),

    text_primary: hex(0x1A1D22),
    text_secondary: hex(0x4A4F57),
    text_hint: hex(0x5F656D),
    text_disabled: hex(0xB4B8BD),

    border: hex(0xE4E7EA),
    border_strong: hex(0xCBCED1),
    border_focus: BRAND_BLUE,
    focus_ring_inner: Color::WHITE,

    hover_bg: hex(0xF2F3F5),
    pressed_bg: hex(0xE4E7EA),
    selected_bg: hex(0xE8F2FF),
    sidebar_hover_bg: hex(0xEBEDF0),
    sidebar_selected_bg: hex(0xE7E9EC),

    mention_bg: hex(0xFFF1D8),
    mention_fg: hex(0x8E5A00),
    mention_me_bg: hex(0xFFE3E6),
    mention_me_fg: hex(0xB00A20),
    own_message_bg: hex(0xF5F9FF),
    highlight_bg: hex(0xFFF6D6),
    link: hex(0x0D5BD1),
    code_bg: hex(0xF2F3F5),
    code_fg: hex(0x1A1D22),
    quote_bar: hex(0xCBCED1),
    quote_bg: hex(0xF7F8FA),
    selection: hexa(0x156FF5, 0.25),

    badge_unread: BRAND_BLUE,
    badge_mention: BRAND_DANGER,
    badge_group_mention: hex(0xB35300),
    badge_thread: hex(0x10529E),
    badge_muted: hex(0xCBCED1),
    badge_fg: Color::WHITE,
    badge_muted_fg: hex(0x1A1D22),

    presence_online: BRAND_SUCCESS,
    presence_away: hex(0xF5B000),
    presence_away_ring: hex(0xB45309),
    presence_busy: BRAND_DANGER,
    presence_offline: hex(0x9EA2A8),

    info_bg: hex(0xE8F2FF),
    info_fg: hex(0x0A4BB0),
    warning_bg: hex(0xFFF8E0),
    warning_fg: hex(0x7A5A00),
    error_bg: hex(0xFFE9EC),
    error_fg: hex(0xB00A20),
    success_bg: hex(0xE5F7F0),
    success_fg: hex(0x0E6B4C),

    tooltip_bg: hex(0x2F343D),
    tooltip_fg: Color::WHITE,

    flag: [
        hex(0xD93025),
        hex(0xC45A00),
        hex(0xA07800),
        hex(0x188038),
        hex(0x1A73E8),
        hex(0x9334E6),
    ],
};

pub static DARK: Tokens = Tokens {
    mode: Mode::Dark,

    primary: hex(0x1263E0),
    primary_hover: hex(0x156FF5),
    primary_pressed: hex(0x0D5BD1),
    on_primary: Color::WHITE,
    primary_tint: hex(0x1D2D47),
    danger: BRAND_DANGER,
    danger_hover: hex(0xD40C26),
    danger_pressed: hex(0xC40B23),
    on_danger: Color::WHITE,
    warning: BRAND_WARNING,
    success: hex(0x1DBF84),
    danger_text: hex(0xFF8593),
    success_text: hex(0x1DBF84),
    warning_icon: BRAND_WARNING,

    surface_room: hex(0x1F2329),
    surface_sidebar: hex(0x262931),
    surface_rail: hex(0x1A1D22),
    surface_raised: hex(0x2C3038),
    surface_sunken: hex(0x1A1D22),
    backdrop: hexa(0x000000, 0.7),
    shadow: hexa(0x000000, 0.45),

    text_primary: hex(0xEEF1F5),
    text_secondary: hex(0xB4B8BE),
    text_hint: hex(0x999DA3),
    text_disabled: hex(0x5C6168),

    border: hex(0x343841),
    border_strong: hex(0x474C55),
    border_focus: hex(0x3A86F7),
    focus_ring_inner: hex(0x1A1D22),

    hover_bg: hex(0x2A2E35),
    pressed_bg: hex(0x30343B),
    selected_bg: hex(0x1D2D47),
    sidebar_hover_bg: hex(0x2B2F37),
    sidebar_selected_bg: hex(0x30343C),

    mention_bg: hex(0x45391A),
    mention_fg: hex(0xFFD666),
    mention_me_bg: hex(0x4D1F27),
    mention_me_fg: hex(0xFF8A99),
    own_message_bg: hex(0x232936),
    highlight_bg: hex(0x332E1C),
    link: hex(0x7DB0FF),
    code_bg: hex(0x2C3038),
    code_fg: hex(0xEEF1F5),
    quote_bar: hex(0x4F555E),
    quote_bg: hex(0x262931),
    selection: hexa(0x3A86F7, 0.35),

    badge_unread: BRAND_BLUE,
    badge_mention: BRAND_DANGER,
    badge_group_mention: hex(0xB35300),
    badge_thread: hex(0x1A66D6),
    badge_muted: hex(0x474C55),
    badge_fg: Color::WHITE,
    badge_muted_fg: hex(0xEEF1F5),

    presence_online: hex(0x1DBF84),
    presence_away: BRAND_WARNING,
    presence_away_ring: BRAND_WARNING,
    presence_busy: hex(0xFF5C6E),
    presence_offline: hex(0x9EA2A8),

    info_bg: hex(0x1D2D47),
    info_fg: hex(0x9CC3FF),
    warning_bg: hex(0x3D3416),
    warning_fg: hex(0xFFD666),
    error_bg: hex(0x45202A),
    error_fg: hex(0xFF9AA7),
    success_bg: hex(0x173A2F),
    success_fg: hex(0x7FE0BC),

    tooltip_bg: hex(0xE4E7EA),
    tooltip_fg: hex(0x1F2329),

    flag: [
        hex(0xF28B82),
        hex(0xFCAD70),
        hex(0xFDD663),
        hex(0x81C995),
        hex(0x8AB4F8),
        hex(0xC58AF9),
    ],
};

/// Builds the iced theme for a mode.
pub fn theme(mode: Mode) -> Theme {
    let t = mode.tokens();
    let name = match mode {
        Mode::Light => "Rustket Light",
        Mode::Dark => "Rustket Dark",
    };
    Theme::custom(
        name,
        Palette {
            background: t.surface_room,
            text: t.text_primary,
            primary: t.primary,
            success: t.success,
            warning: t.warning,
            danger: t.danger,
        },
    )
}

/// Returns the tokens matching an iced theme (dark tokens for any dark theme).
pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// WCAG relative-luminance contrast ratio between two opaque colours.
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let la = a.relative_luminance();
    let lb = b.relative_luminance();
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: f32 = 4.5;
    const GRAPHIC: f32 = 3.0;

    /// fontdb (what cosmic-text matches with) finds a bundled face of exactly
    /// each UI weight under the family "Noto Sans".
    #[test]
    fn every_ui_weight_is_bundled() {
        use iced::advanced::graphics::text::cosmic_text::fontdb;
        let mut db = fontdb::Database::new();
        for face in [
            NOTO_SANS_REGULAR,
            NOTO_SANS_MEDIUM,
            NOTO_SANS_SEMIBOLD,
            NOTO_SANS_BOLD,
        ] {
            db.load_font_data(face.to_vec());
        }
        assert_eq!(db.len(), 4);
        let iced::font::Family::Name(family) = DEFAULT_FONT.family else {
            panic!("the UI font is not a named family");
        };
        for font in [DEFAULT_FONT, FONT_MEDIUM, FONT_SEMIBOLD, FONT_BOLD] {
            let want = match font.weight {
                Weight::Normal => 400,
                Weight::Medium => 500,
                Weight::Semibold => 600,
                Weight::Bold => 700,
                other => panic!("unexpected weight {other:?}"),
            };
            assert_eq!(font.family, DEFAULT_FONT.family);
            let id = db
                .query(&fontdb::Query {
                    families: &[fontdb::Family::Name(family)],
                    weight: fontdb::Weight(want),
                    ..fontdb::Query::default()
                })
                .unwrap_or_else(|| panic!("no {family} face for weight {want}"));
            let face = db.face(id).expect("queried face exists");
            assert_eq!(
                face.weight.0, want,
                "weight {want} falls back to another face"
            );
            assert!(
                face.families.iter().any(|(name, _)| name == family),
                "{:?}",
                face.families
            );
        }
        assert_eq!(fonts().len(), 5);
    }

    fn both() -> [&'static Tokens; 2] {
        [&LIGHT, &DARK]
    }

    /// Backgrounds that body/secondary/hint text and timestamps are drawn on.
    fn text_surfaces(t: &Tokens) -> Vec<(&'static str, Color)> {
        vec![
            ("room", t.surface_room),
            ("sidebar", t.surface_sidebar),
            ("raised", t.surface_raised),
            ("sunken", t.surface_sunken),
            ("hover", t.hover_bg),
            ("pressed", t.pressed_bg),
            ("selected", t.selected_bg),
            ("sidebar hover", t.sidebar_hover_bg),
            ("sidebar selected", t.sidebar_selected_bg),
            ("mention row", t.highlight_bg),
            ("own row", t.own_message_bg),
            ("quote", t.quote_bg),
            ("primary tint", t.primary_tint),
        ]
    }

    fn check(
        t: &Tokens,
        fg: (&str, Color),
        bg: (&str, Color),
        min: f32,
        failures: &mut Vec<String>,
    ) {
        let ratio = contrast_ratio(fg.1, bg.1);
        if ratio < min {
            failures.push(format!(
                "{:?}: {} on {} = {ratio:.2} < {min}",
                t.mode, fg.0, bg.0
            ));
        }
    }

    #[test]
    fn every_text_pair_meets_wcag_aa() {
        let mut failures = Vec::new();
        for t in both() {
            for bg in text_surfaces(t) {
                for fg in [
                    ("text_primary", t.text_primary),
                    ("text_secondary", t.text_secondary),
                    ("text_hint", t.text_hint),
                    ("link", t.link),
                    ("danger_text", t.danger_text),
                ] {
                    check(t, fg, bg, TEXT, &mut failures);
                }
                // Icons and presence/status graphics.
                for fg in [
                    ("warning_icon", t.warning_icon),
                    ("presence_online", t.presence_online),
                    ("presence_busy", t.presence_busy),
                ] {
                    check(t, fg, bg, GRAPHIC, &mut failures);
                }
            }
            for bg in [
                ("room", t.surface_room),
                ("sidebar", t.surface_sidebar),
                ("raised", t.surface_raised),
            ] {
                check(t, ("success_text", t.success_text), bg, TEXT, &mut failures);
                check(
                    t,
                    ("presence_away_ring", t.presence_away_ring),
                    bg,
                    GRAPHIC,
                    &mut failures,
                );
            }
            for (name, fg, bg) in [
                ("code", t.code_fg, t.code_bg),
                ("mention", t.mention_fg, t.mention_bg),
                ("mention me", t.mention_me_fg, t.mention_me_bg),
                ("tooltip", t.tooltip_fg, t.tooltip_bg),
                ("on primary", t.on_primary, t.primary),
                ("on primary hover", t.on_primary, t.primary_hover),
                ("on danger", t.on_danger, t.danger),
                ("info banner", t.info_fg, t.info_bg),
                ("warning banner", t.warning_fg, t.warning_bg),
                ("error banner", t.error_fg, t.error_bg),
                ("success banner", t.success_fg, t.success_bg),
                ("unread badge", t.badge_fg, t.badge_unread),
                ("mention badge", t.badge_fg, t.badge_mention),
                ("@all badge", t.badge_fg, t.badge_group_mention),
                ("thread badge", t.badge_fg, t.badge_thread),
                ("muted badge", t.badge_muted_fg, t.badge_muted),
                ("text on mention row", t.text_primary, t.highlight_bg),
                ("mention me on row", t.mention_me_fg, t.highlight_bg),
            ] {
                check(t, (name, fg), ("its background", bg), TEXT, &mut failures);
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// WCAG 2.2 AA non-text contrast (1.4.11) for the keyboard focus ring:
    /// on every surface an interactive widget sits on, one of its two lines
    /// (focus colour outside, `focus_ring_inner` inside) reaches 3:1 against
    /// what is around it, also on filled widgets (primary, danger, selected).
    #[test]
    fn focus_ring_is_visible_everywhere() {
        let mut failures = Vec::new();
        for t in both() {
            let mut surfaces = text_surfaces(t);
            surfaces.extend([
                ("rail", t.surface_rail),
                ("primary", t.primary),
                ("primary hover", t.primary_hover),
                ("danger", t.danger),
                ("primary tint", t.primary_tint),
                ("tooltip", t.tooltip_bg),
            ]);
            for (name, bg) in surfaces {
                let outer = contrast_ratio(t.border_focus, bg);
                let inner = contrast_ratio(t.focus_ring_inner, bg);
                // The two lines also differ from each other, so the ring
                // reads as one double line.
                let between = contrast_ratio(t.border_focus, t.focus_ring_inner);
                if outer.max(inner) < GRAPHIC || between < GRAPHIC {
                    failures.push(format!(
                        "{:?}: ring on {name}: outer {outer:.2}, inner {inner:.2}, between {between:.2}",
                        t.mode
                    ));
                }
            }
            // Text inputs show focus with the focus colour as their border.
            for (name, bg) in [
                ("room", t.surface_room),
                ("sidebar", t.surface_sidebar),
                ("raised", t.surface_raised),
                ("sunken", t.surface_sunken),
            ] {
                check(
                    t,
                    ("border_focus", t.border_focus),
                    (name, bg),
                    GRAPHIC,
                    &mut failures,
                );
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Placeholders, captions and disabled-looking but readable states.
    #[test]
    fn secondary_pairs_meet_wcag_aa() {
        let mut failures = Vec::new();
        for t in both() {
            for (name, fg, bg) in [
                ("on primary pressed", t.on_primary, t.primary_pressed),
                ("on danger hover", t.on_danger, t.danger_hover),
                ("on danger pressed", t.on_danger, t.danger_pressed),
                ("hint on tooltip-free raised", t.text_hint, t.surface_raised),
                ("link on primary tint", t.link, t.primary_tint),
                ("danger text on raised", t.danger_text, t.surface_raised),
            ] {
                check(t, (name, fg), ("its background", bg), TEXT, &mut failures);
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Flag marks and swatches (non-text, 3:1) on every surface they are
    /// drawn on: sidebar rows in each state, menus and the room header.
    #[test]
    fn flag_colors_are_visible() {
        let mut failures = Vec::new();
        for t in both() {
            for (i, fg) in t.flag.iter().enumerate() {
                for bg in [
                    ("sidebar", t.surface_sidebar),
                    ("sidebar hover", t.sidebar_hover_bg),
                    ("sidebar selected", t.sidebar_selected_bg),
                    ("room", t.surface_room),
                    ("raised", t.surface_raised),
                    ("menu hover", t.hover_bg),
                    ("pressed", t.pressed_bg),
                    ("selected", t.selected_bg),
                ] {
                    check(t, ("flag", *fg), bg, GRAPHIC, &mut failures);
                }
                // No two flags share a colour.
                for other in &t.flag[i + 1..] {
                    assert_ne!(fg, other, "{:?}", t.mode);
                }
            }
            assert_eq!(t.flag_color(0), t.flag[0]);
            assert_eq!(t.flag_color(FLAG_COUNT), t.text_hint);
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn text_levels_keep_a_visible_hierarchy() {
        for t in both() {
            let primary_secondary = contrast_ratio(t.text_primary, t.text_secondary);
            let secondary_hint = contrast_ratio(t.text_secondary, t.text_hint);
            assert!(
                primary_secondary >= 1.75,
                "{:?}: {primary_secondary:.2}",
                t.mode
            );
            assert!(secondary_hint >= 1.35, "{:?}: {secondary_hint:.2}", t.mode);
            // Hint sits near the AA floor on the main surface, not far above it.
            assert!(
                contrast_ratio(t.text_hint, t.surface_room) < 6.5,
                "{:?}",
                t.mode
            );
        }
    }

    #[test]
    fn hover_states_move_away_from_the_base() {
        for t in both() {
            let lighter = |a: Color, b: Color| a.relative_luminance() > b.relative_luminance();
            if t.mode == Mode::Dark {
                assert!(lighter(t.primary_hover, t.primary));
                assert!(!lighter(t.primary_pressed, t.primary));
            } else {
                assert!(!lighter(t.primary_hover, t.primary));
            }
        }
    }

    #[test]
    fn theme_mode_round_trips() {
        assert_eq!(tokens(&theme(Mode::Light)).mode, Mode::Light);
        assert_eq!(tokens(&theme(Mode::Dark)).mode, Mode::Dark);
        assert_eq!(Mode::Light.toggle(), Mode::Dark);
    }
}
