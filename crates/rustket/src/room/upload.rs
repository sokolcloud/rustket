//! File uploads of the composer: picking files (xdg portal through `rfd`),
//! drag and drop, clipboard images, the upload dialog (thumbnail, file name,
//! description, message), validation against the server settings, and
//! transfers with progress and cancel (`rooms.media` + `rooms.mediaConfirm`).
//! Strings: `locales/room_composer.yml` (`room_composer.upload.*`).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use futures::StreamExt;
use futures::stream::BoxStream;
use iced::widget::{column, container, image, progress_bar, row, text, text_input};
use iced::{Alignment, Element, Length, Task};
use rocketchat::RestClient;
use rocketchat::rest::{Bytes, ProgressFn, UploadRequest, UploadSource};
use rust_i18n::t;
use rustket_core::settings::Settings;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{BannerKind, icon_button_with, primary_button, secondary_button};
use rustket_ui::{Icon, icon, style};

use super::{Action, Effect, UpdateCtx};
use crate::error::{Failure, Res};

/// Thumbnails are decoded for images up to this size.
const THUMB_MAX_BYTES: u64 = 20 * 1024 * 1024;
const THUMB_PX: u32 = 320;
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub enum Source {
    Path(PathBuf),
    Bytes(Arc<Vec<u8>>),
}

/// A file waiting in the upload dialog.
#[derive(Debug, Clone)]
pub struct PendingFile {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub source: Source,
    pub thumb: Option<image::Handle>,
    /// Why it cannot be uploaded (localised).
    pub error: Option<String>,
}

/// The upload dialog: the first queued file is shown with editable fields.
#[derive(Debug)]
pub struct Dialog {
    pub queue: VecDeque<PendingFile>,
    pub name: String,
    pub description: String,
    pub message: String,
}

impl Dialog {
    fn new(files: Vec<PendingFile>) -> Option<Self> {
        let mut d = Self {
            queue: files.into(),
            name: String::new(),
            description: String::new(),
            message: String::new(),
        };
        d.reset_fields();
        (!d.queue.is_empty()).then_some(d)
    }

    fn reset_fields(&mut self) {
        self.name = self
            .queue
            .front()
            .map(|f| f.name.clone())
            .unwrap_or_default();
        self.description.clear();
        self.message.clear();
    }

    pub fn current(&self) -> Option<&PendingFile> {
        self.queue.front()
    }

    /// Drops the current file; `false` when none is left.
    fn advance(&mut self) -> bool {
        self.queue.pop_front();
        self.reset_fields();
        !self.queue.is_empty()
    }
}

/// A running upload (a snapshot of the registry entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub sent: u64,
    pub total: u64,
}

struct Running {
    server: String,
    rid: String,
    tmid: Option<String>,
    transfer: Transfer,
    handle: iced::task::Handle,
}

/// Running uploads of every composer by transfer id. Like the text outbox
/// they outlive the room and thread views: switching rooms does not cancel
/// an upload, a reopened composer shows its progress again, and a result
/// that arrives while the view is gone is reported by
/// [`orphan_result`]. Entries are removed by the upload task itself.
static RUNNING: LazyLock<Mutex<BTreeMap<u64, Running>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn running() -> std::sync::MutexGuard<'static, BTreeMap<u64, Running>> {
    RUNNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The running uploads of a composer (server, room, thread).
pub fn transfers(server: &str, rid: &str, tmid: Option<&str>) -> Vec<Transfer> {
    running()
        .values()
        .filter(|r| r.server == server && r.rid == rid && r.tmid.as_deref() == tmid)
        .map(|r| r.transfer.clone())
        .collect()
}

/// Adds a running upload. Its handle is not aborted when the composer goes
/// away: only Cancel stops an upload.
fn register(
    server: &str,
    rid: &str,
    tmid: Option<&str>,
    transfer: Transfer,
    handle: iced::task::Handle,
) {
    running().insert(
        transfer.id,
        Running {
            server: server.to_owned(),
            rid: rid.to_owned(),
            tmid: tmid.map(str::to_owned),
            transfer,
            handle,
        },
    );
}

