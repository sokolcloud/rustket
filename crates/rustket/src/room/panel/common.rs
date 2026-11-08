//! Pieces shared by the header and the panels: room actions (favorite, hide,
//! leave, delete, mark read), error texts and small view helpers.

use chrono::Local;
use iced::widget::{button, column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Theme};
use rocketchat::models::{RoomType, UserStatus};
use rocketchat::{RcDate, RestClient};
use rust_i18n::t;
use rustket_core::permissions;
use rustket_core::room_list;
use rustket_core::workspace::Workspace;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
    TEXT_SMALL, tokens,
};
use rustket_ui::widget::{
    AvatarShape, BannerKind, Presence, avatar, ellipsis, empty_state, loading_placeholder,
    secondary_button,
};

use crate::error::Failure;
use crate::room::{Effect, ViewCtx};

/// Room-level actions offered by the header menu and the info panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomAction {
    Favorite(bool),
    MarkRead,
    Hide,
    Leave,
    Delete,
}

impl RoomAction {
    /// Actions that ask for confirmation first.
    pub fn needs_confirm(self) -> bool {
        matches!(self, Self::Hide | Self::Leave | Self::Delete)
    }

    pub fn confirm_text(self, room: &str) -> String {
        match self {
            Self::Hide => t!("room_panel.confirm_hide", room = room).into_owned(),
            Self::Leave => t!("room_panel.confirm_leave", room = room).into_owned(),
            Self::Delete => t!("room_panel.confirm_delete", room = room).into_owned(),
            _ => String::new(),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Favorite(true) => t!("room_panel.favorite"),
            Self::Favorite(false) => t!("room_panel.unfavorite"),
            Self::MarkRead => t!("room_panel.mark_read"),
            Self::Hide => t!("room_panel.hide"),
            Self::Leave => t!("room_panel.leave"),
            Self::Delete => t!("room_panel.delete"),
        }
        .into_owned()
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Favorite(true) => Icon::Star,
            Self::Favorite(false) => Icon::StarOff,
            Self::MarkRead => Icon::CheckCheck,
            Self::Hide => Icon::EyeOff,
            Self::Leave => Icon::LogOut,
            Self::Delete => Icon::Trash2,
        }
    }

    pub fn danger(self) -> bool {
        matches!(self, Self::Leave | Self::Delete)
    }

    pub async fn run(self, rest: RestClient, rid: String) -> rocketchat::Result<()> {
        match self {
            Self::Favorite(f) => rest.rooms_favorite(&rid, f).await,
            Self::MarkRead => rest.subscriptions_read(&rid, true).await,
            Self::Hide => rest.rooms_hide(&rid).await,
            Self::Leave => rest.rooms_leave(&rid).await,
            Self::Delete => rest.rooms_delete(&rid).await,
        }
    }

    /// Toast after success (`None`: nothing to say).
    pub fn done_text(self) -> Option<String> {
        match self {
            Self::Favorite(_) | Self::MarkRead => None,
            Self::Hide => Some(t!("room_panel.hidden_ok").into_owned()),
            Self::Leave => Some(t!("room_panel.left_ok").into_owned()),
            Self::Delete => Some(t!("room_panel.deleted_ok").into_owned()),
        }
    }
}

impl RoomAction {
    /// Effects after the action succeeded: a toast, and leaving a hidden room
    /// (it is gone from the sidebar; the web navigates home).
    pub fn done_action<M>(self) -> crate::room::Action<M> {
        let mut out = match self.done_text() {
            Some(s) => crate::room::Action::effect(ok_toast(s)),
            None => crate::room::Action::none(),
        };
        if self == Self::Hide {
            out = out.with(Effect::CloseRoom);
        }
        out
    }
}

