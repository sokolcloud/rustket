//! Message rendering shared by the timeline and the right panel (threads,
//! pinned, search results …). Owner: timeline agent; other components only
//! call [`message_body`] (or [`message_body_with`] when they keep a
//! [`RenderState`]) and handle [`RenderMsg`]. [`message_body`] has no state,
//! so it renders spoilers revealed and file/image/collapse controls inert;
//! only `OpenUrl`, `OpenRoom`, `OpenUser` and `Copy` come out of it.
//! Strings: `locales/room_render.yml` (`room_render.*`).
//!
//! Everything built here is `'static` (owned strings), so callers can wrap
//! rows in `iced::widget::lazy`.

mod attachments;
mod linkify;
mod md;
mod preview;
pub mod system;

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::Element;
use iced::widget::{Column, column};
use rocketchat::ServerUrl;
use rocketchat::models::Message;
use rustket_core::{ServerId, Workspace};
use rustket_ui::Tokens;
use rustket_ui::theme::{Mode, SPACE_1};

use super::ViewCtx;
use crate::images::Images;

pub use linkify::{find_links, safe_url};
pub use md::{EmojiPiece, emoji_element, emoji_piece, format_timestamp};
pub use preview::preview_handle;

/// Interactions inside a rendered message body.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderMsg {
    OpenUrl(String),
    /// A `#channel` mention (room name).
    OpenRoom(String),
    /// A `@user` mention (username).
    OpenUser(String),
    /// Reveal/hide a spoiler: message id and spoiler index.
    ToggleSpoiler(String, usize),
    /// Copy text (code block) to the clipboard.
    Copy(String),
    /// Collapse/expand an attachment or link preview: message id and index.
    ToggleCollapsed(String, usize),
    /// Show an image attachment in the lightbox.
    OpenImage(ImageRef),
    /// Save a file attachment (asks where).
    Download(FileRef),
    /// Download an audio/video attachment and open it with the system player.
    Play(FileRef),
    /// Open a file that was downloaded before.
    OpenFile(PathBuf),
    /// A `video_conf` block action (`crate::videoconf::block`).
    Call(crate::videoconf::CallAction),
}

/// An image attachment to show full size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    pub mid: String,
    /// Absolute URL.
    pub url: String,
    pub title: String,
    /// `image_type` of the attachment.
    pub mime: Option<String>,
}

/// A file attachment to download.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub mid: String,
    /// Absolute URL.
    pub url: String,
    pub name: String,
    pub size: Option<u64>,
    /// Declared MIME type (`audio_type`, `video_type`, file type).
    pub mime: Option<String>,
}

/// Progress of a download started from a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Transfer {
    Running { received: u64, total: Option<u64> },
    Done(PathBuf),
    Failed(String),
}

/// Per-view interaction state of rendered bodies: revealed spoilers, toggled
/// attachments and downloads. Keep one per list of messages (timeline, panel).
#[derive(Debug, Clone, Default)]
pub struct RenderState {
    revealed: BTreeSet<(String, usize)>,
    toggled: BTreeSet<(String, usize)>,
    downloads: BTreeMap<(String, String), Transfer>,
}

static EMPTY_STATE: RenderState = RenderState::new();

impl RenderState {
    pub const fn new() -> Self {
        Self {
            revealed: BTreeSet::new(),
            toggled: BTreeSet::new(),
            downloads: BTreeMap::new(),
        }
    }

    /// Applies the toggles of `msg`; returns whether it was one.
    pub fn handle(&mut self, msg: &RenderMsg) -> bool {
        let (set, key) = match msg {
            RenderMsg::ToggleSpoiler(mid, i) => (&mut self.revealed, (mid.clone(), *i)),
            RenderMsg::ToggleCollapsed(mid, i) => (&mut self.toggled, (mid.clone(), *i)),
            _ => return false,
        };
        if !set.remove(&key) {
            set.insert(key);
        }
        true
    }

    pub fn is_revealed(&self, mid: &str, i: usize) -> bool {
        self.revealed.contains(&(mid.to_owned(), i))
    }

    pub fn is_toggled(&self, mid: &str, i: usize) -> bool {
        self.toggled.contains(&(mid.to_owned(), i))
    }

    pub fn set_transfer(&mut self, mid: &str, url: &str, t: Transfer) {
        self.downloads.insert((mid.to_owned(), url.to_owned()), t);
    }

    pub fn transfer(&self, mid: &str, url: &str) -> Option<&Transfer> {
        self.downloads.get(&(mid.to_owned(), url.to_owned()))
    }

    /// Forgets everything about `mid` (message deleted).
    pub fn forget(&mut self, mid: &str) {
        self.revealed.retain(|(m, _)| m != mid);
        self.toggled.retain(|(m, _)| m != mid);
        self.downloads.retain(|(m, _), _| m != mid);
    }

    /// Hash of everything that affects how `mid` renders (for `lazy` keys).
    pub fn key(&self, mid: &str) -> u64 {
        let mut h = DefaultHasher::new();
        let lo = (mid.to_owned(), 0usize);
        let hi = (mid.to_owned(), usize::MAX);
        for k in self.revealed.range(lo.clone()..=hi.clone()) {
            k.1.hash(&mut h);
        }
        0xffu8.hash(&mut h);
        for k in self.toggled.range(lo..=hi) {
            k.1.hash(&mut h);
        }
        0xfeu8.hash(&mut h);
        for ((m, url), t) in &self.downloads {
            if m == mid {
                url.hash(&mut h);
                t.hash(&mut h);
            }
        }
        h.finish()
    }
}

static DARK: AtomicBool = AtomicBool::new(false);

/// Sets the appearance used for colours baked into rich text spans (links,
/// mentions, inline code). The app calls it before building the room view.
pub fn set_mode(mode: Mode) {
    DARK.store(mode == Mode::Dark, Ordering::Relaxed);
}

