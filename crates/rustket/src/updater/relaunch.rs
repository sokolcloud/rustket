//! Starting the updated app after this process exits, and cleaning up after
//! earlier updates.
//!
//! The new process gets `--wait-parent` and the read end of a pipe as stdin;
//! this process keeps the write end until it exits. The new process blocks on
//! stdin until EOF, so it starts only after the old one released its
//! single-instance endpoint (otherwise it would forward to it and exit).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use semver::Version;

use super::UpdateError;
use super::detect::InstallKind;
use crate::renderer;

pub const WAIT_PARENT_FLAG: &str = "--wait-parent";
/// How long a relaunched process waits for the old one to exit.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Written next to the downloaded file by the macOS helper (`ok` or
/// `failed:<step>`) and by an Inno setup that did not finish a silent
/// update (`failed:<step>`, `rustket.iss`).
pub const RESULT_FILE: &str = "result";

/// Write ends of the relaunch pipes; never dropped, so they close only when
/// this process exits.
static RELAUNCH_PIPES: Mutex<Vec<ChildStdin>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relaunch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env_remove: Vec<&'static str>,
}

impl Relaunch {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self::with_env(program.into(), false)
    }

    /// An AppImage: the runtime sets its variables anew.
    pub fn appimage(image: impl Into<PathBuf>) -> Self {
        Self::with_env(image.into(), true)
    }

    fn with_env(program: PathBuf, appimage: bool) -> Self {
        let reexec = std::env::var_os(renderer::REEXEC_MARKER).is_some();
        Self {
            program,
            args: vec![WAIT_PARENT_FLAG.into()],
            env_remove: env_to_remove(reexec, appimage),
        }
    }
}

/// Variables the relaunched process must not inherit: activation tokens
/// (already used by this window), the AppImage runtime's variables (AppRun
/// keeps an inherited `APPDIR`) and the renderer this process chose for
/// itself (`REEXEC_MARKER` set). A backend the user set stays.
pub fn env_to_remove(reexec_marker_set: bool, appimage: bool) -> Vec<&'static str> {
    let mut vars = vec!["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"];
    if appimage {
        vars.extend(["APPIMAGE", "APPDIR", "ARGV0", "OWD"]);
    }
    if reexec_marker_set {
        vars.extend([renderer::BACKEND_VAR, renderer::REEXEC_MARKER]);
    }
    vars
}

/// Starts `r` detached, with a pipe on stdin whose write end this process
/// keeps until it exits.
pub fn spawn(r: &Relaunch) -> std::io::Result<()> {
    let mut cmd = Command::new(&r.program);
    cmd.args(&r.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for var in &r.env_remove {
        cmd.env_remove(var);
    }
    detach(&mut cmd);
    let mut child = cmd.spawn()?;
    tracing::info!("updater: started {}", r.program.display());
    if let Some(stdin) = child.stdin.take() {
        RELAUNCH_PIPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(stdin);
    }
    Ok(())
}

/// Own process group (unix) or a detached process without a console
/// (Windows), so the child outlives this process and its terminal.
pub(crate) fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    NotRequested,
    Waited(Duration),
    TimedOut,
}

impl WaitOutcome {
    /// Called after logging is set up.
    pub fn log(self) {
        match self {
            Self::NotRequested => {}
            Self::Waited(d) => {
                tracing::info!("updater: relaunched after the old process exited ({d:?})");
            }
            Self::TimedOut => {
                tracing::warn!("updater: the old process did not exit within {WAIT_TIMEOUT:?}");
            }
        }
    }
}

/// With `--wait-parent`, blocks until stdin reaches EOF (the old process
/// exited) or [`WAIT_TIMEOUT`]. Runs before logging is initialised; the
/// caller logs the outcome.
pub fn wait_for_parent_if_requested() -> WaitOutcome {
    if !std::env::args_os().skip(1).any(|a| a == WAIT_PARENT_FLAG) {
        return WaitOutcome::NotRequested;
    }
    let start = Instant::now();
    if wait_for_eof(std::io::stdin(), WAIT_TIMEOUT) {
        WaitOutcome::Waited(start.elapsed())
    } else {
        WaitOutcome::TimedOut
    }
}

