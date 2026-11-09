//! Start on login: on Linux `$XDG_CONFIG_HOME/autostart/rustket.desktop`
//! (XDG Autostart spec), on macOS a launchd agent
//! `~/Library/LaunchAgents/<bundle id>.plist`, on Windows the `Rustket` value
//! of `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. The file-based
//! locations take their base directory so tests use temporary directories.
//! Everything blocks (call it from `spawn_blocking`).

use std::io;
use std::path::{Path, PathBuf};

/// The XDG autostart file name.
pub const FILE: &str = "rustket.desktop";
/// The `Run` value name (Windows).
pub const RUN_VALUE: &str = "Rustket";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Where the launch-at-login entry lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Autostart {
    /// `<config home>/autostart/rustket.desktop`.
    #[cfg(all(unix, not(target_os = "macos")))]
    Xdg { config_home: PathBuf },
    /// `<home>/Library/LaunchAgents/<bundle id>.plist`.
    #[cfg(any(target_os = "macos", test))]
    LaunchAgent { home: PathBuf },
    /// The current user's `Run` registry key.
    #[cfg(windows)]
    RunKey,
}

impl Autostart {
    /// This platform's location (`None`: unknown home directory).
    pub fn system() -> Option<Self> {
        #[cfg(all(unix, not(target_os = "macos")))]
        return config_home().map(|config_home| Self::Xdg { config_home });
        #[cfg(target_os = "macos")]
        return directories::BaseDirs::new().map(|d| Self::LaunchAgent {
            home: d.home_dir().to_owned(),
        });
        #[cfg(windows)]
        return Some(Self::RunKey);
        #[cfg(not(any(unix, windows)))]
        return None;
    }

    /// The entry's file (`None`: in the registry).
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::Xdg { config_home } => Some(config_home.join("autostart").join(FILE)),
            #[cfg(any(target_os = "macos", test))]
            Self::LaunchAgent { home } => Some(
                home.join("Library")
                    .join("LaunchAgents")
                    .join(format!("{}.plist", crate::BUNDLE_ID)),
            ),
            #[cfg(windows)]
            Self::RunKey => None,
        }
    }

    /// Whether our entry exists and is not disabled.
    pub fn is_enabled(&self) -> bool {
        match self {
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::Xdg { .. } => self.read().is_some_and(|text| {
                !text
                    .lines()
                    .any(|l| l.trim().eq_ignore_ascii_case("hidden=true"))
            }),
            #[cfg(any(target_os = "macos", test))]
            Self::LaunchAgent { .. } => self.read().is_some_and(|text| {
                // A `Disabled` key set to true turns the agent off.
                let compact: String = text.split_whitespace().collect();
                !compact.contains("<key>Disabled</key><true/>")
            }),
            #[cfg(windows)]
            Self::RunKey => windows_registry::CURRENT_USER
                .open(RUN_KEY)
                .and_then(|k| k.get_string(RUN_VALUE))
                .is_ok_and(|v| !v.trim().is_empty()),
        }
    }

    /// Writes (`enabled`) or removes the entry starting `exe`.
    pub fn set_enabled(&self, enabled: bool, exe: &Path) -> io::Result<()> {
        match self {
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::Xdg { .. } => self.write_file(enabled, || desktop_entry(exe)),
            #[cfg(any(target_os = "macos", test))]
            Self::LaunchAgent { .. } => self.write_file(enabled, || launch_agent(exe)),
            #[cfg(windows)]
            Self::RunKey => {
                let key = windows_registry::CURRENT_USER
                    .create(RUN_KEY)
                    .map_err(io::Error::other)?;
                if enabled {
                    key.set_string(RUN_VALUE, run_value(exe))
                        .map_err(io::Error::other)
                } else if key.get_string(RUN_VALUE).is_ok() {
                    key.remove_value(RUN_VALUE).map_err(io::Error::other)
                } else {
                    Ok(())
                }
            }
        }
    }

    #[cfg(any(unix, test))]
    fn read(&self) -> Option<String> {
        std::fs::read_to_string(self.path()?).ok()
    }

    #[cfg(any(unix, test))]
    fn write_file(&self, enabled: bool, text: impl FnOnce() -> String) -> io::Result<()> {
        let Some(p) = self.path() else {
            return Ok(());
        };
        if enabled {
            let dir = p.parent().ok_or_else(|| io::Error::other("no parent"))?;
            std::fs::create_dir_all(dir)?;
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            let tmp = dir.join(format!(
                ".{}.tmp{}",
                name.unwrap_or_default(),
                std::process::id()
            ));
            std::fs::write(&tmp, text())?;
            std::fs::rename(tmp, p)
        } else {
            match std::fs::remove_file(p) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}

/// `$XDG_CONFIG_HOME` (or `~/.config`).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.config_dir().to_owned()))
}