/// Whether `action` is offered for `rid` (the web client's rules).
pub fn room_action_allowed(ws: &Workspace, rid: &str, action: RoomAction) -> bool {
    let Some(entry) = ws.room(rid) else {
        return false;
    };
    let joined = entry.sub.is_some();
    match action {
        RoomAction::Favorite(_) => joined && ws.settings().bool_or("Favorite_Rooms", true),
        RoomAction::Hide => joined,
        RoomAction::MarkRead => entry.sub.as_ref().is_some_and(|s| s.unread > 0 || s.alert),
        RoomAction::Leave => {
            let perm = match entry.room_type() {
                RoomType::Channel => "leave-c",
                RoomType::Private => "leave-p",
                _ => return false,
            };
            joined && permissions::has_permission(ws, perm, Some(rid))
        }
        RoomAction::Delete => permissions::can_delete_room(ws, rid),
    }
}

pub fn is_favorite(ws: &Workspace, rid: &str) -> bool {
    ws.subscription(rid).is_some_and(|s| s.f)
}

/// User-facing text of a failed call, with friendlier wording for known codes.
pub fn failure_text(f: &Failure) -> String {
    match f.error().api_error_code() {
        Some("error-you-are-last-owner") => t!("room_panel.err_last_owner").into_owned(),
        Some("error-remove-last-owner") => t!("room_panel.err_remove_last_owner").into_owned(),
        Some("error-not-authorized")
        | Some("not_authorized")
        | Some("error-not-allowed")
        | Some("error-action-not-allowed") => t!("room_panel.err_not_allowed").into_owned(),
        Some("error-invalid-room-name") => t!("room_panel.err_invalid_name").into_owned(),
        Some("error-duplicate-channel-name") | Some("error-duplicate-handle") => {
            t!("room_panel.err_duplicate_name").into_owned()
        }
        Some("error-archived-duplicate-name") => {
            t!("room_panel.err_archived_duplicate_name").into_owned()
        }
        _ => f.user_text(),
    }
}

/// Errors that belong to the room name field.
pub fn is_name_error(f: &Failure) -> bool {
    matches!(
        f.error().api_error_code(),
        Some(
            "error-invalid-room-name"
                | "error-duplicate-channel-name"
                | "error-duplicate-handle"
                | "error-archived-duplicate-name"
        )
    )
}

pub fn error_toast(title: &str, f: &Failure) -> Effect {
    Effect::Toast(BannerKind::Error, title.to_owned(), failure_text(f))
}

pub fn ok_toast(body: String) -> Effect {
    Effect::Toast(BannerKind::Success, body, String::new())
}

/// Room id of a `#name` mention, if the room is known.
pub fn rid_by_name(ws: &Workspace, name: &str) -> Option<String> {
    ws.rooms()
        .values()
        .find(|e| e.name() == name || e.fname() == Some(name))
        .map(|e| e.rid().to_owned())
}

pub fn presence_of(status: UserStatus) -> Presence {
    match status {
        UserStatus::Online => Presence::Online,
        UserStatus::Away => Presence::Away,
        UserStatus::Busy => Presence::Busy,
        UserStatus::Offline => Presence::Offline,
        UserStatus::Unknown => Presence::Unknown,
    }
}

pub fn status_label(status: UserStatus) -> String {
    match status {
        UserStatus::Online => t!("room_panel.status_online"),
        UserStatus::Away => t!("room_panel.status_away"),
        UserStatus::Busy => t!("room_panel.status_busy"),
        UserStatus::Offline | UserStatus::Unknown => t!("room_panel.status_offline"),
    }
    .into_owned()
}

pub fn role_label(role: &str) -> String {
    match role {
        "owner" => t!("room_panel.role_owner").into_owned(),
        "moderator" => t!("room_panel.role_moderator").into_owned(),
        "leader" => t!("room_panel.role_leader").into_owned(),
        "admin" => t!("room_panel.role_admin").into_owned(),
        "bot" => t!("room_panel.role_bot").into_owned(),
        "user" => t!("room_panel.role_user").into_owned(),
        other => other.to_owned(),
    }
}

pub fn time_label(ts: Option<RcDate>, ctx: &ViewCtx<'_>) -> String {
    ts.map(|ts| crate::format::short_time(ts, Local::now(), ctx.prefs.clock_24h))
        .unwrap_or_default()
}