fn set_progress(id: u64, sent: u64, total: u64) {
    if let Some(r) = running().get_mut(&id) {
        r.transfer.sent = sent;
        r.transfer.total = total;
    }
}

/// Removes a finished or cancelled upload; its file name.
fn finish(id: u64) -> Option<String> {
    running().remove(&id).map(|r| r.transfer.name)
}

/// The outcome of an upload whose composer is gone (room switched, thread
/// closed): the message is applied, a failure becomes a toast instead of
/// getting lost.
pub fn orphan_result(msg: Message, ws: &mut rustket_core::Workspace) -> Option<Effect> {
    match msg {
        Message::Done(_, name, r) => done_effect(name, r, ws),
        _ => None,
    }
}

fn done_effect(
    name: String,
    r: Res<Box<rocketchat::models::Message>>,
    ws: &mut rustket_core::Workspace,
) -> Option<Effect> {
    match r {
        Ok(m) => {
            ws.apply_message(*m);
            None
        }
        Err(e) => Some(Effect::Toast(
            BannerKind::Error,
            t!("room_composer.upload.failed", name = name).into_owned(),
            e.user_text(),
        )),
    }
}

#[derive(Debug, Default)]
pub struct Uploads {
    pub dialog: Option<Dialog>,
}

/// Transfer ids are unique per process: a transfer of a closed (and reopened)
/// composer can never be confused with a new one.
static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub enum Message {
    /// Attach button: open the file chooser.
    Attach,
    /// Chosen or dropped paths.
    Paths(Vec<PathBuf>),
    /// Files read (metadata, thumbnails) and validated.
    Loaded(Vec<PendingFile>),
    /// Image bytes from the clipboard (PNG).
    ClipboardImage(Option<Arc<Vec<u8>>>),
    Name(String),
    Description(String),
    Text(String),
    /// Upload the current file of the dialog.
    Send,
    /// Skip the current file.
    Skip,
    /// Close the dialog, dropping every queued file.
    Close,
    /// Progress of a transfer (the registry is updated by the upload).
    Progress(u64, u64, u64),
    /// A transfer finished (id, file name, result).
    Done(u64, String, Res<Box<rocketchat::models::Message>>),
    Cancel(u64),
}

