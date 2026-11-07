//! Quick switcher (Ctrl+K): find a room or user and open it.
//! Owner: account agent. Strings: `locales/quick_switcher.yml`
//! (`quick_switcher.*`). STUB: a search field over the room list.

use iced::widget::{button, column, text, text_input};
use iced::{Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_core::room_list::{self, RoomListOptions};
use rustket_ui::style;
use rustket_ui::theme::SPACE_2;
use rustket_ui::widget::dialog;

use crate::component::{Action, Ctx, Effect, ViewCtx};

pub const INPUT_ID: &str = "quick-switcher";

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    Open(String),
    Submit,
    Close,
}

#[derive(Debug, Default)]
pub struct QuickSwitcher {
    pub query: String,
}

impl QuickSwitcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Focuses the search field (run when the dialog opens).
    pub fn focus() -> Task<Message> {
        iced::widget::operation::focus(INPUT_ID)
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Query(q) => {
                self.query = q;
                Action::none()
            }
            Message::Open(rid) => Action::effect(Effect::OpenRoom(rid)).with(Effect::Close),
            Message::Submit => match matches(ctx.workspace, &self.query).first() {
                Some((rid, _)) => Action::effect(Effect::OpenRoom(rid.clone())).with(Effect::Close),
                None => Action::none(),
            },
            Message::Close => Action::effect(Effect::Close),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let list = column(matches(ctx.workspace, &self.query).into_iter().take(8).map(
            |(rid, name)| {
                button(text(name))
                    .width(Length::Fill)
                    .style(style::button_list_item(false))
                    .on_press(Message::Open(rid))
                    .into()
            },
        ));
        dialog(
            t!("quick_switcher.title"),
            column![
                text_input(&t!("quick_switcher.placeholder"), &self.query)
                    .id(INPUT_ID)
                    .on_input(Message::Query)
                    .on_submit(Message::Submit)
                    .style(style::text_input)
                    .padding([8, 10]),
                list,
            ]
            .spacing(SPACE_2),
            Vec::new(),
            Some(Message::Close),
        )
    }
}

/// `(rid, display name)` of rooms matching `query`, in sidebar order.
fn matches(ws: &rustket_core::Workspace, query: &str) -> Vec<(String, String)> {
    let opts = RoomListOptions::default();
    room_list::compute(ws, &opts, query)
        .into_iter()
        .flat_map(|s| s.items)
        .map(|i| (i.rid, i.display_name))
        .collect()
}
