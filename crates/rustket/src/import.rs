//! Settings → Import…: theme and locale files picked in a file dialog are
//! checked, then copied into the themes or locales folder (which the Mac App
//! Store build keeps inside its hidden container). Nothing is copied unless
//! every picked file is usable.

use std::path::{Path, PathBuf};

use rust_i18n::t;
use rustket_core::config::Language;

/// What is imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Themes,
    Locales,
}

/// Asks for `.toml` files (`None`: cancelled).
pub async fn pick() -> Option<Vec<PathBuf>> {
    rfd::AsyncFileDialog::new()
        .add_filter("TOML", &["toml"])
        .pick_files()
        .await
        .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
}

/// Checks every file of `files`, then copies them into `dir` as
/// `<stem>.toml` (replacing a file of that name). Returns the names written;
/// the error is shown to the user. Blocking.
pub fn import(dir: &Path, kind: Kind, files: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut checked = Vec::with_capacity(files.len());
    for path in files {
        checked.push((path, target_name(path, kind)?));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut written = Vec::with_capacity(checked.len());
    for (path, name) in checked {
        let target = dir.join(&name);
        let tmp = dir.join(format!(".{name}.import{}", std::process::id()));
        let copied = std::fs::copy(path, &tmp).and_then(|_| std::fs::rename(&tmp, &target));
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{name}: {e}"));
        }
        written.push(name);
    }
    Ok(written)
}

/// The file name `path` gets in the folder, once its name and contents are
/// checked.
fn target_name(path: &Path, kind: Kind) -> Result<String, String> {
    let shown = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let toml = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| toml && !s.is_empty() && !s.starts_with('.'))
        .ok_or_else(|| t!("settings.import_not_toml", file = shown).into_owned())?;
    let name = format!("{stem}.toml");
    match kind {
        Kind::Themes => crate::themes::check_file(path)
            .map_err(|e| format!("{shown}: {}", crate::themes::load_error_text(&e, &name)))?,
        Kind::Locales => {
            if !Language::valid_code(stem) || stem == Language::SYSTEM {
                return Err(t!("settings.import_bad_locale_name", file = shown).into_owned());
            }
            crate::locales::check_file(path)
                .map_err(|e| crate::locales::load_error_text(&e, &shown))?;
        }
    }
    Ok(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const THEME: &str = include_str!("../../../docs/themes/forest.toml");

    #[test]
    fn themes_are_checked_then_copied() {
        let src = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        let good = src.path().join("Forest.TOML");
        std::fs::write(&good, THEME).unwrap();
        let broken = src.path().join("broken.toml");
        std::fs::write(&broken, "name = [").unwrap();
        let other = src.path().join("notes.txt");
        std::fs::write(&other, THEME).unwrap();

        // One bad file: nothing is copied.
        assert!(import(&themes, Kind::Themes, &[good.clone(), broken]).is_err());
        assert!(!themes.exists());
        assert!(import(&themes, Kind::Themes, &[other]).is_err());

        let names = import(&themes, Kind::Themes, std::slice::from_ref(&good)).unwrap();
        assert_eq!(names, ["Forest.toml"]);
        assert_eq!(
            std::fs::read_to_string(themes.join("Forest.toml")).unwrap(),
            THEME
        );
        // Again: replaced, no temporary file left.
        import(&themes, Kind::Themes, &[good]).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&themes).unwrap().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(crate::themes::scan(&themes).len(), 1);
    }

    #[test]
    fn locales_need_a_locale_code_name() {
        let src = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let text = "[meta]\nname = \"Deutsch\"\nversion = 1\n";
        let good = src.path().join("de.toml");
        std::fs::write(&good, text).unwrap();
        let bad_name = src.path().join("my german.toml");
        std::fs::write(&bad_name, text).unwrap();
        let system = src.path().join("system.toml");
        std::fs::write(&system, text).unwrap();
        assert!(import(dir.path(), Kind::Locales, &[bad_name]).is_err());
        assert!(import(dir.path(), Kind::Locales, &[system]).is_err());
        let nameless = src.path().join("fr.toml");
        std::fs::write(&nameless, "[meta]\n").unwrap();
        assert!(import(dir.path(), Kind::Locales, &[nameless]).is_err());
        assert_eq!(
            import(dir.path(), Kind::Locales, &[good]).unwrap(),
            ["de.toml"]
        );
        let scanned = crate::locales::scan(dir.path());
        assert_eq!(scanned.len(), 1);
        assert!(scanned[0].file.is_ok());
    }
}
