//! Registration as the `x-scheme-handler/rocketchat` handler (freedesktop).
//!
//! Writes `$XDG_DATA_HOME/applications/rustket-url-handler.desktop` and runs
//! `xdg-mime default`. Every external command gets the caller's environment
//! overrides, so tests can point `XDG_DATA_HOME`/`XDG_CONFIG_HOME` at temporary
//! directories without touching the real desktop.
//!
//! Packaged installs (`.deb`, see `packaging/`) ship
//! `/usr/share/applications/rustket.desktop` with the same `MimeType`. When
//! the effective `rustket.desktop` (the first one in `$XDG_DATA_HOME`,
//! `$XDG_DATA_DIRS`) starts the executable being registered, registration
//! makes that file the default instead of writing a second entry, and removes
//! a stale `rustket-url-handler.desktop` left from before the install.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const DESKTOP_FILE: &str = "rustket-url-handler.desktop";
/// The desktop file installed by the packages (also the window's app id).
pub const PACKAGED_DESKTOP_FILE: &str = "rustket.desktop";
pub const MIME: &str = "x-scheme-handler/rocketchat";

/// Environment of the registration: data dirs plus variables for child processes.
#[derive(Debug, Clone)]
pub struct XdgEnv {
    pub data_home: PathBuf,
    /// `$XDG_DATA_DIRS`, searched for the packaged desktop file.
    pub data_dirs: Vec<PathBuf>,
    /// `$PATH`, to resolve a relative `Exec` of the packaged desktop file.
    pub path: Vec<PathBuf>,
    /// Extra environment for `xdg-mime` / `update-desktop-database`.
    pub vars: Vec<(String, String)>,
}

impl XdgEnv {
    /// The real user environment.
    pub fn from_env() -> Option<Self> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|d| d.data_local_dir().to_owned()))?;
        let data_dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        Some(Self {
            data_home,
            data_dirs: std::env::split_paths(&data_dirs)
                .filter(|p| p.is_absolute())
                .collect(),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            vars: Vec::new(),
        })
    }

    pub fn applications_dir(&self) -> PathBuf {
        self.data_home.join("applications")
    }

    fn command(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.envs(self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        c
    }
}

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
}

/// The executable to register and warnings to show before registering.
/// `$APPIMAGE` is used only when this process runs from that AppImage (the
/// current executable lies inside `$APPDIR`, the image's mount) and the image
/// is a regular file; otherwise the current executable.
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
    let exe = from_appimage.unwrap_or_else(|| current_exe.to_owned());
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

/// The user's primary gid (`/proc/self` is owned by the effective uid/gid).
fn primary_gid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.gid())
}

/// Whether `gid` is the user's primary group without other members.
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
/// group with write access yields a warning.
pub fn check_executable(exe: &Path) -> Result<Vec<String>, RegisterError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
    let uid = crate::single_instance::current_uid().unwrap_or(u32::MAX);
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

pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Rustket\nComment=Rocket.Chat links\nExec={} %u\nTerminal=false\nNoDisplay=true\nMimeType={MIME};\n",
        quote_exec(&exe.to_string_lossy())
    )
}

