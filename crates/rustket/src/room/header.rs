//! Room header: avatar, name, topic / presence, action buttons that open the
//! right panel. STUB: shows the room name and an info button; to be completed.
//! Strings: `locales/room_header.yml` (`room_header.*`).

use iced::{Element, Subscription};
use rust_i18n::t;
use rustket_core::room_list;
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::widget::{AvatarShape, avatar, header_bar, icon_button_with};

use super::{Action, Effect, UpdateCtx, ViewCtx, panel};

#[derive(Debug, Clone)]
pub enum Message {
    TogglePanel(panel::Kind),
}

pub struct Header {
    rid: String,
}

impl Header {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
        }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = ctx;
        match msg {
            Message::TogglePanel(kind) => Action::effect(Effect::OpenPanel(kind)),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        let _ = (changes, ctx);
        Action::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let (name, avatar_src) = match ws.room(&self.rid) {
            Some(entry) => {
                let use_real = ws.use_real_names(ctx.prefs.use_real_names);
                let name = room_list::display_name(ws, entry, use_real);
                let av = match entry.room_type() {
                    rocketchat::models::RoomType::Direct => room_list::Avatar::User {
                        username: entry.name().to_owned(),
                    },
                    _ => room_list::Avatar::Room {
                        rid: self.rid.clone(),
                        etag: entry.room.as_ref().and_then(|r| r.avatar_etag.clone()),
                    },
                };
                let src = ctx.images.avatar(ctx.server_id, ctx.server, &av, &name);
                (name, src)
            }
            None => {
                let name = t!("room_header.unknown_room").into_owned();
                let src = rustket_ui::widget::AvatarSource::initials(&name, &self.rid);
                (name, src)
            }
        };
        let topic = ws
            .room(&self.rid)
            .and_then(|e| e.room.as_ref())
            .and_then(|r| r.topic.clone())
            .filter(|t| !t.trim().is_empty());
        let info_active = matches!(ctx.panel, Some(panel::Kind::RoomInfo));
        header_bar(
            Some(avatar(avatar_src, 32.0, AvatarShape::Rounded).into()),
            name,
            topic,
            vec![icon_button_with(
                Icon::Info,
                Some(Message::TogglePanel(panel::Kind::RoomInfo)),
                t!("room_header.room_info"),
                info_active,
                32.0,
            )],
        )
    }
}