impl Uploads {
    pub fn is_dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn update(
        &mut self,
        msg: Message,
        ctx: &mut UpdateCtx<'_>,
        tmid: Option<&str>,
    ) -> Action<Message> {
        match msg {
            Message::Attach => {
                if !rustket_core::permissions::can_upload(ctx.workspace, ctx.rid) {
                    return Action::effect(toast_error(
                        t!("room_composer.upload.disabled").into_owned(),
                    ));
                }
                Action::task(pick_files())
            }
            Message::Paths(paths) => {
                if paths.is_empty() {
                    return Action::none();
                }
                let settings = ctx.workspace.settings().clone();
                Action::task(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            paths
                                .iter()
                                .filter_map(|p| load_path(p, &settings))
                                .collect::<Vec<_>>()
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::Loaded,
                ))
            }
            Message::ClipboardImage(Some(bytes)) => {
                let settings = ctx.workspace.settings().clone();
                let name = format!(
                    "clipboard-{}.png",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                );
                Action::task(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            vec![load_bytes(name, "image/png".into(), bytes, &settings)]
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::Loaded,
                ))
            }
            Message::ClipboardImage(None) => Action::none(),
            Message::Loaded(files) => {
                match &mut self.dialog {
                    Some(d) => d.queue.extend(files),
                    None => self.dialog = Dialog::new(files),
                }
                Action::none()
            }
            Message::Name(s) => {
                if let Some(d) = &mut self.dialog {
                    d.name = s;
                }
                Action::none()
            }
            Message::Description(s) => {
                if let Some(d) = &mut self.dialog {
                    d.description = s;
                }
                Action::none()
            }
            Message::Text(s) => {
                if let Some(d) = &mut self.dialog {
                    d.message = s;
                }
                Action::none()
            }
            Message::Skip => {
                if let Some(d) = &mut self.dialog
                    && !d.advance()
                {
                    self.dialog = None;
                }
                Action::none()
            }
            Message::Close => {
                self.dialog = None;
                Action::none()
            }
            Message::Send => {
                let Some(d) = &mut self.dialog else {
                    return Action::none();
                };
                let Some(file) = d.current().cloned() else {
                    return Action::none();
                };
                if file.error.is_some() {
                    return Action::none();
                }
                // Encrypted rooms: encrypted upload, or refused (§7.8).
                let e2e = match crate::e2e::outgoing::upload_mode(ctx.workspace, ctx.rid) {
                    crate::e2e::outgoing::UploadMode::Refused(why) => {
                        return Action::effect(toast_error(why));
                    }
                    crate::e2e::outgoing::UploadMode::Encrypt(m) => Some(m),
                    crate::e2e::outgoing::UploadMode::Plain => None,
                };
                // Keep the file (and the edited fields) when it cannot go out now.
                let Some(rest) = ctx.rest() else {
                    return Action::effect(toast_error(
                        t!("room_composer.upload.not_connected").into_owned(),
                    ));
                };
                let name = match d.name.trim() {
                    "" => file.name.clone(),
                    n => n.to_owned(),
                };
                let description = Some(d.description.trim().to_owned()).filter(|s| !s.is_empty());
                let message = Some(d.message.trim().to_owned()).filter(|s| !s.is_empty());
                if !d.advance() {
                    self.dialog = None;
                }
                let id = NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed);
                let (task, handle) = start(
                    id,
                    rest,
                    ctx.rid.to_owned(),
                    tmid.map(str::to_owned),
                    file.clone(),
                    name.clone(),
                    description,
                    message,
                    e2e,
                )
                .abortable();
                // The task runs only once this update returns: registered
                // before it can finish (it removes itself).
                register(
                    ctx.server_id.as_str(),
                    ctx.rid,
                    tmid,
                    Transfer {
                        id,
                        name,
                        sent: 0,
                        total: file.size,
                    },
                    handle,
                );
                Action::task(task)
            }
            // Only redraws: the upload keeps the registry up to date.
            Message::Progress(..) => Action::none(),
            Message::Done(_, name, result) => match done_effect(name, result, ctx.workspace) {
                Some(e) => Action::effect(e),
                None => Action::none(),
            },
            Message::Cancel(id) => {
                if let Some(r) = running().remove(&id) {
                    r.handle.abort();
                }
                Action::none()
            }
        }
    }

    /// Progress rows of the running uploads of this composer.
    pub fn transfers_view<'a>(
        &self,
        server: &str,
        rid: &str,
        tmid: Option<&str>,
    ) -> Option<Element<'a, Message>> {
        let transfers = transfers(server, rid, tmid);
        if transfers.is_empty() {
            return None;
        }
        let rows = transfers.into_iter().map(|t| {
            let pct = if t.total == 0 {
                0.0
            } else {
                (t.sent as f32 / t.total as f32).clamp(0.0, 1.0)
            };
            row![
                icon(Icon::Upload).size(14.0).style(style::text_secondary),
                text(t.name)
                    .size(TEXT_CAPTION)
                    .width(Length::FillPortion(2))
                    .wrapping(text::Wrapping::None),
                progress_bar(0.0..=1.0, pct)
                    .girth(6.0)
                    .length(Length::FillPortion(3)),
                text(format!("{}%", (pct * 100.0).round() as u32))
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
                icon_button_with(
                    Icon::X,
                    Some(Message::Cancel(t.id)),
                    t!("room_composer.upload.cancel"),
                    false,
                    22.0
                ),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        });
        Some(column(rows).spacing(SPACE_1).into())
    }

    /// The upload dialog, while open.
    pub fn dialog_view<'a>(&'a self) -> Option<Element<'a, Message>> {
        let d = self.dialog.as_ref()?;
        let file = d.current()?;
        let preview: Element<'a, Message> = match &file.thumb {
            Some(h) => container(image(h.clone()).height(Length::Fixed(180.0)))
                .center_x(Length::Fill)
                .into(),
            None => row![
                icon(Icon::File).size(28.0).style(style::text_secondary),
                column![
                    text(file.name.as_str()).size(TEXT_SMALL),
                    text(file.mime.as_str())
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                ]
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .into(),
        };
        let mut body = column![
            preview,
            text(t!(
                "room_composer.upload.size",
                size = human_size(file.size)
            ))
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
            field(
                t!("room_composer.upload.file_name").into_owned(),
                text_input("", &d.name)
                    .on_input(Message::Name)
                    .style(style::text_input)
                    .padding([8, 10])
                    .into()
            ),
            field(
                t!("room_composer.upload.description").into_owned(),
                text_input(&t!("room_composer.upload.description_hint"), &d.description)
                    .on_input(Message::Description)
                    .style(style::text_input)
                    .padding([8, 10])
                    .into()
            ),
            field(
                t!("room_composer.upload.message").into_owned(),
                text_input(&t!("room_composer.upload.message_hint"), &d.message)
                    .on_input(Message::Text)
                    .on_submit(Message::Send)
                    .style(style::text_input)
                    .padding([8, 10])
                    .into()
            ),
        ]
        .spacing(SPACE_3);
        if let Some(err) = &file.error {
            body = body.push(
                row![
                    icon(Icon::TriangleAlert)
                        .size(14.0)
                        .style(style::text_danger),
                    text(err.as_str())
                        .size(TEXT_SMALL)
                        .style(style::text_danger),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            );
        }
        let more = d.queue.len().saturating_sub(1);
        let title = if more > 0 {
            t!("room_composer.upload.title_more", count = more).into_owned()
        } else {
            t!("room_composer.upload.title").into_owned()
        };
        let mut actions: Vec<Element<'a, Message>> = Vec::new();
        if more > 0 {
            actions.push(
                secondary_button(t!("room_composer.upload.skip"), Some(Message::Skip)).into(),
            );
        }
        actions
            .push(secondary_button(t!("room_composer.upload.cancel"), Some(Message::Close)).into());
        actions.push(
            primary_button(
                t!("room_composer.upload.send"),
                file.error.is_none().then_some(Message::Send),
            )
            .into(),
        );
        Some(rustket_ui::widget::dialog(
            title,
            body,
            actions,
            Some(Message::Close),
        ))
    }
}

