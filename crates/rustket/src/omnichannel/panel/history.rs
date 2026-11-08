//! Contact history (`docs/omnichannel.md` §10): the contact's closed chats
//! (`omnichannel/contacts.history`), and one past chat read-only with search
//! (`livechat/{rid}/messages`, oldest first).

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{Message as RcMessage, Paginated, Room};
use rocketchat::rest::LivechatPage;
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL,
};
use rustket_ui::widget::{ChipKind, chip, divider, ellipsis};

use super::PanelKind;
use super::info::source_icon;
use crate::error::Res;
use crate::room::panel::common::{self, list_row, load_more, search_field};
use crate::room::render::RenderMsg;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx, panel};

const PAGE: u64 = 25;

#[derive(Debug, Clone)]
pub enum Msg {
    Chats(u64, Res<Paginated<Room>>),
    Messages(u64, Res<Paginated<RcMessage>>),
    More,
    Query(String),
    Search(u64),
    Retry,
    Open(String),
    Render(RenderMsg),
}

enum Mode {
    List,
    Chat(String),
}

pub struct HistoryPanel {
    mode: Mode,
    chats: Vec<Room>,
    messages: Vec<RcMessage>,
    total: u64,
    loading: bool,
    error: Option<String>,
    query: String,
    seq: u64,
}

