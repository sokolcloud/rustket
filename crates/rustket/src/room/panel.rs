//! Right contextual panel (room info, members, threads, search, pinned, …).
//! Strings: `locales/room_panel.yml` (`room_panel.*`).
//!
//! `open(kind)` has no context, so a freshly opened panel asks for one
//! activation through [`Panel::subscription`]; the activation runs the first
//! load with an [`UpdateCtx`]. Results carry the panel generation, so results
//! for a panel that was replaced meanwhile are dropped.

pub mod api;
pub mod common;
pub mod files;
pub mod info;
pub mod invite;
pub mod members;
pub mod messages;
pub mod notify;
pub mod paged;
pub mod rooms;
pub mod settings;
pub mod thread;
pub mod user_info;

use iced::widget::{column, container, row};
use iced::{Alignment, Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_4, TEXT_SUBTITLE};
use rustket_ui::widget::{divider, ellipsis, icon_button};

use super::{Action, Effect, UpdateCtx, ViewCtx};
use crate::session::SessionHandle;

/// Width of the panel.
pub const WIDTH: f32 = 360.0;

/// What the panel shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Info,
    Members,
    Threads,
    /// Thread of the given parent message id (`tmid`).
    Thread(String),
    Discussions,
    Pinned,
    Starred,
    Mentions,
    Files,
    Search,
    NotificationPrefs,
    RoomSettings,
    /// Profile of a user (username).
    UserInfo(String),
    Invite,
    TeamChannels,
    /// A UIKit contextual bar (view id), drawn by `crate::uikit::bar`.
    App(String),
}

impl Kind {
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Kind::Info => "room_panel.room_info",
            Kind::Members => "room_panel.members",
            Kind::Threads => "room_panel.threads",
            Kind::Thread(_) => "room_panel.thread",
            Kind::Discussions => "room_panel.discussions",
            Kind::Pinned => "room_panel.pinned",
            Kind::Starred => "room_panel.starred",
            Kind::Mentions => "room_panel.mentions",
            Kind::Files => "room_panel.files",
            Kind::Search => "room_panel.search",
            Kind::NotificationPrefs => "room_panel.notification_prefs",
            Kind::RoomSettings => "room_panel.room_settings",
            Kind::UserInfo(_) => "room_panel.user_info",
            Kind::Invite => "room_panel.invite",
            Kind::TeamChannels => "room_panel.team_channels",
            Kind::App(_) => "uikit.app_panel",
        }
    }

    /// Panels reached from another one get a back button to it.
    fn back_target(&self, from: &Kind) -> bool {
        matches!(
            (from, self),
            (Kind::Members, Kind::UserInfo(_) | Kind::Invite)
                | (Kind::Info, Kind::RoomSettings | Kind::TeamChannels)
                | (Kind::Threads, Kind::Thread(_))
                | (Kind::Search, Kind::UserInfo(_))
        )
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
    Back,
    /// First load of the panel with generation `u64`.
    Activate(u64),
    Body(u64, BodyMsg),
    /// "+" in the Discussions panel header.
    CreateDiscussion,
    /// Show the UIKit contextual bar `view_id` (keeps it when shown).
    ShowApp(String),
    /// Close the panel if it shows bar `view_id`.
    HideApp(String),
    /// Open (or keep) a panel.
    Open(Kind),
    /// An interaction inside a contextual bar.
    UiKit(Box<crate::uikit::UiKitMsg>),
}

/// Messages of the panel bodies.
#[derive(Debug, Clone)]
pub enum BodyMsg {
    Info(info::Msg),
    User(user_info::Msg),
    Members(members::Msg),
    Messages(messages::Msg),
    Rooms(rooms::Msg),
    Files(files::Msg),
    Notify(notify::Msg),
    Settings(settings::Msg),
    Invite(invite::Msg),
    Thread(thread::Msg),
}

/// State of the open panel.
enum Body {
    Info(info::Info),
    User(user_info::UserInfo),
    Members(members::Members),
    Messages(messages::Messages),
    Rooms(rooms::Rooms),
    Files(files::Files),
    Notify(notify::Notify),
    Settings(settings::Settings),
    Invite(invite::Invite),
    Thread(Box<thread::Thread>),
    /// A contextual bar: its state lives in the workspace's UIKit store.
    App,
}

