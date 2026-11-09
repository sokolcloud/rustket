//! The `rocketchat://` link handler: which program the OS starts for these
//! links and making it Rustket.
//!
//! - Linux and other freedesktop systems ([`xdg`]): a desktop file and
//!   `xdg-mime default`.
//! - Windows (`windows`): per-user keys under `HKCU\Software\Classes\rocketchat`.
//! - macOS: the app bundle's `Info.plist` declares the scheme and LaunchServices
//!   routes links to it; there is nothing to register from the app.
//!
//! The executable checks ([`handler_executable`]) are shared with autostart.

use std::path::{Path, PathBuf};

#[cfg_attr(not(windows), allow(dead_code))]
pub mod windows;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod xdg;
#[cfg(all(unix, not(target_os = "macos")))]
pub use xdg::*;

pub const DESKTOP_FILE: &str = "rustket-url-handler.desktop";
/// The desktop file installed by the packages: `<crate::APP_ID>.desktop`.
pub const PACKAGED_DESKTOP_FILE: &str = "rustket.desktop";
pub const MIME: &str = "x-scheme-handler/rocketchat";

/// Whether links are routed by the app bundle (macOS) instead of a
/// registration the app performs.
pub const MANAGED_BY_BUNDLE: bool = cfg!(target_os = "macos");

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("xdg-mime failed: {0}")]
    XdgMime(String),
    #[error("{0}")]
    UnsafeExecutable(String),
    #[error("cannot write the registry: {0}")]
    Registry(std::io::Error),
}

const DELETED_SUFFIX: &str = " (deleted)";

/// `/proc/self/exe` of a process whose binary was replaced while it runs (a
/// package upgrade) reads `<path> (deleted)`; the new file at `<path>` is the
/// one to register.
pub fn undeleted_exe(exe: &Path) -> PathBuf {
    exe.to_str()
        .and_then(|s| s.strip_suffix(DELETED_SUFFIX))
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| exe.to_owned())
}

/// The executable to register and warnings to show before registering.
/// `$APPIMAGE` is used only when this process runs from that AppImage (the
/// current executable lies inside `$APPDIR`, the image's mount) and the image
/// is a regular file; otherwise the current executable (see [`undeleted_exe`]).
pub fn handler_executable(
    current_exe: &Path,
    appimage: Option<&std::ffi::OsStr>,
    appdir: Option<&std::ffi::OsStr>,
) -> Result<(PathBuf, Vec<String>), RegisterError> {
    let from_appimage = match (appimage, appdir) {
        (Some(img), Some(dir)) if !img.is_empty() && !dir.is_empty() => {
            let img = PathBuf::from(img);
            (current_exe.starts_with(dir) && img.is_file()).then_some(img)
        }
        _ => None,
    };
    let exe = from_appimage.unwrap_or_else(|| undeleted_exe(current_exe));
    let warnings = check_executable(&exe)?;
    Ok((exe, warnings))
}

/// Who can modify a file or directory, from its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Safe,
    /// Writable by a group that has other members: allowed after a warning.
    SharedGroup,
    /// Writable by everyone, or owned by another user: refused.
    Unsafe,
}

/// `private_group`: the file's group is the user's own primary group with no
/// other members (user-private groups, umask 002).
pub fn verdict(mode: u32, owner: u32, me: u32, private_group: bool) -> Verdict {
    if owner != 0 && owner != me {
        return Verdict::Unsafe;
    }
    if mode & 0o002 != 0 {
        return Verdict::Unsafe;
    }
    if mode & 0o020 != 0 && !private_group {
        return Verdict::SharedGroup;
    }
    Verdict::Safe
}

/// The user's primary (effective) gid.
#[cfg(unix)]
fn primary_gid() -> Option<u32> {
    Some(rustix::process::getegid().as_raw())
}