pub fn mode() -> Mode {
    if DARK.load(Ordering::Relaxed) {
        Mode::Dark
    } else {
        Mode::Light
    }
}

pub(crate) fn tokens() -> &'static Tokens {
    mode().tokens()
}

/// What the renderer needs from the view context (also buildable without a
/// room, e.g. by the panel).
#[derive(Clone, Copy)]
pub struct RenderCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub state: &'a RenderState,
    pub clock_24h: bool,
    pub use_real_names: bool,
    /// Stateful caller that handles every [`RenderMsg`]: spoilers can be
    /// revealed, images/files/collapse toggles are clickable. Without it
    /// spoilers render revealed and those controls are inert.
    pub interactive: bool,
}

impl<'a> RenderCtx<'a> {
    pub fn from_view(ctx: &ViewCtx<'a>, state: &'a RenderState) -> Self {
        Self {
            server_id: ctx.server_id,
            server: ctx.server,
            workspace: ctx.workspace,
            images: ctx.images,
            state,
            clock_24h: ctx.prefs.clock_24h,
            use_real_names: ctx.workspace.use_real_names(ctx.prefs.use_real_names),
            interactive: true,
        }
    }
}

/// The body of `msg` (markdown, attachments, …) without author/time.
pub fn message_body<'a>(msg: &'a Message, ctx: &ViewCtx<'a>) -> Element<'a, RenderMsg> {
    let rc = RenderCtx {
        interactive: false,
        ..RenderCtx::from_view(ctx, &EMPTY_STATE)
    };
    message_body_with(msg, &rc)
}

/// Like [`message_body`] with interaction state (spoilers, collapsed
/// attachments, downloads).
pub fn message_body_with(msg: &Message, rc: &RenderCtx<'_>) -> Element<'static, RenderMsg> {
    let mut col: Column<'static, RenderMsg> = column![].spacing(SPACE_1);
    if let Some(text) = text_part(msg, rc) {
        col = col.push(text);
    }
    for a in attachments::attachments(msg, rc) {
        col = col.push(a);
    }
    for p in attachments::link_previews(msg, rc) {
        col = col.push(p);
    }
    col.into()
}

/// The `video_conf` block of a `videoconf` message (`crate::videoconf`).
pub fn video_conf(msg: &Message, rc: &RenderCtx<'_>) -> Option<Element<'static, RenderMsg>> {
    crate::videoconf::block::view(msg, rc)
}

/// Only the text (markdown) part of `msg`, if it has any.
pub fn text_part(msg: &Message, rc: &RenderCtx<'_>) -> Option<Element<'static, RenderMsg>> {
    match &msg.md {
        Some(root) if !root.is_empty() => Some(md::blocks(root, &msg.id, msg, rc)),
        _ if !msg.msg.trim().is_empty() => Some(md::plain_paragraph(&msg.msg, &msg.id, rc)),
        _ => None,
    }
}

/// Room id for a `#channel` mention ([`RenderMsg::OpenRoom`]): a joined
/// room with that name (or display name); a room id is accepted as well.
pub fn resolve_room(ws: &Workspace, target: &str) -> Option<String> {
    let target = target.trim().trim_start_matches('#');
    if ws.room(target).is_some() {
        return Some(target.to_owned());
    }
    ws.rooms()
        .values()
        .find(|e| e.name() == target)
        .or_else(|| ws.rooms().values().find(|e| e.fname() == Some(target)))
        .map(|e| e.rid().to_owned())
}

/// Image URL a link preview of `u` shows, if any.
pub fn preview_image(u: &rocketchat::models::MessageUrl) -> Option<String> {
    if u.ignore_parse == Some(true) {
        return None;
    }
    attachments::preview_data(u)?
        .image
        .as_deref()
        .and_then(safe_url)
}

/// Plural category of `n` in the current locale for keys with
/// `_one`/`_few`/`_many` forms (English `few` texts equal `many`).
pub fn plural(n: u64) -> &'static str {
    plural_for(&rust_i18n::locale(), n)
}

/// [`plural`] for `locale`: Russian rules for `ru`, otherwise English
/// (`one` only for 1).
pub fn plural_for(locale: &str, n: u64) -> &'static str {
    if !locale.starts_with("ru") {
        return if n == 1 { "one" } else { "many" };
    }
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        "one"
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        "few"
    } else {
        "many"
    }
}

/// Copy of `msg` as plain text (for "Copy text").
pub fn plain_text(msg: &Message) -> String {
    let mut s = msg.plain_text();
    if s.trim().is_empty()
        && let Some(a) = msg.attachments.as_ref().and_then(|a| a.first())
    {
        s = a
            .description
            .clone()
            .or_else(|| a.text.clone())
            .or_else(|| a.title.clone())
            .unwrap_or_default();
    }
    s
}

/// Key for `lazy` rows: everything in a message that changes its rendering.
pub fn message_key(msg: &Message) -> u64 {
    let mut h = DefaultHasher::new();
    msg.id.hash(&mut h);
    msg.updated_at.map(|d| d.timestamp_millis()).hash(&mut h);
    msg.msg.hash(&mut h);
    msg.tcount.hash(&mut h);
    msg.tlm.map(|d| d.timestamp_millis()).hash(&mut h);
    msg.dcount.hash(&mut h);
    msg.pinned.hash(&mut h);
    msg.starred.as_ref().map(Vec::len).hash(&mut h);
    msg.temp.hash(&mut h);
    if let Some(r) = &msg.reactions {
        for (k, v) in r {
            k.hash(&mut h);
            v.usernames.hash(&mut h);
        }
    }
    msg.replies.hash(&mut h);
    h.finish()
}
