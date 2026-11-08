//! The room area: one [`RoomView`] per open room, composed of four components
//! with their own messages — [`header`], [`timeline`], [`composer`] and the
//! right contextual [`panel`]. See `crates/rustket/COMPONENTS.md` for the
//! component contract.

pub mod composer;
pub mod emoji_picker;
pub mod header;
pub mod panel;
pub mod render;
pub mod timeline;
pub mod upload;

use std::sync::Arc;
use std::time::Instant;

use iced::widget::{column, container, opaque, row, space, stack};
use iced::{Element, Length, Subscription, Task};
use rocketchat::rest::HistoryCursor;
use rocketchat::session::RoomWatch;
use rocketchat::{RestClient, ServerUrl};
use rust_i18n::t;
use rustket_core::config::UiPrefs;
use rustket_core::timeline::PageTicket;
use rustket_core::workspace::Changes;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::widget::{BannerKind, empty_state, modal_keyed, vertical_divider};

use crate::error::{Failure, Res};
use crate::images::Images;
use crate::session::SessionHandle;

/// Read-only context for `view`.
pub struct ViewCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub rid: &'a str,
    pub workspace: &'a Workspace,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
    /// Realtime connection is up (disable sending etc. otherwise).
    pub connected: bool,
    /// Which right panel is open (header buttons show it as active).
    pub panel: Option<&'a panel::Kind>,
    pub now: Instant,
}

/// Context for `update` / `on_changes`: mutable workspace plus the session.
pub struct UpdateCtx<'a> {
    pub server_id: &'a ServerId,
    pub server: &'a ServerUrl,
    pub rid: &'a str,
    pub workspace: &'a mut Workspace,
    /// `None` while the session is (re)starting.
    pub session: Option<&'a SessionHandle>,
    pub images: &'a Images,
    pub prefs: &'a UiPrefs,
}

impl UpdateCtx<'_> {
    /// REST client of the session (authenticated), if the session is up.
    pub fn rest(&self) -> Option<RestClient> {
        self.session.map(|s| s.rest().clone())
    }

    /// Runs an authenticated REST call as a task. Without a session the result
    /// is an `Unauthorized` failure, so callers have a single error path.
    pub fn rest_task<T, M, F, Fut>(
        &self,
        call: F,
        map: impl FnOnce(Res<T>) -> M + Send + 'static,
    ) -> Task<M>
    where
        T: Send + 'static,
        M: Send + 'static,
        F: FnOnce(RestClient) -> Fut + Send + 'static,
        Fut: Future<Output = rocketchat::Result<T>> + Send + 'static,
    {
        let rest = self.rest();
        Task::perform(
            async move {
                match rest {
                    Some(r) => call(r).await.map_err(Failure::from),
                    None => Err(Failure::from(no_session())),
                }
            },
            map,
        )
    }

    /// Runs a call on the DDP session (`Session::call`, `set_user_activity`, …).
    pub fn session_task<T, M, F, Fut>(
        &self,
        call: F,
        map: impl FnOnce(Res<T>) -> M + Send + 'static,
    ) -> Task<M>
    where
        T: Send + 'static,
        M: Send + 'static,
        F: FnOnce(SessionHandle) -> Fut + Send + 'static,
        Fut: Future<Output = rocketchat::Result<T>> + Send + 'static,
    {
        let session = self.session.cloned();
        Task::perform(
            async move {
                match session {
                    Some(s) => call(s).await.map_err(Failure::from),
                    None => Err(Failure::from(no_session())),
                }
            },
            map,
        )
    }
}

/// A room the user has not joined whose history the server does not show to
/// non-members (`preview-c-room`).
pub fn preview_blocked(ws: &Workspace, rid: &str) -> bool {
    ws.subscription(rid).is_none()
        && ws.room(rid).is_some()
        && ws.permission_roles("preview-c-room").is_some()
        && !rustket_core::permissions::has_permission(ws, "preview-c-room", None)
}

/// `ts` (ms) of the newest message from someone else in the room timeline
/// (own and pending messages never make a room unread; like the web's read
/// state manager).
fn newest_from_others(ws: &Workspace, rid: &str) -> Option<i64> {
    let me = ws.user_id();
    ws.timeline(rid)?
        .iter()
        .rev()
        .filter(|i| !i.is_pending() && me != Some(i.message.u.id.as_str()))
        .find_map(|i| i.message.ts)
        .map(|d| d.timestamp_millis())
}

/// Whether a read mark (without threads) clears anything: the room is
/// unread, or only the alert is set without unread threads (the web's
/// `attemptMarkAsRead` sends nothing otherwise).
pub(crate) fn mark_clears(sub: &rocketchat::models::Subscription) -> bool {
    sub.unread > 0 || (sub.alert && !sub.has_unread_threads())
}

/// Results for a room view that is gone (room switched or closed): uploads
/// and replies of its composers still settle (see
/// [`composer::orphan_result`]); anything else is dropped.
pub fn orphan_result(m: Message, ws: &mut Workspace) -> Vec<Effect> {
    match m {
        // The room composer settles its text sends from the outbox when the
        // room opens again; uploads report here.
        Message::Composer(composer::Message::Upload(u)) => {
            upload::orphan_result(u, ws).into_iter().collect()
        }
        Message::Panel(panel::Message::Body(
            _,
            panel::BodyMsg::Thread(panel::thread::Msg::Composer(composer::Message::Upload(u))),
        )) => upload::orphan_result(u, ws).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn no_session() -> rocketchat::Error {
    rocketchat::Error::Unauthorized {
        message: Some("not connected".into()),
        code: None,
    }
}

/// Requests a component makes to its parents.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Transient notification (title, body).
    Toast(BannerKind, String, String),
    /// Open another room of the same server.
    OpenRoom(String),
    /// Load the next older history page of this room.
    LoadOlder,
    /// Mark this room as read (automatic: only while it is readable, see
    /// [`RoomView::readable`]; unread threads stay).
    MarkRead,
    /// The explicit "Mark as read" action: the room and its unread threads.
    MarkAllRead,
    OpenPanel(panel::Kind),
    ClosePanel,
    /// Open a URL in the external browser (absolute http, https or mailto only;
    /// anything else is refused with a toast).
    OpenUrl(String),
    /// Close this room's view (e.g. after hiding the room).
    CloseRoom,
    /// Ask the composer to do something (edit, quote, react, …); routed to
    /// `Composer::intent`.
    Compose(ComposeIntent),
    /// An emoji was used (picker): record it in the persisted recent list.
    RecentEmoji(String),
    /// Show message (of this room): its thread for a reply, otherwise its
    /// place in the timeline (search results, pinned, permalinks).
    ShowMessage(Box<rocketchat::models::Message>),
    /// Mark the room unread from message `mid`. The app leaves the room
    /// first, like the web client, so the auto-read of the open room cannot
    /// undo it.
    MarkUnread(String),
    /// Open the "create discussion" dialog with this room as the parent,
    /// optionally started from message `pmid`.
    StartDiscussion {
        pmid: Option<String>,
    },
    /// Video call actions (start, join, call back, info): `crate::videoconf`.
    Call(crate::videoconf::CallAction),
    /// A request to the omnichannel part of the room (quick actions, panels);
    /// routed to `OmniRoom::request`.
    Omni(crate::omnichannel::room::Request),
    /// A UIKit interaction (blocks, app panel, app buttons); handled by the
    /// app's UIKit controller (`crate::uikit`).
    UiKit(Box<crate::uikit::UiKitMsg>),
    /// The account end-to-end encryption flow (enter/save the password).
    E2e(crate::e2e::RoomRequest),
}

/// Requests other room components make to the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeIntent {
    /// Edit message `mid`.
    Edit(String),
    /// Quote message `mid` in the next message.
    Quote(String),
    /// Insert `@username`.
    Mention(String),
    InsertText(String),
    /// Open the emoji picker to react to message `mid`.
    React(String),
    /// Reply in thread `tmid` (also opens the thread panel).
    ReplyInThread(String),
    /// Send failed message `mid` again (room or thread; the room composer's
    /// outbox path).
    Retry(String),
    /// Drop failed message `mid` (room or thread).
    Discard(String),
}

