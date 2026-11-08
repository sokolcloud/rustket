//! OAuth apps — Rocket.Chat as an OAuth2 provider (docs/admin.md §11): list,
//! create/edit with the read-only client credentials and endpoint URLs, delete.

use std::sync::atomic::{AtomicU64, Ordering};

use iced::widget::{column, container, row};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::admin::OAuthApp;
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4};
use rustket_ui::widget::{danger_button, icon_button, primary_button};

use super::common::{
    Confirm, Cx, List, ListMsg, col, date, err_text, field, form_dialog, input, list_view, muted,
    readonly, small, switch, table, title_row, yes_no,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(Res<Vec<OAuthApp>>),
    New,
    Open(String),
    CloseDialog,
    Name(String),
    Redirect(String),
    Active(bool),
    Save,
    /// The answer for the form showing with this token.
    Saved(u64, Res<OAuthApp>),
    Copy(String),
    Delete,
    DeleteNow(String),
    Deleted(String, Res<()>),
}

/// Form showings: a late save answer never retargets another form.
static FORMS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default)]
pub struct Form {
    /// This showing of the form.
    pub token: u64,
    pub app: Option<OAuthApp>,
    pub name: String,
    pub redirect: String,
    pub active: bool,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct State {
    pub list: List<OAuthApp>,
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
        self.list.loading = true;
        cx.ctx.rest_task(
            |r| async move { r.oauth_apps_list().await },
            |r| Message::OAuth(Msg::Loaded(r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let form = self.form.as_mut();
        match m {
            Msg::List(ListMsg::Reload) => return self.load(cx),
            Msg::List(_) => {}
            Msg::Loaded(r) => match r {
                Ok(apps) => self.list.set_all(apps),
                Err(f) => {
                    self.list.loading = false;
                    self.list.loaded = true;
                    self.list.error = Some(err_text(&f, cx.i18n));
                }
            },
            Msg::New => {
                self.form = Some(Form {
                    token: FORMS.fetch_add(1, Ordering::Relaxed),
                    active: true,
                    ..Form::default()
                });
            }
            Msg::Open(id) => {
                if let Some(a) = self.list.items.iter().find(|a| a.id == id) {
                    self.form = Some(Form {
                        token: FORMS.fetch_add(1, Ordering::Relaxed),
                        name: a.name.clone(),
                        redirect: a.redirect_uri.clone(),
                        active: a.active,
                        app: Some(a.clone()),
                        ..Form::default()
                    });
                }
            }
            Msg::CloseDialog => self.form = None,
            Msg::Name(s) => {
                if let Some(f) = form {
                    f.name = s;
                    f.error = None;
                }
            }
            Msg::Redirect(s) => {
                if let Some(f) = form {
                    f.redirect = s;
                    f.error = None;
                }
            }
            Msg::Active(b) => {
                if let Some(f) = form {
                    f.active = b;
                }
            }
            Msg::Save => {
                let Some(f) = form else {
                    return Task::none();
                };
                if f.name.trim().is_empty() || f.redirect.trim().is_empty() {
                    f.error = Some(t!("admin.oauth.required").into_owned());
                    return Task::none();
                }
                f.busy = true;
                let (name, redirect, active) = (
                    f.name.trim().to_owned(),
                    f.redirect.trim().to_owned(),
                    f.active,
                );
                let id = f.app.as_ref().map(|a| a.id.clone());
                let token = f.token;
                return cx.ctx.rest_task(
                    move |r| async move {
                        match id {
                            Some(id) => r.oauth_app_update(&id, &name, active, &redirect).await,
                            None => r.oauth_app_create(&name, active, &redirect).await,
                        }
                    },
                    move |r| Message::OAuth(Msg::Saved(token, r)),
                );
            }
            Msg::Saved(token, r) => {
                // Only the form that saved takes the answer.
                let form = form.filter(|f| f.token == token);
                match r {
                    Ok(app) => {
                        cx.ok(t!("admin.oauth.saved").into_owned());
                        // Stay open after create: the client credentials are shown now.
                        if let Some(f) = form {
                            f.busy = false;
                            f.app = Some(app);
                        }
                        return self.load(cx);
                    }
                    Err(e) => {
                        let text = err_text(&e, cx.i18n);
                        match form {
                            Some(f) => {
                                f.busy = false;
                                f.error = Some(text);
                            }
                            None => cx.fail(&e),
                        }
                    }
                }
            }
            Msg::Copy(s) => {
                cx.ok(t!("admin.copied").into_owned());
                return iced::clipboard::write(s);
            }
            Msg::Delete => {
                if let Some(a) = form.and_then(|f| f.app.clone()) {
                    cx.ask(Confirm::danger(
                        t!("admin.oauth.delete_title").into_owned(),
                        t!("admin.oauth.delete_body", name = a.name).into_owned(),
                        Message::OAuth(Msg::DeleteNow(a.id)),
                    ));
                }
            }
            Msg::DeleteNow(id) => {
                let id2 = id.clone();
                return cx.ctx.rest_task(
                    move |r| async move { r.oauth_app_delete(&id2).await },
                    move |r| Message::OAuth(Msg::Deleted(id.clone(), r)),
                );
            }
            Msg::Deleted(id, r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.oauth.deleted").into_owned());
                    if self
                        .form
                        .as_ref()
                        .is_some_and(|f| f.app.as_ref().is_some_and(|a| a.id == id))
                    {
                        self.form = None;
                    }
                    return self.load(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let rows = self
            .list
            .items
            .iter()
            .map(|a| {
                (
                    vec![
                        small(a.name.clone()),
                        yes_no(a.active),
                        muted(
                            a.created_by
                                .as_ref()
                                .and_then(|u| u.username.clone())
                                .unwrap_or_default(),
                        ),
                        muted(date(a.created_at)),
                    ],
                    Some(Message::OAuth(Msg::Open(a.id.clone()))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        col(t!("admin.oauth.name"), 3),
                        col(t!("admin.oauth.active"), 1),
                        col(t!("admin.oauth.created_by"), 2),
                        col(t!("admin.oauth.created"), 2),
                    ],
                    None,
                    |_| Message::Noop,
                    rows,
                )
            },
            |m| Message::OAuth(Msg::List(m)),
            t!("admin.oauth.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.third_party_login").into_owned(),
                vec![
                    primary_button(t!("admin.oauth.new"), Some(Message::OAuth(Msg::New)))
                        .icon(Icon::Plus)
                        .into()
                ]
            ),
            list
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let mut body = column![
            switch(
                t!("admin.oauth.active").into_owned(),
                f.active,
                Some(|b| Message::OAuth(Msg::Active(b)))
            ),
            field(
                t!("admin.oauth.name").into_owned(),
                None,
                input(&f.name, Some(|s| Message::OAuth(Msg::Name(s))))
            ),
            field(
                t!("admin.oauth.redirect").into_owned(),
                Some(t!("admin.oauth.redirect_hint").into_owned()),
                input(&f.redirect, Some(|s| Message::OAuth(Msg::Redirect(s))))
            ),
        ]
        .spacing(SPACE_3);
        if let Some(a) = &f.app {
            let site = v
                .ctx
                .workspace
                .settings()
                .str("Site_Url")
                .unwrap_or(v.ctx.server.as_str())
                .trim_end_matches('/')
                .to_owned();
            for (label, value) in [
                (
                    t!("admin.oauth.client_id"),
                    a.client_id.clone().unwrap_or_default(),
                ),
                (
                    t!("admin.oauth.client_secret"),
                    a.client_secret.clone().unwrap_or_default(),
                ),
                (
                    t!("admin.oauth.authorize_url"),
                    format!("{site}/oauth/authorize"),
                ),
                (t!("admin.oauth.token_url"), format!("{site}/oauth/token")),
            ] {
                body = body.push(field(
                    label.into_owned(),
                    None,
                    row![
                        readonly(value.clone()),
                        icon_button(
                            Icon::Copy,
                            Some(Message::OAuth(Msg::Copy(value))),
                            t!("admin.copy")
                        ),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center),
                ));
            }
        }
        let extra = if f.app.is_some() {
            vec![
                danger_button(t!("admin.delete"), Some(Message::OAuth(Msg::Delete)))
                    .icon(Icon::Trash2)
                    .into(),
            ]
        } else {
            vec![]
        };
        Some(form_dialog(
            if f.app.is_some() {
                t!("admin.oauth.edit").into_owned()
            } else {
                t!("admin.oauth.new").into_owned()
            },
            container(body).width(Length::Fill).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::OAuth(Msg::Save)),
            Message::OAuth(Msg::CloseDialog),
            extra,
        ))
    }
}