/// Reads `reader` to EOF on a helper thread; `true` if that happened (or the
/// read failed) within `timeout`.
pub fn wait_for_eof<R: Read + Send + 'static>(mut reader: R, timeout: Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("wait-parent".into())
        .spawn(move || {
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            let _ = tx.send(());
        });
    match spawned {
        Ok(_) => rx.recv_timeout(timeout).is_ok(),
        Err(_) => false,
    }
}

/// A failed macOS bundle swap reported by the helper of an earlier run.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleFailure {
    pub version: Version,
    pub error: UpdateError,
    /// The prepared bundle or the downloaded image, for "Show file".
    pub file: Option<PathBuf>,
}

/// Removes download dirs of versions up to the running one, `.part` files,
/// a portable zip's `rustket.exe.old*` and staging dir and AppImage temp
/// files. Reports a failed macOS swap or Inno setup. Blocking.
pub fn cleanup_stale(kind: &InstallKind) -> Option<StaleFailure> {
    let current = super::current_version()?;
    let cache = super::cache_dir()?;
    cleanup_in(&cache, &current, kind)
}

pub(crate) fn cleanup_in(
    cache: &Path,
    current: &Version,
    kind: &InstallKind,
) -> Option<StaleFailure> {
    let mut failure = None;
    for entry in read_dir(cache) {
        let path = entry.path();
        let Some(version) = entry
            .file_name()
            .to_str()
            .and_then(|n| Version::parse(n).ok())
        else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        if version <= *current {
            remove_dir(&path);
            continue;
        }
        for file in read_dir(&path) {
            if file.file_name().to_string_lossy().ends_with(".part") {
                remove_file(&file.path());
            }
        }
        match kind {
            InstallKind::MacApp { bundle } => {
                if let Some(error) = take_result(&path, "swap") {
                    failure = Some(StaleFailure {
                        file: mac_leftover(&path, bundle),
                        version,
                        error,
                    });
                }
            }
            InstallKind::InnoSetup { .. } => {
                if let Some(error) = take_result(&path, "setup") {
                    let file = read_dir(&path)
                        .map(|e| e.path())
                        .find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")));
                    failure = Some(StaleFailure {
                        file,
                        version,
                        error,
                    });
                }
            }
            _ => {}
        }
    }
    match kind {
        InstallKind::PortableZip { dir } => {
            for entry in read_dir(dir) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rustket.exe.old") {
                    remove_file(&entry.path());
                } else if name == super::install::windows::STAGE_DIR {
                    remove_dir(&entry.path());
                }
            }
        }
        InstallKind::AppImage { image } => {
            let image = std::fs::canonicalize(image).unwrap_or_else(|_| image.clone());
            if let (Some(dir), Some(name)) = (image.parent(), image.file_name()) {
                let prefix = format!(".{}.update-", name.to_string_lossy());
                for entry in read_dir(dir) {
                    if entry.file_name().to_string_lossy().starts_with(&prefix) {
                        remove_file(&entry.path());
                    }
                }
            }
        }
        _ => {}
    }
    failure
}

/// Reads and removes the helper's result file; `Some` for a failure.
/// `what`: the failing tool's name for the error text.
fn take_result(dir: &Path, what: &str) -> Option<UpdateError> {
    let path = dir.join(RESULT_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    remove_file(&path);
    let error = parse_result(&text, what);
    if let Some(e) = &error {
        tracing::warn!("updater: the previous update failed: {e}");
    }
    error
}

pub(crate) fn parse_result(text: &str, what: &str) -> Option<UpdateError> {
    let line = text.lines().next().unwrap_or_default().trim();
    if line == "ok" {
        return None;
    }
    let step = line.strip_prefix("failed:").unwrap_or("unknown").trim();
    Some(UpdateError::Tool {
        tool: format!("{what} ({step})"),
        code: None,
    })
}

fn mac_leftover(dir: &Path, bundle: &Path) -> Option<PathBuf> {
    let candidates = [
        Some(dir.join(super::install::macos::NEW_BUNDLE)),
        bundle
            .parent()
            .map(|p| p.join(super::install::macos::SIBLING_BUNDLE)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_dir())
        .or_else(|| {
            read_dir(dir)
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "dmg"))
        })
}