/// Result of a component `update`: a task producing the component's messages
/// plus effects for the parents.
pub struct Action<M> {
    pub task: Task<M>,
    pub effects: Vec<Effect>,
}

impl<M> Default for Action<M> {
    fn default() -> Self {
        Self::none()
    }
}

impl<M> Action<M> {
    pub fn none() -> Self {
        Self {
            task: Task::none(),
            effects: Vec::new(),
        }
    }

    pub fn task(task: Task<M>) -> Self {
        Self {
            task,
            effects: Vec::new(),
        }
    }

    pub fn effect(effect: Effect) -> Self {
        Self {
            task: Task::none(),
            effects: vec![effect],
        }
    }

    pub fn effects(effects: Vec<Effect>) -> Self {
        Self {
            task: Task::none(),
            effects,
        }
    }

    pub fn with(mut self, effect: Effect) -> Self {
        self.effects.push(effect);
        self
    }

    pub fn map<N: Send + 'static>(self, f: impl Fn(M) -> N + Send + 'static) -> Action<N>
    where
        M: Send + 'static,
    {
        Action {
            task: self.task.map(f),
            effects: self.effects,
        }
    }
}

/// Keeps `Session::watch_room` alive while the room is open (cloneable so it
/// can travel in a message; the subscriptions end when the last clone drops).
#[derive(Debug, Clone)]
pub struct WatchGuard(pub Arc<RoomWatch>);

#[derive(Debug, Clone)]
pub enum Message {
    Header(header::Message),
    Timeline(timeline::Message),
    Composer(composer::Message),
    Panel(panel::Message),
    Watched(Res<WatchGuard>),
    HistoryPage(
        HistoryCursor,
        PageTicket,
        Result<Vec<rocketchat::models::Message>, String>,
    ),
    MarkedRead(Res<()>),
    /// Files dropped onto the window while this room is shown: to the open
    /// thread's composer, else the room composer.
    FilesDropped(Vec<std::path::PathBuf>),
    /// The message of a permalink was fetched.
    Focused(Res<Box<rocketchat::models::Message>>),
    /// Omnichannel callouts and dialogs (`crate::omnichannel::room`).
    Omni(crate::omnichannel::room::Msg),
}

/// Everything shown for one open room.
pub struct RoomView {
    pub rid: String,
    watch: Option<WatchGuard>,
    pub header: header::Header,
    pub timeline: timeline::Timeline,
    pub composer: composer::Composer,
    pub panel: panel::Panel,
    /// Omnichannel callouts, quick actions and dialogs (inert for other rooms).
    pub omni: crate::omnichannel::room::OmniRoom,
    marking_read: bool,
    /// A read mark was asked for while one was in flight: checked again
    /// when it ends.
    read_again: bool,
    /// The explicit "Mark as read" waits for the request in flight.
    read_all_again: bool,
    /// The room was opened and not marked read yet (it was not readable):
    /// marked once it is.
    read_on_show: bool,
    /// Shown on screen (focused window, its server active, the server view
    /// without a page or dialog over it); set by the app.
    visible: bool,
    /// [`Self::readable`] at the last [`Self::set_visible`].
    was_readable: bool,
    /// Newest message from others (`ts` ms) the user has seen as read (last
    /// read mark, or history loaded after the read mark of `open`): the
    /// automatic read mark only runs for messages newer than that, so an
    /// unread mark made elsewhere is not undone.
    marked_upto: Option<i64>,
    /// `marked_upto` and `read_on_show` before the read mark in flight:
    /// restored when it fails, so the mark is tried again.
    mark_undo: Option<(Option<i64>, bool)>,
    /// Timeline epoch at `open`: history loaded under it counts as seen by
    /// the open's read mark, a reload after a timeline reset does not.
    open_epoch: Option<(u64, u64)>,
    /// A `watch_room` call is in flight.
    watching: bool,
    /// Set after the first history page arrived.
    pub loaded: bool,
}

impl RoomView {
    /// Opens `rid`: workspace timeline, room watch, first history page, read mark.
    pub fn open(ctx: &mut UpdateCtx<'_>) -> (Self, Action<Message>) {
        let rid = ctx.rid.to_owned();
        ctx.workspace.open_room(&rid);
        let mut view = Self {
            rid: rid.clone(),
            watch: None,
            header: header::Header::new(&rid),
            timeline: timeline::Timeline::new(&rid),
            composer: composer::Composer::new(&rid),
            panel: panel::Panel::new(&rid),
            omni: crate::omnichannel::room::OmniRoom::new(&rid),
            marking_read: false,
            read_again: false,
            read_all_again: false,
            read_on_show: false,
            visible: false,
            was_readable: false,
            marked_upto: None,
            mark_undo: None,
            open_epoch: ctx.workspace.timeline(&rid).map(|t| t.epoch()),
            watching: false,
            loaded: false,
        };
        let watch = if ctx.session.is_some() {
            view.watch_task(ctx)
        } else {
            Task::none()
        };
        // A channel the server does not let non-members read: no history.
        let mut action = if preview_blocked(ctx.workspace, &rid) {
            Action::none()
        } else {
            view.load_older(ctx)
        };
        action.task = Task::batch([action.task, watch]);
        let composer = view.composer.on_open(ctx).map(Message::Composer);
        action.task = Task::batch([action.task, composer.task]);
        action.effects.extend(composer.effects);
        // Marked read once it is shown (`set_visible`) when there is
        // anything to clear; a preview has no subscription to mark.
        view.read_on_show = ctx.workspace.subscription(&rid).is_some_and(mark_clears);
        (view, action)
    }

