//! Rustket palette, design tokens, spacing/typography scales and the iced theme.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

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

    /// The active tokens of this mode (an external theme's, or the
    /// built-in ones when none is active).
    pub fn tokens(self) -> &'static Tokens {
        active_tokens(self)
    }

    /// The built-in Rustket tokens of this mode.
    pub fn builtin(self) -> &'static Tokens {
        builtin_tokens(self)
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

// Corner radii: the built-in values, scaled by the active theme's
// `layout.radius` ([`radius_scale`]).
const RADIUS_XS: f32 = 2.0;
const RADIUS_SM: f32 = 4.0;
const RADIUS_MD: f32 = 6.0;
const RADIUS_LG: f32 = 8.0;
const RADIUS_XL: f32 = 12.0;
/// Fully rounded ends; not scaled.
pub const RADIUS_PILL: f32 = 999.0;
/// Allowed range of the corner-radius scale.
pub const RADIUS_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=2.0;

static RADIUS_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// The active corner-radius scale (1.0 without an external theme).
pub fn radius_scale() -> f32 {
    f32::from_bits(RADIUS_SCALE.load(Ordering::Relaxed))
}

pub fn radius_xs() -> f32 {
    RADIUS_XS * radius_scale()
}
pub fn radius_sm() -> f32 {
    RADIUS_SM * radius_scale()
}
pub fn radius_md() -> f32 {
    RADIUS_MD * radius_scale()
}
pub fn radius_lg() -> f32 {
    RADIUS_LG * radius_scale()
}
pub fn radius_xl() -> f32 {
    RADIUS_XL * radius_scale()
}

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
///
/// A theme's `[fonts]` replace them for the whole session: the fonts are
/// chosen once at startup ([`init_fonts`]) because iced's default font (used
/// by every widget without an explicit font) is fixed when the window opens.
const NOTO_SANS: Font = Font::with_name("Noto Sans");

/// The UI fonts per weight and the monospace font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSet {
    pub regular: Font,
    pub medium: Font,
    pub semibold: Font,
    pub bold: Font,
    /// Inline code and code blocks.
    pub mono: Font,
}

impl FontSet {
    /// Noto Sans (bundled) and the system monospace family.
    pub const BUILTIN: FontSet = FontSet {
        regular: NOTO_SANS,
        medium: Font {
            weight: Weight::Medium,
            ..NOTO_SANS
        },
        semibold: Font {
            weight: Weight::Semibold,
            ..NOTO_SANS
        },
        bold: Font {
            weight: Weight::Bold,
            ..NOTO_SANS
        },
        mono: Font::MONOSPACE,
    };
}

static FONTS: OnceLock<FontSet> = OnceLock::new();

/// Sets the session's fonts; only the first call has an effect (`false`
/// afterwards). Call it before [`settings`].
pub fn init_fonts(fonts: FontSet) -> bool {
    FONTS.set(fonts).is_ok()
}

/// The session's fonts ([`FontSet::BUILTIN`] unless [`init_fonts`] ran).
pub fn active_fonts() -> FontSet {
    FONTS.get().copied().unwrap_or(FontSet::BUILTIN)
}

/// Default UI font (regular weight).
pub fn font_regular() -> Font {
    active_fonts().regular
}
pub fn font_medium() -> Font {
    active_fonts().medium
}
pub fn font_semibold() -> Font {
    active_fonts().semibold
}
pub fn font_bold() -> Font {
    active_fonts().bold
}
/// Monospace font for inline code and code blocks.
pub fn font_mono() -> Font {
    active_fonts().mono
}

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
        default_font: font_regular(),
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

/// Lists every colour field of [`Tokens`] once and generates name-based
/// access (theme files, `--dump-theme`). The exhaustive destructuring in
/// `color_fields` stops the build when a field is added to `Tokens` but
/// not here.
macro_rules! token_colors {
    ($($name:ident),* $(,)?) => {
        /// Names of the colour tokens (Rust field names), in declaration order.
        pub const COLOR_TOKENS: &[&str] = &[$(stringify!($name)),*];

        impl Tokens {
            /// Sets the colour token `name`; `false` for an unknown name.
            pub fn set_color(&mut self, name: &str, color: Color) -> bool {
                match name {
                    $(stringify!($name) => self.$name = color,)*
                    _ => return false,
                }
                true
            }

            /// Every colour token with its name, in declaration order
            /// (without `flag`).
            pub fn colors(&self) -> Vec<(&'static str, Color)> {
                let Tokens { mode: _, flag: _, $($name),* } = self;
                vec![$((stringify!($name), *$name)),*]
            }
        }
    };
}

