//! macOS sounds: `afplay` with a `/System/Library/Sounds` sound.

use std::path::PathBuf;

pub const SOUNDS_DIR: &str = "/System/Library/Sounds";

/// System sound of a freedesktop sound id (`None`: the message sound).
pub fn system_sound(id: Option<&str>) -> &'static str {
    match id {
        Some("phone-incoming-call") => "Submarine",
        Some("phone-outgoing-calling") => "Purr",
        _ => "Glass",
    }
}

pub fn sound_file(name: &str) -> PathBuf {
    PathBuf::from(SOUNDS_DIR).join(format!("{name}.aiff"))
}

/// The `afplay` binary.
#[cfg(target_os = "macos")]
pub type Resolved = Option<PathBuf>;

#[cfg(target_os = "macos")]
pub fn lookup() -> Resolved {
    std::env::var_os("PATH")
        .and_then(|p| super::find_in_path("afplay", &p))
        .or_else(|| {
            let p = PathBuf::from("/usr/bin/afplay");
            p.is_file().then_some(p)
        })
}

#[cfg(target_os = "macos")]
pub fn play_command(
    resolved: &Resolved,
    id: Option<&'static str>,
) -> Option<std::process::Command> {
    use std::process::{Command, Stdio};
    let Some(afplay) = resolved else {
        tracing::debug!("afplay not found");
        return None;
    };
    let file = [system_sound(id), system_sound(None)]
        .iter()
        .map(|s| sound_file(s))
        .find(|p| p.is_file())?;
    let mut c = Command::new(afplay);
    c.arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_mapping() {
        assert_eq!(system_sound(None), "Glass");
        assert_eq!(system_sound(Some("phone-incoming-call")), "Submarine");
        assert_eq!(
            sound_file("Glass"),
            PathBuf::from("/System/Library/Sounds/Glass.aiff")
        );
    }
}
