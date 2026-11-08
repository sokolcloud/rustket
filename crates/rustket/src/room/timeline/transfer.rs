//! File downloads from messages: save dialog (portal), streaming download
//! with progress, opening files with the system handler.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::futures::{SinkExt, Stream, StreamExt};
use rocketchat::RestClient;
use tokio::io::AsyncWriteExt;

/// Progress events of one download.
#[derive(Debug, Clone)]
pub enum DlEvent {
    Progress(u64, Option<u64>),
    Done(PathBuf),
    Failed(String),
}

/// Invisible formatting characters: bidi marks, embeddings, overrides and
/// isolates (which can show `invoice\u{202E}fdp.sh` as `invoicehs.pdf`),
/// zero-width characters and every other Unicode format (Cf) character,
/// plus the line and paragraph separators (Zl, Zp), U+180E, U+2065 and the
/// whole tag block (including its unassigned code points).
fn is_format_char(c: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    matches!(
        c.general_category(),
        GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    ) || matches!(c, '\u{180E}' | '\u{2065}' | '\u{E0000}'..='\u{E007F}')
}

/// A file name safe to create in a directory and to show in a file dialog:
/// no separators, no leading dots, no control or invisible formatting
/// characters.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    if cleaned.is_empty() {
        "download".to_owned()
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// Asks where to save `name` (xdg-desktop-portal through `rfd`).
pub async fn ask_save_path(name: String) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_file_name(safe_file_name(&name))
        .save_file()
        .await
        .map(|h| h.path().to_path_buf())
}

/// File extension for media handed to the system player/viewer, from an
/// allow-list of MIME types (the sender's file name is never used: it
/// decides which application opens the file).
pub fn media_ext(mime: &str) -> Option<&'static str> {
    let mime = mime.split(';').next().unwrap_or_default().trim();
    Some(match mime.to_ascii_lowercase().as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/webm" => "weba",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/flac" | "audio/x-flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/ogg" => "ogv",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => return None,
    })
}

/// Image extension guessed from a URL path (allow-listed).
pub fn image_ext_from_url(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    media_ext(&format!("image/{ext}"))
}

/// Private per-user directory for media opened with the system player
/// (`<cache>/media`, mode 0700).
pub fn media_dir(images_dir: Option<&Path>) -> Option<PathBuf> {
    let base = images_dir
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            directories::ProjectDirs::from("", "", "rustket").map(|d| d.cache_dir().to_path_buf())
        })?;
    Some(base.join("media"))
}

/// Media files opened with the system player are deleted after this age.
pub const MEDIA_TTL: Duration = Duration::from_secs(24 * 3600);
/// How often a running app deletes stale media ([`MEDIA_TTL`]).
pub const MEDIA_PURGE_INTERVAL: Duration = Duration::from_secs(3600);

/// Deletes the media opened with the system player from `dir` (see
/// [`media_dir`]): everything, including decrypted end-to-end media, or only
/// files older than [`MEDIA_TTL`] when `stale_only`. Run off the UI thread.
pub fn purge_media(dir: &Path, stale_only: bool) {
    if !stale_only {
        let _ = std::fs::remove_dir_all(dir);
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age >= MEDIA_TTL);
        if meta.is_file() && stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// [`purge_media`] for the media directory next to `images_dir`, as a task.
pub fn purge_media_task<M: Send + 'static>(
    images_dir: Option<&Path>,
    stale_only: bool,
    done: M,
) -> iced::Task<M> {
    let Some(dir) = media_dir(images_dir) else {
        return iced::Task::none();
    };
    iced::Task::perform(
        async move {
            let _ = tokio::task::spawn_blocking(move || purge_media(&dir, stale_only)).await;
        },
        move |()| done,
    )
}

/// A fresh random file name with extension `ext` in `dir`.
pub fn media_path(dir: &Path, ext: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    dir.join(format!("{:016x}.{ext}", h.finish()))
}

async fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

/// Where a download goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A path the user picked: written through a `.part` file.
    Chosen,
    /// A new private media file: created exclusively, never replaced.
    Private,
}

/// Streams `url` into `path`, reporting progress at most every 100 ms.
pub fn download(
    rest: RestClient,
    url: String,
    path: PathBuf,
    target: Target,
) -> impl Stream<Item = DlEvent> {
    iced::stream::channel(
        16,
        async move |mut tx: iced::futures::channel::mpsc::Sender<DlEvent>| {
            let result = run(&rest, &url, &path, target, &mut tx).await;
            let last = match result {
                Ok(()) => DlEvent::Done(path),
                Err(e) => DlEvent::Failed(e),
            };
            let _ = tx.send(last).await;
        },
    )
}

/// Category of a failed download (shown localised).
pub fn error_text(e: &DlError) -> String {
    use std::io::ErrorKind;
    let key = match e {
        DlError::Network => "room_timeline.dl_error_network",
        DlError::Io(ErrorKind::PermissionDenied) => "room_timeline.dl_error_permission",
        DlError::Io(ErrorKind::StorageFull | ErrorKind::QuotaExceeded) => {
            "room_timeline.dl_error_disk_full"
        }
        DlError::Io(_) => "room_timeline.dl_error_disk",
    };
    rust_i18n::t!(key).into_owned()
}

#[derive(Debug)]
pub enum DlError {
    Network,
    Io(std::io::ErrorKind),
}

fn io(e: std::io::Error) -> String {
    tracing::debug!(error = %e, "download: file error");
    error_text(&DlError::Io(e.kind()))
}

