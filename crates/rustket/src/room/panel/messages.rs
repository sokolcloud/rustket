//! Message lists: threads (with filters), pinned, starred, mentions and the
//! room search. Bodies go through the shared message renderer.

use std::collections::{HashMap, HashSet};

use iced::widget::{column, container, mouse_area, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Message as RcMessage, Paginated, RoomType};
use rust_i18n::t;
use rustket_core::room_list::{self, PreviewKind};
use rustket_core::thread_index::follows;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, font_semibold,
};
use rustket_ui::widget::{
    AvatarShape, BadgePlace, TooltipPosition, avatar, clamp, divider, dot_in, ellipsis,
    icon_button_with, tooltip,
};

use super::api::{self, MessageList, ThreadFilter};
use super::common;
use super::paged::{LoadState, PageReq, Paged};
use super::{Kind, debounce};
use crate::error::Res;
use crate::room::timeline::{Viewer, ViewerMsg};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

/// Translated action text of a system message (the sidebar's preview keys).
pub fn system_text(ws: &Workspace, m: &RcMessage, use_real: bool) -> String {
    let p = room_list::preview(ws, m, RoomType::Channel, false, use_real);
    let arg = p.text.unwrap_or_default();
    let s = t!(p.i18n_key.as_str(), text = arg).into_owned();
    if s.ends_with(p.i18n_key.as_str()) {
        t!("preview.system.generic").into_owned()
    } else {
        s
    }
}

/// Lines of the root message a Threads panel item shows, as in Rocket.Chat.
const THREAD_PREVIEW_LINES: usize = 2;

/// Characters kept of a thread root in the Threads panel (the view shows two
/// lines of it).
const THREAD_PREVIEW_MAX_CHARS: usize = 400;

/// Plain-text preview of a thread root for the Threads panel: the root's text
/// (see [`thread_preview_text`]), else the sidebar's label for what it
/// carries ("Image: shot.png", "Attachment: quoted text", "Encrypted
/// message", …), else "Message".
fn thread_preview(ws: &Workspace, m: &RcMessage, use_real: bool, clock_24h: bool) -> String {
    if m.is_system() {
        return system_text(ws, m, use_real);
    }
    // The sidebar's label of a root that is not plain text.
    let label = |m: &RcMessage| {
        let mut p = room_list::preview(ws, m, RoomType::Direct, false, use_real);
        p.own = false;
        p.author = None;
        (p.kind != PreviewKind::Text).then(|| crate::workspace::sidebar::preview_text(&p))
    };
    if let Some(l) = label(m) {
        return l;
    }
    thread_preview_text(m, clock_24h).unwrap_or_else(|| {
        // Only quote permalinks in the text (a quote reply or a forward
        // without a comment): label what the attachments carry.
        // The label names the first attachment by its title, else its text:
        // that text is the quoted message's raw markdown, so flatten it the
        // same way (spoilers masked, markers dropped).
        let mut bare = m.clone();
        bare.msg.clear();
        bare.md = None;
        if let Some(a) = bare.attachments.as_mut().and_then(|l| l.first_mut())
            && a.title.is_none()
            && let Some(raw) = a.text.as_deref()
        {
            let quotes = a.attachments.as_deref().unwrap_or_default();
            a.text = match &a.md {
                Some(md) if !md.is_empty() => flatten(md, quotes, clock_24h),
                _ => flatten(&crate::e2e::markdown::parse(raw), quotes, clock_24h),
            };
        }
        label(&bare).unwrap_or_else(|| t!("preview.text").into_owned())
    })
}

/// One-line plain text of a thread root for the Threads panel, like the root
/// Rocket.Chat clamps to two lines there: mentions, emoji, links and inline
/// code as text, timestamps formatted, spoilers masked as "•••", code blocks,
/// quotes, lists and tables flattened into the same line, the blank-label
/// permalinks of quoted messages dropped, whitespace collapsed, at most
/// [`THREAD_PREVIEW_MAX_CHARS`] characters ending in "…". A root without
/// `md` is parsed from `msg`. `None` when nothing is left (an
/// attachment-only, quote-only or empty root, which the caller labels).
fn thread_preview_text(m: &RcMessage, clock_24h: bool) -> Option<String> {
    let quotes = m.attachments.as_deref().unwrap_or_default();
    match &m.md {
        Some(md) if !md.is_empty() => flatten(md, quotes, clock_24h),
        _ => flatten(&crate::e2e::markdown::parse(&m.msg), quotes, clock_24h),
    }
}

/// [`thread_preview_text`] of an `md` tree whose message carries the
/// attachments `quotes` (their permalinks are dropped).
fn flatten(
    md: &[rocketchat::models::md::Block],
    quotes: &[rocketchat::models::MessageAttachment],
    clock_24h: bool,
) -> Option<String> {
    use rocketchat::models::md::{Block, Inline};
    struct Walk<'a> {
        quotes: &'a [rocketchat::models::MessageAttachment],
        clock_24h: bool,
        out: String,
    }
    impl Walk<'_> {
        fn line(&mut self, v: &[Inline]) {
            v.iter().for_each(|i| self.inline(i));
            self.out.push(' ');
        }
        fn block(&mut self, b: &Block) {
            match b {
                Block::Paragraph { value }
                | Block::BigEmoji { value }
                | Block::Heading { value, .. } => self.line(value),
                Block::Code { value, .. } => {
                    for l in value {
                        self.out.push_str(&l.value.value);
                        self.out.push(' ');
                    }
                }
                Block::SpoilerBlock { .. } => self.out.push_str("••• "),
                Block::Quote { value } | Block::Blockquote { value } => {
                    value.iter().for_each(|b| self.block(b));
                }
                Block::UnorderedList { value } | Block::OrderedList { value } => {
                    value.iter().for_each(|i| self.line(&i.value));
                }
                Block::Tasks { value } => value.iter().for_each(|t| self.line(&t.value)),
                Block::ListItem { value, .. } => self.line(value),
                Block::Table { value, .. } => {
                    for row in
                        std::iter::once(&value.header).chain(value.rows.iter().map(|r| &r.value))
                    {
                        row.iter().for_each(|c| self.line(&c.value));
                    }
                }
                Block::Katex { value } => {
                    self.out.push_str(value);
                    self.out.push(' ');
                }
                Block::LineBreak | Block::HorizontalRule { .. } | Block::Unknown(_) => {}
            }
        }
        /// Emoji shortcodes become the Unicode emoji where Rocket.Chat has
        /// one (custom emoji stay `:name:`).
        fn inline(&mut self, i: &Inline) {
            match i {
                Inline::Bold { value } | Inline::Italic { value } | Inline::Strike { value } => {
                    value.iter().for_each(|i| self.inline(i));
                }
                Inline::Spoiler { .. } => self.out.push_str("•••"),
                Inline::Timestamp { value, .. } => self.out.push_str(
                    &crate::room::render::format_timestamp(value, self.clock_24h),
                ),
                Inline::Link { value } if self.quote_link(value) => {}
                Inline::Emoji(e) => {
                    let code = e
                        .short_code
                        .as_deref()
                        .or(e.value.as_ref().map(|v| v.value.as_str()));
                    match (&e.unicode, code) {
                        (Some(u), _) => self.out.push_str(u),
                        (None, Some(code)) => match rustket_core::emoji::to_unicode(code) {
                            Some(u) => self.out.push_str(u),
                            None => {
                                self.out.push(':');
                                self.out.push_str(code.trim_matches(':'));
                                self.out.push(':');
                            }
                        },
                        (None, None) => {}
                    }
                }
                other => other.plain_text(&mut self.out),
            }
        }
        /// The `[ ](permalink)` Rocket.Chat puts in quote replies and
        /// forwards: a blank label and an attachment's `message_link`.
        fn quote_link(&self, v: &rocketchat::models::md::LinkValue) -> bool {
            let src = v.src.value.as_str();
            let blank = v.label.as_slice().iter().all(|i| {
                let mut s = String::new();
                i.plain_text(&mut s);
                s.trim().is_empty()
            });
            blank
                && self
                    .quotes
                    .iter()
                    .any(|a| a.message_link.as_deref() == Some(src))
        }
    }
    let mut w = Walk {
        quotes,
        clock_24h,
        out: String::new(),
    };
    md.iter().for_each(|b| w.block(b));
    let mut flat = w.out.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > THREAD_PREVIEW_MAX_CHARS
        && let Some((cut, _)) = flat.char_indices().nth(THREAD_PREVIEW_MAX_CHARS - 1)
    {
        flat.truncate(cut);
        flat.truncate(flat.trim_end().len());
        flat.push('…');
    }
    (!flat.is_empty()).then_some(flat)
}

