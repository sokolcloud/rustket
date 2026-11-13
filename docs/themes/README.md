# Rustket themes

A theme changes Rustket's colours, and optionally its fonts and corner
radius. Each theme is one TOML file; pick it in **Settings → Appearance →
Theme**. The **Mode** control next to it (System / Light / Dark) still
chooses between the theme's light and dark variants: with *System* Rustket
follows the OS appearance.

## Where theme files live

`<config dir>/themes/<id>.toml`, where `<id>` (the file name without
`.toml`) is what the settings remember:

| Platform | Themes folder |
|----------|---------------|
| Linux    | `$XDG_CONFIG_HOME/rustket/themes` (usually `~/.config/rustket/themes`) |
| macOS    | `~/Library/Application Support/rustket/themes` |
| Windows  | `%APPDATA%\rustket\config\themes` |

**Open themes folder** in the settings creates the folder and opens it.
After editing a file press **Reload** (or reopen the settings) to see the
changes; the active theme is applied again.

## Getting started

```sh
rustket --dump-theme > ~/.config/rustket/themes/mine.toml
```

prints a complete template (also checked in as [`template.toml`](template.toml)):
the palette seeds and every colour token of the built-in light and dark
palettes with their built-in values, plus fonts and layout, all commented
out. Uncomment a seed to derive colours from it, uncomment a token to pin
it. Two finished examples:
[`forest.toml`](forest.toml) and [`graphite.toml`](graphite.toml).

## Format

```toml
[meta]
name = "Forest"          # required
name_ru = "Лес"          # optional: shown when the UI is in Russian
author = "..."           # optional
version = 1              # format version

[fonts]                  # optional, applies at the next start
ui = "Inter"
mono = "JetBrains Mono"

[layout]                 # optional
radius = 1.0             # corner-radius scale, 0.0 (square) .. 2.0

[light.palette]          # seed colours, all optional
accent = "#2F7D4F"

[light.tokens]           # exact colours, by token name
sidebar_selected_bg = "#E3E8D8"
flag = ["#D93025", "#C45A00", "#A07800", "#188038", "#1A73E8", "#9334E6"]

[dark.palette]
# ...
[dark.tokens]
# ...
```

Colours are `"#RGB"`, `"#RRGGBB"` or `"#RRGGBBAA"` (case-insensitive).

### Variants and fallback

A theme may define `[light]`, `[dark]` or both. A missing variant uses the
built-in Rustket colours for that mode (the settings say so under the
picker). A file with neither variant is not a theme.

Each variant is resolved in three steps:

1. start from the built-in Rustket tokens of that mode;
2. apply the `palette` seeds that are set: each seed recomputes the tokens
   derived from it;
3. apply the `tokens` overrides exactly as written.

### Palette seeds

| Seed | Derives |
|------|---------|
| `accent` | primary buttons with hover/pressed states and their text colour, selected rows and tints, focus ring, links, text selection, info banners, the quiet unread badge |
| `danger` | destructive buttons, error text and banners, mentions of you, the busy dot, the mention badge |
| `success` | success text and banners, the online dot |
| `warning` | warning icons and banners, @mention highlights, the away dot |
| `background` | the room surface; sidebar, rail, raised and sunken surfaces, hover/pressed/selected rows, borders, quote and code backgrounds, the tooltip (inverted) — mixed from background toward text in small fixed steps |
| `text` | primary text and code; secondary, hint and disabled text are mixed toward the background |

Changing `background` or `text` also recomputes the tints of the other
families, since they are mixed into the background. Seeds that are not set
keep their built-in value as input. Derived text-like colours (hints,
links, error and success text, banner and mention text) are nudged toward
black or white until they reach WCAG AA contrast, so a palette-only theme
usually needs no overrides. Seeds themselves are never changed.

### Token overrides

`[light.tokens]` / `[dark.tokens]` take any token by its name in the
template (`surface_sidebar`, `text_hint`, `badge_unread`, …) and `flag`, an
array of exactly six colours (red, orange, yellow, green, blue, purple
flag marks). Overrides win over everything derived from the palette.

### Fonts and corner radius

`[fonts] ui` and `mono` name installed system font families. They apply to
the whole theme (both variants) and take effect at the next start of
Rustket, because the toolkit fixes its default font when the window opens.
A family that is not installed keeps the bundled Noto Sans (or the system
monospace font) with a warning; a family without some weight uses the
nearest one it has (Medium falls back to Regular, SemiBold to Bold).
Themes cannot bundle font files.

`[layout] radius` scales every corner radius (pill shapes stay round);
values outside 0.0–2.0 are clamped with a warning.

Font sizes and spacing are deliberately not themeable: **Zoom** and the
room-list **Density** in the settings already cover them, and keeping them
fixed keeps every screen's layout tested.

## Warnings and contrast

Problems in a theme never block it. The settings list them under the
picker:

- unknown keys, invalid colours or values, a `flag` array of the wrong
  length (the rest of the file still loads);
- a newer format `version` than this Rustket knows (it still tries);
- missing fonts or font weights;
- colour pairs below WCAG AA contrast (4.5:1 for text, 3:1 for icons, the
  focus ring and flag marks), per variant, with the token names involved.

Only invalid TOML, a missing `[meta] name` or a file without `[light]` and
`[dark]` make a theme unusable; Rustket then uses the built-in theme and
shows the error.
