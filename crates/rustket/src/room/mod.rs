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

use iced::widget::{column, container, row};
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
use rustket_ui::widget::{BannerKind, empty_state, vertical_divider};

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
    /// Mark this room as read now.
    MarkRead,
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
    /// Mark the room unread from message `mid`. The app leaves the room
    /// first, like the web client, so the auto-read of the open room cannot
    /// undo it.
    MarkUnread(String),
    /// Open the "create discussion" dialog with this room as the parent,
    /// optionally started from message `pmid`.
    StartDiscussion {
        pmid: Option<String>,
    },
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
}

/// Everything shown for one open room.
pub struct RoomView {
    pub rid: String,
    watch: Option<WatchGuard>,
    pub header: header::Header,
    pub timeline: timeline::Timeline,
    pub composer: composer::Composer,
    pub panel: panel::Panel,
    marking_read: bool,
    /// Newest message from others (`ts` ms) the user has seen as read (last
    /// read mark, or history loaded after the read mark of `open`): the
    /// automatic read mark only runs for messages newer than that, so an
    /// unread mark made elsewhere is not undone.
    marked_upto: Option<i64>,
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
            marking_read: false,
            marked_upto: None,
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
        // A preview has no subscription to mark.
        if !view.is_preview(ctx.workspace) {
            let read = view.mark_read(ctx);
            action.task = Task::batch([action.task, read]);
        }
        (view, action)
    }

    /// Forgets the timeline and stops the typing indicator; the room watch
    /// ends when `self` is dropped. The returned task only carries the
    /// stop-typing call (its result is not needed).
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
        self.composer.leave(session).map(Message::Composer)
    }

    /// Whether the room is shown as a preview (not joined).
    pub fn is_preview(&self, ws: &Workspace) -> bool {
        ws.subscription(&self.rid).is_none()
    }

    /// Whether the automatic read mark (focused room) should run: the room
    /// is unread and a message from someone else newer than the last read
    /// mark arrived, or only the alert is set (replies in a followed thread,
    /// which are not in the room timeline). An unread mark made elsewhere
    /// (unread > 0, no newer message) is kept.
    pub fn wants_auto_read(&self, ws: &Workspace) -> bool {
        let Some(sub) = ws.subscription(&self.rid) else {
            return false;
        };
        if sub.unread == 0 {
            return sub.alert;
        }
        newest_from_others(ws, &self.rid) > self.marked_upto
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

    /// `subscriptions.read` (one request at a time).
    pub fn mark_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        if self.marking_read {
            return Task::none();
        }
        self.marking_read = true;
        self.marked_upto = self
            .marked_upto
            .max(newest_from_others(ctx.workspace, &self.rid));
        let rid = self.rid.clone();
        ctx.rest_task(
            move |rest| async move { rest.subscriptions_read(&rid).await },
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
                // `open` marked the room read: the loaded history counts as seen.
                if !self.is_preview(ctx.workspace) {
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
                if let Err(e) = r {
                    tracing::debug!(rid = %self.rid, error = %e, "subscriptions.read failed");
                }
                Action::none()
            }
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
                Effect::OpenPanel(kind) => self.panel.open(kind),
                Effect::Compose(intent) => {
                    if let ComposeIntent::ReplyInThread(tmid) = &intent
                        && self.panel.kind() != Some(&panel::Kind::Thread(tmid.clone()))
                    {
                        self.panel.open(panel::Kind::Thread(tmid.clone()));
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
                Effect::ClosePanel => self.panel.close(),
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
            self.timeline.view(ctx).map(Message::Timeline)
        };
        let main = column![
            self.header.view(ctx).map(Message::Header),
            container(timeline).width(Length::Fill).height(Length::Fill),
            self.composer.view(ctx).map(Message::Composer),
        ]
        .width(Length::Fill)
        .height(Length::Fill);
        let body: Element<'a, Message> = match self.panel.view(ctx) {
            Some(panel) => row![
                main,
                vertical_divider(),
                container(panel.map(Message::Panel))
                    .width(Length::Fixed(panel::WIDTH))
                    .height(Length::Fill)
                    .style(style::container_sidebar),
            ]
            .into(),
            None => main.into(),
        };
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::container_room)
            .into()
    }
}