    /// Forgets the timeline, keeps the drafts (room and thread), closes the
    /// panel and stops the typing indicators; the room watch ends when
    /// `self` is dropped. The returned task only carries the stop-typing
    /// calls (their results are not needed).
    pub fn close(mut self, ws: &mut Workspace, session: Option<&SessionHandle>) -> Task<Message> {
        self.composer.on_close(ws);
        let typing = self.composer.leave(session).map(Message::Composer);
        let panel = self.panel.leave(ws, session).map(Message::Panel);
        ws.close_room(&self.rid);
        // A previewed room (never joined) leaves no trace in the workspace.
        ws.forget_preview(&self.rid);
        Task::batch([typing, panel])
    }

    /// The room stays open but is no longer shown (another server became
    /// active): stop the typing indicator.
    pub fn hide(&mut self, session: Option<&SessionHandle>) -> Task<Message> {
        Task::batch([
            self.composer.leave(session).map(Message::Composer),
            self.panel.hide(session).map(Message::Panel),
        ])
    }

    /// Whether the room is shown as a preview (not joined).
    pub fn is_preview(&self, ws: &Workspace) -> bool {
        ws.subscription(&self.rid).is_none()
    }

    /// Whether the automatic read mark should run: the room is unread and a
    /// message from someone else newer than the last read mark arrived, or
    /// only the alert is set without unread threads. An unread mark made
    /// elsewhere (unread > 0, no newer message) is kept, and so are unread
    /// threads: the automatic mark leaves them (and with them the alert).
    pub fn wants_auto_read(&self, ws: &Workspace) -> bool {
        let Some(sub) = ws.subscription(&self.rid) else {
            return false;
        };
        if sub.unread == 0 {
            return sub.alert && !sub.has_unread_threads();
        }
        newest_from_others(ws, &self.rid) > self.marked_upto
    }

    /// Whether the user can see the newest messages: the room is shown,
    /// its timeline is at the bottom (not the frozen view while scrolled
    /// up), not under the E2E cover and not covered by an expanded thread.
    /// The automatic read mark only runs then (like the web's
    /// `attemptMarkAsRead`).
    pub fn readable(&self, ws: &Workspace) -> bool {
        self.visible
            && !self.is_preview(ws)
            && self.timeline.at_bottom()
            && !self.panel.expanded()
            && crate::e2e::room::body_state(ws, &self.rid).is_none()
    }

    /// Shown on screen (see [`Self::set_visible`]).
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The app tells the room whether it is on screen (after every update).
    /// Becoming readable catches up on the read mark of the room (opened
    /// while hidden, or messages arrived meanwhile); becoming visible on
    /// the one of the open thread.
    pub fn set_visible(&mut self, visible: bool, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
        let was_visible = std::mem::replace(&mut self.visible, visible);
        self.panel.set_visible(visible);
        let readable = self.readable(ctx.workspace);
        let was_readable = std::mem::replace(&mut self.was_readable, readable);
        let mut tasks = Vec::new();
        if visible && !was_visible {
            tasks.push(self.mark_thread_read(ctx));
        }
        if readable && !was_readable && (self.read_on_show || self.wants_auto_read(ctx.workspace)) {
            tasks.push(self.mark_read(ctx));
        }
        Task::batch(tasks)
    }

    /// Opens panel `kind` and releases the body it replaces (a thread keeps
    /// its draft and stops typing).
    fn open_panel(&mut self, kind: panel::Kind, ctx: &mut UpdateCtx<'_>) -> Task<Message> {
        self.panel.open(kind);
        self.panel
            .retire(ctx.workspace, ctx.session)
            .map(Message::Panel)
    }

