//! Room information of a chat (`docs/omnichannel.md` §8, web `ChatInfo`):
//! channel, contact, agent, department, tags, topic, times, custom fields,
//! SLA and priority. The room document comes from the workspace (live);
//! visitor and department are loaded.

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Department, SourceType, Visitor};
use rust_i18n::t;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::omnichannel::room::{
    ChannelDetail, ChannelLabel, InfoField, can_edit_room_info, info_fields, now,
};
use rustket_core::permissions::has_permission;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{ChipKind, chip, divider, ellipsis, presence_dot, secondary_button};

use super::PanelKind;
use crate::error::Res;
use crate::room::panel::common::{self, field, presence_of};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx, panel};

#[derive(Debug, Clone)]
pub enum Msg {
    Visitor(Res<Box<Visitor>>),
    Department(Res<Option<Box<Department>>>),
    Open(PanelKind),
}

#[derive(Default)]
pub struct InfoPanel {
    visitor: Option<Visitor>,
    visitor_for: Option<String>,
    department: Option<Department>,
    department_for: Option<String>,
}

pub fn source_icon(kind: SourceType) -> Icon {
    match kind {
        SourceType::Widget => Icon::MessageCircle,
        SourceType::Email => Icon::Mail,
        SourceType::Sms => Icon::Smartphone,
        _ => Icon::Headset,
    }
}

pub fn channel_text(label: &ChannelLabel) -> String {
    match label {
        ChannelLabel::Livechat => t!("omnichannel.source_livechat").into_owned(),
        ChannelLabel::Email => t!("omnichannel.source_email").into_owned(),
        ChannelLabel::Sms => t!("omnichannel.source_sms").into_owned(),
        ChannelLabel::Alias(a) => a.clone(),
        ChannelLabel::CustomApp => t!("omnichannel.source_custom_app").into_owned(),
        ChannelLabel::CustomApi => t!("omnichannel.source_custom_api").into_owned(),
        ChannelLabel::CustomIntegration => t!("omnichannel.source_custom").into_owned(),
        ChannelLabel::Raw(s) => s.clone(),
    }
}

