//! Message lists: threads (with filters), pinned, starred, mentions and the
//! room search. Bodies go through the shared message renderer.

use iced::widget::{column, container, mouse_area, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Message as RcMessage, Paginated};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
};
use rustket_ui::widget::{BadgeKind, divider, dot, ellipsis};

use super::api::{self, MessageList, ThreadFilter};
use super::common;
use super::paged::{LoadState, PageReq, Paged};
use super::{Kind, debounce, render_action};
use crate::error::Res;
use crate::room::render::{self, RenderMsg};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Threads,
    List(MessageList),
    Search,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Filter(ThreadFilter),
    Page(PageReq, Res<Paginated<RcMessage>>),
    More,
    Retry,
    Render(RenderMsg),
    OpenThread(String),
    OpenUser(String),
}

pub struct Messages {
    mode: Mode,
    query: String,
    query_seq: u64,
    filter: ThreadFilter,
    list: Paged<RcMessage>,
}

impl Messages {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            query: String::new(),
            query_seq: 0,
            filter: ThreadFilter::All,
            list: Paged::default(),
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        if self.mode == Mode::Search && self.query.trim().is_empty() {
            self.list.clear();
            return Action::none();
        }
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let rid = ctx.rid.to_owned();
        let q = self.query.clone();
        let map = move |r| Msg::Page(req, r);
        let task = match self.mode {
            Mode::Threads => {
                let f = self.filter;
                ctx.rest_task(move |rest| api::threads(rest, rid, f, q, req.offset), map)
            }
            Mode::List(which) => ctx.rest_task(
                move |rest| api::message_list(rest, rid, which, req.offset),
                map,
            ),
            Mode::Search => ctx.rest_task(move |rest| api::search(rest, rid, q, req.offset), map),
        };
        Action::task(task)
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
            Msg::Render(m) => render_action(ctx.workspace, m),
            Msg::OpenThread(tmid) => Action::effect(Effect::OpenPanel(Kind::Thread(tmid))),
            Msg::OpenUser(u) => Action::effect(Effect::OpenPanel(Kind::UserInfo(u))),
        }
    }

    fn empty_text(&self) -> String {
        match self.mode {
            Mode::Threads => t!("room_panel.no_threads"),
            Mode::List(MessageList::Pinned) => t!("room_panel.no_pinned"),
            Mode::List(MessageList::Starred) => t!("room_panel.no_starred"),
            Mode::List(MessageList::Mentions) => t!("room_panel.no_mentions"),
            Mode::Search => t!("room_panel.no_results"),
        }
        .into_owned()
    }

    fn empty_icon(&self) -> Icon {
        match self.mode {
            Mode::Threads => Icon::MessageCircle,
            Mode::List(MessageList::Pinned) => Icon::Pin,
            Mode::List(MessageList::Starred) => Icon::Star,
            Mode::List(MessageList::Mentions) => Icon::AtSign,
            Mode::Search => Icon::Search,
        }
    }

    fn row<'a>(&'a self, m: &'a RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let use_real = common::use_real_names(ctx);
        let username = m.u.username.clone().unwrap_or_default();
        let name = m
            .alias
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| common::user_display(ws, use_real, &username, m.u.name.as_deref()));
        let head = row![
            mouse_area(ellipsis(name.clone()).size(TEXT_BODY).font(FONT_SEMIBOLD))
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::OpenUser(username.clone())),
            text(common::time_label(m.ts, ctx))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        let mut content = column![head, render::message_body(m, ctx).map(Msg::Render)].spacing(2);
        if self.mode == Mode::Threads {
            let unread = ws
                .subscription(ctx.rid)
                .and_then(|s| s.tunread.as_ref())
                .is_some_and(|l| l.iter().any(|id| id == &m.id));
            let mut foot = row![
                icon::sized(Icon::MessageCircle, 13.0).style(style::text_link),
                text(t!("room_panel.replies", count = m.tcount.unwrap_or(0)).into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_link),
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center);
            if let Some(tlm) = m.tlm {
                foot = foot.push(
                    text(
                        t!(
                            "room_panel.last_reply",
                            time = common::time_label(Some(tlm), ctx)
                        )
                        .into_owned(),
                    )
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
                );
            }
            if unread {
                foot = foot.push(dot(BadgeKind::Thread, 8.0));
            }
            content = content.push(foot.spacing(SPACE_2));
        }
        let line = row![
            common::user_avatar(ctx, &username, &name, 28.0),
            content.width(Length::Fill)
        ]
        .spacing(SPACE_3);
        let cell = container(line)
            .padding([SPACE_2, SPACE_4])
            .width(Length::Fill);
        if self.mode == Mode::Threads {
            mouse_area(cell)
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::OpenThread(m.id.clone()))
                .into()
        } else {
            cell.into()
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let mut top = column![].spacing(SPACE_2).padding([SPACE_3, SPACE_4]);
        match self.mode {
            Mode::Threads => {
                top = top
                    .push(common::search_field(
                        &self.query,
                        t!("room_panel.search_threads").into_owned(),
                        Msg::Query,
                    ))
                    .push(common::filter_pills(
                        &ThreadFilter::ALL.map(|f| {
                            (
                                f,
                                match f {
                                    ThreadFilter::All => t!("room_panel.all"),
                                    ThreadFilter::Following => t!("room_panel.following"),
                                    ThreadFilter::Unread => t!("room_panel.unread"),
                                }
                                .into_owned(),
                            )
                        }),
                        self.filter,
                        Msg::Filter,
                    ));
            }
            Mode::Search => {
                top = top.push(common::search_field(
                    &self.query,
                    t!("room_panel.search_placeholder").into_owned(),
                    Msg::Query,
                ));
            }
            Mode::List(_) => {}
        }
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            LoadState::Idle if self.mode == Mode::Search && !self.list.loaded => {
                common::empty(Icon::Search, t!("room_panel.search_hint").into_owned())
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(self.empty_icon(), self.empty_text())
            }
            _ => {
                let mut col = column![].spacing(0);
                for (i, m) in self.list.items.iter().enumerate() {
                    if i > 0 {
                        col = col.push(container(divider()).padding([0.0, SPACE_4]));
                    }
                    col = col.push(self.row(m, ctx));
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
        if matches!(self.mode, Mode::List(_)) {
            body
        } else {
            column![top, body].height(Length::Fill).into()
        }
    }
}
