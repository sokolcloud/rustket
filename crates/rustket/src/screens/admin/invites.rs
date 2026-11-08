//! Invites (docs/admin.md §6): all stored invites (bare array), links rebuilt
//! like the server does, removal.

use iced::widget::{column, row};
use iced::{Element, Task};
use rocketchat::RcDate;
use rocketchat::models::admin::Invite;
use rust_i18n::t;
use rustket_core::admin::invites::{InviteUrlSettings, invite_link};
use rustket_ui::icon::Icon;
use rustket_ui::theme::{SPACE_1, SPACE_4};
use rustket_ui::widget::icon_button;

use super::common::{
    Confirm, Cx, List, ListMsg, col, date, err_text, list_view, muted, small, table, title_row,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(Res<Vec<Invite>>),
    UrlType(Res<serde_json::Value>),
    Copy(String),
    Remove(String),
    RemoveNow(String),
    Removed(Res<()>),
}

pub struct State {
    pub list: List<Invite>,
    pub url_type: Option<String>,
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
            url_type: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        // Not public: read it as a privileged setting (falls back to the proxy form).
        let url_type = cx.ctx.rest_task(
            |r| async move { r.setting_value("Accounts_Registration_InviteUrlType").await },
            |r| Message::Invites(Msg::UrlType(r)),
        );
        Task::batch([self.fetch(cx), url_type])
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.list.loading = true;
        cx.ctx.rest_task(
            |r| async move { r.list_invites().await },
            |r| Message::Invites(Msg::Loaded(r)),
        )
    }

    fn settings(&self, ws: &rustket_core::Workspace) -> InviteUrlSettings {
        let s = |id: &str| ws.settings().str(id).unwrap_or_default().to_owned();
        InviteUrlSettings {
            site_url: s("Site_Url"),
            invite_url_type: self.url_type.clone(),
            deep_link_url: s("DeepLink_Url"),
            cdn_prefix: s("CDN_PREFIX"),
        }
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::List(ListMsg::Reload) => return self.fetch(cx),
            Msg::List(_) => {}
            Msg::Loaded(r) => match r {
                Ok(mut all) => {
                    all.sort_by_key(|i| std::cmp::Reverse(i.created_at));
                    self.list.set_all(all);
                }
                Err(f) => {
                    self.list.loading = false;
                    self.list.loaded = true;
                    self.list.error = Some(err_text(&f, cx.i18n));
                }
            },
            Msg::UrlType(r) => {
                self.url_type = r.ok().and_then(|v| v.as_str().map(str::to_owned));
            }
            Msg::Copy(link) => {
                cx.ok(t!("admin.invites.copied").into_owned());
                return iced::clipboard::write(link);
            }
            Msg::Remove(id) => cx.ask(Confirm::danger(
                t!("admin.invites.remove_title").into_owned(),
                t!("admin.invites.remove_body", id = id).into_owned(),
                Message::Invites(Msg::RemoveNow(id)),
            )),
            Msg::RemoveNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.remove_invite(&id).await },
                    |r| Message::Invites(Msg::Removed(r)),
                );
            }
            Msg::Removed(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.invites.removed").into_owned());
                    return self.fetch(cx);
                }
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let ws = v.ctx.workspace;
        let settings = self.settings(ws);
        let now = RcDate::now();
        let rows = self
            .list
            .items
            .iter()
            .map(|i| {
                let room = ws
                    .room(&i.rid)
                    .map(|e| e.name().to_owned())
                    .unwrap_or_else(|| i.rid.clone());
                let expires = match i.expires {
                    None => t!("admin.invites.never").into_owned(),
                    Some(_) if i.is_expired(now) => t!("admin.invites.expired").into_owned(),
                    e => date(e),
                };
                let left = match i.uses_left() {
                    None => t!("admin.invites.unlimited").into_owned(),
                    Some(n) => n.to_string(),
                };
                let link = invite_link(i, &settings);
                (
                    vec![
                        small(i.id.clone()),
                        small(room),
                        muted(date(i.created_at)),
                        muted(expires),
                        small(i.uses.to_string()),
                        small(left),
                        row![
                            icon_button(
                                Icon::Copy,
                                Some(Message::Invites(Msg::Copy(link))),
                                t!("admin.invites.copy")
                            ),
                            icon_button(
                                Icon::Trash2,
                                Some(Message::Invites(Msg::Remove(i.id.clone()))),
                                t!("admin.delete")
                            ),
                        ]
                        .spacing(SPACE_1)
                        .into(),
                    ],
                    None,
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        col(t!("admin.invites.token"), 2),
                        col(t!("admin.invites.room"), 2),
                        col(t!("admin.invites.created"), 2),
                        col(t!("admin.invites.expires"), 2),
                        col(t!("admin.invites.uses"), 1),
                        col(t!("admin.invites.left"), 1),
                        col("", 1),
                    ],
                    None,
                    |_| Message::Noop,
                    rows,
                )
            },
            |m| Message::Invites(Msg::List(m)),
            t!("admin.invites.empty").into_owned(),
        );
        column![
            title_row(
                t!("admin.section.invites").into_owned(),
                vec![icon_button(
                    Icon::RefreshCw,
                    Some(Message::Invites(Msg::List(ListMsg::Reload))),
                    t!("admin.refresh")
                )]
            ),
            list
        ]
        .spacing(SPACE_4)
        .into()
    }
}