impl Body {
    fn new(kind: &Kind, rid: &str) -> Self {
        match kind {
            Kind::Info => Body::Info(info::Info::default()),
            Kind::UserInfo(u) => Body::User(user_info::UserInfo::new(u)),
            Kind::Members => Body::Members(members::Members::default()),
            Kind::Threads => Body::Messages(messages::Messages::new(messages::Mode::Threads)),
            Kind::Pinned => Body::Messages(messages::Messages::new(messages::Mode::List(
                api::MessageList::Pinned,
            ))),
            Kind::Starred => Body::Messages(messages::Messages::new(messages::Mode::List(
                api::MessageList::Starred,
            ))),
            Kind::Mentions => Body::Messages(messages::Messages::new(messages::Mode::List(
                api::MessageList::Mentions,
            ))),
            Kind::Search => Body::Messages(messages::Messages::new(messages::Mode::Search)),
            Kind::Discussions => Body::Rooms(rooms::Rooms::new(rooms::Mode::Discussions)),
            Kind::TeamChannels => Body::Rooms(rooms::Rooms::new(rooms::Mode::TeamChannels)),
            Kind::Files => Body::Files(files::Files::default()),
            Kind::NotificationPrefs => Body::Notify(notify::Notify::default()),
            Kind::RoomSettings => Body::Settings(settings::Settings::default()),
            Kind::Invite => Body::Invite(invite::Invite::default()),
            Kind::Thread(tmid) => Body::Thread(Box::new(thread::Thread::new(rid, tmid))),
            Kind::App(_) => Body::App,
        }
    }
}

/// The panel a child was opened from, kept with its state (query, filter,
/// results) so Back restores it instead of reloading.
struct Stash {
    kind: Kind,
    body: Body,
    generation: u64,
}

pub struct Panel {
    rid: String,
    open: Option<Kind>,
    back: Option<Kind>,
    body: Option<Body>,
    generation: u64,
    /// Last generation handed out (generations are never reused).
    counter: u64,
    /// Generation waiting for its activation.
    pending: Option<u64>,
    stash: Option<Stash>,
    /// Bodies replaced or closed since the last [`Panel::retire`] (a thread
    /// keeps its draft and closes its workspace timeline there).
    retired: Vec<Body>,
    /// The thread view was expanded last (new threads open the same way).
    thread_expanded: bool,
}

fn wrap<M: Send + 'static>(generation: u64, a: Action<M>, f: fn(M) -> BodyMsg) -> Action<Message> {
    a.map(move |m| Message::Body(generation, f(m)))
}