/// Follower avatars shown before "+N" in a Threads panel item.
const SHOWN_FOLLOWERS: usize = 2;

/// Size of a follower avatar in a Threads panel item.
const FOLLOWER_AVATAR: f32 = 16.0;

/// Size of the follow bell of a Threads panel item.
const BELL_SIZE: f32 = 28.0;

/// The "+N" after the shown follower avatars, if any are left.
fn more_followers(total: usize) -> Option<String> {
    (total > SHOWN_FOLLOWERS).then(|| format!("+{}", total - SHOWN_FOLLOWERS))
}

/// "N followers", the tooltip of the participants.
fn followers_tip(total: usize) -> String {
    t!(
        format!(
            "room_panel.followers_{}",
            crate::room::render::plural(total as u64)
        ),
        count = total
    )
    .into_owned()
}

/// Puts `me` into the thread's followers (`replies`) or takes it out.
fn set_following(m: &mut RcMessage, me: &str, on: bool) {
    let replies = m.replies.get_or_insert_with(Vec::new);
    replies.retain(|u| u != me);
    if on {
        replies.push(me.to_owned());
    }
}

/// A follow or unfollow from the Threads panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Toggle {
    /// Following after the toggle.
    want: bool,
    /// The call is in flight.
    busy: bool,
    /// A server copy showed the new state: later copies are trusted.
    confirmed: bool,
}

