//! Confirmations: "Move to the queue" (§13, `inquiries.returnAsInquiry`) and
//! "Place chat On-Hold" (§15, EE `room.onHold`).

use iced::widget::{column, text};
use iced::{Element, Task};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{dialog, primary_button, secondary_button};

use super::Outcome;
use crate::error::Res;
use crate::room::UpdateCtx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    ReturnQueue,
    OnHold,
}

pub struct Confirm {
    pub kind: ConfirmKind,
    rid: String,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Confirm,
    Cancel,
    Done(Res<bool>),
}

impl Confirm {
    pub fn new(kind: ConfirmKind, rid: &str) -> Self {
        Self {
            kind,
            rid: rid.to_owned(),
            busy: false,
            error: None,
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> (Task<Msg>, Outcome) {
        match msg {
            Msg::Cancel if !self.busy => (Task::none(), Outcome::Dismiss),
            Msg::Cancel => (Task::none(), Outcome::Stay),
            Msg::Confirm if self.busy => (Task::none(), Outcome::Stay),
            Msg::Confirm => {
                self.busy = true;
                self.error = None;
                let rid = self.rid.clone();
                let task = match self.kind {
                    ConfirmKind::ReturnQueue => ctx.rest_task(
                        move |rest| async move { rest.livechat_return_to_queue(&rid, None).await },
                        Msg::Done,
                    ),
                    ConfirmKind::OnHold => ctx.rest_task(
                        move |rest| async move {
                            rest.livechat_place_on_hold(&rid).await.map(|()| true)
                        },
                        Msg::Done,
                    ),
                };
                (task, Outcome::Stay)
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(true) => {
                        let (toast, close_room) = match self.kind {
                            ConfirmKind::ReturnQueue => {
                                (t!("omnichannel.returned_to_queue").into_owned(), true)
                            }
                            ConfirmKind::OnHold => {
                                (t!("omnichannel.placed_on_hold").into_owned(), false)
                            }
                        };
                        (
                            Task::none(),
                            Outcome::Finished {
                                toast: Some(toast),
                                close_room,
                            },
                        )
                    }
                    Ok(false) => {
                        self.error = Some(t!("omnichannel.err.return_failed").into_owned());
                        (Task::none(), Outcome::Stay)
                    }
                    Err(f) => {
                        self.error = Some(crate::omnichannel::error_text(&f));
                        (Task::none(), Outcome::Stay)
                    }
                }
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let (title, body, confirm) = match self.kind {
            ConfirmKind::ReturnQueue => (
                t!("omnichannel.return_title"),
                t!("omnichannel.return_body"),
                t!("omnichannel.return_confirm"),
            ),
            ConfirmKind::OnHold => (
                t!("omnichannel.on_hold_title"),
                t!("omnichannel.on_hold_body"),
                t!("omnichannel.on_hold_confirm"),
            ),
        };
        let mut col = column![text(body.into_owned()).size(TEXT_BODY)].spacing(SPACE_3);
        if let Some(e) = &self.error {
            col = col.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        }
        dialog(
            title.into_owned(),
            col,
            vec![
                secondary_button(
                    t!("omnichannel.cancel").into_owned(),
                    (!self.busy).then_some(Msg::Cancel),
                )
                .into(),
                primary_button(confirm.into_owned(), (!self.busy).then_some(Msg::Confirm))
                    .loading(self.busy)
                    .into(),
            ],
            (!self.busy).then_some(Msg::Cancel),
        )
    }
}
