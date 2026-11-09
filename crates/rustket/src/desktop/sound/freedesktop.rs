//! Freedesktop sounds: an external player (`pw-play`, `paplay` or
//! `canberra-gtk-play`, whichever is on `PATH`) and a sound theme file
//! (freedesktop, KDE's ocean, Yaru).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{MESSAGE_SOUND, find_in_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    PwPlay,
    Paplay,
    Canberra,
}

impl PlayerKind {
    #[cfg(test)]
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

/// The first available player.
#[cfg(test)]
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

pub type Resolved = Option<(Player, Option<PathBuf>)>;

/// Looks the player and message sound file up (blocking).
pub fn lookup() -> Resolved {
    let file = theme_file(
        &data_dirs(
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("XDG_DATA_DIRS"),
        ),
        MESSAGE_SOUND,
    );
    std::env::var_os("PATH").and_then(|p| resolve(&p, file))
}

/// The command playing theme sound `id` (the message sound when `None` or
/// when the theme lacks it). Blocking.
pub fn play_command(resolved: &Resolved, id: Option<&'static str>) -> Option<Command> {
    let Some((player, file)) = resolved else {
        tracing::debug!("no sound player or sound file found");
        return None;
    };
    let themed = id.and_then(|id| {
        let f = theme_file(
            &data_dirs(
                std::env::var_os("XDG_DATA_HOME"),
                std::env::var_os("XDG_DATA_DIRS"),
            ),
            id,
        );
        match (f, player.kind) {
            (Some(f), _) => command(player, id, Some(&f)),
            (None, PlayerKind::Canberra) => command(player, id, None),
            (None, _) => None,
        }
    });
    themed.or_else(|| command(player, MESSAGE_SOUND, file.as_deref()))
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
}
