//! External colour themes: `<config dir>/themes/<id>.toml` files
//! ([`rustket_ui::theme_file`], `docs/themes/README.md`), the selection in
//! `UiPrefs::theme_name` and the active tokens of `rustket_ui::theme`.
//!
//! Colours and the corner radius apply at once; fonts only at startup
//! ([`init_fonts`]), because iced fixes its default font when the window opens.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rustket_core::config::{AppConfig, CONFIG_FILE};
use rustket_ui::Mode;
use rustket_ui::theme::{self, ContrastIssue, DARK, LIGHT};
use rustket_ui::theme_file::{FontPrefs, ThemeFile, ThemeFileError, ThemeWarning};

/// Subdirectory of the config directory with the theme files.
pub const THEMES_DIR: &str = "themes";
/// Largest theme file read (a full template is ~5 KiB).
const MAX_FILE: u64 = 256 * 1024;

pub fn themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(THEMES_DIR)
}

/// Why a theme cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// No `<id>.toml` (the selected theme was removed or renamed).
    NotFound,
    /// Unreadable.
    Read(String),
    /// Larger than the limit (size in bytes).
    TooLarge(u64),
    Parse(ThemeFileError),
}

/// One theme file.
#[derive(Debug, Clone)]
pub struct ThemeEntry {
    /// File stem.
    pub id: String,
    pub file: Result<Arc<ThemeFile>, LoadError>,
    /// Font problems (missing family or weights) on this machine.
    pub font_warnings: Vec<ThemeWarning>,
    /// Contrast issues of the light and dark variants it defines.
    pub light_issues: Vec<ContrastIssue>,
    pub dark_issues: Vec<ContrastIssue>,
}

impl ThemeEntry {
    fn load(id: String, path: &Path) -> Self {
        let file = read(path).and_then(|text| {
            ThemeFile::parse(&text)
                .map(Arc::new)
                .map_err(LoadError::Parse)
        });
        Self::from_result(id, file)
    }

    fn from_result(id: String, file: Result<Arc<ThemeFile>, LoadError>) -> Self {
        let issues = |mode: Mode| match &file {
            Ok(f) if f.has(mode) => theme::contrast_report(&f.tokens(mode)),
            _ => Vec::new(),
        };
        let (light_issues, dark_issues) = (issues(Mode::Light), issues(Mode::Dark));
        let font_warnings = match &file {
            Ok(f) => rustket_ui::fonts::resolve(&f.fonts).1,
            Err(_) => Vec::new(),
        };
        Self {
            id,
            file,
            font_warnings,
            light_issues,
            dark_issues,
        }
    }

    /// `name_ru` in the Russian UI when set, else `name`; the id for a
    /// broken file.
    pub fn display_name(&self) -> String {
        match &self.file {
            Ok(f) => match (&f.meta.name_ru, &*rust_i18n::locale() == "ru") {
                (Some(ru), true) if !ru.trim().is_empty() => ru.trim().to_owned(),
                _ => f.meta.name.clone(),
            },
            Err(_) => self.id.clone(),
        }
    }
}

fn read(path: &Path) -> Result<String, LoadError> {
    let meta = std::fs::metadata(path).map_err(|e| LoadError::Read(e.to_string()))?;
    if meta.len() > MAX_FILE {
        return Err(LoadError::TooLarge(meta.len()));
    }
    std::fs::read_to_string(path).map_err(|e| LoadError::Read(e.to_string()))
}

/// Every `*.toml` in `dir`, sorted by display name; a missing directory is
/// an empty list.
pub fn scan(dir: &Path) -> Vec<ThemeEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<ThemeEntry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("toml") || !path.is_file() {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_owned();
            Some(ThemeEntry::load(id, &path))
        })
        .collect();
    out.sort_by_cached_key(|e| (e.display_name().to_lowercase(), e.id.clone()));
    out
}

/// The theme list and the selection's state.
#[derive(Debug, Default)]
pub struct Themes {
    /// `<config dir>/themes` (`None`: no config directory, e.g. tests).
    pub dir: Option<PathBuf>,
    pub entries: Vec<ThemeEntry>,
}