fn field<'a>(label: String, input: Element<'a, Message>) -> Element<'a, Message> {
    column![
        text(label).size(TEXT_CAPTION).style(style::text_secondary),
        input
    ]
    .spacing(SPACE_1)
    .into()
}

fn toast_error(body: String) -> Effect {
    Effect::Toast(
        BannerKind::Error,
        t!("room_composer.upload.error_title").into_owned(),
        body,
    )
}

/// Opens the system file chooser (xdg portal on Linux); never blocks.
fn pick_files() -> Task<Message> {
    let title = t!("room_composer.upload.choose").into_owned();
    Task::perform(
        async move {
            rfd::AsyncFileDialog::new()
                .set_title(title)
                .pick_files()
                .await
                .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
                .unwrap_or_default()
        },
        Message::Paths,
    )
}

/// Files copied in a file manager (`Paths`) or an image as PNG
/// (`ClipboardImage(Some)`); `ClipboardImage(None)` when the clipboard holds
/// neither. Blocking work runs off the UI thread.
pub fn clipboard_contents() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                let mut cb = arboard::Clipboard::new().ok()?;
                if let Ok(files) = cb.get().file_list()
                    && !files.is_empty()
                {
                    return Some(Message::Paths(files));
                }
                let img = cb.get_image().ok()?;
                let rgba = ::image::RgbaImage::from_raw(
                    u32::try_from(img.width).ok()?,
                    u32::try_from(img.height).ok()?,
                    img.bytes.into_owned(),
                )?;
                let mut png = Vec::new();
                rgba.write_to(
                    &mut std::io::Cursor::new(&mut png),
                    ::image::ImageFormat::Png,
                )
                .ok()?;
                Some(Message::ClipboardImage(Some(Arc::new(png))))
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(Message::ClipboardImage(None))
        },
        std::convert::identity,
    )
}

