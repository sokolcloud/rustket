//! Thread view (`Kind::Thread(tmid)`): the parent message on top, then the
//! replies (newest page first, older pages when scrolled to the top), with
//! the room timeline's row rendering and message actions
//! (`room::timeline::{list, row, interact}`), pending/failed replies, its
//! own composer (`Composer::for_thread`) with "Also send to channel", and
//! read marking (`chat.readThread`) while the thread is open at the bottom.
//! Strings: `locales/room_panel.yml` (`room_panel.thread_*`).

use std::time::Duration;

use chrono::{Local, Utc};
use iced::widget::scrollable::Viewport;
use iced::widget::{button, checkbox, column, container, row, scrollable, sensor, space, text};
use iced::{Alignment, Element, Length, Padding, Subscription, Task};
use rocketchat::models::{Message as RcMessage, Paginated};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::timeline::{DeliveryState, PAGE_SIZE, PageTicket, TimelineItem};
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{divider, icon_button_with, keyed_list, spinner};

use super::common;
use crate::error::Res;
use crate::room::composer::{self, Composer};
use crate::room::render;
use crate::room::timeline::interact::{self, Interactions, Msg as ItemMsg, Source};
use crate::room::timeline::{Act, SELECTED_FOR, list, row as trow, row_id};
use crate::room::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};
use crate::session::SessionHandle;

/// Width of the thread panel, and the maximum width when expanded (drawn
/// over the room column, like the web's `max-width: 855px`).
pub const WIDTH: f32 = 440.0;
pub const WIDTH_EXPANDED: f32 = 855.0;

#[derive(Debug, Clone)]
pub enum Msg {
    Item(ItemMsg),
    Composer(composer::Message),
    Parent(Res<Box<RcMessage>>),
    Page(PageTicket, Result<Paginated<RcMessage>, String>),
    /// Retry the parent or the failed page.
    Retry,
    ReachedTop,
    Scrolled(Viewport),
    AlsoSend(bool),
    ToggleFollow,
    Followed(Res<()>),
    ToggleExpand,
    MarkedRead(Res<()>),
    Reveal(String),
    /// The highlight with this stamp ends.
    Unselect(u64),
    JumpToLatest,
}

pub struct Thread {
    tmid: String,
    items: Interactions,
    composer: Composer,
    /// The parent as a row item (refreshed from the workspace).
    parent: Option<TimelineItem>,
    parent_loading: bool,
    parent_error: Option<String>,
    /// "Also send to channel".
    tshow: bool,
    /// The default of `tshow` was applied (needs the parent's `tcount`).
    tshow_set: bool,
    scroll_id: iced::widget::Id,
    /// Built window, bottom following and the frozen view while scrolled up.
    follow: list::Follow,
    expanded: bool,
    follow_busy: bool,
    reading: bool,
    /// Unread replies arrived while `chat.readThread` was in flight.
    read_again: bool,
    /// Reply to scroll to and highlight once loaded (notification, search
    /// result); taken when the reveal is scheduled.
    focus: Option<String>,
    /// The revealed reply and the stamp of its highlight.
    selected: Option<(String, u64)>,
    stamp: u64,
    /// `open_thread` ran: `close_thread` is due when the view goes away.
    opened: bool,
    /// The last page request.
    ticket: Option<PageTicket>,
}

/// The user's `alsoSendThreadToChannel` preference (`default`, `always`,
/// `never`; the server default when unset).
pub fn also_send_preference(ws: &Workspace) -> String {
    ws.me()
        .and_then(|u| u.preferences())
        .and_then(|p| p.get("alsoSendThreadToChannel"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            ws.settings()
                .str("Accounts_Default_User_Preferences_alsoSendThreadToChannel")
        })
        .unwrap_or("default")
        .to_owned()
}

/// Initial "Also send to channel" state like the web's `ThreadChat`:
/// `always`/`never` as set, otherwise on for the first reply of a thread.
pub fn also_send_default(ws: &Workspace, tcount: Option<u64>) -> bool {
    match also_send_preference(ws).as_str() {
        "always" => true,
        "never" => false,
        _ => tcount.unwrap_or(0) == 0,
    }
}

impl Thread {
    pub fn new(rid: &str, tmid: &str) -> Self {
        Self {
            tmid: tmid.to_owned(),
            items: Interactions::new(Source::Thread(tmid.to_owned())),
            composer: Composer::for_thread(rid, tmid),
            parent: None,
            parent_loading: false,
            parent_error: None,
            tshow: false,
            tshow_set: false,
            scroll_id: iced::widget::Id::unique(),
            follow: list::Follow::default(),
            expanded: false,
            follow_busy: false,
            reading: false,
            read_again: false,
            focus: None,
            selected: None,
            stamp: 0,
            opened: false,
            ticket: None,
        }
    }

