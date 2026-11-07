//! Room header: avatar, name, topic / presence / parent room, buttons that open
//! the right panel and the room menu (lists, preferences, favorite, hide,
//! leave …).
//! Strings: `locales/room_header.yml` (`room_header.*`).

use iced::widget::{column, container, mouse_area, row};
use iced::{Alignment, Element, Length, Subscription, Theme};
use rocketchat::models::{RoomType, UserStatus};
use rust_i18n::t;
use rustket_core::permissions;
use rustket_core::room_list;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, HEADER_HEIGHT, SPACE_1, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SUBTITLE, tokens,
};
use rustket_ui::widget::{
    ChipKind, MenuEntry, MenuItem, Placement, chip, divider, ellipsis, icon_button_with, popover,
    popover_menu, presence_dot,
};

use super::panel::common::{self, RoomAction};
use super::{Action, Effect, UpdateCtx, ViewCtx, panel};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Message {
    TogglePanel(panel::Kind),
    ToggleMenu,
    CloseMenu,
    /// A menu entry opening a panel.
    MenuPanel(panel::Kind),
    Room(RoomAction),
    Confirm,
    Cancel,
    Done(RoomAction, Res<()>),
    OpenRoom(String),
}

pub struct Header {
    rid: String,
    menu_open: bool,
    confirm: Option<RoomAction>,
    busy: bool,
}

/// What the header shows for a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buttons {
    pub threads: bool,
    pub discussions: bool,
    pub members: bool,
    pub team_channels: bool,
    /// Info opens the user profile in 1:1 DMs.
    pub info: panel::Kind,
}

/// 1:1 direct message partner (username), if `rid` is one.
pub fn dm_partner(ws: &Workspace, rid: &str) -> Option<String> {
    let e = ws.room(rid)?;
    if e.room_type() != RoomType::Direct {
        return None;
    }
    let room = e.room.as_ref();
    let uids = room.and_then(|r| r.uids.as_ref()).map_or(0, Vec::len);
    if uids > 2 {
        return None;
    }
    let me = ws.username().unwrap_or_default();
    room.and_then(|r| r.usernames.as_ref())
        .and_then(|u| u.iter().find(|n| n.as_str() != me).cloned())
        .or_else(|| Some(e.name().to_owned()).filter(|n| !n.is_empty()))
}

pub fn buttons(ws: &Workspace, rid: &str) -> Buttons {
    let dm = dm_partner(ws, rid);
    let entry = ws.room(rid);
    Buttons {
        threads: ws.settings().threads_enabled(),
        discussions: ws.settings().discussions_enabled(),
        members: dm.is_none(),
        team_channels: entry.is_some_and(|e| e.is_team()),
        info: match dm {
            Some(u) => panel::Kind::UserInfo(u),
            None => panel::Kind::Info,
        },
    }
}

/// Menu entries for `rid`.
pub fn menu_entries(ws: &Workspace, rid: &str) -> Vec<MenuEntry<Message>> {
    let mut v: Vec<MenuEntry<Message>> = Vec::new();
    let item = |icon: Icon, key: &str, kind: panel::Kind| -> MenuEntry<Message> {
        MenuItem::new(t!(key).into_owned(), Message::MenuPanel(kind))
            .icon(icon)
            .into()
    };
    v.push(item(Icon::Pin, "room_header.pinned", panel::Kind::Pinned));
    v.push(item(
        Icon::Star,
        "room_header.starred",
        panel::Kind::Starred,
    ));
    v.push(item(
        Icon::AtSign,
        "room_header.mentions",
        panel::Kind::Mentions,
    ));
    v.push(item(
        Icon::Paperclip,
        "room_header.files",
        panel::Kind::Files,
    ));
    v.push(MenuEntry::Separator);
    if ws.subscription(rid).is_some() {
        v.push(item(
            Icon::Bell,
            "room_header.notification_prefs",
            panel::Kind::NotificationPrefs,
        ));
    }
    let direct = ws
        .room(rid)
        .is_some_and(|e| e.room_type() == RoomType::Direct);
    if !direct && permissions::can_edit_room(ws, rid) {
        v.push(item(
            Icon::Settings,
            "room_header.room_settings",
            panel::Kind::RoomSettings,
        ));
    }
    if permissions::can_add_users(ws, rid) || permissions::can_create_invite_links(ws, rid) {
        v.push(item(
            Icon::UserPlus,
            "room_header.invite",
            panel::Kind::Invite,
        ));
    }
    let fav = common::is_favorite(ws, rid);
    let actions = [
        RoomAction::Favorite(!fav),
        RoomAction::MarkRead,
        RoomAction::Hide,
        RoomAction::Leave,
    ];
    let allowed: Vec<RoomAction> = actions
        .into_iter()
        .filter(|a| common::room_action_allowed(ws, rid, *a))
        .collect();
    if !allowed.is_empty() {
        v.push(MenuEntry::Separator);
    }
    for a in allowed {
        v.push(
            MenuItem::new(a.label(), Message::Room(a))
                .icon(a.icon())
                .danger(a.danger())
                .into(),
        );
    }
    v
}