/// Why a file cannot be uploaded according to the server settings (Rocket.Chat's
/// `fileUploadIsValidContentType`, `FileUpload_MaxFileSize`).
pub fn validate(settings: &Settings, mime: &str, size: u64) -> Option<String> {
    if !settings.file_upload_enabled() {
        return Some(t!("room_composer.upload.disabled").into_owned());
    }
    if size == 0 {
        return Some(t!("room_composer.upload.empty").into_owned());
    }
    if let Some(max) = settings.file_upload_max_size()
        && size > max
    {
        return Some(t!("room_composer.upload.too_large", max = human_size(max)).into_owned());
    }
    if !settings.is_upload_type_allowed(mime) {
        return Some(t!("room_composer.upload.type_not_allowed", mime = mime).into_owned());
    }
    None
}

fn load_path(path: &Path, settings: &Settings) -> Option<PendingFile> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().into_owned();
    let mime = mime_for(&name).to_owned();
    let size = meta.len();
    let thumb = (mime.starts_with("image/") && mime != "image/svg+xml" && size <= THUMB_MAX_BYTES)
        .then(|| std::fs::read(path).ok())
        .flatten()
        .and_then(|b| crate::images::decode(&b, THUMB_PX).ok())
        .map(|l| l.handle);
    Some(PendingFile {
        error: validate(settings, &mime, size),
        name,
        mime,
        size,
        source: Source::Path(path.to_owned()),
        thumb,
    })
}

fn load_bytes(name: String, mime: String, bytes: Arc<Vec<u8>>, settings: &Settings) -> PendingFile {
    let size = bytes.len() as u64;
    let thumb = crate::images::decode(&bytes, THUMB_PX)
        .ok()
        .map(|l| l.handle);
    PendingFile {
        error: validate(settings, &mime, size),
        name,
        mime,
        size,
        source: Source::Bytes(bytes),
        thumb,
    }
}

/// Runs one upload, reporting progress (whole percents) and the result.
#[allow(clippy::too_many_arguments)]
fn start(
    id: u64,
    rest: RestClient,
    rid: String,
    tmid: Option<String>,
    file: PendingFile,
    name: String,
    description: Option<String>,
    message: Option<String>,
    e2e: Option<rustket_core::e2e::E2eManager>,
) -> Task<Message> {
    let (tx, rx) = futures::channel::mpsc::unbounded::<(u64, u64)>();
    let last = Arc::new(AtomicU64::new(u64::MAX));
    let progress: ProgressFn = Arc::new(move |sent, total| {
        let pct = (sent * 100).checked_div(total).unwrap_or(100);
        if last.swap(pct, Ordering::Relaxed) != pct {
            set_progress(id, sent, total);
            let _ = tx.unbounded_send((sent, total));
        }
    });
    let upload = async move {
        if let Some(mgr) = e2e {
            let data = match &file.source {
                Source::Bytes(b) => b.as_ref().clone(),
                Source::Path(p) => tokio::fs::read(p).await?,
            };
            let caption = message.unwrap_or_default();
            return crate::e2e::outgoing::upload(
                mgr,
                rest,
                rid,
                tmid,
                name,
                file.mime.clone(),
                data,
                caption,
                description,
                Some(progress),
            )
            .await
            .map_err(crate::e2e::outgoing::local_error);
        }
        let source = match &file.source {
            Source::Bytes(b) => UploadSource::Bytes(Bytes::from(b.as_ref().clone())),
            Source::Path(p) => {
                let f = tokio::fs::File::open(p).await?;
                let len = f.metadata().await?.len();
                UploadSource::Stream {
                    stream: file_stream(f),
                    len,
                }
            }
        };
        let mut req = UploadRequest::new(name, file.mime.clone(), Vec::<u8>::new());
        req.source = source;
        req.description = description;
        req.msg = message;
        req.tmid = tmid;
        req.progress = Some(progress);
        rest.upload_file(&rid, req).await
    };
    let progress_stream = rx.map(move |(s, t)| Message::Progress(id, s, t));
    let done = futures::stream::once(async move {
        let r = upload.await.map(Box::new).map_err(Failure::from);
        let name = finish(id).unwrap_or_default();
        Message::Done(id, name, r)
    });

    Task::stream(futures::stream::select(progress_stream, done))
}