fn read_dir(dir: &Path) -> impl Iterator<Item = std::fs::DirEntry> {
    std::fs::read_dir(dir).into_iter().flatten().flatten()
}

fn remove_dir(path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        tracing::debug!("updater: cannot remove {}: {e}", path.display());
    }
}

fn remove_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::debug!("updater: cannot remove {}: {e}", path.display());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn env_removal() {
        assert_eq!(
            env_to_remove(false, false),
            ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"]
        );
        let all = env_to_remove(true, true);
        for var in ["APPIMAGE", "APPDIR", "ARGV0", "OWD", "ICED_BACKEND"] {
            assert!(all.contains(&var), "{var}");
        }
        assert!(all.contains(&renderer::REEXEC_MARKER));
        assert!(!env_to_remove(false, true).contains(&renderer::BACKEND_VAR));
        let r = Relaunch::appimage("/x/R.AppImage");
        assert_eq!(r.args, [OsString::from(WAIT_PARENT_FLAG)]);
        assert!(r.env_remove.contains(&"APPDIR"));
        assert!(
            !Relaunch::new("/usr/bin/rustket")
                .env_remove
                .contains(&"APPDIR")
        );
    }

    #[test]
    fn waits_for_eof() {
        assert!(wait_for_eof(std::io::empty(), Duration::from_secs(5)));
        let (reader, writer) = std::io::pipe().unwrap();
        let start = Instant::now();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(writer);
        });
        assert!(wait_for_eof(reader, Duration::from_secs(10)));
        assert!(start.elapsed() >= Duration::from_millis(150));
        closer.join().unwrap();
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(!wait_for_eof(reader, Duration::from_millis(100)));
    }

    /// The spawned process sees EOF only once the pipe's write end is
    /// dropped, here by closing it explicitly.
    #[cfg(unix)]
    #[test]
    fn spawn_keeps_the_pipe() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out");
        let script = tmp.path().join("s.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho \"$1 ${{XDG_ACTIVATION_TOKEN:-none}}\" >'{}'\n",
                out.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        spawn(&Relaunch::new(&script)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!out.exists(), "the child must still wait");
        let pipe = RELAUNCH_PIPES.lock().unwrap().pop().unwrap();
        drop(pipe);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !out.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        assert_eq!(text.trim(), "--wait-parent none");
    }

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn cleanup_removes_old_versions_and_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("updates");
        touch(&cache.join("0.2.1/rustket_0.2.1-1_amd64.deb"));
        touch(&cache.join("0.2.2/x.deb"));
        touch(&cache.join("0.3.0/x.deb"));
        touch(&cache.join("0.3.0/x.AppImage.part"));
        touch(&cache.join("notes/keep"));
        let current = Version::new(0, 2, 2);
        assert_eq!(cleanup_in(&cache, &current, &InstallKind::Unmanaged), None);
        assert!(!cache.join("0.2.1").exists());
        assert!(!cache.join("0.2.2").exists());
        assert!(cache.join("0.3.0/x.deb").exists());
        assert!(!cache.join("0.3.0/x.AppImage.part").exists());
        assert!(cache.join("notes/keep").exists());

        let dir = tmp.path().join("portable");
        touch(&dir.join("rustket.exe"));
        touch(&dir.join("rustket.exe.old"));
        touch(&dir.join("rustket.exe.old-42"));
        touch(&dir.join(".rustket-update/Rustket/rustket.exe"));
        cleanup_in(
            &cache,
            &current,
            &InstallKind::PortableZip { dir: dir.clone() },
        );
        let names: Vec<_> = read_dir(&dir).map(|e| e.file_name()).collect();
        assert_eq!(names, [OsString::from("rustket.exe")]);

        let apps = tmp.path().join("apps");
        let image = apps.join("Rustket.AppImage");
        touch(&image);
        touch(&apps.join(".Rustket.AppImage.update-77"));
        touch(&apps.join(".Other.AppImage.update-77"));
        cleanup_in(&cache, &current, &InstallKind::AppImage { image });
        assert!(apps.join("Rustket.AppImage").exists());
        assert!(!apps.join(".Rustket.AppImage.update-77").exists());
        assert!(apps.join(".Other.AppImage.update-77").exists());
    }

    #[test]
    fn cleanup_reports_a_failed_mac_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("updates");
        let apps = tmp.path().join("Applications");
        let bundle = apps.join("Rustket.app");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(apps.join(".Rustket-update.app")).unwrap();
        std::fs::create_dir_all(cache.join("0.3.0")).unwrap();
        std::fs::write(cache.join("0.3.0/result"), "failed:swap\n").unwrap();
        let kind = InstallKind::MacApp {
            bundle: bundle.clone(),
        };
        let got = cleanup_in(&cache, &Version::new(0, 2, 2), &kind).unwrap();
        assert_eq!(got.version, Version::new(0, 3, 0));
        assert_eq!(
            got.error,
            UpdateError::Tool {
                tool: "swap (swap)".into(),
                code: None
            }
        );
        assert_eq!(got.file, Some(apps.join(".Rustket-update.app")));
        // Reported once.
        assert!(!cache.join("0.3.0/result").exists());
        assert_eq!(cleanup_in(&cache, &Version::new(0, 2, 2), &kind), None);

        std::fs::write(cache.join("0.3.0/result"), "ok\n").unwrap();
        assert_eq!(cleanup_in(&cache, &Version::new(0, 2, 2), &kind), None);
    }

    #[test]
    fn cleanup_reports_an_unfinished_inno_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("updates");
        let setup = cache.join("0.3.0/Rustket-0.3.0-x64-setup.exe");
        touch(&setup);
        touch(&cache.join("0.3.0/setup.log"));
        std::fs::write(cache.join("0.3.0/result"), "failed:wait\r\n").unwrap();
        let kind = InstallKind::InnoSetup {
            scope: super::super::detect::InnoScope::AllUsers,
            dir: tmp.path().join("app"),
        };
        let got = cleanup_in(&cache, &Version::new(0, 2, 2), &kind).unwrap();
        assert_eq!(got.version, Version::new(0, 3, 0));
        assert_eq!(
            got.error,
            UpdateError::Tool {
                tool: "setup (wait)".into(),
                code: None
            }
        );
        assert_eq!(got.file, Some(setup));
        assert_eq!(cleanup_in(&cache, &Version::new(0, 2, 2), &kind), None);
        // Other kinds leave the file alone.
        std::fs::write(cache.join("0.3.0/result"), "failed:install\n").unwrap();
        assert_eq!(
            cleanup_in(&cache, &Version::new(0, 2, 2), &InstallKind::Unmanaged),
            None
        );
        assert!(cache.join("0.3.0/result").exists());
    }

    #[test]
    fn results() {
        assert_eq!(parse_result("ok\n", "swap"), None);
        assert!(matches!(
            parse_result("failed:wait\n", "swap"),
            Some(UpdateError::Tool { tool, .. }) if tool == "swap (wait)"
        ));
        assert!(matches!(
            parse_result("failed:install\r\n", "setup"),
            Some(UpdateError::Tool { tool, .. }) if tool == "setup (install)"
        ));
        assert!(parse_result("", "swap").is_some());
    }
}