    pub fn tmid(&self) -> &str {
        &self.tmid
    }

    pub fn width(&self) -> f32 {
        if self.expanded { WIDTH_EXPANDED } else { WIDTH }
    }

    /// Expanded: drawn over the room column (like the web).
    pub fn expanded(&self) -> bool {
        self.expanded
    }

    pub fn set_expanded(&mut self, on: bool) {
        self.expanded = on;
    }

    pub fn tshow(&self) -> bool {
        self.tshow
    }

    /// Highlights reply `mid` once it is loaded.
    pub fn set_focus(&mut self, mid: Option<String>) {
        self.focus = mid;
    }

    /// Opens the thread timeline, restores the draft, loads the parent (if
    /// unknown) and the newest replies.
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        ctx.workspace.open_thread(ctx.rid, &self.tmid);
        self.opened = true;
        self.refresh_parent(ctx.workspace);
        let mut tasks = Vec::new();
        let mut effects = Vec::new();
        let c = self.composer.on_open(ctx);
        tasks.push(c.task.map(Msg::Composer));
        effects.extend(c.effects);
        if self.parent.is_none() {
            tasks.push(self.fetch_parent(ctx));
        }
        tasks.push(self.load_page(ctx));
        Action {
            task: Task::batch(tasks),
            effects,
        }
    }

    /// Stops the typing indicator of the thread composer (the view goes
    /// away or is no longer shown).
    pub fn leave(&mut self, session: Option<&SessionHandle>) -> Task<Msg> {
        self.composer.leave(session).map(Msg::Composer)
    }

    /// The view goes away: keep the draft, forget the thread timeline. A
    /// view that never activated leaves both alone (its composer never
    /// loaded the draft).
    pub fn on_close(&mut self, ws: &mut Workspace) {
        if self.opened {
            self.composer.on_close(ws);
            ws.close_thread(&self.tmid);
            self.opened = false;
        }
    }

    pub fn focus_composer(&self) -> Task<Msg> {
        self.composer.focus().map(Msg::Composer)
    }

    fn fetch_parent(&mut self, ctx: &UpdateCtx<'_>) -> Task<Msg> {
        self.parent_loading = true;
        self.parent_error = None;
        let tmid = self.tmid.clone();
        ctx.rest_task(
            move |rest| async move { rest.get_message(&tmid).await.map(Box::new) },
            Msg::Parent,
        )
    }

    /// Requests the next (older) page of replies, if any.
    fn load_page(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Msg> {
        let Some((offset, ticket)) = ctx.workspace.begin_load_thread_ticket(&self.tmid) else {
            return Task::none();
        };
        self.ticket = Some(ticket);
        let tmid = self.tmid.clone();
        ctx.rest_task(
            move |rest| async move {
                rest.thread_messages_latest(&tmid, offset, u64::from(PAGE_SIZE))
                    .await
            },
            move |r| Msg::Page(ticket, r.map_err(|e| e.user_text())),
        )
    }

    fn refresh_parent(&mut self, ws: &Workspace) {
        let parent = ws.thread(&self.tmid).and_then(|t| t.parent()).cloned();
        if let Some(p) = &parent
            && !self.tshow_set
        {
            self.tshow = also_send_default(ws, p.tcount);
            self.tshow_set = true;
        }
        self.parent = parent.map(|message| TimelineItem {
            message,
            state: DeliveryState::Sent,
        });
    }

    fn thread_unread(&self, ws: &Workspace, rid: &str) -> bool {
        ws.subscription(rid)
            .and_then(|s| s.tunread.as_ref())
            .is_some_and(|l| l.contains(&self.tmid))
    }

    /// `chat.readThread` when the thread has unread replies and the view is
    /// at the bottom (the caller knows the window is focused).
    pub fn mark_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Msg> {
        if !self.follow.at_bottom || !self.thread_unread(ctx.workspace, ctx.rid) {
            return Task::none();
        }
        if self.reading {
            // Checked again when the request in flight ends.
            self.read_again = true;
            return Task::none();
        }
        self.reading = true;
        let tmid = self.tmid.clone();
        ctx.rest_task(
            move |rest| async move { rest.read_thread(&tmid).await },
            Msg::MarkedRead,
        )
    }

    fn following(&self, ws: &Workspace) -> bool {
        let me = ws.user_id();
        self.parent.as_ref().is_some_and(|p| {
            me.is_some_and(|id| p.message.replies.iter().flatten().any(|u| u == id))
        })
    }

    /// Follow/unfollow is offered when the parent's actions offer it.
    fn can_follow(&self, ws: &Workspace) -> bool {
        self.parent.as_ref().is_some_and(|p| {
            interact::available(ws, self.items.source(), &p.message, Utc::now())
                .iter()
                .any(|a| matches!(a, Act::Follow | Act::Unfollow))
        })
    }

    /// Pending replies in this thread (a send adds one).
    fn pending_count(&self, ws: &Workspace) -> usize {
        ws.thread(&self.tmid).map_or(0, |t| t.pending().count())
    }

    fn with_composer(a: Action<composer::Message>) -> Action<Msg> {
        let mut out = a.map(Msg::Composer);
        // Composer effects stay inside the thread: its own intents never
        // reach the room composer.
        out.effects.retain(|e| !matches!(e, Effect::Compose(_)));
        out
    }

    /// Routes composer requests of the rows to the thread composer.
    fn route(&mut self, a: Action<ItemMsg>, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let mut out = Action {
            task: a.task.map(Msg::Item),
            effects: Vec::new(),
        };
        for e in a.effects {
            match e {
                Effect::Compose(ComposeIntent::ReplyInThread(_)) => {
                    out.task = Task::batch([out.task, self.focus_composer()]);
                }
                Effect::Compose(intent) => {
                    let c = Self::with_composer(self.composer.intent(intent, ctx));
                    out.task = Task::batch([out.task, c.task]);
                    out.effects.extend(c.effects);
                }
                other => out.effects.push(other),
            }
        }
        out
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Item(m) => {
                let a = self.items.update(m, ctx);
                self.route(a, ctx)
            }
            Msg::Composer(m) => {
                let before = self.pending_count(ctx.workspace);
                self.composer.set_tshow(self.tshow);
                let a = self.composer.update(m, ctx);
                // A reply was sent: with the `default` preference the box
                // unchecks itself (like the web).
                if self.pending_count(ctx.workspace) > before
                    && also_send_preference(ctx.workspace) == "default"
                {
                    self.tshow = false;
                }
                let mut a = Self::with_composer(a);
                if self.pending_count(ctx.workspace) > before {
                    self.follow.follow_bottom();
                    a.task = Task::batch([a.task, self.snap_to_bottom()]);
                }
                a
            }
            Msg::Parent(r) => {
                self.parent_loading = false;
                match r {
                    Ok(m) => {
                        if m.tmid.is_some() {
                            // A reply, not a parent: show its thread instead.
                            let tmid = m.tmid.clone().unwrap_or_default();
                            return Action::effect(Effect::OpenPanel(super::Kind::Thread(tmid)));
                        }
                        ctx.workspace.apply_message(*m);
                        self.refresh_parent(ctx.workspace);
                    }
                    Err(e) => self.parent_error = Some(common::failure_text(&e)),
                }
                Action::none()
            }
            Msg::Page(ticket, r) => {
                if let Err(e) = &r {
                    tracing::debug!(tmid = %self.tmid, error = %e, "thread page failed");
                }
                let first = ctx
                    .workspace
                    .thread(&self.tmid)
                    .is_some_and(|t| !t.pagination.loaded);
                let applied =
                    ctx.workspace
                        .finish_load_thread_ticket(&self.tmid, ticket, r.clone());
                let mut tasks = Vec::new();
                let loaded = ctx
                    .workspace
                    .thread(&self.tmid)
                    .is_some_and(|t| t.pagination.loaded);
                if applied && r.is_ok() {
                    // A dropped page (a loaded reply was deleted meanwhile):
                    // ask again.
                    let again = ctx.workspace.thread(&self.tmid).is_some_and(|t| {
                        t.pagination.last_page_added == 0 && t.pagination.has_more_before && !first
                    });
                    if again {
                        tasks.push(self.load_page(ctx));
                    }
                    if first && loaded {
                        // Opening the thread reads it (like the web).
                        tasks.push(self.mark_read(ctx));
                    }
                }
                // The first page was dropped, or this one was stale after a
                // reset: (re)load the newest replies.
                tasks.push(self.ensure_loaded(ctx));
                tasks.push(self.reveal_focus(ctx));
                Action::task(Task::batch(tasks))
            }
            Msg::Retry => {
                let mut tasks = Vec::new();
                if self.parent.is_none() {
                    tasks.push(self.fetch_parent(ctx));
                }
                tasks.push(self.load_page(ctx));
                Action::task(Task::batch(tasks))
            }
            Msg::ReachedTop => Action::task(self.reach_top(ctx)),
            Msg::Scrolled(vp) => {
                let s = self.follow.scrolled(&vp, ctx.workspace.thread(&self.tmid));
                let mut tasks = Vec::new();
                if s.back_at_bottom {
                    tasks.push(self.mark_read(ctx));
                }
                if s.near_top {
                    tasks.push(self.reach_top(ctx));
                }
                Action::task(Task::batch(tasks))
            }
            Msg::JumpToLatest => {
                self.follow.follow_bottom();
                // A click: the window is focused and the thread visible.
                Action::task(Task::batch([self.snap_to_bottom(), self.mark_read(ctx)]))
            }
            Msg::AlsoSend(on) => {
                self.tshow = on;
                self.tshow_set = true;
                self.composer.set_tshow(on);
                Action::none()
            }
            Msg::ToggleFollow => {
                if self.follow_busy || !self.can_follow(ctx.workspace) {
                    return Action::none();
                }
                self.follow_busy = true;
                let follow = !self.following(ctx.workspace);
                let tmid = self.tmid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        if follow {
                            rest.follow_message(&tmid).await
                        } else {
                            rest.unfollow_message(&tmid).await
                        }
                    },
                    Msg::Followed,
                ))
            }
            Msg::Followed(r) => {
                self.follow_busy = false;
                match r {
                    Ok(()) => Action::none(),
                    Err(e) => Action::effect(common::error_toast(
                        &t!("room_panel.thread_follow_failed"),
                        &e,
                    )),
                }
            }
            Msg::ToggleExpand => {
                self.expanded = !self.expanded;
                Action::none()
            }
            Msg::MarkedRead(r) => {
                self.reading = false;
                if let Err(e) = r {
                    tracing::debug!(tmid = %self.tmid, error = %e, "chat.readThread failed");
                }
                // A reply that arrived meanwhile may have marked it unread
                // again (still gated by unread and bottom).
                if std::mem::take(&mut self.read_again) {
                    return Action::task(self.mark_read(ctx));
                }
                Action::none()
            }
            Msg::Reveal(mid) => {
                Action::task(list::reveal(self.scroll_id.clone(), row_id(&mid), true))
            }
            Msg::Unselect(stamp) => {
                if self.selected.as_ref().is_some_and(|(_, s)| *s == stamp) {
                    self.selected = None;
                }
                Action::none()
            }
        }
    }

    fn snap_to_bottom(&self) -> Task<Msg> {
        iced::widget::operation::snap_to(
            self.scroll_id.clone(),
            iced::widget::scrollable::RelativeOffset::START,
        )
    }

    /// Near the top: builds more of the loaded replies, else requests the
    /// next older page.
    fn reach_top(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Msg> {
        let Some(t) = ctx.workspace.thread(&self.tmid) else {
            return Task::none();
        };
        if !self.follow.grow(t) {
            return Task::none();
        }
        let p = &t.pagination;
        if p.loaded && p.has_more_before && !p.loading_before && p.error.is_none() {
            self.load_page(ctx)
        } else {
            Task::none()
        }
    }

    /// The thread timeline was reset (resync, history import) or its first
    /// page was dropped: load the newest replies again, following the
    /// bottom.
    fn ensure_loaded(&mut self, ctx: &mut UpdateCtx<'_>) -> Task<Msg> {
        if !self.opened {
            return Task::none();
        }
        let idle = ctx.workspace.thread(&self.tmid).is_some_and(|t| {
            let p = &t.pagination;
            !p.loaded && !p.loading_before && p.error.is_none()
        });
        if !idle {
            return Task::none();
        }
        self.follow.follow_bottom();
        Task::batch([self.load_page(ctx), self.snap_to_bottom()])
    }

    /// Scrolls to the focused reply once it is loaded and highlights it for
    /// a moment (like the room timeline).
    fn reveal_focus(&mut self, ctx: &UpdateCtx<'_>) -> Task<Msg> {
        let Some(mid) = self.focus.clone() else {
            return Task::none();
        };
        let Some(t) = ctx.workspace.thread(&self.tmid) else {
            return Task::none();
        };
        if !self.follow.hold_at(t, &mid) {
            return Task::none();
        }
        self.focus = None;
        self.stamp += 1;
        let stamp = self.stamp;
        self.selected = Some((mid.clone(), stamp));
        Task::batch([
            // After the frame that builds the row.
            Task::perform(tokio::time::sleep(Duration::from_millis(60)), move |_| {
                Msg::Reveal(mid.clone())
            }),
            Task::perform(tokio::time::sleep(SELECTED_FOR), move |_| {
                Msg::Unselect(stamp)
            }),
        ])
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let mut tasks = Vec::new();
        let mut effects = Vec::new();
        let c = Self::with_composer(self.composer.on_changes(changes, ctx));
        tasks.push(c.task);
        effects.extend(c.effects);
        if changes.threads.contains(&self.tmid) || changes.timelines.contains(ctx.rid) {
            let had_parent = self.parent.is_some();
            self.refresh_parent(ctx.workspace);
            let mut snap = false;
            if let Some(t) = ctx.workspace.thread(&self.tmid) {
                let parent = self.tmid.clone();
                self.items.prune(|mid| mid == parent || t.contains(mid));
                // An own reply being sent while scrolled up: jump down.
                snap = self.follow.on_changes(t);
            }
            // The parent was deleted: the web closes the thread.
            if had_parent && self.parent.is_none() && ctx.workspace.thread(&self.tmid).is_some() {
                effects.push(Effect::ClosePanel);
            }
            // New replies while at the bottom: keep following them.
            if snap || self.follow.at_bottom {
                tasks.push(self.snap_to_bottom());
            }
            tasks.push(self.ensure_loaded(ctx));
        }
        Action {
            task: Task::batch(tasks),
            effects,
        }
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        self.composer.subscription().map(Msg::Composer)
    }

    /// The open dialog or lightbox.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(u8, Element<'a, Msg>, Msg)> {
        self.items
            .overlay(ctx)
            .map(|(k, e, d)| (k, e.map(Msg::Item), Msg::Item(d)))
    }

    /// Header title: an excerpt of the parent message.
    pub fn title(&self) -> String {
        let s = self
            .parent
            .as_ref()
            .map(|p| render::plain_text(&p.message))
            .unwrap_or_default();
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.is_empty() {
            return t!("room_panel.thread").into_owned();
        }
        if s.chars().count() > 80 {
            let cut: String = s.chars().take(80).collect();
            format!("{cut}…")
        } else {
            s
        }
    }

    /// Follow and expand buttons for the panel header.
    pub fn header_actions<'a>(&'a self, ctx: &ViewCtx<'a>) -> Vec<Element<'a, Msg>> {
        let mut out = Vec::new();
        out.push(icon_button_with(
            if self.expanded {
                Icon::Minimize2
            } else {
                Icon::Maximize2
            },
            Some(Msg::ToggleExpand),
            if self.expanded {
                t!("room_panel.thread_collapse").into_owned()
            } else {
                t!("room_panel.thread_expand").into_owned()
            },
            false,
            32.0,
        ));
        if self.can_follow(ctx.workspace) {
            let following = self.following(ctx.workspace);
            out.push(icon_button_with(
                if following { Icon::Bell } else { Icon::BellOff },
                (!self.follow_busy).then_some(Msg::ToggleFollow),
                if following {
                    t!("room_panel.thread_following").into_owned()
                } else {
                    t!("room_panel.thread_not_following").into_owned()
                },
                following,
                32.0,
            ));
        }
        out
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let Some(parent) = &self.parent else {
            if let Some(e) = &self.parent_error {
                return common::error_state(e, Msg::Retry);
            }
            return common::loading();
        };
        let Some(tl) = ws.thread(&self.tmid) else {
            return common::loading();
        };
        let env = trow::Env {
            server_id: ctx.server_id,
            server: ctx.server,
            ws,
            images: ctx.images,
            render: &self.items.viewer.render,
            prefs: ctx.prefs,
            rid: ctx.rid,
            source: self.items.source(),
        };
        let common_ = list::Common::new(ctx, ctx.rid);
        let me = ws.user_id().unwrap_or_default();
        let menu = self.items.menu.as_deref();
        let selected = self.selected.as_ref().map(|(m, _)| m.as_str());
        let flags = |m: &RcMessage, sequential: bool| trow::RowFlags {
            sequential,
            menu_open: menu == Some(m.id.as_str()),
            thread_unread: false,
            selected: selected == Some(m.id.as_str()),
        };
        let mut col = keyed_list::<u64, Msg, _, _>()
            .width(Length::Fill)
            .padding(Padding::from([SPACE_2, 0.0]));
        let parent_row = list::PlannedRow {
            item: parent,
            new_day: parent.message.ts.is_some(),
            unread_before: false,
            sequential: false,
        };
        col = col.push(
            list::id_key(&parent.message.id),
            list::row(env, &common_, &parent_row, flags(&parent.message, false)).map(Msg::Item),
        );
        let count = parent.message.tcount.unwrap_or(0).max(
            tl.iter()
                .filter(|i| !i.is_pending())
                .count()
                .try_into()
                .unwrap_or(0),
        );
        col = col.push(
            list::TOP_KEY,
            sensor(self.replies_head(tl, count))
                .key((tl.len(), self.follow.window, tl.pagination.loading_before))
                .anticipate(list::TOP_THRESHOLD)
                .on_show(|_| Msg::ReachedTop),
        );
        let mut plan = list::plan(
            tl,
            list::visible_in(ws, ctx.rid),
            self.follow.frozen(),
            self.follow.window,
            me,
            ws.settings().grouping_period_secs(),
            &Local,
        );
        // The parent's day separator covers replies of the same day.
        let more_above = plan.more_above;
        if let Some(first) = plan.rows.first_mut().filter(|_| !more_above) {
            first.new_day = first.item.message.ts.is_some()
                && rustket_core::timeline::is_new_day(
                    Some(&parent.message),
                    &first.item.message,
                    &Local,
                );
        }
        for r in &plan.rows {
            let m = &r.item.message;
            let f = flags(m, r.sequential);
            let el = list::row(env, &common_, r, f).map(Msg::Item);
            let el = if f.selected {
                container(el).id(row_id(&m.id)).into()
            } else {
                el
            };
            col = col.push(list::id_key(&m.id), el);
        }
        let list = scrollable(col)
            .id(self.scroll_id.clone())
            .anchor_bottom()
            .on_scroll(Msg::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::scrollable);
        let new_below = plan.new_below;
        let list: Element<'a, Msg> = if self.follow.at_bottom && new_below == 0 {
            list.into()
        } else {
            let label = if new_below > 0 {
                t!("room_panel.thread_new_replies", count = new_below).into_owned()
            } else {
                t!("room_timeline.jump_to_latest").into_owned()
            };
            list::with_jump_button(list.into(), label, Msg::JumpToLatest)
        };
        let mut bottom = column![].spacing(0);
        if rustket_core::permissions::post_block(ws, ctx.rid).is_none() {
            bottom = bottom.push(
                container(
                    checkbox(self.tshow)
                        .label(t!("room_panel.thread_also_send").into_owned())
                        .on_toggle(Msg::AlsoSend)
                        .size(16)
                        .text_size(TEXT_SMALL)
                        .style(style::checkbox),
                )
                .padding([SPACE_1, SPACE_4]),
            );
        }
        bottom = bottom.push(self.composer.view(ctx).map(Msg::Composer));
        column![
            container(list).width(Length::Fill).height(Length::Fill),
            bottom
        ]
        .height(Length::Fill)
        .into()
    }

    /// "N replies" with the older-replies state below the parent.
    fn replies_head<'a>(
        &self,
        tl: &rustket_core::timeline::Timeline,
        count: u64,
    ) -> Element<'a, Msg> {
        let p = &tl.pagination;
        let label = t!(
            format!("room_panel.thread_replies_{}", render::plural(count)),
            count = count
        )
        .into_owned();
        let state: Element<'a, Msg> = if p.loading_before {
            row![
                spinner(14.0),
                text(t!("room_panel.thread_loading").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint)
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        } else if p.error.is_some() {
            row![
                text(t!("room_panel.thread_failed").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_danger),
                button(text(t!("room_panel.retry").into_owned()).size(TEXT_CAPTION))
                    .padding([0, 4])
                    .style(style::button_link)
                    .on_press(Msg::Retry)
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        } else if p.loaded && p.has_more_before {
            button(text(t!("room_panel.thread_older").into_owned()).size(TEXT_CAPTION))
                .padding([0, 4])
                .style(style::button_link)
                .on_press(Msg::ReachedTop)
                .into()
        } else {
            space().height(0).into()
        };
        column![
            row![
                text(label)
                    .size(TEXT_CAPTION)
                    .font(FONT_SEMIBOLD)
                    .style(style::text_secondary),
                container(divider()).width(Length::Fill),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
            container(state).center_x(Length::Fill),
        ]
        .spacing(SPACE_1)
        .padding([SPACE_2, SPACE_4])
        .into()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
