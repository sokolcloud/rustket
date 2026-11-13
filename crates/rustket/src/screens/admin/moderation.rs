//! Moderation console (docs/admin.md §18): reported messages per user and
//! reported users, the details of one user's reports, and the web's action
//! sequences (delete message + dismiss, delete all + deactivate + dismiss, …).

use iced::widget::{button, column, container, row, text};
use iced::{Element, Length, Task};
use rocketchat::models::Paginated;
use std::collections::HashMap;

use rocketchat::models::admin::{
    ModerationReport, ReportedMessagesRow, UserReportedMessages, UserReportsByUser, UserReportsRow,
};
use rocketchat::rest::admin::{ModerationFilter, Page as RestPage};
use rust_i18n::t;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL, font_medium};
use rustket_ui::widget::{ChipKind, chip, danger_button, dialog, ghost_button, secondary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, col, date, err_text, list_view, muted, search_bar, small,
    table, title_row,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Messages,
    Users,
}

/// Call sequences of the moderation actions.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    DeleteMessage { rid: String, mid: String },
    DismissMessage(String),
    DismissUser { user_id: String, user_reports: bool },
    DeleteAll(String),
    Deactivate { user_id: String, user_reports: bool },
    ResetAvatar(String),
}

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    MessagesLoaded(u64, Res<Paginated<ReportedMessagesRow>>),
    UsersLoaded(u64, Res<Paginated<UserReportsRow>>),
    Tab(Tab),
    /// Reported messages of a user (tab "Messages").
    Open(String, String),
    /// Reports about a user (tab "Users").
    OpenUser(String, String),
    Details(String, Res<Box<UserReportedMessages>>),
    UserReports(String, Res<Box<UserReportsByUser>>),
    /// Shows / hides who reported a message and why.
    ToggleReports(String),
    MsgReports(String, Res<Paginated<ModerationReport>>),
    CloseDialog,
    Ask(Action),
    Run(Action),
    /// An action finished; `true` closes the details (the reports are gone).
    Ran(bool, Res<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    Messages,
    Users,
}

pub struct Details {
    pub kind: DetailKind,
    pub user_id: String,
    pub name: String,
    pub data: Option<UserReportedMessages>,
    pub user_reports: Option<UserReportsByUser>,
    /// Reports per message id: `None` while loading.
    pub msg_reports: HashMap<String, Option<Vec<ModerationReport>>>,
}

impl Details {
    fn new(kind: DetailKind, user_id: String, name: String) -> Self {
        Self {
            kind,
            user_id,
            name,
            data: None,
            user_reports: None,
            msg_reports: HashMap::new(),
        }
    }
}

