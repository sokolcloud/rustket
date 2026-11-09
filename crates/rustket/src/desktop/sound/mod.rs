//! Notification sounds through an external player, used when the
//! notification server does not play the notification's sound itself: on
//! Linux `pw-play`, `paplay` or `canberra-gtk-play` with a sound theme file;
//! on Windows PowerShell's `System.Media.SoundPlayer` with a
//! `%SystemRoot%\Media` sound; on macOS `afplay` with a system sound. The
//! player is looked up once on a background thread at startup; playing never
//! blocks: the player runs detached and is reaped (or killed after a
//! timeout) on a blocking thread.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(any(windows, test))]
mod win;

#[cfg(all(unix, not(target_os = "macos")))]
use freedesktop as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(windows)]
use win as backend;

/// Freedesktop sound id of an incoming instant message.
pub const MESSAGE_SOUND: &str = "message-new-instant";
/// A player taking longer than this is killed.
const PLAY_TIMEOUT: Duration = Duration::from_secs(15);

/// The first executable `name` in the `PATH`-style list `path` (on Windows
/// also `name.exe`).
pub fn find_in_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![name.to_owned(), format!("{name}.exe")]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .flat_map(|d| names.iter().map(move |n| d.join(n)).collect::<Vec<_>>())
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Plays sounds; disabled (a no-op) in tests and snapshots.
#[derive(Debug, Default)]
pub struct Sound {
    enabled: bool,
    /// Filled by the background lookup.
    resolved: Arc<OnceLock<backend::Resolved>>,
}

impl Sound {
    /// Starts looking the player up in the background.
    pub fn system() -> Self {
        let resolved = Arc::new(OnceLock::new());
        let slot = resolved.clone();
        let spawned = std::thread::Builder::new()
            .name("sound-lookup".into())
            .spawn(move || {
                let _ = slot.set(backend::lookup());
            });
        if let Err(e) = spawned {
            tracing::debug!(error = %e, "cannot look the sound player up");
        }
        Self {
            enabled: true,
            resolved,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Plays the message sound (the server's custom sound names map to the
    /// platform's instant-message sound). Must be called inside the tokio
    /// runtime.
    pub fn play(&self, name: Option<&str>) {
        let _ = name;
        self.play_id(None);
    }

    /// Plays the freedesktop theme sound `id` (e.g. `phone-incoming-call`;
    /// Windows and macOS map it to a system sound), falling back to the
    /// message sound when there is none.
    pub fn play_theme(&self, id: &'static str) {
        self.play_id(Some(id));
    }

    fn play_id(&self, id: Option<&'static str>) {
        if !self.enabled {
            return;
        }
        let Some(resolved) = self.resolved.get() else {
            tracing::debug!("sound player lookup not finished");
            return;
        };
        let resolved = resolved.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn_blocking(move || {
            let Some(cmd) = backend::play_command(&resolved, id) else {
                tracing::debug!("no sound to play");
                return;
            };
            run_reaped(cmd);
        });
    }
}

/// Runs the player, killing it after [`PLAY_TIMEOUT`]. Blocking.
fn run_reaped(mut cmd: Command) {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "cannot start the sound player");
            return;
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if started.elapsed() < PLAY_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn disabled_sound_is_a_no_op() {
        let s = Sound::default();
        assert!(!s.is_enabled());
        s.play(None);
        assert!(s.resolved.get().is_none());
    }

    #[test]
    fn path_lookup_skips_relative_dirs_and_missing_files() {
        let d = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([std::path::Path::new("rel"), d.path()]).unwrap();
        assert!(find_in_path("nope", &path).is_none());
    }
}
