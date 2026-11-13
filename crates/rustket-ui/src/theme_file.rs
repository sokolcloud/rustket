//! External colour themes: one TOML file per theme
//! (`<config dir>/themes/<id>.toml`, see `docs/themes/README.md`).
//!
//! Parsing is tolerant: unknown keys, bad colours and wrong flag lengths
//! become [`ThemeWarning`]s and the rest still loads. Only a TOML syntax
//! error, a missing `[meta] name` or a file without `[light]`/`[dark]` is
//! an error.

use std::fmt;

use iced::Color;

use crate::palette::{Seeds, apply_seeds};
use crate::theme::{
    COLOR_TOKENS, DARK, FLAG_COUNT, LIGHT, Mode, RADIUS_SCALE_RANGE, Tokens, builtin_tokens,
};

/// The theme format version this build writes and understands.
pub const FORMAT_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum ThemeFileError {
    #[error("not valid TOML: {0}")]
    Syntax(String),
    #[error("[meta] name is missing")]
    MissingName,
    #[error("neither [light] nor [dark] is present")]
    NoVariants,
}

/// Something in a theme file that was ignored or adjusted.
#[derive(Debug, Clone, PartialEq)]
pub enum ThemeWarning {
    /// A key this version does not know (dotted path).
    UnknownKey(String),
    /// A value of the wrong type or an unparsable colour.
    BadValue { key: String, value: String },
    /// `flag` must have exactly [`FLAG_COUNT`] colours.
    FlagLength { key: String, len: usize },
    /// `[meta] version` is newer than [`FORMAT_VERSION`].
    NewerVersion(i64),
    /// `[layout] radius` outside [`RADIUS_SCALE_RANGE`].
    RadiusClamped { value: f64, used: f32 },
    /// The `[fonts] ui` family is not installed.
    FontMissing(String),
    /// The `[fonts] mono` family is not installed.
    MonoFontMissing(String),
    /// The family lacks a UI weight; `used` stands in for `wanted`.
    FontWeight {
        family: String,
        wanted: u16,
        used: u16,
    },
}

impl fmt::Display for ThemeWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey(k) => write!(f, "unknown key {k}"),
            Self::BadValue { key, value } => write!(f, "bad value for {key}: {value}"),
            Self::FlagLength { key, len } => {
                write!(f, "{key} needs {FLAG_COUNT} colours, not {len}")
            }
            Self::NewerVersion(v) => write!(f, "format version {v} is newer than {FORMAT_VERSION}"),
            Self::RadiusClamped { value, used } => write!(f, "radius {value} clamped to {used}"),
            Self::FontMissing(family) => write!(f, "font {family} is not installed"),
            Self::MonoFontMissing(family) => {
                write!(f, "monospace font {family} is not installed")
            }
            Self::FontWeight {
                family,
                wanted,
                used,
            } => write!(f, "font {family} has no weight {wanted}; using {used}"),
        }
    }
}

/// `[meta]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Meta {
    pub name: String,
    pub name_ru: Option<String>,
    pub author: Option<String>,
    pub version: Option<i64>,
}

/// `[fonts]`: installed family names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FontPrefs {
    pub ui: Option<String>,
    pub mono: Option<String>,
}

/// A parsed theme file with both variants resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeFile {
    pub meta: Meta,
    /// Resolved tokens of `[light]` (`None`: not in the file).
    pub light: Option<Tokens>,
    pub dark: Option<Tokens>,
    pub fonts: FontPrefs,
    /// `[layout] radius`, clamped.
    pub radius: Option<f32>,
    pub warnings: Vec<ThemeWarning>,
}

