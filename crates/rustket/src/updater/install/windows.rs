//! Windows: the Inno Setup installer, run silently after the app quits, and
//! the portable zip, swapped in place. The file swap is platform-independent
//! (tested everywhere); spawning and extraction need Windows.

use std::ffi::OsString;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::not_writable;
use crate::updater::UpdateError;
use crate::updater::detect::InnoScope;

/// Staging dir of a portable update inside the install dir.
pub const STAGE_DIR: &str = ".rustket-update";
pub const EXE: &str = "rustket.exe";
/// Top-level dir of the portable zip.
pub const ZIP_ROOT: &str = "Rustket";

/// Arguments of a silent update and the raw `/LOG="…"` argument (Inno's own
/// quoting; `raw_arg` keeps it intact for paths with spaces). `/relaunch=1`
/// makes the installer start the app afterwards (`rustket.iss`).
pub fn inno_args(scope: InnoScope, log: &Path) -> (Vec<OsString>, OsString) {
    let scope = match scope {
        InnoScope::CurrentUser => "/CURRENTUSER",
        InnoScope::AllUsers => "/ALLUSERS",
    };
    let args = [
        "/VERYSILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/SP-",
        "/CLOSEAPPLICATIONS",
        scope,
        "/relaunch=1",
    ]
    .map(OsString::from)
    .to_vec();
    (args, format!("/LOG=\"{}\"", log.display()).into())
}

/// Starts the installer detached; it waits for this process to exit
/// (`rustket.iss` `PrepareToInstall`) and restarts the app. A setup that
/// does not finish writes `result` next to itself and starts the old copy,
/// which reports it (`relaunch::cleanup_stale`); a declined UAC prompt
/// leaves no trace.
#[cfg(windows)]
pub fn install_inno(scope: InnoScope, file: &Path) -> Result<super::Outcome, UpdateError> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let log = file.with_file_name("setup.log");
    let _ = std::fs::remove_file(file.with_file_name(crate::updater::relaunch::RESULT_FILE));
    let (args, raw) = inno_args(scope, &log);
    let mut cmd = Command::new(file);
    cmd.args(&args)
        .raw_arg(&raw)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::updater::relaunch::detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| UpdateError::io("start the installer", &e))?;
    tracing::info!("updater: started the installer, log {}", log.display());
    Ok(super::Outcome::Quit)
}

/// Extracts the zip next to the running copy and swaps the files in.
#[cfg(windows)]
pub fn install_zip(dir: &Path, file: &Path) -> Result<super::Outcome, UpdateError> {
    super::probe_writable(dir)?;
    let stage = dir.join(STAGE_DIR);
    match std::fs::remove_dir_all(&stage) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(UpdateError::io("remove the old staging dir", &e));
        }
        _ => {}
    }
    std::fs::create_dir(&stage).map_err(|e| not_writable(dir, &e, "create the staging dir"))?;
    let result =
        expand_archive(file, &stage).and_then(|()| swap_files(&stage.join(ZIP_ROOT), dir, EXE));
    if let Err(e) = std::fs::remove_dir_all(&stage) {
        tracing::debug!("updater: cannot remove {}: {e}", stage.display());
    }
    result?;
    Ok(super::Outcome::Relaunch(
        crate::updater::relaunch::Relaunch::new(dir.join(EXE)),
    ))
}

/// PowerShell `Expand-Archive`; paths go through the environment, so no
/// quoting is involved.
#[cfg(windows)]
fn expand_archive(zip: &Path, dest: &Path) -> Result<(), UpdateError> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::path::PathBuf;
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let ps = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let out = Command::new(&ps)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "Expand-Archive -LiteralPath $env:RUSTKET_UPDATE_ZIP \
             -DestinationPath $env:RUSTKET_UPDATE_DEST -Force",
        ])
        .env("RUSTKET_UPDATE_ZIP", zip)
        .env("RUSTKET_UPDATE_DEST", dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| UpdateError::io("run PowerShell", &e))?;
    if out.status.success() {
        return Ok(());
    }
    let tail = &out.stderr[out.stderr.len().saturating_sub(4096)..];
    tracing::debug!("updater: Expand-Archive: {}", String::from_utf8_lossy(tail));
    Err(UpdateError::Tool {
        tool: "Expand-Archive".into(),
        code: out.status.code(),
    })
}

/// Moves the files of `src` into `dst`: the running `exe` is renamed to
/// `<exe>.old` first (Windows allows renaming a running executable; the next
/// start removes it), and restored if the new one cannot be moved in.
/// `src` must contain `exe` and only regular files.
pub fn swap_files(src: &Path, dst: &Path, exe: &str) -> Result<(), UpdateError> {
    swap_files_with(src, dst, exe, &|from, to| std::fs::rename(from, to))
}