    /// Shows message `mid` (e.g. a permalink `?msg=`): fetched first unless
    /// it is loaded, then its thread or its place in the timeline.
    pub fn focus_message(&mut self, mid: &str, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if let Some(m) = ctx
            .workspace
            .timeline(&self.rid)
            .and_then(|t| t.get(mid))
            .map(|i| i.message.clone())
        {
            let a = self.show_message(m, ctx);
            return self.handle_effects(a, ctx);
        }
        let mid = mid.to_owned();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.get_message(&mid).await.map(Box::new) },
            Message::Focused,
        ))
    }

    fn show_message(
        &mut self,
        m: rocketchat::models::Message,
        ctx: &mut UpdateCtx<'_>,
    ) -> Action<Message> {
        if m.rid != self.rid {
            return Action::none();
        }
        match m.tmid.clone() {
            Some(tmid) => {
                let kind = panel::Kind::Thread(tmid.clone());
                let task = if self.panel.kind() != Some(&kind) {
                    self.open_panel(kind, ctx)
                } else {
                    Task::none()
                };
                // The open thread scrolls to it now, a new one once loaded.
                let reveal = self
                    .panel
                    .focus_thread_reply(&m.id, ctx)
                    .map(Message::Panel);
                Action::task(Task::batch([task, reveal]))
            }
            None => self
                .timeline
                .jump_to(&m.id, m.ts, ctx)
                .map(Message::Timeline),
        }
    }

    /// Marks the open thread read when it is at the bottom (the app calls it
    /// while the window is focused and this room is shown).
    pub fn mark_thread_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        self.panel.mark_thread_read(ctx).map(Message::Panel)
    }

    pub fn has_watch(&self) -> bool {
        self.watch.is_some()
    }

    /// After a (re)connect or resync: re-subscribe the room watch if it is
    /// missing and reload the history when the first page never arrived or
    /// failed.
    pub fn on_reconnect(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let mut tasks = Vec::new();
        if self.watch.is_none() && !self.watching {
            tasks.push(self.watch_task(ctx));
        }
        let needs_history = !preview_blocked(ctx.workspace, &self.rid)
            && ctx
                .workspace
                .timeline(&self.rid)
                .is_none_or(|t| !t.pagination.loaded || t.pagination.error.is_some());
        if ctx.workspace.timeline(&self.rid).is_none() {
            ctx.workspace.open_room(&self.rid);
        }
        if needs_history {
            tasks.push(self.load_older(ctx).task);
        }
        Action::task(Task::batch(tasks))
    }

    /// The room's history was replaced (`MessagesImported`): reload it.
    pub fn reload(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        if ctx.workspace.timeline(&self.rid).is_none() {
            ctx.workspace.open_room(&self.rid);
        }
        self.load_older(ctx)
    }

    fn watch_task(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        self.watching = true;
        let rid = self.rid.clone();
        ctx.session_task(
            move |s| async move { s.session().watch_room(&rid).await },
            |r| Message::Watched(r.map(|w| WatchGuard(Arc::new(w)))),
        )
    }

    fn load_older(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let Some((cursor, ticket)) = ctx.workspace.begin_load_older_ticket(&self.rid) else {
            return Action::none();
        };
        let Some(rest) = ctx.rest() else {
            ctx.workspace.finish_load_older_ticket(
                &self.rid,
                ticket,
                cursor,
                Err("not connected".into()),
            );
            return Action::none();
        };
        Action::task(Task::perform(
            async move {
                let mut c = cursor;
                let r = c.next_page(&rest).await.map_err(|e| e.to_string());
                (c, r)
            },
            move |(c, r)| Message::HistoryPage(c, ticket, r),
        ))
    }

    /// The automatic `subscriptions.read` (unread threads stay): only while
    /// the room is [`readable`](Self::readable), one request at a time; one
    /// asked for while a request is in flight is checked again when it ends.
    pub fn mark_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        if !self.readable(ctx.workspace) {
            return Task::none();
        }
        if self.marking_read {
            self.read_again = true;
            return Task::none();
        }
        self.send_read(ctx, false)
    }

    /// The explicit "Mark as read": the room and its unread threads, also
    /// while the room is not readable.
    pub fn mark_all_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        if self.is_preview(ctx.workspace) {
            return Task::none();
        }
        if self.marking_read {
            self.read_all_again = true;
            return Task::none();
        }
        self.send_read(ctx, true)
    }

    fn send_read(&mut self, ctx: &UpdateCtx<'_>, read_threads: bool) -> Task<Message> {
        self.marking_read = true;
        self.mark_undo = Some((self.marked_upto, self.read_on_show));
        self.read_on_show = false;
        self.marked_upto = self
            .marked_upto
            .max(newest_from_others(ctx.workspace, &self.rid));
        let rid = self.rid.clone();
        ctx.rest_task(
            move |rest| async move { rest.subscriptions_read_with(&rid, read_threads).await },
            Message::MarkedRead,
        )
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let action = match msg {
            Message::Header(m) => {
                // Menu entries open (or keep) their panel instead of toggling it.
                self.header.set_panel(self.panel.kind().cloned());
                self.header.update(m, ctx).map(Message::Header)
            }
            Message::Timeline(m) => self.timeline.update(m, ctx).map(Message::Timeline),
            Message::Composer(m) => self.composer.update(m, ctx).map(Message::Composer),
            Message::Panel(m) => self.panel.update(m, ctx).map(Message::Panel),
            Message::Omni(m) => self.omni.update(m, ctx).map(Message::Omni),
            Message::Watched(Ok(guard)) => {
                self.watching = false;
                self.watch = Some(guard);
                Action::none()
            }
            Message::Watched(Err(e)) => {
                self.watching = false;
                tracing::warn!(rid = %self.rid, error = %e, "room watch failed");
                Action::none()
            }
            Message::HistoryPage(cursor, ticket, result) => {
                if let Err(e) = &result {
                    tracing::warn!(rid = %self.rid, error = %e, "history page failed");
                }
                let ok = result.is_ok();
                self.loaded = true;
                // A page for an earlier timeline (reset, reopened) is dropped.
                ctx.workspace
                    .finish_load_older_ticket(&self.rid, ticket, cursor, result);
                // The history of `open` counts as seen once the open's read
                // mark was sent (or nothing was unread). A reload after a
                // timeline reset may bring messages nobody has read yet: the
                // automatic mark checks them (`App` on the timeline change,
                // `set_visible` once shown).
                if !self.read_on_show
                    && self.open_epoch.is_some()
                    && ctx.workspace.timeline(&self.rid).map(|t| t.epoch()) == self.open_epoch
                    && !self.is_preview(ctx.workspace)
                {
                    self.marked_upto = self
                        .marked_upto
                        .max(newest_from_others(ctx.workspace, &self.rid));
                }
                // A page of hidden thread replies only: fetch the next one.
                let empty_page = ctx.workspace.timeline(&self.rid).is_some_and(|t| {
                    t.pagination.last_page_added == 0 && t.pagination.has_more_before
                });
                if ok && empty_page {
                    self.load_older(ctx)
                } else {
                    Action::none()
                }
            }
            Message::MarkedRead(r) => {
                self.marking_read = false;
                let undo = self.mark_undo.take();
                if let Err(e) = r {
                    tracing::debug!(rid = %self.rid, error = %e, "subscriptions.read failed");
                    // Not marked: the next readable transition or subscription
                    // change tries again (not right away, the network may be
                    // down).
                    if let Some((upto, on_show)) = undo {
                        self.marked_upto = upto;
                        self.read_on_show |= on_show;
                    }
                    if !self.read_all_again {
                        self.read_again = false;
                        return Action::none();
                    }
                }
                if std::mem::take(&mut self.read_all_again) {
                    self.read_again = false;
                    return Action::task(self.send_read(ctx, true));
                }
                // A message that arrived meanwhile may have been counted after
                // the read mark (gated like any automatic mark).
                if std::mem::take(&mut self.read_again) && self.wants_auto_read(ctx.workspace) {
                    return Action::task(self.mark_read(ctx));
                }
                Action::none()
            }
            Message::FilesDropped(paths) => match self.panel.thread_files_dropped(&paths, ctx) {
                Some(a) => a.map(Message::Panel),
                None => self
                    .composer
                    .update(composer::Message::FilesDropped(paths), ctx)
                    .map(Message::Composer),
            },
            Message::Focused(r) => match r {
                Ok(m) => self.show_message(*m, ctx),
                Err(e) => Action::effect(Effect::Toast(
                    BannerKind::Warning,
                    e.user_text(),
                    String::new(),
                )),
            },
        };
        self.handle_effects(action, ctx)
    }

    /// Effects that concern the room itself are handled here; the rest bubble up.
    fn handle_effects(
        &mut self,
        action: Action<Message>,
        ctx: &mut UpdateCtx<'_>,
    ) -> Action<Message> {
        let mut tasks = vec![action.task];
        let mut up = Vec::new();
        for e in action.effects {
            match e {
                Effect::LoadOlder => tasks.push(self.load_older(ctx).task),
                Effect::MarkRead => tasks.push(self.mark_read(ctx)),
                Effect::MarkAllRead => tasks.push(self.mark_all_read(ctx)),
                // The open thread stays open (its summary was clicked again);
                // only header buttons toggle panels.
                Effect::OpenPanel(panel::Kind::Thread(t))
                    if self.panel.kind() == Some(&panel::Kind::Thread(t.clone())) => {}
                Effect::OpenPanel(kind) => tasks.push(self.open_panel(kind, ctx)),
                Effect::ShowMessage(m) => {
                    // Its effects (e.g. `LoadOlder` to reach older history)
                    // concern this room too.
                    let a = self.show_message(*m, ctx);
                    let a = self.handle_effects(a, ctx);
                    tasks.push(a.task);
                    up.extend(a.effects);
                }
                Effect::Compose(intent) => {
                    if let ComposeIntent::ReplyInThread(tmid) = &intent {
                        // The thread view owns the reply composer (focused
                        // when the thread opens).
                        let kind = panel::Kind::Thread(tmid.clone());
                        if self.panel.kind() == Some(&kind) {
                            tasks.push(self.panel.focus_thread_composer().map(Message::Panel));
                        } else {
                            tasks.push(self.open_panel(kind, ctx));
                        }
                        continue;
                    }
                    let a = self.composer.intent(intent, ctx).map(Message::Composer);
                    tasks.push(a.task);
                    // Effects of the composer itself (no recursion into intents).
                    up.extend(
                        a.effects
                            .into_iter()
                            .filter(|e| !matches!(e, Effect::Compose(_))),
                    );
                }
                Effect::Omni(r) => {
                    let a = self.omni.request(r, ctx).map(Message::Omni);
                    let a = self.handle_effects(a, ctx);
                    tasks.push(a.task);
                    up.extend(a.effects);
                }
                Effect::ClosePanel => {
                    self.panel.close();
                    tasks.push(
                        self.panel
                            .retire(ctx.workspace, ctx.session)
                            .map(Message::Panel),
                    );
                }
                other => up.push(other),
            }
        }
        Action {
            task: Task::batch(tasks),
            effects: up,
        }
    }

    /// Called after workspace changes were applied (realtime events, syncs).
    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        // The server says the room is read: everything up to `ls` was seen.
        if changes.rooms.contains(&self.rid)
            && let Some(sub) = ctx.workspace.subscription(&self.rid)
            && sub.unread == 0
            && !sub.alert
            && let Some(ls) = &sub.ls
        {
            self.marked_upto = self.marked_upto.max(Some(ls.timestamp_millis()));
        }
        // While nobody looks at the room, the unread divider follows the
        // server's `ls` (like the web's `updateSubscription`): back in the
        // room it sits above what arrived meanwhile.
        if changes.rooms.contains(&self.rid)
            && !self.visible
            && ctx
                .workspace
                .subscription(&self.rid)
                .is_some_and(|s| s.unread > 0 || s.alert)
        {
            ctx.workspace.refresh_unread_since(&self.rid);
        }
        // Joined after a preview the server refused to show: load the history.
        let history = if changes.rooms.contains(&self.rid)
            && !preview_blocked(ctx.workspace, &self.rid)
            && ctx.workspace.timeline(&self.rid).is_some_and(|t| {
                !t.pagination.loaded && !t.pagination.loading_before && t.pagination.error.is_none()
            }) {
            self.load_older(ctx).task
        } else {
            Task::none()
        };
        let actions = [
            self.header.on_changes(changes, ctx).map(Message::Header),
            self.timeline
                .on_changes(changes, ctx)
                .map(Message::Timeline),
            self.composer
                .on_changes(changes, ctx)
                .map(Message::Composer),
            self.panel.on_changes(changes, ctx).map(Message::Panel),
            self.omni.on_changes(changes, ctx).map(Message::Omni),
        ];
        let mut merged = Action::none();
        let mut tasks = vec![history];
        for a in actions {
            tasks.push(a.task);
            merged.effects.extend(a.effects);
        }
        merged.task = Task::batch(tasks);
        self.handle_effects(merged, ctx)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            self.header.subscription().map(Message::Header),
            self.timeline.subscription().map(Message::Timeline),
            self.composer.subscription().map(Message::Composer),
            self.panel.subscription().map(Message::Panel),
        ])
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let timeline: Element<'a, Message> = if preview_blocked(ctx.workspace, &self.rid) {
            empty_state(
                Icon::Lock,
                t!("app.preview_not_allowed"),
                t!("app.preview_join_hint"),
            )
        } else {
            // The E2E body ("check back later", enter password) covers the
            // timeline instead of replacing it, so the timeline keeps its
            // widget state when the room becomes readable.
            let cover: Element<'a, Message> = match self.header.e2e_body(ctx) {
                Some(b) => opaque(
                    container(b.map(Message::Header))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .style(style::container_room),
                ),
                None => space().into(),
            };
            stack![self.timeline.view(ctx).map(Message::Timeline), cover].into()
        };
        let main = column![
            self.header.view(ctx).map(Message::Header),
            container(timeline).width(Length::Fill).height(Length::Fill),
            self.omni
                .wrap_composer(self.composer.view(ctx).map(Message::Composer), ctx),
        ]
        .width(Length::Fill)
        .height(Length::Fill);
        // One tree shape with and without a panel and for both panel layouts
        // (iced diffs children by position): the room column stays the first
        // child of a stack, so opening, closing or expanding a panel keeps
        // its widget state (timeline scroll, composer focus), and the panel
        // keeps its own when the thread is expanded or collapsed.
        let (side, main_padding): (Element<'a, Message>, f32) = match self.panel.view(ctx) {
            Some(panel) => {
                let side = container(opaque(
                    row![
                        vertical_divider(),
                        container(panel.map(Message::Panel))
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .style(style::container_sidebar),
                    ]
                    .width(Length::Fill)
                    .height(Length::Fill),
                ))
                .height(Length::Fill);
                // The expanded thread covers the room column (up to a maximum
                // width) instead of squeezing it, like the web.
                let (side, padding) = if self.panel.expanded() {
                    let side = side
                        .width(Length::Fill)
                        .max_width(panel::thread::WIDTH_EXPANDED);
                    (side, 0.0)
                } else {
                    // The panel plus its 1px divider.
                    let width = self.panel.width() + 1.0;
                    (side.width(Length::Fixed(width)), width)
                };
                (
                    container(side)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_x(iced::Alignment::End)
                        .into(),
                    padding,
                )
            }
            None => (space().into(), 0.0),
        };
        let body: Element<'a, Message> = stack![
            container(main)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(iced::Padding {
                    right: main_padding,
                    ..iced::Padding::ZERO
                }),
            side,
        ]
        .into();
        // Dialogs and the image lightbox of the timeline and the panel
        // cover the whole room.
        let overlay = self
            .timeline
            .overlay(ctx)
            .map(|(k, e, d)| (k, e.map(Message::Timeline), Message::Timeline(d)))
            .or_else(|| {
                self.panel
                    .overlay(ctx)
                    .map(|(k, e, d)| (k + 10, e.map(Message::Panel), Message::Panel(d)))
            })
            .or_else(|| {
                self.omni
                    .overlay(ctx)
                    .map(|(k, e, d)| (k, e.map(Message::Omni), Message::Omni(d)))
            })
            .or_else(|| {
                self.header
                    .overlay(ctx)
                    .map(|(e, d)| (40, e.map(Message::Header), Message::Header(d)))
            });
        let (overlay, dismiss) = match overlay {
            Some((k, e, d)) => (Some((k, e)), Some(d)),
            None => (None, None),
        };
        let body = modal_keyed(body, overlay, dismiss);
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::container_room)
            .into()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::UserRef;
    use rocketchat::models::{Message as RcMessage, Room, RoomType, Subscription as Sub, User};
    use rocketchat::{RcDate, ServerUrl};
    use rustket_core::sync::InitialData;

    fn workspace() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_initial(InitialData {
            me: User {
                id: "me".into(),
                username: Some("me".into()),
                ..Default::default()
            },
            subscriptions: vec![Sub {
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

    fn message(id: &str, ms: i64) -> RcMessage {
        RcMessage {
            id: id.into(),
            rid: "R1".into(),
            ts: RcDate::from_millis(ms),
            ..Default::default()
        }
    }

    /// Review: a Pinned/Search result older than the loaded history asked
    /// for an older page, but the request went up to the app (ignored).
    #[test]
    fn show_message_loads_older_history_itself() {
        let mut ws = workspace();
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        ctx.workspace.reset_room_timelines("R1");
        let (c, t) = ctx.workspace.begin_load_older_ticket("R1").unwrap();
        ctx.workspace
            .finish_load_older_ticket("R1", t, c, Ok(vec![message("new", 2_000_000)]));
        assert!(
            ctx.workspace
                .timeline("R1")
                .unwrap()
                .pagination
                .has_more_before
        );
        let a = rv.handle_effects(
            Action::effect(Effect::ShowMessage(Box::new(message("old", 1_000_000)))),
            &mut ctx,
        );
        assert!(!a.effects.iter().any(|e| matches!(e, Effect::LoadOlder)));
        // The page was requested here (it fails at once without a session).
        assert!(
            ctx.workspace
                .timeline("R1")
                .unwrap()
                .pagination
                .error
                .is_some()
        );
    }

    fn unread_sub(ws: &mut Workspace, unread: u64, tunread: Option<Vec<String>>) {
        let mut sub = ws.subscription("R1").unwrap().clone();
        sub.unread = unread;
        sub.alert = unread > 0 || tunread.is_some();
        sub.tunread = tunread;
        ws.apply_subscription(rustket_core::Change::Updated, sub);
    }

    fn from_bob(id: &str, ms: i64) -> RcMessage {
        let mut m = message(id, ms);
        m.u = UserRef {
            id: "bob".into(),
            username: Some("bob".into()),
            name: None,
        };
        m
    }

    /// Review: the room was marked read while it could not be seen (other
    /// page, background server, scrolled up in the frozen view); the read
    /// mark now waits until the room is readable, and one asked for while a
    /// request is in flight is not dropped.
    #[tokio::test]
    async fn read_mark_waits_until_the_room_is_readable() {
        let mut ws = workspace();
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        unread_sub(ctx.workspace, 1, None);
        let (mut rv, _) = RoomView::open(&mut ctx);
        ctx.workspace.apply_message(from_bob("a", 1_000_000));
        assert!(!rv.marking_read, "opened hidden: not read");
        let _ = rv.set_visible(false, &mut ctx);
        assert!(!rv.marking_read);
        // Shown while held at a jumped-to message: not yet.
        let _ = rv
            .timeline
            .jump_to("a", RcDate::from_millis(1_000_000), &mut ctx);
        assert!(!rv.timeline.at_bottom());
        let _ = rv.set_visible(true, &mut ctx);
        assert!(!rv.marking_read, "scrolled up");
        // Back at the bottom: the open's read mark runs.
        let a = rv
            .timeline
            .update(timeline::Message::JumpToLatest, &mut ctx);
        let _ = rv.handle_effects(a.map(Message::Timeline), &mut ctx);
        assert!(rv.marking_read && !rv.read_on_show);
        // A new message while the request is in flight: read again after it.
        ctx.workspace.apply_message(from_bob("b", 2_000_000));
        unread_sub(ctx.workspace, 2, None);
        assert!(rv.wants_auto_read(ctx.workspace));
        let _ = rv.mark_read(&ctx);
        assert!(rv.read_again);
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        assert!(rv.marking_read, "the dropped mark was sent");
        assert!(!rv.wants_auto_read(ctx.workspace));
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        assert!(!rv.marking_read);
        // Hidden again: new messages wait, and are read once it is shown.
        let _ = rv.set_visible(false, &mut ctx);
        ctx.workspace.apply_message(from_bob("c", 3_000_000));
        unread_sub(ctx.workspace, 1, None);
        let _ = rv.mark_read(&ctx);
        assert!(!rv.marking_read);
        let _ = rv.set_visible(true, &mut ctx);
        assert!(rv.marking_read, "read when shown again");
    }

    /// Review: the automatic read mark cleared unread threads
    /// (`readThreads: true`) and, with `{rid}` only, an alert kept by unread
    /// threads would be marked again and again.
    #[test]
    fn unread_threads_do_not_trigger_the_automatic_read() {
        let mut ws = workspace();
        unread_sub(&mut ws, 0, Some(vec!["P".into()]));
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (rv, _) = RoomView::open(&mut ctx);
        assert!(!rv.wants_auto_read(ctx.workspace));
        // An alert without unread threads is still read.
        unread_sub(ctx.workspace, 0, None);
        let mut sub = ctx.workspace.subscription("R1").unwrap().clone();
        sub.alert = true;
        ctx.workspace
            .apply_subscription(rustket_core::Change::Updated, sub);
        assert!(rv.wants_auto_read(ctx.workspace));
    }

    /// Review: the explicit "Mark as read" waits for an automatic mark in
    /// flight instead of being dropped.
    #[test]
    fn explicit_mark_read_is_not_dropped() {
        let mut ws = workspace();
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let _ = rv.mark_all_read(&ctx);
        assert!(rv.marking_read, "explicit: also while hidden");
        let _ = rv.handle_effects(Action::effect(Effect::MarkAllRead), &mut ctx);
        assert!(rv.read_all_again);
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        assert!(rv.marking_read && !rv.read_all_again);
    }

    /// Review: the unread divider stayed where it was when the room opened;
    /// while the room is not looked at it follows the server's `ls`.
    #[test]
    fn unread_divider_follows_ls_while_hidden() {
        let mut ws = workspace();
        let mut sub = ws.subscription("R1").unwrap().clone();
        sub.ls = RcDate::from_millis(1_000_000);
        ws.apply_subscription(rustket_core::Change::Updated, sub);
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let since = |ws: &Workspace| ws.timeline("R1").unwrap().unread_since();
        let read_at = |ws: &mut Workspace, ms: i64, unread: u64| {
            let mut sub = ws.subscription("R1").unwrap().clone();
            sub.ls = RcDate::from_millis(ms);
            sub.unread = unread;
            sub.alert = unread > 0;
            ws.apply_subscription(rustket_core::Change::Updated, sub);
            ws.take_changes()
        };
        let _ = rv.set_visible(true, &mut ctx);
        // Looked at: the divider stays.
        let ch = read_at(ctx.workspace, 2_000_000, 1);
        let _ = rv.on_changes(&ch, &mut ctx);
        assert_eq!(since(ctx.workspace), RcDate::from_millis(1_000_000));
        // In the background it follows the last read mark.
        let _ = rv.set_visible(false, &mut ctx);
        let ch = read_at(ctx.workspace, 3_000_000, 2);
        let _ = rv.on_changes(&ch, &mut ctx);
        assert_eq!(since(ctx.workspace), RcDate::from_millis(3_000_000));
        // A read room (unread 0) keeps the mark of what arrived meanwhile.
        let ch = read_at(ctx.workspace, 4_000_000, 0);
        let _ = rv.on_changes(&ch, &mut ctx);
        assert_eq!(since(ctx.workspace), RcDate::from_millis(3_000_000));
    }

    /// Review: clicking the summary of the open thread closed it.
    #[test]
    fn opening_the_open_thread_keeps_it() {
        let mut ws = workspace();
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let open = || Action::effect(Effect::OpenPanel(panel::Kind::Thread("P".into())));
        let _ = rv.handle_effects(open(), &mut ctx);
        let g = rv.panel.generation();
        let _ = rv.handle_effects(open(), &mut ctx);
        assert_eq!(rv.panel.kind(), Some(&panel::Kind::Thread("P".into())));
        assert_eq!(rv.panel.generation(), g, "the same view");
        // Header buttons still toggle other panels.
        let pinned = || Action::effect(Effect::OpenPanel(panel::Kind::Pinned));
        let _ = rv.handle_effects(pinned(), &mut ctx);
        let _ = rv.handle_effects(pinned(), &mut ctx);
        assert_eq!(rv.panel.kind(), None);
    }

    fn view_ctx<'a>(
        ws: &'a Workspace,
        sid: &'a ServerId,
        url: &'a ServerUrl,
        images: &'a Images,
        prefs: &'a UiPrefs,
    ) -> ViewCtx<'a> {
        ViewCtx {
            server_id: sid,
            server: url,
            rid: "R1",
            workspace: ws,
            images,
            prefs,
            connected: true,
            panel: None,
            now: std::time::Instant::now(),
        }
    }

    /// The room as the app shows it: under the connection strip, a dialog
    /// and the toasts (`App::view`).
    fn in_app<'a>(
        rv: &'a RoomView,
        ctx: &ViewCtx<'a>,
        toasts: &'a [rustket_ui::widget::Toast],
        strip: bool,
        dialog: bool,
    ) -> Element<'a, Message> {
        use iced::widget::text;
        let strip = strip.then(|| text("offline").into());
        let base = rustket_ui::widget::above(strip, 0.0, rv.view(ctx)).into();
        let overlay = dialog.then(|| (String::from("app"), text("dialog").into()));
        let dismiss = dialog.then_some(Message::Timeline(timeline::Message::JumpToLatest));
        // The call layer: always a stack over the modal.
        let calls = |c: Element<'a, Message>| iced::widget::stack![c, iced::widget::space()].into();
        crate::app::layered(base, overlay, dismiss, calls, toasts, |_| {
            Message::Timeline(timeline::Message::JumpToLatest)
        })
    }

    /// Opening a panel used to turn the room's body from the room column into
    /// a row (or a stack for the expanded thread) around it, rebuilding the
    /// timeline (scroll offset) and the composer (focus); expanding or
    /// collapsing the thread rebuilt the panel. Toasts, dialogs and the
    /// connection strip must not touch either.
    #[test]
    fn room_keeps_timeline_and_editor_trees() {
        use crate::tree_probe::{assert_first_kept, assert_kept, scrollable_tag, text_editor_tag};
        use iced::advanced::widget::Tree;
        use rustket_ui::widget::{BannerKind, Toast};
        let mut ws = workspace();
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let uctx = |ws: &mut Workspace, f: &mut dyn FnMut(&mut UpdateCtx<'_>)| {
            let mut ctx = UpdateCtx {
                server_id: &sid,
                server: &url,
                rid: "R1",
                workspace: ws,
                session: None,
                images: &images,
                prefs: &prefs,
            };
            f(&mut ctx);
        };
        let mut rv = None;
        uctx(&mut ws, &mut |ctx| rv = Some(RoomView::open(ctx).0));
        let mut rv = rv.unwrap();
        ws.apply_message(message("a", 1_000_000));
        ws.apply_message(message("b", 2_000_000));
        let mut p = message("P", 3_000_000);
        p.tcount = Some(0);
        ws.apply_message(p);
        let toast = [Toast {
            id: 1,
            kind: BannerKind::Info,
            title: "toast".into(),
            body: String::new(),
        }];
        let (s, ed) = (scrollable_tag(), text_editor_tag());
        let mut tree = {
            let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
            Tree::new(in_app(&rv, &ctx, &[], false, false).as_widget())
        };
        type Step<'s> = (&'s str, bool, bool, bool, Option<Effect>);
        let open = |k: panel::Kind| Some(Effect::OpenPanel(k));
        let steps: Vec<Step<'_>> = vec![
            ("toast", true, false, false, None),
            ("dialog", true, false, true, None),
            ("strip", true, true, true, None),
            ("layers gone", false, false, false, None),
            (
                "members panel",
                false,
                false,
                false,
                open(panel::Kind::Members),
            ),
            (
                "thread panel",
                false,
                false,
                false,
                open(panel::Kind::Thread("P".into())),
            ),
        ];
        for (step, toasts, strip, dialog, effect) in steps {
            if let Some(e) = effect {
                uctx(&mut ws, &mut |ctx| {
                    let _ = rv.handle_effects(Action::effect(e.clone()), ctx);
                    let g = rv.panel.generation();
                    let _ = rv.update(Message::Panel(panel::Message::Activate(g)), ctx);
                });
            }
            let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
            let list: &[Toast] = if toasts { &toast } else { &[] };
            let view = in_app(&rv, &ctx, list, strip, dialog);
            // The room's timeline and composer come first; another panel
            // replaces the previous panel's widgets.
            assert_first_kept(&mut tree, &view, s, 1, step);
            assert_first_kept(&mut tree, &view, ed, 1, step);
        }
        // The thread shows its composer: expanding and collapsing it keeps
        // both editors (and the scrollables).
        assert_eq!(crate::tree_probe::states(&tree, ed).len(), 2);
        for step in ["expanded", "collapsed"] {
            let g = rv.panel.generation();
            let m = Message::Panel(panel::Message::Body(
                g,
                panel::BodyMsg::Thread(panel::thread::Msg::ToggleExpand),
            ));
            uctx(&mut ws, &mut |ctx| {
                let _ = rv.update(m.clone(), ctx);
            });
            let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
            let view = in_app(&rv, &ctx, &[], false, false);
            assert_kept(&mut tree, &view, s, step);
            assert_kept(&mut tree, &view, ed, step);
        }
        assert!(!rv.panel.expanded());
        uctx(&mut ws, &mut |ctx| {
            let _ = rv.update(Message::Panel(panel::Message::Close), ctx);
        });
        let ctx = view_ctx(&ws, &sid, &url, &images, &prefs);
        let view = in_app(&rv, &ctx, &[], false, false);
        // The room's own editor and timeline survive closing the panel.
        let before = crate::tree_probe::states(&tree, ed);
        tree.diff(view.as_widget());
        assert_eq!(crate::tree_probe::states(&tree, ed), before[..1]);
    }

    /// Review: a history reload that arrived while the room was hidden
    /// counted the messages that came in meanwhile as seen, so the room was
    /// not marked read when shown again.
    #[test]
    fn reload_while_hidden_is_marked_when_shown() {
        let mut ws = workspace();
        // Unread when opened: the open marks it once shown.
        unread_sub(&mut ws, 1, None);
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let _ = rv.set_visible(true, &mut ctx);
        assert!(rv.marking_read);
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        // Hidden (Settings page, other server, unfocused window).
        let _ = rv.set_visible(false, &mut ctx);
        ctx.workspace.apply_message(from_bob("b", 2_000_000));
        unread_sub(ctx.workspace, 1, None);
        // The room timeline is reset and reloaded (RoomReset / resync) while hidden.
        ctx.workspace.reset_room_timelines("R1");
        let (c, t) = ctx.workspace.begin_load_older_ticket("R1").unwrap();
        let _ = rv.update(
            Message::HistoryPage(c, t, Ok(vec![from_bob("b", 2_000_000)])),
            &mut ctx,
        );
        let _ = rv.set_visible(true, &mut ctx);
        assert_eq!(ctx.workspace.subscription("R1").unwrap().unread, 1);
        assert!(
            rv.marking_read,
            "shown again with unread=1 but not marked read"
        );
    }

    /// Review: a failed automatic mark was never retried: the room stayed
    /// unread on the server while it was read on screen.
    #[test]
    fn failed_mark_is_retried() {
        let mut ws = workspace();
        // Unread when opened: the open marks it once shown.
        unread_sub(&mut ws, 1, None);
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let _ = rv.set_visible(true, &mut ctx);
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        ctx.workspace.apply_message(from_bob("b", 2_000_000));
        unread_sub(ctx.workspace, 1, None);
        assert!(rv.wants_auto_read(ctx.workspace));
        let _ = rv.mark_read(&ctx);
        assert!(rv.marking_read);
        let _ = rv.update(
            Message::MarkedRead(Err(Failure::from(no_session()))),
            &mut ctx,
        );
        // Server still says unread=1; the next check does not mark again.
        let _ = rv.set_visible(false, &mut ctx);
        let _ = rv.set_visible(true, &mut ctx);
        assert!(rv.marking_read, "failed mark not retried (unread stays 1)");
    }

    /// Review: same while the room is shown and readable: a reset + reload
    /// swallowed the read mark of the messages it brought.
    #[test]
    fn reload_while_readable_is_marked() {
        let mut ws = workspace();
        // Unread when opened: the open marks it once shown.
        unread_sub(&mut ws, 1, None);
        let sid = ServerId("s".into());
        let url = ServerUrl::parse("http://chat.test").unwrap();
        let images = Images::new(None);
        let prefs = UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "R1",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let (mut rv, _) = RoomView::open(&mut ctx);
        let _ = rv.set_visible(true, &mut ctx);
        let _ = rv.update(Message::MarkedRead(Ok(())), &mut ctx);
        // Resync: the timeline is reset, the subscription says unread=1.
        ctx.workspace.reset_room_timelines("R1");
        unread_sub(ctx.workspace, 1, None);
        let ch = ctx.workspace.take_changes();
        let _ = rv.on_changes(&ch, &mut ctx);
        if rv.wants_auto_read(ctx.workspace) {
            let _ = rv.mark_read(&ctx);
        }
        assert!(!rv.marking_read, "nothing loaded yet");
        let (c, t) = ctx.workspace.begin_load_older_ticket("R1").unwrap();
        let _ = rv.update(
            Message::HistoryPage(c, t, Ok(vec![from_bob("b", 2_000_000)])),
            &mut ctx,
        );
        let ch = ctx.workspace.take_changes();
        let _ = rv.on_changes(&ch, &mut ctx);
        // What App::after_update does next:
        if rv.wants_auto_read(ctx.workspace) {
            let _ = rv.mark_read(&ctx);
        }
        let _ = rv.set_visible(true, &mut ctx);
        assert!(rv.readable(ctx.workspace));
        assert!(
            rv.marking_read,
            "readable room with unread=1 after reload is never marked"
        );
    }
}