impl ThemeFile {
    pub fn parse(src: &str) -> Result<Self, ThemeFileError> {
        let root: toml::Table = src
            .parse()
            .map_err(|e: toml::de::Error| ThemeFileError::Syntax(syntax_message(src, &e)))?;
        let mut warnings = Vec::new();
        let mut meta = Meta::default();
        let mut name = None;
        let mut light = None;
        let mut dark = None;
        let mut fonts = FontPrefs::default();
        let mut radius = None;
        for (key, value) in &root {
            match key.as_str() {
                "meta" => {
                    let Some(t) = table(value, "meta", &mut warnings) else {
                        continue;
                    };
                    for (k, v) in t {
                        let path = format!("meta.{k}");
                        match k.as_str() {
                            "name" => name = string(v, &path, &mut warnings),
                            "name_ru" => meta.name_ru = string(v, &path, &mut warnings),
                            "author" => meta.author = string(v, &path, &mut warnings),
                            "version" => match v.as_integer() {
                                Some(n) => meta.version = Some(n),
                                None => warnings.push(bad(&path, v)),
                            },
                            _ => warnings.push(ThemeWarning::UnknownKey(path)),
                        }
                    }
                }
                "light" | "dark" => {
                    let mode = if key == "light" {
                        Mode::Light
                    } else {
                        Mode::Dark
                    };
                    let Some(t) = table(value, key, &mut warnings) else {
                        continue;
                    };
                    let tokens = Some(variant(t, key, mode, &mut warnings));
                    if mode == Mode::Light {
                        light = tokens;
                    } else {
                        dark = tokens;
                    }
                }
                "fonts" => {
                    let Some(t) = table(value, "fonts", &mut warnings) else {
                        continue;
                    };
                    for (k, v) in t {
                        let path = format!("fonts.{k}");
                        let s = || string(v, &path, &mut Vec::new()).filter(|s| !s.is_empty());
                        match k.as_str() {
                            "ui" | "mono" => {
                                let Some(s) = s() else {
                                    warnings.push(bad(&path, v));
                                    continue;
                                };
                                if k == "ui" {
                                    fonts.ui = Some(s);
                                } else {
                                    fonts.mono = Some(s);
                                }
                            }
                            _ => warnings.push(ThemeWarning::UnknownKey(path)),
                        }
                    }
                }
                "layout" => {
                    let Some(t) = table(value, "layout", &mut warnings) else {
                        continue;
                    };
                    for (k, v) in t {
                        let path = format!("layout.{k}");
                        match k.as_str() {
                            "radius" => {
                                let n = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
                                match n.filter(|n| n.is_finite()) {
                                    None => warnings.push(bad(&path, v)),
                                    Some(n) => {
                                        let (lo, hi) = (
                                            f64::from(*RADIUS_SCALE_RANGE.start()),
                                            f64::from(*RADIUS_SCALE_RANGE.end()),
                                        );
                                        let used = n.clamp(lo, hi) as f32;
                                        if n < lo || n > hi {
                                            warnings.push(ThemeWarning::RadiusClamped {
                                                value: n,
                                                used,
                                            });
                                        }
                                        radius = Some(used);
                                    }
                                }
                            }
                            _ => warnings.push(ThemeWarning::UnknownKey(path)),
                        }
                    }
                }
                _ => warnings.push(ThemeWarning::UnknownKey(key.clone())),
            }
        }
        meta.name = name
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .ok_or(ThemeFileError::MissingName)?;
        if light.is_none() && dark.is_none() {
            return Err(ThemeFileError::NoVariants);
        }
        if let Some(v) = meta.version.filter(|v| *v > FORMAT_VERSION) {
            warnings.push(ThemeWarning::NewerVersion(v));
        }
        Ok(Self {
            meta,
            light,
            dark,
            fonts,
            radius,
            warnings,
        })
    }

    /// The tokens of a mode: the file's variant, else the built-in ones.
    pub fn tokens(&self, mode: Mode) -> Tokens {
        let own = match mode {
            Mode::Light => self.light,
            Mode::Dark => self.dark,
        };
        own.unwrap_or(*builtin_tokens(mode))
    }

    pub fn has(&self, mode: Mode) -> bool {
        match mode {
            Mode::Light => self.light.is_some(),
            Mode::Dark => self.dark.is_some(),
        }
    }
}

/// `line L, column C: <message>` (1-based, columns in characters), on one line.
fn syntax_message(src: &str, e: &toml::de::Error) -> String {
    let msg = e.message().split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(start) = e.span().map(|s| s.start).filter(|s| *s <= src.len()) else {
        return msg;
    };
    let mut at = start;
    while !src.is_char_boundary(at) {
        at -= 1;
    }
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("line {line}, column {col}: {msg}")
}

