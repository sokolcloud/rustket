//! Message timeline of a room: grouped message rows with day/unread
//! separators, rich bodies (`room::render`), reactions, thread/discussion
//! summaries, hover toolbar and context menu actions, downloads, dialogs.
//! Scrolling is anchored to the bottom; older history loads near the top
//! (`Effect::LoadOlder`), new messages wait below a "jump to latest" button
//! while the user reads older ones. Only a window of the newest loaded
//! messages is built, and every row is a `lazy` widget.
//! Strings: `locales/room_timeline.yml` (`room_timeline.*`).

mod actions;
mod dialogs;
mod row;
mod transfer;

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use chrono::{Datelike, Local, Utc};
use iced::widget::scrollable::{RelativeOffset, Viewport};
use iced::widget::{
    button, column, container, keyed_column, lazy, row as hrow, scrollable, sensor, space, stack,
    text,
};
use iced::{Alignment, Element, Length, Padding, Subscription, Task};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::timeline::{
    DeliveryState, Timeline as CoreTimeline, TimelineItem, is_new_day, is_sequential,
};
use rustket_core::workspace::{Changes, Dirty};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    BannerKind, date_separator, empty_state, loading_placeholder, modal_keyed, spinner,
    unread_separator,
};
use serde_json::json;

use super::panel::Kind;
use super::render::{self, FileRef, RenderMsg, RenderState, Transfer, system};
use super::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};
use crate::error::Res;
use crate::images::ImageRequest;

pub use actions::{Act, available as available_actions, permalink};
use dialogs::Dialog;
use transfer::DlEvent;

/// Rows built initially and added per step when scrolling up.
const WINDOW: usize = 60;
const WINDOW_STEP: usize = 40;
/// Distance from the top (px) that loads more.
const TOP_THRESHOLD: f32 = 400.0;
/// Distance from the bottom (px) that still counts as "at the bottom".
const BOTTOM_THRESHOLD: f32 = 24.0;

#[derive(Debug, Clone)]
pub enum Message {
    /// Scrolled near the top: request the next older page.
    ReachedTop,
    Scrolled(Viewport),
    JumpToLatest,
    /// Retry a failed history page.
    LoadOlder,
    Render(RenderMsg),
    Act(String, Act),
    /// Open (`Some(mid)`) or close the "more" menu of a message.
    Menu(Option<String>),
    /// Toggle reaction `emoji` on message `mid`.
    React(String, String),
    OpenThread(String),
    OpenRoom(String),
    OpenUser(String),
    Retry(String),
    Discard(String),
    ActionDone(Act, Res<()>),
    CloseDialog,
    ConfirmDelete,
    Deleted(String, Res<()>),
    ReportText(String),
    SubmitReport,
    Reported(String, Res<()>),
    ToggleZoom,
    /// Download the lightbox image privately and open it in the image viewer.
    OpenImageExternally,
    SaveImage,
    SaveTo(FileRef, Option<PathBuf>),
    Transfer(FileRef, DlEvent, bool),
    Opened(Result<(), String>),
}

pub struct Timeline {
    rid: String,
    render: RenderState,
    /// Newest messages built as rows.
    window: usize,
    at_bottom: bool,
    /// While scrolled up: the newest message shown (`ts` ms, id); newer ones
    /// wait below the "jump to latest" button so the view does not move.
    frozen: Option<(i64, String)>,
    /// Newest item seen by `on_changes` (grows the window instead of
    /// shifting it when messages arrive at the bottom).
    newest: Option<(i64, String)>,
    menu: Option<String>,
    dialog: Option<Dialog>,
    scroll_id: iced::widget::Id,
}

fn item_key(item: &TimelineItem) -> (i64, &str) {
    let m = &item.message;
    (
        m.ts.or(m.updated_at).map_or(0, |d| d.timestamp_millis()),
        m.id.as_str(),
    )
}

