//! Teams and discussions UI (docs/teams-videoconf.md §2, §5, §6): the right
//! panel bodies for Team info, Team channels, Team members and Discussions,
//! the leave / delete / convert / remove-member dialogs, and helpers for the
//! header (back to team) and the sidebar. Strings: `locales/teams.yml`
//! (`teams.*`).
//!
//! The room panel (`room/panel.rs`) hosts these bodies through one
//! [`TeamPanel`] variant; everything else stays here.

pub mod channels;
pub mod chooser;
pub mod discussions;
pub mod info;
pub mod members;

use iced::Element;
use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::teams;
use rustket_core::workspace::Changes;
use rustket_ui::widget::BannerKind;

use crate::error::Failure;
use crate::room::panel::Kind;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

/// Page size of the team lists.
pub const PAGE: u64 = 25;

/// User text of a teams error (§2.10), falling back to the panel wording.
pub fn error_text(f: &Failure) -> String {
    let key = match f.error().api_error_code() {
        Some("team-does-not-exist") => "teams.err_team_missing",
        Some("Team not found" | "Room not found" | "Resource not found") => {
            "teams.err_team_not_found"
        }
        Some("team-name-already-exists" | "room-name-already-exists") => "teams.err_name_exists",
        Some("error-team-creation") => "teams.err_creation",
        Some("room-already-on-team") => "teams.err_room_on_team",
        Some("error-no-owner-channel") => "teams.err_not_room_owner",
        Some("invalid-room" | "room-not-on-team" | "room-not-on-that-team") => {
            "teams.err_room_not_in_team"
        }
        Some("user-not-on-private-team" | "error-invalid-team-not-a-member") => {
            "teams.err_not_member_private"
        }
        Some("member-does-not-exist") => "teams.err_not_member",
        Some("last-owner-can-not-be-removed" | "error-you-are-last-owner") => {
            "teams.err_last_owner"
        }
        Some("error-cannot-delete-team-channel") => "teams.err_delete_team_instead",
        Some("error-room-is-abac-managed") => "teams.err_abac",
        Some("error-not-allowed") => "teams.err_not_allowed_rooms",
        Some("unauthorized" | "error-no-permission-team-channel" | "Not allowed") => {
            "teams.err_unauthorized"
        }
        _ => return crate::room::panel::common::failure_text(f),
    };
    t!(key).into_owned()
}

pub fn error_toast(title: String, f: &Failure) -> Effect {
    Effect::Toast(BannerKind::Error, title, error_text(f))
}

pub fn ok_toast(title: String) -> Effect {
    Effect::Toast(BannerKind::Success, title, String::new())
}

/// The team main room `rid` belongs to (itself for a main room).
pub fn main_room(ws: &Workspace, rid: &str) -> Option<String> {
    if ws.room(rid).is_some_and(|e| e.is_team()) {
        return Some(rid.to_owned());
    }
    teams::team_id_of(ws, rid).and_then(|t| teams::main_room_of(ws, t))
}

/// "Back to {team} team" target of a child room: the main room, when the
/// workspace has it (the user is a team member or the team was opened).
pub fn back_to_team(ws: &Workspace, rid: &str) -> Option<(String, String)> {
    if !teams::is_child_room(ws, rid) {
        return None;
    }
    let main = main_room(ws, rid)?;
    let e = ws.room(&main)?;
    let name = rustket_core::room_list::display_name(ws, e, ws.use_real_names(None));
    Some((main, name))
}

/// Header menu entries of `rid`: Team info in a team main room, "Convert to
/// team" in a plain channel or group the user may convert.
pub fn header_menu(ws: &Workspace, rid: &str) -> Vec<(rustket_ui::icon::Icon, &'static str, Kind)> {
    use rustket_ui::icon::Icon;
    let Some(e) = ws.room(rid) else {
        return Vec::new();
    };
    if e.is_team() {
        return vec![(Icon::Info, "teams.panel_info", Kind::TeamInfo)];
    }
    if teams::can_convert_to_team(ws, rid) {
        return vec![(Icon::Users, "teams.convert_to_team", Kind::TeamInfo)];
    }
    Vec::new()
}

/// "Back to {team} team" link for the header subtitle.
pub fn back_link<'a, M: Clone + 'a>(team: String, on: M) -> Element<'a, M> {
    use iced::widget::row;
    use rustket_ui::icon::{self, Icon};
    row![
        icon::sized(Icon::ArrowLeft, 12.0).style(rustket_ui::style::text_secondary),
        crate::room::panel::common::link(t!("teams.back_to_team", team = team).into_owned(), on),
    ]
    .spacing(rustket_ui::theme::SPACE_1)
    .align_y(iced::Alignment::Center)
    .into()
}

/// `{key}_one|few|many` with `count` in the current locale.
pub fn count_text(key: &str, n: u64) -> String {
    let form = crate::room::render::plural(n);
    t!(format!("{key}_{form}"), count = n).into_owned()
}

