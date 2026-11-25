//! Opening files and folders with the desktop's default application. macOS
//! asks NSWorkspace in-process (no `/usr/bin/open` child process, which the
//! App Store build must not need); elsewhere the `open` crate.

use std::path::Path;

/// Opens `path` (a file with its default application, a folder in the file
/// manager). Blocking; the error is for logs only.
pub fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return workspace_open(path);
    #[cfg(not(target_os = "macos"))]
    open::that_detached(path).map_err(|e| e.to_string())
}

/// Shows the file at `path` in the file manager: selected in Finder on
/// macOS, its folder opened elsewhere. Blocking; the error is for logs only.
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{NSArray, NSString, NSURL};
        if !path.exists() {
            return Err("the file is gone".to_owned());
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSWorkspace::sharedWorkspace()
            .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let folder = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "the file has no folder".to_owned())?;
        open_path(folder)
    }
}

/// The bundle id the macOS app is built with (an unbundled build has none).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "io.github.sokolcloud.rustket";

/// The System Settings URL of the app's notification settings: the app's
/// own page on macOS 13 and later, the Notifications pane before.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn notification_settings_url(macos_13: bool, bundle_id: &str) -> String {
    if macos_13 {
        format!(
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={bundle_id}"
        )
    } else {
        "x-apple.systempreferences:com.apple.preference.notifications".to_owned()
    }
}

/// Opens the app's notification settings in System Settings (macOS only).
/// Blocking; the error is for logs only.
pub fn notification_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{
            NSBundle, NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL,
        };
        let macos_13 = NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(
            NSOperatingSystemVersion {
                majorVersion: 13,
                minorVersion: 0,
                patchVersion: 0,
            },
        );
        let bundle_id = NSBundle::mainBundle()
            .bundleIdentifier()
            .map_or_else(|| BUNDLE_ID.to_owned(), |id| id.to_string());
        let url = notification_settings_url(macos_13, &bundle_id);
        let url = NSURL::URLWithString(&NSString::from_str(&url))
            .ok_or_else(|| "invalid System Settings URL".to_owned())?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err("System Settings did not open".to_owned())
        }
    }
    #[cfg(not(target_os = "macos"))]
    Err("no notification settings to open on this platform".to_owned())
}

#[cfg(target_os = "macos")]
fn workspace_open(path: &Path) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let ws = NSWorkspace::sharedWorkspace();
    if ws.openURL(&url) {
        return Ok(());
    }
    // A folder Finder would not open is still shown selected in its parent.
    if path.is_dir() {
        ws.activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
        return Ok(());
    }
    Err("NSWorkspace could not open the file".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_settings_urls() {
        assert_eq!(
            notification_settings_url(true, "io.github.sokolcloud.rustket"),
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=io.github.sokolcloud.rustket"
        );
        assert_eq!(
            notification_settings_url(false, "x"),
            "x-apple.systempreferences:com.apple.preference.notifications"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn reveal_needs_a_folder() {
        assert!(reveal(Path::new("file.pdf")).is_err());
    }
}
