//! Windows sounds: `%SystemRoot%\Media` WAV files and Rocket.Chat's sounds
//! (cached WAV files) played by PowerShell's `System.Media.SoundPlayer`,
//! started without a console window.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

/// Media file of a freedesktop sound id (`None`: the message sound).
pub fn media_file(id: Option<&str>) -> &'static str {
    match id {
        Some("phone-incoming-call") => "Windows Ringin.wav",
        Some("phone-outgoing-calling") => "Windows Ringout.wav",
        _ => "Windows Notify Messaging.wav",
    }
}

/// PowerShell command playing `file` to its end. Every character
/// PowerShell takes for a single quote (ASCII and U+2018..U+201B) is
/// doubled, as its `EscapeSingleQuotedStringContent` does.
pub fn script(file: &Path) -> String {
    let mut quoted = String::new();
    for c in file.to_string_lossy().chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    format!("(New-Object System.Media.SoundPlayer '{quoted}').PlaySync()")
}

/// PowerShell and the media directory.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct Setup {
    powershell: PathBuf,
    media: PathBuf,
}

#[cfg(windows)]
pub type Resolved = Option<Setup>;

#[cfg(windows)]
pub fn lookup() -> Resolved {
    let root = std::env::var_os("SystemRoot")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let powershell = std::env::var_os("PATH")
        .and_then(|p| super::find_in_path("powershell", &p))
        .or_else(|| {
            let p = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
            p.is_file().then_some(p)
        })?;
    Some(Setup {
        powershell,
        media: root.join("Media"),
    })
}

#[cfg(windows)]
pub fn play_command(
    resolved: &Resolved,
    id: Option<&'static str>,
) -> Option<std::process::Command> {
    let Some(setup) = resolved else {
        tracing::debug!("PowerShell not found");
        return None;
    };
    let file = [media_file(id), media_file(None)]
        .iter()
        .map(|f| setup.media.join(f))
        .find(|p| p.is_file())?;
    file_command(resolved, &file)
}

/// Runs a player command to its end (or the timeout). Blocking.
#[cfg(windows)]
pub fn run(cmd: std::process::Command) {
    super::run_reaped(cmd);
}

/// The command playing the WAV file `file`.
#[cfg(windows)]
pub fn file_command(resolved: &Resolved, file: &Path) -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(setup) = resolved else {
        tracing::debug!("PowerShell not found");
        return None;
    };
    let mut c = Command::new(&setup.powershell);
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
    ])
    .arg(script(file))
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .creation_flags(CREATE_NO_WINDOW);
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_and_script() {
        assert_eq!(media_file(None), "Windows Notify Messaging.wav");
        assert_eq!(
            media_file(Some("phone-incoming-call")),
            "Windows Ringin.wav"
        );
        assert_eq!(media_file(Some("bell")), media_file(None));
        assert_eq!(
            script(Path::new(r"C:\Win dows\Media\it's.wav")),
            r"(New-Object System.Media.SoundPlayer 'C:\Win dows\Media\it''s.wav').PlaySync()"
        );
        // Typographic quotes end a PowerShell string too.
        assert_eq!(
            script(Path::new(
                "C:\\Users\\D\u{2019}Angelo\u{2018}\u{201A}\u{201B}\\a.wav"
            )),
            "(New-Object System.Media.SoundPlayer \
             'C:\\Users\\D\u{2019}\u{2019}Angelo\u{2018}\u{2018}\u{201A}\u{201A}\u{201B}\u{201B}\\a.wav').PlaySync()"
        );
    }
}
