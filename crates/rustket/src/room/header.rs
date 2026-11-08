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
    /// Menu: create a discussion in this room.
    CreateDiscussion,
    /// Omnichannel quick action or panel (`crate::omnichannel::room`).
    Omni(crate::omnichannel::room::Request),
}

pub struct Header {
    rid: String,
    /// The open right panel (set by `RoomView` before each update): menu
    /// entries open their panel, or keep it open, instead of toggling it.
    panel: Option<panel::Kind>,
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
    // Omnichannel rooms: the chat info panel; no threads, discussions or members.
    if entry.is_some_and(|e| e.room_type() == RoomType::Livechat) {
        return Buttons {
            threads: false,
            discussions: false,
            members: false,
            team_channels: false,
            info: panel::Kind::Omnichannel(crate::omnichannel::panel::PanelKind::Info),
        };
    }
    Buttons {
        threads: ws.settings().threads_enabled(),
        // Discussions cannot be nested (the web hides it inside one too).
        discussions: ws.settings().discussions_enabled()
            && !entry.is_some_and(|e| e.is_discussion()),
        members: dm.is_none(),
        team_channels: entry.is_some_and(|e| e.is_team()),
        info: match dm {
            Some(u) => panel::Kind::UserInfo(u),
            None => panel::Kind::Info,
        },
    }
}

/// Menu entries for `rid`.
/// `overflow`: header buttons that did not fit, listed first.
pub fn menu_entries(
    ws: &Workspace,
    rid: &str,
    overflow: &[HeaderButton],
) -> Vec<MenuEntry<Message>> {
    let mut v: Vec<MenuEntry<Message>> = Vec::new();
    let item = |icon: Icon, key: &str, kind: panel::Kind| -> MenuEntry<Message> {
        MenuItem::new(t!(key).into_owned(), Message::MenuPanel(kind))
            .icon(icon)
            .into()
    };
    for b in overflow {
        v.push(item(b.icon, b.key, b.kind.clone()));
    }
    if !overflow.is_empty() {
        v.push(MenuEntry::Separator);
    }
    let room_type = ws.room(rid).map(|e| e.room_type());
    let direct = room_type == Some(RoomType::Direct);
    if ws.settings().allow_pinning() {
        v.push(item(Icon::Pin, "room_header.pinned", panel::Kind::Pinned));
    }
    v.push(item(
        Icon::Star,
        "room_header.starred",
        panel::Kind::Starred,
    ));
    // The web offers mentions in channels, groups and teams only.
    if !direct {
        v.push(item(
            Icon::AtSign,
            "room_header.mentions",
            panel::Kind::Mentions,
        ));
    }
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
    if !direct && permissions::can_edit_room(ws, rid) {
        v.push(item(
            Icon::Settings,
            "room_header.room_settings",
            panel::Kind::RoomSettings,
        ));
    }
    // Direct messages do not accept invites (`error-room-type-not-allowed`).
    let invitable = matches!(room_type, Some(RoomType::Channel | RoomType::Private));
    if invitable
        && (permissions::can_add_users(ws, rid) || permissions::can_create_invite_links(ws, rid))
    {
        v.push(item(
            Icon::UserPlus,
            "room_header.invite",
            panel::Kind::Invite,
        ));
    }
    if permissions::can_create_discussion(ws, Some(rid)) && ws.subscription(rid).is_some() {
        v.push(
            MenuItem::new(
                t!("room_header.create_discussion").into_owned(),
                Message::CreateDiscussion,
            )
            .icon(Icon::MessagesSquare)
            .into(),
        );
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

/// A header button opening a panel.
#[derive(Debug, Clone)]
pub struct HeaderButton {
    pub icon: Icon,
    pub key: &'static str,
    pub kind: panel::Kind,
}

const BUTTON_SLOT: f32 = 32.0 + SPACE_1;
/// Width kept for the room name before buttons move into the menu.
const MIN_TITLE_WIDTH: f32 = 160.0;

/// Splits the header buttons into the ones shown inline and the ones moved
/// into the menu for a header `width` px wide, plus whether the disabled
/// call placeholder still fits. Team channels, discussions, threads,
/// members and search go into the menu in that order; Info always stays.
pub fn layout_buttons(b: &Buttons, width: f32) -> (Vec<HeaderButton>, Vec<HeaderButton>, bool) {
    let info_key = if matches!(b.info, panel::Kind::UserInfo(_)) {
        "room_header.user_info"
    } else {
        "room_header.room_info"
    };
    // Display order with a drop priority (lower drops first).
    let mut all: Vec<(u8, HeaderButton)> = Vec::new();
    let mut push = |prio: u8, icon: Icon, key: &'static str, kind: panel::Kind| {
        all.push((prio, HeaderButton { icon, key, kind }));
    };
    if b.threads {
        push(
            3,
            Icon::MessageCircle,
            "room_header.threads",
            panel::Kind::Threads,
        );
    }
    push(u8::MAX, Icon::Info, info_key, b.info.clone());
    if b.team_channels {
        push(
            1,
            Icon::LayoutList,
            "room_header.team_channels",
            panel::Kind::TeamChannels,
        );
    }
    if b.discussions {
        push(
            2,
            Icon::MessagesSquare,
            "room_header.discussions",
            panel::Kind::Discussions,
        );
    }
    if b.members {
        push(4, Icon::Users, "room_header.members", panel::Kind::Members);
    }
    push(5, Icon::Search, "room_header.search", panel::Kind::Search);

    let budget = width - 2.0 * SPACE_4 - 32.0 - 2.0 * SPACE_3 - MIN_TITLE_WIDTH;
    // Slots: the buttons, the menu trigger and the call placeholder.
    let fits = |n: usize| (n as f32) * BUTTON_SLOT <= budget;
    let call = fits(all.len() + 2);
    let mut overflow_prios: Vec<u8> = Vec::new();
    if !call {
        let mut prios: Vec<u8> = all
            .iter()
            .map(|(p, _)| *p)
            .filter(|p| *p != u8::MAX)
            .collect();
        prios.sort_unstable();
        for p in prios {
            if fits(all.len() - overflow_prios.len() + 1) {
                break;
            }
            overflow_prios.push(p);
        }
    }
    let (over, inline): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|(p, _)| overflow_prios.contains(p));
    (
        inline.into_iter().map(|(_, b)| b).collect(),
        over.into_iter().map(|(_, b)| b).collect(),
        call,
    )
}

