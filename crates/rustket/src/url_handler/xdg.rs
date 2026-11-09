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
//! makes that file the default instead of writing a second entry, and then
//! removes a stale `rustket-url-handler.desktop` left from before the install.
//! [`registered_for`] counts a default as Rustket's only when its desktop file
//! starts the executable being registered, so a registration left by another
//! build (or by a removed package) shows the register action again.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{DESKTOP_FILE, MIME, PACKAGED_DESKTOP_FILE, RegisterError, quote_exec};

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

/// Whether the default handler `current` is Rustket's and its desktop file
/// (the effective one in the data dirs) starts `exe`. Without a known `exe`
/// only the name is compared.
pub fn registered_for(env: &XdgEnv, current: Option<&str>, exe: Option<&Path>) -> bool {
    let Some(id) = current.filter(|id| is_ours(id)) else {
        return false;
    };
    let Some(exe) = exe else {
        return true;
    };
    effective_entry(env, id)
        .and_then(|file| parse_entry(env, &file))
        .is_some_and(|e| e.mime && !e.hidden && same_file(&e.program, exe))
}

/// Whether Rustket is the registered handler for `exe`.
pub fn is_registered(env: &XdgEnv, exe: &Path) -> bool {
    registered_for(env, current_handler(env).as_deref(), Some(exe))
}

/// The first token of an `Exec` value, unquoted (Desktop Entry spec).
fn exec_program(exec: &str) -> Option<String> {
    let exec = exec.trim_start().replace("\\\\", "\\");
    let Some(rest) = exec.strip_prefix('"') else {
        return exec.split_whitespace().next().map(|p| p.replace("%%", "%"));
    };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out.replace("%%", "%")),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

/// The first `applications/<id>` in `$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`.
fn effective_entry(env: &XdgEnv, id: &str) -> Option<PathBuf> {
    std::iter::once(&env.data_home)
        .chain(&env.data_dirs)
        .map(|d| d.join("applications").join(id))
        .find(|f| f.is_file())
}

/// What matters about a desktop file for link handling.
struct Entry {
    /// The `Exec` program, resolved through `PATH` when relative.
    program: PathBuf,
    mime: bool,
    hidden: bool,
}

fn parse_entry(env: &XdgEnv, file: &Path) -> Option<Entry> {
    let text = std::fs::read_to_string(file).ok()?;
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
    Some(Entry {
        program,
        mime,
        hidden,
    })
}

/// Compared after resolving symlinks; missing files never match.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The packaged desktop file when it is the effective `rustket.desktop` (the
/// first one in the data dirs), is not hidden, handles [`MIME`] and starts
/// `exe` (compared after resolving symlinks; a relative `Exec` through `PATH`).
pub fn packaged_entry(env: &XdgEnv, exe: &Path) -> Option<PathBuf> {
    let file = effective_entry(env, PACKAGED_DESKTOP_FILE)?;
    let entry = parse_entry(env, &file)?;
    (entry.mime && !entry.hidden && same_file(&entry.program, exe)).then_some(file)
}

/// Makes Rustket the default handler: the packaged desktop file when it starts
/// `exe` ([`packaged_entry`]), otherwise a written `rustket-url-handler.desktop`.
/// Returns the path of the desktop file.
pub fn register(env: &XdgEnv, exe: &Path) -> Result<PathBuf, RegisterError> {
    let dir = env.applications_dir();
    let packaged = packaged_entry(env, exe);
    let (id, path) = match packaged.clone() {
        Some(packaged) => (PACKAGED_DESKTOP_FILE, packaged),
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
    if packaged.is_some() {
        // Our own entry from before the package was installed; removed only
        // once the default points at the packaged file.
        let _ = std::fs::remove_file(dir.join(DESKTOP_FILE));
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
        assert!(registered_for(&env, Some(DESKTOP_FILE), None));

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
        assert!(is_registered(&env, &exe));
    }

    /// Review: a default left by another build is not "registered", and a
    /// failed `xdg-mime` keeps the old entry.
    #[test]
    fn stale_registration_is_not_ours() {
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let sys = tempfile::tempdir().unwrap();
        let mut env = test_env(data.path(), config.path(), sys.path());
        let exe = install_packaged(sys.path(), "Exec=rustket %u");
        let dev = sys.path().join("dev-rustket");
        std::fs::write(&dev, "").unwrap();
        let apps = data.path().join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join(DESKTOP_FILE), desktop_entry(&dev)).unwrap();

        assert!(registered_for(&env, Some(DESKTOP_FILE), Some(&dev)));
        assert!(!registered_for(&env, Some(DESKTOP_FILE), Some(&exe)));
        assert!(registered_for(
            &env,
            Some(PACKAGED_DESKTOP_FILE),
            Some(&exe)
        ));
        assert!(!registered_for(
            &env,
            Some(PACKAGED_DESKTOP_FILE),
            Some(&dev)
        ));
        assert!(!registered_for(&env, Some("other.desktop"), Some(&exe)));
        assert!(!registered_for(&env, None, Some(&exe)));
        // Deleted build: never registered.
        std::fs::remove_file(&dev).unwrap();
        assert!(!registered_for(&env, Some(DESKTOP_FILE), Some(&dev)));
        // Removed package: the desktop file is gone.
        std::fs::remove_file(sys.path().join("applications").join(PACKAGED_DESKTOP_FILE)).unwrap();
        assert!(!registered_for(
            &env,
            Some(PACKAGED_DESKTOP_FILE),
            Some(&exe)
        ));

        // xdg-mime failing leaves the old entry in place.
        install_packaged(sys.path(), "Exec=rustket %u");
        let fake = tempfile::tempdir().unwrap();
        let tool = fake.path().join("xdg-mime");
        std::fs::write(&tool, "#!/bin/sh\necho broken >&2\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        env.vars
            .push(("PATH".into(), fake.path().display().to_string()));
        assert!(matches!(
            register(&env, &exe),
            Err(RegisterError::XdgMime(_))
        ));
        assert!(apps.join(DESKTOP_FILE).exists());
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
}
