//! Start on login: on Linux `$XDG_CONFIG_HOME/autostart/rustket.desktop`
//! (XDG Autostart spec), on macOS a launchd agent
//! `~/Library/LaunchAgents/<bundle id>.plist` (the Mac App Store flavor:
//! `SMAppService.mainApp` on macOS 13+, nothing before, because the sandbox
//! redirects the agent into its container), on Windows the `Rustket` value
//! of `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. The file-based
//! locations take their base directory so tests use temporary directories.
//! Everything blocks (call it from `spawn_blocking`).

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod macos;

/// The XDG autostart file name.
pub const FILE: &str = "rustket.desktop";
/// The `Run` value name (Windows).
pub const RUN_VALUE: &str = "Rustket";
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager and Settings → Apps → Startup mark a `Run` entry
/// disabled, leaving the `Run` value in place.
#[cfg(windows)]
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

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
    /// The app itself as a login item (`SMAppService.mainApp`, macOS 13+).
    #[cfg(target_os = "macos")]
    LoginItem,
}

/// Whether the app starts at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Off,
    On,
    /// Registered, but the user has to allow it in System Settings → Login
    /// Items first.
    NeedsApproval,
}

impl State {
    /// The switch shows "on" (the user asked for it).
    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// The state of an `SMAppServiceStatus` value: `notRegistered` (0),
/// `enabled` (1), `requiresApproval` (2), `notFound` (3).
#[cfg(any(target_os = "macos", test))]
pub fn login_item_state(status: isize) -> State {
    match status {
        1 => State::On,
        2 => State::NeedsApproval,
        _ => State::Off,
    }
}

impl Autostart {
    /// This platform's location (`None`: unknown home directory; in the Mac
    /// App Store flavor, macOS before 13).
    pub fn system() -> Option<Self> {
        #[cfg(all(unix, not(target_os = "macos")))]
        return config_home().map(|config_home| Self::Xdg { config_home });
        #[cfg(target_os = "macos")]
        if cfg!(feature = "mas") {
            return macos::supported().then_some(Self::LoginItem);
        }
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
            #[cfg(target_os = "macos")]
            Self::LoginItem => None,
        }
    }

    /// Whether the app starts at login, including a pending approval.
    pub fn state(&self) -> State {
        match self {
            #[cfg(target_os = "macos")]
            Self::LoginItem => macos::state(),
            _ if self.is_enabled() => State::On,
            _ => State::Off,
        }
    }

    /// Whether our entry exists and is not disabled.
    pub fn is_enabled(&self) -> bool {
        match self {
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::Xdg { .. } => self.read().is_some_and(|text| {
                // GNOME's Startup Applications turns an entry off with
                // `X-GNOME-Autostart-enabled=false`.
                !text.lines().any(|l| {
                    let l = l.trim();
                    l.eq_ignore_ascii_case("hidden=true")
                        || l.eq_ignore_ascii_case("x-gnome-autostart-enabled=false")
                })
            }),
            #[cfg(any(target_os = "macos", test))]
            Self::LaunchAgent { .. } => self.read().is_some_and(|text| {
                // A `Disabled` key set to true turns the agent off.
                let compact: String = text.split_whitespace().collect();
                !compact.contains("<key>Disabled</key><true/>")
            }),
            #[cfg(windows)]
            Self::RunKey => {
                windows_registry::CURRENT_USER
                    .open(RUN_KEY)
                    .and_then(|k| k.get_string(RUN_VALUE))
                    .is_ok_and(|v| !v.trim().is_empty())
                    && !windows_registry::CURRENT_USER
                        .open(APPROVED_KEY)
                        .and_then(|k| k.get_value(RUN_VALUE))
                        .is_ok_and(|v| approved_disabled(&v))
            }
            #[cfg(target_os = "macos")]
            Self::LoginItem => macos::state().is_on(),
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
                        .map_err(io::Error::other)?;
                    // Clear a "disabled" mark left by Task Manager; a missing
                    // mark counts as enabled, so the key is never created.
                    if let Ok(approved) = windows_registry::CURRENT_USER
                        .options()
                        .read()
                        .write()
                        .open(APPROVED_KEY)
                        && approved.get_value(RUN_VALUE).is_ok()
                    {
                        approved.remove_value(RUN_VALUE).map_err(io::Error::other)?;
                    }
                    Ok(())
                } else if key.get_string(RUN_VALUE).is_ok() {
                    key.remove_value(RUN_VALUE).map_err(io::Error::other)
                } else {
                    Ok(())
                }
            }
            #[cfg(target_os = "macos")]
            Self::LoginItem => macos::set_enabled(enabled),
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

/// Whether a `StartupApproved\Run` value marks the entry disabled: an odd
/// first byte (Task Manager writes 02 or 06 for enabled, 03 for disabled).
#[cfg(any(windows, test))]
fn approved_disabled(b: &[u8]) -> bool {
    b.first().is_some_and(|x| x & 1 == 1)
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

    /// GNOME's Startup Applications switch; turning it on again repairs it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn gnome_disabled_entry_counts_as_disabled() {
        let d = tempfile::tempdir().unwrap();
        let a = Autostart::Xdg {
            config_home: d.path().to_owned(),
        };
        std::fs::create_dir_all(d.path().join("autostart")).unwrap();
        std::fs::write(
            a.path().unwrap(),
            "[Desktop Entry]\nExec=/usr/bin/rustket\n X-GNOME-Autostart-enabled=False\n",
        )
        .unwrap();
        assert!(!a.is_enabled());
        a.set_enabled(true, Path::new("/usr/bin/rustket")).unwrap();
        assert!(a.is_enabled());
    }

    #[test]
    fn startup_approved_flag() {
        assert!(approved_disabled(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!approved_disabled(&[2, 0, 0]));
        assert!(!approved_disabled(&[6, 0]));
        assert!(!approved_disabled(&[]));
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
    fn login_item_status_mapping() {
        assert_eq!(login_item_state(0), State::Off);
        assert_eq!(login_item_state(1), State::On);
        assert_eq!(login_item_state(2), State::NeedsApproval);
        assert_eq!(login_item_state(3), State::Off);
        assert_eq!(login_item_state(-1), State::Off);
        assert!(State::NeedsApproval.is_on() && !State::Off.is_on());
    }

    #[test]
    fn run_value_is_quoted() {
        assert_eq!(
            run_value(Path::new(r"C:\Program Files\Rustket\rustket.exe")),
            r#""C:\Program Files\Rustket\rustket.exe""#
        );
    }
}
