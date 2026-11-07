//! Uploads (`rooms.media` + `rooms.mediaConfirm`) and authenticated downloads.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::stream::{self, BoxStream, Stream, StreamExt};
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

use super::{RestClient, field};
use crate::error::{Error, Result};
use crate::models::{Message, UploadedFile};

/// Upload progress callback: `(bytes_sent, total_bytes)`.
pub type ProgressFn = Arc<dyn Fn(u64, u64) + Send + Sync>;

const CHUNK: usize = 64 * 1024;

pub enum UploadSource {
    Bytes(Bytes),
    /// A byte stream of known length (e.g. a file read with `tokio_util::io::ReaderStream`).
    Stream {
        stream: BoxStream<'static, std::io::Result<Bytes>>,
        len: u64,
    },
}

impl fmt::Debug for UploadSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(b) => write!(f, "Bytes({})", b.len()),
            Self::Stream { len, .. } => write!(f, "Stream({len})"),
        }
    }
}

impl UploadSource {
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(b) => b.len() as u64,
            Self::Stream { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A file to upload into a room.
pub struct UploadRequest {
    pub filename: String,
    pub mime: String,
    pub source: UploadSource,
    /// Caption shown as the file description.
    pub description: Option<String>,
    /// Message text sent with the file.
    pub msg: Option<String>,
    /// Upload into a thread.
    pub tmid: Option<String>,
    pub progress: Option<ProgressFn>,
}

impl fmt::Debug for UploadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadRequest")
            .field("filename", &self.filename)
            .field("mime", &self.mime)
            .field("source", &self.source)
            .field("tmid", &self.tmid)
            .finish_non_exhaustive()
    }
}

impl UploadRequest {
    pub fn new(
        filename: impl Into<String>,
        mime: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Self {
        Self {
            filename: filename.into(),
            mime: mime.into(),
            source: UploadSource::Bytes(data.into()),
            description: None,
            msg: None,
            tmid: None,
            progress: None,
        }
    }
}

/// Upload progress shared with the watchdog.
#[derive(Default)]
struct UploadState {
    /// Milliseconds since `start` of the last chunk handed to the connection.
    last_progress_ms: AtomicU64,
    /// Set (to ms since `start`) once the whole body was handed over.
    done_ms: AtomicU64,
}

fn counting_body(
    source: UploadSource,
    progress: Option<ProgressFn>,
    state: Arc<UploadState>,
    start: Instant,
) -> (reqwest::Body, u64) {
    let total = source.len();
    let raw: BoxStream<'static, std::io::Result<Bytes>> = match source {
        UploadSource::Bytes(b) => {
            let chunks: Vec<std::io::Result<Bytes>> = (0..b.len())
                .step_by(CHUNK)
                .map(|start| Ok(b.slice(start..(start + CHUNK).min(b.len()))))
                .collect();
            stream::iter(chunks).boxed()
        }
        UploadSource::Stream { stream, .. } => stream,
    };
    let sent = Arc::new(AtomicU64::new(0));
    let elapsed = move || {
        u64::try_from(start.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .max(1)
    };
    let st = state.clone();
    let counted = raw.inspect(move |chunk| {
        if let Ok(c) = chunk {
            let now = sent.fetch_add(c.len() as u64, Ordering::Relaxed) + c.len() as u64;
            st.last_progress_ms.store(elapsed(), Ordering::Relaxed);
            if now >= total {
                st.done_ms.store(elapsed(), Ordering::Relaxed);
            }
            if let Some(p) = &progress {
                p(now, total);
            }
        }
    });
    let end = stream::once(async move {
        state.done_ms.store(elapsed(), Ordering::Relaxed);
        None
    })
    .filter_map(|x: Option<std::io::Result<Bytes>>| async move { x });
    (reqwest::Body::wrap_stream(counted.chain(end)), total)
}

/// A streaming download response.
pub struct Download {
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    response: reqwest::Response,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("content_type", &self.content_type)
            .field("content_length", &self.content_length)
            .finish()
    }
}

impl Download {
    pub async fn bytes(self) -> Result<Bytes> {
        Ok(self.response.bytes().await?)
    }

    pub fn stream(self) -> impl Stream<Item = Result<Bytes>> + Send {
        self.response.bytes_stream().map(|r| r.map_err(Error::from))
    }
}

impl RestClient {
    /// Uploads a file using the 8.x two-step flow: `POST rooms.media/{rid}`
    /// (multipart field `file`) then `POST rooms.mediaConfirm/{rid}/{fileId}`.
    /// Returns the created message.
    pub async fn upload_file(&self, rid: &str, req: UploadRequest) -> Result<Message> {
        let uploaded = self
            .upload_media(
                rid,
                req.filename.clone(),
                req.mime.clone(),
                req.source,
                req.progress,
            )
            .await?;
        self.confirm_media(
            rid,
            &uploaded.id,
            req.msg.as_deref(),
            req.description.as_deref(),
            req.tmid.as_deref(),
        )
        .await
    }

