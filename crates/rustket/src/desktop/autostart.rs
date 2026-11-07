//! Start on login: `$XDG_CONFIG_HOME/autostart/rustket.desktop` (XDG
//! Autostart spec). All functions take the config home so tests use
//! temporary directories; they block (call them from `spawn_blocking`).

use std::io;
use std::path::{Path, PathBuf};

pub const FILE: &str = "rustket.desktop";

/// `$XDG_CONFIG_HOME` (or `~/.config`).
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.config_dir().to_owned()))
}

pub fn path(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(FILE)
}

/// The autostart entry starting `exe` minimized-aware (the app reads its own
/// "start minimized" preference).
pub fn entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Rustket\nComment=Rocket.Chat client\nExec={}\nIcon=rustket\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        crate::url_handler::quote_exec(&exe.to_string_lossy())
    )
}

/// Whether our entry exists and is not disabled (`Hidden=true`).
pub fn is_enabled(config_home: &Path) -> bool {
    match std::fs::read_to_string(path(config_home)) {
        Ok(text) => !text
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("hidden=true")),
        Err(_) => false,
    }
}

/// Writes (`enabled`) or removes the entry.
pub fn set_enabled(config_home: &Path, enabled: bool, exe: &Path) -> io::Result<()> {
    let p = path(config_home);
    if enabled {
        let dir = config_home.join("autostart");
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{FILE}.tmp{}", std::process::id()));
        std::fs::write(&tmp, entry(exe))?;
        std::fs::rename(tmp, p)
    } else {
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The executable an autostart entry should run (`$APPIMAGE` when running
/// from one).
pub fn executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let appimage = std::env::var_os("APPIMAGE");
    let appdir = std::env::var_os("APPDIR");
    crate::url_handler::handler_executable(&current, appimage.as_deref(), appdir.as_deref())
        .map(|(p, _)| p)
        .map_err(io::Error::other)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn enable_and_disable_in_a_temporary_config_home() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        assert!(!is_enabled(home));
        set_enabled(home, true, Path::new("/opt/my apps/rustket")).unwrap();
        assert!(is_enabled(home));
        let text = std::fs::read_to_string(path(home)).unwrap();
        assert!(text.contains("Exec=\"/opt/my apps/rustket\"\n"), "{text}");
        assert!(text.starts_with("[Desktop Entry]\n"));
        // Idempotent.
        set_enabled(home, true, Path::new("/usr/bin/rustket")).unwrap();
        assert!(
            std::fs::read_to_string(path(home))
                .unwrap()
                .contains("Exec=/usr/bin/rustket\n")
        );
        set_enabled(home, false, Path::new("/usr/bin/rustket")).unwrap();
        assert!(!is_enabled(home));
        assert!(!path(home).exists());
        // Removing twice is fine.
        set_enabled(home, false, Path::new("/usr/bin/rustket")).unwrap();
    }

    #[test]
    fn hidden_entry_counts_as_disabled() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("autostart")).unwrap();
        std::fs::write(path(d.path()), "[Desktop Entry]\nHidden=true\n").unwrap();
        assert!(!is_enabled(d.path()));
    }
}