fn net(e: impl std::fmt::Display) -> String {
    tracing::debug!(error = %e, "download: transfer error");
    error_text(&DlError::Network)
}

async fn run(
    rest: &RestClient,
    url: &str,
    path: &Path,
    target: Target,
    tx: &mut iced::futures::channel::mpsc::Sender<DlEvent>,
) -> Result<(), String> {
    // End-to-end encrypted files are decrypted while they are written.
    let mut e2e = crate::e2e::files::Decryptor::for_url(url);
    if e2e.is_none() && crate::e2e::files::is_marked(url) {
        // Never save the ciphertext as if it were the file.
        return Err(crate::e2e::files::key_missing_text());
    }
    let url = crate::e2e::files::strip(url).unwrap_or(url);
    let dl = rest.download(url).await.map_err(net)?;
    let total = dl.content_length;
    let write_path = match target {
        Target::Chosen => {
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir).await.map_err(io)?;
            }
            let mut part = path.as_os_str().to_owned();
            part.push(".part");
            PathBuf::from(part)
        }
        Target::Private => {
            if let Some(dir) = path.parent() {
                create_private_dir(dir).await.map_err(io)?;
            }
            path.to_path_buf()
        }
    };
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true);
    match target {
        Target::Chosen => {
            opts.create(true).truncate(true);
        }
        Target::Private => {
            opts.create_new(true);
            #[cfg(unix)]
            opts.mode(0o600);
        }
    }
    let mut file = opts.open(&write_path).await.map_err(io)?;
    let mut stream = Box::pin(dl.stream());
    let mut received = 0u64;
    let mut last = Instant::now();
    let _ = tx.send(DlEvent::Progress(0, total)).await;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(net(e));
            }
        };
        let chunk = match e2e.as_mut() {
            Some(d) => d.apply(&chunk).into(),
            None => chunk,
        };
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&write_path).await;
            return Err(io(e));
        }
        received += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            let _ = tx.send(DlEvent::Progress(received, total)).await;
        }
    }
    file.flush().await.map_err(io)?;
    drop(file);
    if let Some(d) = e2e
        && !d.finish()
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(crate::e2e::files::hash_mismatch_text());
    }
    if write_path != path {
        tokio::fs::rename(&write_path, path).await.map_err(io)?;
    }
    Ok(())
}

/// Opens `path` with the desktop's default application.
pub async fn open_path(path: PathBuf) -> Result<(), String> {
    let quiet = |e: &dyn std::fmt::Display| {
        tracing::debug!(error = %e, "opening a file failed");
        String::new()
    };
    tokio::task::spawn_blocking(move || open::that_detached(&path).map_err(|e| quiet(&e)))
        .await
        .map_err(|e| quiet(&e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(safe_file_name("report.pdf"), "report.pdf");
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name("  "), "download");
        assert_eq!(safe_file_name("a\nb"), "a_b");
        // Bidi overrides would show `invoice\u{202E}fdp.desktop` as a PDF.
        assert_eq!(
            safe_file_name("invoice\u{202E}fdp.desktop"),
            "invoicefdp.desktop"
        );
        for c in [
            '\u{202A}',
            '\u{202B}',
            '\u{202C}',
            '\u{202D}',
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{200E}',
            '\u{200F}',
            '\u{061C}',
            '\u{200B}',
            '\u{FEFF}',
            '\u{E0041}',
            '\u{2028}',
            '\u{2029}',
            '\u{0600}',
            '\u{06DD}',
            '\u{070F}',
            '\u{08E2}',
            '\u{110BD}',
            '\u{13430}',
            '\u{1BCA0}',
            '\u{1D173}',
            '\u{00AD}',
        ] {
            assert_eq!(safe_file_name(&format!("a{c}b.txt")), "ab.txt", "{c:?}");
        }
        assert_eq!(safe_file_name("\u{202E}.\u{202E}hidden"), "hidden");
        assert_eq!(safe_file_name("отчёт.pdf"), "отчёт.pdf");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn media_purge() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("media");
        std::fs::create_dir_all(dir.join("e2e")).unwrap();
        let old = dir.join("old.mp4");
        let fresh = dir.join("fresh.mp4");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(dir.join("e2e").join("d.mp4"), b"x").unwrap();
        let past = std::time::SystemTime::now() - MEDIA_TTL - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        purge_media(&dir, true);
        assert!(!old.exists());
        assert!(fresh.exists());
        purge_media(&dir, false);
        assert!(!dir.exists());
    }

    #[test]
    fn media_types_are_allow_listed() {
        assert_eq!(media_ext("video/mp4"), Some("mp4"));
        assert_eq!(media_ext("Audio/OGG; codecs=opus"), Some("ogg"));
        assert_eq!(media_ext("application/java-archive"), None);
        assert_eq!(media_ext("application/x-desktop"), None);
        assert_eq!(media_ext("text/html"), None);
        assert_eq!(
            image_ext_from_url("https://x/a/b.PNG?rc_uid=1"),
            Some("png")
        );
        assert_eq!(image_ext_from_url("https://x/a/b.svg"), None);
        let dir = Path::new("/cache/media");
        let a = media_path(dir, "mp4");
        assert_eq!(a.parent(), Some(dir));
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert_ne!(a, media_path(dir, "mp4"));
        assert_eq!(
            media_dir(Some(Path::new("/c/rustket/images"))),
            Some(PathBuf::from("/c/rustket/media"))
        );
    }
}