pub(crate) fn swap_files_with(
    src: &Path,
    dst: &Path,
    exe: &str,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), UpdateError> {
    let files = regular_files(src)?;
    if !files.iter().any(|f| f == exe) {
        return Err(UpdateError::BadRelease(format!("the archive has no {exe}")));
    }
    let target = dst.join(exe);
    let mut old = dst.join(format!("{exe}.old"));
    if old.symlink_metadata().is_ok() && std::fs::remove_file(&old).is_err() {
        old = dst.join(format!("{exe}.old-{}", std::process::id()));
        let _ = std::fs::remove_file(&old);
    }
    let had_exe = target.symlink_metadata().is_ok();
    if had_exe {
        rename(&target, &old).map_err(|e| not_writable(dst, &e, "move the old executable"))?;
    }
    if let Err(e) = rename(&src.join(exe), &target) {
        if had_exe && let Err(back) = rename(&old, &target) {
            tracing::error!("updater: cannot restore {}: {back}", target.display());
        }
        return Err(not_writable(dst, &e, "move the new executable"));
    }
    for name in files.iter().filter(|f| *f != exe) {
        rename(&src.join(name), &dst.join(name))
            .map_err(|e| not_writable(dst, &e, "move an updated file"))?;
    }
    Ok(())
}

/// Names of the entries of `dir`; anything but a regular file (a directory,
/// a link or another reparse point) is refused.
fn regular_files(dir: &Path) -> Result<Vec<String>, UpdateError> {
    let read = |e: std::io::Error| UpdateError::io("read the extracted update", &e);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read)? {
        let entry = entry.map_err(read)?;
        let meta = entry.path().symlink_metadata().map_err(read)?;
        let name = entry.file_name().into_string().ok();
        match name {
            Some(name) if meta.file_type().is_file() && !reparse_point(&meta) => names.push(name),
            _ => {
                return Err(UpdateError::BadRelease(format!(
                    "unexpected archive entry {:?}",
                    entry.file_name()
                )));
            }
        }
    }
    Ok(names)
}

#[cfg(windows)]
fn reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn inno_arguments() {
        let log = Path::new(r"C:\Users\A B\AppData\Local\rustket\cache\updates\0.3.0\setup.log");
        let (args, raw) = inno_args(InnoScope::CurrentUser, log);
        assert_eq!(
            args,
            [
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/SP-",
                "/CLOSEAPPLICATIONS",
                "/CURRENTUSER",
                "/relaunch=1"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            raw,
            OsString::from(
                r#"/LOG="C:\Users\A B\AppData\Local\rustket\cache\updates\0.3.0\setup.log""#
            )
        );
        let (args, _) = inno_args(InnoScope::AllUsers, log);
        assert!(args.contains(&OsString::from("/ALLUSERS")));
        assert!(!args.contains(&OsString::from("/CURRENTUSER")));
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("stage/Rustket");
        let dst = tmp.path().join("app");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        for (name, text) in [(EXE, "new exe"), ("LICENSE.txt", "new license")] {
            std::fs::write(src.join(name), text).unwrap();
        }
        for (name, text) in [(EXE, "old exe"), ("LICENSE.txt", "old license")] {
            std::fs::write(dst.join(name), text).unwrap();
        }
        (tmp, src, dst)
    }

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn swap_moves_everything() {
        let (_tmp, src, dst) = setup();
        swap_files(&src, &dst, EXE).unwrap();
        assert_eq!(read(dst.join(EXE)), "new exe");
        assert_eq!(read(dst.join("LICENSE.txt")), "new license");
        assert_eq!(read(dst.join("rustket.exe.old")), "old exe");
        assert_eq!(std::fs::read_dir(&src).unwrap().count(), 0);
    }

    #[test]
    fn swap_with_a_stuck_old_file() {
        let (_tmp, src, dst) = setup();
        // A directory cannot be removed with `remove_file`.
        std::fs::create_dir_all(dst.join("rustket.exe.old/x")).unwrap();
        swap_files(&src, &dst, EXE).unwrap();
        assert_eq!(read(dst.join(EXE)), "new exe");
        let pid_old = dst.join(format!("rustket.exe.old-{}", std::process::id()));
        assert_eq!(read(pid_old), "old exe");
    }

    #[test]
    fn swap_rolls_back_the_exe() {
        let (_tmp, src, dst) = setup();
        let new_exe = src.join(EXE);
        let failing = |from: &Path, to: &Path| {
            if from == new_exe {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::rename(from, to)
            }
        };
        assert_eq!(
            swap_files_with(&src, &dst, EXE, &failing),
            Err(UpdateError::NotWritable(dst.clone()))
        );
        assert_eq!(read(dst.join(EXE)), "old exe");
        assert_eq!(read(dst.join("LICENSE.txt")), "old license");
        assert!(!dst.join("rustket.exe.old").exists());
    }

    #[test]
    fn swap_refuses_odd_archives() {
        let (_tmp, src, dst) = setup();
        std::fs::create_dir(src.join("sub")).unwrap();
        assert!(matches!(
            swap_files(&src, &dst, EXE),
            Err(UpdateError::BadRelease(_))
        ));
        std::fs::remove_dir(src.join("sub")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
            assert!(matches!(
                swap_files(&src, &dst, EXE),
                Err(UpdateError::BadRelease(_))
            ));
            std::fs::remove_file(src.join("link")).unwrap();
        }
        std::fs::remove_file(src.join(EXE)).unwrap();
        assert!(matches!(
            swap_files(&src, &dst, EXE),
            Err(UpdateError::BadRelease(_))
        ));
        // Nothing was touched.
        assert_eq!(read(dst.join(EXE)), "old exe");
        assert_eq!(read(dst.join("LICENSE.txt")), "old license");
    }
}
