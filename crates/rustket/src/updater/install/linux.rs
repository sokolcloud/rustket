//! deb and rpm through pkexec, AppImage by replacing the image file. The
//! argv builders are platform-independent; the rest needs unix.
//!
//! The package manager never reads a path the user can write: root copies
//! the staged package into a directory of its own, checks the copy against
//! the release's digest and installs the copy ([`ROOT_SCRIPT`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::{Outcome, copy_verified, not_writable, run_pkexec};
#[cfg(unix)]
use crate::updater::UpdateError;
#[cfg(unix)]
use crate::updater::relaunch::Relaunch;

/// Where deb and rpm install the binary.
pub const LINUX_EXE: &str = "/usr/bin/rustket";

/// `apt-get install` of a local file (resolves dependencies), or `dpkg -i`
/// without apt; [`ROOT_SCRIPT`] appends the file. pkexec scrubs the
/// environment; with stdin on the null device and no terminal, debconf and
/// needrestart do not prompt.
pub fn deb_argv(apt: Option<&Path>, dpkg: Option<&Path>) -> Option<Vec<OsString>> {
    let argv: Vec<OsString> = match (apt, dpkg) {
        (Some(apt), _) => vec![
            apt.into(),
            "install".into(),
            "-y".into(),
            "--allow-downgrades".into(),
            "-o".into(),
            "Dpkg::Options::=--force-confdef".into(),
            "-o".into(),
            "Dpkg::Options::=--force-confold".into(),
        ],
        (None, Some(dpkg)) => vec![dpkg.into(), "-i".into()],
        (None, None) => return None,
    };
    Some(argv)
}

/// The first of dnf, zypper and rpm, without the file ([`ROOT_SCRIPT`]
/// appends it). The packages are not signed.
pub fn rpm_argv(
    dnf: Option<&Path>,
    zypper: Option<&Path>,
    rpm: Option<&Path>,
) -> Option<Vec<OsString>> {
    let argv: Vec<OsString> = if let Some(dnf) = dnf {
        vec![dnf.into(), "install".into(), "-y".into()]
    } else if let Some(zypper) = zypper {
        vec![
            zypper.into(),
            "--non-interactive".into(),
            "install".into(),
            "--allow-unsigned-rpm".into(),
        ]
    } else {
        let rpm = rpm?;
        vec![rpm.into(), "-U".into(), "--replacepkgs".into()]
    };
    Some(argv)
}

/// Exit status of [`ROOT_SCRIPT`]: the copy does not have the digest.
pub const EXIT_MISMATCH: i32 = 97;
/// Exit status of [`ROOT_SCRIPT`]: the copy could not be made.
pub const EXIT_STAGING: i32 = 98;

/// Runs as root (`sh -c`), arguments `<package> <sha256 hex> <size> <name>
/// <installer argv…>`. The package path is writable by the user, also while
/// the authentication dialog is open, so root copies at most `size` bytes
/// into a directory only root can write, checks the copy's digest and runs
/// the installer on the copy. All values are positional arguments; nothing
/// from them is evaluated. The copy becomes world-readable (apt's `_apt`
/// user) only after the check.
pub const ROOT_SCRIPT: &str = r#"PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
umask 077
src=$1 sha=$2 size=$3 name=$4
shift 4
dir=$(mktemp -d /run/rustket-update.XXXXXXXXXX 2>/dev/null) ||
  dir=$(mktemp -d /tmp/rustket-update.XXXXXXXXXX) || exit 98
trap 'rm -rf "$dir"' EXIT
trap 'exit 98' HUP INT TERM
pkg=$dir/$name
head -c "$size" -- "$src" >"$pkg" || exit 98
sum=$(sha256sum <"$pkg") || exit 98
[ "${sum%% *}" = "$sha" ] || exit 97
chmod 755 "$dir" && chmod 644 "$pkg" || exit 98
umask 022
"$@" "$pkg"
"#;

/// `sh -c ROOT_SCRIPT` for the staged package `staged` of `size` bytes with
/// the digest `sha`, installed by `installer`.
pub fn root_argv(
    sh: &Path,
    staged: &Path,
    sha: &[u8; 32],
    size: u64,
    installer: Vec<OsString>,
) -> Vec<OsString> {
    let name = staged.file_name().map(OsString::from).unwrap_or_default();
    let mut argv: Vec<OsString> = vec![
        sh.into(),
        "-c".into(),
        ROOT_SCRIPT.into(),
        "rustket-update".into(),
        staged.into(),
        hex::encode(sha).into(),
        size.to_string().into(),
        name,
    ];
    argv.extend(installer);
    argv
}

