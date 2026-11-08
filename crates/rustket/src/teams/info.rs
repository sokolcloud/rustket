//! Team info (§5.3): the team's main room with its actions (edit, channels,
//! members, leave, convert to channel, delete). For a plain channel or group
//! the same panel offers "Convert to team" (§2.2).

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{RoomType, Team, TeamType};
use rocketchat::rest::TeamRef;
use rust_i18n::t;
use rustket_core::permissions;
use rustket_core::teams;
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_3, SPACE_4, TEXT_BODY, TEXT_TITLE};
use rustket_ui::widget::{ChipKind, chip, primary_button};

use super::chooser::{self, Chooser, Flow, Outcome};
use crate::error::Res;
use crate::room::panel::{Kind, common};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Team>),
    Open(Kind),
    Start(Flow),
    Chooser(chooser::Msg),
    ConvertToTeam,
    Converted(Res<Team>),
}

#[derive(Default)]
pub struct Info {
    team: Option<Result<Team, String>>,
    chooser: Option<Chooser>,
    converting: bool,
}

impl Info {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.load(ctx)
    }

    fn load(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let Some(team_id) = teams::team_id_of(ctx.workspace, ctx.rid)
            .filter(|_| ctx.workspace.room(ctx.rid).is_some_and(|e| e.is_team()))
            .map(str::to_owned)
        else {
            self.team = None;
            return Action::none();
        };
        Action::task(ctx.rest_task(
            move |rest| async move { rest.teams_info(TeamRef::Id(&team_id)).await },
            Msg::Loaded,
        ))
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        // Converted to / from a team: reload.
        let is_team = ctx.workspace.room(ctx.rid).is_some_and(|e| e.is_team());
        if changes.rooms.contains(ctx.rid) && is_team != self.team.is_some() {
            return self.load(ctx);
        }
        Action::none()
    }

    fn team_name(&self, ctx: &UpdateCtx<'_>) -> String {
        ctx.workspace
            .room(ctx.rid)
            .map(|e| {
                rustket_core::room_list::display_name(
                    ctx.workspace,
                    e,
                    ctx.workspace.use_real_names(ctx.prefs.use_real_names),
                )
            })
            .unwrap_or_default()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Loaded(r) => {
                self.team = Some(r.map_err(|e| super::error_text(&e)));
                Action::none()
            }
            Msg::Open(k) => Action::effect(Effect::OpenPanel(k)),
            Msg::Start(flow) => {
                let Some(team_id) = teams::team_id_of(ctx.workspace, ctx.rid).map(str::to_owned)
                else {
                    return Action::none();
                };
                let c = Chooser::new(flow, &team_id, ctx.rid, &self.team_name(ctx));
                let task = c.load(ctx).map(Msg::Chooser);
                self.chooser = Some(c);
                Action::task(task)
            }
            Msg::Chooser(m) => {
                let Some(c) = self.chooser.as_mut() else {
                    return Action::none();
                };
                let (task, outcome) = c.update(m, ctx);
                let flow = c.flow.clone();
                match outcome {
                    Outcome::None => Action::task(task.map(Msg::Chooser)),
                    Outcome::Cancelled => {
                        self.chooser = None;
                        Action::none()
                    }
                    Outcome::Finished => {
                        self.chooser = None;
                        let key = match flow {
                            Flow::Leave => "teams.left_team",
                            Flow::Delete => "teams.team_deleted",
                            Flow::Convert => "teams.converted_to_channel",
                            Flow::RemoveMember { .. } => "teams.member_removed",
                        };
                        let a = Action::effect(super::ok_toast(t!(key).into_owned()));
                        match flow {
                            Flow::Leave | Flow::Delete => a.with(Effect::CloseRoom),
                            _ => a.with(Effect::ClosePanel),
                        }
                    }
                }
            }
            Msg::ConvertToTeam => {
                if self.converting {
                    return Action::none();
                }
                self.converting = true;
                let rid = ctx.rid.to_owned();
                let private = ctx
                    .workspace
                    .room(ctx.rid)
                    .is_some_and(|e| e.room_type() == RoomType::Private);
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        if private {
                            rest.groups_convert_to_team(&rid).await
                        } else {
                            rest.channels_convert_to_team(&rid).await
                        }
                    },
                    Msg::Converted,
                ))
            }
            Msg::Converted(r) => {
                self.converting = false;
                match r {
                    Ok(team) => {
                        self.team = Some(Ok(team));
                        Action::effect(super::ok_toast(t!("teams.converted_to_team").into_owned()))
                    }
                    Err(e) => Action::effect(super::error_toast(
                        t!("teams.convert_to_team").into_owned(),
                        &e,
                    )),
                }
            }
        }
    }

    pub fn overlay<'a>(&'a self, _ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        self.chooser.as_ref().map(|c| {
            (
                c.view().map(Msg::Chooser),
                Msg::Chooser(chooser::Msg::Cancel),
            )
        })
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let Some(entry) = ws.room(ctx.rid) else {
            return common::empty(Icon::Users, t!("teams.not_a_team").into_owned());
        };
        if !entry.is_team() {
            return self.convert_view(ctx);
        }
        let name = rustket_core::room_list::display_name(ws, entry, common::use_real_names(ctx));
        let room = entry.room.as_ref();
        let private = entry.room_type() == RoomType::Private
            || matches!(&self.team, Some(Ok(t)) if t.kind == TeamType::Private);
        let mut col = column![
            container(common::room_avatar(
                ctx,
                ctx.rid,
                room.and_then(|r| r.avatar_etag.clone()),
                &name,
                96.0
            ))
            .center_x(Length::Fill),
            container(text(name).size(TEXT_TITLE).font(FONT_SEMIBOLD)).center_x(Length::Fill),
            container(
                row![
                    chip(t!("teams.team").into_owned(), ChipKind::Primary),
                    chip(
                        if private {
                            t!("teams.private")
                        } else {
                            t!("teams.public")
                        }
                        .into_owned(),
                        ChipKind::Neutral
                    ),
                ]
                .spacing(SPACE_1)
            )
            .center_x(Length::Fill),
        ]
        .spacing(SPACE_3);
        if let Some(Err(e)) = &self.team {
            col = col.push(text(e.clone()).size(TEXT_BODY).style(style::text_danger));
        }
        for (key, value) in [
            ("teams.topic", room.and_then(|r| r.topic.clone())),
            (
                "teams.announcement",
                room.and_then(|r| r.announcement.clone()),
            ),
            (
                "teams.description",
                room.and_then(|r| r.description.clone()),
            ),
        ] {
            if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
                col = col.push(common::field(t!(key).into_owned(), common::body_text(&v)));
            }
        }
        if let Some(Ok(team)) = &self.team
            && let Some(by) = team.created_by.as_ref().and_then(|u| u.username.clone())
        {
            col = col.push(common::field(
                t!("teams.created_by").into_owned(),
                common::body_text(&format!("@{by}")),
            ));
        }
        let mut actions = column![].spacing(SPACE_1 / 2.0);
        if permissions::can_edit_room(ws, ctx.rid) {
            actions = actions.push(common::action_entry(
                Icon::Pencil,
                t!("teams.edit").into_owned(),
                false,
                Some(Msg::Open(Kind::RoomSettings)),
            ));
        }
        actions = actions
            .push(common::action_entry(
                Icon::LayoutList,
                t!("teams.channels").into_owned(),
                false,
                Some(Msg::Open(Kind::TeamChannels)),
            ))
            .push(common::action_entry(
                Icon::Users,
                t!("teams.members").into_owned(),
                false,
                Some(Msg::Open(Kind::TeamMembers)),
            ));
        if ws.subscription(ctx.rid).is_some() {
            actions = actions.push(common::action_entry(
                Icon::LogOut,
                t!("teams.leave").into_owned(),
                true,
                Some(Msg::Start(Flow::Leave)),
            ));
        }
        if teams::can_convert_team(ws, ctx.rid) {
            actions = actions.push(common::action_entry(
                Icon::Hash,
                t!("teams.convert").into_owned(),
                false,
                Some(Msg::Start(Flow::Convert)),
            ));
        }
        if teams::can_delete_team(ws, ctx.rid) {
            actions = actions.push(common::action_entry(
                Icon::Trash2,
                t!("teams.delete").into_owned(),
                true,
                Some(Msg::Start(Flow::Delete)),
            ));
        }
        col = col.push(actions);
        scrollable(container(col).padding(SPACE_4))
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }

    fn convert_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        if !teams::can_convert_to_team(ctx.workspace, ctx.rid) {
            return common::empty(Icon::Users, t!("teams.not_a_team").into_owned());
        }
        container(
            column![
                text(t!("teams.convert_to_team").into_owned())
                    .size(TEXT_BODY)
                    .font(FONT_SEMIBOLD),
                text(t!("teams.convert_to_team_hint").into_owned())
                    .size(TEXT_BODY)
                    .style(style::text_secondary),
                primary_button(
                    t!("teams.convert_to_team").into_owned(),
                    Some(Msg::ConvertToTeam)
                )
                .icon(Icon::Users)
                .loading(self.converting),
            ]
            .spacing(SPACE_3)
            .align_x(Alignment::Start),
        )
        .padding(SPACE_4)
        .width(Length::Fill)
        .into()
    }
}