/// The current handler (`xdg-mime query default`), e.g. `rocketchat.desktop`.
pub fn current_handler(env: &XdgEnv) -> Option<String> {
    let out = env
        .command("xdg-mime")
        .args(["query", "default", MIME])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Whether a desktop file id (from [`current_handler`]) is one of Rustket's.
pub fn is_ours(id: &str) -> bool {
    id == DESKTOP_FILE || id == PACKAGED_DESKTOP_FILE
}

/// Whether Rustket is the registered handler.
pub fn is_registered(env: &XdgEnv) -> bool {
    current_handler(env).as_deref().is_some_and(is_ours)
}

/// The first token of an `Exec` value, unquoted (Desktop Entry spec).
fn exec_program(exec: &str) -> Option<String> {
    let exec = exec.trim_start().replace("\\\\", "\\");
    let Some(rest) = exec.strip_prefix('"') else {
        return exec.split_whitespace().next().map(str::to_owned);
    };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

/// The packaged desktop file when it is the effective `rustket.desktop` (the
/// first one in the data dirs), is not hidden, handles [`MIME`] and starts
/// `exe` (compared after resolving symlinks; a relative `Exec` through `PATH`).
pub fn packaged_entry(env: &XdgEnv, exe: &Path) -> Option<PathBuf> {
    let file = std::iter::once(&env.data_home)
        .chain(&env.data_dirs)
        .map(|d| d.join("applications").join(PACKAGED_DESKTOP_FILE))
        .find(|f| f.is_file())?;
    let text = std::fs::read_to_string(&file).ok()?;
    let (mut in_entry, mut exec, mut mime, mut hidden) = (false, None, false, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_entry) else {
            continue;
        };
        match key.trim() {
            "Exec" => exec = exec_program(value),
            "MimeType" => mime = value.split(';').any(|m| m.trim() == MIME),
            "Hidden" => hidden = value.trim() == "true",
            _ => {}
        }
    }
    if hidden || !mime {
        return None;
    }
    let program = PathBuf::from(exec?);
    let program = if program.is_absolute() {
        program
    } else if program.components().count() == 1 {
        env.path
            .iter()
            .map(|d| d.join(&program))
            .find(|p| p.is_file())?
    } else {
        return None;
    };
    let same = match (program.canonicalize(), exe.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    same.then_some(file)
}

/// Makes Rustket the default handler: the packaged desktop file when it starts
/// `exe` ([`packaged_entry`]), otherwise a written `rustket-url-handler.desktop`.
/// Returns the path of the desktop file.
pub fn register(env: &XdgEnv, exe: &Path) -> Result<PathBuf, RegisterError> {
    let dir = env.applications_dir();
    let (id, path) = match packaged_entry(env, exe) {
        Some(packaged) => {
            // Our own entry from before the package was installed.
            let _ = std::fs::remove_file(dir.join(DESKTOP_FILE));
            (PACKAGED_DESKTOP_FILE, packaged)
        }
        None => {
            std::fs::create_dir_all(&dir).map_err(|source| RegisterError::Write {
                path: dir.clone(),
                source,
            })?;
            let path = dir.join(DESKTOP_FILE);
            std::fs::write(&path, desktop_entry(exe)).map_err(|source| RegisterError::Write {
                path: path.clone(),
                source,
            })?;
            (DESKTOP_FILE, path)
        }
    };
    let out = env
        .command("xdg-mime")
        .args(["default", id, MIME])
        .output()
        .map_err(|e| RegisterError::XdgMime(e.to_string()))?;
    if !out.status.success() {
        return Err(RegisterError::XdgMime(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    // Optional: refresh the MIME cache when the tool exists.
    match env.command("update-desktop-database").arg(&dir).output() {
        Ok(o) if !o.status.success() => tracing::debug!(
            stderr = %String::from_utf8_lossy(&o.stderr),
            "update-desktop-database failed"
        ),
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "update-desktop-database not available"),
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quoting() {
        let e = desktop_entry(Path::new("/opt/my apps/rustket"));
        assert!(e.contains("Exec=\"/opt/my apps/rustket\" %u\n"));
        assert!(e.contains("MimeType=x-scheme-handler/rocketchat;\n"));
        assert!(e.contains("NoDisplay=true\n"));
        assert_eq!(quote_exec("/usr/bin/rustket"), "/usr/bin/rustket");
        assert_eq!(quote_exec("/a/100%"), "/a/100%%");
        assert_eq!(quote_exec("/a b/$x"), "\"/a b/\\\\$x\"");
        // A literal backslash needs four in the file.
        assert_eq!(quote_exec("/a\\b"), "\"/a\\\\\\\\b\"");
    }

    #[test]
    fn registers_in_temporary_xdg_dirs() {
        if Command::new("xdg-mime").arg("--version").output().is_err() {
            eprintln!("xdg-mime not installed; skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let sys = tempfile::tempdir().unwrap();
        let env = test_env(data.path(), config.path(), sys.path());
        let path = register(&env, Path::new("/usr/bin/rustket")).unwrap();
        assert!(path.starts_with(data.path()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Exec=/usr/bin/rustket %u"));
        let mimeapps = std::fs::read_to_string(config.path().join("mimeapps.list")).unwrap();
        assert!(mimeapps.contains("x-scheme-handler/rocketchat=rustket-url-handler.desktop"));
        assert_eq!(current_handler(&env).as_deref(), Some(DESKTOP_FILE));
        assert!(is_registered(&env));

        // After installing the package, registering the packaged binary
        // switches to the packaged desktop file and drops the old entry.
        let exe = install_packaged(sys.path(), "Exec=rustket %u");
        let path = register(&env, &exe).unwrap();
        assert_eq!(
            path,
            sys.path().join("applications").join("rustket.desktop")
        );
        assert!(!data.path().join("applications").join(DESKTOP_FILE).exists());
        let mimeapps = std::fs::read_to_string(config.path().join("mimeapps.list")).unwrap();
        assert!(mimeapps.contains("x-scheme-handler/rocketchat=rustket.desktop"));
        assert!(is_registered(&env));
    }

    fn test_env(data: &Path, config: &Path, sys: &Path) -> XdgEnv {
        XdgEnv {
            data_home: data.to_owned(),
            data_dirs: vec![sys.to_owned()],
            path: vec![sys.join("bin")],
            vars: vec![
                ("XDG_DATA_HOME".into(), data.display().to_string()),
                ("XDG_DATA_DIRS".into(), sys.display().to_string()),
                ("XDG_CONFIG_HOME".into(), config.display().to_string()),
                // Use the generic backend, never the desktop's own (kde/gnome) tools.
                ("XDG_CURRENT_DESKTOP".into(), "X-Generic".into()),
                ("DE".into(), "generic".into()),
                ("KDE_SESSION_VERSION".into(), String::new()),
            ],
        }
    }

    /// A fake package install under `share`: `bin/rustket` and
    /// `applications/rustket.desktop` with the given `Exec` line.
    fn install_packaged(share: &Path, exec: &str) -> PathBuf {
        let bin = share.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("rustket");
        std::fs::write(&exe, "").unwrap();
        let apps = share.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join(PACKAGED_DESKTOP_FILE),
            format!(
                "[Desktop Entry]\nType=Application\nName=Rustket\n{exec}\nMimeType=x-scheme-handler/rocketchat;\n"
            ),
        )
        .unwrap();
        exe
    }

    #[test]
    fn packaged_entry_rules() {
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let sys = tempfile::tempdir().unwrap();
        let env = test_env(data.path(), config.path(), sys.path());
        let other = sys.path().join("other");
        std::fs::write(&other, "").unwrap();

        assert_eq!(packaged_entry(&env, &other), None, "no package installed");
        let exe = install_packaged(sys.path(), "Exec=rustket %u");
        let file = sys.path().join("applications").join(PACKAGED_DESKTOP_FILE);
        assert_eq!(packaged_entry(&env, &exe), Some(file.clone()));
        assert_eq!(packaged_entry(&env, &other), None, "another executable");

        // Absolute and quoted Exec values.
        install_packaged(sys.path(), &format!("Exec=\"{}\" %u", exe.display()));
        assert_eq!(packaged_entry(&env, &exe), Some(file.clone()));

        // Without the MIME type, or hidden: not a handler.
        std::fs::write(&file, "[Desktop Entry]\nExec=rustket %u\n").unwrap();
        assert_eq!(packaged_entry(&env, &exe), None);
        install_packaged(sys.path(), "Exec=rustket %u\nHidden=true");
        assert_eq!(packaged_entry(&env, &exe), None);

        // A user override in $XDG_DATA_HOME shadows the packaged file.
        install_packaged(sys.path(), "Exec=rustket %u");
        install_packaged(data.path(), &format!("Exec={} %u", other.display()));
        assert_eq!(packaged_entry(&env, &exe), None);

        assert!(is_ours(DESKTOP_FILE) && is_ours("rustket.desktop"));
        assert!(!is_ours("rocketchat.desktop"));
        assert_eq!(exec_program("  rustket %u").as_deref(), Some("rustket"));
        assert_eq!(
            exec_program("\"/opt/my apps/rustket\" %u").as_deref(),
            Some("/opt/my apps/rustket")
        );
        assert_eq!(exec_program("\"/a\\\\\\\\b\" %u").as_deref(), Some("/a\\b"));
        assert_eq!(exec_program("\"unterminated"), None);
    }

    /// Review #12/#13 and re-review #3: what may be registered.
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