/// Lookups and the process runner, injected for tests.
pub struct Tools<'a> {
    pub find: &'a dyn Fn(&str) -> Option<PathBuf>,
    pub pkexec: Option<PathBuf>,
    pub runner: &'a super::Runner<'a>,
    /// Parent of the staging dir.
    pub temp: PathBuf,
}

impl Tools<'static> {
    pub fn system() -> Self {
        Self {
            find: &system_find,
            pkexec: super::pkexec_path(&super::system_program),
            runner: &super::run_quiet,
            temp: std::env::temp_dir(),
        }
    }
}

fn system_find(name: &str) -> Option<PathBuf> {
    super::find_program(name, &super::system_program)
}

#[cfg(unix)]
pub fn install_deb(file: &Path, sha: [u8; 32], tools: &Tools) -> Result<Outcome, UpdateError> {
    let apt = (tools.find)("apt-get");
    let dpkg = (tools.find)("dpkg");
    if apt.is_none() && dpkg.is_none() {
        return Err(UpdateError::Unsupported);
    }
    let installer = deb_argv(apt.as_deref(), dpkg.as_deref()).ok_or(UpdateError::Unsupported)?;
    install_as_root(file, &sha, tools, installer)
}

#[cfg(unix)]
pub fn install_rpm(file: &Path, sha: [u8; 32], tools: &Tools) -> Result<Outcome, UpdateError> {
    let dnf = (tools.find)("dnf");
    let zypper = (tools.find)("zypper");
    let rpm = (tools.find)("rpm");
    if dnf.is_none() && zypper.is_none() && rpm.is_none() {
        return Err(UpdateError::Unsupported);
    }
    let installer = rpm_argv(dnf.as_deref(), zypper.as_deref(), rpm.as_deref())
        .ok_or(UpdateError::Unsupported)?;
    install_as_root(file, &sha, tools, installer)
}

/// Stages `file` and runs `installer` on root's verified copy of it.
#[cfg(unix)]
fn install_as_root(
    file: &Path,
    sha: &[u8; 32],
    tools: &Tools,
    installer: Vec<OsString>,
) -> Result<Outcome, UpdateError> {
    let sh = (tools.find)("sh").ok_or(UpdateError::Unsupported)?;
    let tool = super::tool_name(installer.first().map(Path::new));
    let staged = Staging::new(&tools.temp, file, sha)?;
    let argv = root_argv(&sh, &staged.file, sha, staged.size, installer);
    match run_pkexec(tools.pkexec.as_deref(), &argv, tools.runner) {
        Ok(()) => Ok(Outcome::Relaunch(Relaunch::new(LINUX_EXE))),
        Err(UpdateError::Tool {
            code: Some(EXIT_MISMATCH),
            ..
        }) => {
            tracing::warn!("updater: the staged package changed before root read it");
            Err(UpdateError::ChecksumMismatch)
        }
        Err(UpdateError::Tool {
            code: Some(EXIT_STAGING),
            ..
        }) => Err(UpdateError::Io {
            op: "copy the package as root",
            kind: std::io::ErrorKind::Other,
        }),
        // The script exits with the installer's status.
        Err(UpdateError::Tool { code, .. }) => Err(UpdateError::Tool { tool, code }),
        Err(e) => Err(e),
    }
}

/// A fresh world-readable copy of the package outside the home dir (homes on
/// root-squash NFS are not readable by root), re-verified after the copy.
/// It stays writable by the user: root trusts only its own copy
/// ([`ROOT_SCRIPT`]). Removed on drop.
#[cfg(unix)]
struct Staging {
    dir: PathBuf,
    file: PathBuf,
    /// Length of the verified copy.
    size: u64,
}

