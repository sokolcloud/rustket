//! Notification sounds through an external player (`pw-play`, `paplay` or
//! `canberra-gtk-play`, whichever is on `PATH`) and the freedesktop sound
//! theme. Used when the notification server does not play the notification's
//! sound hint itself. Playing never blocks: the player runs detached and is
//! reaped (or killed after a timeout) on a blocking thread.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
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

/// The sound file of `id` in the freedesktop theme (`freedesktop/stereo`).
pub fn theme_file(dirs: &[PathBuf], id: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|d| {
        ["oga", "ogg", "wav"]
            .iter()
            .map(|ext| {
                d.join("sounds/freedesktop/stereo")
                    .join(format!("{id}.{ext}"))
            })
            .find(|p| p.is_file())
    })
}

/// The command playing `id` (`file`: its theme file, needed by all players
/// except canberra, which looks the id up itself).
pub fn command(player: &Player, id: &str, file: Option<&Path>) -> Option<Command> {
    let mut c = Command::new(&player.path);
    match player.kind {
        PlayerKind::Canberra => {
            c.args(["-i", id]);
        }
        PlayerKind::PwPlay | PlayerKind::Paplay => {
            c.arg(file?);
        }
    }
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(c)
}

/// Plays sounds; disabled (a no-op) in tests and snapshots.
#[derive(Debug, Default)]
pub struct Sound {
    enabled: bool,
    resolved: OnceLock<Option<(Player, Option<PathBuf>)>>,
}

impl Sound {
    pub fn system() -> Self {
        Self {
            enabled: true,
            resolved: OnceLock::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Plays the message sound (the server's custom sound names map to the
    /// theme's instant-message sound). Must be called inside the tokio runtime.
    pub fn play(&self, name: Option<&str>) {
        let _ = name;
        if !self.enabled {
            return;
        }
        let resolved = self.resolved.get_or_init(|| {
            let path = std::env::var_os("PATH")?;
            let player = find_player(&path)?;
            let dirs = data_dirs(
                std::env::var_os("XDG_DATA_HOME"),
                std::env::var_os("XDG_DATA_DIRS"),
            );
            Some((player, theme_file(&dirs, MESSAGE_SOUND)))
        });
        let Some((player, file)) = resolved else {
            tracing::debug!("no sound player found");
            return;
        };
        let Some(mut cmd) = command(player, MESSAGE_SOUND, file.as_deref()) else {
            tracing::debug!("no sound theme file found");
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn_blocking(move || {
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
    fn disabled_sound_is_a_no_op() {
        let s = Sound::default();
        assert!(!s.is_enabled());
        s.play(None);
        assert!(s.resolved.get().is_none());
    }
}
