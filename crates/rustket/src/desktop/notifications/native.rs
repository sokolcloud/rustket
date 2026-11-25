//! Shared by the Windows and macOS backends: the channel carrying clicks and
//! dismissals from the platform's callback threads to the subscription, and
//! avatar images written to files (both platforms take image paths). Neither
//! leaves images piling up in the temporary directory: Windows keeps them
//! for a few days, macOS removes each one after posting.
// Elsewhere only the tests use it.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use futures::channel::mpsc;

use super::{Message, Signal};

struct Channel {
    tx: mpsc::UnboundedSender<Message>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
}

static CHANNEL: LazyLock<Channel> = LazyLock::new(|| {
    let (tx, rx) = mpsc::unbounded();
    Channel {
        tx,
        rx: Mutex::new(Some(rx)),
    }
});

/// Reports a click, action or dismissal of notification `id`.
pub fn emit(id: u32, signal: Signal) {
    let _ = CHANNEL.tx.unbounded_send(Message::Signal { id, signal });
}

/// The signals (the app-wide subscription runs once).
pub fn signals() -> impl futures::Stream<Item = Message> {
    match CHANNEL.rx.lock().ok().and_then(|mut g| g.take()) {
        Some(rx) => rx.left_stream(),
        None => futures::stream::empty().right_stream(),
    }
}

/// The directory of the avatar images.
fn image_dir() -> PathBuf {
    std::env::temp_dir().join("rustket-notifications")
}

/// Writes RGBA pixels as a PNG to `path` (through a temporary sibling).
/// Blocking.
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Option<()> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let dir = path.parent()?;
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::debug!(error = %e, "cannot create the notification image directory");
        return None;
    }
    let name = path.file_name()?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    match img.save_with_format(&tmp, image::ImageFormat::Png) {
        Ok(()) => std::fs::rename(&tmp, path).ok(),
        Err(e) => {
            tracing::debug!(error = %e, "cannot write the notification image");
            let _ = std::fs::remove_file(&tmp);
            None
        }
    }
}

/// Removes the images in `dir` last modified more than `max_age` ago.
fn prune(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).unwrap_or_default() >= max_age);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How long a Windows toast image is kept: Action Center keeps toasts for
/// up to three days.
#[cfg(windows)]
const KEEP_IMAGES: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// Writes RGBA pixels as a PNG into the temporary directory, named by
/// content, and returns its path (toasts read it while shown). Images older
/// than [`KEEP_IMAGES`] are removed once per launch. Blocking.
#[cfg(windows)]
pub fn image_file(width: u32, height: u32, rgba: &[u8]) -> Option<PathBuf> {
    use sha2::Digest;
    static PRUNED: std::sync::Once = std::sync::Once::new();
    let dir = image_dir();
    PRUNED.call_once(|| prune(&dir, KEEP_IMAGES));
    let hash = hex::encode(&sha2::Sha256::digest(rgba)[..12]);
    let path = dir.join(format!("{hash}-{width}x{height}.png"));
    if path.is_file() {
        return Some(path);
    }
    write_png(&path, width, height, rgba).map(|()| path)
}

/// A PNG of one notification that is removed when dropped (the system
/// moves a `UNNotificationAttachment`'s file into its own store once the
/// request is added; whatever is left is ours to remove).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
pub struct TempImage(PathBuf);

#[cfg(any(target_os = "macos", test))]
impl TempImage {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for TempImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes RGBA pixels as a uniquely named PNG into `dir`. Blocking.
#[cfg(any(target_os = "macos", test))]
fn temp_image_in(dir: &Path, width: u32, height: u32, rgba: &[u8]) -> Option<TempImage> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("{}-{seq}.png", std::process::id()));
    write_png(&path, width, height, rgba).map(|()| TempImage(path))
}

/// Writes RGBA pixels as a PNG for one notification. Blocking.
#[cfg(target_os = "macos")]
pub fn temp_image(width: u32, height: u32, rgba: &[u8]) -> Option<TempImage> {
    temp_image_in(&image_dir(), width, height, rgba)
}

/// Removes every image an earlier launch left behind (macOS: the sandbox
/// container's temporary directory is not cleaned). Blocking.
#[cfg(target_os = "macos")]
pub fn clear_images() {
    prune(&image_dir(), Duration::ZERO);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn temp_images_are_unique_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let px = [255u8, 0, 0, 255];
        let a = temp_image_in(dir.path(), 1, 1, &px).unwrap();
        let b = temp_image_in(dir.path(), 1, 1, &px).unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().is_file() && b.path().is_file());
        let kept = a.path().to_owned();
        drop(a);
        assert!(!kept.exists());
        // Gone already (moved by the system): dropping is quiet.
        std::fs::remove_file(b.path()).unwrap();
        drop(b);
        assert!(
            temp_image_in(dir.path(), 2, 2, &px).is_none(),
            "short buffer"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn prune_removes_only_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.png");
        std::fs::write(&f, b"x").unwrap();
        prune(dir.path(), Duration::from_secs(3600));
        assert!(f.exists());
        prune(dir.path(), Duration::ZERO);
        assert!(!f.exists());
    }
}
