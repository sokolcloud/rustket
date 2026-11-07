//! Message composer. STUB: a disabled input placeholder; to be completed
//! (multi-line editor, send via `chat.sendMessage` with pending messages in the
//! workspace, typing notifications through `TypingThrottle`, drafts).
//! Strings: `locales/room_composer.yml` (`room_composer.*`).

use iced::widget::{container, text_input};
use iced::{Element, Length, Subscription};
use rust_i18n::t;
use rustket_core::workspace::Changes;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, SPACE_4};

use super::{Action, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Message {
    Edited(String),
}

pub struct Composer {
    rid: String,
    text: String,
}

impl Composer {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            text: String::new(),
        }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::Edited(s) => self.text = s,
        }
        Action::none()
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = (changes, ctx);
        Action::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let _ = (ctx, &self.rid);
        container(
            text_input(&t!("room_composer.placeholder"), &self.text)
                .style(style::text_input)
                .padding([10, 12]),
        )
        .padding([SPACE_3, SPACE_4])
        .width(Length::Fill)
        .into()
    }
}