/// Full local date and time (`dd.mm.yyyy 14:05`), e.g. for future dates.
pub fn date_time_label(ts: RcDate, ctx: &ViewCtx<'_>) -> String {
    use chrono::TimeZone;
    let Some(local) = Local.timestamp_millis_opt(ts.timestamp_millis()).single() else {
        return String::new();
    };
    if ctx.prefs.clock_24h {
        local.format("%d.%m.%Y %H:%M").to_string()
    } else {
        local.format("%d.%m.%Y %-I:%M %p").to_string()
    }
}

/// The display name of a user for lists (real name when the server or the
/// user prefers it).
pub fn user_display(ws: &Workspace, use_real: bool, username: &str, name: Option<&str>) -> String {
    let _ = ws;
    match name.filter(|n| use_real && !n.trim().is_empty()) {
        Some(n) => n.to_owned(),
        None => username.to_owned(),
    }
}

pub fn use_real_names(ctx: &ViewCtx<'_>) -> bool {
    ctx.workspace.use_real_names(ctx.prefs.use_real_names)
}

pub fn user_avatar<'a, M: 'a>(
    ctx: &ViewCtx<'a>,
    username: &str,
    name: &str,
    size: f32,
) -> Element<'a, M> {
    let src = ctx.images.avatar(
        ctx.server_id,
        ctx.server,
        &room_list::Avatar::User {
            username: username.to_owned(),
        },
        name,
    );
    avatar(src, size, AvatarShape::Rounded).into()
}

pub fn room_avatar<'a, M: 'a>(
    ctx: &ViewCtx<'a>,
    rid: &str,
    etag: Option<String>,
    name: &str,
    size: f32,
) -> Element<'a, M> {
    let src = ctx.images.avatar(
        ctx.server_id,
        ctx.server,
        &room_list::Avatar::Room {
            rid: rid.to_owned(),
            etag,
        },
        name,
    );
    avatar(src, size, AvatarShape::Rounded).into()
}

// ---- view helpers --------------------------------------------------------------

/// Loading / error / empty states of a list.
pub fn loading<'a, M: 'a>() -> Element<'a, M> {
    container(loading_placeholder(None))
        .width(Length::Fill)
        .padding(SPACE_4)
        .into()
}

pub fn error_state<'a, M: Clone + 'a>(message: &str, retry: M) -> Element<'a, M> {
    container(
        column![
            icon::sized(Icon::TriangleAlert, 28.0).style(style::text_danger),
            text(message.to_owned())
                .size(TEXT_SMALL)
                .style(style::text_secondary)
                .align_x(Alignment::Center),
            secondary_button(t!("room_panel.retry").into_owned(), Some(retry)),
        ]
        .spacing(SPACE_3)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(SPACE_4)
    .center_x(Length::Fill)
    .into()
}

pub fn empty<'a, M: 'a>(icon: Icon, title: String) -> Element<'a, M> {
    container(empty_state(icon, title, String::new()))
        .width(Length::Fill)
        .padding([SPACE_4 * 2.0, SPACE_4])
        .into()
}

/// Search field with a leading icon.
pub fn search_field<'a, M: Clone + 'a>(
    value: &str,
    placeholder: String,
    on_input: impl Fn(String) -> M + 'a,
) -> Element<'a, M> {
    text_input(&placeholder, value)
        .on_input(on_input)
        .size(TEXT_BODY)
        .padding([7, 10])
        .icon(text_input::Icon {
            font: icon::ICON_FONT,
            code_point: char::from(Icon::Search),
            size: Some(14.0.into()),
            spacing: 8.0,
            side: text_input::Side::Left,
        })
        .style(style::text_input)
        .into()
}

/// A row of selectable filter pills.
pub fn filter_pills<'a, M: Clone + 'a, T: Copy + PartialEq + 'a>(
    options: &[(T, String)],
    selected: T,
    on: impl Fn(T) -> M + 'a,
) -> Element<'a, M> {
    let mut r = row![].spacing(SPACE_1).align_y(Alignment::Center);
    for (value, label) in options {
        let active = *value == selected;
        r = r.push(
            button(
                text(label.clone())
                    .size(TEXT_CAPTION)
                    .font(FONT_MEDIUM)
                    .wrapping(text::Wrapping::None),
            )
            .padding([4, 10])
            .style(style::button_list_item(active))
            .on_press(on(*value)),
        );
    }
    scrollable(r)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .width(Length::Fill)
        .into()
}