impl Panel {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            open: None,
            back: None,
            body: None,
            generation: 0,
            counter: 0,
            pending: None,
            stash: None,
            retired: Vec::new(),
            thread_expanded: false,
        }
    }

    /// Lets replaced/closed bodies release workspace state (thread
    /// timelines, drafts) and stop their typing indicator (the task only
    /// carries that call). Runs around every panel update; the room view
    /// also calls it after opening or closing the panel.
    pub fn retire(&mut self, ws: &mut Workspace, session: Option<&SessionHandle>) -> Task<Message> {
        let mut tasks = Vec::new();
        for body in self.retired.drain(..) {
            if let Body::Thread(mut t) = body {
                tasks.push(t.leave(session).discard());
                t.on_close(ws);
            }
        }
        Task::batch(tasks)
    }

    fn drop_body(&mut self, body: Option<Body>) {
        if let Some(b) = body {
            self.retired.push(b);
        }
    }

    /// Width of the open panel (the thread view is wider).
    pub fn width(&self) -> f32 {
        match &self.body {
            Some(Body::Thread(t)) => t.width(),
            _ => WIDTH,
        }
    }

    /// The open panel is an expanded thread (drawn over the room column).
    pub fn expanded(&self) -> bool {
        matches!(&self.body, Some(Body::Thread(t)) if t.expanded())
    }

    /// The open thread, if the panel shows one.
    pub fn thread_tmid(&self) -> Option<&str> {
        match &self.body {
            Some(Body::Thread(t)) => Some(t.tmid()),
            _ => None,
        }
    }

    /// Focuses the thread composer (reply in thread on an open thread).
    pub fn focus_thread_composer(&self) -> Task<Message> {
        let g = self.generation;
        match &self.body {
            Some(Body::Thread(t)) if self.pending.is_none() => t
                .focus_composer()
                .map(move |m| Message::Body(g, BodyMsg::Thread(m))),
            _ => Task::none(),
        }
    }

    /// Highlights reply `mid` in the open thread once it is loaded.
    pub fn focus_thread_reply(&mut self, mid: &str) {
        if let Some(Body::Thread(t)) = &mut self.body {
            t.set_focus(Some(mid.to_owned()));
        }
    }

    /// Marks the open thread read when it is at the bottom and has unread
    /// replies; the caller knows the window is focused.
    pub fn mark_thread_read(&mut self, ctx: &UpdateCtx<'_>) -> Task<Message> {
        let g = self.generation;
        match &mut self.body {
            Some(Body::Thread(t)) if self.pending.is_none() => t
                .mark_read(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Thread(m))),
            _ => Task::none(),
        }
    }

    /// The dialog or lightbox of the open body (shown over the whole room),
    /// with its key and the message that dismisses it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(u8, Element<'a, Message>, Message)> {
        let g = self.generation;
        match self.body.as_ref()? {
            Body::Thread(b) => b.overlay(ctx).map(|(k, e, d)| {
                (
                    k,
                    e.map(move |m| Message::Body(g, BodyMsg::Thread(m))),
                    Message::Body(g, BodyMsg::Thread(d)),
                )
            }),
            Body::Messages(b) => b.overlay(ctx).map(|(e, d)| {
                (
                    3,
                    e.map(move |m| Message::Body(g, BodyMsg::Messages(m))),
                    Message::Body(g, BodyMsg::Messages(d)),
                )
            }),
            Body::Files(b) => b.overlay(ctx).map(|(e, d)| {
                (
                    3,
                    e.map(move |m| Message::Body(g, BodyMsg::Files(m))),
                    Message::Body(g, BodyMsg::Files(d)),
                )
            }),
            Body::Info(b) => b.overlay(ctx).map(|(e, d)| {
                (
                    3,
                    e.map(move |m| Message::Body(g, BodyMsg::Info(m))),
                    Message::Body(g, BodyMsg::Info(d)),
                )
            }),
            _ => None,
        }
    }

    pub fn kind(&self) -> Option<&Kind> {
        self.open.as_ref()
    }

    /// Generation of the open panel (tags [`Message::Body`]).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Opens `kind`; opening the kind that is already shown closes the panel.
    pub fn open(&mut self, kind: Kind) {
        if self.open.as_ref() == Some(&kind) {
            self.close();
            return;
        }
        self.back = self
            .open
            .as_ref()
            .filter(|from| kind.back_target(from))
            .cloned();
        let previous = self.body.take();
        let old_stash = self.stash.take();
        self.drop_body(old_stash.map(|s| s.body));
        self.stash = match (&self.back, previous) {
            // A body that never ran its first load is not worth keeping.
            (Some(from), Some(body)) if self.pending != Some(self.generation) => Some(Stash {
                kind: from.clone(),
                body,
                generation: self.generation,
            }),
            (_, body) => {
                self.drop_body(body);
                None
            }
        };
        let mut body = Body::new(&kind, &self.rid);
        if let Body::Thread(t) = &mut body {
            t.set_expanded(self.thread_expanded);
        }
        self.body = Some(body);
        self.open = Some(kind);
        self.counter += 1;
        self.generation = self.counter;
        self.pending = Some(self.generation);
    }

    /// Back to the stashed panel; `false` when there is none for `kind`.
    fn restore(&mut self, kind: &Kind) -> bool {
        match self.stash.take() {
            Some(st) if &st.kind == kind => {
                self.open = Some(st.kind);
                let current = self.body.replace(st.body);
                self.drop_body(current);
                self.generation = st.generation;
                self.back = None;
                self.pending = None;
                true
            }
            _ => false,
        }
    }

    /// The room view is closing (room switch, logout, …): closes the panel;
    /// a thread view stores its draft and stops its typing indicator. The
    /// task only carries the stop-typing call.
    pub fn leave(&mut self, ws: &mut Workspace, session: Option<&SessionHandle>) -> Task<Message> {
        self.close();
        self.retire(ws, session)
    }

    /// The room stays open but is not shown (another server became active):
    /// the open thread stops its typing indicator.
    pub fn hide(&mut self, session: Option<&SessionHandle>) -> Task<Message> {
        let g = self.generation;
        match &mut self.body {
            Some(Body::Thread(t)) => t
                .leave(session)
                .map(move |m| Message::Body(g, BodyMsg::Thread(m))),
            _ => Task::none(),
        }
    }

    pub fn close(&mut self) {
        self.open = None;
        self.back = None;
        let body = self.body.take();
        self.drop_body(body);
        let stash = self.stash.take();
        self.drop_body(stash.map(|s| s.body));
        self.pending = None;
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let before = self.retire(ctx.workspace, ctx.session);
        let mut a = self.update_inner(msg, ctx);
        // Back to a stashed panel retires the thread it leaves.
        let after = self.retire(ctx.workspace, ctx.session);
        a.task = Task::batch([before, a.task, after]);
        a
    }

    fn update_inner(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::Close => Action::effect(Effect::ClosePanel),
            Message::CreateDiscussion => Action::effect(Effect::StartDiscussion { pmid: None }),
            Message::ShowApp(id) => {
                let kind = Kind::App(id);
                if self.open.as_ref() == Some(&kind) {
                    return Action::none();
                }
                Action::effect(Effect::OpenPanel(kind))
            }
            Message::HideApp(id) => {
                if self.open.as_ref() == Some(&Kind::App(id)) {
                    return Action::effect(Effect::ClosePanel);
                }
                Action::none()
            }
            Message::Open(kind) => {
                if self.open.as_ref() == Some(&kind) {
                    return Action::none();
                }
                Action::effect(Effect::OpenPanel(kind))
            }
            Message::UiKit(m) => Action::effect(Effect::UiKit(m)),
            Message::Back => match self.back.clone() {
                Some(kind) if self.restore(&kind) => Action::none(),
                Some(kind) => Action::effect(Effect::OpenPanel(kind)),
                None => Action::effect(Effect::ClosePanel),
            },
            Message::Activate(g) => {
                if self.pending != Some(g) || g != self.generation {
                    return Action::none();
                }
                self.pending = None;
                self.activate(ctx)
            }
            Message::Body(g, m) => {
                if g == self.generation {
                    let a = Self::update_body(self.body.as_mut(), g, m, ctx);
                    if let Some(Body::Thread(t)) = &self.body {
                        self.thread_expanded = t.expanded();
                    }
                    return a;
                }
                // Results for the stashed panel keep it up to date.
                match self.stash.as_mut() {
                    Some(st) if st.generation == g => {
                        Self::update_body(Some(&mut st.body), g, m, ctx)
                    }
                    _ => Action::none(),
                }
            }
        }
    }

    fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let g = self.generation;
        match self.body.as_mut() {
            Some(Body::Info(b)) => wrap(g, b.activate(ctx), BodyMsg::Info),
            Some(Body::User(b)) => wrap(g, b.activate(ctx), BodyMsg::User),
            Some(Body::Members(b)) => wrap(g, b.activate(ctx), BodyMsg::Members),
            Some(Body::Messages(b)) => wrap(g, b.activate(ctx), BodyMsg::Messages),
            Some(Body::Rooms(b)) => wrap(g, b.activate(ctx), BodyMsg::Rooms),
            Some(Body::Files(b)) => wrap(g, b.activate(ctx), BodyMsg::Files),
            Some(Body::Notify(b)) => wrap(g, b.activate(ctx), BodyMsg::Notify),
            Some(Body::Settings(b)) => wrap(g, b.activate(ctx), BodyMsg::Settings),
            Some(Body::Invite(b)) => wrap(g, b.activate(ctx), BodyMsg::Invite),
            Some(Body::Thread(b)) => wrap(g, b.activate(ctx), BodyMsg::Thread),
            Some(Body::App) | None => Action::none(),
        }
    }

    fn update_body(
        body: Option<&mut Body>,
        g: u64,
        m: BodyMsg,
        ctx: &mut UpdateCtx<'_>,
    ) -> Action<Message> {
        match (body, m) {
            (Some(Body::Info(b)), BodyMsg::Info(m)) => wrap(g, b.update(m, ctx), BodyMsg::Info),
            (Some(Body::User(b)), BodyMsg::User(m)) => wrap(g, b.update(m, ctx), BodyMsg::User),
            (Some(Body::Members(b)), BodyMsg::Members(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Members)
            }
            (Some(Body::Messages(b)), BodyMsg::Messages(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Messages)
            }
            (Some(Body::Rooms(b)), BodyMsg::Rooms(m)) => wrap(g, b.update(m, ctx), BodyMsg::Rooms),
            (Some(Body::Files(b)), BodyMsg::Files(m)) => wrap(g, b.update(m, ctx), BodyMsg::Files),
            (Some(Body::Notify(b)), BodyMsg::Notify(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Notify)
            }
            (Some(Body::Settings(b)), BodyMsg::Settings(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Settings)
            }
            (Some(Body::Invite(b)), BodyMsg::Invite(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Invite)
            }
            (Some(Body::Thread(b)), BodyMsg::Thread(m)) => {
                wrap(g, b.update(m, ctx), BodyMsg::Thread)
            }
            _ => Action::none(),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let retired = self.retire(ctx.workspace, ctx.session);
        let mut a = self.body_changes(changes, ctx);
        a.task = Task::batch([retired, a.task]);
        a
    }

    fn body_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let g = self.generation;
        if self.pending.is_some() {
            return Action::none();
        }
        match self.body.as_mut() {
            Some(Body::Info(b)) => wrap(g, b.on_changes(changes, ctx), BodyMsg::Info),
            Some(Body::User(b)) => wrap(g, b.on_changes(changes, ctx), BodyMsg::User),
            Some(Body::Rooms(b)) => wrap(g, b.on_changes(changes, ctx), BodyMsg::Rooms),
            Some(Body::Settings(b)) => wrap(g, b.on_changes(changes, ctx), BodyMsg::Settings),
            Some(Body::Thread(b)) => wrap(g, b.on_changes(changes, ctx), BodyMsg::Thread),
            _ => Action::none(),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let activate = match self.pending {
            Some(g) => Subscription::run_with(g, |g| {
                let g = *g;
                iced::futures::stream::once(async move { Message::Activate(g) })
            }),
            None => Subscription::none(),
        };
        let body = match &self.body {
            Some(Body::Thread(t)) if self.pending.is_none() => t
                .subscription()
                .with(self.generation)
                .map(|(g, m)| Message::Body(g, BodyMsg::Thread(m))),
            _ => Subscription::none(),
        };
        Subscription::batch([activate, body])
    }

    /// `None` when closed.
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let _ = &self.rid;
        let kind = self.open.as_ref()?;
        if let Kind::App(view_id) = kind {
            return Some(
                crate::uikit::bar::view(view_id, ctx).map(|m| Message::UiKit(Box::new(m))),
            );
        }
        let g = self.generation;
        let body: Element<'a, Message> = match self.body.as_ref() {
            Some(Body::Info(b)) => b.view(ctx).map(move |m| Message::Body(g, BodyMsg::Info(m))),
            Some(Body::User(b)) => b.view(ctx).map(move |m| Message::Body(g, BodyMsg::User(m))),
            Some(Body::Members(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Members(m))),
            Some(Body::Messages(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Messages(m))),
            Some(Body::Rooms(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Rooms(m))),
            Some(Body::Files(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Files(m))),
            Some(Body::Notify(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Notify(m))),
            Some(Body::Settings(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Settings(m))),
            Some(Body::Invite(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Invite(m))),
            Some(Body::Thread(b)) => b
                .view(ctx)
                .map(move |m| Message::Body(g, BodyMsg::Thread(m))),
            Some(Body::App) | None => common::loading(),
        };
        let mut head = row![].spacing(SPACE_2).align_y(Alignment::Center);
        if self.back.is_some() {
            head = head.push(icon_button(
                Icon::ArrowLeft,
                Some(Message::Back),
                t!("room_panel.back"),
            ));
        }
        let title = match self.body.as_ref() {
            Some(Body::Thread(t)) => t.title(),
            _ => t!(kind.i18n_key()).into_owned(),
        };
        head = head.push(
            ellipsis(title)
                .size(TEXT_SUBTITLE)
                .font(FONT_SEMIBOLD)
                .width(Length::Fill),
        );
        if let Some(Body::Thread(t)) = self.body.as_ref() {
            for b in t.header_actions(ctx) {
                head = head.push(b.map(move |m| Message::Body(g, BodyMsg::Thread(m))));
            }
        }
        if *kind == Kind::Discussions
            && ctx.workspace.subscription(ctx.rid).is_some()
            && rustket_core::permissions::can_create_discussion(ctx.workspace, Some(ctx.rid))
        {
            head = head.push(icon_button(
                Icon::Plus,
                Some(Message::CreateDiscussion),
                t!("room_panel.create_discussion"),
            ));
        }
        head = head.push(icon_button(
            Icon::X,
            Some(Message::Close),
            t!("room_panel.close"),
        ));
        Some(
            column![
                container(head)
                    .height(rustket_ui::theme::HEADER_HEIGHT)
                    .padding([0.0, SPACE_4])
                    .center_y(rustket_ui::theme::HEADER_HEIGHT),
                divider(),
                container(body).width(Length::Fill).height(Length::Fill),
            ]
            .height(Length::Fill)
            .into(),
        )
    }
}

