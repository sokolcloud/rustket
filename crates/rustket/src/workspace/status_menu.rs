//! Status menu: the user's avatar in the sidebar header and its popover
//! (online/away/busy/offline, custom status message, account, settings,
//! logout). Owner: desktop agent. Strings: `locales/status_menu.yml`
//! (`status_menu.*`). STUB: account/session entries only.

use iced::widget::{button, column, row};
use iced::{Alignment, Element, Length, Subscription, Theme};
use rust_i18n::t;
use rustket_core::room_list::Avatar;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, TEXT_BODY, TEXT_CAPTION, tokens};
use rustket_ui::widget::{
    AvatarShape, MenuEntry, MenuItem, Placement, avatar, ellipsis, popover_menu,
};

use crate::component::{Action, Ctx, Effect, Nav, ViewCtx};
use crate::workspace::sidebar::presence;

#[derive(Debug, Clone)]
pub enum Message {
    Toggle(bool),
    Account,
    Settings,
    Logout,
    LogoutEverywhere,
    RemoveServer,
}

#[derive(Debug, Default)]
pub struct StatusMenu {
    pub open: bool,
}

impl StatusMenu {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        let _ = ctx;
        if !matches!(msg, Message::Toggle(_)) {
            self.open = false;
        }
        match msg {
            Message::Toggle(open) => {
                self.open = open;
                Action::none()
            }
            Message::Account => Action::effect(Effect::Navigate(Nav::Account)),
            Message::Settings => Action::effect(Effect::Navigate(Nav::Settings)),
            Message::Logout => Action::effect(Effect::Logout { everywhere: false }),
            Message::LogoutEverywhere => Action::effect(Effect::Logout { everywhere: true }),
            Message::RemoveServer => Action::effect(Effect::RemoveServer),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    /// The header trigger (avatar, name) with the popover.
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let username = ws.username().unwrap_or_default().to_owned();
        let name = ws
            .me()
            .and_then(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| username.clone());
        let src = ctx.images.avatar(
            ctx.server_id,
            ctx.server,
            &Avatar::User {
                username: username.clone(),
            },
            &name,
        );
        let me_avatar = avatar(src, 32.0, AvatarShape::Rounded)
            .presence(Some(presence(ws.my_status())))
            .ring_with(|t| t.surface_sidebar);
        let trigger = button(
            row![
                me_avatar,
                column![
                    ellipsis(name).size(TEXT_BODY).font(FONT_SEMIBOLD),
                    ellipsis(format!("@{username}"))
                        .size(TEXT_CAPTION)
                        .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
                ]
                .width(Length::Fill),
                icon::sized(Icon::ChevronDown, 14.0),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .padding([4, 6])
        .width(Length::Fill)
        .style(style::button_ghost)
        .on_press(Message::Toggle(!self.open));
        let entries = vec![
            MenuEntry::Header(t!("status_menu.status_soon").into_owned()),
            MenuEntry::Separator,
            MenuItem::new(t!("status_menu.account"), Message::Account)
                .icon(Icon::User)
                .into(),
            MenuItem::new(t!("status_menu.settings"), Message::Settings)
                .icon(Icon::Settings)
                .into(),
            MenuEntry::Separator,
            MenuItem::new(t!("status_menu.logout"), Message::Logout)
                .icon(Icon::LogOut)
                .into(),
            MenuItem::new(
                t!("status_menu.logout_everywhere"),
                Message::LogoutEverywhere,
            )
            .icon(Icon::LogOut)
            .into(),
            MenuItem::new(t!("status_menu.remove_server"), Message::RemoveServer)
                .icon(Icon::Trash2)
                .danger(true)
                .into(),
        ];
        popover_menu(
            trigger,
            &entries,
            self.open,
            Message::Toggle(false),
            Placement::BelowStart,
        )
    }
}
