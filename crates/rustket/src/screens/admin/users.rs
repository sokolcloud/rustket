//! Users (docs/admin.md §4): the paged list with tabs, search and sort, the
//! details dialog with its actions, the create/edit form, and the last-owner
//! confirmation (`confirmRelinquish`).

use iced::widget::{Column, button, checkbox, column, container, row, text};
use iced::{Alignment, Element, Task};
use rocketchat::models::Paginated;
use rocketchat::models::admin::{AdminUser, LastOwnerDetails, NewUser, UserUpdate, UsersFilter};
use rocketchat::rest::UserLookup;
use rust_i18n::t;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{
    ChipKind, chip, danger_button, dialog, ghost_button, primary_button, secondary_button,
};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, card, col, date, err_text, field, form_dialog, input, kv,
    list_view, muted, search_bar, small, sortable, switch, table, title_row,
};
use super::guard::{Done, Op};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    All,
    Pending,
    Active,
    Deactivated,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::All, Tab::Pending, Tab::Active, Tab::Deactivated];

    fn filter(self) -> UsersFilter {
        match self {
            Tab::All => UsersFilter::all(),
            Tab::Pending => UsersFilter::pending(),
            Tab::Active => UsersFilter::active(),
            Tab::Deactivated => UsersFilter::deactivated(),
        }
    }

    fn label(self) -> String {
        match self {
            Tab::All => t!("admin.users.all"),
            Tab::Pending => t!("admin.users.pending"),
            Tab::Active => t!("admin.users.active"),
            Tab::Deactivated => t!("admin.users.deactivated"),
        }
        .into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F {
    Name,
    Username,
    Email,
    StatusText,
    Bio,
    Nickname,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Verified,
    RandomPassword,
    RequirePasswordChange,
    JoinDefault,
    Welcome,
}

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<AdminUser>>),
    Tab(Tab),
    Pending(Res<Paginated<AdminUser>>),
    Smtp(Res<bool>),
    Open(String),
    Info(Res<AdminUser>),
    CloseDialog,
    New,
    Edit,
    Field(F, String),
    Flag(Flag, bool),
    Role(String, bool),
    Save,
    Created(Res<AdminUser>),
    SetActive(String, bool, bool),
    ActiveSet(String, bool, Res<()>),
    Delete(String),
    DeleteNow(String, bool),
    Deleted(String, Res<()>),
    ToggleAdmin(String, bool),
    AdminToggled(Res<()>),
    ResetTotp(String),
    ResetE2e(String),
    DoReset(Op),
    Welcome(String),
    WelcomeSent(Res<()>),
}

/// The create/edit form.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub user: Option<AdminUser>,
    pub name: String,
    pub username: String,
    pub email: String,
    pub verified: bool,
    pub status_text: String,
    pub bio: String,
    pub nickname: String,
    pub password: String,
    pub random_password: bool,
    pub require_password_change: bool,
    pub join_default: bool,
    pub welcome: bool,
    pub roles: Vec<String>,
    pub busy: bool,
    pub error: Option<String>,
}

impl Form {
    pub fn new() -> Self {
        Self {
            roles: vec!["user".into()],
            join_default: true,
            verified: false,
            ..Self::default()
        }
    }