/// Debounce delay of search fields.
pub(crate) const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(350);

/// A task that yields `msg` after [`DEBOUNCE`].
pub(crate) fn debounce<M: Send + 'static>(msg: M) -> Task<M> {
    Task::perform(tokio::time::sleep(DEBOUNCE), move |_| msg)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn open_toggle_and_back() {
        let mut p = Panel::new("r");
        p.open(Kind::Members);
        assert_eq!(p.kind(), Some(&Kind::Members));
        let g1 = p.pending.expect("pending");
        p.open(Kind::UserInfo("bob".into()));
        assert_eq!(p.back, Some(Kind::Members));
        assert!(p.pending.expect("pending") > g1);
        p.open(Kind::Files);
        assert_eq!(p.back, None);
        p.open(Kind::Files);
        assert_eq!(p.kind(), None);
        assert!(p.body.is_none());
        assert!(p.pending.is_none());
    }

    #[test]
    fn activation_and_stale_generations() {
        use rocketchat::ServerUrl;
        use rustket_core::ServerId;
        use rustket_core::workspace::Change;
        let mut ws = Workspace::new();
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(serde_json::json!({
                "_id": "r", "t": "c", "name": "dev", "topic": "Release", "description": " "
            }))
            .unwrap(),
        );
        let (sid, url) = (
            ServerId("s".into()),
            ServerUrl::parse("http://localhost:3000").unwrap(),
        );
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut p = Panel::new("r");
        p.open(Kind::Info);
        let g = p.generation();
        // A stale activation is ignored, the current one runs once.
        let _ = p.update(Message::Activate(g + 7), &mut ctx);
        assert_eq!(p.pending, Some(g));
        let _ = p.update(Message::Activate(g), &mut ctx);
        assert_eq!(p.pending, None);
        match &p.body {
            Some(Body::Info(info)) => assert_eq!(info.text_keys(), vec!["room_panel.topic"]),
            _ => panic!("info body expected"),
        }
        // Results tagged with an older generation never reach the new body:
        // Info → Members → Info gives the same body type a new generation.
        p.open(Kind::Members);
        p.open(Kind::Info);
        let g_now = p.generation();
        assert!(g_now > g);
        let open = |g| Message::Body(g, BodyMsg::Info(info::Msg::Open(Kind::RoomSettings)));
        let a = p.update(open(g), &mut ctx);
        assert!(a.effects.is_empty());
        let a = p.update(open(g_now), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::OpenPanel(Kind::RoomSettings)]
        ));
    }

    #[test]
    fn back_restores_the_stashed_panel() {
        let mut p = Panel::new("r");
        p.open(Kind::Members);
        let g_members = p.generation();
        p.pending = None; // activated
        if let Some(Body::Members(m)) = p.body.as_mut() {
            m.set_query_for_test("bo");
        }
        p.open(Kind::UserInfo("bob".into()));
        assert!(p.generation() > g_members);
        assert!(p.restore(&Kind::Members));
        assert_eq!(p.kind(), Some(&Kind::Members));
        assert_eq!(p.generation(), g_members);
        assert!(p.pending.is_none());
        match &p.body {
            Some(Body::Members(m)) => assert_eq!(m.query_for_test(), "bo"),
            _ => panic!("members body expected"),
        }
        // New generations never reuse one handed out before.
        p.open(Kind::Files);
        assert!(p.generation() > g_members + 1);
    }

    #[test]
    fn thread_from_threads_has_back() {
        let mut p = Panel::new("r");
        p.open(Kind::Threads);
        p.open(Kind::Thread("t".into()));
        assert_eq!(p.back, Some(Kind::Threads));
    }
}
