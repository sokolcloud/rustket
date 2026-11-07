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

use chrono::{Local, Utc};
use iced::widget::scrollable::{RelativeOffset, Viewport};
use iced::widget::{
    Column, button, column, container, lazy, row as hrow, scrollable, space, stack, text,
};
use iced::{Alignment, Element, Length, Padding, Subscription, Task};
use rocketchat::models::{Message as RcMessage, OutgoingMessage};
use rust_i18n::t;
use rustket_core::timeline::{DeliveryState, TimelineItem, is_new_day, is_sequential};
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
    Resent(String, Option<String>, Res<Box<RcMessage>>),
    ActionDone(Act, Res<()>),
    CloseDialog,
    ConfirmDelete,
    Deleted(Res<()>),
    ReportText(String),
    SubmitReport,
    Reported(Res<()>),
    ToggleZoom,
    OpenImageInBrowser,
    SaveImage,
    SaveTo(FileRef, Option<PathBuf>),
    Transfer(FileRef, DlEvent, bool),
    Opened(Result<(), String>),
    Focus(bool),
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
    focused: bool,
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
            focused: true,
            menu: None,
            dialog: None,
            scroll_id: iced::widget::Id::unique(),
        }
    }

    pub fn is_at_bottom(&self) -> bool {
        self.at_bottom
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
                let mut a = Action::task(self.snap_to_bottom());
                if self.focused && Self::unread(ctx) {
                    a = a.with(Effect::MarkRead);
                }
                a
            }
            Message::Focus(f) => {
                self.focused = f;
                if f && self.at_bottom && self.frozen.is_none() && Self::unread(ctx) {
                    Action::effect(Effect::MarkRead)
                } else {
                    Action::none()
                }
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
            Message::OpenRoom(rid) => Action::effect(Effect::OpenRoom(rid)),
            Message::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
            Message::Retry(mid) => self.retry(&mid, ctx),
            Message::Discard(mid) => {
                let tmid = self.pending_tmid(&mid, ctx);
                ctx.workspace
                    .discard_pending(ctx.rid, tmid.as_deref(), &mid);
                Action::none()
            }
            Message::Resent(mid, tmid, r) => {
                match r {
                    Ok(m) => ctx.workspace.apply_message(*m),
                    Err(e) => ctx.workspace.mark_pending_failed(
                        ctx.rid,
                        tmid.as_deref(),
                        &mid,
                        &e.user_text(),
                    ),
                }
                Action::none()
            }
            Message::ActionDone(act, r) => match r {
                Ok(()) => match act {
                    Act::MarkUnread => Action::effect(Self::toast_ok(
                        t!("room_timeline.marked_unread").into_owned(),
                    )),
                    _ => Action::none(),
                },
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
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.delete_message(&rid, &mid).await },
                    Message::Deleted,
                ))
            }
            Message::Deleted(r) => {
                self.dialog = None;
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
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.post_ok("chat.reportMessage", &body).await },
                    Message::Reported,
                ))
            }
            Message::Reported(r) => match r {
                Ok(()) => {
                    self.dialog = None;
                    Action::effect(Self::toast_ok(t!("room_timeline.reported").into_owned()))
                }
                Err(e) => {
                    if let Some(Dialog::Report { busy, .. }) = &mut self.dialog {
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
            Message::OpenImageInBrowser => match &self.dialog {
                Some(Dialog::Image { image, .. }) => {
                    Action::effect(Effect::OpenUrl(image.url.clone()))
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
                    };
                    self.render_msg(RenderMsg::Download(f), ctx)
                }
                _ => Action::none(),
            },
            Message::SaveTo(f, path) => match path {
                Some(p) => self.start_download(f, p, false, ctx),
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
            self.window = self.window.min(WINDOW.max(self.window / 2));
            if self.focused && Self::unread(ctx) {
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

    fn pending_tmid(&self, mid: &str, ctx: &UpdateCtx<'_>) -> Option<String> {
        ctx.workspace
            .timeline(ctx.rid)
            .and_then(|t| t.get(mid))
            .and_then(|i| i.message.tmid.clone())
    }

    fn retry(&mut self, mid: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some(item) = ctx.workspace.timeline(ctx.rid).and_then(|t| t.get(mid)) else {
            return Action::none();
        };
        if !matches!(item.state, DeliveryState::Failed { .. }) {
            return Action::none();
        }
        let m = &item.message;
        let out = OutgoingMessage {
            id: Some(m.id.clone()),
            rid: m.rid.clone(),
            msg: m.msg.clone(),
            tmid: m.tmid.clone(),
            tshow: m.tshow.filter(|t| *t),
            ..Default::default()
        };
        let tmid = m.tmid.clone();
        ctx.workspace
            .mark_pending_sending(ctx.rid, tmid.as_deref(), mid);
        let mid = mid.to_owned();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.send_message(&out).await.map(Box::new) },
            move |r| Message::Resent(mid, tmid, r),
        ))
    }

    fn start_download(
        &mut self,
        f: FileRef,
        path: PathBuf,
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
        let stream = transfer::download(rest, f.url.clone(), path);
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
            RenderMsg::OpenRoom(target) => {
                let ws = &*ctx.workspace;
                let rid = if ws.room(&target).is_some() {
                    Some(target.clone())
                } else {
                    ws.rooms()
                        .values()
                        .find(|e| e.name() == target)
                        .map(|e| e.rid().to_owned())
                };
                match rid {
                    Some(rid) => Action::effect(Effect::OpenRoom(rid)),
                    None => Action::effect(Self::toast_error(
                        t!("room_timeline.room_not_found", name = target.as_str()).into_owned(),
                        String::new(),
                    )),
                }
            }
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
                if let Some(Transfer::Done(path)) = self.render.transfer(&f.mid, &f.url)
                    && path.exists()
                {
                    return Action::task(Task::perform(
                        transfer::open_path(path.clone()),
                        Message::Opened,
                    ));
                }
                let path = transfer::media_path(&f.mid, &f.name);
                self.start_download(f, path, true, ctx)
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
            return Action::none();
        }
        let m = item.message.clone();
        let mid = mid.to_owned();
        let rest_act = |ctx: &UpdateCtx<'_>, act: Act| {
            let mid = mid.clone();
            let rid = ctx.rid.to_owned();
            Action::task(ctx.rest_task(
                move |rest| async move {
                    match act {
                        Act::Pin => rest.pin_message(&mid).await.map(|_| ()),
                        Act::Unpin => rest.unpin_message(&mid).await,
                        Act::Star => rest.star_message(&mid).await,
                        Act::Unstar => rest.unstar_message(&mid).await,
                        Act::Follow => rest.follow_message(&mid).await,
                        Act::Unfollow => rest.unfollow_message(&mid).await,
                        Act::MarkUnread => rest.subscriptions_unread(&rid, Some(&mid)).await,
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
            Act::Pin
            | Act::Unpin
            | Act::Star
            | Act::Unstar
            | Act::Follow
            | Act::Unfollow
            | Act::MarkUnread => rest_act(ctx, act),
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

    pub fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
            _ => None,
        })
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
        // Newest first: skip what waits below while frozen, take the window
        // plus one row of context for grouping.
        let mut new_below = 0usize;
        let mut picked: Vec<&'a TimelineItem> = Vec::with_capacity(self.window + 1);
        for item in tl.iter().rev() {
            if !visible(&item.message) {
                continue;
            }
            if let Some((fts, fid)) = &self.frozen {
                let (ts, id) = item_key(item);
                if (ts, id) > (*fts, fid.as_str()) {
                    new_below += 1;
                    continue;
                }
            }
            if picked.len() > self.window {
                break;
            }
            picked.push(item);
        }
        let more_loaded_above = picked.len() > self.window;
        picked.reverse();
        if picked.is_empty() && new_below == 0 {
            return empty_state(
                Icon::MessagesSquare,
                t!("room_timeline.empty_title"),
                t!("room_timeline.empty_subtitle"),
            );
        }

        let me = ws.user_id().unwrap_or_default();
        let divider = tl.unread_divider(me);
        let grouping = ws.settings().grouping_period_secs();
        let tunread = ws.subscription(&self.rid).and_then(|s| s.tunread.as_ref());
        let dark = render::mode() == rustket_ui::theme::Mode::Dark;
        let revs = (
            ws.revision(Dirty::SETTINGS | Dirty::PERMISSIONS | Dirty::ROLES | Dirty::ME),
            ws.revision(Dirty::EMOJI),
            ctx.images.len(),
            ctx.images.generation(ctx.server_id),
        );
        let prefs = (
            ctx.prefs.clock_24h,
            ws.use_real_names(ctx.prefs.use_real_names),
        );
        let now_local = Local::now();
        let mut col: Column<'a, Message> = Column::new()
            .width(Length::Fill)
            .padding(Padding::from([SPACE_2, 0.0]));
        if !more_loaded_above {
            col = col.push(top_indicator(tl));
        }
        for (i, item) in picked.iter().enumerate() {
            if more_loaded_above && i == 0 {
                continue;
            }
            let m = &item.message;
            let prev = i.checked_sub(1).map(|p| &picked[p].message);
            if let Some(ts) = m.ts
                && is_new_day(prev, m, &Local)
            {
                col = col.push(date_separator(row::day_label(ts, now_local)));
            }
            if divider == Some(m.id.as_str()) {
                col = col.push(unread_separator(t!("room_timeline.unread").into_owned()));
            }
            let flags = row::RowFlags {
                sequential: prev.is_some_and(|p| is_sequential(p, m, grouping, &Local, divider)),
                menu_open: self.menu.as_deref() == Some(m.id.as_str()),
                thread_unread: tunread.is_some_and(|t| t.contains(&m.id)),
            };
            let state = match &item.state {
                DeliveryState::Sent => 0u8,
                DeliveryState::Sending => 1,
                DeliveryState::Failed { .. } => 2,
            };
            let dep = RowKey {
                msg: render::message_key(m),
                state,
                render: self.render.key(&m.id),
                flags,
                dark,
                prefs,
                revs,
            };
            let item: &'a TimelineItem = item;
            col = col.push(lazy(dep, move |_| {
                row::build(&env, item, flags, Utc::now())
            }));
        }
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

/// `lazy` dependency of a row.
#[derive(Clone, Copy)]
struct RowKey {
    msg: u64,
    state: u8,
    render: u64,
    flags: row::RowFlags,
    dark: bool,
    prefs: (bool, bool),
    revs: (u64, u64, usize, u64),
}

impl Hash for RowKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.msg.hash(h);
        self.state.hash(h);
        self.render.hash(h);
        self.flags.hash(h);
        self.dark.hash(h);
        self.prefs.hash(h);
        self.revs.hash(h);
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
