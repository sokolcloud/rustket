//! "End conversation" (`docs/omnichannel.md` §11, web `CloseChatModal`): the
//! full "Wrap up conversation" form when a comment or tags are required or a
//! transcript can be requested, a simple confirmation otherwise.

use iced::widget::{checkbox, column, row, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{CloseChat, Department, LicenseInfo, Room, TranscriptEmail, Visitor};
use rust_i18n::t;
use rustket_core::omnichannel::permissions::{self as perm, settings};
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{
    ChipKind, chip, danger_button, dialog, icon_button, secondary_button, text_field,
};

use super::Outcome;
use crate::error::Res;
use crate::room::{UpdateCtx, ViewCtx};

pub const COMMENT_INPUT_ID: &str = "omni-close-comment";

pub struct CloseChatDialog {
    rid: String,
    pub comment: String,
    pub tags: Vec<String>,
    tag_input: String,
    comment_required: bool,
    department: Option<Department>,
    loading: usize,
    /// Visitor e-mail, when the e-mail transcript can be offered.
    visitor_email: Option<String>,
    email_offered: bool,
    pub email_transcript: bool,
    subject: String,
    pdf_offered: bool,
    pdf: bool,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Comment(String),
    TagInput(String),
    AddTag,
    RemoveTag(String),
    EmailTranscript(bool),
    Pdf(bool),
    Subject(String),
    Department(Res<Option<Box<Department>>>),
    Visitor(Res<Box<Visitor>>),
    Submit,
    Cancel,
    Done(Res<()>),
}

impl CloseChatDialog {
    /// A request of the dialog is in flight.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Opens the dialog for `room`, loading its department (closing tags) and
    /// the visitor (transcript e-mail).
    pub fn open(room: &Room, ctx: &UpdateCtx<'_>) -> (Self, Task<Msg>) {
        let ws = &*ctx.workspace;
        let rid = room.id.clone();
        let omni = crate::omnichannel::shared::get(ctx.server_id).unwrap_or_default();
        let email_offered = has_permission(ws, perm::SEND_OMNICHANNEL_CHAT_TRANSCRIPT, Some(&rid))
            && !ws
                .settings()
                .bool_or(settings::TRANSCRIPT_SEND_ALWAYS, false);
        let pref = |k: &str| {
            crate::desktop::user_pref(ws, k)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let subject = ws
            .settings()
            .str(settings::TRANSCRIPT_EMAIL_SUBJECT)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| t!("omnichannel.transcript_default_subject").into_owned());
        let mut d = Self {
            rid: rid.clone(),
            comment: String::new(),
            tags: room.tags.clone().unwrap_or_default(),
            tag_input: String::new(),
            comment_required: ws
                .settings()
                .bool_or(settings::REQUEST_COMMENT_WHEN_CLOSING, false),
            department: None,
            loading: 0,
            visitor_email: None,
            email_offered,
            email_transcript: email_offered && pref("omnichannelTranscriptEmail"),
            subject,
            pdf_offered: omni.has_module(LicenseInfo::LIVECHAT_ENTERPRISE)
                && has_permission(ws, perm::REQUEST_PDF_TRANSCRIPT, Some(&rid)),
            pdf: pref("omnichannelTranscriptPDF"),
            busy: false,
            error: None,
        };
        let mut tasks = vec![iced::widget::operation::focus(COMMENT_INPUT_ID)];
        if let Some(dep) = room.department_id.clone().filter(|d| !d.is_empty()) {
            d.loading += 1;
            tasks.push(ctx.rest_task(
                move |rest| async move {
                    rest.livechat_department(&dep)
                        .await
                        .map(|d| d.map(Box::new))
                },
                Msg::Department,
            ));
        }
        if d.email_offered
            && let Some(v) = room.visitor().filter(|v| !v.id.is_empty())
        {
            d.loading += 1;
            tasks.push(ctx.rest_task(
                move |rest| async move { rest.livechat_visitor_info(&v.id).await.map(Box::new) },
                Msg::Visitor,
            ));
        }
        (d, Task::batch(tasks))
    }

    fn tags_required(&self) -> bool {
        self.department
            .as_ref()
            .is_some_and(|d| d.request_tag_before_closing_chat)
    }

    fn can_email(&self) -> bool {
        self.email_offered && self.visitor_email.is_some()
    }

    /// The "Wrap up conversation" form instead of a plain confirmation.
    pub fn full_form(&self) -> bool {
        self.comment_required || self.tags_required() || self.can_email() || self.pdf_offered
    }

    /// Everything required is filled in.
    pub fn ready(&self) -> bool {
        (!self.comment_required || !self.comment.trim().is_empty())
            && (!self.tags_required() || !self.tags.is_empty())
            && (!(self.can_email() && self.email_transcript) || !self.subject.trim().is_empty())
    }

    /// The request body.
    pub fn body(&self) -> CloseChat {
        let mut c = CloseChat::new(&self.rid, Some(self.comment.trim().to_owned()));
        c.tags = self.tags.clone();
        c.transcript_email = match (&self.visitor_email, self.email_transcript) {
            (Some(email), true) if self.email_offered => {
                TranscriptEmail::to_visitor(email.clone(), self.subject.trim())
            }
            _ => TranscriptEmail::none(),
        };
        if self.pdf_offered && self.pdf {
            c.generate_transcript_pdf = Some(true);
        }
        c
    }

    fn add_tag(&mut self) {
        let tag = self.tag_input.trim().to_owned();
        if !tag.is_empty() && !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
        self.tag_input.clear();
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> (Task<Msg>, Outcome) {
        let mut out = Outcome::Stay;
        let mut task = Task::none();
        match msg {
            Msg::Comment(s) => self.comment = s,
            Msg::TagInput(s) => self.tag_input = s,
            Msg::AddTag => self.add_tag(),
            Msg::RemoveTag(tag) => self.tags.retain(|t| *t != tag),
            Msg::EmailTranscript(on) => self.email_transcript = on,
            Msg::Pdf(on) => self.pdf = on,
            Msg::Subject(s) => self.subject = s,
            Msg::Department(r) => {
                self.loading = self.loading.saturating_sub(1);
                match r {
                    Ok(d) => self.department = d.map(|d| *d),
                    Err(e) => tracing::debug!(error = %e, "department of the chat"),
                }
            }
            Msg::Visitor(r) => {
                self.loading = self.loading.saturating_sub(1);
                match r {
                    Ok(v) => self.visitor_email = v.email().map(str::to_owned),
                    Err(e) => tracing::debug!(error = %e, "visitor of the chat"),
                }
            }
            Msg::Cancel if !self.busy => out = Outcome::Dismiss,
            Msg::Cancel => {}
            Msg::Submit => {
                if !self.tag_input.trim().is_empty() {
                    self.add_tag();
                }
                if !self.busy && self.ready() {
                    self.busy = true;
                    self.error = None;
                    let body = self.body();
                    task = ctx.rest_task(
                        move |rest| async move { rest.livechat_close_chat(&body).await },
                        Msg::Done,
                    );
                }
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        out = Outcome::Finished {
                            toast: Some(t!("omnichannel.closed_ok").into_owned()),
                            close_room: super::hide_after_closing(ctx.workspace),
                        }
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
        }
        (task, out)
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let _ = ctx;
        let enabled = !self.busy;
        let mut body = column![].spacing(SPACE_3);
        if !self.full_form() {
            body =
                body.push(text(t!("omnichannel.close_confirm_body").into_owned()).size(TEXT_BODY));
        } else {
            body = body.push(
                text(t!("omnichannel.close_room_description").into_owned())
                    .size(TEXT_BODY)
                    .style(rustket_ui::style::text_secondary),
            );
            let comment_label = if self.comment_required {
                t!("omnichannel.comment_required")
            } else {
                t!("omnichannel.comment")
            };
            body = body.push(
                text_field(comment_label.into_owned(), &self.comment, Msg::Comment)
                    .placeholder(t!("omnichannel.comment_placeholder"))
                    .id(COMMENT_INPUT_ID)
                    .on_submit(Msg::Submit)
                    .disabled(!enabled),
            );
            let tags_label = if self.tags_required() {
                t!("omnichannel.tags_required")
            } else {
                t!("omnichannel.tags")
            };
            let mut tags_row = row![].spacing(SPACE_1).align_y(Alignment::Center);
            for tag in &self.tags {
                tags_row = tags_row.push(
                    row![
                        chip(tag.clone(), ChipKind::Neutral),
                        icon_button(
                            Icon::X,
                            enabled.then(|| Msg::RemoveTag(tag.clone())),
                            t!("omnichannel.remove_tag"),
                        ),
                    ]
                    .align_y(Alignment::Center),
                );
            }
            body = body.push(
                column![
                    row![
                        text_field(tags_label.into_owned(), &self.tag_input, Msg::TagInput)
                            .placeholder(t!("omnichannel.tag_placeholder"))
                            .on_submit(Msg::AddTag)
                            .disabled(!enabled),
                        secondary_button(
                            t!("omnichannel.add_tag").into_owned(),
                            (enabled && !self.tag_input.trim().is_empty()).then_some(Msg::AddTag)
                        ),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::End),
                    tags_row.wrap(),
                ]
                .spacing(SPACE_1),
            );
            if self.can_email() || self.pdf_offered {
                body = body.push(
                    text(t!("omnichannel.transcript").into_owned())
                        .size(TEXT_SMALL)
                        .style(style::text_hint),
                );
            }
            if self.pdf_offered {
                body = body.push(
                    checkbox(self.pdf)
                        .label(t!("omnichannel.transcript_pdf").into_owned())
                        .on_toggle_maybe(enabled.then_some(Msg::Pdf))
                        .size(16)
                        .text_size(TEXT_BODY)
                        .style(style::checkbox),
                );
            }
            if let (true, Some(email)) = (self.can_email(), &self.visitor_email) {
                body = body.push(
                    checkbox(self.email_transcript)
                        .label(t!("omnichannel.transcript_email").into_owned())
                        .on_toggle_maybe(enabled.then_some(Msg::EmailTranscript))
                        .size(16)
                        .text_size(TEXT_BODY)
                        .style(style::checkbox),
                );
                if self.email_transcript {
                    body = body.push(
                        text(t!("omnichannel.transcript_to", email = email).into_owned())
                            .size(TEXT_SMALL)
                            .style(style::text_secondary),
                    );
                    body = body.push(
                        text_field(
                            t!("omnichannel.subject").into_owned(),
                            &self.subject,
                            Msg::Subject,
                        )
                        .disabled(!enabled),
                    );
                }
            }
        }
        if let Some(e) = &self.error {
            body = body.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        }
        let title = if self.full_form() {
            t!("omnichannel.close_title_full")
        } else {
            t!("omnichannel.close_title")
        };
        dialog(
            title.into_owned(),
            body.width(Length::Fill),
            vec![
                secondary_button(
                    t!("omnichannel.cancel").into_owned(),
                    enabled.then_some(Msg::Cancel),
                )
                .into(),
                danger_button(
                    t!("omnichannel.close_confirm").into_owned(),
                    (enabled && self.ready() && self.loading == 0).then_some(Msg::Submit),
                )
                .loading(self.busy)
                .into(),
            ],
            enabled.then_some(Msg::Cancel),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dialog_for(comment_required: bool) -> CloseChatDialog {
        CloseChatDialog {
            rid: "r".into(),
            comment: String::new(),
            tags: vec!["alpha".into()],
            tag_input: String::new(),
            comment_required,
            department: None,
            loading: 0,
            visitor_email: None,
            email_offered: false,
            email_transcript: false,
            subject: "S".into(),
            pdf_offered: false,
            pdf: false,
            busy: false,
            error: None,
        }
    }

    #[test]
    fn requirements_and_body() {
        let mut d = dialog_for(false);
        assert!(!d.full_form() && d.ready());
        let b = serde_json::to_value(d.body()).unwrap();
        assert_eq!(b["rid"], "r");
        assert!(b.get("comment").is_none(), "blank comment omitted");
        assert_eq!(b["tags"], json!(["alpha"]));
        assert_eq!(b["transcriptEmail"], json!({"sendToVisitor": false}));

        let mut d2 = dialog_for(true);
        assert!(d2.full_form() && !d2.ready());
        d2.comment = "Resolved".into();
        assert!(d2.ready());

        d.department = Some(
            serde_json::from_value(json!({"_id":"d","name":"D",
            "requestTagBeforeClosingChat": true}))
            .unwrap(),
        );
        d.tags.clear();
        assert!(d.full_form() && !d.ready());
        d.tag_input = " billing ".into();
        d.add_tag();
        d.tag_input = "billing".into();
        d.add_tag();
        assert_eq!(d.tags, ["billing"]);
        assert!(d.ready());

        d.email_offered = true;
        d.visitor_email = Some("v@x.test".into());
        d.email_transcript = true;
        let b = serde_json::to_value(d.body()).unwrap();
        assert_eq!(
            b["transcriptEmail"],
            json!({"sendToVisitor": true, "requestData": {"email": "v@x.test", "subject": "S"}})
        );
    }
}
