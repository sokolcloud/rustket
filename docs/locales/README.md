# Rustket translations

Rustket ships with English, Russian, Spanish, Thai and Simplified Chinese.
More languages, and fixes to the bundled ones, are TOML files that you put
in the languages folder; pick the language in **Settings → Language**.

## Where locale files live

`<config dir>/locales/<code>.toml`. The file name without `.toml` is the
locale code, which the settings remember: `de.toml`, `pt_BR.toml`,
`zh_TW.toml`. Codes are letters, digits, `_` and `-` (up to 32 characters);
files with other names are ignored.

| Platform | Languages folder |
|----------|------------------|
| Linux    | `$XDG_CONFIG_HOME/rustket/locales` (usually `~/.config/rustket/locales`) |
| macOS    | `~/Library/Application Support/rustket/locales` |
| Windows  | `%APPDATA%\rustket\config\locales` |

**Open languages folder** in the settings creates the folder and opens it.
After editing a file press **Reload** (or reopen the settings): the files
are read again and the selected language is applied again.

## Getting started

```sh
rustket --dump-locale > ~/.config/rustket/locales/de.toml      # English template
rustket --dump-locale ru > ~/.config/rustket/locales/ru.toml   # the bundled Russian
```

`--dump-locale [CODE]` (default `en`) prints a complete template: `[meta]`
and every UI string in a stable order, grouped by component with
`# --- component ---` comments. For a code other than `en`, each string is
preceded by a `# en: "…"` comment with the English source, and strings that
are not translated yet are commented out. `CODE` can be a bundled locale or
an external one already in the languages folder (handy to pick up keys
added in a newer Rustket). An unknown code, or an external file that
cannot be read or parsed, prints an error and exits with status 2; the
file's warnings go to stderr. Strings that were skipped because their
placeholders differ from English are kept as commented-out lines so they
can be fixed.

To refresh an external locale, write to a different file and then move it:
redirecting into the file being read truncates it before Rustket reads it.

```sh
rustket --dump-locale de > de.new.toml && mv de.new.toml ~/.config/rustket/locales/de.toml
```

To start a new language, dump `en`, set `[meta] name` and `plural`, and
translate the values in place.

## Format

```toml
[meta]
name = "Deutsch"          # required: the language's own name
author = "..."            # optional
version = 1               # format version
plural = "one-other"      # one-other | east-slavic | none (default one-other)

[strings]
"settings.title" = "Einstellungen"
"settings.language" = "Sprache"
"teams.members_count_one" = "%{count} Mitglied"
"teams.members_count_many" = "%{count} Mitglieder"
```

- Keys are the bundled English keys (quote them: they contain dots;
  unquoted dotted keys also work).
- `%{name}` placeholders must be the same as in the English text (order may
  change). A string whose placeholders differ is skipped with a warning.
- Keys ending in `_fmt` are [chrono format strings](https://docs.rs/chrono/latest/chrono/format/strftime/index.html)
  for dates (`"app.date_short_fmt" = "%d. %b"`).
- An empty or missing string shows in English.

## Plural rules

Countable strings have three keys: `<key>_one`, `<key>_few` and
`<key>_many`. `[meta] plural` decides which one is used for a number `n`:

| Rule          | Languages (examples)            | Picks |
|---------------|---------------------------------|-------|
| `one-other`   | English, Spanish, German, …     | `_one` for 1, else `_many` (`_few` is unused) |
| `east-slavic` | Russian, Ukrainian, Belarusian  | `_one` for 1, 21, 31…; `_few` for 2–4, 22–24…; `_many` for the rest (0, 5–20, 25–30…) |
| `none`        | Thai, Chinese, Japanese, …      | always `_many` |

Translate all three keys anyway (`_few` can repeat `_many`): the file then
also works if the rule changes.

## Overriding a bundled language

A file named after a bundled locale (`en`, `ru`, `es`, `th`, `zh`) does not
add a language: it overrides that locale key by key, e.g. to fix a single
string, and everything it does not contain stays bundled. Its `plural`
replaces the bundled rule only when it is set. The language keeps its
bundled name in the picker.

## Fallback and coverage

Each string is looked up in the selected language's file, then in the
bundled translation of that code, then in English. The picker shows the
coverage of incomplete languages (translated keys / English keys), e.g.
`Deutsch (62%)`.

If the selected language is no longer available (its file was removed,
renamed or cannot be parsed), Rustket follows the system language and the
settings say why. **System** picks the language from `LC_ALL`,
`LC_MESSAGES`, `LANGUAGE` or `LANG`: an exact match of an available code
(`pt_BR`) first, then the language part (`de_AT` → `de`, `zh_TW` → `zh`),
else English.

## Errors and warnings

Like theme files, locale files load tolerantly:

- **Errors** (the file is not used): not valid TOML (with line and column),
  no `[meta] name`, unreadable or larger than 4 MiB.
- **Warnings** (the rest still loads): unknown keys, values that are not
  strings, placeholder mismatches, an unknown plural rule, a newer format
  version.

The settings list the problems of the selected language's file.

## Fonts

The bundled Noto Sans covers Latin, Cyrillic and Greek. Other scripts
(Thai, Chinese, Japanese, Korean, Arabic, …) are drawn with installed
system fonts through font fallback, so they need such fonts on the
machine. Windows and macOS ship them; on Linux install for example Noto
Sans Thai and Noto Sans CJK (`fonts-noto-cjk` / `google-noto-sans-cjk-fonts`).

Fallback chooses per character among the installed fonts with the closest
weight, so a stray font (for instance a kanji stroke-order font) can be
picked for some bold characters. A theme can name a font that covers the
script as its UI font (`[fonts] ui = "Noto Sans CJK SC"`, see
`docs/themes/README.md`).