impl Timeline {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            render: RenderState::new(),
            window: WINDOW,
            at_bottom: true,
            frozen: None,
            newest: None,
            menu: None,
            dialog: None,
            scroll_id: iced::widget::Id::unique(),
        }
    }

    fn toast_error(title: String, body: String) -> Effect {
        Effect::Toast(BannerKind::Error, title, body)
    }

    fn toast_ok(title: String) -> Effect {
        Effect::Toast(BannerKind::Success, title, String::new())
    }

    fn unread(ctx: &UpdateCtx<'_>) -> bool {
        ctx.workspace
            .subscription(ctx.rid)
            .is_some_and(|s| s.unread > 0 || s.alert)
    }

    fn snap_to_bottom(&self) -> Task<Message> {
        iced::widget::operation::snap_to(self.scroll_id.clone(), RelativeOffset::START)
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::ReachedTop => self.reach_top(ctx),
            Message::LoadOlder => Action::effect(Effect::LoadOlder),
            Message::Scrolled(vp) => self.scrolled(vp, ctx),
            Message::JumpToLatest => {
                self.frozen = None;
                self.at_bottom = true;
                self.window = WINDOW;
                // A click: the window is focused and this room is visible.
                let mut a = Action::task(self.snap_to_bottom());
                if Self::unread(ctx) {
                    a = a.with(Effect::MarkRead);
                }
                a
            }
            Message::Render(r) => self.render_msg(r, ctx),
            Message::Act(mid, act) => {
                self.menu = None;
                self.act(&mid, act, ctx)
            }
            Message::Menu(m) => {
                self.menu = m;
                Action::none()
            }
            Message::React(mid, emoji) => {
                if !rustket_core::permissions::can_react(ctx.workspace, ctx.rid) {
                    return Action::none();
                }
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.react(&mid, &emoji, None).await },
                    |r| Message::ActionDone(Act::React, r),
                ))
            }
            Message::OpenThread(mid) => Action::effect(Effect::OpenPanel(Kind::Thread(mid))),
            Message::OpenRoom(rid) => {
                if ctx.workspace.room(&rid).is_some() {
                    Action::effect(Effect::OpenRoom(rid))
                } else {
                    Action::effect(Self::toast_error(
                        t!("room_timeline.not_a_member").into_owned(),
                        String::new(),
                    ))
                }
            }
            Message::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            // One send path for room and thread messages: the composer outbox.
            Message::Retry(mid) => Action::effect(Effect::Compose(ComposeIntent::Retry(mid))),
            Message::Discard(mid) => Action::effect(Effect::Compose(ComposeIntent::Discard(mid))),
            Message::ActionDone(act, r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(Self::toast_error(
                    t!("room_timeline.action_failed", action = act.label()).into_owned(),
                    e.user_text(),
                )),
            },
            Message::CloseDialog => {
                self.dialog = None;
                Action::none()
            }
            Message::ConfirmDelete => {
                let Some(Dialog::Delete { mid, busy }) = &mut self.dialog else {
                    return Action::none();
                };
                *busy = true;
                let (rid, mid) = (ctx.rid.to_owned(), mid.clone());
                let done = mid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.delete_message(&rid, &mid).await },
                    move |r| Message::Deleted(done.clone(), r),
                ))
            }
            Message::Deleted(mid, r) => {
                if matches!(&self.dialog, Some(Dialog::Delete { mid: m, .. }) if *m == mid) {
                    self.dialog = None;
                }
                match r {
                    Ok(()) => Action::none(),
                    Err(e) => Action::effect(Self::toast_error(
                        t!("room_timeline.delete_failed").into_owned(),
                        e.user_text(),
                    )),
                }
            }
            Message::ReportText(s) => {
                if let Some(Dialog::Report { description, .. }) = &mut self.dialog {
                    *description = s;
                }
                Action::none()
            }
            Message::SubmitReport => {
                let Some(Dialog::Report {
                    mid,
                    description,
                    busy,
                }) = &mut self.dialog
                else {
                    return Action::none();
                };
                if description.trim().is_empty() || *busy {
                    return Action::none();
                }
                *busy = true;
                let body = json!({"messageId": mid, "description": description.trim()});
                let done = mid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.post_ok("chat.reportMessage", &body).await },
                    move |r| Message::Reported(done.clone(), r),
                ))
            }
            Message::Reported(mid, r) => match r {
                Ok(()) => {
                    if matches!(&self.dialog, Some(Dialog::Report { mid: m, .. }) if *m == mid) {
                        self.dialog = None;
                    }
                    Action::effect(Self::toast_ok(t!("room_timeline.reported").into_owned()))
                }
                Err(e) => {
                    if let Some(Dialog::Report { mid: m, busy, .. }) = &mut self.dialog
                        && *m == mid
                    {
                        *busy = false;
                    }
                    Action::effect(Self::toast_error(
                        t!("room_timeline.report_failed").into_owned(),
                        e.user_text(),
                    ))
                }
            },
            Message::ToggleZoom => {
                if let Some(Dialog::Image { zoom, .. }) = &mut self.dialog {
                    *zoom = !*zoom;
                }
                Action::none()
            }
            Message::OpenImageExternally => match &self.dialog {
                Some(Dialog::Image { image, .. }) => {
                    let ext = image
                        .mime
                        .as_deref()
                        .and_then(transfer::media_ext)
                        .filter(|e| transfer::media_ext(&format!("image/{e}")).is_some())
                        .or_else(|| transfer::image_ext_from_url(&image.url));
                    let f = FileRef {
                        mid: image.mid.clone(),
                        url: image.url.clone(),
                        name: image.title.clone(),
                        size: None,
                        mime: image.mime.clone(),
                    };
                    self.open_privately(f, ext, ctx)
                }
                _ => Action::none(),
            },
            Message::SaveImage => match &self.dialog {
                Some(Dialog::Image { image, .. }) => {
                    let f = FileRef {
                        mid: image.mid.clone(),
                        url: image.url.clone(),
                        name: if image.title.is_empty() {
                            "image".into()
                        } else {
                            image.title.clone()
                        },
                        size: None,
                        mime: image.mime.clone(),
                    };
                    self.render_msg(RenderMsg::Download(f), ctx)
                }
                _ => Action::none(),
            },
            Message::SaveTo(f, path) => match path {
                Some(p) => self.start_download(f, p, transfer::Target::Chosen, false, ctx),
                None => Action::none(),
            },
            Message::Transfer(f, ev, open_after) => match ev {
                DlEvent::Progress(received, total) => {
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Running { received, total });
                    Action::none()
                }
                DlEvent::Done(path) => {
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Done(path.clone()));
                    if open_after {
                        Action::task(Task::perform(transfer::open_path(path), Message::Opened))
                    } else {
                        Action::effect(Effect::Toast(
                            BannerKind::Success,
                            t!("room_timeline.downloaded").into_owned(),
                            path.display().to_string(),
                        ))
                    }
                }
                DlEvent::Failed(e) => {
                    self.render
                        .set_transfer(&f.mid, &f.url, Transfer::Failed(e.clone()));
                    Action::effect(Self::toast_error(
                        t!("room_timeline.download_failed", name = f.name.as_str()).into_owned(),
                        e,
                    ))
                }
            },
            Message::Opened(r) => match r {
                Ok(()) => Action::none(),
                Err(e) => Action::effect(Self::toast_error(
                    t!("room_timeline.open_failed").into_owned(),
                    e,
                )),
            },
        }
    }

    fn scrolled(&mut self, vp: Viewport, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let from_bottom = vp.absolute_offset().y;
        let from_top = vp.absolute_offset_reversed().y;
        let was_bottom = self.at_bottom;
        self.at_bottom = from_bottom <= BOTTOM_THRESHOLD;
        let mut action = Action::none();
        if self.at_bottom && !was_bottom {
            // Back at the bottom: show what arrived meanwhile.
            self.frozen = None;
            self.window = WINDOW;
            // Scrolling happens in the visible room of a focused window.
            if Self::unread(ctx) {
                action = action.with(Effect::MarkRead);
            }
        } else if !self.at_bottom && self.frozen.is_none() {
            self.frozen = ctx
                .workspace
                .timeline(ctx.rid)
                .and_then(|t| t.iter().next_back())
                .map(|i| {
                    let (ts, id) = item_key(i);
                    (ts, id.to_owned())
                });
        }
        let short = vp.content_bounds().height <= vp.bounds().height + 1.0;
        if from_top < TOP_THRESHOLD || short {
            let more = self.reach_top(ctx);
            action.task = more.task;
            action.effects.extend(more.effects);
        }
        action
    }

    /// Near the top: build more of the loaded rows, else load an older page.
    fn reach_top(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        if tl.len() > self.window {
            self.window += WINDOW_STEP;
            return Action::none();
        }
        let p = &tl.pagination;
        if p.has_more_before && !p.loading_before && p.error.is_none() {
            Action::effect(Effect::LoadOlder)
        } else {
            Action::none()
        }
    }

    /// Downloads `f` into a new private media file with extension `ext` (from
    /// an allow-list of media types) and opens it with the system handler.
    fn open_privately(
        &mut self,
        f: FileRef,
        ext: Option<&'static str>,
        ctx: &UpdateCtx<'_>,
    ) -> Action<Message> {
        let dir = transfer::media_dir(ctx.images.disk_dir());
        match (ext, dir) {
            (Some(ext), Some(dir)) => {
                let path = transfer::media_path(&dir, ext);
                self.start_download(f, path, transfer::Target::Private, true, ctx)
            }
            _ => Action::effect(Self::toast_error(
                t!("room_timeline.unsupported_media").into_owned(),
                String::new(),
            )),
        }
    }

    fn start_download(
        &mut self,
        f: FileRef,
        path: PathBuf,
        target: transfer::Target,
        open_after: bool,
        ctx: &UpdateCtx<'_>,
    ) -> Action<Message> {
        let Some(rest) = ctx.rest() else {
            return Action::effect(Self::toast_error(
                t!("room_timeline.download_failed", name = f.name.as_str()).into_owned(),
                t!("room_timeline.offline").into_owned(),
            ));
        };
        self.render.set_transfer(
            &f.mid,
            &f.url,
            Transfer::Running {
                received: 0,
                total: f.size,
            },
        );
        let stream = transfer::download(rest, f.url.clone(), path, target);
        Action::task(Task::run(stream, move |ev| {
            Message::Transfer(f.clone(), ev, open_after)
        }))
    }

    fn render_msg(&mut self, r: RenderMsg, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if self.render.handle(&r) {
            return Action::none();
        }
        match r {
            RenderMsg::OpenUrl(u) => Action::effect(Effect::OpenUrl(u)),
            RenderMsg::OpenRoom(name) => match render::resolve_room(ctx.workspace, &name) {
                Some(rid) => Action::effect(Effect::OpenRoom(rid)),
                // Not joined: the web shows a preview; open the channel there.
                None => match actions::channel_url(ctx.server, ctx.workspace, &name) {
                    Some(url) => Action::effect(Effect::OpenUrl(url)),
                    None => Action::effect(Self::toast_error(
                        t!("room_timeline.room_not_found", name = name.as_str()).into_owned(),
                        String::new(),
                    )),
                },
            },
            RenderMsg::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            RenderMsg::Copy(s) => Action::task(iced::clipboard::write(s))
                .with(Self::toast_ok(t!("room_timeline.copied").into_owned())),
            RenderMsg::OpenImage(image) => {
                ctx.images.want(ImageRequest {
                    server_id: ctx.server_id.clone(),
                    url: image.url.clone(),
                });
                self.dialog = Some(Dialog::Image { image, zoom: false });
                Action::none()
            }
            RenderMsg::Download(f) => {
                let name = f.name.clone();
                Action::task(Task::perform(transfer::ask_save_path(name), move |p| {
                    Message::SaveTo(f.clone(), p)
                }))
            }
            RenderMsg::Play(f) => {
                let ext = f.mime.as_deref().and_then(transfer::media_ext);
                if ext.is_none() {
                    // Not a known audio/video type: let the user save it.
                    return self.render_msg(RenderMsg::Download(f), ctx);
                }
                self.open_privately(f, ext, ctx)
            }
            RenderMsg::OpenFile(p) => {
                Action::task(Task::perform(transfer::open_path(p), Message::Opened))
            }
            RenderMsg::ToggleSpoiler(..) | RenderMsg::ToggleCollapsed(..) => Action::none(),
        }
    }

    fn act(&mut self, mid: &str, act: Act, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(item) = ctx.workspace.timeline(ctx.rid).and_then(|t| t.get(mid)) else {
            return Action::none();
        };
        if item.state != DeliveryState::Sent
            || !actions::available(ctx.workspace, &item.message, Utc::now()).contains(&act)
        {
            // The toolbar was built before a time limit or a permission changed.
            return Action::effect(Self::toast_error(
                t!("room_timeline.action_unavailable").into_owned(),
                String::new(),
            ));
        }
        let m = item.message.clone();
        let mid = mid.to_owned();
        // A reply follows its parent thread.
        let thread = m.tmid.clone().unwrap_or_else(|| mid.clone());
        let rest_act = |ctx: &UpdateCtx<'_>, act: Act| {
            let mid = mid.clone();
            let thread = thread.clone();
            Action::task(ctx.rest_task(
                move |rest| async move {
                    match act {
                        Act::Pin => rest.pin_message(&mid).await.map(|_| ()),
                        Act::Unpin => rest.unpin_message(&mid).await,
                        Act::Star => rest.star_message(&mid).await,
                        Act::Unstar => rest.unstar_message(&mid).await,
                        Act::Follow => rest.follow_message(&thread).await,
                        Act::Unfollow => rest.unfollow_message(&thread).await,
                        _ => Ok(()),
                    }
                },
                move |r| Message::ActionDone(act, r),
            ))
        };
        match act {
            Act::React => Action::effect(Effect::Compose(ComposeIntent::React(mid))),
            Act::ReplyInThread => {
                Action::effect(Effect::Compose(ComposeIntent::ReplyInThread(mid)))
            }
            Act::Quote => Action::effect(Effect::Compose(ComposeIntent::Quote(mid))),
            Act::Edit => Action::effect(Effect::Compose(ComposeIntent::Edit(mid))),
            // Leaves the room first (the app), like the web client.
            Act::MarkUnread => Action::effect(Effect::MarkUnread(Some(mid))),
            Act::StartDiscussion => Action::effect(Effect::StartDiscussion { pmid: Some(mid) }),
            Act::CopyText => Action::task(iced::clipboard::write(render::plain_text(&m)))
                .with(Self::toast_ok(t!("room_timeline.copied").into_owned())),
            Act::CopyLink => Action::task(iced::clipboard::write(permalink(
                ctx.server,
                ctx.workspace,
                &m,
            )))
            .with(Self::toast_ok(t!("room_timeline.link_copied").into_owned())),
            Act::Delete => {
                self.dialog = Some(Dialog::Delete { mid, busy: false });
                Action::none()
            }
            Act::Report => {
                self.dialog = Some(Dialog::Report {
                    mid,
                    description: String::new(),
                    busy: false,
                });
                Action::task(iced::widget::operation::focus(dialogs::REPORT_INPUT_ID))
            }
            Act::Pin | Act::Unpin | Act::Star | Act::Unstar | Act::Follow | Act::Unfollow => {
                rest_act(ctx, act)
            }
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if !changes.timelines.contains(ctx.rid) {
            return Action::none();
        }
        let Some(tl) = ctx.workspace.timeline(ctx.rid) else {
            return Action::none();
        };
        // Forget state of deleted messages.
        if let Some(Dialog::Delete { mid, .. } | Dialog::Report { mid, .. }) = &self.dialog
            && !tl.contains(mid)
        {
            self.dialog = None;
        }
        if self.menu.as_ref().is_some_and(|m| !tl.contains(m)) {
            self.menu = None;
        }
        // New messages at the bottom grow the window rather than shifting
        // every row; it is trimmed back once in a while.
        let newest = tl.iter().next_back().map(|i| {
            let (ts, id) = item_key(i);
            (ts, id.to_owned())
        });
        if let (Some(prev), Some(_)) = (&self.newest, &newest)
            && self.at_bottom
            && self.frozen.is_none()
        {
            let added = tl
                .iter()
                .rev()
                .take_while(|i| item_key(i) > (prev.0, prev.1.as_str()))
                .count();
            self.window += added;
            if self.window > WINDOW + WINDOW_STEP {
                self.window = WINDOW;
            }
        }
        self.newest = newest;
        // An own message being sent while scrolled up: jump down to it.
        let own_pending = tl
            .iter()
            .next_back()
            .is_some_and(|i| i.state == DeliveryState::Sending);
        if self.frozen.is_some() && own_pending {
            self.frozen = None;
            self.at_bottom = true;
            return Action::task(self.snap_to_bottom());
        }
        Action::none()
    }

    /// Window focus is handled by the app (`mark_active_room_read`), which
    /// knows which server and room are visible.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let base = self.list(ctx);
        let overlay = self.dialog.as_ref().map(|d| {
            let key = match d {
                Dialog::Delete { .. } => 1u8,
                Dialog::Report { .. } => 2,
                Dialog::Image { .. } => 3,
            };
            let el = match d {
                Dialog::Delete { busy, .. } => dialogs::delete(*busy),
                Dialog::Report {
                    description, busy, ..
                } => dialogs::report(description, *busy),
                Dialog::Image { image, zoom } => {
                    dialogs::lightbox(image, *zoom, ctx.images.get(ctx.server_id, &image.url))
                }
            };
            (key, el)
        });
        modal_keyed(base, overlay, Some(Message::CloseDialog))
    }

    fn list<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let Some(tl) = ws.timeline(&self.rid) else {
            return loading_placeholder(Some(t!("room_timeline.loading").into_owned()));
        };
        if tl.is_empty() && !tl.pagination.loaded {
            if tl.pagination.error.is_some() && !tl.pagination.loading_before {
                return first_page_failed();
            }
            return loading_placeholder(Some(t!("room_timeline.loading").into_owned()));
        }
        let env = row::Env {
            server_id: ctx.server_id,
            server: ctx.server,
            ws,
            images: ctx.images,
            render: &self.render,
            prefs: ctx.prefs,
            rid: &self.rid,
        };
        let hidden = ws.settings().hide_system_messages();
        let room_sys = ws
            .room(&self.rid)
            .and_then(|e| e.room.as_ref())
            .and_then(|r| r.sys_mes.as_ref());
        let visible = |m: &RcMessage| {
            !m.t.as_deref()
                .is_some_and(|t| m.is_system() && system::is_hidden(t, &hidden, room_sys))
        };
        let me = ws.user_id().unwrap_or_default();
        let plan = plan(
            tl,
            visible,
            self.frozen.as_ref().map(|(ts, id)| (*ts, id.as_str())),
            self.window,
            me,
            ws.settings().grouping_period_secs(),
            &Local,
        );
        if plan.rows.is_empty() && plan.new_below == 0 {
            return empty_state(
                Icon::MessagesSquare,
                t!("room_timeline.empty_title"),
                t!("room_timeline.empty_subtitle"),
            );
        }

        let tunread = ws.subscription(&self.rid).and_then(|s| s.tunread.as_ref());
        let dark = render::mode() == rustket_ui::theme::Mode::Dark;
        let revs = (
            ws.revision(Dirty::SETTINGS | Dirty::PERMISSIONS | Dirty::ROLES | Dirty::ME),
            ws.revision(Dirty::EMOJI),
            room_state_key(ws, &self.rid),
            ctx.images.generation(ctx.server_id),
        );
        let prefs = (
            ctx.prefs.clock_24h,
            ws.use_real_names(ctx.prefs.use_real_names),
        );
        let locale = locale_key();
        let now = Utc::now();
        // Edit/delete limits are time based: own rows rebuild every minute.
        let minute = now.timestamp() / 60;
        let now_local = Local::now();
        let today = now_local.date_naive();
        let mut col = keyed_column::<u64, Message, _, _>([])
            .width(Length::Fill)
            .padding(Padding::from([SPACE_2, 0.0]));
        // Near the top (also when nothing scrolls yet): build more rows or
        // load the next page; the key re-arms it after every change.
        let top: Element<'a, Message> = if plan.more_above {
            space().height(1).into()
        } else {
            top_indicator(tl)
        };
        col = col.push(
            TOP_KEY,
            sensor(top)
                .key((tl.len(), self.window, tl.pagination.loading_before))
                .anticipate(TOP_THRESHOLD)
                .on_show(|_| Message::ReachedTop),
        );
        for r in &plan.rows {
            let item: &'a TimelineItem = r.item;
            let m = &item.message;
            let flags = row::RowFlags {
                sequential: r.sequential,
                menu_open: self.menu.as_deref() == Some(m.id.as_str()),
                thread_unread: tunread.is_some_and(|t| t.contains(&m.id)),
            };
            let state = match &item.state {
                DeliveryState::Sent => 0u8,
                DeliveryState::Sending => 1,
                DeliveryState::Failed { .. } => 2,
            };
            let own = !me.is_empty() && m.u.id == me;
            let day = r
                .new_day
                .then(|| m.ts.map(|ts| row::day_label(ts, now_local)));
            let dep = RowKey {
                msg: render::message_key(m),
                state,
                render: self.render.key(&m.id),
                flags,
                separators: (r.new_day, r.unread_before),
                dark,
                prefs,
                revs,
                images: row::images_key(&env, m),
                locale,
                minute: if own { minute } else { 0 },
                today: if r.new_day {
                    today.num_days_from_ce()
                } else {
                    0
                },
            };
            let unread_before = r.unread_before;
            col = col.push(
                id_key(&m.id),
                lazy(dep, move |_| {
                    let row = row::build(&env, item, flags, now);
                    if day.is_none() && !unread_before {
                        return row;
                    }
                    let mut c = column![].width(Length::Fill);
                    if let Some(Some(label)) = &day {
                        c = c.push(date_separator(label.clone()));
                    }
                    if unread_before {
                        c = c.push(unread_separator(t!("room_timeline.unread").into_owned()));
                    }
                    c.push(row).into()
                }),
            );
        }
        let new_below = plan.new_below;
        let list = scrollable(col)
            .id(self.scroll_id.clone())
            .anchor_bottom()
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::scrollable);
        if self.at_bottom && new_below == 0 {
            return list.into();
        }
        let label = if new_below > 0 {
            t!("room_timeline.new_messages", count = new_below).into_owned()
        } else {
            t!("room_timeline.jump_to_latest").into_owned()
        };
        let jump = button(
            hrow![
                icon::sized(Icon::ArrowDown, 14.0),
                text(label).size(TEXT_SMALL)
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(style::button_primary)
        .on_press(Message::JumpToLatest);
        stack![
            list,
            container(jump)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::End)
                .padding(SPACE_4)
        ]
        .into()
    }
}