/// Whether `gid` is the user's primary group without other members.
#[cfg(unix)]
fn is_private_group(gid: u32, uid: u32) -> bool {
    if primary_gid() != Some(gid) {
        return false;
    }
    if gid == uid {
        return true;
    }
    let me = std::env::var("USER").unwrap_or_default();
    std::fs::read_to_string("/etc/group").is_ok_and(|text| {
        text.lines().any(|l| {
            let f: Vec<&str> = l.split(':').collect();
            f.len() >= 4
                && f[2].parse::<u32>().ok() == Some(gid)
                && f[3].split(',').filter(|m| !m.is_empty()).all(|m| m == me)
        })
    })
}

/// Checks the executable and every ancestor directory: refuses temporary
/// locations and anything another user (or everyone) can modify; a shared
/// group with write access yields a warning. On Windows only the path is
/// checked; ACLs are not inspected.
pub fn check_executable(exe: &Path) -> Result<Vec<String>, RegisterError> {
    let refuse = |why: String| {
        Err(RegisterError::UnsafeExecutable(format!(
            "{}: {why}",
            exe.display()
        )))
    };
    if !exe.is_absolute() {
        return refuse("not an absolute path".into());
    }
    if exe.to_string_lossy().chars().any(char::is_control) {
        return refuse("control characters in the path".into());
    }
    let tmp = std::env::temp_dir();
    if exe.starts_with(&tmp)
        || exe.starts_with("/tmp")
        || exe.starts_with("/var/tmp")
        || exe.starts_with("/dev/shm")
    {
        return refuse("temporary location".into());
    }
    #[cfg(not(unix))]
    return Ok(Vec::new());
    #[cfg(unix)]
    unix_permissions(exe)
}

#[cfg(unix)]
fn unix_permissions(exe: &Path) -> Result<Vec<String>, RegisterError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let refuse = |why: String| {
        Err(RegisterError::UnsafeExecutable(format!(
            "{}: {why}",
            exe.display()
        )))
    };
    let uid = crate::single_instance::unix::current_uid().unwrap_or(u32::MAX);
    let mut warnings = Vec::new();
    let mut path = Some(exe);
    while let Some(p) = path {
        let Ok(m) = std::fs::metadata(p) else {
            return refuse(format!("{} is not readable", p.display()));
        };
        let mode = m.permissions().mode();
        match verdict(mode, m.uid(), uid, is_private_group(m.gid(), uid)) {
            Verdict::Safe => {}
            Verdict::SharedGroup => warnings.push(p.display().to_string()),
            Verdict::Unsafe => {
                return refuse(format!("{} can be modified by other users", p.display()));
            }
        }
        path = p.parent();
    }
    Ok(warnings)
}

/// Quotes an `Exec` argument per the Desktop Entry spec: field-code `%`
/// doubling, the quoting rule, then the string-level backslash escape (which
/// the spec applies before unquoting, so a literal backslash becomes four).
pub fn quote_exec(arg: &str) -> String {
    quote_arg(arg).replace('\\', "\\\\")
}

fn quote_arg(arg: &str) -> String {
    let needs = arg.chars().any(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '\\'
                    | '>'
                    | '<'
                    | '~'
                    | '|'
                    | '&'
                    | ';'
                    | '$'
                    | '*'
                    | '?'
                    | '#'
                    | '('
                    | ')'
                    | '`'
            )
    });
    let escaped = arg.replace('%', "%%");
    if !needs {
        return escaped;
    }
    let mut s = String::from("\"");
    for c in escaped.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

/// What the settings and the register dialog show.
#[derive(Debug, Clone)]
pub struct Query {
    /// The current handler: a desktop file id (freedesktop) or the program
    /// the registry starts (Windows).
    pub current: Option<String>,
    /// This executable handles the links.
    pub registered: bool,
    /// Warnings about the executable to register, or why it cannot be.
    pub check: Result<Vec<String>, String>,
}

/// The executable `rocketchat://` links would start (blocking).
pub fn current_executable() -> Result<(PathBuf, Vec<String>), String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let appimage = std::env::var_os("APPIMAGE");
    let appdir = std::env::var_os("APPDIR");
    handler_executable(&current, appimage.as_deref(), appdir.as_deref()).map_err(|e| e.to_string())
}