impl HistoryPanel {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            chats: Vec::new(),
            messages: Vec::new(),
            total: 0,
            loading: false,
            error: None,
            query: String::new(),
            seq: 0,
        }
    }

    pub fn list() -> Self {
        Self::new(Mode::List)
    }

    pub fn chat(rid: &str) -> Self {
        Self::new(Mode::Chat(rid.to_owned()))
    }

    fn len(&self) -> u64 {
        match self.mode {
            Mode::List => self.chats.len() as u64,
            Mode::Chat(_) => self.messages.len() as u64,
        }
    }

    fn load(&mut self, ctx: &UpdateCtx<'_>, offset: u64) -> Task<Msg> {
        self.loading = true;
        self.error = None;
        self.seq += 1;
        let seq = self.seq;
        let page = LivechatPage::new(offset, PAGE);
        match &self.mode {
            Mode::List => {
                let Some(cid) = ctx
                    .workspace
                    .room(ctx.rid)
                    .and_then(|e| e.room.as_ref())
                    .and_then(|r| r.contact_id.clone())
                else {
                    self.loading = false;
                    return Task::none();
                };
                ctx.rest_task(
                    move |rest| async move { rest.omnichannel_contact_history(&cid, None, page).await },
                    move |r| Msg::Chats(seq, r),
                )
            }
            Mode::Chat(rid) => {
                let rid = rid.clone();
                let q = Some(self.query.trim().to_owned()).filter(|q| !q.is_empty());
                ctx.rest_task(
                    move |rest| async move {
                        rest.livechat_room_messages(&rid, q.as_deref(), page, true)
                            .await
                    },
                    move |r| Msg::Messages(seq, r),
                )
            }
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        Action::task(self.load(ctx, 0))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Chats(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => {
                        self.total = p.total;
                        self.chats.extend(p.items);
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Messages(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => {
                        self.total = p.total;
                        self.messages.extend(p.items);
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Chats(..) | Msg::Messages(..) => {}
            Msg::More if !self.loading && self.len() < self.total => {
                let offset = self.len();
                return Action::task(self.load(ctx, offset));
            }
            Msg::More => {}
            Msg::Query(q) => {
                self.query = q;
                self.seq += 1;
                return Action::task(panel::debounce(Msg::Search(self.seq)));
            }
            Msg::Search(seq) if seq == self.seq => {
                self.messages.clear();
                return Action::task(self.load(ctx, 0));
            }
            Msg::Search(_) => {}
            Msg::Retry => {
                self.chats.clear();
                self.messages.clear();
                return Action::task(self.load(ctx, 0));
            }
            Msg::Open(rid) => {
                return Action::effect(Effect::OpenPanel(panel::Kind::Omnichannel(
                    PanelKind::ContactHistoryChat(rid),
                )));
            }
            Msg::Render(RenderMsg::OpenUrl(u)) => return Action::effect(Effect::OpenUrl(u)),
            Msg::Render(_) => {}
        }
        Action::none()
    }

    fn chat_row<'a>(&self, r: &'a Room, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let src = r.omnichannel_source().map(|s| s.kind).unwrap_or_default();
        let when = r.closed_at.or(r.ts);
        let agent = r
            .served_by_agent()
            .and_then(|s| s.username)
            .map(|u| format!("@{u}"))
            .unwrap_or_default();
        let mut meta = row![
            text(common::time_label(when, ctx))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            text(agent).size(TEXT_CAPTION).style(style::text_secondary),
            text(t!("omnichannel.messages_count", count = r.msgs.unwrap_or(0)).into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_2);
        for tag in r.tags.iter().flatten().take(3) {
            meta = meta.push(chip(tag.clone(), ChipKind::Neutral));
        }
        let name = r.fname.clone().unwrap_or_default();
        list_row(
            row![
                icon::sized(source_icon(src), 16.0).style(style::text_secondary),
                column![ellipsis(name).size(TEXT_BODY).font(FONT_SEMIBOLD), meta]
                    .spacing(SPACE_1)
                    .width(Length::Fill),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
            Some(Msg::Open(r.id.clone())),
        )
    }

    fn message_row<'a>(&self, m: &'a RcMessage, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let author = m
            .alias
            .clone()
            .filter(|a| !a.trim().is_empty())
            .or_else(|| m.u.name.clone().filter(|n| !n.trim().is_empty()))
            .or_else(|| m.u.username.clone())
            .unwrap_or_default();
        let body: Element<'a, Msg> = if m.is_system() {
            text(
                crate::omnichannel::system::text(m, &rust_i18n::locale())
                    .unwrap_or_else(|| m.msg.clone()),
            )
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .into()
        } else {
            crate::room::render::message_body(m, ctx).map(Msg::Render)
        };
        container(
            column![
                row![
                    text(author).size(TEXT_SMALL).font(FONT_SEMIBOLD),
                    text(common::time_label(m.ts, ctx))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
                body,
            ]
            .spacing(2),
        )
        .padding([SPACE_2, SPACE_3])
        .width(Length::Fill)
        .into()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let mut list: Column<'a, Msg> = column![].spacing(0);
        let empty = self.len() == 0;
        if let (Some(e), true) = (&self.error, empty) {
            return common::error_state(e, Msg::Retry);
        }
        match &self.mode {
            Mode::List => {
                if empty && !self.loading {
                    return common::empty(Icon::History, t!("omnichannel.no_history").into_owned());
                }
                for r in &self.chats {
                    list = list.push(self.chat_row(r, ctx));
                }
            }
            Mode::Chat(_) => {
                for m in self
                    .messages
                    .iter()
                    .filter(|m| !crate::omnichannel::system::hidden(m))
                {
                    list = list.push(self.message_row(m, ctx));
                }
                if empty && !self.loading {
                    list = list.push(common::empty(
                        Icon::MessageSquare,
                        t!("omnichannel.no_messages").into_owned(),
                    ));
                }
            }
        }
        list = list.push(load_more(self.loading, self.len() < self.total, Msg::More));
        let scroll = scrollable(list)
            .height(Length::Fill)
            .style(style::scrollable);
        match &self.mode {
            Mode::List => scroll.into(),
            Mode::Chat(_) => column![
                container(search_field(
                    &self.query,
                    t!("omnichannel.search_messages").into_owned(),
                    Msg::Query
                ))
                .padding(SPACE_3),
                divider(),
                scroll,
            ]
            .height(Length::Fill)
            .into(),
        }
    }
}