fn bad(key: &str, v: &toml::Value) -> ThemeWarning {
    ThemeWarning::BadValue {
        key: key.to_owned(),
        value: v.to_string(),
    }
}

fn table<'a>(
    v: &'a toml::Value,
    key: &str,
    warnings: &mut Vec<ThemeWarning>,
) -> Option<&'a toml::Table> {
    let t = v.as_table();
    if t.is_none() {
        warnings.push(bad(key, v));
    }
    t
}

fn string(v: &toml::Value, key: &str, warnings: &mut Vec<ThemeWarning>) -> Option<String> {
    let s = v.as_str().map(str::to_owned);
    if s.is_none() {
        warnings.push(bad(key, v));
    }
    s
}

fn color(v: &toml::Value, key: &str, warnings: &mut Vec<ThemeWarning>) -> Option<Color> {
    let c = v.as_str().and_then(parse_color);
    if c.is_none() {
        warnings.push(bad(key, v));
    }
    c
}

/// Built-in tokens of `mode` → `palette` seeds → `tokens` overrides.
fn variant(t: &toml::Table, key: &str, mode: Mode, warnings: &mut Vec<ThemeWarning>) -> Tokens {
    let mut tokens = *builtin_tokens(mode);
    let mut seeds = Seeds::default();
    let mut overrides: Vec<(String, Color)> = Vec::new();
    let mut flag = None;
    for (k, v) in t {
        let path = format!("{key}.{k}");
        match k.as_str() {
            "palette" => {
                let Some(p) = table(v, &path, warnings) else {
                    continue;
                };
                for (name, value) in p {
                    let path = format!("{path}.{name}");
                    if !Seeds::NAMES.contains(&name.as_str()) {
                        warnings.push(ThemeWarning::UnknownKey(path));
                    } else if let Some(c) = color(value, &path, warnings) {
                        seeds.set(name, c);
                    }
                }
            }
            "tokens" => {
                let Some(p) = table(v, &path, warnings) else {
                    continue;
                };
                for (name, value) in p {
                    let path = format!("{path}.{name}");
                    if name == "flag" {
                        flag = flag_colors(value, &path, warnings);
                    } else if !COLOR_TOKENS.contains(&name.as_str()) {
                        warnings.push(ThemeWarning::UnknownKey(path));
                    } else if let Some(c) = color(value, &path, warnings) {
                        overrides.push((name.clone(), c));
                    }
                }
            }
            _ => warnings.push(ThemeWarning::UnknownKey(path)),
        }
    }
    apply_seeds(&mut tokens, &seeds);
    for (name, c) in overrides {
        tokens.set_color(&name, c);
    }
    if let Some(f) = flag {
        tokens.flag = f;
    }
    tokens
}

fn flag_colors(
    v: &toml::Value,
    key: &str,
    warnings: &mut Vec<ThemeWarning>,
) -> Option<[Color; FLAG_COUNT]> {
    let Some(arr) = v.as_array() else {
        warnings.push(bad(key, v));
        return None;
    };
    if arr.len() != FLAG_COUNT {
        warnings.push(ThemeWarning::FlagLength {
            key: key.to_owned(),
            len: arr.len(),
        });
        return None;
    }
    let mut out = [Color::BLACK; FLAG_COUNT];
    for (i, (slot, value)) in out.iter_mut().zip(arr).enumerate() {
        *slot = color(value, &format!("{key}[{i}]"), warnings)?;
    }
    Some(out)
}

/// `#RGB`, `#RRGGBB` or `#RRGGBBAA` (case-insensitive).
pub fn parse_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 => {
            let nib = |i: usize| u8::from_str_radix(hex.get(i..=i)?, 16).ok().map(|n| n * 17);
            (nib(0)?, nib(1)?, nib(2)?, 255)
        }
        6 => (byte(0)?, byte(2)?, byte(4)?, 255),
        8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
        _ => return None,
    };
    Some(Color::from_rgba8(r, g, b, f32::from(a) / 255.0))
}

