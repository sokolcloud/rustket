//! Streaming download of a release asset, verified against the release's
//! `SHA256SUMS` before it is handed to the installer.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::futures::{SinkExt, Stream};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::UpdateError;
use super::assets::{lookup, parse_sums};
use super::github::{Asset, Endpoints, Release, network, valid_download_url};

/// Larger packages are refused before the download starts.
pub const MAX_ASSET_SIZE: u64 = 512 * 1024 * 1024;
const SUMS_MAX: u64 = 64 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub enum DlEvent {
    Progress {
        received: u64,
        total: u64,
    },
    /// The verified file and its SHA-256.
    Done(Result<(PathBuf, [u8; 32]), UpdateError>),
}

/// Downloads `asset` of `release` into `dir` (`<cache>/updates/<version>/`).
/// The last event is always `Done`.
pub fn download(
    http: reqwest::Client,
    release: Release,
    asset: Asset,
    dir: PathBuf,
) -> impl Stream<Item = DlEvent> {
    iced::stream::channel(
        16,
        async move |mut tx: iced::futures::channel::mpsc::Sender<DlEvent>| {
            let mut progress_tx = tx.clone();
            let total = asset.size;
            let mut progress = move |received: u64| {
                // A full channel only drops an intermediate progress update.
                let _ = progress_tx.try_send(DlEvent::Progress { received, total });
            };
            let ep = Endpoints::current();
            let result = run(&http, &ep, &release, &asset, &dir, &mut progress).await;
            if let Err(e) = &result {
                tracing::info!("updater: download of {} failed: {e}", asset.name);
            }
            let _ = tx.send(DlEvent::Done(result)).await;
        },
    )
}

/// The steps of [`download`]: checksum lookup, cache hit or a verified
/// download through a `.part` file.
pub(crate) async fn run(
    http: &reqwest::Client,
    ep: &Endpoints,
    release: &Release,
    asset: &Asset,
    dir: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<(PathBuf, [u8; 32]), UpdateError> {
    if !safe_file_name(&asset.name) {
        return Err(UpdateError::BadRelease(format!(
            "asset name {:?}",
            asset.name
        )));
    }
    if asset.size == 0 || asset.size > MAX_ASSET_SIZE {
        return Err(UpdateError::BadRelease(format!(
            "asset size {}",
            asset.size
        )));
    }
    let sha = expected_digest(http, ep, release, &asset.name).await?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| UpdateError::io("create the download dir", &e))?;
    let dest = dir.join(&asset.name);
    if matches_digest(&dest, asset.size, &sha).await {
        tracing::debug!("updater: using the cached {}", asset.name);
        progress(asset.size);
        return Ok((dest, sha));
    }
    if !valid_download_url(&asset.url, ep) {
        return Err(UpdateError::BadRelease(format!(
            "asset URL {:?}",
            asset.url
        )));
    }
    fetch_verified(http, &asset.url, asset.size, sha, &dest, progress).await?;
    Ok((dest, sha))
}

/// The digest of `name` from the release's `SHA256SUMS`.
async fn expected_digest(
    http: &reqwest::Client,
    ep: &Endpoints,
    release: &Release,
    name: &str,
) -> Result<[u8; 32], UpdateError> {
    let no_sum = || UpdateError::NoChecksum(name.to_owned());
    let sums = release.sums.as_ref().ok_or_else(no_sum)?;
    if !valid_download_url(&sums.url, ep) {
        return Err(UpdateError::BadRelease(format!(
            "checksum URL {:?}",
            sums.url
        )));
    }
    let bytes = get_capped(http, &sums.url, SUMS_MAX).await?;
    let text = String::from_utf8(bytes)
        .map_err(|_| UpdateError::BadRelease("checksum file is not UTF-8".into()))?;
    lookup(&parse_sums(&text), name).ok_or_else(no_sum)
}

async fn get(http: &reqwest::Client, url: &str) -> Result<reqwest::Response, UpdateError> {
    let resp = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .map_err(|e| network(&e))?;
    match resp.status().as_u16() {
        200 => Ok(resp),
        code => Err(UpdateError::Http(code)),
    }
}