/// Key of the top sensor in the keyed column (rows use [`id_key`]).
const TOP_KEY: u64 = 0;

fn id_key(id: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    id.hash(&mut h);
    h.finish().max(1)
}

/// Room/subscription state the row actions depend on (read-only, muted,
/// room roles …); `Dirty::ROOMS` would change with every new message.
fn room_state_key(ws: &rustket_core::Workspace, rid: &str) -> u64 {
    use rustket_core::permissions as perm;
    let mut h = std::hash::DefaultHasher::new();
    (
        perm::can_post(ws, rid),
        perm::can_react(ws, rid),
        perm::can_pin(ws, rid),
        perm::can_star(ws, rid),
        perm::is_read_only(ws, rid),
    )
        .hash(&mut h);
    let sub = ws.subscription(rid);
    sub.is_some().hash(&mut h);
    sub.and_then(|s| s.roles.as_ref()).hash(&mut h);
    h.finish()
}

fn locale_key() -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    rust_i18n::locale().hash(&mut h);
    h.finish()
}

/// One row to build, with its separators and grouping.
#[derive(Debug)]
struct PlannedRow<'a> {
    item: &'a TimelineItem,
    new_day: bool,
    unread_before: bool,
    sequential: bool,
}

/// What [`Timeline::list`] shows.
#[derive(Debug)]
struct Plan<'a> {
    rows: Vec<PlannedRow<'a>>,
    /// Visible messages newer than the frozen view (behind "jump to latest").
    new_below: usize,
    /// More visible loaded messages exist above the window.
    more_above: bool,
}