/// Brings a server copy of a thread in line with a toggle of it: until a
/// copy shows the toggled state, copies are older than the toggle and get
/// its state.
fn settle(toggles: &mut HashMap<String, Toggle>, m: &mut RcMessage, me: Option<&str>) {
    let (Some(me), Some(t)) = (me, toggles.get_mut(&m.id)) else {
        return;
    };
    if follows(m, Some(me)) == t.want {
        t.confirmed = true;
        if !t.busy {
            toggles.remove(&m.id);
        }
    } else if !t.confirmed {
        set_following(m, me, t.want);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Threads,
    List(MessageList),
    Search,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Filter(ThreadFilter),
    Page(PageReq, Res<Paginated<RcMessage>>),
    More,
    Retry,
    View(ViewerMsg),
    OpenThread(String),
    OpenUser(String),
    /// A result was clicked: show it in its thread or in the timeline.
    Show(String),
    /// The bell of a Threads panel item: follow or unfollow the thread.
    ToggleFollow(String),
    /// A follow (`true`) or unfollow call of the thread finished.
    Followed(String, bool, Res<()>),
}

pub struct Messages {
    mode: Mode,
    query: String,
    query_seq: u64,
    filter: ThreadFilter,
    list: Paged<RcMessage>,
    viewer: Viewer,
    /// Follow toggles of threads in the list, by id.
    toggles: HashMap<String, Toggle>,
    /// Usernames of the followers shown as avatars, by user id.
    names: HashMap<String, String>,
}

impl Messages {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            query: String::new(),
            query_seq: 0,
            filter: ThreadFilter::All,
            list: Paged::default(),
            viewer: Viewer::new(),
            toggles: HashMap::new(),
            names: HashMap::new(),
        }
    }

    /// Workspace changes: threads take the server's newer copies (replies,
    /// followers), keeping a follow toggle's state until the server shows
    /// it; encrypted results that were not decrypted yet are tried again
    /// (keys may have arrived; processed ones are skipped).
    pub fn on_changes(&mut self, changes: &Changes, ctx: &UpdateCtx<'_>) {
        if self.mode == Mode::Threads && !changes.parents.is_empty() {
            let me = ctx.workspace.user_id();
            for m in &mut self.list.items {
                let Some(copy) = changes.parents.get(&m.id) else {
                    continue;
                };
                let older = matches!((copy.updated_at, m.updated_at), (Some(c), Some(o)) if c < o);
                if copy.rid != m.rid || older {
                    continue;
                }
                *m = copy.clone();
                settle(&mut self.toggles, m, me);
            }
            self.learn_names(ctx.workspace);
        }
        crate::e2e::shown::process_list(ctx.workspace, ctx.server, &mut self.list.items);
    }

    /// Usernames of the shown followers that are not known yet, from me,
    /// presence, and the authors and mentions of the list and the loaded
    /// messages (avatars by username).
    fn learn_names(&mut self, ws: &Workspace) {
        let mut wanted: HashSet<&str> = self
            .list
            .items
            .iter()
            .flat_map(|m| m.replies.iter().flatten().take(SHOWN_FOLLOWERS))
            .map(String::as_str)
            .filter(|uid| !self.names.contains_key(*uid))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let mut found = Vec::new();
        if let (Some(id), Some(name)) = (ws.user_id(), ws.username())
            && wanted.remove(id)
        {
            found.push((id.to_owned(), name.to_owned()));
        }
        wanted.retain(
            |uid| match ws.presence().get(uid).and_then(|p| p.username.as_ref()) {
                Some(name) => {
                    found.push(((*uid).to_owned(), name.clone()));
                    false
                }
                None => true,
            },
        );
        let messages = || self.list.items.iter().chain(ws.loaded_messages());
        let authors = messages().map(|m| (m.u.id.as_str(), m.u.username.as_deref()));
        let mentioned = messages()
            .flat_map(|m| m.mentions.iter().flatten())
            .map(|u| (u.id.as_str(), u.username.as_deref()));
        for (id, name) in authors.chain(mentioned) {
            if wanted.is_empty() {
                break;
            }
            if let Some(name) = name
                && wanted.remove(id)
            {
                found.push((id.to_owned(), name.to_owned()));
            }
        }
        self.names.extend(found);
    }

    /// The image lightbox, if open, and the message that closes it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        self.viewer
            .overlay(ctx)
            .map(|e| (e.map(Msg::View), Msg::View(ViewerMsg::CloseImage)))
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        if self.mode == Mode::Search && self.query.trim().is_empty() {
            self.list.clear();
            return Action::none();
        }
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let rid = ctx.rid.to_owned();
        let q = self.query.clone();
        let map = move |r| Msg::Page(req, r);
        let task = match self.mode {
            Mode::Threads => {
                let f = self.filter;
                ctx.rest_task(move |rest| api::threads(rest, rid, f, q, req.offset), map)
            }
            Mode::List(which) => ctx.rest_task(
                move |rest| api::message_list(rest, rid, which, req.offset),
                map,
            ),
            Mode::Search => ctx.rest_task(move |rest| api::search(rest, rid, q, req.offset), map),
        };
        Action::task(task)
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => self.reload(ctx),
            Msg::Search(_) => Action::none(),
            Msg::Filter(f) => {
                self.filter = f;
                self.reload(ctx)
            }
            Msg::Page(req, r) => {
                // Encrypted results are decrypted before they are shown.
                let me = ctx.workspace.user_id();
                let toggles = &mut self.toggles;
                let r = r.map(|mut page| {
                    for m in &mut page.items {
                        settle(toggles, m, me);
                    }
                    crate::e2e::shown::process_list(ctx.workspace, ctx.server, &mut page.items);
                    page
                });
                self.list.finish(req, r);
                if self.mode == Mode::Threads {
                    self.learn_names(ctx.workspace);
                }
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::View(m) => self.viewer.update(m, ctx).map(Msg::View),
            Msg::Show(id) => match self.list.items.iter().find(|m| m.id == id) {
                Some(m) if self.mode == Mode::Threads => {
                    Action::effect(Effect::OpenPanel(Kind::Thread(m.id.clone())))
                }
                Some(m) => Action::effect(Effect::ShowMessage(Box::new(m.clone()))),
                None => Action::none(),
            },
            Msg::OpenThread(tmid) => Action::effect(Effect::OpenPanel(Kind::Thread(tmid))),
            Msg::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            Msg::ToggleFollow(id) => {
                let busy = self.toggles.get(&id).is_some_and(|t| t.busy);
                let me = ctx.workspace.user_id();
                let (Some(me), Some(m), false) =
                    (me, self.list.items.iter_mut().find(|m| m.id == id), busy)
                else {
                    return Action::none();
                };
                let want = !follows(m, Some(me));
                set_following(m, me, want);
                self.toggles.insert(
                    id.clone(),
                    Toggle {
                        want,
                        busy: true,
                        confirmed: false,
                    },
                );
                self.learn_names(ctx.workspace);
                let tmid = id.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        if want {
                            rest.follow_message(&tmid).await
                        } else {
                            rest.unfollow_message(&tmid).await
                        }
                    },
                    move |r| Msg::Followed(id, want, r),
                ))
            }
            Msg::Followed(id, want, r) => {
                let Some(t) = self.toggles.get_mut(&id) else {
                    return Action::none();
                };
                t.busy = false;
                match r {
                    Ok(()) => {
                        if t.confirmed {
                            self.toggles.remove(&id);
                        }
                        Action::none()
                    }
                    // A server copy already showed the new state: the call
                    // went through, only its answer was lost.
                    Err(_) if t.confirmed => {
                        self.toggles.remove(&id);
                        Action::none()
                    }
                    Err(e) => {
                        self.toggles.remove(&id);
                        if let Some(me) = ctx.workspace.user_id()
                            && let Some(m) = self.list.items.iter_mut().find(|m| m.id == id)
                        {
                            set_following(m, me, !want);
                        }
                        Action::effect(common::error_toast(
                            &t!("room_panel.thread_follow_failed"),
                            &e,
                        ))
                    }
                }
            }
        }
    }

    fn empty_text(&self) -> String {
        match self.mode {
            Mode::Threads => t!("room_panel.no_threads"),
            Mode::List(MessageList::Pinned) => t!("room_panel.no_pinned"),
            Mode::List(MessageList::Starred) => t!("room_panel.no_starred"),
            Mode::List(MessageList::Mentions) => t!("room_panel.no_mentions"),
            Mode::Search => t!("room_panel.no_results"),
        }
        .into_owned()
    }

    fn empty_icon(&self) -> Icon {
        match self.mode {
            Mode::Threads => Icon::MessageCircle,
            Mode::List(MessageList::Pinned) => Icon::Pin,
            Mode::List(MessageList::Starred) => Icon::Star,
            Mode::List(MessageList::Mentions) => Icon::AtSign,
            Mode::Search => Icon::Search,
        }
    }

    /// A compact line for system messages (topic changed, user joined, …),
    /// like the web's `SystemMessage` in these lists.
    fn system_row<'a>(&'a self, m: &'a RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let use_real = common::use_real_names(ctx);
        let username = m.u.username.clone().unwrap_or_default();
        let name = common::user_display(ws, use_real, &username, m.u.name.as_deref());
        let line = row![
            icon::sized(Icon::Info, 13.0).style(style::text_hint),
            text(system_text(ws, m, use_real))
                .size(TEXT_CAPTION)
                .style(style::text_secondary)
                .width(Length::Fill),
            text(common::time_label(m.ts, ctx))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let line = column![
            mouse_area(text(name).size(TEXT_CAPTION).font(font_semibold()))
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::OpenUser(username)),
            line
        ]
        .spacing(2);
        container(line)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill)
            .into()
    }

    /// A follower's avatar: by username when known (the picture or letter
    /// shared with the rest of the app); otherwise the `/avatar/uid/`
    /// picture, or a user icon while it loads or when the server only has
    /// a letter avatar for them (whose letter is not known here).
    fn follower_face<'a>(&self, uid: &str, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        if let Some(username) = self.names.get(uid) {
            return common::user_avatar(ctx, username, username, FOLLOWER_AVATAR);
        }
        let src = ctx.images.avatar(
            ctx.server_id,
            ctx.server,
            &room_list::Avatar::UserId {
                uid: uid.to_owned(),
            },
            "",
        );
        if src.image.is_some() {
            return avatar(src, FOLLOWER_AVATAR, AvatarShape::Rounded).into();
        }
        container(icon::sized(Icon::User, FOLLOWER_AVATAR * 0.7).style(style::text_hint))
            .center(FOLLOWER_AVATAR)
            .style(|theme| {
                let t = rustket_ui::theme::tokens(theme);
                container::Style {
                    background: Some(t.border.into()),
                    border: iced::border::rounded(2),
                    ..container::Style::default()
                }
            })
            .into()
    }

    /// Followers (up to two avatars and "+N"), the replies and the last
    /// reply under a thread's preview, as in Rocket.Chat's thread list.
    fn thread_meta<'a>(&'a self, m: &RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let mut foot = row![].spacing(SPACE_2).align_y(Alignment::Center);
        let followers = m.replies.as_deref().unwrap_or_default();
        if !followers.is_empty() {
            let total = followers.len();
            let mut shown = row![].spacing(2).align_y(Alignment::Center);
            for uid in followers.iter().take(SHOWN_FOLLOWERS) {
                shown = shown.push(self.follower_face(uid, ctx));
            }
            if let Some(more) = more_followers(total) {
                shown = shown.push(text(more).size(TEXT_CAPTION).style(style::text_hint));
            }
            foot = foot.push(tooltip(shown, followers_tip(total), TooltipPosition::Top));
        }
        foot = foot.push(
            row![
                icon::sized(Icon::MessageCircle, 13.0).style(style::text_link),
                text(t!("room_panel.replies", count = m.tcount.unwrap_or(0)).into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_link),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center),
        );
        // Cut, not wrapped, in a narrow panel: the row stays one line.
        if let Some(tlm) = m.tlm {
            foot = foot.push(
                ellipsis(
                    t!(
                        "room_panel.last_reply",
                        time = common::time_label(Some(tlm), ctx)
                    )
                    .into_owned(),
                )
                .size(TEXT_CAPTION)
                .style(|t| style::text_hint(t).color),
            );
        }
        foot.into()
    }

    /// The right-hand column of a Threads panel item: the follow bell and,
    /// under it, the unread dot.
    fn thread_side<'a>(&'a self, m: &RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let mut side = column![]
            .spacing(SPACE_2)
            .width(BELL_SIZE)
            .align_x(Alignment::Center);
        let me = ctx.workspace.user_id();
        if me.is_some() {
            let following = follows(m, me);
            // Pressable while a call is in flight too (`ToggleFollow`
            // ignores it): a click on the bell never opens the thread.
            side = side.push(icon_button_with(
                if following { Icon::Bell } else { Icon::BellOff },
                Some(Msg::ToggleFollow(m.id.clone())),
                if following {
                    t!("room_panel.thread_following").into_owned()
                } else {
                    t!("room_panel.thread_not_following").into_owned()
                },
                following,
                BELL_SIZE,
            ));
        }
        let unread = crate::room::timeline::parent_thread_kind(
            ctx.workspace.subscription(ctx.rid),
            &m.id,
            ctx.prefs.indicator_scheme,
        );
        if let Some(kind) = unread {
            let dot = dot_in(kind, 8.0, BadgePlace::Content);
            side = side.push(
                match crate::room::timeline::rc_thread_dot_tip(kind, ctx.prefs.indicator_scheme) {
                    Some(tip) => tooltip(dot, tip, TooltipPosition::Top),
                    None => dot,
                },
            );
        }
        side.into()
    }

    fn row<'a>(&'a self, m: &'a RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        if m.is_system() {
            return self.system_row(m, ctx);
        }
        let ws = ctx.workspace;
        let use_real = common::use_real_names(ctx);
        let username = m.u.username.clone().unwrap_or_default();
        let name = m
            .alias
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| common::user_display(ws, use_real, &username, m.u.name.as_deref()));
        let head = row![
            mouse_area(ellipsis(name.clone()).size(TEXT_BODY).font(font_semibold()))
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::OpenUser(username.clone())),
            text(common::time_label(m.ts, ctx))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let content = if self.mode == Mode::Threads {
            let preview = clamp(thread_preview(ws, m, use_real, ctx.prefs.clock_24h))
                .lines(THREAD_PREVIEW_LINES)
                .size(TEXT_BODY);
            column![head, preview, self.thread_meta(m, ctx)].spacing(2)
        } else {
            column![head, self.viewer.body(m, ctx).map(Msg::View)].spacing(2)
        };
        let mut line = row![
            common::user_avatar(ctx, &username, &name, 28.0),
            content.width(Length::Fill)
        ]
        .spacing(SPACE_3);
        if self.mode == Mode::Threads {
            line = line.push(self.thread_side(m, ctx));
        }
        let cell = container(line)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill);
        let target = if self.mode == Mode::Threads {
            Msg::OpenThread(m.id.clone())
        } else {
            Msg::Show(m.id.clone())
        };
        // On click, not on press: a press in the body may start a text
        // selection, and links in it act on their own.
        rustket_ui::widget::selectable::click_area(cell, target)
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let mut top = column![].spacing(SPACE_2).padding([SPACE_3, SPACE_4]);
        match self.mode {
            Mode::Threads => {
                top = top
                    .push(common::search_field(
                        &self.query,
                        t!("room_panel.search_threads").into_owned(),
                        Msg::Query,
                    ))
                    .push(common::filter_pills(
                        &ThreadFilter::ALL.map(|f| {
                            (
                                f,
                                match f {
                                    ThreadFilter::All => t!("room_panel.all"),
                                    ThreadFilter::Following => t!("room_panel.following"),
                                    ThreadFilter::Unread => t!("room_panel.unread"),
                                }
                                .into_owned(),
                            )
                        }),
                        self.filter,
                        Msg::Filter,
                    ));
            }
            Mode::Search => {
                top = top.push(common::search_field(
                    &self.query,
                    t!("room_panel.search_placeholder").into_owned(),
                    Msg::Query,
                ));
                if let Some(h) = crate::e2e::room::search_hint(ctx.workspace, ctx.rid) {
                    top = top.push(h);
                }
            }
            Mode::List(_) => {}
        }
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            LoadState::Idle if self.mode == Mode::Search && !self.list.loaded => {
                common::empty(Icon::Search, t!("room_panel.search_hint").into_owned())
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(self.empty_icon(), self.empty_text())
            }
            _ => {
                let mut col = column![].spacing(0);
                for (i, m) in self.list.items.iter().enumerate() {
                    if i > 0 {
                        col = col.push(container(divider()).padding([0.0, SPACE_4]));
                    }
                    col = col.push(self.row(m, ctx));
                }
                col = col.push(common::load_more(
                    self.list.state == LoadState::LoadingMore,
                    self.list.has_more(),
                    Msg::More,
                ));
                scrollable(col)
                    .height(Length::Fill)
                    .style(style::scrollable)
                    .into()
            }
        };
        if matches!(self.mode, Mode::List(_)) {
            body
        } else {
            column![top, body].height(Length::Fill).into()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn system_text_is_translated() {
        let ws = Workspace::new();
        let m: RcMessage = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "msg": "Release", "t": "room_changed_topic",
            "u": {"_id": "a", "username": "admin"}
        }))
        .unwrap();
        assert!(m.is_system());
        assert_eq!(system_text(&ws, &m, false), "Topic changed: Release");
        let m: RcMessage = serde_json::from_value(serde_json::json!({
            "_id": "m", "rid": "r", "msg": "", "t": "some-new-type",
            "u": {"_id": "a", "username": "admin"}
        }))
        .unwrap();
        assert_eq!(
            system_text(&ws, &m, false),
            t!("preview.system.generic").into_owned()
        );
    }

    fn root(v: serde_json::Value) -> RcMessage {
        let mut base = serde_json::json!({
            "_id": "T1", "rid": "R1", "msg": "", "ts": "2026-11-20T14:00:00.000Z",
            "u": {"_id": "b", "username": "bob"}, "tcount": 21,
            "tlm": "2026-11-20T14:48:00.000Z"
        });
        for (k, x) in v.as_object().unwrap() {
            base[k] = x.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn thread_preview_labels_roots_without_text() {
        let ws = Workspace::new();
        let text = root(serde_json::json!({"msg": "Deploy\n\n**today**?"}));
        assert_eq!(thread_preview(&ws, &text, false, true), "Deploy today?");
        let image = root(serde_json::json!({
            "file": {"_id": "f", "name": "shot.png", "type": "image/png"},
            "files": [{"_id": "f", "name": "shot.png", "type": "image/png"}],
            "attachments": [{"title": "shot.png", "image_url": "/file-upload/f/shot.png"}]
        }));
        assert_eq!(thread_preview(&ws, &image, false, true), "Image: shot.png");
        let quote = root(serde_json::json!({
            "attachments": [{"text": "quoted text", "message_link": "http://chat.test/x"}]
        }));
        assert_eq!(
            thread_preview(&ws, &quote, false, true),
            "Attachment: quoted text"
        );
        assert_eq!(
            thread_preview(&ws, &root(serde_json::json!({})), false, true),
            "Message"
        );
        let encrypted = root(serde_json::json!({"t": "e2e", "msg": "AAAA", "e2e": "pending"}));
        assert_eq!(
            thread_preview(&ws, &encrypted, false, true),
            t!("preview.encrypted").into_owned()
        );
        // "You:" and author prefixes of the sidebar are not used here.
        let mut ws = Workspace::new();
        ws.apply_me(rocketchat::models::User {
            id: "b".into(),
            username: Some("bob".into()),
            ..Default::default()
        });
        assert_eq!(thread_preview(&ws, &image, false, true), "Image: shot.png");
    }

    const FORWARDED: &str =
        include_str!("../../../../rocketchat/tests/fixtures/message_forwarded.json");
    const SYNC: &str =
        include_str!("../../../../rocketchat/tests/fixtures/chat_sync_messages.json");

    /// Quote replies and forwards keep the quoted message's permalink in
    /// `msg` as `[ ](link)`: the preview drops it, as the timeline does.
    #[test]
    fn thread_preview_drops_quote_permalinks() {
        let ws = Workspace::new();
        let sync: serde_json::Value = serde_json::from_str(SYNC).unwrap();
        let quoting = sync["result"]["updated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["_id"] == "TxqM7zbRF9puS6otS")
            .unwrap();
        let quoting: RcMessage = serde_json::from_value(quoting.clone()).unwrap();
        assert_eq!(thread_preview(&ws, &quoting, false, true), "quoting it");
        // The same text without `md` is parsed from `msg`.
        let mut raw = quoting.clone();
        raw.md = None;
        assert_eq!(thread_preview(&ws, &raw, false, true), "quoting it");
        // Nothing but permalinks: the attachment label, as in the sidebar.
        let forwarded: RcMessage = serde_json::from_str(FORWARDED).unwrap();
        assert_eq!(
            thread_preview(&ws, &forwarded, false, true),
            "Attachment: first forwarded"
        );
        // A blank-label link to elsewhere is not a quote and stays.
        let other = root(serde_json::json!({"msg": "[ ](https://example.com/x) see"}));
        assert_eq!(
            thread_preview(&ws, &other, false, true),
            "https://example.com/x see"
        );
    }

    /// A quote-only root is labelled from the quoted text: its spoilers stay
    /// masked and its markup is dropped, from `md` or from the raw text.
    #[test]
    fn thread_preview_masks_spoilers_in_quoted_text() {
        let ws = Workspace::new();
        let mut forwarded: RcMessage = serde_json::from_str(FORWARDED).unwrap();
        let a = &mut forwarded.attachments.as_mut().unwrap()[0];
        a.text = Some("**Answer:** ||the butler did it||".into());
        a.md = Some(
            serde_json::from_value(serde_json::json!([{"type": "PARAGRAPH", "value": [
                {"type": "BOLD", "value": [{"type": "PLAIN_TEXT", "value": "Answer:"}]},
                {"type": "PLAIN_TEXT", "value": " "},
                {"type": "SPOILER", "value": [
                    {"type": "PLAIN_TEXT", "value": "the butler did it"}
                ]}
            ]}]))
            .unwrap(),
        );
        assert_eq!(
            thread_preview(&ws, &forwarded, false, true),
            "Attachment: Answer: •••"
        );
        let a = &mut forwarded.attachments.as_mut().unwrap()[0];
        a.md = None;
        assert_eq!(
            thread_preview(&ws, &forwarded, false, true),
            "Attachment: Answer: •••"
        );
    }

    #[test]
    fn thread_preview_masks_spoilers_and_formats_timestamps() {
        let ws = Workspace::new();
        let spoiler = root(serde_json::json!({
            "msg": "Answer: ||the butler did it||",
            "md": [{"type": "PARAGRAPH", "value": [
                {"type": "PLAIN_TEXT", "value": "Answer: "},
                {"type": "SPOILER", "value": [{"type": "PLAIN_TEXT", "value": "the butler did it"}]}
            ]}]
        }));
        assert_eq!(thread_preview(&ws, &spoiler, false, true), "Answer: •••");
        let mut raw = spoiler.clone();
        raw.md = None;
        assert_eq!(thread_preview(&ws, &raw, false, true), "Answer: •••");
        let ts = root(serde_json::json!({"msg": "Freeze at <t:1767225600:f>"}));
        let p = thread_preview(&ws, &ts, false, true);
        assert!(p.starts_with("Freeze at "), "{p}");
        assert!(!p.contains("1767225600"), "{p}");
        assert!(p.contains("2026") || p.contains("2025"), "{p}");
    }

    #[test]
    fn thread_preview_flattens_markdown() {
        let m = root(serde_json::json!({
            "msg": "ignored when md is present",
            "md": [
                {"type": "HEADING", "level": 1, "value": [{"type": "PLAIN_TEXT", "value": "Release"}]},
                {"type": "PARAGRAPH", "value": [
                    {"type": "MENTION_USER", "value": {"type": "PLAIN_TEXT", "value": "bob"}},
                    {"type": "PLAIN_TEXT", "value": " see "},
                    {"type": "BOLD", "value": [{"type": "PLAIN_TEXT", "value": "this"}]},
                    {"type": "PLAIN_TEXT", "value": " "},
                    {"type": "EMOJI", "value": {"type": "PLAIN_TEXT", "value": "thumbsup"}, "shortCode": "thumbsup"},
                    {"type": "PLAIN_TEXT", "value": " "},
                    {"type": "EMOJI", "value": {"type": "PLAIN_TEXT", "value": "party_parrot"}, "shortCode": "party_parrot"},
                    {"type": "PLAIN_TEXT", "value": " "},
                    {"type": "EMOJI", "value": null, "unicode": "🎉"},
                    {"type": "PLAIN_TEXT", "value": " in "},
                    {"type": "MENTION_CHANNEL", "value": {"type": "PLAIN_TEXT", "value": "general"}},
                    {"type": "PLAIN_TEXT", "value": " "},
                    {"type": "LINK", "value": {"src": {"type": "PLAIN_TEXT", "value": "https://example.com"},
                     "label": [{"type": "PLAIN_TEXT", "value": "docs"}]}},
                    {"type": "PLAIN_TEXT", "value": " "},
                    {"type": "INLINE_CODE", "value": {"type": "PLAIN_TEXT", "value": "x = 1"}}
                ]},
                {"type": "CODE", "language": "rust", "value": [
                    {"type": "CODE_LINE", "value": {"type": "PLAIN_TEXT", "value": "fn main() {"}},
                    {"type": "CODE_LINE", "value": {"type": "PLAIN_TEXT", "value": "}"}}
                ]},
                {"type": "QUOTE", "value": [{"type": "PARAGRAPH", "value": [{"type": "PLAIN_TEXT", "value": "quoted\ntext"}]}]},
                {"type": "LINE_BREAK", "value": null},
                {"type": "UNORDERED_LIST", "value": [
                    {"type": "LIST_ITEM", "value": [{"type": "PLAIN_TEXT", "value": "one"}]},
                    {"type": "LIST_ITEM", "value": [{"type": "PLAIN_TEXT", "value": "two"}]}
                ]}
            ]
        }));
        assert_eq!(
            thread_preview_text(&m, true).unwrap(),
            "Release @bob see this 👍\u{fe0f} :party_parrot: 🎉 in #general docs x = 1 fn main() { } quoted text one two"
        );
    }

    #[test]
    fn thread_preview_without_md_parses_msg_and_caps_long_text() {
        let text = |msg: &str| thread_preview_text(&root(serde_json::json!({"msg": msg})), true);
        assert_eq!(
            text("  line one\n\n\tline two  ").unwrap(),
            "line one line two"
        );
        assert_eq!(
            text("**bold** _it_ ~strike~ `code` @bob").unwrap(),
            "bold it strike code @bob"
        );
        let p = text(&"word ".repeat(200)).unwrap();
        assert_eq!(p.chars().count(), THREAD_PREVIEW_MAX_CHARS);
        assert!(p.ends_with("word…"), "{p}");
        // A cut after a space drops it.
        let spaced = format!(
            "{} tail of the message",
            "x".repeat(THREAD_PREVIEW_MAX_CHARS - 2)
        );
        let p = text(&spaced).unwrap();
        assert!(p.ends_with("x…"), "{p}");
        assert_eq!(p.chars().count(), THREAD_PREVIEW_MAX_CHARS - 1);
        let exact = "x".repeat(THREAD_PREVIEW_MAX_CHARS);
        assert_eq!(text(&exact).unwrap(), exact);
    }

    #[test]
    fn thread_preview_keeps_cjk_and_thai_whole() {
        let text = |msg: &str| thread_preview_text(&root(serde_json::json!({"msg": msg})), true);
        let p = text(&"这是一个很长的中文消息".repeat(60)).unwrap();
        assert_eq!(p.chars().count(), THREAD_PREVIEW_MAX_CHARS);
        assert!(p.ends_with('…'));
        let th = "ภาษาไทย ที่ยาวมาก";
        assert_eq!(text(th).unwrap(), th);
    }

    #[test]
    fn thread_preview_text_is_none_without_text() {
        let text = |v| thread_preview_text(&root(v), true);
        assert_eq!(text(serde_json::json!({})), None);
        assert_eq!(text(serde_json::json!({"msg": " \n "})), None);
        assert_eq!(
            text(serde_json::json!({
                "file": {"_id": "f", "name": "shot.png", "type": "image/png"},
                "attachments": [{"title": "shot.png", "image_url": "/file-upload/f/shot.png"}]
            })),
            None
        );
    }

    mod view {
        #![allow(clippy::unwrap_used)]
        use super::super::*;
        use super::root;
        use crate::images::Images;
        use iced::advanced::widget::{Id, Operation};
        use iced::{Point, Rectangle, Size, mouse};
        use iced_runtime::user_interface::{Cache, UserInterface};
        use rocketchat::ServerUrl;
        use rocketchat::models::{Room, Subscription, User, UserRef};
        use rustket_core::ServerId;
        use rustket_core::config::UiPrefs;
        use rustket_core::sync::InitialData;

        struct Texts(Vec<(Rectangle, String)>);
        impl Operation for Texts {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
                operate(self);
            }
            fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, text: &str) {
                self.0.push((bounds, text.to_owned()));
            }
        }

        fn workspace() -> Workspace {
            let mut ws = Workspace::new();
            ws.apply_initial(InitialData {
                me: User {
                    id: "me".into(),
                    username: Some("me".into()),
                    ..Default::default()
                },
                subscriptions: vec![Subscription {
                    id: "s1".into(),
                    rid: "R1".into(),
                    name: "general".into(),
                    t: RoomType::Channel,
                    u: UserRef {
                        id: "me".into(),
                        username: Some("me".into()),
                        name: None,
                    },
                    open: true,
                    tunread: Some(vec!["T1".into()]),
                    ..Default::default()
                }],
                rooms: vec![Room {
                    id: "R1".into(),
                    t: RoomType::Channel,
                    name: Some("general".into()),
                    ..Default::default()
                }],
                ..Default::default()
            });
            ws
        }

        struct Harness {
            ws: Workspace,
            sid: ServerId,
            url: ServerUrl,
            images: Images,
            prefs: UiPrefs,
        }

        impl Harness {
            fn new() -> Self {
                Self {
                    ws: workspace(),
                    sid: ServerId("messages-tests".into()),
                    url: ServerUrl::parse("http://chat.test").unwrap(),
                    images: Images::new(None),
                    prefs: UiPrefs::default(),
                }
            }
            fn update(&mut self) -> UpdateCtx<'_> {
                UpdateCtx {
                    server_id: &self.sid,
                    server: &self.url,
                    rid: "R1",
                    workspace: &mut self.ws,
                    session: None,
                    images: &self.images,
                    prefs: &self.prefs,
                }
            }
            fn view(&self) -> ViewCtx<'_> {
                ViewCtx {
                    server_id: &self.sid,
                    server: &self.url,
                    rid: "R1",
                    workspace: &self.ws,
                    images: &self.images,
                    prefs: &self.prefs,
                    connected: true,
                    panel: None,
                    now: std::time::Instant::now(),
                }
            }
        }

        fn threads(items: Vec<RcMessage>) -> Messages {
            let mut panel = Messages::new(Mode::Threads);
            let req = panel.list.reset();
            let n = items.len();
            panel.list.finish(
                req,
                Ok(Paginated {
                    raw_len: n,
                    count: n as u64,
                    items,
                    offset: 0,
                    total: n as u64,
                    dropped: 0,
                }),
            );
            panel
        }

        fn following(panel: &Messages, id: &str) -> bool {
            let m = panel.list.items.iter().find(|m| m.id == id).unwrap();
            follows(m, Some("me"))
        }

        fn parent_copy(replies: &[&str], updated: &str) -> RcMessage {
            root(serde_json::json!({"replies": replies, "_updatedAt": updated}))
        }

        fn changes_with(m: RcMessage) -> Changes {
            let mut c = Changes::default();
            c.parents.insert(m.id.clone(), m);
            c
        }

        fn failure() -> crate::error::Failure {
            crate::error::Failure::from(rocketchat::Error::Protocol("boom".into()))
        }

        /// The item follows when `replies` holds my id, as in Rocket.Chat.
        #[test]
        fn following_comes_from_replies() {
            let panel = threads(vec![
                root(serde_json::json!({"_id": "A", "replies": ["b", "me"]})),
                root(serde_json::json!({"_id": "B", "replies": ["b"]})),
                root(serde_json::json!({"_id": "C"})),
            ]);
            assert!(following(&panel, "A"));
            assert!(!following(&panel, "B"));
            assert!(!following(&panel, "C"));
        }

        /// A click shows the new state at once; a failure reverts it and
        /// toasts; clicks while the call is in flight are ignored.
        #[test]
        fn toggle_is_optimistic_and_reverts_on_error() {
            let mut h = Harness::new();
            let mut panel = threads(vec![parent_copy(&["b"], "2026-11-20T14:48:00.000Z")]);
            let a = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            assert!(a.effects.is_empty());
            assert!(following(&panel, "T1"));
            assert!(panel.toggles["T1"].busy);
            // A second click while busy changes nothing.
            let _ = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            assert!(following(&panel, "T1"));
            let a = panel.update(
                Msg::Followed("T1".into(), true, Err(failure())),
                &mut h.update(),
            );
            assert!(!following(&panel, "T1"));
            assert!(panel.toggles.is_empty());
            assert!(matches!(
                a.effects.as_slice(),
                [Effect::Toast(rustket_ui::widget::BannerKind::Error, title, _)]
                    if *title == t!("room_panel.thread_follow_failed")
            ));
            // Unfollowing works the same way.
            let mut panel = threads(vec![parent_copy(&["b", "me"], "2026-11-20T14:48:00.000Z")]);
            let _ = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            assert!(!following(&panel, "T1"));
            let a = panel.update(Msg::Followed("T1".into(), false, Ok(())), &mut h.update());
            assert!(a.effects.is_empty());
            assert!(!following(&panel, "T1"));
        }

        /// Server copies replace the item (newer ones only); a copy that
        /// does not show the toggle yet keeps the toggled state, and once a
        /// copy showed it, later copies are trusted again.
        #[test]
        fn server_copies_after_a_toggle_keep_the_right_state() {
            let mut h = Harness::new();
            let mut panel = threads(vec![parent_copy(&["b"], "2026-11-20T14:48:00.000Z")]);
            let _ = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            // A reply's broadcast sent before the follow was applied.
            let mut stale = parent_copy(&["b", "c"], "2026-11-20T14:49:00.000Z");
            stale.tcount = Some(22);
            panel.on_changes(&changes_with(stale), &h.update());
            assert!(following(&panel, "T1"));
            assert_eq!(panel.list.items[0].tcount, Some(22));
            let _ = panel.update(Msg::Followed("T1".into(), true, Ok(())), &mut h.update());
            assert!(following(&panel, "T1"), "kept until the server shows it");
            // The follow's own broadcast confirms it.
            panel.on_changes(
                &changes_with(parent_copy(&["b", "c", "me"], "2026-11-20T14:50:00.000Z")),
                &h.update(),
            );
            assert!(following(&panel, "T1"));
            assert!(panel.toggles.is_empty());
            // An older copy is ignored ...
            panel.on_changes(
                &changes_with(parent_copy(&["b"], "2026-11-20T14:40:00.000Z")),
                &h.update(),
            );
            assert!(following(&panel, "T1"));
            // ... a newer one decides (unfollowed elsewhere).
            panel.on_changes(
                &changes_with(parent_copy(&["b", "c"], "2026-11-20T14:55:00.000Z")),
                &h.update(),
            );
            assert!(!following(&panel, "T1"));
            // A copy of another room's message with the same id is not used.
            let mut other = parent_copy(&["b", "c", "me"], "2026-11-20T15:00:00.000Z");
            other.rid = "R2".into();
            panel.on_changes(&changes_with(other), &h.update());
            assert!(!following(&panel, "T1"));
        }

        /// The broadcast may come before the call returns: the toggle ends
        /// with the call and later copies are trusted.
        #[test]
        fn confirmation_before_the_response_ends_the_toggle() {
            let mut h = Harness::new();
            let mut panel = threads(vec![parent_copy(&["b"], "2026-11-20T14:48:00.000Z")]);
            let _ = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            panel.on_changes(
                &changes_with(parent_copy(&["b", "me"], "2026-11-20T14:50:00.000Z")),
                &h.update(),
            );
            assert!(panel.toggles["T1"].confirmed);
            let _ = panel.update(Msg::Followed("T1".into(), true, Ok(())), &mut h.update());
            assert!(panel.toggles.is_empty());
            assert!(following(&panel, "T1"));
        }

        /// Review: a failed call after a server copy already showed the new
        /// state keeps that state and does not toast.
        #[test]
        fn failure_after_confirmation_keeps_the_server_state() {
            let mut h = Harness::new();
            let mut panel = threads(vec![parent_copy(&["b"], "2026-11-20T14:48:00.000Z")]);
            let _ = panel.update(Msg::ToggleFollow("T1".into()), &mut h.update());
            panel.on_changes(
                &changes_with(parent_copy(&["b", "me"], "2026-11-20T14:50:00.000Z")),
                &h.update(),
            );
            let a = panel.update(
                Msg::Followed("T1".into(), true, Err(failure())),
                &mut h.update(),
            );
            assert!(a.effects.is_empty(), "{:?}", a.effects.len());
            assert!(following(&panel, "T1"));
            assert!(panel.toggles.is_empty());
        }

        /// Review: the list stashed behind a thread opened from it keeps
        /// taking the server's copies, so Back shows follow changes made
        /// meanwhile and a toggle still in flight gets confirmed.
        #[test]
        fn stashed_list_keeps_taking_server_copies() {
            use crate::room::panel::{Body, BodyMsg, Kind, Message as PanelMsg, Panel};
            let mut h = Harness::new();
            let mut p = Panel::new("R1");
            p.open(Kind::Threads);
            p.pending = None;
            p.body = Some(Body::Messages(threads(vec![parent_copy(
                &["b"],
                "2026-11-20T14:48:00.000Z",
            )])));
            let g_list = p.generation();
            let _ = p.update(
                PanelMsg::Body(g_list, BodyMsg::Messages(Msg::ToggleFollow("T1".into()))),
                &mut h.update(),
            );
            p.open(Kind::Thread("T1".into()));
            let _ = p.update(
                PanelMsg::Body(
                    g_list,
                    BodyMsg::Messages(Msg::Followed("T1".into(), true, Ok(()))),
                ),
                &mut h.update(),
            );
            // The follow's broadcast, then an unfollow from the thread view.
            for (replies, at) in [
                (&["b", "me"][..], "2026-11-20T14:50:00.000Z"),
                (&["b"][..], "2026-11-20T14:52:00.000Z"),
            ] {
                let _ = p.on_changes(&changes_with(parent_copy(replies, at)), &mut h.update());
            }
            let _ = p.update(PanelMsg::Back, &mut h.update());
            match &p.body {
                Some(Body::Messages(m)) => {
                    assert!(!following(m, "T1"));
                    assert!(m.toggles.is_empty());
                }
                _ => panic!("threads list expected"),
            }
        }

        #[test]
        fn followers_label_and_plurals() {
            assert_eq!(more_followers(0), None);
            assert_eq!(more_followers(2), None);
            assert_eq!(more_followers(3).as_deref(), Some("+1"));
            assert_eq!(more_followers(12).as_deref(), Some("+10"));
            assert_eq!(followers_tip(1), "1 follower");
            assert_eq!(followers_tip(5), "5 followers");
            for (n, key) in [(1, "one"), (3, "few"), (5, "many"), (21, "one")] {
                assert_eq!(crate::room::render::plural_for("ru", n), key);
                let ru = t!(
                    format!("room_panel.followers_{key}"),
                    locale = "ru",
                    count = n
                );
                assert!(ru.starts_with(&n.to_string()), "{ru}");
            }
            assert_eq!(
                t!("room_panel.followers_few", locale = "ru", count = 3),
                "3 подписчика"
            );
            assert_eq!(
                t!("room_panel.followers_many", locale = "ru", count = 5),
                "5 подписчиков"
            );
        }

        fn texts(panel: &Messages, ctx: &ViewCtx<'_>, width: f32) -> Vec<(Rectangle, String)> {
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::font_regular(),
                iced::Pixels(TEXT_BODY),
            ));
            let mut ui = UserInterface::build(
                panel.view(ctx),
                Size::new(width, 700.0),
                Cache::default(),
                &mut renderer,
            );
            let mut t = Texts(Vec::new());
            ui.operate(&renderer, &mut t);
            t.0
        }

        /// "+N" after two avatars; none with two followers or fewer.
        #[test]
        fn followers_show_plus_n() {
            let h = Harness::new();
            let panel = threads(vec![root(
                serde_json::json!({"replies": ["b", "c", "d", "me", "e"]}),
            )]);
            let shown = texts(&panel, &h.view(), 420.0);
            assert!(shown.iter().any(|(_, t)| t == "+3"), "{shown:?}");
            assert!(!shown.iter().any(|(_, t)| t == "5"), "{shown:?}");
            let panel = threads(vec![root(serde_json::json!({"replies": ["b", "me"]}))]);
            let shown = texts(&panel, &h.view(), 420.0);
            assert!(!shown.iter().any(|(_, t)| t.starts_with('+')), "{shown:?}");
        }

        /// Review: a follower whose username is unknown was drawn with "?"
        /// initials. Usernames now also come from presence and mentions;
        /// still unknown ones show a user icon, never "?".
        #[test]
        fn unknown_followers_show_no_question_mark() {
            let mut h = Harness::new();
            h.ws.apply_user_status(
                "c",
                Some("carol"),
                rocketchat::models::UserStatus::Online,
                None,
            );
            let mut panel = threads(vec![
                root(serde_json::json!({"replies": ["c", "zz"]})),
                root(serde_json::json!({
                    "_id": "T2", "replies": ["d"],
                    "mentions": [{"_id": "d", "username": "dave"}]
                })),
            ]);
            panel.learn_names(&h.ws);
            assert_eq!(panel.names.get("c").map(String::as_str), Some("carol"));
            assert_eq!(panel.names.get("d").map(String::as_str), Some("dave"));
            assert!(!panel.names.contains_key("zz"));
            let shown = texts(&panel, &h.view(), 420.0);
            assert!(shown.iter().any(|(_, t)| t == "C"), "{shown:?}");
            assert!(shown.iter().any(|(_, t)| t == "D"), "{shown:?}");
            assert!(!shown.iter().any(|(_, t)| t == "?"), "{shown:?}");
        }

        /// A long root takes two lines, with the replies row (followers
        /// first) right under it and the bell column to the right of both,
        /// at narrow and wide panel widths; a click on it opens the thread,
        /// a click on the bell toggles following and does not.
        #[test]
        fn thread_item_is_two_lines_with_meta_under_it() {
            let ws = workspace();
            let (sid, url) = (
                ServerId("messages-tests".into()),
                ServerUrl::parse("http://chat.test").unwrap(),
            );
            let images = Images::new(None);
            let prefs = UiPrefs::default();
            let ctx = ViewCtx {
                server_id: &sid,
                server: &url,
                rid: "R1",
                workspace: &ws,
                images: &images,
                prefs: &prefs,
                connected: true,
                panel: None,
                now: std::time::Instant::now(),
            };
            let long = "A long root message that goes on and on. ".repeat(30);
            let mut panel = Messages::new(Mode::Threads);
            let req = panel.list.reset();
            let items = vec![root(
                serde_json::json!({"msg": long, "replies": ["b", "me", "c", "d"]}),
            )];
            panel.list.finish(
                req,
                Ok(Paginated {
                    raw_len: 1,
                    count: 1,
                    items,
                    offset: 0,
                    total: 1,
                    dropped: 0,
                }),
            );
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                rustket_ui::theme::font_regular(),
                iced::Pixels(TEXT_BODY),
            ));
            let line = TEXT_BODY * 1.3;
            for width in [280.0, 420.0, 900.0] {
                let size = Size::new(width, 700.0);
                let mut ui =
                    UserInterface::build(panel.view(&ctx), size, Cache::default(), &mut renderer);
                let mut texts = Texts(Vec::new());
                ui.operate(&renderer, &mut texts);
                let find = |pred: &dyn Fn(&str) -> bool| {
                    texts
                        .0
                        .iter()
                        .find(|(_, t)| pred(t))
                        .map(|(b, _)| *b)
                        .unwrap()
                };
                let preview = find(&|t| t.starts_with("A long root"));
                let meta = find(&|t| t == "Replies: 21");
                let name = find(&|t| t == "bob");
                let more = find(&|t| t == "+2");
                assert!(
                    (more.center_y() - meta.center_y()).abs() < 1.0 && more.x < meta.x,
                    "{width}: {more:?} {meta:?}"
                );
                // The bell glyph: the only text right of the preview.
                let bell = texts
                    .0
                    .iter()
                    .find(|(b, _)| b.x >= preview.x + preview.width)
                    .map(|(b, _)| *b)
                    .unwrap();
                assert!(
                    bell.x + bell.width <= width - SPACE_4 + 0.5,
                    "{width}: {bell:?}"
                );
                assert!(bell.y < preview.y + preview.height, "{width}: {bell:?}");
                assert!(
                    (preview.height - 2.0 * line).abs() < 0.5,
                    "{width}: {preview:?}"
                );
                assert!(preview.y > name.y, "{width}");
                let gap = meta.y - (preview.y + preview.height);
                assert!(
                    (0.0..=SPACE_2).contains(&gap),
                    "{width}: {preview:?} {meta:?}"
                );

                let at = Point::new(preview.center_x(), preview.center_y());
                let events = [
                    iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ];
                let mut out = Vec::new();
                ui.update(
                    &events,
                    mouse::Cursor::Available(at),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut out,
                );
                assert!(
                    out.iter()
                        .any(|m| matches!(m, Msg::OpenThread(id) if id == "T1")),
                    "{width}: {out:?}"
                );

                let at = Point::new(bell.center_x(), bell.center_y());
                let events = [
                    iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ];
                let mut out = Vec::new();
                ui.update(
                    &events,
                    mouse::Cursor::Available(at),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut out,
                );
                assert!(
                    matches!(out.as_slice(), [Msg::ToggleFollow(id)] if id == "T1"),
                    "{width}: {out:?}"
                );
            }
        }
    }
}
