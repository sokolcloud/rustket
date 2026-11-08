//! "Forward chat" (`docs/omnichannel.md` §12, web `ForwardChatModal`): to an
//! available agent or to another department, with an optional comment.
//! Exactly one target is sent.

use iced::widget::{Column, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{Department, ForwardChat, ForwardTarget, LivechatAgent, Paginated, Room};
use rocketchat::rest::{AgentQuery, DepartmentQuery};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    ellipsis, presence_dot, primary_button, secondary_button, spinner, text_field,
};

use super::Outcome;
use crate::error::Res;
use crate::room::panel::common::{filter_pills, presence_of, search_field};
use crate::room::{UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Agent,
    Department,
}

/// The chosen target: kind, id, label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub kind: TargetKind,
    pub id: String,
    pub label: String,
}

pub struct ForwardDialog {
    rid: String,
    serving: Option<String>,
    current_department: Option<String>,
    pub kind: TargetKind,
    query: String,
    seq: u64,
    loading: bool,
    agents: Vec<LivechatAgent>,
    departments: Vec<Department>,
    list_error: Option<String>,
    pub selected: Option<Selected>,
    comment: String,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Kind(TargetKind),
    Query(String),
    Search(u64),
    Agents(u64, Res<Paginated<LivechatAgent>>),
    Departments(u64, Res<Paginated<Department>>),
    Select(Selected),
    Comment(String),
    Submit,
    Cancel,
    Done(Res<()>),
}

impl ForwardDialog {
    pub fn open(room: &Room, ctx: &UpdateCtx<'_>) -> (Self, Task<Msg>) {
        let mut d = Self {
            rid: room.id.clone(),
            serving: room.served_by_agent().map(|s| s.id),
            current_department: room.department_id.clone().filter(|d| !d.is_empty()),
            kind: TargetKind::Agent,
            query: String::new(),
            seq: 0,
            loading: false,
            agents: Vec::new(),
            departments: Vec::new(),
            list_error: None,
            selected: None,
            comment: String::new(),
            busy: false,
            error: None,
        };
        let t = d.search(ctx);
        (d, t)
    }

    fn search(&mut self, ctx: &UpdateCtx<'_>) -> Task<Msg> {
        self.seq += 1;
        self.loading = true;
        self.list_error = None;
        let seq = self.seq;
        let text = Some(self.query.trim().to_owned()).filter(|s| !s.is_empty());
        match self.kind {
            TargetKind::Agent => {
                let q = AgentQuery::for_forward(text, self.serving.as_deref());
                ctx.rest_task(
                    move |rest| async move { rest.livechat_agents(&q).await },
                    move |r| Msg::Agents(seq, r),
                )
            }
            TargetKind::Department => {
                let q = DepartmentQuery::for_forward(text, self.current_department.as_deref());
                ctx.rest_task(
                    move |rest| async move { rest.livechat_departments(&q).await },
                    move |r| Msg::Departments(seq, r),
                )
            }
        }
    }

