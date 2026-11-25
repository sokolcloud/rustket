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
