//! Permissions and roles (docs/admin.md §13): the permission matrix (ordinary
//! and setting permissions, filter, 25 rows per page, atomic add/remove of a
//! role), the roles list with the role editor (protected roles keep name and
//! scope) and the users in a role.

use iced::widget::{button, checkbox, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::admin::{PermissionDef, RoleParams, UserInRole};
use rocketchat::models::{Paginated, Permission, Role};
use rocketchat::session::ChangeKind;
use rust_i18n::t;
use rustket_core::admin::server_i18n::ServerI18n;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL, font_semibold,
};
use rustket_ui::widget::{
    BannerKind, above, banner, dialog, icon_button, primary_button, secondary_button,
};

use super::common::{
    Confirm, Cx, List, ListMsg, Step, card, err_text, field, input, kv, muted, search_bar, switch,
    title_row,
};
use super::{Message, V};
use crate::error::Res;

const ROWS: usize = 25;
const LABEL_W: f32 = 300.0;
const CELL_W: f32 = 84.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Permissions,
    Settings,
    Roles,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Vec<PermissionDef>>),
    Tab(Tab),
    Filter(String),
    Page(usize),
    Toggle(String, String, bool),
    Toggled(String, String, bool, Res<()>),
    OpenRole(String),
    CloseDialog,
    Description(String),
    Mandatory2fa(bool),
    SaveRole,
    RoleSaved(Res<Role>),
    Users(ListMsg),
    UsersLoaded(u64, Res<Paginated<UserInRole>>),
    AddUser(String),
    AddUserNow,
    RemoveUser(String),
    RemoveUserNow(String),
    UserChanged(Res<()>),
    Reload,
}

pub struct RoleEditor {
    pub role: Role,
    pub description: String,
    pub mandatory2fa: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub users: List<UserInRole>,
    pub add: String,
}

pub struct State {
    pub tab: Tab,
    pub all: Vec<PermissionDef>,
    pub loaded: bool,
    pub error: Option<String>,
    pub filter: String,
    pub page: usize,
    /// Toggles in flight (permission, role).
    pub pending: Vec<(String, String)>,
    pub role: Option<RoleEditor>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

/// Every word of `filter` is a case-insensitive substring of the id or of the
/// joined translated labels (`mapPermissionKeys.ts`).
pub fn matches(p: &PermissionDef, filter: &str, i18n: &ServerI18n) -> bool {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return true;
    }
    let mut hay = p.id.to_lowercase();
    for k in [
        p.group.as_deref(),
        p.section.as_deref(),
        p.setting_id.as_deref(),
        Some(p.id.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        hay.push(' ');
        hay.push_str(&i18n.t_or_key(k).to_lowercase());
    }
    words.iter().all(|w| hay.contains(w.as_str()))
}

pub fn label(p: &PermissionDef, i18n: &ServerI18n) -> String {
    match &p.setting_id {
        Some(s) if p.is_setting_permission() => i18n.t_or_key(s).to_owned(),
        _ => i18n.t_or_key(&p.id).to_owned(),
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            tab: Tab::Permissions,
            all: Vec::new(),
            loaded: false,
            error: None,
            filter: String::new(),
            page: 0,
            pending: Vec::new(),
            role: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        cx.ctx.rest_task(
            |r| async move { r.permissions_list_all_full(None).await.map(|d| d.update) },
            |r| Message::Permissions(Msg::Loaded(r)),
        )
    }

    fn rows<'a>(&'a self, i18n: &ServerI18n) -> Vec<&'a PermissionDef> {
        let settings = self.tab == Tab::Settings;
        let mut v: Vec<&PermissionDef> = self
            .all
            .iter()
            .filter(|p| p.is_setting_permission() == settings)
            .filter(|p| matches(p, &self.filter, i18n))
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// A `permissions-changed` event (the workspace applied it already).
    pub fn on_permission_event(&mut self, kind: ChangeKind, p: &Permission) {
        match kind {
            ChangeKind::Removed => self.all.retain(|d| d.id != p.id),
            _ => match self.all.iter_mut().find(|d| d.id == p.id) {
                Some(d) => d.roles = p.roles.clone(),
                None => self.all.push(PermissionDef {
                    id: p.id.clone(),
                    roles: p.roles.clone(),
                    ..PermissionDef::default()
                }),
            },
        }
    }