token_colors!(
    primary,
    primary_hover,
    primary_pressed,
    on_primary,
    primary_tint,
    danger,
    danger_hover,
    danger_pressed,
    on_danger,
    warning,
    success,
    danger_text,
    success_text,
    warning_icon,
    surface_room,
    surface_sidebar,
    surface_rail,
    surface_raised,
    surface_sunken,
    backdrop,
    shadow,
    text_primary,
    text_secondary,
    text_hint,
    text_disabled,
    border,
    border_strong,
    border_focus,
    focus_ring_inner,
    hover_bg,
    pressed_bg,
    selected_bg,
    sidebar_hover_bg,
    sidebar_selected_bg,
    mention_bg,
    mention_fg,
    mention_me_bg,
    mention_me_fg,
    own_message_bg,
    highlight_bg,
    link,
    code_bg,
    code_fg,
    quote_bar,
    quote_bg,
    selection,
    badge_unread,
    badge_mention,
    badge_group_mention,
    badge_thread,
    badge_muted,
    badge_fg,
    badge_muted_fg,
    presence_online,
    presence_away,
    presence_away_ring,
    presence_busy,
    presence_offline,
    info_bg,
    info_fg,
    warning_bg,
    warning_fg,
    error_bg,
    error_fg,
    success_bg,
    success_fg,
    tooltip_bg,
    tooltip_fg,
);

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// `a` is the alpha byte, so every built-in colour has an exact
/// `#RRGGBBAA` form (theme files and `--dump-theme` round-trip exactly).
const fn hexa(rgb: u32, a: u8) -> Color {
    Color::from_rgba8(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
        a as f32 / 255.0,
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
    backdrop: hexa(0x0B0D10, 153),
    shadow: hexa(0x000000, 36),

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
    selection: hexa(0x156FF5, 64),

    // Plain unread and thread-only counters are quiet; mentions stay loud.
    badge_unread: hex(0x5574A6),
    badge_mention: BRAND_DANGER,
    badge_group_mention: hex(0xB35300),
    badge_thread: hex(0x66758C),
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
    backdrop: hexa(0x000000, 179),
    shadow: hexa(0x000000, 115),

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
    selection: hexa(0x3A86F7, 89),

    badge_unread: hex(0x3A5F94),
    badge_mention: BRAND_DANGER,
    badge_group_mention: hex(0xB35300),
    badge_thread: hex(0x475263),
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

/// The built-in Rustket tokens of a mode.
pub fn builtin_tokens(mode: Mode) -> &'static Tokens {
    match mode {
        Mode::Light => &LIGHT,
        Mode::Dark => &DARK,
    }
}

/// The tokens in use: an external theme's (see [`set_active`]) or the
/// built-in ones.
struct Active {
    light: &'static Tokens,
    dark: &'static Tokens,
    /// Theme id (`None`: built-in), part of the iced theme name.
    id: Option<String>,
}

static ACTIVE: RwLock<Active> = RwLock::new(Active {
    light: &LIGHT,
    dark: &DARK,
    id: None,
});

/// Bumped whenever the active tokens or radius change (cache keys of
/// views that bake colours or radii into their widgets).
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Every distinct token set made `'static` by [`intern`].
static INTERNED: Mutex<Vec<&'static Tokens>> = Mutex::new(Vec::new());

/// A `'static` copy of `t`: the built-ins or an earlier equal copy when
/// there is one, otherwise a leaked box. Invariant: the leak is bounded by
/// the number of distinct token sets activated in a session (a few per
/// theme edit), each about 1 KiB.
fn intern(t: &Tokens) -> &'static Tokens {
    if *t == LIGHT {
        return &LIGHT;
    }
    if *t == DARK {
        return &DARK;
    }
    let mut seen = INTERNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = seen.iter().find(|s| **s == t) {
        return found;
    }
    let leaked: &'static Tokens = Box::leak(Box::new(*t));
    seen.push(leaked);
    leaked
}