    pub fn edit(u: &AdminUser) -> Self {
        Self {
            name: u.name.clone().unwrap_or_default(),
            username: u.username.clone().unwrap_or_default(),
            email: u
                .primary_email()
                .map(|e| e.address.clone())
                .unwrap_or_default(),
            verified: u.primary_email().is_some_and(|e| e.verified),
            status_text: u.status_text.clone().unwrap_or_default(),
            bio: u.bio.clone().unwrap_or_default(),
            nickname: u.nickname.clone().unwrap_or_default(),
            require_password_change: u.require_password_change.unwrap_or(false),
            roles: u.roles.clone(),
            user: Some(u.clone()),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Option<String> {
        if self.name.trim().is_empty() || self.username.trim().is_empty() {
            return Some(t!("admin.users.required").into_owned());
        }
        if !self.email.contains('@') {
            return Some(t!("admin.users.bad_email").into_owned());
        }
        if self.user.is_none() && !self.random_password && self.password.is_empty() {
            return Some(t!("admin.users.password_required").into_owned());
        }
        None
    }

    pub fn new_user(&self) -> NewUser {
        NewUser {
            email: self.email.trim().to_owned(),
            name: self.name.trim().to_owned(),
            username: self.username.trim().to_owned(),
            // Required by the schema even with a random password.
            password: if self.random_password {
                String::new()
            } else {
                self.password.clone()
            },
            verified: Some(self.verified),
            status_text: non_empty(&self.status_text),
            bio: non_empty(&self.bio),
            nickname: non_empty(&self.nickname),
            roles: Some(self.roles.clone()),
            join_default_channels: Some(self.join_default),
            require_password_change: Some(self.require_password_change),
            set_random_password: Some(self.random_password),
            send_welcome_email: Some(self.welcome),
            ..NewUser::default()
        }
    }

    /// Only the changed fields (an editor without `assign-admin-role` can then
    /// edit an admin's other fields).
    pub fn update(&self) -> UserUpdate {
        let Some(u) = &self.user else {
            return UserUpdate::default();
        };
        let changed = |new: &str, old: Option<&str>| {
            (new.trim() != old.unwrap_or("").trim()).then(|| new.trim().to_owned())
        };
        let old_email = u.primary_email();
        let mut up = UserUpdate {
            name: changed(&self.name, u.name.as_deref()),
            username: changed(&self.username, u.username.as_deref()),
            email: changed(&self.email, old_email.map(|e| e.address.as_str())),
            status_text: changed(&self.status_text, u.status_text.as_deref()),
            bio: changed(&self.bio, u.bio.as_deref()),
            nickname: changed(&self.nickname, u.nickname.as_deref()),
            ..UserUpdate::default()
        };
        if self.verified != old_email.is_some_and(|e| e.verified) {
            up.verified = Some(self.verified);
        }
        if self.random_password {
            up.set_random_password = Some(true);
        } else if !self.password.is_empty() {
            up.password = Some(self.password.clone());
        }
        if self.require_password_change != u.require_password_change.unwrap_or(false) {
            up.require_password_change = Some(self.require_password_change);
        }
        let mut a = self.roles.clone();
        let mut b = u.roles.clone();
        a.sort();
        b.sort();
        if a != b {
            up.roles = Some(self.roles.clone());
        }
        up
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

pub enum Dialog {
    Details(Box<AdminUser>),
    Form(Box<Form>),
}

pub struct State {
    pub tab: Tab,
    pub list: List<AdminUser>,
    pub pending_count: Option<u64>,
    pub smtp: bool,
    pub dialog: Option<Dialog>,
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
            tab: Tab::All,
            list: List::new(Some(("name", true))),
            pending_count: None,
            smtp: true,
            dialog: None,
            busy: false,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let pending = cx.ctx.rest_task(
            |r| async move {
                r.admin_users_list(
                    &UsersFilter::pending(),
                    &rocketchat::rest::admin::Page::new(0, 1),
                )
                .await
            },
            |r| Message::Users(Msg::Pending(r)),
        );
        let smtp = cx.ctx.rest_task(
            |r| async move { r.smtp_check().await },
            |r| Message::Users(Msg::Smtp(r)),
        );
        Task::batch([self.fetch(cx), pending, smtp])
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let mut filter = self.tab.filter();
        filter.search_term = self.list.search_term();
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.admin_users_list(&filter, &page).await },
            move |r| Message::Users(Msg::Loaded(req, r)),
        )
    }

    fn info(&self, cx: &mut Cx<'_, '_>, id: String) -> Task<Message> {
        cx.ctx.rest_task(
            move |r| async move { r.admin_user_info(UserLookup::Id(&id)).await },
            |r| Message::Users(Msg::Info(r)),
        )
    }

    fn form(&mut self) -> Option<&mut Form> {
        match &mut self.dialog {
            Some(Dialog::Form(f)) => Some(f),
            _ => None,
        }
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::List(m) => match self.list.update(m) {
                Step::None => Task::none(),
                Step::Wait(t) => t.map(|m| Message::Users(Msg::List(m))),
                Step::Load => self.fetch(cx),
            },
            Msg::Loaded(req, r) => {
                self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n)));
                Task::none()
            }
            Msg::Tab(tab) => {
                self.tab = tab;
                // Pending users are sorted by `active`, the others by name (web).
                self.list.sort = Some(if tab == Tab::Pending {
                    ("active", true)
                } else {
                    ("name", true)
                });
                self.list.offset = 0;
                self.fetch(cx)
            }
            Msg::Pending(r) => {
                self.pending_count = r.ok().map(|p| p.total);
                Task::none()
            }
            Msg::Smtp(r) => {
                self.smtp = r.unwrap_or(true);
                Task::none()
            }
            Msg::Open(id) => self.info(cx, id),
            Msg::Info(r) => {
                match r {
                    Ok(u) => {
                        // Refresh an open form's base too.
                        if let Some(f) = self.form() {
                            f.user = Some(u.clone());
                        } else {
                            self.dialog = Some(Dialog::Details(Box::new(u)));
                        }
                    }
                    Err(f) => cx.fail(&f),
                }
                Task::none()
            }
            Msg::CloseDialog => {
                self.dialog = None;
                Task::none()
            }
            Msg::New => {
                self.dialog = Some(Dialog::Form(Box::new(Form::new())));
                Task::none()
            }
            Msg::Edit => {
                if let Some(Dialog::Details(u)) = &self.dialog {
                    self.dialog = Some(Dialog::Form(Box::new(Form::edit(u))));
                }
                Task::none()
            }
            Msg::Field(field, v) => {
                if let Some(f) = self.form() {
                    f.error = None;
                    match field {
                        F::Name => f.name = v,
                        F::Username => f.username = v,
                        F::Email => f.email = v,
                        F::StatusText => f.status_text = v,
                        F::Bio => f.bio = v,
                        F::Nickname => f.nickname = v,
                        F::Password => f.password = v,
                    }
                }
                Task::none()
            }
            Msg::Flag(flag, v) => {
                if let Some(f) = self.form() {
                    match flag {
                        Flag::Verified => f.verified = v,
                        Flag::RandomPassword => f.random_password = v,
                        Flag::RequirePasswordChange => f.require_password_change = v,
                        Flag::JoinDefault => f.join_default = v,
                        Flag::Welcome => f.welcome = v,
                    }
                }
                Task::none()
            }
            Msg::Role(role, on) => {
                if let Some(f) = self.form() {
                    f.roles.retain(|r| *r != role);
                    if on {
                        f.roles.push(role);
                    }
                }
                Task::none()
            }
            Msg::Save => {
                let Some(f) = self.form() else {
                    return Task::none();
                };
                if let Some(e) = f.validate() {
                    f.error = Some(e);
                    return Task::none();
                }
                f.busy = true;
                match &f.user {
                    None => {
                        let nu = f.new_user();
                        cx.ctx.rest_task(
                            move |r| async move { r.admin_user_create(&nu).await },
                            |r| Message::Users(Msg::Created(r)),
                        )
                    }
                    Some(u) => {
                        let data = f.update();
                        if data.is_empty() {
                            self.dialog = None;
                            return Task::none();
                        }
                        cx.guarded(Op::UpdateUser {
                            id: u.id.clone(),
                            data: Box::new(data),
                            relinquish: false,
                        })
                    }
                }
            }
            Msg::Created(r) => {
                match r {
                    Ok(u) => {
                        cx.ok(t!(
                            "admin.users.created",
                            name = u.username.clone().unwrap_or_default()
                        )
                        .into_owned());
                        self.dialog = None;
                        return self.fetch(cx);
                    }
                    Err(e) => {
                        let text = err_text(&e, cx.i18n);
                        if let Some(f) = self.form() {
                            f.busy = false;
                            f.error = Some(text);
                        }
                    }
                }
                Task::none()
            }
            Msg::SetActive(id, active, relinquish) => {
                self.busy = true;
                let id2 = id.clone();
                cx.ctx.rest_task(
                    move |r| async move {
                        r.admin_set_active_status(&id, active, relinquish)
                            .await
                            .map(|_| ())
                    },
                    move |r| Message::Users(Msg::ActiveSet(id2, active, r)),
                )
            }
            Msg::ActiveSet(id, active, r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        cx.ok(if active {
                            t!("admin.users.activated").into_owned()
                        } else {
                            t!("admin.users.deactivated_done").into_owned()
                        });
                        let refresh = self.reopen(cx);
                        return Task::batch([self.fetch(cx), refresh]);
                    }
                    Err(f) => {
                        if let Some(d) = LastOwnerDetails::from_error(f.error()) {
                            cx.ask(last_owner(
                                d,
                                Message::Users(Msg::SetActive(id, active, true)),
                            ));
                        } else {
                            cx.fail(&f);
                        }
                    }
                }
                Task::none()
            }
            Msg::Delete(id) => {
                let erasure = cx
                    .ctx
                    .workspace
                    .settings()
                    .str("Message_ErasureType")
                    .unwrap_or("Delete")
                    .to_owned();
                let body = cx
                    .i18n
                    .t(&format!("Delete_User_Warning_{erasure}"))
                    .map(super::common::strip_tags)
                    .unwrap_or_else(|| t!("admin.users.delete_body").into_owned());
                cx.ask(Confirm::danger(
                    t!("admin.users.delete_title").into_owned(),
                    body,
                    Message::Users(Msg::DeleteNow(id, false)),
                ));
                Task::none()
            }
            Msg::DeleteNow(id, relinquish) => {
                self.busy = true;
                let id2 = id.clone();
                cx.ctx.rest_task(
                    move |r| async move { r.admin_user_delete(&id, relinquish).await.map(|_| ()) },
                    move |r| Message::Users(Msg::Deleted(id2, r)),
                )
            }
            Msg::Deleted(id, r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        cx.ok(t!("admin.users.deleted").into_owned());
                        self.dialog = None;
                        return self.fetch(cx);
                    }
                    Err(f) => match LastOwnerDetails::from_error(f.error()) {
                        Some(d) => cx.ask(last_owner(d, Message::Users(Msg::DeleteNow(id, true)))),
                        None => cx.fail(&f),
                    },
                }
                Task::none()
            }
            Msg::ToggleAdmin(username, make) => cx.ctx.rest_task(
                move |r| async move {
                    if make {
                        r.roles_add_user("admin", &username, None).await.map(|_| ())
                    } else {
                        r.roles_remove_user("admin", &username, None)
                            .await
                            .map(|_| ())
                    }
                },
                |r| Message::Users(Msg::AdminToggled(r)),
            ),
            Msg::AdminToggled(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.users.roles_updated").into_owned());
                    let refresh = self.reopen(cx);
                    return Task::batch([self.fetch(cx), refresh]);
                }
                Task::none()
            }
            Msg::ResetTotp(id) => {
                cx.ask(Confirm::ask(
                    t!("admin.users.reset_totp").into_owned(),
                    t!("admin.users.reset_totp_body").into_owned(),
                    t!("admin.users.reset").into_owned(),
                    Message::Users(Msg::DoReset(Op::ResetTotp(id))),
                ));
                Task::none()
            }
            Msg::ResetE2e(id) => {
                cx.ask(Confirm::ask(
                    t!("admin.users.reset_e2e").into_owned(),
                    t!("admin.users.reset_e2e_body").into_owned(),
                    t!("admin.users.reset").into_owned(),
                    Message::Users(Msg::DoReset(Op::ResetE2e(id))),
                ));
                Task::none()
            }
            Msg::DoReset(op) => cx.guarded(op),
            Msg::Welcome(email) => cx.ctx.rest_task(
                move |r| async move { r.admin_send_welcome_email(&email).await },
                |r| Message::Users(Msg::WelcomeSent(r)),
            ),
            Msg::WelcomeSent(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.users.welcome_sent").into_owned());
                }
                Task::none()
            }
        }
    }

    fn open_id(&self) -> Option<String> {
        match &self.dialog {
            Some(Dialog::Details(u)) => Some(u.id.clone()),
            Some(Dialog::Form(f)) => f.user.as_ref().map(|u| u.id.clone()),
            None => None,
        }
    }

    fn reopen(&self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match self.open_id() {
            Some(id) => self.info(cx, id),
            None => Task::none(),
        }
    }

    pub fn on_guarded(&mut self, op: &Op, r: &Res<Done>, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match (op, r) {
            (Op::UpdateUser { .. }, Ok(Done::User(u))) => {
                cx.ok(t!("admin.users.saved").into_owned());
                self.dialog = Some(Dialog::Details(u.clone()));
                self.fetch(cx)
            }
            (Op::UpdateUser { .. }, Err(e)) => {
                let text = err_text(e, cx.i18n);
                match self.form() {
                    Some(f) => {
                        f.busy = false;
                        f.error = Some(text);
                    }
                    None => cx.fail(e),
                }
                Task::none()
            }
            (Op::ResetTotp(_), Ok(_)) => {
                cx.ok(t!("admin.users.totp_reset").into_owned());
                Task::none()
            }
            (Op::ResetE2e(_), Ok(_)) => {
                cx.ok(t!("admin.users.e2e_reset").into_owned());
                Task::none()
            }
            (_, r) => {
                cx.check(r);
                Task::none()
            }
        }
    }

    pub fn on_guard_cancelled(&mut self, op: &Op) {
        if let (Op::UpdateUser { .. }, Some(f)) = (op, self.form()) {
            f.busy = false;
        }
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let ws = v.ctx.workspace;
        let can_create = rustket_core::admin::can_create_users(ws, v.license);
        let lm = |m| Message::Users(Msg::List(m));
        let mut tabs = row![].spacing(SPACE_2);
        for tab in Tab::ALL {
            let mut label = tab.label();
            if tab == Tab::Pending
                && let Some(n) = self.pending_count.filter(|n| *n > 0)
            {
                label = format!("{label} ({n})");
            }
            tabs = tabs.push(
                button(text(label).size(TEXT_SMALL))
                    .padding([6, 12])
                    .style(if tab == self.tab {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press(Message::Users(Msg::Tab(tab))),
            );
        }
        let rows = self
            .list
            .items
            .iter()
            .map(|u| {
                let mut roles = row![].spacing(SPACE_1);
                for r in u.roles.iter().take(3) {
                    roles = roles.push(chip(r.clone(), ChipKind::Neutral));
                }
                let status: Element<'a, Message> = if u.is_active() {
                    muted(u.status.map(|s| s.as_str().to_owned()).unwrap_or_default())
                } else if u.reason.is_some() || self.tab == Tab::Pending {
                    row![
                        chip(t!("admin.users.pending_chip"), ChipKind::Warning),
                        ghost_button(
                            t!("admin.users.activate"),
                            Some(Message::Users(Msg::SetActive(u.id.clone(), true, false)))
                        ),
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center)
                    .into()
                } else {
                    chip(t!("admin.users.deactivated_chip"), ChipKind::Danger)
                };
                (
                    vec![
                        column![
                            text(u.name.clone().unwrap_or_default())
                                .size(TEXT_BODY)
                                .font(FONT_MEDIUM),
                            muted(format!("@{}", u.username.clone().unwrap_or_default())),
                        ]
                        .into(),
                        small(
                            u.primary_email()
                                .map(|e| e.address.clone())
                                .unwrap_or_default(),
                        ),
                        roles.into(),
                        status,
                    ],
                    Some(Message::Users(Msg::Open(u.id.clone()))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        sortable(t!("admin.users.name"), 3, "name"),
                        sortable(t!("admin.users.email"), 3, "emails.address"),
                        col(t!("admin.users.roles"), 3),
                        sortable(t!("admin.users.status"), 2, "status"),
                    ],
                    self.list.sort,
                    |f| Message::Users(Msg::List(ListMsg::Sort(f))),
                    rows,
                )
            },
            lm,
            t!("admin.users.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.users").into_owned(),
                vec![
                    primary_button(
                        t!("admin.users.new"),
                        can_create.then_some(Message::Users(Msg::New))
                    )
                    .icon(Icon::UserPlus)
                    .into()
                ]
            ),
            tabs,
            search_bar(
                &self.list.query,
                t!("admin.users.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            list,
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        match self.dialog.as_ref()? {
            Dialog::Details(u) => Some(self.details(u, v)),
            Dialog::Form(f) => Some(form_view(f, v, self.smtp)),
        }
    }

    fn details<'a>(&'a self, u: &'a AdminUser, v: &V<'a>) -> Element<'a, Message> {
        let ws = v.ctx.workspace;
        let can = |p: &str| has_permission(ws, p, None);
        let me = ws.me().map(|m| m.id.clone()).unwrap_or_default();
        let id = u.id.clone();
        let username = u.username.clone().unwrap_or_default();
        let is_admin = u.roles.iter().any(|r| r == "admin");
        let mut info = column![
            kv(
                t!("admin.users.username").into_owned(),
                format!("@{username}")
            ),
            kv(
                t!("admin.users.email").into_owned(),
                u.emails
                    .iter()
                    .map(|e| {
                        if e.verified {
                            format!("{} ✓", e.address)
                        } else {
                            e.address.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            kv(t!("admin.users.roles").into_owned(), u.roles.join(", ")),
            kv(
                t!("admin.users.status").into_owned(),
                if u.is_active() {
                    u.status.map(|s| s.as_str().to_owned()).unwrap_or_default()
                } else {
                    t!("admin.users.deactivated_chip").into_owned()
                }
            ),
            kv(
                t!("admin.users.last_login").into_owned(),
                date(u.last_login)
            ),
            kv(
                t!("admin.users.created_at").into_owned(),
                date(u.created_at)
            ),
        ]
        .spacing(SPACE_2);
        for (k, val) in [
            (t!("admin.users.nickname"), &u.nickname),
            (t!("admin.users.bio"), &u.bio),
            (t!("admin.users.status_text"), &u.status_text),
        ] {
            if let Some(val) = val.as_ref().filter(|s| !s.is_empty()) {
                info = info.push(kv(k.into_owned(), val.clone()));
            }
        }
        if !u.rooms.is_empty() {
            info = info.push(kv(
                t!("admin.users.rooms").into_owned(),
                u.rooms
                    .iter()
                    .filter_map(|r| r.name.clone())
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        let m = |x| Some(Message::Users(x));
        let mut actions: Vec<Element<'a, Message>> = Vec::new();
        if can("edit-other-user-info") && u.federated != Some(true) {
            actions.push(
                secondary_button(t!("admin.edit"), m(Msg::Edit))
                    .icon(Icon::Pencil)
                    .into(),
            );
        }
        if can("edit-other-user-active-status") && id != me {
            actions.push(
                secondary_button(
                    if u.is_active() {
                        t!("admin.users.deactivate")
                    } else {
                        t!("admin.users.activate")
                    },
                    (!self.busy)
                        .then(|| Message::Users(Msg::SetActive(id.clone(), !u.is_active(), false))),
                )
                .into(),
            );
        }
        if can("assign-admin-role") && can("access-permissions") && id != me {
            actions.push(
                secondary_button(
                    if is_admin {
                        t!("admin.users.remove_admin")
                    } else {
                        t!("admin.users.make_admin")
                    },
                    m(Msg::ToggleAdmin(username.clone(), !is_admin)),
                )
                .into(),
            );
        }
        let mut more: Vec<Element<'a, Message>> = Vec::new();
        if can("edit-other-user-totp")
            && ws
                .settings()
                .bool_or("Accounts_TwoFactorAuthentication_Enabled", true)
        {
            more.push(
                ghost_button(t!("admin.users.reset_totp"), m(Msg::ResetTotp(id.clone()))).into(),
            );
        }
        if can("edit-other-user-e2ee") {
            more.push(
                ghost_button(t!("admin.users.reset_e2e"), m(Msg::ResetE2e(id.clone()))).into(),
            );
        }
        if can("send-mail")
            && let Some(e) = u.primary_email()
        {
            more.push(
                ghost_button(
                    t!("admin.users.send_welcome"),
                    m(Msg::Welcome(e.address.clone())),
                )
                .into(),
            );
        }
        if can("delete-user") && id != me {
            more.push(
                danger_button(
                    t!("admin.delete"),
                    (!self.busy).then(|| Message::Users(Msg::Delete(id.clone()))),
                )
                .icon(Icon::Trash2)
                .into(),
            );
        }
        let body = column![
            card(u.name.clone().unwrap_or_default(), info),
            row(actions).spacing(SPACE_2).wrap(),
            row(more).spacing(SPACE_2).wrap(),
        ]
        .spacing(SPACE_3);
        dialog(
            t!("admin.users.details"),
            body,
            vec![],
            Some(Message::Users(Msg::CloseDialog)),
        )
    }
}

fn last_owner(d: LastOwnerDetails, retry: Message) -> Confirm {
    let mut body = t!("admin.users.last_owner_body").into_owned();
    if !d.should_change_owner.is_empty() {
        body.push_str("\n\n");
        body.push_str(&t!(
            "admin.users.new_owner",
            rooms = d.should_change_owner.join(", ")
        ));
    }
    if !d.should_be_removed.is_empty() {
        body.push_str("\n\n");
        body.push_str(&t!(
            "admin.users.rooms_removed",
            rooms = d.should_be_removed.join(", ")
        ));
    }
    Confirm {
        title: t!("admin.users.last_owner_title").into_owned(),
        body,
        danger: true,
        yes_label: t!("admin.users.continue").into_owned(),
        yes: Box::new(retry),
    }
}

fn form_view<'a>(f: &'a Form, v: &V<'a>, smtp: bool) -> Element<'a, Message> {
    let fm = |field: F| move |s| Message::Users(Msg::Field(field, s));
    let flag = |fl: Flag| Some(move |b| Message::Users(Msg::Flag(fl, b)));
    let creating = f.user.is_none();
    let mut col: Column<'a, Message> = column![
        field(
            t!("admin.users.name").into_owned(),
            None,
            input(&f.name, Some(fm(F::Name)))
        ),
        field(
            t!("admin.users.username").into_owned(),
            None,
            input(&f.username, Some(fm(F::Username)))
        ),
        field(
            t!("admin.users.email").into_owned(),
            None,
            input(&f.email, Some(fm(F::Email)))
        ),
        switch(
            t!("admin.users.verified").into_owned(),
            f.verified,
            flag(Flag::Verified)
        ),
        field(
            t!("admin.users.status_text").into_owned(),
            None,
            input(&f.status_text, Some(fm(F::StatusText)))
        ),
        field(
            t!("admin.users.bio").into_owned(),
            None,
            input(&f.bio, Some(fm(F::Bio)))
        ),
        field(
            t!("admin.users.nickname").into_owned(),
            None,
            input(&f.nickname, Some(fm(F::Nickname)))
        ),
        switch(
            t!("admin.users.random_password").into_owned(),
            f.random_password,
            flag(Flag::RandomPassword)
        ),
    ]
    .spacing(SPACE_3);
    if !f.random_password {
        col = col.push(field(
            t!("admin.password").into_owned(),
            (!creating).then(|| t!("admin.users.password_keep").into_owned()),
            input(&f.password, Some(fm(F::Password))).secure(true),
        ));
    }
    col = col.push(switch(
        t!("admin.users.require_change").into_owned(),
        f.require_password_change,
        flag(Flag::RequirePasswordChange),
    ));
    let mut roles: Vec<&rocketchat::models::Role> = v
        .ctx
        .workspace
        .roles()
        .values()
        .filter(|r| r.scope.as_deref().unwrap_or("Users") == "Users")
        .collect();
    roles.sort_by(|a, b| a.name.cmp(&b.name));
    let mut role_col = row![].spacing(SPACE_3);
    for r in roles {
        let on = f.roles.contains(&r.id);
        let id = r.id.clone();
        role_col = role_col.push(
            checkbox(on)
                .label(r.name.clone())
                .size(16)
                .text_size(TEXT_SMALL)
                .style(style::checkbox)
                .on_toggle(move |b| Message::Users(Msg::Role(id.clone(), b))),
        );
    }
    col = col.push(field(
        t!("admin.users.roles").into_owned(),
        None,
        role_col.wrap(),
    ));
    if creating {
        col = col.push(switch(
            t!("admin.users.join_default").into_owned(),
            f.join_default,
            flag(Flag::JoinDefault),
        ));
        col = col.push(switch(
            t!("admin.users.welcome").into_owned(),
            f.welcome && smtp,
            smtp.then_some(move |b| Message::Users(Msg::Flag(Flag::Welcome, b))),
        ));
        if !smtp {
            col = col.push(muted(t!("admin.users.no_smtp")));
        }
    }
    form_dialog(
        if creating {
            t!("admin.users.new").into_owned()
        } else {
            t!("admin.users.edit_title").into_owned()
        },
        container(col).padding([0.0, SPACE_1]).into(),
        f.error.as_deref(),
        f.busy,
        Some(Message::Users(Msg::Save)),
        Message::Users(Msg::CloseDialog),
        vec![],
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::Email;

    fn user() -> AdminUser {
        AdminUser {
            id: "u1".into(),
            username: Some("bob".into()),
            name: Some("Bob".into()),
            emails: vec![Email {
                address: "bob@x.io".into(),
                verified: true,
            }],
            roles: vec!["user".into(), "admin".into()],
            ..AdminUser::default()
        }
    }

    #[test]
    fn update_sends_only_changed_fields() {
        let mut f = Form::edit(&user());
        assert!(f.update().is_empty());
        f.name = "Bobby".into();
        f.roles = vec!["admin".into(), "user".into()];
        let up = f.update();
        assert_eq!(up.name.as_deref(), Some("Bobby"));
        // Same roles in another order: not sent (would need assign-admin-role).
        assert!(up.roles.is_none());
        assert!(up.email.is_none() && up.password.is_none());
        f.password = "pw".into();
        assert_eq!(f.update().password.as_deref(), Some("pw"));
        f.random_password = true;
        let up = f.update();
        assert_eq!(up.set_random_password, Some(true));
        assert!(up.password.is_none());
    }

    #[test]
    fn create_validation() {
        let mut f = Form::new();
        assert!(f.validate().is_some());
        f.name = "A".into();
        f.username = "a".into();
        f.email = "a@b.c".into();
        assert!(f.validate().is_some(), "password required");
        f.random_password = true;
        assert!(f.validate().is_none());
        let nu = f.new_user();
        assert_eq!(nu.password, "");
        assert_eq!(nu.set_random_password, Some(true));
        assert_eq!(nu.roles.as_deref(), Some(&["user".to_owned()][..]));
    }
}