/// The XDG autostart entry starting `exe` (the app reads its own "start
/// minimized" preference).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Rustket\nComment=Rocket.Chat client\nExec={}\nIcon=rustket\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        crate::url_handler::quote_exec(&exe.to_string_lossy())
    )
}

#[cfg(any(target_os = "macos", test))]
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The launchd agent starting `exe` at login.
#[cfg(any(target_os = "macos", test))]
pub fn launch_agent(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        escape_xml(crate::BUNDLE_ID),
        escape_xml(&exe.to_string_lossy())
    )
}

/// The `Run` value starting `exe` (quoted: paths contain spaces).
#[cfg(any(windows, test))]
pub fn run_value(exe: &Path) -> String {
    format!("\"{}\"", exe.to_string_lossy().replace('"', ""))
}

/// The executable an autostart entry should run (`$APPIMAGE` when running
/// from one; on macOS the binary inside the app bundle when running from it).
pub fn executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let appimage = std::env::var_os("APPIMAGE");
        let appdir = std::env::var_os("APPDIR");
        crate::url_handler::handler_executable(&current, appimage.as_deref(), appdir.as_deref())
            .map(|(p, _)| p)
            .map_err(io::Error::other)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    Ok(current)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn enable_and_disable_in_a_temporary_config_home() {
        let d = tempfile::tempdir().unwrap();
        let a = Autostart::Xdg {
            config_home: d.path().to_owned(),
        };
        let path = a.path().unwrap();
        assert_eq!(path, d.path().join("autostart/rustket.desktop"));
        assert!(!a.is_enabled());
        a.set_enabled(true, Path::new("/opt/my apps/rustket"))
            .unwrap();
        assert!(a.is_enabled());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=\"/opt/my apps/rustket\"\n"), "{text}");
        assert!(text.starts_with("[Desktop Entry]\n"));
        // Idempotent.
        a.set_enabled(true, Path::new("/usr/bin/rustket")).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=/usr/bin/rustket\n")
        );
        a.set_enabled(false, Path::new("/usr/bin/rustket")).unwrap();
        assert!(!a.is_enabled());
        assert!(!path.exists());
        // Removing twice is fine.
        a.set_enabled(false, Path::new("/usr/bin/rustket")).unwrap();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn hidden_entry_counts_as_disabled() {
        let d = tempfile::tempdir().unwrap();
        let a = Autostart::Xdg {
            config_home: d.path().to_owned(),
        };
        std::fs::create_dir_all(d.path().join("autostart")).unwrap();
        std::fs::write(a.path().unwrap(), "[Desktop Entry]\nHidden=true\n").unwrap();
        assert!(!a.is_enabled());
    }

    #[test]
    fn launch_agent_in_a_temporary_home() {
        let d = tempfile::tempdir().unwrap();
        let a = Autostart::LaunchAgent {
            home: d.path().to_owned(),
        };
        let path = a.path().unwrap();
        assert_eq!(
            path,
            d.path()
                .join("Library/LaunchAgents/io.github.sokolcloud.rustket.plist")
        );
        assert!(!a.is_enabled());
        let exe = Path::new("/Applications/R & D/Rustket.app/Contents/MacOS/rustket");
        a.set_enabled(true, exe).unwrap();
        assert!(a.is_enabled());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("<key>Label</key>\n\t<string>io.github.sokolcloud.rustket</string>"));
        assert!(text.contains(
            "<string>/Applications/R &amp; D/Rustket.app/Contents/MacOS/rustket</string>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        std::fs::write(
            &path,
            text.replace("<dict>", "<dict>\n<key>Disabled</key>\n<true/>"),
        )
        .unwrap();
        assert!(!a.is_enabled());
        a.set_enabled(false, exe).unwrap();
        assert!(!path.exists() && !a.is_enabled());
        a.set_enabled(false, exe).unwrap();
    }

    #[test]
    fn run_value_is_quoted() {
        assert_eq!(
            run_value(Path::new(r"C:\Program Files\Rustket\rustket.exe")),
            r#""C:\Program Files\Rustket\rustket.exe""#
        );
    }
}
