//! deb and rpm through pkexec, AppImage by replacing the image file. The
//! argv builders are platform-independent; the rest needs unix.

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

/// `apt-get install` of the local file (resolves dependencies), or
/// `dpkg -i` without apt. pkexec scrubs the environment; with stdin on the
/// null device and no terminal, debconf and needrestart do not prompt.
pub fn deb_argv(apt: Option<&Path>, dpkg: Option<&Path>, file: &Path) -> Option<Vec<OsString>> {
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
            file.into(),
        ],
        (None, Some(dpkg)) => vec![dpkg.into(), "-i".into(), file.into()],
        (None, None) => return None,
    };
    Some(argv)
}

/// The first of dnf, zypper and rpm. The packages are not signed.
pub fn rpm_argv(
    dnf: Option<&Path>,
    zypper: Option<&Path>,
    rpm: Option<&Path>,
    file: &Path,
) -> Option<Vec<OsString>> {
    let argv: Vec<OsString> = if let Some(dnf) = dnf {
        vec![dnf.into(), "install".into(), "-y".into(), file.into()]
    } else if let Some(zypper) = zypper {
        vec![
            zypper.into(),
            "--non-interactive".into(),
            "install".into(),
            "--allow-unsigned-rpm".into(),
            file.into(),
        ]
    } else {
        let rpm = rpm?;
        vec![rpm.into(), "-U".into(), "--replacepkgs".into(), file.into()]
    };
    Some(argv)
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
    let staged = Staging::new(&tools.temp, file, &sha)?;
    let argv =
        deb_argv(apt.as_deref(), dpkg.as_deref(), &staged.file).ok_or(UpdateError::Unsupported)?;
    run_pkexec(tools.pkexec.as_deref(), &argv, tools.runner)?;
    Ok(Outcome::Relaunch(Relaunch::new(LINUX_EXE)))
}

#[cfg(unix)]
pub fn install_rpm(file: &Path, sha: [u8; 32], tools: &Tools) -> Result<Outcome, UpdateError> {
    let dnf = (tools.find)("dnf");
    let zypper = (tools.find)("zypper");
    let rpm = (tools.find)("rpm");
    if dnf.is_none() && zypper.is_none() && rpm.is_none() {
        return Err(UpdateError::Unsupported);
    }
    let staged = Staging::new(&tools.temp, file, &sha)?;
    let argv = rpm_argv(
        dnf.as_deref(),
        zypper.as_deref(),
        rpm.as_deref(),
        &staged.file,
    )
    .ok_or(UpdateError::Unsupported)?;
    run_pkexec(tools.pkexec.as_deref(), &argv, tools.runner)?;
    Ok(Outcome::Relaunch(Relaunch::new(LINUX_EXE)))
}

/// A fresh world-readable copy of the package outside the home dir (homes on
/// root-squash NFS are not readable by root), re-verified after the copy.
/// Removed on drop.
#[cfg(unix)]
struct Staging {
    dir: PathBuf,
    file: PathBuf,
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
        let staging = Self {
            file: dir.join(name),
            dir,
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
        let file = p("/tmp/rustket-update-1-2/rustket_0.3.0-1_amd64.deb");
        assert_eq!(
            strs(deb_argv(
                Some(&p("/usr/bin/apt-get")),
                Some(&p("/usr/bin/dpkg")),
                &file
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
                "/tmp/rustket-update-1-2/rustket_0.3.0-1_amd64.deb",
            ]
        );
        assert_eq!(
            strs(deb_argv(None, Some(&p("/usr/bin/dpkg")), &file)),
            [
                "/usr/bin/dpkg",
                "-i",
                "/tmp/rustket-update-1-2/rustket_0.3.0-1_amd64.deb"
            ]
        );
        assert_eq!(deb_argv(None, None, &file), None);
    }

    #[test]
    fn rpm_commands() {
        let file = p("/tmp/s/rustket-0.3.0-1.x86_64.rpm");
        let dnf = p("/usr/bin/dnf");
        let zypper = p("/usr/bin/zypper");
        let rpm = p("/usr/bin/rpm");
        assert_eq!(
            strs(rpm_argv(Some(&dnf), Some(&zypper), Some(&rpm), &file)),
            [
                "/usr/bin/dnf",
                "install",
                "-y",
                "/tmp/s/rustket-0.3.0-1.x86_64.rpm"
            ]
        );
        assert_eq!(
            strs(rpm_argv(None, Some(&zypper), Some(&rpm), &file)),
            [
                "/usr/bin/zypper",
                "--non-interactive",
                "install",
                "--allow-unsigned-rpm",
                "/tmp/s/rustket-0.3.0-1.x86_64.rpm"
            ]
        );
        assert_eq!(
            strs(rpm_argv(None, None, Some(&rpm), &file)),
            [
                "/usr/bin/rpm",
                "-U",
                "--replacepkgs",
                "/tmp/s/rustket-0.3.0-1.x86_64.rpm"
            ]
        );
        assert_eq!(rpm_argv(None, None, None, &file), None);
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
            matches!(name, "apt-get" | "dpkg" | "rpm").then(|| PathBuf::from("/usr/bin").join(name))
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
                let staged = PathBuf::from(argv.last().unwrap());
                assert_eq!(std::fs::read(&staged).unwrap(), b"deb");
                assert_eq!(mode(&staged), 0o644);
                assert_eq!(mode(staged.parent().unwrap()), 0o755);
                assert_eq!(staged.parent().unwrap().parent(), Some(temp.as_path()));
                seen.borrow_mut().push((pk.to_owned(), argv[0].clone()));
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

            // A changed package never reaches pkexec.
            assert_eq!(
                install_deb(&pkg, sha(b"other"), &tools),
                Err(UpdateError::ChecksumMismatch)
            );
            assert_eq!(seen.borrow().len(), 2);
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
            let nothing = Tools {
                find: &|_| None,
                ..no_pkexec
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