/// "Load more" footer: spinner while loading, button when there is more.
pub fn load_more<'a, M: Clone + 'a>(loading_more: bool, has_more: bool, msg: M) -> Element<'a, M> {
    if loading_more {
        return container(rustket_ui::widget::spinner(18.0))
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding(SPACE_2)
            .into();
    }
    if !has_more {
        return iced::widget::space().height(SPACE_2).into();
    }
    container(secondary_button(
        t!("room_panel.load_more").into_owned(),
        Some(msg),
    ))
    .width(Length::Fill)
    .center_x(Length::Fill)
    .padding(SPACE_2)
    .into()
}

/// A labelled section in detail panels.
pub fn field<'a, M: 'a>(label: String, content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    column![
        text(label)
            .size(TEXT_CAPTION)
            .font(FONT_SEMIBOLD)
            .style(style::text_hint),
        content.into(),
    ]
    .spacing(SPACE_1)
    .width(Length::Fill)
    .into()
}

/// Plain selectable-looking text block.
pub fn body_text<'a, M: 'a>(s: &str) -> Element<'a, M> {
    text(s.to_owned()).size(TEXT_BODY).into()
}

/// A clickable list row.
pub fn list_row<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on: Option<M>,
) -> Element<'a, M> {
    button(content)
        .width(Length::Fill)
        .padding([SPACE_2, SPACE_4])
        .style(style::button_list_item(false))
        .on_press_maybe(on)
        .into()
}

/// Full-width action entry (icon + label) in detail panels.
pub fn action_entry<'a, M: Clone + 'a>(
    icon_: Icon,
    label: String,
    danger: bool,
    on: Option<M>,
) -> Element<'a, M> {
    button(
        row![icon::sized(icon_, 16.0), ellipsis(label).size(TEXT_BODY)]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([7.0, SPACE_3])
    .style(style::button_menu_item(danger))
    .on_press_maybe(on)
    .into()
}

/// A chip-like boolean badge row (read-only, archived, …).
pub fn badges<'a, M: 'a>(items: Vec<(String, rustket_ui::widget::ChipKind)>) -> Element<'a, M> {
    let mut r = row![].spacing(SPACE_1);
    for (label, kind) in items {
        r = r.push(rustket_ui::widget::chip(label, kind));
    }
    r.wrap().into()
}

/// Inline confirmation card (text + cancel / confirm).
pub fn confirm_card<'a, M: Clone + 'a>(
    message: String,
    confirm_label: String,
    danger: bool,
    busy: bool,
    on_confirm: M,
    on_cancel: M,
) -> Element<'a, M> {
    let confirm = if danger {
        rustket_ui::widget::danger_button(confirm_label, Some(on_confirm))
    } else {
        rustket_ui::widget::primary_button(confirm_label, Some(on_confirm))
    }
    .loading(busy);
    container(
        column![
            text(message).size(TEXT_SMALL),
            row![
                iced::widget::space().width(Length::Fill),
                secondary_button(t!("room_panel.cancel").into_owned(), Some(on_cancel)),
                confirm,
            ]
            .spacing(SPACE_2),
        ]
        .spacing(SPACE_3),
    )
    .padding(SPACE_3)
    .width(Length::Fixed(300.0))
    .style(style::container_popover)
    .into()
}

/// Wraps a clickable text (used for links in the header and panels).
pub fn link<'a, M: Clone + 'a>(label: String, on: M) -> Element<'a, M> {
    mouse_area(
        ellipsis(label)
            .size(TEXT_CAPTION)
            .style(|theme: &Theme| Some(tokens(theme).link)),
    )
    .interaction(iced::mouse::Interaction::Pointer)
    .on_press(on)
    .into()
}