/// Activates a theme: tokens for each mode, its id (`None`: built-in
/// Rustket) and corner-radius scale (clamped to [`RADIUS_SCALE_RANGE`]).
pub fn set_active(id: Option<&str>, light: &Tokens, dark: &Tokens, radius: f32) {
    let light = intern(&Tokens {
        mode: Mode::Light,
        ..*light
    });
    let dark = intern(&Tokens {
        mode: Mode::Dark,
        ..*dark
    });
    let radius = if radius.is_finite() {
        radius.clamp(*RADIUS_SCALE_RANGE.start(), *RADIUS_SCALE_RANGE.end())
    } else {
        1.0
    };
    let mut changed = RADIUS_SCALE.swap(radius.to_bits(), Ordering::Relaxed) != radius.to_bits();
    // Poisoned: nothing changes and reads fall back to the built-ins.
    if let Ok(mut a) = ACTIVE.write() {
        changed |=
            !std::ptr::eq(a.light, light) || !std::ptr::eq(a.dark, dark) || a.id.as_deref() != id;
        *a = Active {
            light,
            dark,
            id: id.map(str::to_owned),
        };
    }
    if changed {
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Changes whenever [`set_active`] changes the look.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The active tokens of a mode (built-ins if the registry is poisoned).
pub fn active_tokens(mode: Mode) -> &'static Tokens {
    match ACTIVE.read() {
        Ok(a) => match mode {
            Mode::Light => a.light,
            Mode::Dark => a.dark,
        },
        Err(_) => builtin_tokens(mode),
    }
}

/// Builds the iced theme for a mode from the active tokens.
pub fn theme(mode: Mode) -> Theme {
    let id = ACTIVE.read().ok().and_then(|a| a.id.clone());
    let name = match (id, mode) {
        (None, Mode::Light) => "Rustket Light".to_owned(),
        (None, Mode::Dark) => "Rustket Dark".to_owned(),
        (Some(id), Mode::Light) => format!("Rustket {id} Light"),
        (Some(id), Mode::Dark) => format!("Rustket {id} Dark"),
    };
    theme_named(mode.tokens(), name)
}

/// An iced theme from tokens. `is_dark` follows the tokens' mode, not the
/// background's luminance, so [`tokens`] maps it back to the same mode.
pub fn theme_named(t: &Tokens, name: impl Into<Cow<'static, str>>) -> Theme {
    let dark = t.mode == Mode::Dark;
    Theme::custom_with_fn(
        name,
        Palette {
            background: t.surface_room,
            text: t.text_primary,
            primary: t.primary,
            success: t.success,
            warning: t.warning,
            danger: t.danger,
        },
        move |p| {
            let mut e = iced::theme::palette::Extended::generate(p);
            e.is_dark = dark;
            e
        },
    )
}

/// Returns the active tokens matching an iced theme (dark tokens for any
/// dark theme).
pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        active_tokens(Mode::Dark)
    } else {
        active_tokens(Mode::Light)
    }
}

/// WCAG relative-luminance contrast ratio between two opaque colours.
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let la = a.relative_luminance();
    let lb = b.relative_luminance();
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG AA minimum for text.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;
/// WCAG AA minimum for graphics (icons, focus ring, marks).
pub const MIN_GRAPHIC_CONTRAST: f32 = 3.0;

/// A colour pair below its WCAG AA minimum ([`contrast_report`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastIssue {
    /// Which pair, by token names (`text_hint on sidebar`).
    pub what: String,
    pub ratio: f32,
    pub min: f32,
}

impl std::fmt::Display for ContrastIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {:.2} < {}", self.what, self.ratio, self.min)
    }
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