/// The current registration (blocking).
#[cfg(not(target_os = "macos"))]
pub fn query() -> Query {
    let exe = current_executable();
    let path = exe.as_ref().ok().map(|(p, _)| p.as_path());
    #[cfg(windows)]
    let (current, registered) = windows::query(path);
    #[cfg(not(windows))]
    let (current, registered) = match XdgEnv::from_env() {
        Some(env) => {
            let current = current_handler(&env);
            let registered = registered_for(&env, current.as_deref(), path);
            (current, registered)
        }
        None => (None, false),
    };
    Query {
        current,
        registered,
        check: exe.map(|(_, w)| w),
    }
}

/// The bundle declares the scheme: nothing to check or register.
#[cfg(target_os = "macos")]
pub fn query() -> Query {
    Query {
        current: None,
        registered: true,
        check: Ok(Vec::new()),
    }
}

/// Makes this executable the handler (blocking). Returns where it was
/// registered (a desktop file, a registry key).
pub fn register_current() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    return Err(rust_i18n::t!("handler.bundle").into_owned());
    #[cfg(not(target_os = "macos"))]
    {
        let (exe, _) = current_executable()?;
        #[cfg(windows)]
        return windows::register(&exe)
            .map(|()| format!(r"HKCU\{}", windows::KEY))
            .map_err(|e| e.to_string());
        #[cfg(not(windows))]
        {
            let env = XdgEnv::from_env().ok_or("no data directory")?;
            register(&env, &exe)
                .map(|p| p.display().to_string())
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(windows)]
pub use windows::is_ours;

/// Whether a current handler (from [`Query`]) is Rustket's.
#[cfg(target_os = "macos")]
pub fn is_ours(_current: &str) -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn deleted_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rustket");
        std::fs::write(&exe, "").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", exe.display()));
        assert_eq!(undeleted_exe(&deleted), exe);
        assert_eq!(undeleted_exe(&exe), exe);
        // Nothing at the stripped path: unchanged (and later refused).
        let gone = PathBuf::from(format!("{} (deleted)", dir.path().join("gone").display()));
        assert_eq!(undeleted_exe(&gone), gone);
    }

    /// Review #12/#13 and re-review #3: what may be registered.
    #[cfg(unix)]
    #[test]
    fn executable_checks() {
        use std::ffi::OsStr;
        let sys = Path::new("/usr/bin/env");
        assert_eq!(handler_executable(sys, None, None).unwrap().0, sys);
        // $APPIMAGE only when running from inside $APPDIR.
        assert_eq!(
            handler_executable(
                Path::new("/tmp/.mount_x/usr/bin/rustket"),
                Some(OsStr::new("/usr/bin/env")),
                Some(OsStr::new("/tmp/.mount_x")),
            )
            .unwrap()
            .0,
            sys
        );
        assert_eq!(
            handler_executable(
                sys,
                Some(OsStr::new("/usr/bin/true")),
                Some(OsStr::new("/tmp/.mount_x")),
            )
            .unwrap()
            .0,
            sys,
            "foreign APPIMAGE ignored"
        );
        assert!(handler_executable(Path::new("/tmp/.mount_x/AppRun"), None, None).is_err());
        assert!(check_executable(Path::new("relative/rustket")).is_err());
        assert!(check_executable(Path::new("/usr/bin/a\nb")).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(check_executable(&dir.path().join("rustket")).is_err());
        // Permission rules.
        let me = 1000;
        assert_eq!(verdict(0o755, me, me, false), Verdict::Safe);
        assert_eq!(verdict(0o755, 0, me, false), Verdict::Safe);
        assert_eq!(
            verdict(0o775, me, me, true),
            Verdict::Safe,
            "user-private group"
        );
        assert_eq!(verdict(0o775, me, me, false), Verdict::SharedGroup);
        assert_eq!(verdict(0o777, me, me, true), Verdict::Unsafe);
        assert_eq!(verdict(0o755, 1001, me, true), Verdict::Unsafe);
    }
}