#[cfg(unix)]
impl Staging {
    fn new(temp: &Path, src: &Path, sha: &[u8; 32]) -> Result<Self, UpdateError> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        let name = src
            .file_name()
            .ok_or_else(|| UpdateError::BadRelease("package path".into()))?;
        let dir = temp.join(format!(
            "rustket-update-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        // `create`, not `create_dir_all`: an existing dir is an error.
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&dir)
            .map_err(|e| UpdateError::io("create the staging dir", &e))?;
        let mut staging = Self {
            file: dir.join(name),
            dir,
            size: 0,
        };
        // Explicit modes: the umask must not hide the file from root on
        // systems where root's access is restricted.
        std::fs::set_permissions(&staging.dir, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| UpdateError::io("create the staging dir", &e))?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&staging.file)
            .map_err(|e| UpdateError::io("stage the package", &e))?;
        out.set_permissions(std::fs::Permissions::from_mode(0o644))
            .map_err(|e| UpdateError::io("stage the package", &e))?;
        copy_verified(src, &mut out, sha)?;
        staging.size = out
            .metadata()
            .map_err(|e| UpdateError::io("stage the package", &e))?
            .len();
        Ok(staging)
    }
}

#[cfg(unix)]
impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!("updater: cannot remove {}: {e}", self.dir.display());
        }
    }
}

/// Replaces the AppImage `image` (symlinks resolved) with `file`.
#[cfg(unix)]
pub fn install_appimage(image: &Path, file: &Path, sha: [u8; 32]) -> Result<Outcome, UpdateError> {
    let image =
        std::fs::canonicalize(image).map_err(|e| UpdateError::io("find the AppImage", &e))?;
    replace_file(file, &image, &sha)?;
    Ok(Outcome::Relaunch(Relaunch::appimage(image)))
}