impl Header {
    pub fn new(rid: &str) -> Self {
        Self {
            rid: rid.to_owned(),
            panel: None,
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
                if self.panel.as_ref() == Some(&kind) {
                    return Action::none();
                }
                Action::effect(Effect::OpenPanel(kind))
            }
            Message::CreateDiscussion => {
                self.menu_open = false;
                Action::effect(Effect::StartDiscussion { pmid: None })
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
                    Ok(()) => a.done_action(),
                    Err(e) => Action::effect(common::error_toast(&a.label(), &e)),
                }
            }
            Message::OpenRoom(rid) => Action::effect(Effect::OpenRoom(rid)),
            Message::Omni(r) => {
                self.menu_open = false;
                Action::effect(Effect::Omni(r))
            }
        }
    }

    /// Which right panel is open (see [`Header::update`]).
    pub fn set_panel(&mut self, kind: Option<panel::Kind>) {
        self.panel = kind;
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
        let (server_id, server, rid, workspace, images, prefs, connected, panel_kind, now) = (
            ctx.server_id,
            ctx.server,
            ctx.rid,
            ctx.workspace,
            ctx.images,
            ctx.prefs,
            ctx.connected,
            ctx.panel,
            ctx.now,
        );
        let bar = iced::widget::responsive(move |size| {
            let ctx = ViewCtx {
                server_id,
                server,
                rid,
                workspace,
                images,
                prefs,
                connected,
                panel: panel_kind,
                now,
            };
            self.bar(&ctx, size.width)
        });
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

    /// The header row for a header `width` px wide (incl. padding).
    fn bar<'a>(&'a self, ctx: &ViewCtx<'a>, width: f32) -> Element<'a, Message> {
        let width = width + 2.0 * SPACE_4;
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
            // Only a parent room in the workspace can be opened.
            let parent: Element<'a, Message> = match ws.room(&prid) {
                Some(p) => common::link(
                    room_list::display_name(ws, p, use_real),
                    Message::OpenRoom(prid),
                ),
                None => ellipsis(t!("room_header.parent_room").into_owned())
                    .size(TEXT_CAPTION)
                    .style(|t: &Theme| Some(tokens(t).text_secondary))
                    .into(),
            };
            Some(
                row![
                    icon::sized(Icon::CornerDownRight, 12.0).style(style::text_secondary),
                    parent,
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

        // Buttons; the ones that do not fit go into the menu.
        let (inline, overflow, call) = layout_buttons(&buttons(ws, &self.rid), width);
        let mut actions = row![].spacing(SPACE_1).align_y(Alignment::Center);
        for b in crate::omnichannel::room::header_buttons(ctx) {
            actions = actions.push(b.map(Message::Omni));
        }
        if call {
            actions = actions.push(icon_button_with(
                Icon::Video,
                None,
                t!("room_header.call_soon").into_owned(),
                false,
                32.0,
            ));
        }
        for hb in inline {
            let active = ctx.panel == Some(&hb.kind);
            actions = actions.push(icon_button_with(
                hb.icon,
                Some(Message::TogglePanel(hb.kind)),
                t!(hb.key).into_owned(),
                active,
                32.0,
            ));
        }

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
                &menu_entries(ws, &self.rid, &overflow),
                self.menu_open,
                Message::CloseMenu,
                Placement::BelowEnd,
            ),
        };
        actions = actions.push(kebab);

        row![avatar, titles, actions]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .height(Length::Fill)
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
        let labels: Vec<String> = menu_entries(&ws, "r", &[])
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
        let labels: Vec<String> = menu_entries(&ws, "r", &[])
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

    fn labels(ws: &Workspace) -> Vec<String> {
        menu_entries(ws, "r", &[])
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Item(i) => Some(i.label),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn gating_follows_the_web() {
        // Discussions cannot be nested.
        let disc = ws("p", json!({"prid": "parent"}));
        assert!(!buttons(&disc, "r").discussions);
        // No mentions or invites in DMs, even with invite permissions.
        let mut dm = ws("d", json!({"uids": ["me", "b", "c"]}));
        dm.apply_permission(
            Change::Updated,
            Permission {
                id: "create-invite-links".into(),
                roles: vec!["owner".into(), "user".into()],
                updated_at: None,
            },
        );
        let l = labels(&dm);
        assert!(!l.contains(&t!("room_header.mentions").into_owned()));
        assert!(!l.contains(&t!("room_header.invite").into_owned()));
        assert!(l.contains(&t!("room_header.starred").into_owned()));
        // Pinned and favorite follow their settings.
        let mut c = ws("c", json!({}));
        assert!(labels(&c).contains(&t!("room_header.pinned").into_owned()));
        for id in ["Message_AllowPinning", "Favorite_Rooms"] {
            c.apply_setting(
                Change::Updated,
                Setting {
                    id: id.into(),
                    value: json!(false),
                    ..Default::default()
                },
            );
        }
        let l = labels(&c);
        assert!(!l.contains(&t!("room_header.pinned").into_owned()));
        assert!(!l.contains(&RoomAction::Favorite(true).label()));
    }

    #[test]
    fn menu_entries_open_or_keep_their_panel() {
        let mut w = ws("c", json!({}));
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "r",
            workspace: &mut w,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut h = Header::new("r");
        // Already open: the menu entry keeps it (no toggle effect).
        h.set_panel(Some(panel::Kind::Files));
        let a = h.update(Message::MenuPanel(panel::Kind::Files), &mut ctx);
        assert!(a.effects.is_empty());
        // Another panel open: the entry opens its own.
        h.set_panel(Some(panel::Kind::Members));
        let a = h.update(Message::MenuPanel(panel::Kind::Files), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::OpenPanel(panel::Kind::Files)]
        ));
        // Header buttons still toggle.
        h.set_panel(Some(panel::Kind::Files));
        let a = h.update(Message::TogglePanel(panel::Kind::Files), &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::OpenPanel(panel::Kind::Files)]
        ));
        let a = h.update(Message::CreateDiscussion, &mut ctx);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::StartDiscussion { pmid: None }]
        ));
    }

    #[test]
    fn create_discussion_needs_the_permission() {
        let entry = t!("room_header.create_discussion").into_owned();
        let mut w = ws("c", json!({}));
        assert!(!labels(&w).contains(&entry));
        w.apply_permission(
            Change::Updated,
            Permission {
                id: "start-discussion".into(),
                roles: vec!["user".into()],
                updated_at: None,
            },
        );
        assert!(labels(&w).contains(&entry));
    }

    #[test]
    fn hide_closes_the_room() {
        let a: Action<Message> = RoomAction::Hide.done_action();
        assert!(a.effects.iter().any(|e| matches!(e, Effect::CloseRoom)));
        let a: Action<Message> = RoomAction::Favorite(true).done_action();
        assert!(a.effects.is_empty());
    }

    #[test]
    fn narrow_headers_move_buttons_into_the_menu() {
        let b = Buttons {
            threads: true,
            discussions: true,
            members: true,
            team_channels: true,
            info: panel::Kind::Info,
        };
        let (inline, over, call) = layout_buttons(&b, 1200.0);
        assert_eq!(inline.len(), 6);
        assert!(over.is_empty() && call);
        // Room for the title plus info, two buttons and the menu trigger.
        let w = 2.0 * SPACE_4 + 32.0 + 2.0 * SPACE_3 + MIN_TITLE_WIDTH + 4.0 * BUTTON_SLOT;
        let (inline, over, call) = layout_buttons(&b, w);
        assert!(!call);
        let kinds: Vec<_> = inline.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![panel::Kind::Info, panel::Kind::Members, panel::Kind::Search]
        );
        let kinds: Vec<_> = over.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                panel::Kind::Threads,
                panel::Kind::TeamChannels,
                panel::Kind::Discussions
            ]
        );
        // Info stays even when nothing fits.
        let (inline, _, _) = layout_buttons(&b, 100.0);
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].kind, panel::Kind::Info);
    }
}
