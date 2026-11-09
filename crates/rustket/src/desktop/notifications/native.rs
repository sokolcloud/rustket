//! Shared by the Windows and macOS backends: the channel carrying clicks and
//! dismissals from the platform's callback threads to the subscription, and
//! avatar images written to files (both platforms take image paths).

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

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

/// Writes RGBA pixels as a PNG into the temporary directory, named by
/// content, and returns its path. Blocking.
pub fn image_file(width: u32, height: u32, rgba: &[u8]) -> Option<PathBuf> {
    use sha2::Digest;
    let dir = std::env::temp_dir().join("rustket-notifications");
    let hash = hex::encode(&sha2::Sha256::digest(rgba)[..12]);
    let path = dir.join(format!("{hash}-{width}x{height}.png"));
    if path.is_file() {
        return Some(path);
    }
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::debug!(error = %e, "cannot create the notification image directory");
        return None;
    }
    let tmp = dir.join(format!(".{hash}.tmp{}", std::process::id()));
    match img.save_with_format(&tmp, image::ImageFormat::Png) {
        Ok(()) => std::fs::rename(&tmp, &path).ok().map(|()| path),
        Err(e) => {
            tracing::debug!(error = %e, "cannot write the notification image");
            let _ = std::fs::remove_file(&tmp);
            None
        }
    }
}
