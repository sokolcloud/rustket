//! "Send transcript via e-mail" (`docs/omnichannel.md` §14, web
//! `TranscriptModal`): an open room requests the transcript for when it
//! closes (or shows the pending request with Discard); a closed room sends
//! it now through the visitor endpoint (`livechat/transcript` with the
//! visitor token).

use iced::widget::{column, text};
use iced::{Element, Length, Task};
use rocketchat::models::{Room, Visitor};
use rust_i18n::t;
use rustket_core::omnichannel::permissions::settings;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{
    danger_button, dialog, primary_button, secondary_button, spinner, text_field,
};

use super::Outcome;
use crate::error::Res;
use crate::room::UpdateCtx;

pub struct TranscriptDialog {
    rid: String,
    open: bool,
    /// Pending request of an open room: (email, subject).
    requested: Option<(String, String)>,
    token: Option<String>,
    email: Option<String>,
    loading: bool,
    subject: String,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Visitor(Res<Box<Visitor>>),
    Subject(String),
    Submit,
    Discard,
    Cancel,
    Done(bool, Res<()>),
}

impl TranscriptDialog {
    /// A request of the dialog is in flight.
    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn open(room: &Room, ctx: &UpdateCtx<'_>) -> (Self, Task<Msg>) {
        let requested = room.transcript_request.as_ref().and_then(|r| {
            let email = r.get("email")?.as_str()?.to_owned();
            let subject = r
                .get("subject")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_owned();
            Some((email, subject))
        });
        let visitor = room.visitor();
        let subject = ctx
            .workspace
            .settings()
            .str(settings::TRANSCRIPT_EMAIL_SUBJECT)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| t!("omnichannel.transcript_default_subject").into_owned());
        let mut d = Self {
            rid: room.id.clone(),
            open: room.open == Some(true),
            requested,
            token: visitor.as_ref().and_then(|v| v.token.clone()),
            email: None,
            loading: false,
            subject,
            busy: false,
            error: None,
        };
        let task = match visitor.filter(|v| !v.id.is_empty()) {
            Some(v) => {
                d.loading = true;
                ctx.rest_task(
                    move |rest| async move { rest.livechat_visitor_info(&v.id).await.map(Box::new) },
                    Msg::Visitor,
                )
            }
            None => Task::none(),
        };
        (d, task)
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> (Task<Msg>, Outcome) {
        let mut task = Task::none();
        let mut out = Outcome::Stay;
        match msg {
            Msg::Visitor(r) => {
                self.loading = false;
                match r {
                    Ok(v) => self.email = v.email().map(str::to_owned),
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Subject(s) => self.subject = s,
            Msg::Cancel if !self.busy => out = Outcome::Dismiss,
            Msg::Cancel => {}
            Msg::Discard if !self.busy => {
                self.busy = true;
                let rid = self.rid.clone();
                task = ctx.rest_task(
                    move |rest| async move { rest.livechat_cancel_email_transcript(&rid).await },
                    |r| Msg::Done(true, r),
                );
            }
            Msg::Submit if !self.busy => {
                let Some(email) = self.email.clone() else {
                    return (task, out);
                };
                let rid = self.rid.clone();
                if self.open {
                    let subject = self.subject.trim().to_owned();
                    self.busy = true;
                    task = ctx.rest_task(
                        move |rest| async move {
                            rest.livechat_request_email_transcript(&rid, &email, &subject)
                                .await
                        },
                        |r| Msg::Done(false, r),
                    );
                } else if let Some(token) = self.token.clone() {
                    self.busy = true;
                    task = ctx.rest_task(
                        move |rest| async move {
                            rest.livechat_send_transcript(&token, &rid, &email).await
                        },
                        |r| Msg::Done(false, r),
                    );
                } else {
                    self.error = Some(t!("omnichannel.err.no_visitor_token").into_owned());
                }
            }
            Msg::Discard | Msg::Submit => {}
            Msg::Done(discard, r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        let toast = if discard {
                            t!("omnichannel.transcript_discarded")
                        } else if self.open {
                            t!("omnichannel.transcript_requested")
                        } else {
                            t!("omnichannel.transcript_sent")
                        };
                        out = Outcome::Finished {
                            toast: Some(toast.into_owned()),
                            close_room: false,
                        };
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
        }
        (task, out)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let enabled = !self.busy;
        let mut body = column![].spacing(SPACE_3).width(Length::Fill);
        let mut actions: Vec<Element<'_, Msg>> = vec![
            secondary_button(
                t!("omnichannel.cancel").into_owned(),
                enabled.then_some(Msg::Cancel),
            )
            .into(),
        ];
        if self.loading {
            body = body.push(spinner(18.0));
        } else if let Some((email, subject)) = &self.requested {
            body = body.push(
                text(t!("omnichannel.transcript_pending", email = email).into_owned())
                    .size(TEXT_BODY),
            );
            if !subject.is_empty() {
                body = body.push(
                    text(t!("omnichannel.transcript_subject_is", subject = subject).into_owned())
                        .size(TEXT_SMALL)
                        .style(style::text_secondary),
                );
            }
            if self.open {
                actions.push(
                    danger_button(
                        t!("omnichannel.transcript_discard").into_owned(),
                        enabled.then_some(Msg::Discard),
                    )
                    .loading(self.busy)
                    .into(),
                );
            }
        } else if let Some(email) = &self.email {
            body = body.push(
                text(t!("omnichannel.transcript_to", email = email).into_owned()).size(TEXT_BODY),
            );
            if self.open {
                body = body.push(
                    text_field(
                        t!("omnichannel.subject").into_owned(),
                        &self.subject,
                        Msg::Subject,
                    )
                    .disabled(!enabled),
                );
            }
            actions.push(
                primary_button(
                    t!("omnichannel.transcript_send").into_owned(),
                    (enabled && (!self.open || !self.subject.trim().is_empty()))
                        .then_some(Msg::Submit),
                )
                .loading(self.busy)
                .into(),
            );
        } else {
            body = body.push(
                text(t!("omnichannel.transcript_no_email").into_owned())
                    .size(TEXT_BODY)
                    .style(style::text_secondary),
            );
        }
        if let Some(e) = &self.error {
            body = body.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        }
        dialog(
            t!("omnichannel.transcript_title").into_owned(),
            body,
            actions,
            enabled.then_some(Msg::Cancel),
        )
    }
}