/// [`back_link`] taking only its own width, followed by the room topic.
pub fn back_link_with<'a, M: Clone + 'a>(
    team: String,
    on: M,
    topic: Element<'a, M>,
) -> Element<'a, M> {
    use iced::widget::{mouse_area, row, text};
    use rustket_ui::icon::{self, Icon};
    use rustket_ui::theme::{TEXT_CAPTION, tokens};
    let label = rustket_ui::widget::ellipsis(t!("teams.back_to_team", team = team).into_owned())
        .width(iced::Length::Shrink)
        .size(TEXT_CAPTION)
        .style(|theme: &iced::Theme| Some(tokens(theme).link));
    row![
        icon::sized(Icon::ArrowLeft, 12.0).style(rustket_ui::style::text_secondary),
        mouse_area(label)
            .interaction(iced::mouse::Interaction::Pointer)
            .on_press(on),
        text("·")
            .size(TEXT_CAPTION)
            .style(rustket_ui::style::text_secondary),
        topic,
    ]
    .spacing(rustket_ui::theme::SPACE_1)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Sidebar second line of a team child room without a last message: "in
/// {team}" (the main room's name).
pub fn sidebar_caption(ws: &Workspace, rid: &str) -> Option<String> {
    let (_, team) = back_to_team(ws, rid)?;
    Some(t!("teams.in_team", team = team).into_owned())
}

/// Endpoint prefix of a room type (`channels` / `groups`).
pub fn prefix(t: RoomType) -> &'static str {
    if t == RoomType::Private {
        "groups"
    } else {
        "channels"
    }
}

/// The body of a team panel kind.
pub enum TeamPanel {
    Info(Box<info::Info>),
    Channels(Box<channels::Channels>),
    Members(Box<members::Members>),
    Discussions(Box<discussions::Discussions>),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Info(info::Msg),
    Channels(channels::Msg),
    Members(members::Msg),
    Discussions(discussions::Msg),
}

fn map<M: Send + 'static>(a: Action<M>, f: fn(M) -> Msg) -> Action<Msg> {
    a.map(f)
}

impl TeamPanel {
    /// The body for `kind`, if this module owns it.
    pub fn new(kind: &Kind) -> Option<Self> {
        Some(match kind {
            Kind::TeamInfo => Self::Info(Box::default()),
            Kind::TeamChannels => Self::Channels(Box::default()),
            Kind::TeamMembers => Self::Members(Box::default()),
            Kind::Discussions => Self::Discussions(Box::default()),
            _ => return None,
        })
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match self {
            Self::Info(b) => map(b.activate(ctx), Msg::Info),
            Self::Channels(b) => map(b.activate(ctx), Msg::Channels),
            Self::Members(b) => map(b.activate(ctx), Msg::Members),
            Self::Discussions(b) => map(b.activate(ctx), Msg::Discussions),
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match (self, msg) {
            (Self::Info(b), Msg::Info(m)) => map(b.update(m, ctx), Msg::Info),
            (Self::Channels(b), Msg::Channels(m)) => map(b.update(m, ctx), Msg::Channels),
            (Self::Members(b), Msg::Members(m)) => map(b.update(m, ctx), Msg::Members),
            (Self::Discussions(b), Msg::Discussions(m)) => map(b.update(m, ctx), Msg::Discussions),
            _ => Action::none(),
        }
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match self {
            Self::Info(b) => map(b.on_changes(changes, ctx), Msg::Info),
            Self::Channels(b) => map(b.on_changes(changes, ctx), Msg::Channels),
            Self::Members(b) => map(b.on_changes(changes, ctx), Msg::Members),
            Self::Discussions(b) => map(b.on_changes(changes, ctx), Msg::Discussions),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        match self {
            Self::Info(b) => b.view(ctx).map(Msg::Info),
            Self::Channels(b) => b.view(ctx).map(Msg::Channels),
            Self::Members(b) => b.view(ctx).map(Msg::Members),
            Self::Discussions(b) => b.view(ctx).map(Msg::Discussions),
        }
    }

    /// Dialog shown over the room, with the message that dismisses it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        match self {
            Self::Info(b) => b
                .overlay(ctx)
                .map(|(e, d)| (e.map(Msg::Info), Msg::Info(d))),
            Self::Members(b) => b
                .overlay(ctx)
                .map(|(e, d)| (e.map(Msg::Members), Msg::Members(d))),
            Self::Channels(b) => b
                .overlay(ctx)
                .map(|(e, d)| (e.map(Msg::Channels), Msg::Channels(d))),
            Self::Discussions(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fail(body: &str, status: u16) -> Failure {
        Failure::from(rocketchat::Error::from_response(
            status,
            None,
            body.as_bytes(),
        ))
    }

    #[test]
    fn error_texts() {
        let f = fail(
            r#"{"success":false,"error":"last-owner-can-not-be-removed"}"#,
            400,
        );
        assert_eq!(error_text(&f), t!("teams.err_last_owner"));
        let f = fail(r#"{"success":false,"error":"unauthorized"}"#, 403);
        assert_eq!(error_text(&f), t!("teams.err_unauthorized"));
    }
}