/// Picks the newest `window` visible messages (skipping what waits below a
/// frozen view), with day separators, the unread divider (above the first
/// visible unread message) and grouping. One extra older row is used as
/// grouping context only.
fn plan<'a, Tz: chrono::TimeZone>(
    tl: &'a CoreTimeline,
    visible: impl Fn(&RcMessage) -> bool,
    frozen: Option<(i64, &str)>,
    window: usize,
    me: &str,
    grouping_secs: i64,
    tz: &Tz,
) -> Plan<'a> {
    let divider_key = tl
        .unread_divider(me)
        .and_then(|id| tl.get(id))
        .map(item_key);
    let mut divider: Option<&'a str> = None;
    let mut reached_divider = divider_key.is_none();
    let mut new_below = 0usize;
    let mut picked: Vec<&'a TimelineItem> = Vec::with_capacity(window + 1);
    let mut exhausted = true;
    for item in tl.iter().rev() {
        let key = item_key(item);
        if let Some(dk) = divider_key
            && key <= dk
        {
            reached_divider = true;
        }
        if !visible(&item.message) {
            continue;
        }
        if let Some(dk) = divider_key
            && key >= dk
            && !item.is_pending()
            && item.message.u.id != me
        {
            divider = Some(item.message.id.as_str());
        }
        if let Some(f) = frozen
            && key > f
        {
            new_below += 1;
            continue;
        }
        if picked.len() > window {
            exhausted = false;
            break;
        }
        picked.push(item);
    }
    if !(reached_divider || exhausted) {
        divider = None;
    }
    let more_above = picked.len() > window;
    picked.reverse();
    let mut rows = Vec::with_capacity(picked.len());
    for (i, item) in picked.iter().enumerate() {
        if more_above && i == 0 {
            continue;
        }
        let m = &item.message;
        let prev = i.checked_sub(1).map(|p| &picked[p].message);
        rows.push(PlannedRow {
            item,
            new_day: m.ts.is_some() && is_new_day(prev, m, tz),
            unread_before: divider == Some(m.id.as_str()),
            sequential: prev.is_some_and(|p| is_sequential(p, m, grouping_secs, tz, divider)),
        });
    }
    Plan {
        rows,
        new_below,
        more_above,
    }
}

