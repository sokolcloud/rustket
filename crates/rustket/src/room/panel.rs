//! Right contextual panel (room info, members, thread, search, pinned, …).
//! STUB: renders the panel title and a close button; to be completed.
//! Strings: `locales/room_panel.yml` (`room_panel.*`).

use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length, Subscription};
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_3, SPACE_4, TEXT_SUBTITLE};
use rustket_ui::widget::{divider, icon_button};

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
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
}

pub struct Panel {
    rid: String,
    open: Option<Kind>,
}

impl Panel {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            open: None,
        }
    }

    pub fn kind(&self) -> Option<&Kind> {
        self.open.as_ref()
    }

    /// Opens `kind`; opening the kind that is already shown closes the panel.
    pub fn open(&mut self, kind: Kind) {
        if self.open.as_ref() == Some(&kind) {
            self.open = None;
        } else {
            self.open = Some(kind);
        }
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::Close => Action::effect(Effect::ClosePanel),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = (changes, ctx);
        Action::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    /// `None` when closed.
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let _ = (ctx, &self.rid);
        let kind = self.open.as_ref()?;
        Some(
            column![
                row![
                    text(t!(kind.i18n_key()).into_owned())
                        .size(TEXT_SUBTITLE)
                        .font(FONT_SEMIBOLD)
                        .width(Length::Fill),
                    icon_button(Icon::X, Some(Message::Close), t!("room_panel.close")),
                ]
                .align_y(Alignment::Center)
                .padding([SPACE_3, SPACE_4]),
                divider(),
                rustket_ui::widget::empty_state(
                    Icon::Info,
                    t!("room_panel.placeholder"),
                    match kind {
                        Kind::Thread(id) | Kind::UserInfo(id) => id.clone(),
                        _ => String::new(),
                    }
                ),
            ]
            .into(),
        )
    }
}