impl Themes {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            entries: Vec::new(),
        }
    }

    /// Reads the theme files again.
    pub fn rescan(&mut self) {
        self.entries = self.dir.as_deref().map(scan).unwrap_or_default();
    }

    pub fn get(&self, id: &str) -> Option<&ThemeEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The usable theme file of `selected`, or why there is none.
    pub fn resolve(&self, selected: Option<&str>) -> Option<Result<&Arc<ThemeFile>, LoadError>> {
        let id = selected?;
        Some(match self.get(id) {
            None => Err(LoadError::NotFound),
            Some(e) => e.file.as_ref().map_err(Clone::clone),
        })
    }

    /// Activates `selected` (built-in when `None` or unusable).
    pub fn apply(&self, selected: Option<&str>) {
        match self.resolve(selected) {
            Some(Ok(f)) => theme::set_active(
                selected,
                &f.tokens(Mode::Light),
                &f.tokens(Mode::Dark),
                f.radius.unwrap_or(1.0),
            ),
            Some(Err(e)) => {
                tracing::warn!(theme = ?selected, error = ?e, "theme unusable; using the built-in one");
                theme::set_active(None, &LIGHT, &DARK, 1.0);
            }
            None => theme::set_active(None, &LIGHT, &DARK, 1.0),
        }
    }

    /// Whether `selected` uses other fonts than the ones of this session
    /// (they change at the next start).
    pub fn fonts_need_restart(&self, selected: Option<&str>) -> bool {
        let wanted = match self.resolve(selected) {
            Some(Ok(f)) => f.fonts.clone(),
            _ => FontPrefs::default(),
        };
        wanted != session_fonts()
    }
}

static SESSION_FONTS: OnceLock<FontPrefs> = OnceLock::new();

/// The `[fonts]` the session started with.
pub fn session_fonts() -> FontPrefs {
    SESSION_FONTS.get().cloned().unwrap_or_default()
}

/// Startup (before iced runs): the fonts of the theme selected in the
/// config, checked against the installed fonts.
pub fn init_fonts(config_dir: Option<&Path>) {
    let Some(dir) = config_dir else {
        return;
    };
    let selected = std::fs::read_to_string(dir.join(CONFIG_FILE))
        .ok()
        .and_then(|text| AppConfig::parse(&text).ok())
        .and_then(|c| c.ui.theme_name);
    let Some(id) = selected else {
        return;
    };
    let Ok(file) = read(&themes_dir(dir).join(format!("{id}.toml")))
        .and_then(|t| ThemeFile::parse(&t).map_err(LoadError::Parse))
    else {
        return;
    };
    let (set, warnings) = rustket_ui::fonts::resolve(&file.fonts);
    for w in &warnings {
        tracing::warn!(theme = %id, "{w}");
    }
    theme::init_fonts(set);
    let _ = SESSION_FONTS.set(file.fonts);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FOREST: &str = include_str!("../../../docs/themes/forest.toml");

    #[test]
    fn scan_reads_toml_files_only() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&dir.path().join("missing")).is_empty());
        std::fs::write(dir.path().join("forest.toml"), FOREST).unwrap();
        std::fs::write(
            dir.path().join("a-dark.toml"),
            "[meta]\nname = \"Zed\"\nname_ru = \"Альфа\"\n[dark.palette]\naccent = \"#7FA7E0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.toml"), "[meta").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        std::fs::create_dir(dir.path().join("sub.toml")).unwrap();
        let list = scan(dir.path());
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        // By display name: "broken" (the id), "Forest", "Zed".
        assert_eq!(ids, ["broken", "forest", "a-dark"]);
        assert!(matches!(list[0].file, Err(LoadError::Parse(_))));
        let dark_only = list[2].file.as_ref().unwrap();
        assert!(!dark_only.has(Mode::Light) && dark_only.has(Mode::Dark));
        assert!(list[1].light_issues.is_empty() && list[1].dark_issues.is_empty());

        let themes = Themes {
            dir: Some(dir.path().to_owned()),
            entries: list,
        };
        assert!(themes.resolve(None).is_none());
        assert!(matches!(
            themes.resolve(Some("gone")),
            Some(Err(LoadError::NotFound))
        ));
        assert!(matches!(
            themes.resolve(Some("broken")),
            Some(Err(LoadError::Parse(_)))
        ));
        assert!(themes.resolve(Some("forest")).unwrap().is_ok());
        assert!(!themes.fonts_need_restart(Some("forest")));
        assert!(!themes.fonts_need_restart(None));
    }
}
