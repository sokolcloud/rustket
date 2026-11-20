//! Freedesktop sounds: an external player (`pw-play`, `paplay` or
//! `canberra-gtk-play`, whichever is on `PATH`) and a sound theme file
//! (freedesktop, KDE's ocean, Yaru), or a sound file (Rocket.Chat's sounds).
//! [`quiet`] tells whether the desktop's Do Not Disturb is on.

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
        && let Some(p) = PlayerKind::ALL.into_iter().find_map(found)
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

/// The players found at startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// The theme sound player and the message sound's theme file.
    theme: Option<(Player, Option<PathBuf>)>,
    /// The player of sound files (Rocket.Chat's sounds).
    file: Option<Player>,
}

/// Looks the players and the message sound file up (blocking).
pub fn lookup() -> Resolved {
    let file = theme_file(
        &data_dirs(
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("XDG_DATA_DIRS"),
        ),
        MESSAGE_SOUND,
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    Resolved {
        theme: resolve(&path, file),
        file: find_player(&path),
    }
}

/// The command playing the sound file `file`.
pub fn file_command(resolved: &Resolved, file: &Path) -> Option<Command> {
    let Some(player) = &resolved.file else {
        tracing::debug!("no sound player found");
        return None;
    };
    command(player, "", Some(file))
}

/// The command playing theme sound `id` (the message sound when `None` or
/// when the theme lacks it). Blocking.
pub fn play_command(resolved: &Resolved, id: Option<&'static str>) -> Option<Command> {
    let Some((player, file)) = &resolved.theme else {
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

/// How long the Do Not Disturb check may take before the sound plays.
const QUIET_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);

/// The desktop's Do Not Disturb is on: the notification server's
/// `Inhibited` property (KDE Plasma) or, on GNOME, `show-banners` off.
/// Absent, failing or slow (beyond [`QUIET_TIMEOUT`]) means off.
pub async fn quiet() -> bool {
    let check = async {
        if inhibited().await {
            return true;
        }
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        if !is_gnome(&desktop) {
            return false;
        }
        let Some(gsettings) = std::env::var_os("PATH").and_then(|p| find_in_path("gsettings", &p))
        else {
            return false;
        };
        tokio::task::spawn_blocking(move || banners_off(gsettings_banners(&gsettings).as_deref()))
            .await
            .unwrap_or(false)
    };
    tokio::time::timeout(QUIET_TIMEOUT, check)
        .await
        .unwrap_or(false)
}

/// `org.freedesktop.Notifications`' `Inhibited` property (KDE Plasma).
/// Asked only of a running server, so an activatable daemon is not
/// started by the question.
async fn inhibited() -> bool {
    const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
    let running = super::super::dbus::call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        &(NOTIFICATIONS,),
    )
    .await
    .and_then(|r| r.body().deserialize::<bool>().ok())
    .unwrap_or(false);
    if !running {
        return false;
    }
    let Some(reply) = super::super::dbus::call(
        NOTIFICATIONS,
        "/org/freedesktop/Notifications",
        "org.freedesktop.DBus.Properties",
        "Get",
        &(NOTIFICATIONS, "Inhibited"),
    )
    .await
    else {
        return false;
    };
    let body = reply.body();
    body.deserialize::<zbus::zvariant::Value<'_>>()
        .ok()
        .and_then(|v| variant_bool(&v))
        .unwrap_or(false)
}

fn variant_bool(v: &zbus::zvariant::Value<'_>) -> Option<bool> {
    match v {
        zbus::zvariant::Value::Bool(b) => Some(*b),
        zbus::zvariant::Value::Value(inner) => variant_bool(inner),
        _ => None,
    }
}

/// `XDG_CURRENT_DESKTOP` (a `:`-separated list) names GNOME.
fn is_gnome(desktop: &str) -> bool {
    desktop.split(':').any(|d| d.eq_ignore_ascii_case("gnome"))
}

/// `gsettings get org.gnome.desktop.notifications show-banners` printed
/// `false`.
fn banners_off(output: Option<&str>) -> bool {
    output.is_some_and(|o| o.trim() == "false")
}

/// The output of `gsettings get org.gnome.desktop.notifications
/// show-banners`, killed after [`QUIET_TIMEOUT`]. Blocking.
fn gsettings_banners(gsettings: &Path) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new(gsettings)
        .args(["get", "org.gnome.desktop.notifications", "show-banners"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if started.elapsed() < QUIET_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(Some(_)) | Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn gnome_do_not_disturb_from_gsettings() {
        assert!(is_gnome("GNOME") && is_gnome("ubuntu:GNOME") && is_gnome("gnome"));
        assert!(!is_gnome("KDE") && !is_gnome("GNOME-Flashback-x") && !is_gnome(""));
        assert!(banners_off(Some("false\n")));
        assert!(!banners_off(Some("true\n")));
        assert!(!banners_off(Some("")));
        assert!(!banners_off(None));
        use zbus::zvariant::Value;
        assert_eq!(variant_bool(&Value::Bool(true)), Some(true));
        assert_eq!(
            variant_bool(&Value::Value(Box::new(Value::Bool(false)))),
            Some(false)
        );
        assert_eq!(variant_bool(&Value::from("x")), None);
    }

    #[test]
    fn gsettings_output_is_read() {
        let echo = Path::new("/bin/echo");
        if echo.is_file() {
            let out = gsettings_banners(echo).unwrap();
            assert_eq!(
                out.trim(),
                "get org.gnome.desktop.notifications show-banners"
            );
        }
        let fail = Path::new("/bin/false");
        if fail.is_file() {
            assert_eq!(gsettings_banners(fail), None);
        }
    }

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
    fn sound_files_play_without_a_theme() {
        let pw = Player {
            kind: PlayerKind::PwPlay,
            path: "/usr/bin/pw-play".into(),
        };
        let r = Resolved {
            theme: None,
            file: Some(pw),
        };
        let f = Path::new("/cache/chime-100.wav");
        let c = file_command(&r, f).unwrap();
        assert_eq!(c.get_args().collect::<Vec<_>>(), [f.as_os_str()]);
        // No theme: no fallback sound.
        assert!(play_command(&r, None).is_none());
        let ca = Resolved {
            theme: None,
            file: Some(Player {
                kind: PlayerKind::Canberra,
                path: "/usr/bin/canberra-gtk-play".into(),
            }),
        };
        let c = file_command(&ca, f).unwrap();
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["-f", "/cache/chime-100.wav"]
        );
        assert!(file_command(&Resolved::default(), f).is_none());
    }
}
