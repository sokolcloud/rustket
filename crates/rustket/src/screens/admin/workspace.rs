//! Workspace info and statistics (docs/admin.md §3), the instances list, the
//! read-only license summary and banners (§19); with `raw` the "Reports" page
//! (the statistics JSON, §1).

use iced::widget::{column, container, row, text};
use iced::{Element, Length, Task};
use rocketchat::models::ServerInfo;
use rocketchat::models::admin::{Banner, Instance, LicenseInfo, Statistics};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{MONOSPACE_FONT, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{BannerKind, banner, dialog, loading_placeholder, secondary_button};

use super::common::{Cx, card, date, kv, num, size, title_row, yes_no_text};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    Info(Res<ServerInfo>),
    Stats(Res<Box<Statistics>>),
    Instances(Res<Vec<Instance>>),
    License(Res<LicenseInfo>),
    Banners(Res<Vec<Banner>>),
    Refresh,
    Download,
    Saved(Result<bool, String>),
    ShowInstances,
    CloseDialog,
}

pub struct State {
    pub raw: bool,
    pub info: Option<ServerInfo>,
    pub stats: Option<Statistics>,
    pub instances: Vec<Instance>,
    pub license: Option<LicenseInfo>,
    pub banners: Vec<Banner>,
    pub error: Option<String>,
    pub loading: bool,
    pub show_instances: bool,
}

