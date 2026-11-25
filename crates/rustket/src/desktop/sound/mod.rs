//! Sounds through an external player. Message, reminder and new-chat sounds
//! are Rocket.Chat's own ([`rc`]: bundled MP3s, decoded once at their volume
//! into cached WAV files); calls use the platform's sounds. Players: on
//! Linux `pw-play`, `paplay` or `canberra-gtk-play` (with a sound theme file
//! for theme sounds); on Windows PowerShell's `System.Media.SoundPlayer`
//! (theme sounds map to `%SystemRoot%\Media`); on macOS NSSound in-process
//! (theme sounds map to system sounds). When a Rocket.Chat sound cannot be
//! decoded or written, the platform's message sound plays instead. The player
//! is looked up once on a background thread at startup; playing never blocks:
//! the player runs detached and is reaped (or killed after a timeout) on a
//! blocking thread. [`route`] decides whether a notification's backend
//! plays a message or reminder sound instead.

use std::path::PathBuf;
#[cfg(not(target_os = "macos"))]
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use std::time::Instant;

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop;
#[cfg(any(target_os = "macos", test))]
mod macos;
pub mod rc;
pub mod route;
#[cfg(any(windows, test))]
mod win;

#[cfg(all(unix, not(target_os = "macos")))]
use freedesktop as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(windows)]
use win as backend;

pub use route::{OsSound, Platform, Route, Toast, route};

/// Freedesktop sound id of an incoming instant message.
pub const MESSAGE_SOUND: &str = "message-new-instant";
/// A player taking longer than this is killed (macOS: the sound is stopped).
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

/// One of Rocket.Chat's sounds at a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcSound {
    /// The sound's id (`chime`, `door`, …).
    pub name: String,
    /// Percent, 1..=100 (`notificationsSoundVolume` × `masterVolume`).
    pub volume: u8,
}

/// The directory of the decoded sounds.
pub fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustket").map(|d| d.cache_dir().join("sounds"))
}

/// Plays sounds; disabled (a no-op) in tests and snapshots.
#[derive(Debug, Default)]
pub struct Sound {
    enabled: bool,
    /// Filled by the background lookup.
    resolved: Arc<OnceLock<backend::Resolved>>,
    /// Where decoded sounds are kept.
    cache: Option<PathBuf>,
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
            cache: cache_dir(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Plays Rocket.Chat sound `sound` that comes without a sounding
    /// notification ([`route`] with [`Toast::None`]): on Linux not while
    /// the desktop's Do Not Disturb is on. Must be called inside the tokio
    /// runtime.
    pub fn play_alone(&self, sound: &RcSound) {
        if let Route::Local { quiet_check } =
            route(Platform::current(), Toast::None, Some(sound), None)
        {
            self.play_rc(sound, quiet_check);
        }
    }

    /// Plays Rocket.Chat sound `sound` ([`prepared`]); one that cannot be
    /// prepared plays the platform's message sound. `quiet_check`: not
    /// while the desktop's Do Not Disturb is on (Linux). Must be called
    /// inside the tokio runtime.
    pub fn play_rc(&self, sound: &RcSound, quiet_check: bool) {
        if !self.enabled || sound.volume == 0 {
            return;
        }
        let Some(resolved) = self.resolved.get() else {
            tracing::debug!("sound player lookup not finished");
            return;
        };
        let resolved = resolved.clone();
        let cache = self.cache.clone();
        let RcSound { name, volume } = sound.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            if quiet_check && desktop_quiet().await {
                tracing::debug!("Do Not Disturb is on: sound not played");
                return;
            }
            let played = tokio::task::spawn_blocking(move || {
                let file = match cache {
                    Some(dir) => prepared(&dir, &name, volume).map_err(|e| e.to_string()),
                    None => Err("no cache directory".to_owned()),
                };
                let cmd = match file {
                    Ok(f) => backend::file_command(&resolved, &f),
                    Err(e) => {
                        tracing::debug!(sound = %name, error = %e, "sound not prepared");
                        None
                    }
                }
                .or_else(|| backend::play_command(&resolved, None));
                match cmd {
                    Some(cmd) => backend::run(cmd),
                    None => tracing::debug!("no sound to play"),
                }
            })
            .await;
            if let Err(e) = played {
                tracing::debug!(error = %e, "sound task failed");
            }
        });
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
            backend::run(cmd);
        });
    }
}

/// The WAV file to play for Rocket.Chat sound `name` at `volume` percent:
/// a sound Rustket does not bundle (a server's custom sound, `telephone`)
/// plays Rocket.Chat's default `chime` at the same volume. Blocking.
fn prepared(dir: &std::path::Path, name: &str, volume: u8) -> Result<PathBuf, rc::Error> {
    match rc::wav_file(dir, name, volume) {
        Err(rc::Error::Unknown(_)) => rc::wav_file(dir, DEFAULT_SOUND, volume),
        other => other,
    }
}

/// The WAV file of Rocket.Chat sound `sound` for a notification to play
/// ([`prepared`] in the cache directory). Blocking.
pub fn prepared_file(sound: &RcSound) -> Option<PathBuf> {
    let dir = cache_dir()?;
    match prepared(&dir, &sound.name, sound.volume) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::debug!(sound = %sound.name, error = %e, "sound not prepared");
            None
        }
    }
}

/// The copy of Rocket.Chat sound `sound` in `~/Library/Sounds` that a
/// notification plays by name (`macos::install_in`). Blocking.
#[cfg(target_os = "macos")]
pub fn notification_sound(sound: &RcSound) -> Option<PathBuf> {
    let file = prepared_file(sound)?;
    let dir = directories::BaseDirs::new()?
        .home_dir()
        .join("Library")
        .join("Sounds");
    match macos::install_in(&dir, &file) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::debug!(error = %e, "cannot copy the sound to ~/Library/Sounds");
            None
        }
    }
}

/// The desktop's Do Not Disturb is on (Linux; elsewhere never known).
/// Bounded by a short timeout; any error means it is off.
async fn desktop_quiet() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    return freedesktop::quiet().await;
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    false
}

/// Rocket.Chat's default new-message sound.
const DEFAULT_SOUND: &str = "chime";

/// Runs the player, killing it after [`PLAY_TIMEOUT`]. Blocking.
#[cfg(not(target_os = "macos"))]
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
        s.play_rc(
            &RcSound {
                name: "chime".into(),
                volume: 100,
            },
            false,
        );
        s.play_theme("phone-incoming-call");
        assert!(s.resolved.get().is_none());
    }

    /// A sound Rustket does not bundle plays the default at the same
    /// volume (not the platform's sound at full volume).
    #[test]
    fn unbundled_sounds_play_the_default_at_their_volume() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            prepared(d.path(), "telephone", 10).unwrap(),
            rc::cache_file(d.path(), DEFAULT_SOUND, 10)
        );
        assert_eq!(
            prepared(d.path(), "ding", 10).unwrap(),
            rc::cache_file(d.path(), "ding", 10)
        );
    }

    #[test]
    fn path_lookup_skips_relative_dirs_and_missing_files() {
        let d = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([std::path::Path::new("rel"), d.path()]).unwrap();
        assert!(find_in_path("nope", &path).is_none());
    }
}