    /// The request body (one target only).
    pub fn body(&self) -> Option<ForwardChat> {
        let s = self.selected.as_ref()?;
        Some(ForwardChat {
            room_id: self.rid.clone(),
            target: match s.kind {
                TargetKind::Agent => ForwardTarget::Agent(s.id.clone()),
                TargetKind::Department => ForwardTarget::Department(s.id.clone()),
            },
            comment: Some(self.comment.trim().to_owned()).filter(|c| !c.is_empty()),
        })
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> (Task<Msg>, Outcome) {
        let mut task = Task::none();
        let mut out = Outcome::Stay;
        match msg {
            Msg::Kind(k) if k != self.kind => {
                self.kind = k;
                self.selected = None;
                task = self.search(ctx);
            }
            Msg::Kind(_) => {}
            Msg::Query(q) => {
                self.query = q;
                self.seq += 1;
                task = crate::room::panel::debounce(Msg::Search(self.seq));
            }
            Msg::Search(seq) if seq == self.seq => task = self.search(ctx),
            Msg::Search(_) => {}
            Msg::Agents(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => self.agents = p.items,
                    Err(f) => self.list_error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Departments(seq, r) if seq == self.seq => {
                self.loading = false;
                match r {
                    Ok(p) => self.departments = p.items,
                    Err(f) => self.list_error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Agents(..) | Msg::Departments(..) => {}
            Msg::Select(s) => self.selected = Some(s),
            Msg::Comment(c) => self.comment = c,
            Msg::Cancel if !self.busy => out = Outcome::Dismiss,
            Msg::Cancel => {}
            Msg::Submit => {
                if let (false, Some(body)) = (self.busy, self.body()) {
                    self.busy = true;
                    self.error = None;
                    task = ctx.rest_task(
                        move |rest| async move { rest.livechat_forward_chat(&body).await },
                        Msg::Done,
                    );
                }
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        out = Outcome::Finished {
                            toast: Some(t!("omnichannel.forwarded_ok").into_owned()),
                            close_room: true,
                        }
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
        }
        (task, out)
    }

    fn option<'a>(&self, sel: Selected, sub: Element<'a, Msg>) -> Element<'a, Msg> {
        let active = self.selected.as_ref().is_some_and(|s| s.id == sel.id);
        let mut r = row![
            column![ellipsis(sel.label.clone()).size(TEXT_BODY), sub]
                .spacing(0)
                .width(Length::Fill)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if active {
            r = r.push(icon::sized(Icon::Check, 14.0));
        }
        button(r)
            .width(Length::Fill)
            .padding([SPACE_1 + 2.0, SPACE_2])
            .style(style::button_list_item(active))
            .on_press_maybe((!self.busy).then_some(Msg::Select(sel)))
            .into()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let _ = ctx;
        let enabled = !self.busy;
        let pills = filter_pills(
            &[
                (
                    TargetKind::Agent,
                    t!("omnichannel.forward_to_agent").into_owned(),
                ),
                (
                    TargetKind::Department,
                    t!("omnichannel.forward_to_department").into_owned(),
                ),
            ],
            self.kind,
            Msg::Kind,
        );
        let placeholder = match self.kind {
            TargetKind::Agent => t!("omnichannel.search_agents"),
            TargetKind::Department => t!("omnichannel.search_departments"),
        };
        let mut list = Column::new().spacing(0);
        if self.loading {
            list = list.push(container(spinner(18.0)).padding(SPACE_2));
        } else if let Some(e) = &self.list_error {
            list = list.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        } else {
            match self.kind {
                TargetKind::Agent => {
                    for a in &self.agents {
                        let username = a.username.clone().unwrap_or_default();
                        let label = a
                            .name
                            .clone()
                            .filter(|n| !n.trim().is_empty())
                            .unwrap_or_else(|| username.clone());
                        let sub = row![
                            presence_dot(presence_of(a.status.unwrap_or_default()), 8.0),
                            text(format!("@{username}"))
                                .size(TEXT_CAPTION)
                                .style(style::text_secondary),
                        ]
                        .spacing(SPACE_1)
                        .align_y(Alignment::Center)
                        .into();
                        list = list.push(self.option(
                            Selected {
                                kind: TargetKind::Agent,
                                id: a.id.clone(),
                                label,
                            },
                            sub,
                        ));
                    }
                    if self.agents.is_empty() {
                        list = list.push(
                            text(t!("omnichannel.no_agents").into_owned())
                                .size(TEXT_SMALL)
                                .style(style::text_hint),
                        );
                    }
                }
                TargetKind::Department => {
                    for d in &self.departments {
                        let sub = text(
                            d.description
                                .clone()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_default(),
                        )
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                        .into();
                        list = list.push(self.option(
                            Selected {
                                kind: TargetKind::Department,
                                id: d.id.clone(),
                                label: d.name.clone(),
                            },
                            sub,
                        ));
                    }
                    if self.departments.is_empty() {
                        list = list.push(
                            text(t!("omnichannel.no_departments").into_owned())
                                .size(TEXT_SMALL)
                                .style(style::text_hint),
                        );
                    }
                }
            }
        }
        let mut body = column![
            pills,
            search_field(&self.query, placeholder.into_owned(), Msg::Query),
            container(scrollable(list).height(Length::Shrink))
                .max_height(220)
                .width(Length::Fill),
            text_field(
                t!("omnichannel.forward_comment").into_owned(),
                &self.comment,
                Msg::Comment
            )
            .placeholder(t!("omnichannel.comment_placeholder"))
            .disabled(!enabled),
        ]
        .spacing(SPACE_3)
        .width(Length::Fill);
        if let Some(e) = &self.error {
            body = body.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        }
        rustket_ui::widget::dialog(
            t!("omnichannel.forward_title").into_owned(),
            body,
            vec![
                secondary_button(
                    t!("omnichannel.cancel").into_owned(),
                    enabled.then_some(Msg::Cancel),
                )
                .into(),
                primary_button(
                    t!("omnichannel.forward_confirm").into_owned(),
                    (enabled && self.selected.is_some()).then_some(Msg::Submit),
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

    #[test]
    fn body_sends_one_target() {
        let mut d = ForwardDialog {
            rid: "r".into(),
            serving: Some("me".into()),
            current_department: None,
            kind: TargetKind::Agent,
            query: String::new(),
            seq: 0,
            loading: false,
            agents: Vec::new(),
            departments: Vec::new(),
            list_error: None,
            selected: None,
            comment: "  ".into(),
            busy: false,
            error: None,
        };
        assert!(d.body().is_none());
        d.selected = Some(Selected {
            kind: TargetKind::Department,
            id: "d1".into(),
            label: "Sales".into(),
        });
        let b = d.body().unwrap().to_body();
        assert_eq!(b["departmentId"], "d1");
        assert!(b.get("userId").is_none() && b.get("comment").is_none());
        assert_eq!(b["clientAction"], true);
    }
}