impl State {
    pub fn new(raw: bool) -> Self {
        Self {
            raw,
            info: None,
            stats: None,
            instances: Vec::new(),
            license: None,
            banners: Vec::new(),
            error: None,
            loading: false,
            show_instances: false,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.fetch(cx, false)
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>, refresh: bool) -> Task<Message> {
        self.loading = true;
        let stats = cx.ctx.rest_task(
            move |r| async move { r.statistics(refresh).await.map(Box::new) },
            |r| Message::Workspace(Msg::Stats(r)),
        );
        if self.raw {
            return stats;
        }
        Task::batch([
            stats,
            cx.ctx.rest_task(
                |r| async move { r.admin_server_info().await },
                |r| Message::Workspace(Msg::Info(r)),
            ),
            cx.ctx.rest_task(
                |r| async move { r.instances_get().await },
                |r| Message::Workspace(Msg::Instances(r)),
            ),
            cx.ctx.rest_task(
                |r| async move { r.admin_licenses_info(true).await },
                |r| Message::Workspace(Msg::License(r)),
            ),
            cx.ctx.rest_task(
                |r| async move { r.admin_banners_list("web").await },
                |r| Message::Workspace(Msg::Banners(r)),
            ),
        ])
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::Info(r) => match r {
                Ok(i) => self.info = Some(i),
                Err(f) => self.error = Some(super::common::err_text(&f, cx.i18n)),
            },
            Msg::Stats(r) => {
                self.loading = false;
                match r {
                    Ok(s) => {
                        self.stats = Some(*s);
                        self.error = None;
                    }
                    Err(f) => self.error = Some(super::common::err_text(&f, cx.i18n)),
                }
            }
            Msg::Instances(r) => self.instances = r.unwrap_or_default(),
            Msg::License(r) => self.license = r.ok(),
            Msg::Banners(r) => self.banners = r.unwrap_or_default(),
            Msg::Refresh => return self.fetch(cx, true),
            Msg::Download => {
                let Some(s) = &self.stats else {
                    return Task::none();
                };
                let json = serde_json::to_string_pretty(s).unwrap_or_default();
                return Task::perform(
                    async move {
                        let Some(f) = rfd::AsyncFileDialog::new()
                            .set_file_name("statistics.json")
                            .save_file()
                            .await
                        else {
                            return Ok(false);
                        };
                        tokio::fs::write(f.path(), json)
                            .await
                            .map(|()| true)
                            .map_err(|e| e.to_string())
                    },
                    |r| Message::Workspace(Msg::Saved(r)),
                );
            }
            Msg::Saved(Ok(true)) => cx.ok(t!("admin.ws.saved").into_owned()),
            Msg::Saved(Ok(false)) => {}
            Msg::Saved(Err(e)) => cx.effects.push(crate::component::Effect::Toast(
                BannerKind::Error,
                t!("admin.failed").into_owned(),
                e,
            )),
            Msg::ShowInstances => self.show_instances = true,
            Msg::CloseDialog => self.show_instances = false,
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let _ = v;
        let title = if self.raw {
            t!("admin.section.reports")
        } else {
            t!("admin.section.workspace")
        };
        let mut buttons = vec![
            secondary_button(t!("admin.refresh"), Some(Message::Workspace(Msg::Refresh)))
                .icon(Icon::RefreshCw)
                .loading(self.loading)
                .into(),
        ];
        if self.stats.is_some() {
            buttons.push(
                secondary_button(
                    t!("admin.ws.download"),
                    Some(Message::Workspace(Msg::Download)),
                )
                .icon(Icon::Download)
                .into(),
            );
        }
        let mut col = column![title_row(title.into_owned(), buttons)].spacing(SPACE_4);
        if let Some(e) = &self.error {
            col = col.push(banner(BannerKind::Error, e.clone(), None));
        }
        let Some(s) = &self.stats else {
            return col
                .push(container(loading_placeholder(None)).height(240))
                .into();
        };
        if self.raw {
            let json = serde_json::to_string_pretty(s).unwrap_or_default();
            return col
                .push(
                    container(text(json).font(MONOSPACE_FONT).size(TEXT_CAPTION))
                        .padding(SPACE_3)
                        .width(Length::Fill)
                        .style(style::container_code_block),
                )
                .into();
        }
        let info = self.info.as_ref();
        let build = info.and_then(|i| i.info.as_ref());
        let version = card(
            t!("admin.ws.version").into_owned(),
            column![
                kv(
                    t!("admin.ws.version").into_owned(),
                    info.and_then(|i| i.version.clone())
                        .or_else(|| s.version.clone())
                        .unwrap_or_default()
                ),
                kv(
                    t!("admin.ws.license").into_owned(),
                    match &self.license {
                        Some(l) if l.has_valid_license => t!("admin.ws.licensed").into_owned(),
                        Some(_) => t!("admin.ws.community").into_owned(),
                        None => "—".into(),
                    }
                ),
                kv(
                    t!("admin.ws.modules").into_owned(),
                    self.license
                        .as_ref()
                        .map(|l| {
                            if l.active_modules.is_empty() {
                                "—".to_owned()
                            } else {
                                l.active_modules.join(", ")
                            }
                        })
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.build").into_owned(),
                    build
                        .and_then(|b| b.build.as_ref())
                        .and_then(|b| b.get("date"))
                        .and_then(|d| d.as_str())
                        .map(local_time)
                        .unwrap_or_else(|| "—".into())
                ),
            ]
            .spacing(SPACE_2),
        );
        let users = card(
            t!("admin.ws.users").into_owned(),
            column![
                kv(t!("admin.ws.online").into_owned(), num(s.online_users)),
                kv(t!("admin.ws.busy").into_owned(), num(s.busy_users)),
                kv(t!("admin.ws.away").into_owned(), num(s.away_users)),
                kv(t!("admin.ws.offline").into_owned(), num(s.offline_users)),
                kv(t!("admin.ws.total").into_owned(), num(s.total_users)),
                kv(
                    t!("admin.ws.connected").into_owned(),
                    num(s.total_connected_users)
                ),
                kv(
                    t!("admin.ws.active_users").into_owned(),
                    num(s.active_users)
                ),
                kv(
                    t!("admin.ws.active_guests").into_owned(),
                    num(s.active_guests)
                ),
                kv(
                    t!("admin.ws.non_active").into_owned(),
                    num(s.non_active_users)
                ),
                kv(t!("admin.ws.app_users").into_owned(), num(s.app_users)),
                kv(t!("admin.ws.uploads").into_owned(), num(s.uploads_total)),
                kv(
                    t!("admin.ws.uploads_size").into_owned(),
                    s.uploads_total_size.map(size).unwrap_or_else(|| "—".into())
                ),
            ]
            .spacing(SPACE_2),
        );
        let rooms = card(
            t!("admin.ws.rooms_messages").into_owned(),
            column![
                kv(t!("admin.ws.channels").into_owned(), num(s.total_channels)),
                kv(
                    t!("admin.ws.private_groups").into_owned(),
                    num(s.total_private_groups)
                ),
                kv(t!("admin.ws.direct").into_owned(), num(s.total_direct)),
                kv(
                    t!("admin.ws.discussions").into_owned(),
                    num(s.total_discussions)
                ),
                kv(t!("admin.ws.livechat").into_owned(), num(s.total_livechat)),
                kv(t!("admin.ws.total_rooms").into_owned(), num(s.total_rooms)),
                kv(
                    t!("admin.ws.channel_messages").into_owned(),
                    num(s.total_channel_messages)
                ),
                kv(
                    t!("admin.ws.group_messages").into_owned(),
                    num(s.total_private_group_messages)
                ),
                kv(
                    t!("admin.ws.direct_messages").into_owned(),
                    num(s.total_direct_messages)
                ),
                kv(
                    t!("admin.ws.total_messages").into_owned(),
                    num(s.total_messages)
                ),
            ]
            .spacing(SPACE_2),
        );
        let commit = build.and_then(|b| b.commit.as_ref());
        let deployment = card(
            t!("admin.ws.deployment").into_owned(),
            column![
                kv(
                    t!("admin.ws.site_url").into_owned(),
                    info.and_then(|i| i.workspace_url.clone())
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.deployment_id").into_owned(),
                    s.unique_id.clone().unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.apps_engine").into_owned(),
                    s.apps
                        .as_ref()
                        .and_then(|a| a.engine_version.clone())
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.node").into_owned(),
                    s.process
                        .as_ref()
                        .and_then(|p| p.node_version.clone())
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.mongo").into_owned(),
                    format!(
                        "{} / {}",
                        s.mongo_version.clone().unwrap_or_default(),
                        s.mongo_storage_engine.clone().unwrap_or_default()
                    )
                ),
                kv(
                    t!("admin.ws.migration").into_owned(),
                    s.migration
                        .as_ref()
                        .map(|m| format!("{} ({})", num(m.version), date(m.locked_at)))
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.commit").into_owned(),
                    commit
                        .map(|c| {
                            let hash = c.get("hash").and_then(|h| h.as_str()).unwrap_or("");
                            let branch = c.get("branch").and_then(|h| h.as_str()).unwrap_or("");
                            format!("{} {branch}", hash.chars().take(9).collect::<String>())
                        })
                        .unwrap_or_else(|| "—".into())
                ),
                kv(
                    t!("admin.ws.pid").into_owned(),
                    num(s.process.as_ref().and_then(|p| p.pid))
                ),
                kv(t!("admin.ws.stats_at").into_owned(), date(s.created_at)),
                secondary_button(
                    t!("admin.ws.instances", n = self.instances.len()),
                    Some(Message::Workspace(Msg::ShowInstances))
                )
                .icon(Icon::Server),
            ]
            .spacing(SPACE_2),
        );
        col = col
            .push(row![version, deployment].spacing(SPACE_4))
            .push(row![users, rooms].spacing(SPACE_4));
        let banners = if self.banners.is_empty() {
            column![super::common::muted(t!("admin.ws.no_banners"))]
        } else {
            column(self.banners.iter().map(|b| {
                kv(
                    b.id.clone(),
                    format!("{} – {}", date(b.start_at), date(b.expire_at)),
                )
            }))
            .spacing(SPACE_2)
        };
        col = col.push(card(t!("admin.ws.banners").into_owned(), banners));
        col.into()
    }

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        if !self.show_instances {
            return None;
        }
        let mut col = column![].spacing(SPACE_3);
        for i in &self.instances {
            let r = &i.instance_record;
            let extra = r
                .extra_information
                .as_ref()
                .map(|e| serde_json::to_string_pretty(e).unwrap_or_default())
                .unwrap_or_default();
            col = col.push(
                column![
                    kv(
                        t!("admin.ws.address").into_owned(),
                        i.address.clone().unwrap_or_default()
                    ),
                    kv(
                        t!("admin.ws.connected_flag").into_owned(),
                        yes_no_text(i.current_status.connected.unwrap_or(false))
                    ),
                    kv(
                        t!("admin.ws.local").into_owned(),
                        yes_no_text(i.current_status.local.unwrap_or(false))
                    ),
                    kv(
                        t!("admin.ws.heartbeat").into_owned(),
                        num(i.current_status.last_heartbeat_time)
                    ),
                    kv("ID".into(), r.id.clone()),
                    kv(
                        t!("admin.ws.name").into_owned(),
                        r.name.clone().unwrap_or_default()
                    ),
                    kv(t!("admin.ws.pid").into_owned(), num(r.pid)),
                    kv(t!("admin.ws.created").into_owned(), date(r.created_at)),
                    text(extra).font(MONOSPACE_FONT).size(TEXT_SMALL),
                ]
                .spacing(SPACE_2),
            );
        }
        Some(dialog(
            t!("admin.ws.instances_title"),
            col,
            vec![],
            Some(Message::Workspace(Msg::CloseDialog)),
        ))
    }
}

/// An ISO timestamp in local time, or the text as is.
fn local_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_owned())
}
