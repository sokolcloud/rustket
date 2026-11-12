//! Installing a verified package the way this copy was installed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::UpdateError;
use super::detect::InstallKind;
use super::github::Release;
use super::relaunch::Relaunch;

pub mod linux;
pub mod macos;
pub mod windows;

/// What the app does after a successful install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Start this, then quit.
    Relaunch(Relaunch),
    /// A spawned installer or helper restarts the app; just quit.
    Quit,
}

/// Installs `file` (verified, digest `sha`) for `kind`. Blocking; may ask
/// for authentication (pkexec, osascript).
pub fn install(
    kind: &InstallKind,
    file: &Path,
    sha: [u8; 32],
    release: &Release,
) -> Result<Outcome, UpdateError> {
    tracing::info!(?kind, "updater: installing {}", release.version);
    match kind {
        InstallKind::Unmanaged => Err(UpdateError::Unsupported),
        #[cfg(target_os = "linux")]
        InstallKind::Deb { .. } => linux::install_deb(file, sha, &linux::Tools::system()),
        #[cfg(target_os = "linux")]
        InstallKind::Rpm { .. } => linux::install_rpm(file, sha, &linux::Tools::system()),
        #[cfg(target_os = "linux")]
        InstallKind::AppImage { image } => linux::install_appimage(image, file, sha),
        #[cfg(windows)]
        InstallKind::InnoSetup { scope, .. } => windows::install_inno(*scope, file),
        #[cfg(windows)]
        InstallKind::PortableZip { dir } => windows::install_zip(dir, file),
        #[cfg(target_os = "macos")]
        InstallKind::MacApp { bundle } => macos::install_dmg(bundle, file, release),
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (file, sha);
            Err(UpdateError::Unsupported)
        }
    }
}

/// Programs run as root are looked up only here, never through `$PATH`.
pub const SYSTEM_DIRS: [&str; 4] = ["/usr/bin", "/usr/sbin", "/bin", "/sbin"];
const PKEXEC: [&str; 2] = ["/usr/bin/pkexec", "/bin/pkexec"];
/// Debug builds only: a pkexec stand-in for end-to-end tests.
const PKEXEC_OVERRIDE_VAR: &str = "RUSTKET_UPDATE_PKEXEC";
const STDERR_TAIL: usize = 4 * 1024;

/// `name` in [`SYSTEM_DIRS`] accepted by `is_ok` (see [`system_program`]).
pub fn find_program(name: &str, is_ok: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    SYSTEM_DIRS
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| is_ok(p))
}

/// An absolute path to a regular, executable file owned by root and not
/// writable by group or others.
pub fn system_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        path.is_absolute()
            && std::fs::metadata(path).is_ok_and(|m| {
                m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0 && m.mode() & 0o111 != 0
            })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// pkexec, or in debug builds the `RUSTKET_UPDATE_PKEXEC` override.
pub fn pkexec_path(is_ok: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(over) = std::env::var_os(PKEXEC_OVERRIDE_VAR).filter(|v| !v.is_empty())
    {
        tracing::debug!("updater: pkexec override {}", Path::new(&over).display());
        return Some(PathBuf::from(over));
    }
    PKEXEC.iter().map(PathBuf::from).find(|p| is_ok(p))
}

/// Runs a program and returns its exit code (`None`: killed by a signal).
pub type Runner<'a> = dyn Fn(&Path, &[OsString]) -> std::io::Result<Option<i32>> + 'a;

/// Runs `program` with stdin and stdout on the null device; the tail of
/// stderr goes to the debug log.
pub fn run_quiet(program: &Path, args: &[OsString]) -> std::io::Result<Option<i32>> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output()?;
    let tail = &out.stderr[out.stderr.len().saturating_sub(STDERR_TAIL)..];
    if !tail.is_empty() {
        tracing::debug!(
            "updater: {} stderr: {}",
            program.display(),
            String::from_utf8_lossy(tail)
        );
    }
    Ok(out.status.code())
}