/// Atomically replaces `target` with a verified copy of `src`, keeping the
/// target's name and permission bits (plus `0o755`). A process that has the
/// old file open or mapped keeps reading the old content.
#[cfg(unix)]
pub fn replace_file(src: &Path, target: &Path, sha: &[u8; 32]) -> Result<(), UpdateError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(UpdateError::Unsupported);
    };
    let part = dir.join(format!(
        ".{}.update-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let old_mode = std::fs::metadata(target).map_or(0o755, |m| m.permissions().mode());
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&part)
        .map_err(|e| not_writable(dir, &e, "create the new AppImage"))?;
    let result = copy_verified(src, &mut out, sha)
        .and_then(|()| {
            out.set_permissions(std::fs::Permissions::from_mode((old_mode | 0o755) & 0o7777))
                .map_err(|e| UpdateError::io("set the AppImage mode", &e))
        })
        .and_then(|()| {
            std::fs::rename(&part, target)
                .map_err(|e| not_writable(dir, &e, "replace the AppImage"))
        });
    drop(out);
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
        return result;
    }
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        tracing::debug!("updater: cannot sync {}: {e}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn strs(argv: Option<Vec<OsString>>) -> Vec<String> {
        argv.unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn deb_commands() {
        assert_eq!(
            strs(deb_argv(
                Some(&p("/usr/bin/apt-get")),
                Some(&p("/usr/bin/dpkg"))
            )),
            [
                "/usr/bin/apt-get",
                "install",
                "-y",
                "--allow-downgrades",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
            ]
        );
        assert_eq!(
            strs(deb_argv(None, Some(&p("/usr/bin/dpkg")))),
            ["/usr/bin/dpkg", "-i"]
        );
        assert_eq!(deb_argv(None, None), None);
    }

    #[test]
    fn rpm_commands() {
        let dnf = p("/usr/bin/dnf");
        let zypper = p("/usr/bin/zypper");
        let rpm = p("/usr/bin/rpm");
        assert_eq!(
            strs(rpm_argv(Some(&dnf), Some(&zypper), Some(&rpm))),
            ["/usr/bin/dnf", "install", "-y"]
        );
        assert_eq!(
            strs(rpm_argv(None, Some(&zypper), Some(&rpm))),
            [
                "/usr/bin/zypper",
                "--non-interactive",
                "install",
                "--allow-unsigned-rpm"
            ]
        );
        assert_eq!(
            strs(rpm_argv(None, None, Some(&rpm))),
            ["/usr/bin/rpm", "-U", "--replacepkgs"]
        );
        assert_eq!(rpm_argv(None, None, None), None);
    }

    #[test]
    fn root_command() {
        let staged = p("/tmp/rustket-update-1-2/rustket_0.3.0-1_amd64.deb");
        let argv = root_argv(
            &p("/usr/bin/sh"),
            &staged,
            &[0xab; 32],
            1234,
            vec!["/usr/bin/dpkg".into(), "-i".into()],
        );
        assert_eq!(
            strs(Some(argv)),
            [
                "/usr/bin/sh",
                "-c",
                ROOT_SCRIPT,
                "rustket-update",
                "/tmp/rustket-update-1-2/rustket_0.3.0-1_amd64.deb",
                "ab".repeat(32).as_str(),
                "1234",
                "rustket_0.3.0-1_amd64.deb",
                "/usr/bin/dpkg",
                "-i",
            ]
        );
    }

    #[test]
    fn root_script_shape() {
        let s = ROOT_SCRIPT;
        assert!(!s.contains('`'));
        assert!(!s.contains("eval"));
        // The installer runs on root's copy, after the digest check.
        let copy = s.find("head -c \"$size\" -- \"$src\" >\"$pkg\"").unwrap();
        let check = s.find("[ \"${sum%% *}\" = \"$sha\" ] || exit 97").unwrap();
        let open = s.find("chmod 755 \"$dir\"").unwrap();
        let run = s.find("\"$@\" \"$pkg\"").unwrap();
        assert!(copy < check && check < open && open < run);
        // The user's path is read once, for the copy.
        assert_eq!(s.matches("$src").count(), 1);
        assert!(s.contains(&format!("exit {EXIT_MISMATCH}")));
        assert!(s.contains(&format!("exit {EXIT_STAGING}")));
    }

    #[cfg(unix)]
    mod unix {
        use std::cell::RefCell;
        use std::os::unix::fs::PermissionsExt;

        use sha2::{Digest, Sha256};

        use super::super::*;

        fn sha(data: &[u8]) -> [u8; 32] {
            Sha256::digest(data).into()
        }

        fn mode(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
        }

        fn finder(name: &str) -> Option<PathBuf> {
            match name {
                "sh" => Some(PathBuf::from("/bin/sh")),
                "apt-get" | "dpkg" | "rpm" => Some(PathBuf::from("/usr/bin").join(name)),
                _ => None,
            }
        }

        /// Stands in for the package manager: copies the file it is given
        /// to `out` and its path to `out.path`.
        #[cfg(target_os = "linux")]
        fn fake_installer(out: &Path) -> Vec<OsString> {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                r#"cat "$1" >"$0" && echo "$1" >"$0.path""#.into(),
                out.into(),
            ]
        }

        /// Runs the root script as the test user.
        #[cfg(target_os = "linux")]
        fn run_script(src: &Path, sha: &[u8; 32], size: u64, out: &Path) -> Option<i32> {
            let argv = root_argv(Path::new("/bin/sh"), src, sha, size, fake_installer(out));
            crate::updater::install::run_quiet(Path::new(&argv[0]), &argv[1..]).unwrap()
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn root_script_installs_only_a_verified_copy() {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("rustket_9.9.9-1_amd64.deb");
            let out = tmp.path().join("installed");
            std::fs::write(&src, b"deb").unwrap();

            assert_eq!(run_script(&src, &sha(b"deb"), 3, &out), Some(0));
            assert_eq!(std::fs::read(&out).unwrap(), b"deb");
            let seen = std::fs::read_to_string(out.with_extension("path")).unwrap();
            let seen = PathBuf::from(seen.trim_end());
            assert_ne!(seen, src);
            assert_eq!(seen.file_name(), src.file_name());
            // Root's copy is gone afterwards.
            assert!(!seen.parent().unwrap().exists());
            std::fs::remove_file(&out).unwrap();

            // Another file under the same path: the installer does not run.
            std::fs::write(&src, b"bad").unwrap();
            assert_eq!(run_script(&src, &sha(b"deb"), 3, &out), Some(EXIT_MISMATCH));
            assert!(!out.exists());

            // A link to something else, too.
            std::fs::remove_file(&src).unwrap();
            let other = tmp.path().join("other");
            std::fs::write(&other, b"other package").unwrap();
            std::os::unix::fs::symlink(&other, &src).unwrap();
            assert_eq!(run_script(&src, &sha(b"deb"), 3, &out), Some(EXIT_MISMATCH));
            assert!(!out.exists());
            std::fs::remove_file(&src).unwrap();

            // Bytes past the verified length are not copied.
            std::fs::write(&src, b"deb and a payload").unwrap();
            assert_eq!(run_script(&src, &sha(b"deb"), 3, &out), Some(0));
            assert_eq!(std::fs::read(&out).unwrap(), b"deb");
            std::fs::remove_file(&out).unwrap();

            std::fs::remove_file(&src).unwrap();
            assert_eq!(run_script(&src, &sha(b"deb"), 3, &out), Some(EXIT_STAGING));
            assert!(!out.exists());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn root_script_exits_with_the_installer_status() {
            let tmp = tempfile::tempdir().unwrap();
            let src = tmp.path().join("p.rpm");
            std::fs::write(&src, b"rpm").unwrap();
            let installer = vec!["/bin/sh".into(), "-c".into(), "exit 5".into()];
            let argv = root_argv(Path::new("/bin/sh"), &src, &sha(b"rpm"), 3, installer);
            let code = crate::updater::install::run_quiet(Path::new(&argv[0]), &argv[1..]);
            assert_eq!(code.unwrap(), Some(5));
        }

        /// The exploit: the staged file is replaced while the authentication
        /// dialog is open (here: right before the pkexec stand-in runs).
        #[cfg(target_os = "linux")]
        #[test]
        fn a_package_swapped_during_authentication_is_not_installed() {
            let tmp = tempfile::tempdir().unwrap();
            let pkg = tmp.path().join("rustket_9.9.9-1_amd64.deb");
            std::fs::write(&pkg, b"deb").unwrap();
            let temp = tmp.path().join("tmp");
            std::fs::create_dir(&temp).unwrap();
            let out = tmp.path().join("installed");
            let swap = std::cell::Cell::new(true);
            let runner = |_: &Path, argv: &[OsString]| {
                let staged = PathBuf::from(&argv[4]);
                if swap.get() {
                    let crafted = staged.with_extension("crafted");
                    std::fs::write(&crafted, b"bad")?;
                    std::fs::rename(&crafted, &staged)?;
                }
                // The script and its arguments as built, the package
                // manager replaced.
                let mut args = argv[1..8].to_vec();
                args.extend(fake_installer(&out));
                crate::updater::install::run_quiet(Path::new(&argv[0]), &args)
            };
            let tools = Tools {
                find: &finder,
                pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
                runner: &runner,
                temp: temp.clone(),
            };
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::ChecksumMismatch)
            );
            assert!(!out.exists());
            assert_eq!(
                install_rpm(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::ChecksumMismatch)
            );
            assert!(!out.exists());

            swap.set(false);
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Ok(Outcome::Relaunch(Relaunch::new(LINUX_EXE)))
            );
            assert_eq!(std::fs::read(&out).unwrap(), b"deb");
            assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0);
        }

        #[test]
        fn deb_goes_through_a_staging_copy() {
            let tmp = tempfile::tempdir().unwrap();
            let pkg = tmp.path().join("rustket_9.9.9-1_amd64.deb");
            std::fs::write(&pkg, b"deb").unwrap();
            let temp = tmp.path().join("tmp");
            std::fs::create_dir(&temp).unwrap();
            let seen = RefCell::new(Vec::new());
            let exit = RefCell::new(Some(0));
            let runner = |pk: &Path, argv: &[OsString]| {
                // pkexec runs the root script, not the package manager.
                assert_eq!(argv[0].to_str(), Some("/bin/sh"));
                assert_eq!(argv[1].to_str(), Some("-c"));
                assert_eq!(argv[2].to_str(), Some(ROOT_SCRIPT));
                let staged = PathBuf::from(&argv[4]);
                assert_eq!(std::fs::read(&staged).unwrap(), b"deb");
                assert_eq!(mode(&staged), 0o644);
                assert_eq!(mode(staged.parent().unwrap()), 0o755);
                assert_eq!(staged.parent().unwrap().parent(), Some(temp.as_path()));
                assert_eq!(argv[5].to_str(), Some(hex::encode(sha(b"deb")).as_str()));
                assert_eq!(argv[6].to_str(), Some("3"));
                assert_eq!(argv[7].to_str(), Some("rustket_9.9.9-1_amd64.deb"));
                // The package manager gets no path from this side.
                assert_eq!(
                    argv.last().unwrap().to_str(),
                    Some("Dpkg::Options::=--force-confold")
                );
                seen.borrow_mut().push((pk.to_owned(), argv[8].clone()));
                Ok(*exit.borrow())
            };
            let tools = Tools {
                find: &finder,
                pkexec: Some(PathBuf::from("/usr/bin/pkexec")),
                runner: &runner,
                temp: temp.clone(),
            };
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Ok(Outcome::Relaunch(Relaunch::new(LINUX_EXE)))
            );
            assert_eq!(
                seen.borrow().as_slice(),
                [(
                    PathBuf::from("/usr/bin/pkexec"),
                    OsString::from("/usr/bin/apt-get")
                )]
            );
            assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0);

            *exit.borrow_mut() = Some(126);
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::AuthCancelled)
            );
            assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0);

            // Root's own check failed.
            *exit.borrow_mut() = Some(EXIT_MISMATCH);
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::ChecksumMismatch)
            );
            *exit.borrow_mut() = Some(EXIT_STAGING);
            assert!(matches!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::Io { .. })
            ));
            // Any other status is the package manager's.
            *exit.borrow_mut() = Some(100);
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &tools),
                Err(UpdateError::Tool {
                    tool: "apt-get".into(),
                    code: Some(100)
                })
            );
            assert_eq!(seen.borrow().len(), 5);

            // A changed package never reaches pkexec.
            assert_eq!(
                install_deb(&pkg, sha(b"other"), &tools),
                Err(UpdateError::ChecksumMismatch)
            );
            assert_eq!(seen.borrow().len(), 5);
            assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0);

            let no_pkexec = Tools {
                pkexec: None,
                ..tools
            };
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &no_pkexec),
                Err(UpdateError::NoAuthAgent)
            );
            let rpm = install_rpm(&pkg, sha(b"deb"), &no_pkexec);
            assert_eq!(rpm, Err(UpdateError::NoAuthAgent));
            // No trusted shell for the root script.
            let no_sh = Tools {
                find: &|name| finder(name).filter(|_| name != "sh"),
                ..no_pkexec
            };
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &no_sh),
                Err(UpdateError::Unsupported)
            );
            let nothing = Tools {
                find: &|_| None,
                ..no_sh
            };
            assert_eq!(
                install_deb(&pkg, sha(b"deb"), &nothing),
                Err(UpdateError::Unsupported)
            );
        }

        #[test]
        fn replace_keeps_open_files_and_sets_the_mode() {
            let tmp = tempfile::tempdir().unwrap();
            let target = tmp.path().join("Rustket-0.2.2-x86_64.AppImage");
            std::fs::write(&target, b"old").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut old = std::fs::File::open(&target).unwrap();
            let src = tmp.path().join("new");
            std::fs::write(&src, b"new image").unwrap();

            replace_file(&src, &target, &sha(b"new image")).unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"new image");
            assert_eq!(mode(&target), 0o755);
            let mut text = String::new();
            std::io::Read::read_to_string(&mut old, &mut text).unwrap();
            assert_eq!(text, "old");

            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o775)).unwrap();
            replace_file(&src, &target, &sha(b"new image")).unwrap();
            assert_eq!(mode(&target), 0o775);

            assert_eq!(
                replace_file(&src, &target, &sha(b"other")),
                Err(UpdateError::ChecksumMismatch)
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"new image");
            let names: Vec<_> = std::fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names.len(), 2, "{names:?}");
        }

        #[test]
        fn replace_through_a_symlink_replaces_the_image() {
            let tmp = tempfile::tempdir().unwrap();
            let image = tmp.path().join("Rustket.AppImage");
            std::fs::write(&image, b"old").unwrap();
            let link = tmp.path().join("rustket");
            std::os::unix::fs::symlink(&image, &link).unwrap();
            let src = tmp.path().join("new");
            std::fs::write(&src, b"new").unwrap();
            let out = install_appimage(&link, &src, sha(b"new")).unwrap();
            let Outcome::Relaunch(r) = out else {
                panic!("{out:?}");
            };
            assert_eq!(r.program, image.canonicalize().unwrap());
            assert!(r.env_remove.contains(&"APPIMAGE"));
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
            assert_eq!(std::fs::read(&image).unwrap(), b"new");
        }

        #[test]
        fn replace_in_a_read_only_dir_is_not_writable() {
            if rustix::process::geteuid().is_root() {
                return;
            }
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("ro");
            std::fs::create_dir(&dir).unwrap();
            let target = dir.join("R.AppImage");
            std::fs::write(&target, b"old").unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let src = tmp.path().join("new");
            std::fs::write(&src, b"new").unwrap();
            let got = replace_file(&src, &target, &sha(b"new"));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(got, Err(UpdateError::NotWritable(dir)));
        }
    }
}
