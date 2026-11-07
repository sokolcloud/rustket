//! Thread panel placeholder: shows the parent message (from the room timeline
//! or `chat.getMessage`) until the thread view exists.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::divider;

use super::common;
use super::render_action;
use crate::error::Res;
use crate::room::render::{self, RenderMsg};
use crate::room::{Action, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Box<RcMessage>>),
    Retry,
    Render(RenderMsg),
}

pub struct Thread {
    tmid: String,
    parent: Option<Box<RcMessage>>,
    loading: bool,
    error: Option<String>,
}

fn find(ws: &Workspace, rid: &str, tmid: &str) -> Option<Box<RcMessage>> {
    ws.thread(tmid)
        .and_then(|t| t.parent().cloned())
        .or_else(|| ws.timeline(rid)?.get(tmid).map(|i| i.message.clone()))
        .map(Box::new)
}

impl Thread {
    pub fn new(tmid: &str) -> Self {
        Self {
            tmid: tmid.to_owned(),
            parent: None,
            loading: false,
            error: None,
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(m) = find(ctx.workspace, ctx.rid, &self.tmid) {
            self.parent = Some(m);
            return Action::none();
        }
        self.loading = true;
        self.error = None;
        let tmid = self.tmid.clone();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.get_message(&tmid).await.map(Box::new) },
            Msg::Loaded,
        ))
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if (changes.timelines.contains(ctx.rid) || changes.threads.contains(&self.tmid))
            && let Some(m) = find(ctx.workspace, ctx.rid, &self.tmid)
        {
            self.parent = Some(m);
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Loaded(r) => {
                self.loading = false;
                match r {
                    Ok(m) => self.parent = Some(m),
                    Err(e) => self.error = Some(common::failure_text(&e)),
                }
                Action::none()
            }
            Msg::Retry => self.activate(ctx),
            Msg::Render(m) => render_action(ctx.workspace, m),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let Some(m) = &self.parent else {
            if let Some(e) = &self.error {
                return common::error_state(e, Msg::Retry);
            }
            return common::loading();
        };
        let username = m.u.username.clone().unwrap_or_default();
        let use_real = common::use_real_names(ctx);
        let name = common::user_display(ctx.workspace, use_real, &username, m.u.name.as_deref());
        let col = column![
            row![
                common::user_avatar(ctx, &username, &name, 32.0),
                column![
                    row![
                        text(name.clone()).size(TEXT_BODY).font(FONT_SEMIBOLD),
                        text(common::time_label(m.ts, ctx))
                            .size(TEXT_CAPTION)
                            .style(style::text_hint),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center),
                    render::message_body(m, ctx).map(Msg::Render),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(SPACE_3),
            text(t!("room_panel.replies", count = m.tcount.unwrap_or(0)).into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_link),
            divider(),
            container(common::empty(
                Icon::MessageCircle,
                t!("room_panel.thread_soon").into_owned()
            )),
        ]
        .spacing(SPACE_3)
        .padding(SPACE_4);
        scrollable(col)
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}