/// `#RRGGBB`, or `#RRGGBBAA` when not opaque.
pub fn color_hex(c: Color) -> String {
    let [r, g, b, a] = c.into_rgba8();
    if a == 255 {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

/// What each palette seed derives, for the template's comments.
const SEED_HELP: [(&str, &str); 6] = [
    (
        "accent",
        "primary buttons and their states, selection, focus ring, links, tints, unread badge",
    ),
    (
        "danger",
        "destructive buttons, error text and banners, @me mentions, busy dot, mention badge",
    ),
    ("success", "success text and banners, online dot"),
    (
        "warning",
        "warning icons and banners, @mention highlights, away dot",
    ),
    (
        "background",
        "room surface; sidebar, rail, raised/sunken, hover/pressed rows, borders, tooltip",
    ),
    (
        "text",
        "primary text; secondary, hint and disabled text are mixed toward the background",
    ),
];

/// A complete template theme (`rustket --dump-theme`): every token of the
/// built-in palettes, palette seeds, fonts and layout, all commented out.
pub fn dump_template() -> String {
    let mut out = String::new();
    out.push_str(
        "# Rustket theme template, generated by `rustket --dump-theme`.\n\
         # Save it as <config dir>/themes/<id>.toml (see docs/themes/README.md),\n\
         # then pick it in Settings → Appearance → Theme.\n\
         #\n\
         # Each variant ([light], [dark]) starts from the built-in Rustket colours,\n\
         # applies its [*.palette] seeds (each seed recomputes the colours derived\n\
         # from it), then its [*.tokens] overrides. A missing variant uses the\n\
         # built-in colours. Colours: \"#RGB\", \"#RRGGBB\" or \"#RRGGBBAA\".\n\
         # Seeds and tokens are listed commented out with the built-in values:\n\
         # uncomment a seed (and its [*.palette] header) to derive colours from\n\
         # it, uncomment a token to pin it (a pinned token wins over the seeds).\n\n",
    );
    out.push_str("[meta]\nname = \"My theme\"\n# name_ru = \"Моя тема\"\n# author = \"\"\n");
    out.push_str(&format!("version = {FORMAT_VERSION}\n\n"));
    out.push_str(
        "# Installed system font families, for the whole theme (applied at the\n\
         # next start). A missing family keeps the built-in Noto Sans.\n\
         # [fonts]\n# ui = \"Inter\"\n# mono = \"JetBrains Mono\"\n\n\
         # Corner-radius scale: 0.0 (square) .. 2.0, default 1.0.\n\
         # [layout]\n# radius = 1.0\n",
    );
    for (key, t) in [("light", &LIGHT), ("dark", &DARK)] {
        out.push_str(&format!(
            "\n# Seeds with the built-in values; uncomment to derive from them.\n# [{key}.palette]\n"
        ));
        for ((name, c), (_, help)) in Seeds::of(t).iter().zip(SEED_HELP) {
            out.push_str(&format!("# {name} = \"{}\"  # {help}\n", color_hex(*c)));
        }
        out.push_str(&format!(
            "\n# Tokens with the built-in values; uncomment to pin one.\n[{key}.tokens]\n"
        ));
        for (name, c) in t.colors() {
            out.push_str(&format!("# {name} = \"{}\"\n", color_hex(c)));
        }
        let flags: Vec<String> = t
            .flag
            .iter()
            .map(|c| format!("\"{}\"", color_hex(*c)))
            .collect();
        out.push_str(&format!("# flag = [{}]\n", flags.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_and_print() {
        assert_eq!(parse_color("#fff"), Some(Color::WHITE));
        assert_eq!(parse_color("#000000"), Some(Color::BLACK));
        assert_eq!(
            parse_color("#156ff5"),
            Some(Color::from_rgb8(0x15, 0x6F, 0xF5))
        );
        let c = parse_color("#11223380").expect("rgba");
        assert_eq!(c.into_rgba8(), [0x11, 0x22, 0x33, 0x80]);
        for bad in ["fff", "#ff", "#ggg", "#12345", "#1234567", "", "#+12345"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
        assert_eq!(color_hex(Color::from_rgb8(0x15, 0x6F, 0xF5)), "#156FF5");
        assert_eq!(color_hex(c), "#11223380");
    }

    /// The template parses to the built-ins as is, and with every token line
    /// uncommented it still does: the token list covers every field.
    #[test]
    fn template_round_trips_to_the_builtins() {
        let text = dump_template();
        let f = ThemeFile::parse(&text).expect("template parses");
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.light, Some(LIGHT));
        assert_eq!(f.dark, Some(DARK));
        assert_eq!(f.meta.version, Some(FORMAT_VERSION));
        assert_eq!(f.fonts, FontPrefs::default());
        assert_eq!(f.radius, None);
        for name in COLOR_TOKENS {
            assert!(text.contains(&format!("\n# {name} = \"")), "{name}");
        }
        // Uncomment the token lines (not the seeds): exactly the built-ins.
        let mut in_tokens = false;
        let pinned: String = text
            .lines()
            .map(|l| {
                // Commented headers (`# [dark.palette]`) end a section too.
                if l.starts_with('[') || l.starts_with("# [") {
                    in_tokens = l.ends_with(".tokens]");
                }
                let l = match l.strip_prefix("# ") {
                    Some(rest) if in_tokens && rest.contains(" = ") => rest,
                    _ => l,
                };
                format!("{l}\n")
            })
            .collect();
        assert_ne!(pinned, text);
        let f = ThemeFile::parse(&pinned).expect("pinned");
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.light, Some(LIGHT));
        assert_eq!(f.dark, Some(DARK));
    }

    /// Uncommenting a seed in the template changes the derived tokens.
    #[test]
    fn template_seed_takes_effect() {
        let seeded = dump_template()
            .replace("# [light.palette]", "[light.palette]")
            .replacen("# accent = \"#156FF5\"", "accent = \"#8A3FD1\"", 1);
        let f = ThemeFile::parse(&seeded).expect("seeded");
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        let light = f.light.expect("light");
        assert_eq!(light.primary, Color::from_rgb8(0x8A, 0x3F, 0xD1));
        assert_eq!(f.dark, Some(DARK));
    }

    #[test]
    fn tolerant_of_bad_values() {
        let f = ThemeFile::parse(
            r##"
            surprise = 1
            [meta]
            name = "T"
            version = 7
            colour = "x"
            [light]
            extra = true
            [light.palette]
            accent = "#8A3FD1"
            glow = "#fff"
            text = "nope"
            [light.tokens]
            link = "#123"
            not_a_token = "#fff"
            text_hint = 5
            flag = ["#f00", "#0f0"]
            "##,
        )
        .expect("parses");
        assert_eq!(f.meta.name, "T");
        assert!(f.dark.is_none());
        assert_eq!(f.tokens(Mode::Dark), DARK);
        let light = f.light.expect("light");
        assert_eq!(light.primary, Color::from_rgb8(0x8A, 0x3F, 0xD1));
        assert_eq!(light.link, Color::from_rgb8(0x11, 0x22, 0x33));
        assert_eq!(light.flag, LIGHT.flag);
        assert_eq!(light.text_primary, LIGHT.text_primary);
        let w: Vec<String> = f.warnings.iter().map(ToString::to_string).collect();
        for want in [
            "unknown key surprise",
            "unknown key meta.colour",
            "unknown key light.extra",
            "unknown key light.palette.glow",
            "unknown key light.tokens.not_a_token",
            "bad value for light.palette.text: \"nope\"",
            "bad value for light.tokens.text_hint: 5",
            "light.tokens.flag needs 6 colours, not 2",
            "format version 7 is newer than 1",
        ] {
            assert!(w.iter().any(|x| x == want), "{want}: {w:#?}");
        }
        assert_eq!(w.len(), 9, "{w:#?}");
    }

    #[test]
    fn hard_errors() {
        assert!(matches!(
            ThemeFile::parse("[meta"),
            Err(ThemeFileError::Syntax(_))
        ));
        // Line and column (in characters) of the error.
        let Err(ThemeFileError::Syntax(msg)) =
            ThemeFile::parse("[meta]\nname = \"Тёма\"\nversion = = 1\n")
        else {
            panic!("syntax error expected");
        };
        assert!(msg.starts_with("line 3, column "), "{msg}");
        assert!(!msg.contains('\n'), "{msg}");
        assert_eq!(
            ThemeFile::parse("[light.palette]\naccent = \"#fff\""),
            Err(ThemeFileError::MissingName)
        );
        assert_eq!(
            ThemeFile::parse("[meta]\nname = \"  \"\n[dark]\n"),
            Err(ThemeFileError::MissingName)
        );
        assert_eq!(
            ThemeFile::parse("[meta]\nname = \"x\"\nlight = 3"),
            Err(ThemeFileError::NoVariants)
        );
    }

    #[test]
    fn fonts_and_layout() {
        let f = ThemeFile::parse(
            "[meta]\nname = \"x\"\n[dark]\n[fonts]\nui = \"Inter\"\nmono = 3\nsize = 12\n[layout]\nradius = 0.5\n",
        )
        .expect("parses");
        assert_eq!(f.fonts.ui.as_deref(), Some("Inter"));
        assert_eq!(f.fonts.mono, None);
        assert_eq!(f.radius, Some(0.5));
        assert_eq!(f.warnings.len(), 2, "{:?}", f.warnings);
        assert!(f.has(Mode::Dark) && !f.has(Mode::Light));
        assert_eq!(f.dark, Some(DARK));

        let f = ThemeFile::parse("[meta]\nname = \"x\"\n[dark]\n[layout]\nradius = 5\n")
            .expect("parses");
        assert_eq!(f.radius, Some(2.0));
        assert_eq!(
            f.warnings,
            [ThemeWarning::RadiusClamped {
                value: 5.0,
                used: 2.0
            }]
        );
        let f = ThemeFile::parse("[meta]\nname = \"x\"\n[dark]\n[layout]\nradius = 0.3\n")
            .expect("parses");
        assert_eq!(f.radius, Some(0.3));
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        let f = ThemeFile::parse("[meta]\nname = \"x\"\n[dark]\n[layout]\nradius = -1.0\n")
            .expect("parses");
        assert_eq!(f.radius, Some(0.0));
        let f = ThemeFile::parse("[meta]\nname = \"x\"\n[dark]\n[layout]\nradius = \"big\"\n")
            .expect("parses");
        assert_eq!(f.radius, None);
        assert_eq!(f.warnings.len(), 1);
    }

    /// Overrides win over palette-derived colours; `mode` is not a token.
    #[test]
    fn overrides_apply_after_the_palette() {
        let f = ThemeFile::parse(
            "[meta]\nname = \"x\"\n[dark.palette]\naccent = \"#D08A2E\"\n[dark.tokens]\nprimary = \"#FF0000\"\nmode = \"#000\"\n",
        )
        .expect("parses");
        let d = f.dark.expect("dark");
        assert_eq!(d.primary, Color::from_rgb8(255, 0, 0));
        assert_ne!(d.primary_hover, DARK.primary_hover);
        assert_eq!(d.mode, Mode::Dark);
        assert_eq!(
            f.warnings,
            [ThemeWarning::UnknownKey("dark.tokens.mode".into())]
        );
    }

    /// `docs/themes/template.toml` is exactly the `--dump-theme` output.
    #[test]
    fn checked_in_template_is_current() {
        let file = include_str!("../../../docs/themes/template.toml");
        assert!(
            file == dump_template(),
            "docs/themes/template.toml is stale: regenerate it with `rustket --dump-theme`"
        );
    }

    /// The example themes load cleanly and meet WCAG AA in both variants.
    #[test]
    fn example_themes_are_clean() {
        for (name, src) in [
            ("forest", include_str!("../../../docs/themes/forest.toml")),
            (
                "graphite",
                include_str!("../../../docs/themes/graphite.toml"),
            ),
        ] {
            let f = ThemeFile::parse(src).expect(name);
            assert!(f.warnings.is_empty(), "{name}: {:?}", f.warnings);
            assert_eq!(f.fonts, FontPrefs::default(), "{name}");
            for mode in [Mode::Light, Mode::Dark] {
                assert!(f.has(mode), "{name} {mode:?}");
                let report = crate::theme::contrast_report(&f.tokens(mode));
                assert!(report.is_empty(), "{name} {mode:?}: {report:#?}");
            }
        }
    }
}
