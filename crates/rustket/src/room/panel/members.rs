//! Room members (`rooms.membersOrderedByRole`): search, online/all filter,
//! roles; a click opens the user's profile.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Paginated, UserStatus, UserSummary};
use rust_i18n::t;
use rustket_core::permissions;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{ChipKind, chip, ellipsis, presence_dot, primary_button};

use super::api;
use super::common;
use super::paged::{LoadState, PageReq, Paged};
use super::{Kind, debounce};
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Online,
    All,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Filter(Filter),
    Page(PageReq, Res<Paginated<UserSummary>>),
    More,
    Retry,
    Open(String),
    Add,
}

#[derive(Default)]
pub struct Members {
    query: String,
    query_seq: u64,
    filter: Filter,
    list: Paged<UserSummary>,
}

impl Members {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let (rid, q, online) = (
            ctx.rid.to_owned(),
            self.query.clone(),
            self.filter == Filter::Online,
        );
        Action::task(ctx.rest_task(
            move |rest| api::members(rest, rid, q, online, req.offset),
            move |r| Msg::Page(req, r),
        ))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => self.reload(ctx),
            Msg::Search(_) => Action::none(),
            Msg::Filter(f) => {
                self.filter = f;
                self.reload(ctx)
            }
            Msg::Page(req, r) => {
                self.list.finish(req, r);
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Open(username) => Action::effect(Effect::OpenPanel(Kind::UserInfo(username))),
            Msg::Add => Action::effect(Effect::OpenPanel(Kind::Invite)),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let mut top = column![
            common::search_field(
                &self.query,
                t!("room_panel.search_members").into_owned(),
                Msg::Query
            ),
            row![common::filter_pills(
                &[
                    (Filter::Online, t!("room_panel.online").into_owned()),
                    (Filter::All, t!("room_panel.all").into_owned()),
                ],
                self.filter,
                Msg::Filter,
            ),],
        ]
        .spacing(SPACE_2)
        .padding([SPACE_3, SPACE_4]);
        if permissions::can_add_users(ws, ctx.rid) {
            top = top.push(
                primary_button(t!("room_panel.add_users").into_owned(), Some(Msg::Add))
                    .icon(Icon::UserPlus)
                    .width(Length::Fill),
            );
        }

        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(Icon::Users, t!("room_panel.no_members").into_owned())
            }
            _ => {
                let use_real = common::use_real_names(ctx);
                let mut col = column![].spacing(0);
                if self.list.total > 0 {
                    col = col.push(
                        container(
                            text(
                                t!(
                                    "room_panel.showing_count",
                                    shown = self.list.items.len(),
                                    total = self.list.total
                                )
                                .into_owned(),
                            )
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                        )
                        .padding([SPACE_1, SPACE_4]),
                    );
                }
                for u in &self.list.items {
                    let username = u.username.clone().unwrap_or_default();
                    let name = common::user_display(ws, use_real, &username, u.name.as_deref());
                    let status = ws
                        .presence()
                        .get(&u.id)
                        .map(|p| p.status)
                        .or(u.status)
                        .unwrap_or(UserStatus::Offline);
                    let mut line = row![
                        common::user_avatar(ctx, &username, &name, 32.0),
                        column![
                            row![
                                presence_dot(common::presence_of(status), 8.0),
                                ellipsis(name).size(TEXT_BODY).font(FONT_MEDIUM),
                            ]
                            .spacing(SPACE_1 + 2.0)
                            .align_y(Alignment::Center),
                            ellipsis(format!("@{username}")).size(TEXT_CAPTION).style(
                                |t: &iced::Theme| Some(rustket_ui::tokens(t).text_secondary)
                            ),
                        ]
                        .width(Length::Fill),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center);
                    for r in u.roles.iter().flatten() {
                        if matches!(r.as_str(), "owner" | "moderator" | "leader") {
                            line = line.push(chip(common::role_label(r), ChipKind::Primary));
                        }
                    }
                    col = col.push(common::list_row(
                        line,
                        (!username.is_empty()).then(|| Msg::Open(username.clone())),
                    ));
                }
                col = col.push(common::load_more(
                    self.list.state == LoadState::LoadingMore,
                    self.list.has_more(),
                    Msg::More,
                ));
                scrollable(col)
                    .height(Length::Fill)
                    .style(style::scrollable)
                    .into()
            }
        };
        column![top, body].height(Length::Fill).into()
    }
}