impl Header {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            menu_open: false,
            confirm: None,
            busy: false,
        }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message> {
        match msg {
            Message::TogglePanel(kind) => Action::effect(Effect::OpenPanel(kind)),
            Message::ToggleMenu => {
                self.menu_open = !self.menu_open;
                self.confirm = None;
                Action::none()
            }
            Message::CloseMenu => {
                self.menu_open = false;
                Action::none()
            }
            Message::MenuPanel(kind) => {
                self.menu_open = false;
                Action::effect(Effect::OpenPanel(kind))
            }
            Message::Room(RoomAction::MarkRead) => {
                self.menu_open = false;
                Action::effect(Effect::MarkRead)
            }
            Message::Room(a) => {
                self.menu_open = false;
                if a.needs_confirm() {
                    self.confirm = Some(a);
                    Action::none()
                } else {
                    self.run(a, ctx)
                }
            }
            Message::Confirm => match self.confirm {
                Some(a) => self.run(a, ctx),
                None => Action::none(),
            },
            Message::Cancel => {
                if !self.busy {
                    self.confirm = None;
                }
                Action::none()
            }
            Message::Done(a, r) => {
                self.busy = false;
                self.confirm = None;
                match r {
                    Ok(()) => match a.done_text() {
                        Some(s) => Action::effect(common::ok_toast(s)),
                        None => Action::none(),
                    },
                    Err(e) => Action::effect(common::error_toast(&a.label(), &e)),
                }
            }
            Message::OpenRoom(rid) => Action::effect(Effect::OpenRoom(rid)),
        }
    }

    fn run(&mut self, a: RoomAction, ctx: &UpdateCtx<'_>) -> Action<Message> {
        if self.busy {
            return Action::none();
        }
        self.busy = true;
        let rid = self.rid.clone();
        Action::task(ctx.rest_task(move |rest| a.run(rest, rid), move |r| Message::Done(a, r)))
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
        let entry = ws.room(&self.rid);
        let use_real = ws.use_real_names(ctx.prefs.use_real_names);
        let dm = dm_partner(ws, &self.rid);
        let name = match entry {
            Some(e) => room_list::display_name(ws, e, use_real),
            None => t!("room_header.unknown_room").into_owned(),
        };
        let avatar: Element<'a, Message> = match (&dm, entry) {
            (Some(u), _) => common::user_avatar(ctx, u, &name, 32.0),
            (None, Some(e)) if e.room_type() == RoomType::Direct => {
                common::user_avatar(ctx, e.name(), &name, 32.0)
            }
            (None, Some(e)) => common::room_avatar(
                ctx,
                &self.rid,
                e.room.as_ref().and_then(|r| r.avatar_etag.clone()),
                &name,
                32.0,
            ),
            (None, None) => rustket_ui::widget::avatar(
                rustket_ui::widget::AvatarSource::initials(&name, &self.rid),
                32.0,
                rustket_ui::widget::AvatarShape::Rounded,
            )
            .into(),
        };

        // Title row: type glyph, name, team badge.
        let glyph = entry.and_then(|e| {
            if e.is_discussion() {
                Some(Icon::MessagesSquare)
            } else if e.is_team() {
                Some(Icon::Users)
            } else {
                match e.room_type() {
                    RoomType::Private => Some(Icon::Lock),
                    RoomType::Channel => Some(Icon::Hash),
                    _ => None,
                }
            }
        });
        let mut title = row![].spacing(SPACE_1).align_y(Alignment::Center);
        if let Some(g) = glyph {
            title = title.push(icon::sized(g, 15.0).style(style::text_secondary));
        }
        title = title.push(
            ellipsis(name.clone())
                .size(TEXT_SUBTITLE)
                .font(FONT_SEMIBOLD),
        );
        if entry.is_some_and(|e| e.is_team()) {
            title = title.push(chip(t!("room_header.team").into_owned(), ChipKind::Primary));
        }
        if entry.is_some_and(|e| e.is_archived()) {
            title = title.push(chip(
                t!("room_header.archived").into_owned(),
                ChipKind::Neutral,
            ));
        }

        // Subtitle: presence (DM), parent room (discussion) or topic.
        let subtitle: Option<Element<'a, Message>> = if let Some(u) = &dm {
            let p = ws.presence().get_by_username(u);
            let status = p.map(|p| p.status).unwrap_or(UserStatus::Offline);
            let text_ = p
                .and_then(|p| p.status_text.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| common::status_label(status));
            Some(
                row![
                    presence_dot(common::presence_of(status), 8.0),
                    ellipsis(text_)
                        .size(TEXT_CAPTION)
                        .style(|t: &Theme| Some(tokens(t).text_secondary)),
                ]
                .spacing(SPACE_1 + 2.0)
                .align_y(Alignment::Center)
                .into(),
            )
        } else if let Some(prid) = entry.and_then(|e| {
            e.room
                .as_ref()
                .and_then(|r| r.prid.clone())
                .or_else(|| e.sub.as_ref().and_then(|s| s.prid.clone()))
        }) {
            let parent = ws
                .room(&prid)
                .map(|p| room_list::display_name(ws, p, use_real))
                .unwrap_or_else(|| t!("room_header.parent_room").into_owned());
            Some(
                row![
                    icon::sized(Icon::CornerDownRight, 12.0).style(style::text_secondary),
                    common::link(parent, Message::OpenRoom(prid)),
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center)
                .into(),
            )
        } else {
            entry
                .and_then(|e| e.room.as_ref())
                .and_then(|r| {
                    r.topic
                        .clone()
                        .filter(|t| !t.trim().is_empty())
                        .or_else(|| r.announcement.clone().filter(|a| !a.trim().is_empty()))
                })
                .map(|topic| {
                    let one_line = topic.lines().next().unwrap_or_default().to_owned();
                    mouse_area(
                        ellipsis(one_line)
                            .size(TEXT_CAPTION)
                            .style(|t: &Theme| Some(tokens(t).text_secondary)),
                    )
                    .interaction(iced::mouse::Interaction::Pointer)
                    .on_press(Message::TogglePanel(panel::Kind::Info))
                    .into()
                })
        };
        let mut titles = column![title].spacing(0).width(Length::Fill);
        if let Some(s) = subtitle {
            titles = titles.push(s);
        }

        // Buttons.
        let b = buttons(ws, &self.rid);
        let btn = |icon_: Icon, key: &str, kind: panel::Kind| -> Element<'a, Message> {
            let active = ctx.panel == Some(&kind);
            icon_button_with(
                icon_,
                Some(Message::TogglePanel(kind)),
                t!(key).into_owned(),
                active,
                32.0,
            )
        };
        let mut actions = row![].spacing(SPACE_1).align_y(Alignment::Center);
        actions = actions.push(icon_button_with(
            Icon::Video,
            None,
            t!("room_header.call_soon").into_owned(),
            false,
            32.0,
        ));
        if b.threads {
            actions = actions.push(btn(
                Icon::MessageCircle,
                "room_header.threads",
                panel::Kind::Threads,
            ));
        }
        let info_key = if matches!(b.info, panel::Kind::UserInfo(_)) {
            "room_header.user_info"
        } else {
            "room_header.room_info"
        };
        actions = actions.push(btn(Icon::Info, info_key, b.info.clone()));
        if b.team_channels {
            actions = actions.push(btn(
                Icon::LayoutList,
                "room_header.team_channels",
                panel::Kind::TeamChannels,
            ));
        }
        if b.discussions {
            actions = actions.push(btn(
                Icon::MessagesSquare,
                "room_header.discussions",
                panel::Kind::Discussions,
            ));
        }
        if b.members {
            actions = actions.push(btn(
                Icon::Users,
                "room_header.members",
                panel::Kind::Members,
            ));
        }
        actions = actions.push(btn(Icon::Search, "room_header.search", panel::Kind::Search));

        let trigger = icon_button_with(
            Icon::EllipsisVertical,
            Some(Message::ToggleMenu),
            t!("room_header.more").into_owned(),
            self.menu_open,
            32.0,
        );
        let kebab: Element<'a, Message> = match self.confirm {
            Some(a) => popover(
                trigger,
                common::confirm_card(
                    a.confirm_text(&name),
                    a.label(),
                    a.danger(),
                    self.busy,
                    Message::Confirm,
                    Message::Cancel,
                ),
                true,
                Placement::BelowEnd,
                Message::Cancel,
            ),
            None => popover_menu(
                trigger,
                &menu_entries(ws, &self.rid),
                self.menu_open,
                Message::CloseMenu,
                Placement::BelowEnd,
            ),
        };
        actions = actions.push(kebab);

        let bar = row![avatar, titles, actions]
            .spacing(SPACE_3)
            .align_y(Alignment::Center);
        column![
            container(bar)
                .height(HEADER_HEIGHT)
                .width(Length::Fill)
                .padding([0.0, SPACE_4])
                .center_y(HEADER_HEIGHT)
                .style(style::container_header),
            divider(),
        ]
        .spacing(0)
        .into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::{Permission, Setting, Subscription};
    use rustket_core::workspace::Change;
    use serde_json::json;

    fn ws(t: &str, extra_room: serde_json::Value) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(
            serde_json::from_value(json!({"_id": "me", "username": "me", "roles": ["user"]}))
                .unwrap(),
        );
        let sub: Subscription = serde_json::from_value(json!({
            "_id": "s", "rid": "r", "name": "bob", "t": t, "u": {"_id": "me"}, "unread": 2, "roles": ["owner"]
        }))
        .unwrap();
        ws.apply_subscription(Change::Inserted, sub);
        let mut room = json!({"_id": "r", "t": t, "name": "bob"});
        if let (Some(o), Some(e)) = (room.as_object_mut(), extra_room.as_object()) {
            o.extend(e.clone());
        }
        ws.apply_room(Change::Inserted, serde_json::from_value(room).unwrap());
        for (id, v) in [("Threads_enabled", true), ("Discussion_enabled", true)] {
            ws.apply_setting(
                Change::Updated,
                Setting {
                    id: id.into(),
                    value: json!(v),
                    ..Default::default()
                },
            );
        }
        for (p, r) in [("leave-c", vec!["user"]), ("edit-room", vec!["owner"])] {
            ws.apply_permission(
                Change::Updated,
                Permission {
                    id: p.into(),
                    roles: r.into_iter().map(String::from).collect(),
                    updated_at: None,
                },
            );
        }
        ws
    }

    #[test]
    fn dm_buttons_open_user_info() {
        let ws = ws(
            "d",
            json!({"uids": ["me", "b"], "usernames": ["me", "bob"]}),
        );
        assert_eq!(dm_partner(&ws, "r").as_deref(), Some("bob"));
        let b = buttons(&ws, "r");
        assert!(!b.members);
        assert_eq!(b.info, panel::Kind::UserInfo("bob".into()));
    }

    #[test]
    fn group_dm_is_not_a_partner() {
        let ws = ws("d", json!({"uids": ["me", "b", "c"]}));
        assert_eq!(dm_partner(&ws, "r"), None);
        assert!(buttons(&ws, "r").members);
    }

    #[test]
    fn channel_menu_respects_permissions() {
        let ws = ws("c", json!({}));
        let labels: Vec<String> = menu_entries(&ws, "r")
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some(i.label),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&t!("room_header.room_settings").into_owned()));
        assert!(labels.contains(&RoomAction::Leave.label()));
        assert!(labels.contains(&RoomAction::MarkRead.label()));
        assert!(labels.contains(&RoomAction::Favorite(true).label()));
        assert!(!labels.contains(&t!("room_header.invite").into_owned()));
        let b = buttons(&ws, "r");
        assert!(b.members && b.threads && b.discussions && !b.team_channels);
    }

    #[test]
    fn dm_menu_has_no_leave_or_settings() {
        let ws = ws("d", json!({"uids": ["me", "b"]}));
        let labels: Vec<String> = menu_entries(&ws, "r")
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some(i.label),
                _ => None,
            })
            .collect();
        assert!(!labels.contains(&RoomAction::Leave.label()));
        assert!(!labels.contains(&t!("room_header.room_settings").into_owned()));
        assert!(labels.contains(&RoomAction::Hide.label()));
    }
}