fn file_stream(file: tokio::fs::File) -> BoxStream<'static, std::io::Result<Bytes>> {
    use tokio::io::AsyncReadExt;
    futures::stream::unfold(Some(file), |state| async move {
        let mut f = state?;
        let mut buf = vec![0u8; CHUNK];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(f)))
            }
            Err(e) => Some((Err(e), None)),
        }
    })
    .boxed()
}

/// MIME type from the file extension (`application/octet-stream` otherwise).
pub fn mime_for(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        _ => "application/octet-stream",
    }
}

/// `1.5 MB` style sizes (binary units, as Rocket.Chat shows them; unit names
/// are localised).
pub fn human_size(bytes: u64) -> String {
    const UNITS: usize = 5;
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS - 1 {
        v /= 1024.0;
        u += 1;
    }
    let unit = match u {
        0 => t!("room_composer.upload.unit.b"),
        1 => t!("room_composer.upload.unit.kb"),
        2 => t!("room_composer.upload.unit.mb"),
        3 => t!("room_composer.upload.unit.gb"),
        _ => t!("room_composer.upload.unit.tb"),
    };
    if u == 0 {
        format!("{bytes} {unit}")
    } else {
        format!("{v:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn settings(pairs: &[(&str, serde_json::Value)]) -> Settings {
        let mut s = Settings::new();
        for (k, v) in pairs {
            s.set_value(*k, v.clone());
        }
        s
    }

    /// Review: switching rooms dropped the composer and aborted its running
    /// uploads without a word. Uploads now live in the registry: they keep
    /// running, a new composer of the room shows them, and the outcome of
    /// one whose composer is gone is reported.
    #[test]
    fn uploads_outlive_the_composer() {
        rust_i18n::set_locale("en");
        let (_task, handle) =
            Task::<Message>::future(futures::future::pending::<Message>()).abortable();
        let watch = handle.clone();
        let id = NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed);
        let composer = Uploads::default();
        register(
            "srv-outlive",
            "R1",
            None,
            Transfer {
                id,
                name: "video.mp4".into(),
                sent: 0,
                total: 300,
            },
            handle,
        );
        drop(composer);
        assert!(!watch.is_aborted(), "the room switch does not cancel it");
        set_progress(id, 150, 300);
        let shown = transfers("srv-outlive", "R1", None);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].sent, shown[0].total), (150, 300));
        assert!(transfers("srv-outlive", "R1", Some("T1")).is_empty());
        assert!(transfers("srv-outlive", "R2", None).is_empty());
        assert!(
            Uploads::default()
                .transfers_view("srv-outlive", "R1", None)
                .is_some()
        );
        // The task removes its entry; the result reaches the app without
        // a composer and a failure becomes a toast.
        assert_eq!(finish(id).as_deref(), Some("video.mp4"));
        assert!(transfers("srv-outlive", "R1", None).is_empty());
        let mut ws = rustket_core::Workspace::new();
        let failed = Message::Done(
            id,
            "video.mp4".into(),
            Err(Failure::from(rocketchat::Error::Unauthorized {
                message: None,
                code: None,
            })),
        );
        match orphan_result(failed, &mut ws) {
            Some(Effect::Toast(BannerKind::Error, title, _)) => {
                assert!(title.contains("video.mp4"));
            }
            other => panic!("expected a toast, got {other:?}"),
        }
        // Cancel aborts.
        let (_task, handle) =
            Task::<Message>::future(futures::future::pending::<Message>()).abortable();
        let watch = handle.clone();
        let id = NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed);
        register(
            "srv-outlive",
            "R1",
            None,
            Transfer {
                id,
                name: "b".into(),
                sent: 0,
                total: 1,
            },
            handle,
        );
        let mut env_ws = rustket_core::Workspace::new();
        let sid = rustket_core::ServerId("srv-outlive".into());
        let url = rocketchat::ServerUrl::parse("http://chat.test").unwrap();
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut env_ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let _ = Uploads::default().update(Message::Cancel(id), &mut ctx, None);
        assert!(watch.is_aborted());
        assert!(transfers("srv-outlive", "R1", None).is_empty());
    }

    #[test]
    fn validation_follows_server_settings() {
        rust_i18n::set_locale("en");
        let s = settings(&[]);
        assert_eq!(validate(&s, "image/png", 10), None);
        assert!(
            validate(&s, "image/svg+xml", 10).is_some(),
            "svg blacklisted by default"
        );
        assert!(validate(&s, "image/png", 0).is_some());
        assert!(validate(&s, "image/png", 200 * 1024 * 1024).is_some());
        let s = settings(&[("FileUpload_Enabled", json!(false))]);
        assert!(validate(&s, "image/png", 10).is_some());
        let s = settings(&[
            ("FileUpload_MaxFileSize", json!(-1)),
            (
                "FileUpload_MediaTypeWhiteList",
                json!("image/*,application/pdf"),
            ),
        ]);
        assert_eq!(validate(&s, "image/jpeg", u64::MAX / 2), None);
        assert_eq!(validate(&s, "application/pdf", 5), None);
        assert!(validate(&s, "text/plain", 5).is_some());
    }

    #[test]
    fn mime_and_sizes() {
        rust_i18n::set_locale("en");
        assert_eq!(mime_for("a.PNG"), "image/png");
        assert_eq!(mime_for("archive.tar.gz"), "application/gzip");
        assert_eq!(mime_for("noext"), "application/octet-stream");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(104_857_600), "100.0 MB");
    }

    #[test]
    fn dialog_queue() {
        let f = |n: &str| PendingFile {
            name: n.into(),
            mime: "text/plain".into(),
            size: 1,
            source: Source::Bytes(Arc::new(vec![1])),
            thumb: None,
            error: None,
        };
        let mut d = Dialog::new(vec![f("a.txt"), f("b.txt")]).unwrap();
        assert_eq!(d.name, "a.txt");
        d.description = "x".into();
        assert!(d.advance());
        assert_eq!(d.name, "b.txt");
        assert!(d.description.is_empty());
        assert!(!d.advance());
        assert!(Dialog::new(vec![]).is_none());
    }

    #[test]
    fn loads_files_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("notes.txt");
        std::fs::write(&p, b"hello").unwrap();
        let f = load_path(&p, &settings(&[])).unwrap();
        assert_eq!(
            (f.name.as_str(), f.mime.as_str(), f.size),
            ("notes.txt", "text/plain", 5)
        );
        assert!(f.error.is_none() && f.thumb.is_none());
        assert!(
            load_path(dir.path(), &settings(&[])).is_none(),
            "directories are skipped"
        );
    }
}