/// Surfaces flag marks and swatches are drawn on: sidebar rows in each
/// state, menus and the room header.
fn flag_surfaces(t: &Tokens) -> [(&'static str, Color); 8] {
    [
        ("sidebar", t.surface_sidebar),
        ("sidebar hover", t.sidebar_hover_bg),
        ("sidebar selected", t.sidebar_selected_bg),
        ("room", t.surface_room),
        ("raised", t.surface_raised),
        ("menu hover", t.hover_bg),
        ("pressed", t.pressed_bg),
        ("selected", t.selected_bg),
    ]
}

/// Every colour pair of `t` below its WCAG AA minimum: text on each
/// surface, icons and presence dots, the keyboard focus ring (WCAG 2.2
/// 1.4.11), filled buttons, banners, badges and flag marks. Empty for the
/// built-in tokens; for theme files the issues are shown as warnings.
pub fn contrast_report(t: &Tokens) -> Vec<ContrastIssue> {
    let mut out = Vec::new();
    let mut check = |fg: (&str, Color), bg: (&str, Color), min: f32| {
        let ratio = contrast_ratio(fg.1, bg.1);
        if ratio < min {
            out.push(ContrastIssue {
                what: format!("{} on {}", fg.0, bg.0),
                ratio,
                min,
            });
        }
    };
    let (text, graphic) = (MIN_TEXT_CONTRAST, MIN_GRAPHIC_CONTRAST);
    for bg in text_surfaces(t) {
        for fg in [
            ("text_primary", t.text_primary),
            ("text_secondary", t.text_secondary),
            ("text_hint", t.text_hint),
            ("link", t.link),
            ("danger_text", t.danger_text),
        ] {
            check(fg, bg, text);
        }
        for fg in [
            ("warning_icon", t.warning_icon),
            ("presence_online", t.presence_online),
            ("presence_busy", t.presence_busy),
        ] {
            check(fg, bg, graphic);
        }
    }
    for bg in [
        ("room", t.surface_room),
        ("sidebar", t.surface_sidebar),
        ("raised", t.surface_raised),
    ] {
        check(("success_text", t.success_text), bg, text);
        check(("presence_away_ring", t.presence_away_ring), bg, graphic);
    }
    for (name, fg, bg) in [
        ("code_fg on code_bg", t.code_fg, t.code_bg),
        ("mention_fg on mention_bg", t.mention_fg, t.mention_bg),
        (
            "mention_me_fg on mention_me_bg",
            t.mention_me_fg,
            t.mention_me_bg,
        ),
        ("tooltip_fg on tooltip_bg", t.tooltip_fg, t.tooltip_bg),
        ("on_primary on primary", t.on_primary, t.primary),
        ("on_primary on primary_hover", t.on_primary, t.primary_hover),
        (
            "on_primary on primary_pressed",
            t.on_primary,
            t.primary_pressed,
        ),
        ("on_danger on danger", t.on_danger, t.danger),
        ("on_danger on danger_hover", t.on_danger, t.danger_hover),
        ("on_danger on danger_pressed", t.on_danger, t.danger_pressed),
        ("info_fg on info_bg", t.info_fg, t.info_bg),
        ("warning_fg on warning_bg", t.warning_fg, t.warning_bg),
        ("error_fg on error_bg", t.error_fg, t.error_bg),
        ("success_fg on success_bg", t.success_fg, t.success_bg),
        ("badge_fg on badge_unread", t.badge_fg, t.badge_unread),
        ("badge_fg on badge_mention", t.badge_fg, t.badge_mention),
        (
            "badge_fg on badge_group_mention",
            t.badge_fg,
            t.badge_group_mention,
        ),
        ("badge_fg on badge_thread", t.badge_fg, t.badge_thread),
        (
            "badge_muted_fg on badge_muted",
            t.badge_muted_fg,
            t.badge_muted,
        ),
        (
            "text_primary on highlight_bg",
            t.text_primary,
            t.highlight_bg,
        ),
        (
            "mention_me_fg on highlight_bg",
            t.mention_me_fg,
            t.highlight_bg,
        ),
    ] {
        let ratio = contrast_ratio(fg, bg);
        if ratio < text {
            out.push(ContrastIssue {
                what: name.to_owned(),
                ratio,
                min: text,
            });
        }
    }
    // The focus ring: on every surface an interactive widget sits on, one
    // of its two lines (focus colour outside, `focus_ring_inner` inside)
    // reaches 3:1, and the two lines differ from each other.
    let mut ring = text_surfaces(t);
    ring.extend([
        ("rail", t.surface_rail),
        ("primary", t.primary),
        ("primary hover", t.primary_hover),
        ("danger", t.danger),
        ("tooltip", t.tooltip_bg),
    ]);
    let between = contrast_ratio(t.border_focus, t.focus_ring_inner);
    if between < graphic {
        out.push(ContrastIssue {
            what: "border_focus on focus_ring_inner".to_owned(),
            ratio: between,
            min: graphic,
        });
    }
    for (name, bg) in ring {
        let best = contrast_ratio(t.border_focus, bg).max(contrast_ratio(t.focus_ring_inner, bg));
        if best < graphic {
            out.push(ContrastIssue {
                what: format!("focus ring on {name}"),
                ratio: best,
                min: graphic,
            });
        }
    }
    // Text inputs show focus with the focus colour as their border.
    for bg in [
        ("room", t.surface_room),
        ("sidebar", t.surface_sidebar),
        ("raised", t.surface_raised),
        ("sunken", t.surface_sunken),
    ] {
        let ratio = contrast_ratio(t.border_focus, bg.1);
        if ratio < graphic {
            out.push(ContrastIssue {
                what: format!("border_focus on {}", bg.0),
                ratio,
                min: graphic,
            });
        }
    }
    for (i, fg) in t.flag.iter().enumerate() {
        for (name, bg) in flag_surfaces(t) {
            let ratio = contrast_ratio(*fg, bg);
            if ratio < graphic {
                out.push(ContrastIssue {
                    what: format!("flag[{i}] on {name}"),
                    ratio,
                    min: graphic,
                });
            }
        }
        // Flags must be told apart.
        for (j, other) in t.flag.iter().enumerate().skip(i + 1) {
            if fg == other {
                out.push(ContrastIssue {
                    what: format!("flag[{i}] same as flag[{j}]"),
                    ratio: 1.0,
                    min: 1.0,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let iced::font::Family::Name(family) = NOTO_SANS.family else {
            panic!("the UI font is not a named family");
        };
        let set = FontSet::BUILTIN;
        for font in [set.regular, set.medium, set.semibold, set.bold] {
            let want = match font.weight {
                Weight::Normal => 400,
                Weight::Medium => 500,
                Weight::Semibold => 600,
                Weight::Bold => 700,
                other => panic!("unexpected weight {other:?}"),
            };
            assert_eq!(font.family, NOTO_SANS.family);
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

    /// Text, icons, focus ring, filled widgets, banners, badges and flags
    /// of both built-in palettes meet WCAG AA.
    #[test]
    fn builtin_contrast_report_is_empty() {
        for t in both() {
            let report = contrast_report(t);
            assert!(report.is_empty(), "{:?}: {report:#?}", t.mode);
            assert_eq!(t.flag_color(0), t.flag[0]);
            assert_eq!(t.flag_color(FLAG_COUNT), t.text_hint);
        }
    }

    #[test]
    fn contrast_report_finds_bad_pairs() {
        let mut t = LIGHT;
        t.text_hint = t.surface_room;
        t.flag[1] = t.flag[0];
        t.focus_ring_inner = t.border_focus;
        let report = contrast_report(&t);
        let has = |what: &str| report.iter().any(|i| i.what == what);
        assert!(has("text_hint on room"), "{report:#?}");
        assert!(has("flag[0] same as flag[1]"));
        assert!(has("border_focus on focus_ring_inner"));
        assert!(report.iter().all(|i| i.ratio < i.min || i.min == 1.0));
    }

    #[test]
    fn color_tokens_cover_every_field() {
        let colors = LIGHT.colors();
        assert_eq!(colors.len(), COLOR_TOKENS.len());
        let mut t = DARK;
        for (name, c) in &colors {
            assert!(t.set_color(name, *c), "{name}");
        }
        t.flag = LIGHT.flag;
        t.mode = Mode::Light;
        assert_eq!(t, LIGHT);
        assert!(!t.set_color("mode", Color::BLACK));
        assert!(!t.set_color("flag", Color::BLACK));
    }

    #[test]
    fn interning_reuses_equal_tokens() {
        assert!(std::ptr::eq(intern(&LIGHT.clone()), &LIGHT));
        let mut t = DARK;
        t.link = Color::WHITE;
        let a = intern(&t);
        let b = intern(&t.clone());
        assert!(std::ptr::eq(a, b));
        assert_eq!(*a, t);
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
