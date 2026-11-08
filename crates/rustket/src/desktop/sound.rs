//! Notification sounds through an external player (`pw-play`, `paplay` or
//! `canberra-gtk-play`, whichever is on `PATH`) and a sound theme file
//! (freedesktop, KDE's ocean, Yaru). Used when the notification server does
//! not play the notification's sound hint itself. The player and file are
//! looked up once on a background thread at startup; playing never blocks:
//! the player runs detached and is reaped (or killed after a timeout) on a
//! blocking thread.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Freedesktop sound id of an incoming instant message.
pub const MESSAGE_SOUND: &str = "message-new-instant";
/// A player taking longer than this is killed.
const PLAY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    PwPlay,
    Paplay,
    Canberra,
}

impl PlayerKind {
    pub const ALL: [PlayerKind; 3] = [Self::PwPlay, Self::Paplay, Self::Canberra];

    pub fn program(self) -> &'static str {
        match self {
            Self::PwPlay => "pw-play",
            Self::Paplay => "paplay",
            Self::Canberra => "canberra-gtk-play",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub kind: PlayerKind,
    pub path: PathBuf,
}

/// The first executable `name` in the `PATH`-style list `path`.
pub fn find_in_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The first available player.
pub fn find_player(path: &std::ffi::OsStr) -> Option<Player> {
    PlayerKind::ALL
        .iter()
        .find_map(|k| find_in_path(k.program(), path).map(|p| Player { kind: *k, path: p }))
}

/// A player and the file it plays: file players need a theme file;
/// canberra (which resolves the configured theme by id) is the fallback.
pub fn resolve(path: &std::ffi::OsStr, file: Option<PathBuf>) -> Option<(Player, Option<PathBuf>)> {
    let found =
        |k: PlayerKind| find_in_path(k.program(), path).map(|p| Player { kind: k, path: p });
    if let Some(file) = file
        && let Some(p) = [PlayerKind::PwPlay, PlayerKind::Paplay, PlayerKind::Canberra]
            .into_iter()
            .find_map(found)
    {
        return Some((p, Some(file)));
    }
    found(PlayerKind::Canberra).map(|p| (p, None))
}

/// `$XDG_DATA_HOME` followed by `$XDG_DATA_DIRS` (with the spec defaults).
pub fn data_dirs(data_home: Option<OsString>, data_dirs: Option<OsString>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match data_home.filter(|v| !v.is_empty()) {
        Some(h) => out.push(PathBuf::from(h)),
        None => {
            if let Some(d) = directories::BaseDirs::new() {
                out.push(d.data_local_dir().to_owned());
            }
        }
    }
    let dirs = data_dirs
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
    out.extend(std::env::split_paths(&dirs).filter(|p| p.is_absolute()));
    out
}

/// Sound themes searched for the message sound, in order.
pub const THEMES: [&str; 3] = ["freedesktop", "ocean", "Yaru"];

/// The sound file of `id` in the first theme of [`THEMES`] that has it.
pub fn theme_file(dirs: &[PathBuf], id: &str) -> Option<PathBuf> {
    THEMES.iter().find_map(|theme| {
        dirs.iter().find_map(|d| {
            ["oga", "ogg", "wav"]
                .iter()
                .map(|ext| {
                    d.join("sounds")
                        .join(theme)
                        .join("stereo")
                        .join(format!("{id}.{ext}"))
                })
                .find(|p| p.is_file())
        })
    })
}

/// The command playing `id` (`file`: its theme file, needed by all players
/// except canberra, which looks the id up itself).
pub fn command(player: &Player, id: &str, file: Option<&Path>) -> Option<Command> {
    let mut c = Command::new(&player.path);
    match (player.kind, file) {
        (PlayerKind::Canberra, Some(f)) => {
            c.arg("-f").arg(f);
        }
        (PlayerKind::Canberra, None) => {
            c.args(["-i", id]);
        }
        (PlayerKind::PwPlay | PlayerKind::Paplay, f) => {
            c.arg(f?);
        }
    }
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(c)
}

type Resolved = Option<(Player, Option<PathBuf>)>;

/// Plays sounds; disabled (a no-op) in tests and snapshots.
#[derive(Debug, Default)]
pub struct Sound {
    enabled: bool,
    /// Filled by the background lookup.
    resolved: Arc<OnceLock<Resolved>>,
}

impl Sound {
    /// Starts looking the player and sound file up in the background.
    pub fn system() -> Self {
        let resolved = Arc::new(OnceLock::new());
        let slot = resolved.clone();
        let spawned = std::thread::Builder::new()
            .name("sound-lookup".into())
            .spawn(move || {
                let file = theme_file(
                    &data_dirs(
                        std::env::var_os("XDG_DATA_HOME"),
                        std::env::var_os("XDG_DATA_DIRS"),
                    ),
                    MESSAGE_SOUND,
                );
                let r = std::env::var_os("PATH").and_then(|p| resolve(&p, file));
                let _ = slot.set(r);
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
    /// theme's instant-message sound). Must be called inside the tokio runtime.
    pub fn play(&self, name: Option<&str>) {
        let _ = name;
        self.play_id(None);
    }

    /// Plays the freedesktop theme sound `id` (e.g. `phone-incoming-call`),
    /// falling back to the message sound when the theme lacks it.
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
        let Some((player, file)) = resolved else {
            tracing::debug!("no sound player or sound file found");
            return;
        };
        let (player, file) = (player.clone(), file.clone());
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn_blocking(move || {
            let themed = id.and_then(|id| {
                let f = theme_file(
                    &data_dirs(
                        std::env::var_os("XDG_DATA_HOME"),
                        std::env::var_os("XDG_DATA_DIRS"),
                    ),
                    id,
                );
                match (f, player.kind) {
                    (Some(f), _) => command(&player, id, Some(&f)),
                    (None, PlayerKind::Canberra) => command(&player, id, None),
                    (None, _) => None,
                }
            });
            let Some(mut cmd) = themed.or_else(|| command(&player, MESSAGE_SOUND, file.as_deref()))
            else {
                tracing::debug!("no sound theme file found");
                return;
            };
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
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn player_lookup_prefers_pw_play() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let paplay = exe(a.path(), "paplay");
        // Not executable: skipped.
        std::fs::write(b.path().join("pw-play"), "").unwrap();
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        let p = find_player(&path).unwrap();
        assert_eq!((p.kind, p.path), (PlayerKind::Paplay, paplay));
        let pw = exe(b.path(), "pw-play");
        std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_player(&path).unwrap().kind, PlayerKind::PwPlay);
        assert!(find_player(std::ffi::OsStr::new("relative/dir")).is_none());
    }

    #[test]
    fn theme_file_and_commands() {
        let d = tempfile::tempdir().unwrap();
        let stereo = d.path().join("share/sounds/freedesktop/stereo");
        std::fs::create_dir_all(&stereo).unwrap();
        std::fs::write(stereo.join("message-new-instant.oga"), "x").unwrap();
        let dirs = data_dirs(
            Some(d.path().join("home").into_os_string()),
            Some(d.path().join("share").into_os_string()),
        );
        assert_eq!(dirs[0], d.path().join("home"));
        let file = theme_file(&dirs, MESSAGE_SOUND).unwrap();
        assert_eq!(file, stereo.join("message-new-instant.oga"));
        assert!(theme_file(&dirs, "nope").is_none());

        let pw = Player {
            kind: PlayerKind::PwPlay,
            path: "/usr/bin/pw-play".into(),
        };
        let c = command(&pw, MESSAGE_SOUND, Some(&file)).unwrap();
        assert_eq!(c.get_args().collect::<Vec<_>>(), [file.as_os_str()]);
        assert!(command(&pw, MESSAGE_SOUND, None).is_none());
        let ocean = d.path().join("share/sounds/ocean/stereo");
        std::fs::create_dir_all(&ocean).unwrap();
        std::fs::write(ocean.join("message-new-email.ogg"), "x").unwrap();
        assert_eq!(
            theme_file(&dirs, "message-new-email").unwrap(),
            ocean.join("message-new-email.ogg")
        );
        let ca = Player {
            kind: PlayerKind::Canberra,
            path: "/usr/bin/canberra-gtk-play".into(),
        };
        let c = command(&ca, MESSAGE_SOUND, None).unwrap();
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["-i", "message-new-instant"]
        );
    }

    #[test]
    fn player_and_file_are_chosen_together() {
        let a = tempfile::tempdir().unwrap();
        let path = a.path().as_os_str().to_owned();
        exe(a.path(), "pw-play");
        // A file player without a file: nothing to play.
        assert!(resolve(&path, None).is_none());
        let f = PathBuf::from("/x/message-new-instant.oga");
        let (p, file) = resolve(&path, Some(f.clone())).unwrap();
        assert_eq!((p.kind, file), (PlayerKind::PwPlay, Some(f)));
        // No theme file: canberra resolves the id itself.
        exe(a.path(), "canberra-gtk-play");
        let (p, file) = resolve(&path, None).unwrap();
        assert_eq!((p.kind, file), (PlayerKind::Canberra, None));
    }

    #[test]
    fn disabled_sound_is_a_no_op() {
        let s = Sound::default();
        assert!(!s.is_enabled());
        s.play(None);
        assert!(s.resolved.get().is_none());
    }
}