/// Coarse "OS / browser" of a user agent string.
pub fn client_of(ua: &str) -> String {
    let os = [
        ("Windows", "Windows"),
        ("Android", "Android"),
        ("iPhone", "iOS"),
        ("iPad", "iOS"),
        ("Mac OS", "macOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map(|(_, v)| *v);
    let browser = [
        ("Edg/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(k, _)| ua.contains(k))
    .map(|(_, v)| *v);
    match (os, browser) {
        (Some(o), Some(b)) => format!("{o} / {b}"),
        (Some(x), None) | (None, Some(x)) => x.to_owned(),
        (None, None) => ua.chars().take(60).collect(),
    }
}

impl InfoPanel {
    fn load(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let Some(room) = ctx.workspace.room(ctx.rid).and_then(|e| e.room.clone()) else {
            return Action::none();
        };
        let mut tasks = Vec::new();
        if let Some(v) = room.visitor().map(|v| v.id).filter(|v| !v.is_empty())
            && self.visitor_for.as_ref() != Some(&v)
        {
            self.visitor_for = Some(v.clone());
            tasks.push(ctx.rest_task(
                move |rest| async move { rest.livechat_visitor_info(&v).await.map(Box::new) },
                Msg::Visitor,
            ));
        }
        if let Some(d) = room.department_id.clone().filter(|d| !d.is_empty())
            && self.department_for.as_ref() != Some(&d)
        {
            self.department_for = Some(d.clone());
            tasks.push(
                ctx.rest_task(
                    move |rest| async move {
                        rest.livechat_department(&d).await.map(|d| d.map(Box::new))
                    },
                    Msg::Department,
                ),
            );
        }
        Action::task(iced::Task::batch(tasks))
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.load(ctx)
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if changes.rooms.contains(ctx.rid) {
            return self.load(ctx);
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let _ = ctx;
        match msg {
            Msg::Visitor(Ok(v)) => self.visitor = Some(*v),
            Msg::Department(Ok(d)) => self.department = d.map(|d| *d),
            Msg::Visitor(Err(e)) | Msg::Department(Err(e)) => {
                tracing::debug!(error = %e, "chat info")
            }
            Msg::Open(k) => {
                return Action::effect(Effect::OpenPanel(panel::Kind::Omnichannel(k)));
            }
        }
        Action::none()
    }

    fn contact<'a>(&'a self, ctx: &ViewCtx<'a>, fallback: &str) -> Element<'a, Msg> {
        let name = self
            .visitor
            .as_ref()
            .map(|v| v.display_name().to_owned())
            .unwrap_or_else(|| fallback.to_owned());
        let mut sub = Vec::new();
        if let Some(v) = &self.visitor {
            if let Some(u) = &v.username {
                sub.push(u.clone());
            }
            if let Some(p) = v.phone_number() {
                sub.push(p.to_owned());
            }
            if let Some(e) = v.email() {
                sub.push(e.to_owned());
            }
        }
        let status = self
            .visitor
            .as_ref()
            .and_then(|v| v.status)
            .unwrap_or_default();
        let avatar = rustket_ui::widget::avatar(
            rustket_ui::widget::AvatarSource::initials(&name, ctx.rid),
            36.0,
            rustket_ui::widget::AvatarShape::Rounded,
        );
        row![
            avatar,
            column![
                row![
                    ellipsis(name).size(TEXT_BODY).font(FONT_SEMIBOLD),
                    presence_dot(presence_of(status), 8.0)
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
                text(sub.join(" · "))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary),
            ]
            .width(Length::Fill),
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center)
        .into()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let Some(room) = ws.room(ctx.rid).and_then(|e| e.room.as_ref()) else {
            return common::loading();
        };
        let omni = crate::omnichannel::room::omni_of(ctx.server_id);
        let can_cf = has_permission(ws, perm::VIEW_LIVECHAT_ROOM_CUSTOMFIELDS, Some(ctx.rid));
        let fields = info_fields(room, now(), &omni.custom_fields, can_cf);
        let fallback = room
            .visitor()
            .map(|v| v.display_name().to_owned())
            .or_else(|| room.fname.clone())
            .unwrap_or_default();
        let mut col: Column<'a, Msg> = column![].spacing(SPACE_4).padding(SPACE_4);
        let label = |k: &str| t!(k).into_owned();
        let plain = |s: String| -> Element<'a, Msg> { text(s).size(TEXT_BODY).into() };
        for f in fields {
            let el: Element<'a, Msg> = match f {
                InfoField::Channel {
                    kind,
                    label: l,
                    detail,
                } => {
                    let mut r = row![
                        icon::sized(source_icon(kind), 16.0),
                        text(channel_text(&l)).size(TEXT_BODY)
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center);
                    match detail {
                        ChannelDetail::None => {}
                        ChannelDetail::Text(d) => {
                            r = r.push(text(d).size(TEXT_SMALL).style(style::text_secondary));
                        }
                        ChannelDetail::External => {
                            r = r.push(
                                text(label("omnichannel.source_external"))
                                    .size(TEXT_SMALL)
                                    .style(style::text_secondary),
                            );
                        }
                    }
                    field(label("omnichannel.info.channel"), r)
                }
                InfoField::Contact { .. } => {
                    let mut c = column![self.contact(ctx, &fallback)].spacing(SPACE_2);
                    if let Some(ua) = self.visitor.as_ref().and_then(|v| v.user_agent.clone()) {
                        let mut parts = vec![client_of(&ua)];
                        if let Some(h) = self.visitor.as_ref().and_then(|v| v.host.clone()) {
                            parts.push(h);
                        }
                        if let Some(ip) = self.visitor.as_ref().and_then(|v| v.ip.clone()) {
                            parts.push(ip);
                        }
                        c = c.push(
                            text(parts.join(" · "))
                                .size(TEXT_SMALL)
                                .style(style::text_secondary),
                        );
                    }
                    field(label("omnichannel.info.contact"), c)
                }
                InfoField::Agent { user_id, username } => {
                    let u = username.unwrap_or(user_id);
                    field(
                        label("omnichannel.info.agent"),
                        row![
                            common::user_avatar(ctx, &u, &u, 24.0),
                            text(format!("@{u}")).size(TEXT_BODY)
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                    )
                }
                InfoField::Department { id } => {
                    let name = self
                        .department
                        .as_ref()
                        .filter(|d| d.id == id)
                        .map(|d| d.name.clone())
                        .unwrap_or(id);
                    field(label("omnichannel.info.department"), plain(name))
                }
                InfoField::Tags(tags) => {
                    let mut r = row![].spacing(SPACE_1);
                    for t in tags {
                        r = r.push(chip(t, ChipKind::Neutral));
                    }
                    field(label("omnichannel.info.tags"), r.wrap())
                }
                InfoField::Topic(topic) => field(label("omnichannel.info.topic"), plain(topic)),
                InfoField::QueueTime { secs } => field(
                    label("omnichannel.info.queue_time"),
                    plain(super::duration(secs)),
                ),
                InfoField::ChatDuration { secs } => field(
                    label("omnichannel.info.duration"),
                    plain(super::duration(secs)),
                ),
                InfoField::CreatedAt(ts) => field(
                    label("omnichannel.info.created_at"),
                    plain(common::date_time_label(ts, ctx)),
                ),
                InfoField::ClosedAt(ts) => field(
                    label("omnichannel.info.closed_at"),
                    plain(common::date_time_label(ts, ctx)),
                ),
                InfoField::TakenAt(ts) => field(
                    label("omnichannel.info.taken_at"),
                    plain(common::date_time_label(ts, ctx)),
                ),
                InfoField::AvgResponseTime { secs } => field(
                    label("omnichannel.info.avg_response"),
                    plain(super::duration(secs.round() as i64)),
                ),
                InfoField::InactivityTime { secs } => field(
                    label("omnichannel.info.inactivity"),
                    plain(super::duration(secs)),
                ),
                InfoField::CustomFields(list) => {
                    let mut c = column![].spacing(SPACE_3);
                    for (k, v) in list {
                        c = c.push(field(k, plain(v)));
                    }
                    c.into()
                }
                InfoField::Sla { id } => field(label("omnichannel.info.sla"), plain(id)),
                InfoField::Priority { id } => {
                    let name = omni
                        .priorities
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| {
                            p.custom_name()
                                .or(p.i18n.as_deref())
                                .unwrap_or_default()
                                .to_owned()
                        })
                        .unwrap_or(id);
                    field(label("omnichannel.info.priority"), plain(name))
                }
            };
            col = col.push(el);
        }
        let mut actions = row![].spacing(SPACE_2);
        if can_edit_room_info(ws, room) {
            actions = actions.push(
                secondary_button(
                    t!("omnichannel.edit").into_owned(),
                    Some(Msg::Open(PanelKind::Edit)),
                )
                .icon(Icon::Pencil),
            );
        }
        if room.contact_id.is_some() && has_permission(ws, perm::VIEW_LIVECHAT_CONTACT, None) {
            actions = actions.push(
                secondary_button(
                    t!("omnichannel.contact_info").into_owned(),
                    Some(Msg::Open(PanelKind::Contact)),
                )
                .icon(Icon::Contact),
            );
        }
        let body = column![
            scrollable(col)
                .height(Length::Fill)
                .style(style::scrollable),
            divider(),
            container(actions.wrap()).padding(SPACE_3),
        ];
        body.height(Length::Fill).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agents() {
        assert_eq!(
            client_of(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0 Safari/537.36"
            ),
            "Linux / Chrome"
        );
        assert_eq!(client_of("curl/8.0"), "curl/8.0");
    }
}