/// The first history page failed: error with a retry button.
fn first_page_failed<'a>() -> Element<'a, Message> {
    container(
        column![
            icon::sized(Icon::CircleAlert, 28.0).style(style::text_danger),
            text(t!("room_timeline.history_first_failed").into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
            button(text(t!("room_timeline.retry").into_owned()).size(TEXT_SMALL))
                .padding([4, 12])
                .style(style::button_secondary)
                .on_press(Message::LoadOlder),
        ]
        .spacing(SPACE_2)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

/// `lazy` dependency of a row.
#[derive(Clone, Copy)]
struct RowKey {
    msg: u64,
    state: u8,
    render: u64,
    flags: row::RowFlags,
    /// Day separator, unread divider above the row.
    separators: (bool, bool),
    dark: bool,
    prefs: (bool, bool),
    revs: (u64, u64, u64, u64),
    /// Which of the row's images are loaded.
    images: u64,
    locale: u64,
    /// Current minute for own rows (time-limited edit/delete), else 0.
    minute: i64,
    /// Current day for rows with a day separator ("Today"/"Yesterday").
    today: i32,
}

impl Hash for RowKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.msg.hash(h);
        self.state.hash(h);
        self.render.hash(h);
        self.flags.hash(h);
        self.separators.hash(h);
        self.dark.hash(h);
        self.prefs.hash(h);
        self.revs.hash(h);
        self.images.hash(h);
        self.locale.hash(h);
        self.minute.hash(h);
        self.today.hash(h);
    }
}

fn top_indicator<'a>(tl: &rustket_core::timeline::Timeline) -> Element<'a, Message> {
    let p = &tl.pagination;
    let content: Element<'a, Message> = if p.loading_before {
        hrow![
            spinner(16.0),
            text(t!("room_timeline.loading_older").into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_hint)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into()
    } else if p.error.is_some() {
        hrow![
            text(t!("room_timeline.history_failed").into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_danger),
            button(text(t!("room_timeline.retry").into_owned()).size(TEXT_CAPTION))
                .padding([0, 4])
                .style(style::button_link)
                .on_press(Message::LoadOlder)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center)
        .into()
    } else if !p.has_more_before && p.loaded {
        column![
            icon::sized(Icon::MessagesSquare, 28.0).style(style::text_hint),
            text(t!("room_timeline.start_of_conversation").into_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_2)
        .align_x(Alignment::Center)
        .into()
    } else {
        space().height(24).into()
    };
    container(content)
        .center_x(Length::Fill)
        .padding([SPACE_4, 0.0])
        .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use rocketchat::RcDate;
    use rocketchat::models::UserRef;

    fn m(id: &str, uid: &str, ts: i64, t: Option<&str>) -> RcMessage {
        RcMessage {
            id: id.into(),
            rid: "r".into(),
            msg: id.into(),
            t: t.map(Into::into),
            ts: RcDate::from_millis(ts),
            u: UserRef {
                id: uid.into(),
                username: Some(uid.into()),
                name: None,
            },
            ..Default::default()
        }
    }

    fn ids<'a>(p: &Plan<'a>) -> Vec<&'a str> {
        p.rows.iter().map(|r| r.item.message.id.as_str()).collect()
    }

    const T0: i64 = 1_800_000_000_000;

    fn timeline() -> CoreTimeline {
        let mut tl = CoreTimeline::new_room("r");
        tl.extend([
            m("a", "bob", T0, None),
            m("b", "bob", T0 + 1_000, None),
            m("j", "carol", T0 + 2_000, Some("uj")),
            m("c", "carol", T0 + 3_000, None),
            m("d", "me", T0 + 4_000, None),
            m("e", "bob", T0 + 5_000, None),
        ]);
        tl
    }

    #[test]
    fn plan_groups_and_hides() {
        let tl = timeline();
        let visible = |m: &RcMessage| m.t.as_deref() != Some("uj");
        let p = plan(&tl, visible, None, 60, "me", 300, &Utc);
        assert_eq!(ids(&p), ["a", "b", "c", "d", "e"]);
        assert!(!p.more_above);
        assert_eq!(p.new_below, 0);
        let seq: Vec<bool> = p.rows.iter().map(|r| r.sequential).collect();
        assert_eq!(seq, [false, true, false, false, false]);
        assert!(p.rows[0].new_day && !p.rows[1].new_day);
        // Window of 2: one extra row of context, not shown.
        let p = plan(&tl, visible, None, 2, "me", 300, &Utc);
        assert_eq!(ids(&p), ["d", "e"]);
        assert!(p.more_above);
        assert!(!p.rows[0].new_day, "the context row continues the day");
        // Frozen at "c": newer ones wait below.
        let p = plan(&tl, visible, Some((T0 + 3_000, "c")), 60, "me", 300, &Utc);
        assert_eq!(ids(&p), ["a", "b", "c"]);
        assert_eq!(p.new_below, 2);
    }

    #[test]
    fn unread_divider_skips_hidden_messages() {
        let mut tl = timeline();
        // Last seen after "b": the first unread is the hidden join.
        tl.set_unread_since(RcDate::from_millis(T0 + 1_500));
        assert_eq!(tl.unread_divider("me"), Some("j"));
        let visible = |m: &RcMessage| m.t.as_deref() != Some("uj");
        let p = plan(&tl, visible, None, 60, "me", 300, &Utc);
        let marked: Vec<&str> = p
            .rows
            .iter()
            .filter(|r| r.unread_before)
            .map(|r| r.item.message.id.as_str())
            .collect();
        assert_eq!(marked, ["c"]);
        // Shown when the join is visible.
        let p = plan(&tl, |_| true, None, 60, "me", 300, &Utc);
        assert!(
            p.rows
                .iter()
                .any(|r| r.unread_before && r.item.message.id == "j")
        );
        // Older than the window: no divider (and no wrong one).
        let p = plan(&tl, visible, None, 1, "me", 300, &Utc);
        assert!(p.rows.iter().all(|r| !r.unread_before));
    }

    #[test]
    fn render_state_toggles() {
        let mut s = RenderState::new();
        let k0 = s.key("m1");
        assert!(s.handle(&RenderMsg::ToggleSpoiler("m1".into(), 0)));
        assert!(s.is_revealed("m1", 0));
        assert_ne!(s.key("m1"), k0);
        assert_eq!(s.key("m2"), RenderState::new().key("m2"));
        assert!(s.handle(&RenderMsg::ToggleSpoiler("m1".into(), 0)));
        assert!(!s.is_revealed("m1", 0));
        assert!(!s.handle(&RenderMsg::OpenUrl("https://x".into())));
        s.set_transfer(
            "m1",
            "u",
            Transfer::Running {
                received: 1,
                total: None,
            },
        );
        assert_ne!(s.key("m1"), k0);
        s.forget("m1");
        assert_eq!(s.key("m1"), k0);
    }
}
