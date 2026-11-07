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

/// A file name safe to create in a directory (no separators, no leading dots).
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
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

/// Where audio/video is stored before handing it to the system player.
pub fn media_path(mid: &str, name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("rustket-media")
        .join(safe_file_name(mid))
        .join(safe_file_name(name))
}

/// Streams `url` into `path` (through a `.part` file), reporting progress at
/// most every 100 ms.
pub fn download(rest: RestClient, url: String, path: PathBuf) -> impl Stream<Item = DlEvent> {
    iced::stream::channel(
        16,
        async move |mut tx: iced::futures::channel::mpsc::Sender<DlEvent>| {
            let result = run(&rest, &url, &path, &mut tx).await;
            let last = match result {
                Ok(()) => DlEvent::Done(path),
                Err(e) => DlEvent::Failed(e),
            };
            let _ = tx.send(last).await;
        },
    )
}

async fn run(
    rest: &RestClient,
    url: &str,
    path: &Path,
    tx: &mut iced::futures::channel::mpsc::Sender<DlEvent>,
) -> Result<(), String> {
    let dl = rest.download(url).await.map_err(|e| e.to_string())?;
    let total = dl.content_length;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = Box::pin(dl.stream());
    let mut received = 0u64;
    let mut last = Instant::now();
    let _ = tx.send(DlEvent::Progress(0, total)).await;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                drop(file);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e.to_string());
            }
        };
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e.to_string());
        }
        received += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            let _ = tx.send(DlEvent::Progress(received, total)).await;
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&part, path)
        .await
        .map_err(|e| e.to_string())
}

/// Opens `path` with the desktop's default application.
pub async fn open_path(path: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || open::that_detached(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
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
    }
}
