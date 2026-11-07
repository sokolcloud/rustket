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

use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_4, TEXT_SUBTITLE};
use rustket_ui::widget::{divider, icon_button};

use super::render::RenderMsg;
use super::{Action, Effect, UpdateCtx, ViewCtx};

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
    Thread(thread::Thread),
}

impl Body {
    fn new(kind: &Kind) -> Self {
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
            Kind::Thread(tmid) => Body::Thread(thread::Thread::new(tmid)),
        }
    }
}

pub struct Panel {
    rid: String,
    open: Option<Kind>,
    back: Option<Kind>,
    body: Option<Body>,
    generation: u64,
    /// Generation waiting for its activation.
    pending: Option<u64>,
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
            pending: None,
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
        self.body = Some(Body::new(&kind));
        self.open = Some(kind);
        self.generation += 1;
        self.pending = Some(self.generation);
    }

    pub fn close(&mut self) {
        self.open = None;
        self.back = None;
        self.body = None;
        self.pending = None;
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::Close => Action::effect(Effect::ClosePanel),
            Message::Back => match self.back.clone() {
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
                if g != self.generation {
                    return Action::none();
                }
                self.update_body(m, ctx)
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
            None => Action::none(),
        }
    }

    fn update_body(&mut self, m: BodyMsg, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let g = self.generation;
        match (self.body.as_mut(), m) {
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
        match self.pending {
            Some(g) => Subscription::run_with(g, |g| {
                let g = *g;
                iced::futures::stream::once(async move { Message::Activate(g) })
            }),
            None => Subscription::none(),
        }
    }

    /// `None` when closed.
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let _ = &self.rid;
        let kind = self.open.as_ref()?;
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
            None => common::loading(),
        };
        let mut head = row![].spacing(SPACE_2).align_y(Alignment::Center);
        if self.back.is_some() {
            head = head.push(icon_button(
                Icon::ArrowLeft,
                Some(Message::Back),
                t!("room_panel.back"),
            ));
        }
        head = head
            .push(
                text(t!(kind.i18n_key()).into_owned())
                    .size(TEXT_SUBTITLE)
                    .font(FONT_SEMIBOLD)
                    .wrapping(text::Wrapping::None)
                    .width(Length::Fill),
            )
            .push(icon_button(
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

/// Turns an interaction inside a rendered message body into effects.
pub(crate) fn render_action<M: Send + 'static>(ws: &Workspace, m: RenderMsg) -> Action<M> {
    match m {
        RenderMsg::OpenUrl(url) => Action::effect(Effect::OpenUrl(url)),
        RenderMsg::OpenRoom(name) => match common::rid_by_name(ws, &name) {
            Some(rid) => Action::effect(Effect::OpenRoom(rid)),
            None => Action::none(),
        },
        RenderMsg::OpenUser(username) => {
            Action::effect(Effect::OpenPanel(Kind::UserInfo(username)))
        }
        RenderMsg::Copy(text) => Action::task(iced::clipboard::write(text)),
        #[allow(unreachable_patterns)]
        _ => Action::none(),
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
        // Results tagged with an older generation never reach the new body.
        p.open(Kind::Members);
        let a = p.update(
            Message::Body(g, BodyMsg::Info(info::Msg::Confirm)),
            &mut ctx,
        );
        assert!(a.effects.is_empty());
    }

    #[test]
    fn thread_from_threads_has_back() {
        let mut p = Panel::new("r");
        p.open(Kind::Threads);
        p.open(Kind::Thread("t".into()));
        assert_eq!(p.back, Some(Kind::Threads));
    }
}
