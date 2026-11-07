//! Account of one server: profile, security (2FA, password), personal access
//! tokens, preferences, active sessions. Owner: account agent.
//! Strings: `locales/account.yml` (`account.*`). STUB: tabs with placeholders.

use iced::widget::{button, column, row, text};
use iced::{Element, Subscription};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_4, TEXT_BODY};

use crate::component::{Action, Ctx, Effect, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Profile,
    Security,
    Tokens,
    Preferences,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Profile, Tab::Security, Tab::Tokens, Tab::Preferences];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Tab::Profile => "account.profile",
            Tab::Security => "account.security",
            Tab::Tokens => "account.tokens",
            Tab::Preferences => "account.preferences",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Tab(Tab),
    Close,
}

#[derive(Debug, Default)]
pub struct Account {
    pub tab: Tab,
}

impl Account {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::Tab(t) => {
                self.tab = t;
                Action::none()
            }
            Message::Close => Action::effect(Effect::Close),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let _ = ctx;
        let tabs = row(Tab::ALL.iter().map(|&tab| {
            button(text(t!(tab.i18n_key()).into_owned()).size(TEXT_BODY))
                .padding([6, 12])
                .style(if tab == self.tab {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(Message::Tab(tab))
                .into()
        }))
        .spacing(SPACE_2);
        let body = column![
            tabs,
            crate::screens::page::placeholder(t!(self.tab.i18n_key()).into_owned())
        ]
        .spacing(SPACE_4);
        crate::screens::page::page(
            t!("account.title").into_owned(),
            body.into(),
            Message::Close,
        )
    }
}
