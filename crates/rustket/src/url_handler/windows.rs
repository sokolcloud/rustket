//! Per-user `rocketchat://` registration on Windows:
//!
//! ```text
//! HKCU\Software\Classes\rocketchat
//!     (Default)            = URL:Rocket.Chat Protocol
//!     URL Protocol         = ""
//!     DefaultIcon\(Default) = "<exe>",0
//!     shell\open\command\(Default) = "<exe>" "%1"
//! ```
//!
//! The installer (`packaging/windows`) writes the same keys and values. The
//! effective handler is read from `HKEY_CLASSES_ROOT`, the merged view of the
//! per-user and machine keys. The string logic is platform-neutral so it is
//! tested everywhere.

use std::path::Path;

/// Below `HKEY_CURRENT_USER`.
pub const KEY: &str = r"Software\Classes\rocketchat";
pub const PROTOCOL_NAME: &str = "URL:Rocket.Chat Protocol";
/// Marks the class as a URL scheme; its value is empty.
pub const URL_PROTOCOL: &str = "URL Protocol";
pub const ICON_KEY: &str = "DefaultIcon";
pub const COMMAND_KEY: &str = r"shell\open\command";

/// `DefaultIcon` value: the executable's first icon.
pub fn icon_value(exe: &Path) -> String {
    format!("\"{}\",0", exe.display())
}

/// `shell\open\command` value.
pub fn command_value(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// The program a command value starts: the quoted first token, or everything
/// up to the first space.
pub fn command_program(command: &str) -> Option<String> {
    let command = command.trim_start();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split_once('"')?.0,
        None => command.split_whitespace().next()?,
    };
    (!program.is_empty()).then(|| program.to_owned())
}

/// For comparisons: no `\\?\` prefix, backslashes, lower case.
fn normalize(path: &str) -> String {
    let path = path.trim();
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    path.replace('/', "\\").to_lowercase()
}

/// Whether the registered `program` is `exe` (case-insensitive, normalized;
/// also after resolving both when they exist).
pub fn same_program(program: &str, exe: &Path) -> bool {
    if normalize(program) == normalize(&exe.to_string_lossy()) {
        return true;
    }
    match (Path::new(program).canonicalize(), exe.canonicalize()) {
        (Ok(a), Ok(b)) => normalize(&a.to_string_lossy()) == normalize(&b.to_string_lossy()),
        _ => false,
    }
}

/// Whether a registered program (from [`query`]) is a Rustket executable.
pub fn is_ours(program: &str) -> bool {
    let normalized = normalize(program);
    let name = normalized.rsplit('\\').next().unwrap_or_default();
    name == "rustket.exe" || name == "rustket"
}

/// The registered program and whether it is `exe`.
pub fn status(command: Option<&str>, exe: Option<&Path>) -> (Option<String>, bool) {
    let program = command.and_then(command_program);
    let registered = match (&program, exe) {
        (Some(p), Some(exe)) => same_program(p, exe),
        _ => false,
    };
    (program, registered)
}

/// The current handler and whether it is `exe` (blocking).
#[cfg(windows)]
pub fn query(exe: Option<&Path>) -> (Option<String>, bool) {
    let command = windows_registry::CLASSES_ROOT
        .open(format!(r"rocketchat\{COMMAND_KEY}"))
        .and_then(|k| k.get_string(""))
        .ok();
    status(command.as_deref(), exe)
}

/// Writes the per-user keys for `exe`.
#[cfg(windows)]
pub fn register(exe: &Path) -> Result<(), super::RegisterError> {
    let write = || -> windows_registry::Result<()> {
        let root = windows_registry::CURRENT_USER.create(KEY)?;
        root.set_string("", PROTOCOL_NAME)?;
        root.set_string(URL_PROTOCOL, "")?;
        root.create(ICON_KEY)?.set_string("", icon_value(exe))?;
        root.create(COMMAND_KEY)?.set_string("", command_value(exe))
    };
    write().map_err(|e| super::RegisterError::Registry(std::io::Error::other(e.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let exe = Path::new(r"C:\Users\me\AppData\Local\Programs\Rustket\rustket.exe");
        assert_eq!(
            command_value(exe),
            r#""C:\Users\me\AppData\Local\Programs\Rustket\rustket.exe" "%1""#
        );
        assert_eq!(
            icon_value(exe),
            r#""C:\Users\me\AppData\Local\Programs\Rustket\rustket.exe",0"#
        );
        assert_eq!(
            command_program(&command_value(exe)).as_deref(),
            Some(r"C:\Users\me\AppData\Local\Programs\Rustket\rustket.exe")
        );
    }

    #[test]
    fn programs() {
        assert_eq!(
            command_program(r#""C:\Program Files\Rocket.Chat\Rocket.Chat.exe" "%1""#).as_deref(),
            Some(r"C:\Program Files\Rocket.Chat\Rocket.Chat.exe")
        );
        assert_eq!(
            command_program(r"C:\rc\rc.exe %1").as_deref(),
            Some(r"C:\rc\rc.exe")
        );
        assert_eq!(command_program(r#""unterminated"#), None);
        assert_eq!(command_program(""), None);
        assert_eq!(command_program(r#""" "%1""#), None);

        let exe = Path::new(r"C:\Apps\Rustket\rustket.exe");
        assert!(same_program(r"c:\apps\rustket\RUSTKET.EXE", exe));
        assert!(same_program(r"\\?\C:\Apps\Rustket\rustket.exe", exe));
        assert!(same_program("C:/Apps/Rustket/rustket.exe", exe));
        assert!(!same_program(r"C:\Other\rustket.exe", exe));

        assert!(is_ours(r"C:\Apps\Rustket\Rustket.exe"));
        assert!(!is_ours(r"C:\Program Files\Rocket.Chat\Rocket.Chat.exe"));

        let ours = r#""C:\Apps\Rustket\rustket.exe" "%1""#;
        assert_eq!(
            status(Some(ours), Some(exe)),
            (Some(r"C:\Apps\Rustket\rustket.exe".to_owned()), true)
        );
        assert!(!status(Some(ours), Some(Path::new(r"D:\rustket.exe"))).1);
        assert_eq!(status(None, Some(exe)), (None, false));
        assert!(!status(Some(ours), None).1);
    }
}