/// Runs `argv` through `pkexec`: exit 126 means the user dismissed the
/// authentication dialog, 127 that no agent could authenticate.
pub fn run_pkexec(
    pkexec: Option<&Path>,
    argv: &[OsString],
    runner: &Runner<'_>,
) -> Result<(), UpdateError> {
    let Some(pkexec) = pkexec else {
        return Err(UpdateError::NoAuthAgent);
    };
    tracing::debug!("updater: pkexec {argv:?}");
    match runner(pkexec, argv) {
        Ok(Some(0)) => Ok(()),
        Ok(Some(126)) => Err(UpdateError::AuthCancelled),
        Ok(Some(127)) => Err(UpdateError::NoAuthAgent),
        Ok(code) => Err(UpdateError::Tool {
            tool: tool_name(argv.first().map(Path::new)),
            code,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(UpdateError::NoAuthAgent),
        Err(e) => Err(UpdateError::io("run pkexec", &e)),
    }
}

/// Runs a tool without elevation; a non-zero exit is `Tool`.
pub fn run_tool(program: &Path, args: &[OsString], runner: &Runner<'_>) -> Result<(), UpdateError> {
    match runner(program, args) {
        Ok(Some(0)) => Ok(()),
        Ok(code) => Err(UpdateError::Tool {
            tool: tool_name(Some(program)),
            code,
        }),
        Err(e) => Err(UpdateError::io("run a tool", &e)),
    }
}

fn tool_name(program: Option<&Path>) -> String {
    program
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether this process can create files in `dir`.
pub fn probe_writable(dir: &Path) -> Result<(), UpdateError> {
    let probe = dir.join(format!(".rustket-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(not_writable(dir, &e, "probe the install dir")),
    }
}

/// Permission problems are `NotWritable(dir)`, anything else `Io`.
pub(crate) fn not_writable(dir: &Path, e: &std::io::Error, op: &'static str) -> UpdateError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => {
            UpdateError::NotWritable(dir.to_owned())
        }
        _ => UpdateError::io(op, e),
    }
}

/// Copies `src` into the open `dst`, hashing on the way, and syncs it; the
/// copy must have the digest `sha`.
#[cfg(unix)]
pub(crate) fn copy_verified(
    src: &Path,
    dst: &mut std::fs::File,
    sha: &[u8; 32],
) -> Result<(), UpdateError> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let mut input =
        std::fs::File::open(src).map_err(|e| UpdateError::io("read the package", &e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| UpdateError::io("read the package", &e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])
            .map_err(|e| UpdateError::io("copy the package", &e))?;
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if digest != *sha {
        return Err(UpdateError::ChecksumMismatch);
    }
    dst.sync_all()
        .map_err(|e| UpdateError::io("copy the package", &e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn programs_come_from_system_dirs_only() {
        let found = find_program("apt-get", &|p| p == Path::new("/usr/sbin/apt-get"));
        assert_eq!(found, Some(PathBuf::from("/usr/sbin/apt-get")));
        assert_eq!(find_program("dnf", &|_| false), None);
        let seen = RefCell::new(Vec::new());
        find_program("rpm", &|p| {
            seen.borrow_mut().push(p.to_owned());
            false
        });
        assert_eq!(
            seen.into_inner(),
            ["/usr/bin/rpm", "/usr/sbin/rpm", "/bin/rpm", "/sbin/rpm"].map(PathBuf::from)
        );
        assert!(!system_program(Path::new("sh")));
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            let own = tmp.path().join("x");
            std::fs::write(&own, b"#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o755)).unwrap();
            // Owned by the test user unless the tests run as root.
            assert_eq!(system_program(&own), rustix::process::geteuid().is_root());
        }
    }

    #[test]
    fn pkexec_exit_codes() {
        let argv: Vec<OsString> = ["/usr/bin/apt-get", "install"].map(OsString::from).to_vec();
        let pk = Path::new("/usr/bin/pkexec");
        let code = |c: Option<i32>| run_pkexec(Some(pk), &argv, &move |_, _| Ok(c));
        assert_eq!(code(Some(0)), Ok(()));
        assert_eq!(code(Some(126)), Err(UpdateError::AuthCancelled));
        assert_eq!(code(Some(127)), Err(UpdateError::NoAuthAgent));
        assert_eq!(
            code(Some(100)),
            Err(UpdateError::Tool {
                tool: "apt-get".into(),
                code: Some(100)
            })
        );
        assert_eq!(
            code(None),
            Err(UpdateError::Tool {
                tool: "apt-get".into(),
                code: None
            })
        );
        assert_eq!(
            run_pkexec(None, &argv, &|_, _| Ok(Some(0))),
            Err(UpdateError::NoAuthAgent)
        );
        let missing = run_pkexec(Some(pk), &argv, &|_, _| {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!(missing, Err(UpdateError::NoAuthAgent));
        let seen = RefCell::new(None);
        run_pkexec(Some(pk), &argv, &|p, a| {
            *seen.borrow_mut() = Some((p.to_owned(), a.to_vec()));
            Ok(Some(0))
        })
        .unwrap();
        assert_eq!(seen.into_inner(), Some((pk.to_owned(), argv.clone())));
    }

    #[cfg(unix)]
    #[test]
    fn run_quiet_reports_the_exit_code() {
        let sh = Path::new("/bin/sh");
        let args = |s: &str| vec![OsString::from("-c"), OsString::from(s)];
        assert_eq!(
            run_quiet(sh, &args("echo err >&2; exit 3")).unwrap(),
            Some(3)
        );
        assert_eq!(run_tool(sh, &args("exit 0"), &run_quiet), Ok(()));
        assert_eq!(
            run_tool(sh, &args("exit 2"), &run_quiet),
            Err(UpdateError::Tool {
                tool: "sh".into(),
                code: Some(2)
            })
        );
    }

    #[test]
    fn writability_probe() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(probe_writable(tmp.path()), Ok(()));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        assert!(matches!(
            probe_writable(&tmp.path().join("missing")),
            Err(UpdateError::Io { .. })
        ));
        #[cfg(unix)]
        if !rustix::process::geteuid().is_root() {
            use std::os::unix::fs::PermissionsExt;
            let ro = tmp.path().join("ro");
            std::fs::create_dir(&ro).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
            assert_eq!(
                probe_writable(&ro),
                Err(UpdateError::NotWritable(ro.clone()))
            );
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn copy_checks_the_digest() {
        use sha2::{Digest, Sha256};
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::write(&src, b"package").unwrap();
        let sha: [u8; 32] = Sha256::digest(b"package").into();
        let mut dst = std::fs::File::create(tmp.path().join("dst")).unwrap();
        assert_eq!(copy_verified(&src, &mut dst, &sha), Ok(()));
        assert_eq!(std::fs::read(tmp.path().join("dst")).unwrap(), b"package");
        let mut dst = std::fs::File::create(tmp.path().join("dst2")).unwrap();
        assert_eq!(
            copy_verified(&src, &mut dst, &[0; 32]),
            Err(UpdateError::ChecksumMismatch)
        );
    }

    #[test]
    fn unmanaged_is_unsupported() {
        let release = Release {
            version: semver::Version::new(9, 9, 9),
            tag: "v9.9.9".into(),
            html_url: String::new(),
            notes: String::new(),
            prerelease: false,
            assets: vec![],
            sums: None,
        };
        assert_eq!(
            install(&InstallKind::Unmanaged, Path::new("/x"), [0; 32], &release),
            Err(UpdateError::Unsupported)
        );
    }
}
