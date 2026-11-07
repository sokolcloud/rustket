//! Create a channel, private group, team, direct message or discussion.
//! Owner: account agent. Strings: `locales/create_room.yml` (`create_room.*`).
//! STUB: a dialog with a placeholder.

use iced::widget::text;
use iced::{Element, Subscription};
use rust_i18n::t;
use rustket_ui::widget::{dialog, secondary_button};

use crate::component::{Action, Ctx, Effect, ViewCtx};

/// What to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Channel,
    Group,
    Team,
    DirectMessage,
    Discussion,
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
}

#[derive(Debug, Default)]
pub struct CreateRoom {
    pub kind: Kind,
}

impl CreateRoom {
    pub fn new(kind: Kind) -> Self {
        Self { kind }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::Close => Action::effect(Effect::Close),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let _ = ctx;
        dialog(
            t!("create_room.title"),
            text(t!("app.not_yet")),
            vec![secondary_button(t!("app.close"), Some(Message::Close)).into()],
            Some(Message::Close),
        )
    }
}
