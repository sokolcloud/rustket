//! Directory of one server: channels, users, teams (search, join, open DM).
//! Owner: account agent. Strings: `locales/directory.yml` (`directory.*`).
//! STUB: placeholder.

use iced::{Element, Subscription};
use rust_i18n::t;

use crate::component::{Action, Ctx, Effect, ViewCtx};

#[derive(Debug, Clone)]
pub enum Message {
    Close,
}

#[derive(Debug, Default)]
pub struct Directory;

impl Directory {
    pub fn new() -> Self {
        Self
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
        crate::screens::page::page(
            t!("directory.title").into_owned(),
            crate::screens::page::placeholder(t!("directory.title").into_owned()),
            Message::Close,
        )
    }
}