async fn get_capped(http: &reqwest::Client, url: &str, max: u64) -> Result<Vec<u8>, UpdateError> {
    let mut resp = get(http, url).await?;
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| network(&e))? {
        if (body.len() + chunk.len()) as u64 > max {
            return Err(UpdateError::BadRelease("checksum file too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Streams `url` into `dest` through `dest.part`; `dest` appears only when
/// exactly `size` bytes with the digest `sha` arrived. `progress` gets the
/// received byte count at most every 100 ms and once at the end.
pub async fn fetch_verified(
    http: &reqwest::Client,
    url: &str,
    size: u64,
    sha: [u8; 32],
    dest: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<(), UpdateError> {
    let part = part_path(dest);
    let result = stream_to(http, url, size, sha, &part, progress).await;
    let result = match result {
        Ok(()) => tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| UpdateError::io("rename the download", &e)),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&part).await;
    }
    result
}

async fn stream_to(
    http: &reqwest::Client,
    url: &str,
    size: u64,
    sha: [u8; 32],
    part: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<(), UpdateError> {
    let mut resp = get(http, url).await?;
    if resp.content_length().is_some_and(|len| len != size) {
        return Err(UpdateError::SizeMismatch);
    }
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| UpdateError::io("create the download file", &e))?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut last = Instant::now();
    while let Some(chunk) = resp.chunk().await.map_err(|e| network(&e))? {
        received += chunk.len() as u64;
        if received > size {
            return Err(UpdateError::SizeMismatch);
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| UpdateError::io("write the download", &e))?;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(received);
        }
    }
    if received != size {
        return Err(UpdateError::SizeMismatch);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if digest != sha {
        return Err(UpdateError::ChecksumMismatch);
    }
    file.sync_all()
        .await
        .map_err(|e| UpdateError::io("write the download", &e))?;
    progress(received);
    Ok(())
}

/// A previous download of the same file is reused only when it still matches.
async fn matches_digest(path: &Path, size: u64, sha: &[u8; 32]) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return false;
    };
    if !meta.is_file() || meta.len() != size {
        return false;
    }
    sha256_file(path).await.is_ok_and(|d| d == *sha)
}

async fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

pub(crate) fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// A plain file name: no separators, no leading dot, no control characters.
pub(crate) fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c == ':' || c.is_control())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    const REPO_SLUG: &str = "sokolcloud/rustket";

    fn digest(data: &[u8]) -> [u8; 32] {
        Sha256::digest(data).into()
    }

    /// Serves `routes` (path → raw HTTP response) until the test ends and
    /// records every requested path.
    async fn serve(routes: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = req.split(' ').nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let resp = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| response("404 Not Found", b""));
                let _ = sock.write_all(&resp).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let mut r = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        r.extend_from_slice(body);
        r
    }

    fn noop() -> impl FnMut(u64) + Send {
        |_| {}
    }

    #[tokio::test]
    async fn fetch_verified_cases() {
        let data = vec![7u8; 300_000];
        let sha = digest(&data);
        let mut routes = HashMap::new();
        routes.insert("/ok".to_owned(), response("200 OK", &data));
        routes.insert("/short".to_owned(), response("200 OK", &data[..1000]));
        // Content-Length matches what is sent; the expected size is smaller.
        routes.insert("/long".to_owned(), response("200 OK", &data));
        routes.insert("/gone".to_owned(), response("404 Not Found", b""));
        let mut corrupt = data.clone();
        corrupt[5] = 0;
        routes.insert("/corrupt".to_owned(), response("200 OK", &corrupt));
        let (base, _) = serve(routes).await;
        let http = reqwest::Client::new();
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("pkg.AppImage");

        let mut seen = Vec::new();
        let mut progress = |n: u64| seen.push(n);
        fetch_verified(
            &http,
            &format!("{base}/ok"),
            data.len() as u64,
            sha,
            &dest,
            &mut progress,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(seen.last(), Some(&(data.len() as u64)));
        assert!(!part_path(&dest).exists());
        std::fs::remove_file(&dest).unwrap();

        let size = data.len() as u64;
        let cases = [
            ("/corrupt", size, UpdateError::ChecksumMismatch),
            ("/short", size, UpdateError::SizeMismatch),
            ("/long", 1000, UpdateError::SizeMismatch),
            ("/gone", size, UpdateError::Http(404)),
        ];
        for (path, size, want) in cases {
            let got = fetch_verified(
                &http,
                &format!("{base}{path}"),
                size,
                sha,
                &dest,
                &mut noop(),
            )
            .await;
            assert_eq!(got, Err(want), "{path}");
            assert!(!dest.exists(), "{path}");
            assert!(!part_path(&dest).exists(), "{path}");
        }
    }

    /// Chunked transfer: no length up front, the running count aborts it.
    #[tokio::test]
    async fn chunked_oversized_body_is_aborted() {
        let chunk = vec![2u8; 4096];
        let mut body = Vec::new();
        for _ in 0..8 {
            body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            body.extend_from_slice(&chunk);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"0\r\n\r\n");
        let mut resp =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        resp.extend_from_slice(&body);
        let mut routes = HashMap::new();
        routes.insert("/chunked".to_owned(), resp);
        let (base, _) = serve(routes).await;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("f");
        let got = fetch_verified(
            &reqwest::Client::new(),
            &format!("{base}/chunked"),
            5000,
            [0; 32],
            &dest,
            &mut noop(),
        )
        .await;
        assert_eq!(got, Err(UpdateError::SizeMismatch));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    fn release(base: &str, name: &str, size: u64, with_sums: bool) -> (Release, Asset) {
        let asset = Asset {
            name: name.to_owned(),
            size,
            url: format!("{base}/dl/{name}"),
        };
        let release = Release {
            version: semver::Version::new(9, 9, 9),
            tag: "v9.9.9".into(),
            html_url: "https://github.com/sokolcloud/rustket/releases/tag/v9.9.9".into(),
            notes: String::new(),
            prerelease: false,
            assets: vec![asset.clone()],
            sums: with_sums.then(|| Asset {
                name: "SHA256SUMS".into(),
                size: 100,
                url: format!("{base}/dl/SHA256SUMS"),
            }),
        };
        (release, asset)
    }

    #[tokio::test]
    async fn run_verifies_and_reuses_the_cache() {
        let name = "Rustket-9.9.9-x86_64.AppImage";
        let data = b"#!/bin/sh\necho new\n".to_vec();
        let sums = format!("{}  {name}\n", hex::encode(digest(&data)));
        let mut routes = HashMap::new();
        routes.insert(format!("/dl/{name}"), response("200 OK", &data));
        routes.insert(
            "/dl/SHA256SUMS".to_owned(),
            response("200 OK", sums.as_bytes()),
        );
        let (base, seen) = serve(routes).await;
        let ep = Endpoints::new(format!("{base}/api"), REPO_SLUG);
        let http = reqwest::Client::new();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("9.9.9");
        let (rel, asset) = release(&base, name, data.len() as u64, true);

        let (path, sha) = run(&http, &ep, &rel, &asset, &dir, &mut noop())
            .await
            .unwrap();
        assert_eq!(path, dir.join(name));
        assert_eq!(sha, digest(&data));
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(seen.lock().unwrap().len(), 2);

        // Second run: only the checksum file is fetched.
        let again = run(&http, &ep, &rel, &asset, &dir, &mut noop())
            .await
            .unwrap();
        assert_eq!(again, (path.clone(), sha));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["/dl/SHA256SUMS", &format!("/dl/{name}"), "/dl/SHA256SUMS"]
        );

        // A changed cached file is downloaded again.
        std::fs::write(&path, b"#!/bin/sh\necho old\n").unwrap();
        run(&http, &ep, &rel, &asset, &dir, &mut noop())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }

    #[tokio::test]
    async fn run_refuses_bad_input() {
        let name = "rustket_9.9.9-1_amd64.deb";
        let mut routes = HashMap::new();
        routes.insert(
            "/dl/SHA256SUMS".to_owned(),
            response(
                "200 OK",
                format!("{}  other.deb\n", "a".repeat(64)).as_bytes(),
            ),
        );
        let (base, seen) = serve(routes).await;
        let ep = Endpoints::new(format!("{base}/api"), REPO_SLUG);
        let http = reqwest::Client::new();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("v");

        let (rel, asset) = release(&base, name, 10, false);
        assert_eq!(
            run(&http, &ep, &rel, &asset, &dir, &mut noop()).await,
            Err(UpdateError::NoChecksum(name.into()))
        );
        let (rel, asset) = release(&base, name, 10, true);
        assert_eq!(
            run(&http, &ep, &rel, &asset, &dir, &mut noop()).await,
            Err(UpdateError::NoChecksum(name.into()))
        );
        let (rel, asset) = release(&base, name, MAX_ASSET_SIZE + 1, true);
        assert!(matches!(
            run(&http, &ep, &rel, &asset, &dir, &mut noop()).await,
            Err(UpdateError::BadRelease(_))
        ));
        for bad in ["../x.deb", ".hidden", "a/b", "a\\b", ""] {
            let (rel, asset) = release(&base, bad, 10, true);
            assert!(matches!(
                run(&http, &ep, &rel, &asset, &dir, &mut noop()).await,
                Err(UpdateError::BadRelease(_))
            ));
        }
        // A foreign origin is never contacted.
        let (rel, asset) = release("http://127.0.0.2:1", name, 10, true);
        assert!(matches!(
            run(&http, &ep, &rel, &asset, &dir, &mut noop()).await,
            Err(UpdateError::BadRelease(_))
        ));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn file_names() {
        assert!(safe_file_name("Rustket-0.2.3-x64-setup.exe"));
        assert!(safe_file_name("rustket-0.3.0~rc1-1.x86_64.rpm"));
        assert!(!safe_file_name("C:x"));
        assert!(!safe_file_name("a\nb"));
        assert_eq!(
            part_path(Path::new("/c/updates/1/x.deb")),
            Path::new("/c/updates/1/x.deb.part")
        );
    }
}
