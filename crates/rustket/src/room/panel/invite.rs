//! Add users to the room (`users.autocomplete` → `channels.invite` /
//! `groups.invite`) and the room's invite link (`findOrCreateInvite`).

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::UserSummary;
use rust_i18n::t;
use rustket_core::permissions;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
};
use rustket_ui::widget::{ellipsis, icon_button, primary_button, secondary_button};

use super::api::{self, Invite as InviteLink};
use super::common;
use super::{Kind, debounce};
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Results(u64, Res<Vec<UserSummary>>),
    Pick(UserSummary),
    Unpick(String),
    Add,
    Added(Res<()>),
    CreateLink,
    Link(Res<InviteLink>),
    Copy,
}

#[derive(Default)]
pub struct Invite {
    query: String,
    query_seq: u64,
    results: Vec<UserSummary>,
    searching: bool,
    selected: Vec<UserSummary>,
    adding: bool,
    link: Option<InviteLink>,
    link_busy: bool,
    link_error: Option<String>,
}

impl Invite {
    pub fn activate(&mut self, _ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        Action::none()
    }

    /// Selection after picking `u` (no duplicates).
    fn pick(&mut self, u: UserSummary) {
        if !self.selected.iter().any(|s| s.id == u.id) {
            self.selected.push(u);
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                if self.query.trim().is_empty() {
                    self.results.clear();
                    self.searching = false;
                    return Action::none();
                }
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => {
                self.searching = true;
                let q = self.query.trim().to_owned();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.users_autocomplete(&q).await },
                    move |r| Msg::Results(seq, r),
                ))
            }
            Msg::Search(_) => Action::none(),
            Msg::Results(seq, r) => {
                if seq == self.query_seq {
                    self.searching = false;
                    match r {
                        Ok(list) => self.results = list,
                        Err(e) => {
                            self.results.clear();
                            tracing::debug!(error = %e, "users.autocomplete failed");
                        }
                    }
                }
                Action::none()
            }
            Msg::Pick(u) => {
                self.pick(u);
                self.query.clear();
                self.results.clear();
                Action::none()
            }
            Msg::Unpick(id) => {
                self.selected.retain(|u| u.id != id);
                Action::none()
            }
            Msg::Add => {
                if self.adding || self.selected.is_empty() {
                    return Action::none();
                }
                let Some(t) = ctx.workspace.room(ctx.rid).map(|e| e.room_type()) else {
                    return Action::none();
                };
                self.adding = true;
                let names: Vec<String> = self
                    .selected
                    .iter()
                    .filter_map(|u| u.username.clone())
                    .collect();
                let rid = ctx.rid.to_owned();
                Action::task(ctx.rest_task(
                    move |rest| api::invite_users(rest, t, rid, names),
                    Msg::Added,
                ))
            }
            Msg::Added(r) => {
                self.adding = false;
                match r {
                    Ok(()) => {
                        self.selected.clear();
                        Action::effect(common::ok_toast(t!("room_panel.users_added").into_owned()))
                            .with(Effect::OpenPanel(Kind::Members))
                    }
                    Err(e) => Action::effect(common::error_toast(&t!("room_panel.add_users"), &e)),
                }
            }
            Msg::CreateLink => {
                if self.link_busy {
                    return Action::none();
                }
                self.link_busy = true;
                self.link_error = None;
                let rid = ctx.rid.to_owned();
                Action::task(
                    ctx.rest_task(move |rest| api::find_or_create_invite(rest, rid), Msg::Link),
                )
            }
            Msg::Link(r) => {
                self.link_busy = false;
                match r {
                    Ok(l) => self.link = Some(l),
                    Err(e) => self.link_error = Some(common::failure_text(&e)),
                }
                Action::none()
            }
            Msg::Copy => match &self.link {
                Some(l) => Action::task(iced::clipboard::write::<Msg>(l.url.clone()))
                    .with(common::ok_toast(t!("room_panel.link_copied").into_owned())),
                None => Action::task(Task::none()),
            },
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let use_real = common::use_real_names(ctx);
        let mut col = column![].spacing(SPACE_3).padding(SPACE_4);
        if permissions::can_add_users(ws, ctx.rid) {
            col = col.push(
                text(t!("room_panel.choose_users").into_owned())
                    .size(TEXT_CAPTION)
                    .font(FONT_SEMIBOLD)
                    .style(style::text_hint),
            );
            col = col.push(common::search_field(
                &self.query,
                t!("room_panel.search_users").into_owned(),
                Msg::Query,
            ));
            if !self.selected.is_empty() {
                let mut chips = row![].spacing(SPACE_1);
                for u in &self.selected {
                    let username = u.username.clone().unwrap_or_default();
                    chips = chips.push(
                        button(
                            row![
                                common::user_avatar(ctx, &username, &username, 18.0),
                                text(username.clone()).size(TEXT_CAPTION),
                                icon::sized(Icon::X, 12.0),
                            ]
                            .spacing(SPACE_1)
                            .align_y(Alignment::Center),
                        )
                        .padding([2, 6])
                        .style(style::button_secondary)
                        .on_press(Msg::Unpick(u.id.clone())),
                    );
                }
                col = col.push(chips.wrap());
            }
            if self.searching {
                col = col.push(common::loading());
            }
            let mut results = column![].spacing(0);
            let me = ws.user_id().unwrap_or_default();
            for u in self
                .results
                .iter()
                .filter(|u| u.id != me && !self.selected.iter().any(|s| s.id == u.id))
            {
                let username = u.username.clone().unwrap_or_default();
                let name = common::user_display(ws, use_real, &username, u.name.as_deref());
                results = results.push(
                    button(
                        row![
                            common::user_avatar(ctx, &username, &name, 28.0),
                            column![
                                ellipsis(name).size(TEXT_BODY).font(FONT_MEDIUM),
                                ellipsis(format!("@{username}")).size(TEXT_CAPTION).style(
                                    |t: &iced::Theme| Some(rustket_ui::tokens(t).text_secondary)
                                ),
                            ]
                            .width(Length::Fill),
                        ]
                        .spacing(SPACE_3)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([SPACE_1, SPACE_2])
                    .style(style::button_list_item(false))
                    .on_press(Msg::Pick(u.clone())),
                );
            }
            col = col.push(results);
            col = col.push(
                primary_button(
                    t!("room_panel.add_users").into_owned(),
                    (!self.selected.is_empty()).then_some(Msg::Add),
                )
                .icon(Icon::UserPlus)
                .loading(self.adding)
                .width(Length::Fill),
            );
        }
        if permissions::can_create_invite_links(ws, ctx.rid) {
            col = col.push(iced::widget::space().height(SPACE_2));
            col = col.push(
                text(t!("room_panel.invite_link").into_owned())
                    .size(TEXT_CAPTION)
                    .font(FONT_SEMIBOLD)
                    .style(style::text_hint),
            );
            match &self.link {
                Some(l) => {
                    col = col.push(
                        row![
                            text_input("", &l.url)
                                .size(TEXT_BODY)
                                .padding([7, 10])
                                .style(style::text_input),
                            icon_button(Icon::Copy, Some(Msg::Copy), t!("room_panel.copy")),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                    );
                    if let Some(exp) = l.expires {
                        col = col.push(
                            text(
                                t!(
                                    "room_panel.link_expires",
                                    time = common::time_label(Some(exp), ctx)
                                )
                                .into_owned(),
                            )
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                        );
                    }
                }
                None => {
                    col = col.push(
                        secondary_button(
                            t!("room_panel.create_link").into_owned(),
                            Some(Msg::CreateLink),
                        )
                        .icon(Icon::Link)
                        .loading(self.link_busy)
                        .width(Length::Fill),
                    );
                }
            }
            if let Some(e) = &self.link_error {
                col = col.push(text(e.clone()).size(TEXT_CAPTION).style(style::text_danger));
            }
        }
        if !permissions::can_add_users(ws, ctx.rid)
            && !permissions::can_create_invite_links(ws, ctx.rid)
        {
            return common::empty(Icon::Lock, t!("room_panel.err_not_allowed").into_owned());
        }
        scrollable(container(col).width(Length::Fill))
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_dedups() {
        let mut i = Invite::default();
        let u = UserSummary {
            id: "u".into(),
            username: Some("bob".into()),
            ..Default::default()
        };
        i.pick(u.clone());
        i.pick(u);
        assert_eq!(i.selected.len(), 1);
    }
}
