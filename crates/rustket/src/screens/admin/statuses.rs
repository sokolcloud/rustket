//! Custom user statuses (docs/admin.md §9): list, create/edit (always both
//! `name` and `statusType`: an omitted type clears it) and delete.

use iced::widget::{column, container, pick_list, row};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{CustomUserStatus, Paginated, UserStatus};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, TEXT_BODY};
use rustket_ui::widget::{Presence, danger_button, presence_dot, primary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, col, err_text, field, form_dialog, input, list_view, muted,
    search_bar, small, table, title_row,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind(pub UserStatus);

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&kind_label(self.0))
    }
}

const KINDS: [Kind; 4] = [
    Kind(UserStatus::Online),
    Kind(UserStatus::Away),
    Kind(UserStatus::Busy),
    Kind(UserStatus::Offline),
];

fn kind_label(s: UserStatus) -> String {
    match s {
        UserStatus::Online => t!("admin.statuses.online"),
        UserStatus::Away => t!("admin.statuses.away"),
        UserStatus::Busy => t!("admin.statuses.busy"),
        _ => t!("admin.statuses.offline"),
    }
    .into_owned()
}

fn presence(s: UserStatus) -> Presence {
    match s {
        UserStatus::Online => Presence::Online,
        UserStatus::Away => Presence::Away,
        UserStatus::Busy => Presence::Busy,
        _ => Presence::Offline,
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<CustomUserStatus>>),
    New,
    Open(Box<CustomUserStatus>),
    CloseDialog,
    Name(String),
    Kind(Kind),
    Save,
    Saved(Res<()>),
    Delete,
    DeleteNow(String),
    Deleted(Res<()>),
}

#[derive(Debug, Clone)]
pub struct Form {
    pub status: Option<CustomUserStatus>,
    pub name: String,
    pub kind: Kind,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct State {
    pub list: List<CustomUserStatus>,
    pub form: Option<Form>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            list: List::new(None),
            form: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.fetch(cx)
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let name = self.list.query.trim().to_owned();
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.custom_user_status_page(&name, &page).await },
            move |r| Message::Statuses(Msg::Loaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let form = self.form.as_mut();
        match m {
            Msg::List(m) => {
                return match self.list.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Statuses(Msg::List(m))),
                    Step::Load => self.fetch(cx),
                };
            }
            Msg::Loaded(req, r) => self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))),
            Msg::New => {
                self.form = Some(Form {
                    status: None,
                    name: String::new(),
                    kind: Kind(UserStatus::Online),
                    busy: false,
                    error: None,
                });
            }
            Msg::Open(s) => {
                self.form = Some(Form {
                    name: s.name.clone(),
                    kind: Kind(s.status_type),
                    status: Some(*s),
                    busy: false,
                    error: None,
                });
            }
            Msg::CloseDialog => self.form = None,
            Msg::Name(n) => {
                if let Some(f) = form {
                    f.name = n;
                    f.error = None;
                }
            }
            Msg::Kind(k) => {
                if let Some(f) = form {
                    f.kind = k;
                }
            }
            Msg::Save => {
                let Some(f) = form else {
                    return Task::none();
                };
                let name = f.name.trim().to_owned();
                if name.is_empty() {
                    f.error = Some(t!("admin.name_required").into_owned());
                    return Task::none();
                }
                if name.chars().any(|c| "<>&\"'".contains(c)) {
                    f.error = Some(t!("admin.statuses.invalid_name").into_owned());
                    return Task::none();
                }
                f.busy = true;
                let kind = f.kind.0;
                let id = f.status.as_ref().map(|s| s.id.clone());
                return cx.ctx.rest_task(
                    move |r| async move {
                        match id {
                            Some(id) => r.custom_user_status_update(&id, &name, kind).await,
                            None => r.custom_user_status_create(&name, kind).await,
                        }
                        .map(|_| ())
                    },
                    |r| Message::Statuses(Msg::Saved(r)),
                );
            }
            Msg::Saved(r) => match r {
                Ok(()) => {
                    cx.ok(t!("admin.statuses.saved").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
                Err(e) => {
                    let text = err_text(&e, cx.i18n);
                    if let Some(f) = form {
                        f.busy = false;
                        f.error = Some(text);
                    }
                }
            },
            Msg::Delete => {
                if let Some(s) = form.and_then(|f| f.status.clone()) {
                    cx.ask(Confirm::danger(
                        t!("admin.statuses.delete_title").into_owned(),
                        t!("admin.statuses.delete_body", name = s.name).into_owned(),
                        Message::Statuses(Msg::DeleteNow(s.id)),
                    ));
                }
            }
            Msg::DeleteNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.custom_user_status_delete(&id).await },
                    |r| Message::Statuses(Msg::Deleted(r)),
                );
            }
            Msg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.statuses.deleted").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let lm = |m| Message::Statuses(Msg::List(m));
        let rows = self
            .list
            .items
            .iter()
            .map(|s| {
                (
                    vec![
                        small(s.name.clone()),
                        row![
                            presence_dot(presence(s.status_type), 10.0),
                            muted(kind_label(s.status_type))
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center)
                        .into(),
                    ],
                    Some(Message::Statuses(Msg::Open(Box::new(s.clone())))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        col(t!("admin.statuses.name"), 3),
                        col(t!("admin.statuses.presence"), 2),
                    ],
                    None,
                    |_| Message::Noop,
                    rows,
                )
            },
            lm,
            t!("admin.statuses.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.user_status").into_owned(),
                vec![
                    primary_button(t!("admin.statuses.new"), Some(Message::Statuses(Msg::New)))
                        .icon(Icon::Plus)
                        .into()
                ]
            ),
            search_bar(
                &self.list.query,
                t!("admin.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            list
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let body = column![
            field(
                t!("admin.statuses.name").into_owned(),
                None,
                input(&f.name, Some(|s| Message::Statuses(Msg::Name(s))))
            ),
            field(
                t!("admin.statuses.presence").into_owned(),
                None,
                pick_list(KINDS, Some(f.kind), |k| Message::Statuses(Msg::Kind(k)))
                    .text_size(TEXT_BODY)
                    .width(Length::Fill)
                    .style(style::pick_list)
                    .menu_style(style::menu)
            ),
        ]
        .spacing(SPACE_3);
        let extra = if f.status.is_some() {
            vec![
                danger_button(t!("admin.delete"), Some(Message::Statuses(Msg::Delete)))
                    .icon(Icon::Trash2)
                    .into(),
            ]
        } else {
            vec![]
        };
        Some(form_dialog(
            if f.status.is_some() {
                t!("admin.statuses.edit").into_owned()
            } else {
                t!("admin.statuses.new").into_owned()
            },
            container(body).width(Length::Fill).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::Statuses(Msg::Save)),
            Message::Statuses(Msg::CloseDialog),
            extra,
        ))
    }
}