    fn users(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let Some(e) = &mut self.role else {
            return Task::none();
        };
        let req = e.users.begin();
        let (role, page) = (e.role.id.clone(), e.users.rest_page());
        cx.ctx.rest_task(
            move |r| async move { r.roles_get_users_in_role(&role, None, &page).await },
            move |r| Message::Permissions(Msg::UsersLoaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::Loaded(r) => {
                self.loaded = true;
                match r {
                    Ok(all) => {
                        self.all = all;
                        self.error = None;
                    }
                    Err(f) => self.error = Some(err_text(&f, cx.i18n)),
                }
            }
            Msg::Reload => return self.load(cx),
            Msg::Tab(t) => {
                self.tab = t;
                self.page = 0;
            }
            Msg::Filter(f) => {
                self.filter = f;
                self.page = 0;
            }
            Msg::Page(p) => self.page = p,
            Msg::Toggle(perm, role, on) => {
                if self.pending.iter().any(|(p, r)| *p == perm && *r == role) {
                    return Task::none();
                }
                self.pending.push((perm.clone(), role.clone()));
                return cx.ctx.rest_task(
                    {
                        let (perm, role) = (perm.clone(), role.clone());
                        move |r| async move {
                            if on {
                                r.permissions_add_role(&perm, &role).await
                            } else {
                                r.permissions_remove_role(&perm, &role).await
                            }
                        }
                    },
                    move |r| Message::Permissions(Msg::Toggled(perm, role, on, r)),
                );
            }
            Msg::Toggled(perm, role, on, r) => {
                self.pending.retain(|(p, x)| !(*p == perm && *x == role));
                if cx.check(&r) {
                    let group = self
                        .all
                        .iter()
                        .find(|d| d.id == perm)
                        .and_then(|d| d.group_permission_id.clone());
                    // A setting permission also changes its group permission.
                    for d in self
                        .all
                        .iter_mut()
                        .filter(|d| d.id == perm || Some(&d.id) == group.as_ref())
                    {
                        d.roles.retain(|x| *x != role);
                        if on {
                            d.roles.push(role.clone());
                        }
                    }
                }
            }
            Msg::OpenRole(id) => {
                if let Some(role) = cx.ctx.workspace.roles().get(&id).cloned() {
                    self.role = Some(RoleEditor {
                        description: role.description.clone().unwrap_or_default(),
                        mandatory2fa: role.mandatory2fa.unwrap_or(false),
                        role,
                        busy: false,
                        error: None,
                        users: List::new(None),
                        add: String::new(),
                    });
                    return self.users(cx);
                }
            }
            Msg::CloseDialog => self.role = None,
            Msg::Description(s) => {
                if let Some(e) = &mut self.role {
                    e.description = s;
                }
            }
            Msg::Mandatory2fa(b) => {
                if let Some(e) = &mut self.role {
                    e.mandatory2fa = b;
                }
            }
            Msg::SaveRole => {
                if let Some(e) = &mut self.role {
                    e.busy = true;
                    // Always repeat name and scope (protected roles reject changes,
                    // an omitted description clears it).
                    let mut p = RoleParams::update_of(&e.role);
                    p.description = Some(e.description.clone());
                    p.mandatory2fa = Some(e.mandatory2fa);
                    return cx.ctx.rest_task(
                        move |r| async move { r.role_update(&p).await },
                        |r| Message::Permissions(Msg::RoleSaved(r)),
                    );
                }
            }
            Msg::RoleSaved(r) => {
                let text = r.as_ref().err().map(|f| err_text(f, cx.i18n));
                if let Some(e) = &mut self.role {
                    e.busy = false;
                    e.error = text;
                    if let Ok(role) = r {
                        e.role = role;
                        cx.ok(t!("admin.perms.role_saved").into_owned());
                    }
                }
            }
            Msg::Users(m) => {
                let Some(e) = &mut self.role else {
                    return Task::none();
                };
                return match e.users.update(m) {
                    Step::None => Task::none(),
                    Step::Wait(t) => t.map(|m| Message::Permissions(Msg::Users(m))),
                    Step::Load => self.users(cx),
                };
            }
            Msg::UsersLoaded(req, r) => {
                if let Some(e) = &mut self.role
                    && e.users.finish(req, r.map_err(|f| err_text(&f, cx.i18n)))
                {
                    return self.users(cx);
                }
            }
            Msg::AddUser(s) => {
                if let Some(e) = &mut self.role {
                    e.add = s;
                }
            }
            Msg::AddUserNow => {
                if let Some(e) = &self.role {
                    let username = e.add.trim().trim_start_matches('@').to_owned();
                    if username.is_empty() {
                        return Task::none();
                    }
                    let role = e.role.id.clone();
                    return cx.ctx.rest_task(
                        move |r| async move {
                            r.roles_add_user(&role, &username, None).await.map(|_| ())
                        },
                        |r| Message::Permissions(Msg::UserChanged(r)),
                    );
                }
            }
            Msg::RemoveUser(username) => {
                if let Some(e) = &self.role {
                    cx.ask(Confirm::danger(
                        t!("admin.perms.remove_user_title").into_owned(),
                        t!(
                            "admin.perms.remove_user_body",
                            user = username.clone(),
                            role = e.role.name.clone()
                        )
                        .into_owned(),
                        Message::Permissions(Msg::RemoveUserNow(username)),
                    ));
                }
            }
            Msg::RemoveUserNow(username) => {
                if let Some(e) = &self.role {
                    let role = e.role.id.clone();
                    return cx.ctx.rest_task(
                        move |r| async move {
                            r.roles_remove_user(&role, &username, None)
                                .await
                                .map(|_| ())
                        },
                        |r| Message::Permissions(Msg::UserChanged(r)),
                    );
                }
            }
            Msg::UserChanged(r) => {
                if cx.check(&r) {
                    if let Some(e) = &mut self.role {
                        e.add.clear();
                    }
                    return self.users(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let ws = v.ctx.workspace;
        let m = Message::Permissions;
        let can_settings = has_permission(ws, "access-setting-permissions", None);
        let can_edit = has_permission(ws, "access-permissions", None);
        let mut tabs = row![].spacing(SPACE_2);
        for (tab, label, enabled) in [
            (Tab::Permissions, t!("admin.perms.permissions"), true),
            (
                Tab::Settings,
                t!("admin.perms.setting_permissions"),
                can_settings,
            ),
            (Tab::Roles, t!("admin.perms.roles"), true),
        ] {
            tabs = tabs.push(
                button(text(label).size(TEXT_SMALL))
                    .padding([6, 12])
                    .style(if tab == self.tab {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press_maybe(enabled.then_some(m(Msg::Tab(tab)))),
            );
        }
        let col = column![
            title_row(
                t!("admin.section.permissions").into_owned(),
                if self.tab == Tab::Roles {
                    vec![icon_button(
                        Icon::RefreshCw,
                        Some(m(Msg::Reload)),
                        t!("admin.refresh"),
                    )]
                } else {
                    // The search bar carries the refresh button.
                    vec![]
                }
            ),
            tabs
        ]
        .spacing(SPACE_4);
        // The error has a slot of its own: it must not shift the filter
        // field and the matrix (focus, horizontal scroll).
        let error: Option<Element<'a, Message>> = self
            .error
            .as_ref()
            .map(|e| banner(BannerKind::Error, e.clone(), None));
        let head = col;
        let wrap = move |col: iced::widget::Column<'a, Message>| -> Element<'a, Message> {
            head.push(above(error, SPACE_4, col)).into()
        };
        let mut col = column![].spacing(SPACE_4);
        let mut roles: Vec<&Role> = ws.roles().values().collect();
        roles.sort_by(|a, b| {
            b.protected
                .unwrap_or(false)
                .cmp(&a.protected.unwrap_or(false))
                .then(a.name.cmp(&b.name))
        });
        if self.tab == Tab::Roles {
            let custom = v.license.is_some_and(|l| l.has_module("custom-roles"));
            col = col.push(muted(if custom {
                t!("admin.perms.roles_hint")
            } else {
                t!("admin.perms.custom_roles_premium")
            }));
            let mut list = column![].spacing(SPACE_1);
            for r in roles {
                list = list.push(
                    button(
                        row![
                            text(r.name.clone())
                                .size(TEXT_SMALL)
                                .font(font_semibold())
                                .width(Length::FillPortion(2)),
                            text(r.scope.clone().unwrap_or_else(|| "Users".into()))
                                .size(TEXT_SMALL)
                                .style(style::text_secondary)
                                .width(Length::FillPortion(1)),
                            text(r.description.clone().unwrap_or_default())
                                .size(TEXT_SMALL)
                                .style(style::text_secondary)
                                .width(Length::FillPortion(4)),
                        ]
                        .spacing(SPACE_3),
                    )
                    .padding([SPACE_2, SPACE_3])
                    .width(Length::Fill)
                    .style(style::button_list_item(false))
                    .on_press(m(Msg::OpenRole(r.id.clone()))),
                );
            }
            return wrap(
                col.push(
                    container(list)
                        .style(style::container_card)
                        .width(Length::Fill),
                ),
            );
        }
        col = col.push(search_bar(
            &self.filter,
            t!("admin.perms.filter").into_owned(),
            move |s| m(Msg::Filter(s)),
            m(Msg::Reload),
        ));
        if !self.loaded {
            return wrap(col.push(muted(t!("admin.loading"))));
        }
        let rows = self.rows(v.i18n);
        let pages = rows.len().div_ceil(ROWS).max(1);
        let page = self.page.min(pages - 1);
        let mut header = row![
            container(
                text(t!("admin.perms.permission"))
                    .size(TEXT_CAPTION)
                    .font(font_semibold())
            )
            .width(LABEL_W)
        ]
        .spacing(0)
        .padding([SPACE_2, SPACE_3]);
        for r in &roles {
            header = header.push(
                button(
                    text(r.name.clone())
                        .size(TEXT_CAPTION)
                        .font(font_semibold())
                        .style(style::text_link),
                )
                .padding(0)
                .width(CELL_W)
                .style(style::button_bare)
                .on_press(m(Msg::OpenRole(r.id.clone()))),
            );
        }
        let mut grid = column![header].spacing(0);
        for p in rows.iter().skip(page * ROWS).take(ROWS) {
            let mut line = row![
                container(
                    column![
                        text(label(p, v.i18n)).size(TEXT_SMALL),
                        text(p.id.clone())
                            .size(TEXT_CAPTION)
                            .style(style::text_secondary),
                    ]
                    .spacing(0)
                )
                .width(LABEL_W)
                .clip(true)
            ]
            .align_y(Alignment::Center)
            .padding([SPACE_1, SPACE_3]);
            for r in &roles {
                let on = p.roles.contains(&r.id);
                let busy = self.pending.iter().any(|(x, y)| *x == p.id && *y == r.id);
                let (pid, rid) = (p.id.clone(), r.id.clone());
                line = line.push(
                    container(
                        checkbox(on)
                            .size(16)
                            .style(style::checkbox)
                            .on_toggle_maybe((can_edit && !busy).then_some(move |b| {
                                Message::Permissions(Msg::Toggle(pid.clone(), rid.clone(), b))
                            })),
                    )
                    .width(CELL_W),
                );
            }
            grid = grid.push(line);
        }
        col = col.push(
            container(scrollable(grid).horizontal().style(style::scrollable))
                .width(Length::Fill)
                .style(style::container_card),
        );
        col = col.push(
            row![
                text(t!(
                    "admin.perms.page",
                    page = page + 1,
                    pages = pages,
                    total = rows.len()
                ))
                .size(TEXT_SMALL)
                .style(style::text_secondary)
                .width(Length::Fill),
                icon_button(
                    Icon::ChevronLeft,
                    (page > 0).then(|| m(Msg::Page(page - 1))),
                    t!("admin.prev")
                ),
                icon_button(
                    Icon::ChevronRight,
                    (page + 1 < pages).then(|| m(Msg::Page(page + 1))),
                    t!("admin.next")
                ),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        );
        wrap(col)
    }

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        let e = self.role.as_ref()?;
        let m = Message::Permissions;
        let protected = e.role.protected.unwrap_or(false);
        let mut users = column![].spacing(SPACE_1);
        for u in &e.users.items {
            let name = u.username.clone().unwrap_or_default();
            users = users.push(
                row![
                    text(format!("{} (@{name})", u.name.clone().unwrap_or_default()))
                        .size(TEXT_SMALL)
                        .width(Length::Fill),
                    icon_button(
                        Icon::UserMinus,
                        Some(m(Msg::RemoveUser(name))),
                        t!("admin.perms.remove_user_title")
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }
        if e.users.items.is_empty() && e.users.loaded {
            users = users.push(muted(t!("admin.perms.no_users")));
        }
        let mut body = column![
            kv(
                t!("admin.perms.role_name").into_owned(),
                e.role.name.clone()
            ),
            kv(
                t!("admin.perms.scope").into_owned(),
                e.role.scope.clone().unwrap_or_else(|| "Users".into())
            ),
            field(
                t!("admin.perms.description").into_owned(),
                None,
                input(&e.description, Some(move |s| m(Msg::Description(s))))
            ),
            switch(
                t!("admin.perms.mandatory2fa").into_owned(),
                e.mandatory2fa,
                Some(move |b| m(Msg::Mandatory2fa(b)))
            ),
        ]
        .spacing(SPACE_3);
        if protected {
            body = body.push(muted(t!("admin.perms.protected")));
        }
        // In a slot: a failed request must not move the add-user field.
        let error = e
            .error
            .as_ref()
            .map(|err| banner(BannerKind::Error, err.clone(), None));
        let body = body.push(above(
            error,
            SPACE_3,
            card(
                t!("admin.perms.users_in_role", total = e.users.total).into_owned(),
                column![
                    row![
                        input(&e.add, Some(move |s| m(Msg::AddUser(s))))
                            .on_submit(m(Msg::AddUserNow))
                            .width(Length::Fill),
                        secondary_button(t!("admin.perms.add_user"), Some(m(Msg::AddUserNow)))
                            .icon(Icon::UserPlus),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center),
                    users,
                    super::common::pager(&e.users, |x| m(Msg::Users(x))),
                ]
                .spacing(SPACE_2),
            ),
        ));
        Some(dialog(
            t!("admin.perms.role_title", name = e.role.name.clone()),
            body,
            vec![
                secondary_button(t!("admin.close"), Some(m(Msg::CloseDialog))).into(),
                primary_button(t!("admin.save"), (!e.busy).then_some(m(Msg::SaveRole)))
                    .loading(e.busy)
                    .into(),
            ],
            Some(m(Msg::CloseDialog)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn filter_words() {
        let en: HashMap<String, String> = [
            ("view-logs".to_owned(), "View Logs".to_owned()),
            ("Accounts".to_owned(), "Accounts".to_owned()),
        ]
        .into();
        let en = Arc::new(en);
        let i18n = ServerI18n::new("en", en.clone(), en);
        let p = PermissionDef {
            id: "view-logs".into(),
            ..PermissionDef::default()
        };
        assert!(matches(&p, "view LOGS", &i18n));
        assert!(!matches(&p, "view admin", &i18n));
        let s = PermissionDef {
            id: "change-setting-Accounts_X".into(),
            level: Some("settings".into()),
            group: Some("Accounts".into()),
            setting_id: Some("Accounts_X".into()),
            ..PermissionDef::default()
        };
        assert!(matches(&s, "accounts", &i18n));
        assert_eq!(label(&s, &i18n), "Accounts_X");
    }
}