pub struct State {
    pub tab: Tab,
    pub messages: List<ReportedMessagesRow>,
    pub users: List<UserReportsRow>,
    pub details: Option<Details>,
    pub busy: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            tab: Tab::Messages,
            messages: List::new(Some(("reports.ts", false))),
            users: List::new(Some(("reports.ts", false))),
            details: None,
            busy: false,
        }
    }

    fn list(&mut self) -> &mut List<ReportedMessagesRow> {
        &mut self.messages
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match self.tab {
            Tab::Messages => {
                let l = self.list();
                let req = l.begin();
                let filter = ModerationFilter {
                    selector: l.search_term(),
                    ..ModerationFilter::default()
                };
                let page = RestPage::new(l.offset, l.page_size);
                cx.ctx.rest_task(
                    move |r| async move { r.moderation_reports_by_users(&filter, &page).await },
                    move |r| Message::Moderation(Msg::MessagesLoaded(req, r)),
                )
            }
            Tab::Users => {
                let l = &mut self.users;
                let req = l.begin();
                let filter = ModerationFilter {
                    selector: l.search_term(),
                    ..ModerationFilter::default()
                };
                let page = RestPage::new(l.offset, l.page_size);
                cx.ctx.rest_task(
                    move |r| async move { r.moderation_user_reports(&filter, &page).await },
                    move |r| Message::Moderation(Msg::UsersLoaded(req, r)),
                )
            }
        }
    }

    fn details(&self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let Some(d) = &self.details else {
            return Task::none();
        };
        let id = d.user_id.clone();
        let id2 = id.clone();
        match d.kind {
            DetailKind::Messages => cx.ctx.rest_task(
                move |r| async move {
                    r.moderation_user_reported_messages(&id, None, &RestPage::new(0, 50))
                        .await
                        .map(Box::new)
                },
                move |r| Message::Moderation(Msg::Details(id2, r)),
            ),
            DetailKind::Users => cx.ctx.rest_task(
                move |r| async move {
                    r.moderation_user_reports_by_user_id(&id, &RestPage::new(0, 50))
                        .await
                        .map(Box::new)
                },
                move |r| Message::Moderation(Msg::UserReports(id2, r)),
            ),
        }
    }

    fn msg_reports(&self, mid: String, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let mid2 = mid.clone();
        cx.ctx.rest_task(
            move |r| async move {
                r.moderation_message_reports(&mid, &RestPage::new(0, 50))
                    .await
            },
            move |r| Message::Moderation(Msg::MsgReports(mid2, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::List(m) => {
                let step = match self.tab {
                    Tab::Messages => self.messages.update(m),
                    Tab::Users => self.users.update(m),
                };
                return match step {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Moderation(Msg::List(m))),
                    Step::Load => self.load(cx),
                };
            }
            Msg::MessagesLoaded(req, r) => {
                if self
                    .messages
                    .finish(req, r.map_err(|f| err_text(&f, cx.i18n)))
                    && self.tab == Tab::Messages
                {
                    return self.load(cx);
                }
            }
            Msg::UsersLoaded(req, r) => {
                if self.users.finish(req, r.map_err(|f| err_text(&f, cx.i18n)))
                    && self.tab == Tab::Users
                {
                    return self.load(cx);
                }
            }
            Msg::Tab(t) => {
                self.tab = t;
                return self.load(cx);
            }
            Msg::Open(user_id, name) => {
                self.details = Some(Details::new(DetailKind::Messages, user_id, name));
                return self.details(cx);
            }
            Msg::OpenUser(user_id, name) => {
                self.details = Some(Details::new(DetailKind::Users, user_id, name));
                return self.details(cx);
            }
            Msg::Details(uid, r) => {
                let Some(x) = self.details.as_mut().filter(|x| x.user_id == uid) else {
                    return Task::none();
                };
                match r {
                    Ok(d) => {
                        // Reload the opened report lists too.
                        x.msg_reports.retain(|mid, _| {
                            d.messages
                                .iter()
                                .any(|m| m.message.as_ref().is_some_and(|m| m.id == *mid))
                        });
                        x.data = Some(*d);
                        let mids: Vec<String> = x.msg_reports.keys().cloned().collect();
                        return Task::batch(mids.into_iter().map(|mid| self.msg_reports(mid, cx)));
                    }
                    Err(f) => cx.fail(&f),
                }
            }
            Msg::UserReports(uid, r) => {
                let Some(x) = self.details.as_mut().filter(|x| x.user_id == uid) else {
                    return Task::none();
                };
                match r {
                    Ok(d) => x.user_reports = Some(*d),
                    Err(f) => cx.fail(&f),
                }
            }
            Msg::ToggleReports(mid) => {
                let Some(x) = &mut self.details else {
                    return Task::none();
                };
                if x.msg_reports.remove(&mid).is_none() {
                    x.msg_reports.insert(mid.clone(), None);
                    return self.msg_reports(mid, cx);
                }
            }
            Msg::MsgReports(mid, r) => {
                let Some(slot) = self
                    .details
                    .as_mut()
                    .and_then(|x| x.msg_reports.get_mut(&mid))
                else {
                    return Task::none();
                };
                match r {
                    Ok(p) => *slot = Some(p.items),
                    Err(f) => {
                        *slot = Some(Vec::new());
                        cx.fail(&f);
                    }
                }
            }
            Msg::CloseDialog => self.details = None,
            Msg::Ask(a) => {
                let (title, body) = match &a {
                    Action::DeleteMessage { .. } => (
                        t!("admin.mod.delete_message"),
                        t!("admin.mod.delete_message_body"),
                    ),
                    Action::DeleteAll(_) => {
                        (t!("admin.mod.delete_all"), t!("admin.mod.delete_all_body"))
                    }
                    Action::Deactivate { .. } => {
                        (t!("admin.mod.deactivate"), t!("admin.mod.deactivate_body"))
                    }
                    Action::ResetAvatar(_) => (
                        t!("admin.mod.reset_avatar"),
                        t!("admin.mod.reset_avatar_body"),
                    ),
                    Action::DismissMessage(_) | Action::DismissUser { .. } => {
                        (t!("admin.mod.dismiss"), t!("admin.mod.dismiss_body"))
                    }
                };
                cx.ask(Confirm {
                    title: title.into_owned(),
                    body: body.into_owned(),
                    danger: !matches!(a, Action::DismissMessage(_) | Action::DismissUser { .. }),
                    yes_label: t!("admin.mod.confirm").into_owned(),
                    yes: Box::new(Message::Moderation(Msg::Run(a))),
                });
            }
            Msg::Run(a) => {
                self.busy = true;
                let close = matches!(a, Action::DismissUser { .. } | Action::Deactivate { .. });
                return cx.ctx.rest_task(
                    move |r| async move {
                        match a {
                            Action::DeleteMessage { rid, mid } => {
                                r.moderation_delete_message(&rid, &mid).await?;
                                r.moderation_dismiss_message_reports(&mid, None, None).await
                            }
                            Action::DismissMessage(mid) => {
                                r.moderation_dismiss_message_reports(&mid, None, None).await
                            }
                            Action::DismissUser {
                                user_id,
                                user_reports,
                            } => {
                                if user_reports {
                                    r.moderation_dismiss_user_reports(&user_id, None, None)
                                        .await
                                } else {
                                    r.moderation_dismiss_reports_of_user(&user_id, None, None)
                                        .await
                                }
                            }
                            Action::DeleteAll(user_id) => {
                                r.moderation_delete_reported_messages(&user_id, None).await
                            }
                            Action::Deactivate {
                                user_id,
                                user_reports,
                            } => {
                                if !user_reports {
                                    r.moderation_delete_reported_messages(&user_id, None)
                                        .await?;
                                }
                                // The web always relinquishes ownership here.
                                r.admin_set_active_status(&user_id, false, true).await?;
                                r.moderation_dismiss_user_reports(&user_id, None, None)
                                    .await
                            }
                            Action::ResetAvatar(user_id) => r.users_reset_avatar(&user_id).await,
                        }
                    },
                    move |r| Message::Moderation(Msg::Ran(close, r)),
                );
            }
            Msg::Ran(close, r) => {
                self.busy = false;
                if cx.check(&r) {
                    cx.ok(t!("admin.mod.done").into_owned());
                    if close {
                        self.details = None;
                    }
                    let d = self.details(cx);
                    return Task::batch([self.load(cx), d]);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let m = Message::Moderation;
        let lm = |x| Message::Moderation(Msg::List(x));
        let mut tabs = row![].spacing(SPACE_2);
        for (tab, label) in [
            (Tab::Messages, t!("admin.mod.messages")),
            (Tab::Users, t!("admin.mod.users")),
        ] {
            tabs = tabs.push(
                button(text(label).size(TEXT_SMALL))
                    .padding([6, 12])
                    .style(if tab == self.tab {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press(m(Msg::Tab(tab))),
            );
        }
        let query = match self.tab {
            Tab::Messages => &self.messages.query,
            Tab::Users => &self.users.query,
        };
        let list = match self.tab {
            Tab::Messages => {
                let rows = self
                    .messages
                    .items
                    .iter()
                    .map(|r| {
                        (
                            vec![
                                column![
                                    text(r.name.clone().unwrap_or_default())
                                        .size(TEXT_BODY)
                                        .font(font_medium()),
                                    muted(format!("@{}", r.username.clone().unwrap_or_default())),
                                ]
                                .into(),
                                small(r.message.clone().unwrap_or_default()),
                                muted(
                                    r.rooms
                                        .iter()
                                        .map(|x| {
                                            x.fname.clone().or(x.name.clone()).unwrap_or_default()
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                ),
                                muted(date(r.ts)),
                                small(r.count.to_string()),
                            ],
                            Some(m(Msg::Open(
                                r.user_id.clone(),
                                r.username.clone().unwrap_or_default(),
                            ))),
                        )
                    })
                    .collect();
                list_view(
                    &self.messages,
                    move || {
                        table(
                            vec![
                                col(t!("admin.mod.user"), 2),
                                col(t!("admin.mod.message"), 4),
                                col(t!("admin.mod.rooms"), 2),
                                col(t!("admin.mod.reported_at"), 2),
                                col(t!("admin.mod.reports"), 1),
                            ],
                            None,
                            |_| Message::Noop,
                            rows,
                        )
                    },
                    lm,
                    t!("admin.mod.empty").into_owned(),
                )
            }
            Tab::Users => {
                let rows = self
                    .users
                    .items
                    .iter()
                    .map(|r| {
                        let u = &r.reported_user;
                        (
                            vec![
                                column![
                                    text(u.name.clone().unwrap_or_default())
                                        .size(TEXT_BODY)
                                        .font(font_medium()),
                                    muted(format!("@{}", u.username.clone().unwrap_or_default())),
                                ]
                                .into(),
                                muted(
                                    u.emails
                                        .first()
                                        .map(|e| e.address.clone())
                                        .unwrap_or_default(),
                                ),
                                muted(date(r.ts)),
                                small(r.count.to_string()),
                                row![ghost_button(
                                    t!("admin.mod.dismiss"),
                                    Some(m(Msg::Ask(Action::DismissUser {
                                        user_id: u.id.clone(),
                                        user_reports: true
                                    })))
                                ),]
                                .into(),
                            ],
                            Some(m(Msg::OpenUser(
                                u.id.clone(),
                                u.username.clone().unwrap_or_default(),
                            ))),
                        )
                    })
                    .collect();
                list_view(
                    &self.users,
                    move || {
                        table(
                            vec![
                                col(t!("admin.mod.user"), 3),
                                col(t!("admin.users.email"), 3),
                                col(t!("admin.mod.reported_at"), 2),
                                col(t!("admin.mod.reports"), 1),
                                col("", 2),
                            ],
                            None,
                            |_| Message::Noop,
                            rows,
                        )
                    },
                    lm,
                    t!("admin.mod.empty").into_owned(),
                )
            }
        };
        column![
            title_row(
                t!("admin.section.moderation").into_owned(),
                vec![chip(t!("admin.beta"), ChipKind::Primary)]
            ),
            tabs,
            search_bar(
                query,
                t!("admin.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            list,
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        let d = self.details.as_ref()?;
        let m = Message::Moderation;
        let ws = v.ctx.workspace;
        let can_act = has_permission(ws, "manage-moderation-actions", None);
        let can_deactivate = has_permission(ws, "edit-other-user-active-status", None);
        let act = |ok: bool, a: Action| (ok && can_act && !self.busy).then(|| m(Msg::Ask(a)));
        let uid = d.user_id.clone();
        let mut body = column![].spacing(SPACE_3);
        match d.kind {
            DetailKind::Messages => match &d.data {
                None => body = body.push(muted(t!("admin.loading"))),
                Some(data) => {
                    for rm in &data.messages {
                        let Some(msg) = &rm.message else { continue };
                        body =
                            body.push(message_card(rm, msg, d.msg_reports.get(&msg.id), can_act));
                    }
                    if data.messages.is_empty() {
                        body = body.push(muted(t!("admin.mod.no_messages")));
                    }
                }
            },
            DetailKind::Users => match &d.user_reports {
                None => body = body.push(muted(t!("admin.loading"))),
                Some(data) => {
                    for r in &data.reports {
                        body = body.push(
                            container(report_entry(r))
                                .padding(SPACE_3)
                                .width(Length::Fill)
                                .style(style::container_card),
                        );
                    }
                    if data.reports.is_empty() {
                        body = body.push(muted(t!("admin.mod.no_reports")));
                    }
                }
            },
        }
        let user_reports = d.kind == DetailKind::Users;
        let mut actions = row![
            secondary_button(
                if user_reports {
                    t!("admin.mod.dismiss")
                } else {
                    t!("admin.mod.dismiss_all")
                },
                act(
                    true,
                    Action::DismissUser {
                        user_id: uid.clone(),
                        user_reports,
                    }
                )
            ),
            secondary_button(
                t!("admin.mod.reset_avatar"),
                act(true, Action::ResetAvatar(uid.clone()))
            ),
        ]
        .spacing(SPACE_2);
        if !user_reports {
            actions = actions.push(danger_button(
                t!("admin.mod.delete_all"),
                act(true, Action::DeleteAll(uid.clone())),
            ));
        }
        actions = actions.push(
            danger_button(
                t!("admin.mod.deactivate"),
                act(
                    can_deactivate,
                    Action::Deactivate {
                        user_id: uid.clone(),
                        user_reports,
                    },
                ),
            )
            .icon(Icon::Ban),
        );
        body = body.push(actions.wrap());
        Some(dialog(
            t!("admin.mod.details", user = d.name.clone()),
            body,
            vec![],
            Some(m(Msg::CloseDialog)),
        ))
    }
}

/// One reported message with its actions and (when opened) its reports.
fn message_card<'a>(
    rm: &'a rocketchat::models::admin::ReportedMessage,
    msg: &'a rocketchat::models::Message,
    reports: Option<&'a Option<Vec<ModerationReport>>>,
    can_act: bool,
) -> Element<'a, Message> {
    let m = Message::Moderation;
    let room = rm
        .room
        .as_ref()
        .and_then(|r| r.fname.clone().or(r.name.clone()))
        .unwrap_or_default();
    let mut c = column![
        row![
            muted(format!("#{room}")),
            container(muted(date(rm.ts)))
                .width(Length::Fill)
                .align_right(Length::Fill),
        ],
        text(crate::room::render::plain_text(msg)).size(TEXT_SMALL),
        row![
            ghost_button(
                if reports.is_some() {
                    t!("admin.mod.hide_reports")
                } else {
                    t!("admin.mod.show_reports")
                },
                Some(m(Msg::ToggleReports(msg.id.clone())))
            ),
            ghost_button(
                t!("admin.mod.dismiss"),
                can_act.then(|| m(Msg::Ask(Action::DismissMessage(msg.id.clone()))))
            ),
            ghost_button(
                t!("admin.mod.delete_message"),
                can_act.then(|| m(Msg::Ask(Action::DeleteMessage {
                    rid: msg.rid.clone(),
                    mid: msg.id.clone()
                })))
            ),
        ]
        .spacing(SPACE_1),
    ]
    .spacing(SPACE_1);
    match reports {
        None => {}
        Some(None) => c = c.push(muted(t!("admin.loading"))),
        Some(Some(list)) if list.is_empty() => c = c.push(muted(t!("admin.mod.no_reports"))),
        Some(Some(list)) => {
            let mut l = column![].spacing(SPACE_2);
            for r in list {
                l = l.push(report_entry(r));
            }
            c = c.push(container(l).padding([0.0, SPACE_3]));
        }
    }
    container(c)
        .padding(SPACE_3)
        .width(Length::Fill)
        .style(style::container_card)
        .into()
}

/// Reporter, date and reason of one report.
fn report_entry<'a>(r: &'a ModerationReport) -> Element<'a, Message> {
    let by = r
        .reported_by
        .as_ref()
        .map(|u| {
            let username = u.username.clone().unwrap_or_default();
            match u.name.as_deref().filter(|n| !n.is_empty()) {
                Some(n) => format!("{n} (@{username})"),
                None => format!("@{username}"),
            }
        })
        .unwrap_or_else(|| t!("admin.mod.unknown_reporter").into_owned());
    column![
        row![
            text(t!("admin.mod.reported_by", user = by))
                .size(TEXT_SMALL)
                .font(font_medium())
                .width(Length::Fill),
            muted(date(r.ts)),
        ]
        .spacing(SPACE_2),
        text(r.description.clone().unwrap_or_default()).size(TEXT_SMALL),
    ]
    .spacing(SPACE_1)
    .into()
}