    /// Step 1: store the file (not yet visible in the room).
    pub async fn upload_media(
        &self,
        rid: &str,
        filename: String,
        mime: String,
        source: UploadSource,
        progress: Option<ProgressFn>,
    ) -> Result<UploadedFile> {
        let start = Instant::now();
        let state = Arc::new(UploadState::default());
        let (body, len) = counting_body(source, progress, state.clone(), start);
        let part = Part::stream_with_length(body, len)
            .file_name(filename)
            .mime_str(&mime)
            .map_err(|e| Error::Protocol(format!("invalid MIME type {mime}: {e}")))?;
        let form = Form::new().part("file", part);
        let url = self.server().api(&format!(
            "rooms.media/{}",
            crate::server_url::encode_segment(rid)
        ));
        // No total/read timeout (the response starts only after the whole body is
        // sent); a watchdog aborts on stalled progress or a missing response.
        let rb = self
            .authed(self.upload_http(), Method::POST, &url, None)
            .multipart(form);
        let cfg = *self.config();
        let watchdog = async move {
            let tick = (cfg.upload_stall_timeout.min(cfg.upload_response_timeout) / 4)
                .clamp(Duration::from_millis(50), Duration::from_secs(1));
            loop {
                tokio::time::sleep(tick).await;
                let now = start.elapsed();
                let done = state.done_ms.load(Ordering::Relaxed);
                if done > 0 {
                    if now.saturating_sub(Duration::from_millis(done)) > cfg.upload_response_timeout
                    {
                        return Error::Timeout("no response after upload");
                    }
                } else {
                    let last =
                        Duration::from_millis(state.last_progress_ms.load(Ordering::Relaxed));
                    if now.saturating_sub(last) > cfg.upload_stall_timeout {
                        return Error::Timeout("upload stalled");
                    }
                }
            }
        };
        let v: Value = tokio::select! {
            r = self.send(rb) => r?,
            e = watchdog => return Err(e),
        };
        field(v, "file")
    }

    /// Step 2: publish the stored file as a message.
    pub async fn confirm_media(
        &self,
        rid: &str,
        file_id: &str,
        msg: Option<&str>,
        description: Option<&str>,
        tmid: Option<&str>,
    ) -> Result<Message> {
        let mut body = json!({});
        if let Some(m) = msg {
            body["msg"] = Value::String(m.into());
        }
        if let Some(d) = description {
            body["description"] = Value::String(d.into());
        }
        if let Some(t) = tmid {
            body["tmid"] = Value::String(t.into());
        }
        let v: Value = self
            .post(
                &format!(
                    "rooms.mediaConfirm/{}/{}",
                    crate::server_url::encode_segment(rid),
                    crate::server_url::encode_segment(file_id)
                ),
                &body,
            )
            .await?;
        field(v, "message")
    }

    /// Downloads an attachment, avatar or any URL. Server-relative paths are resolved
    /// against the server. Auth headers are sent only to this server; a redirect to
    /// another origin (e.g. S3/GCS file storage) is followed without credentials,
    /// cookies or `Referer`, and never from https to http.
    ///
    /// The credentials also go as the `rc_uid` / `rc_token` cookies the web client
    /// sets: `/avatar/…` ignores the auth headers and, with
    /// `Accounts_AvatarBlockUnauthenticatedAccess` (the default), answers
    /// anonymous requests with the default SVG.
    pub async fn download(&self, url_or_path: &str) -> Result<Download> {
        let url = self.server().absolute(url_or_path);
        let mut response = if self.server().owns(&url) {
            let mut rb = self.authed(self.download_http(), Method::GET, &url, None);
            if let Some(c) = self.credentials() {
                rb = rb.header(
                    reqwest::header::COOKIE,
                    format!("rc_uid={}; rc_token={}", c.user_id, c.auth_token),
                );
            }
            rb.send().await?
        } else {
            self.foreign_http().get(&url).send().await?
        };
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| Error::Protocol("redirect without Location".into()))?;
            let base = response.url().clone();
            let target = base
                .join(&location)
                .map_err(|e| Error::Url(format!("{location}: {e}")))?;
            let downgrade = base.scheme() == "https" && target.scheme() != "https";
            if downgrade || !matches!(target.scheme(), "http" | "https") {
                return Err(Error::Redirect {
                    location: target.to_string(),
                    cross_origin: true,
                });
            }
            tracing::debug!(host = ?target.host_str(), "following download redirect without credentials");
            response = self.foreign_http().get(target).send().await?;
            if response.status().is_redirection() {
                return Err(super::redirect_error(&response));
            }
        }
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.bytes().await.unwrap_or_default();
            return Err(Error::from_response(status, None, &body));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_length = response.content_length();
        Ok(Download {
            content_type,
            content_length,
            response,
        })
    }
}
