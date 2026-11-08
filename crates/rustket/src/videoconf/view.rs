//! Call popups (incoming / outgoing, stacked bottom-right over everything) and
//! the call dialogs (start, no provider, call information).

use iced::widget::{Column, column, container, row, space, stack, text, toggler};
use iced::{Alignment, Element, Length};
use rocketchat::models::VideoConfCapabilities;
use rust_i18n::t;
use rustket_core::room_list::{self, Avatar};
use rustket_core::videoconf::InfoState;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, TEXT_BODY, TEXT_CAPTION,
    TEXT_SUBTITLE,
};
use rustket_ui::widget::{
    AvatarShape, avatar, danger_button, dialog, icon_button_with, modal, primary_button,
    secondary_button,
};

use super::{Dialog, Message, block, provider_error_text};
use crate::app::App;
use crate::images::Images;

type AppMsg = crate::message::Message;

fn m(msg: Message) -> AppMsg {
    AppMsg::Calls(msg)
}

fn room_name(ws: &Workspace, rid: &str) -> String {
    ws.room(rid)
        .map(|e| room_list::display_name(ws, e, ws.use_real_names(None)))
        .unwrap_or_default()
}

fn toggles<'a>(caps: Option<&VideoConfCapabilities>, mic: bool, cam: bool) -> Element<'a, AppMsg> {
    let mut r = row![].spacing(SPACE_4).align_y(Alignment::Center);
    let caps = caps.copied().unwrap_or_default();
    if caps.mic == Some(true) {
        r = r.push(
            toggler(mic)
                .label(t!("videoconf.mic").into_owned())
                .on_toggle(|_| m(Message::ToggleMic))
                .style(style::toggler)
                .size(18.0)
                .text_size(TEXT_CAPTION),
        );
    }
    if caps.cam == Some(true) {
        r = r.push(
            toggler(cam)
                .label(t!("videoconf.cam").into_owned())
                .on_toggle(|_| m(Message::ToggleCam))
                .style(style::toggler)
                .size(18.0)
                .text_size(TEXT_CAPTION),
        );
    }
    r.into()
}

/// One popup card.
fn popup<'a>(
    heading: String,
    avatar_el: Element<'a, AppMsg>,
    title: String,
    subtitle: Option<String>,
    extra: Option<Element<'a, AppMsg>>,
    buttons: Vec<Element<'a, AppMsg>>,
    on_close: Option<AppMsg>,
) -> Element<'a, AppMsg> {
    let mut head = row![
        text(heading)
            .size(TEXT_CAPTION)
            .style(style::text_secondary)
            .width(Length::Fill)
    ]
    .align_y(Alignment::Center);
    if let Some(c) = on_close {
        head = head.push(icon_button_with(
            Icon::X,
            Some(c),
            t!("videoconf.mute_dismiss").into_owned(),
            false,
            24.0,
        ));
    }
    let mut who =
        column![text(title).size(TEXT_SUBTITLE).font(FONT_SEMIBOLD)].spacing(SPACE_1 / 2.0);
    if let Some(s) = subtitle {
        who = who.push(text(s).size(TEXT_CAPTION).style(style::text_secondary));
    }
    let mut col = column![
        head,
        row![avatar_el, who.width(Length::Fill)]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
    ]
    .spacing(SPACE_3);
    if let Some(e) = extra {
        col = col.push(e);
    }
    col = col.push(
        row![space().width(Length::Fill)]
            .extend(buttons)
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
    );
    container(col)
        .padding(SPACE_4)
        .width(Length::Fixed(320.0))
        .style(style::container_popover)
        .into()
}

fn room_avatar<'a>(
    images: &Images,
    sid: &ServerId,
    s: &crate::workspace::ServerState,
    rid: &str,
    fallback_user: Option<&str>,
    name: &str,
) -> Element<'a, AppMsg> {
    let ws = &s.workspace;
    let partner = crate::room::header::dm_partner(ws, rid);
    let av = match (partner.as_deref().or(fallback_user), ws.room(rid)) {
        (Some(u), _) => Avatar::User {
            username: u.to_owned(),
        },
        (None, e) => Avatar::Room {
            rid: rid.to_owned(),
            etag: e
                .and_then(|e| e.room.as_ref())
                .and_then(|r| r.avatar_etag.clone()),
        },
    };
    avatar(
        images.avatar(sid, &s.url, &av, name),
        40.0,
        AvatarShape::Rounded,
    )
    .into()
}

/// Incoming / outgoing popups of every server.
fn popups(app: &App) -> Vec<Element<'_, AppMsg>> {
    let mut out = Vec::new();
    for (sid, s) in &app.servers {
        let Some(mgr) = app.calls.manager(sid) else {
            continue;
        };
        for c in mgr.incoming() {
            let room = room_name(&s.workspace, &c.rid);
            let caller = c.caller.clone().unwrap_or_else(|| room.clone());
            let accepting = c.accept_deadline.is_some();
            let buttons: Vec<Element<'_, AppMsg>> = vec![
                danger_button(
                    t!("videoconf.decline").into_owned(),
                    (!accepting).then(|| m(Message::Decline(sid.clone(), c.call_id.clone()))),
                )
                .icon(Icon::PhoneOff)
                .into(),
                primary_button(
                    t!("videoconf.accept").into_owned(),
                    (!accepting).then(|| m(Message::Accept(sid.clone(), c.call_id.clone()))),
                )
                .icon(Icon::Phone)
                .loading(accepting)
                .into(),
            ];
            out.push(popup(
                t!("videoconf.incoming_title").into_owned(),
                room_avatar(&app.images, sid, s, &c.rid, None, &caller),
                caller.clone(),
                (room != caller && !room.is_empty()).then_some(room),
                Some(toggles(
                    c.capabilities.as_ref(),
                    app.calls.mic,
                    app.calls.cam,
                )),
                buttons,
                Some(m(Message::Mute(sid.clone(), c.call_id.clone()))),
            ));
        }
        if let Some(o) = mgr.outgoing().filter(|o| !o.joined) {
            let room = room_name(&s.workspace, &o.rid);
            out.push(popup(
                t!("videoconf.outgoing_title").into_owned(),
                room_avatar(&app.images, sid, s, &o.rid, None, &room),
                room,
                Some(t!("videoconf.waiting").into_owned()),
                None,
                vec![
                    danger_button(
                        t!("videoconf.hang_up").into_owned(),
                        Some(m(Message::Hangup(sid.clone()))),
                    )
                    .icon(Icon::PhoneOff)
                    .into(),
                ],
                None,
            ));
        }
    }
    out
}

fn dialog_view<'a>(app: &'a App, sid: &'a ServerId, d: &'a Dialog) -> Element<'a, AppMsg> {
    let close = Some(m(Message::CloseDialog));
    match d {
        Dialog::Start { rid, caps, busy } => {
            let room = app
                .servers
                .get(sid)
                .map(|s| room_name(&s.workspace, rid))
                .unwrap_or_default();
            let body = column![
                row![
                    icon::sized(Icon::Video, 20.0).style(style::text_secondary),
                    text(room).size(TEXT_BODY).font(FONT_SEMIBOLD),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
                text(t!("videoconf.start_hint").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary),
                toggles(Some(caps), app.calls.mic, app.calls.cam),
            ]
            .spacing(SPACE_3);
            dialog(
                t!("videoconf.start_title").into_owned(),
                body,
                vec![
                    secondary_button(t!("videoconf.cancel").into_owned(), close.clone()).into(),
                    primary_button(
                        t!("videoconf.start").into_owned(),
                        (!busy).then(|| m(Message::Confirm)),
                    )
                    .icon(Icon::Video)
                    .loading(*busy)
                    .into(),
                ],
                close,
            )
        }
        Dialog::NoProvider { code, admin } => {
            let (title, body) = provider_error_text(code, *admin);
            let mut actions: Vec<Element<'a, AppMsg>> =
                vec![secondary_button(t!("videoconf.close").into_owned(), close.clone()).into()];
            if *admin {
                actions.push(
                    primary_button(
                        t!("videoconf.open_settings").into_owned(),
                        Some(m(Message::OpenAdmin(sid.clone()))),
                    )
                    .icon(Icon::ExternalLink)
                    .into(),
                );
            }
            dialog(
                title,
                row![
                    icon::sized(Icon::VideoOff, 28.0).style(style::text_secondary),
                    text(body).size(TEXT_BODY).width(Length::Fill),
                ]
                .spacing(SPACE_3)
                .align_y(Alignment::Start),
                actions,
                close,
            )
        }
        Dialog::Info { call_id } => {
            let state = app
                .servers
                .get(sid)
                .and_then(|s| s.workspace.videoconf().state(call_id));
            let body: Element<'a, AppMsg> = match state {
                Some(InfoState::Loaded(v)) => {
                    let mut c: Column<'a, AppMsg> = column![].spacing(SPACE_2);
                    let line = |k: String, v: String| {
                        row![
                            text(k)
                                .size(TEXT_CAPTION)
                                .style(style::text_secondary)
                                .width(Length::Fixed(120.0)),
                            text(v).size(TEXT_BODY).width(Length::Fill),
                        ]
                        .spacing(SPACE_2)
                    };
                    let status = match block::shown(v) {
                        block::Shown::Calling => t!("videoconf.calling"),
                        block::Shown::Ongoing => t!("videoconf.ongoing"),
                        block::Shown::Ended { .. } => t!("videoconf.ended"),
                    };
                    if let Some(title) = v.title.clone().filter(|t| !t.is_empty()) {
                        c = c.push(line(t!("videoconf.info_title_label").into_owned(), title));
                    }
                    c = c.push(line(
                        t!("videoconf.info_status").into_owned(),
                        status.into_owned(),
                    ));
                    if !v.provider_name.is_empty() {
                        c = c.push(line(
                            t!("videoconf.info_provider").into_owned(),
                            v.provider_name.clone(),
                        ));
                    }
                    if let Some(by) = &v.created_by {
                        c = c.push(line(
                            t!("videoconf.info_started_by").into_owned(),
                            by.name.clone().or(by.username.clone()).unwrap_or_default(),
                        ));
                    }
                    let joined: Vec<String> = v
                        .joined_users()
                        .filter_map(|u| u.name.clone().or(u.username.clone()))
                        .collect();
                    if !joined.is_empty() {
                        c = c.push(line(
                            t!("videoconf.info_participants").into_owned(),
                            joined.join(", "),
                        ));
                    }
                    if let Some(url) = v.url.clone().filter(|u| !u.is_empty()) {
                        c = c.push(line(t!("videoconf.info_url").into_owned(), url));
                    }
                    if let Some(d) = v.discussion_rid.clone() {
                        c = c.push(
                            secondary_button(
                                t!("videoconf.discussion").into_owned(),
                                Some(m(Message::OpenRoom(sid.clone(), d))),
                            )
                            .icon(Icon::MessagesSquare),
                        );
                    }
                    c.into()
                }
                Some(InfoState::Failed) => text(t!("videoconf.unavailable").into_owned())
                    .size(TEXT_BODY)
                    .into(),
                _ => text(t!("videoconf.loading").into_owned())
                    .size(TEXT_BODY)
                    .style(style::text_secondary)
                    .into(),
            };
            dialog(
                t!("videoconf.info_dialog_title").into_owned(),
                body,
                vec![secondary_button(t!("videoconf.close").into_owned(), close.clone()).into()],
                close,
            )
        }
    }
}

/// Wraps the app content with the call dialog (modal) and the popups.
pub fn layer<'a>(app: &'a App, base: Element<'a, AppMsg>) -> Element<'a, AppMsg> {
    let (overlay, dismiss) = match &app.calls.dialog {
        Some((sid, d)) => (
            Some(dialog_view(app, sid, d)),
            Some(m(Message::CloseDialog)),
        ),
        None => (None, None),
    };
    let base = modal(base, overlay, dismiss);
    let cards = popups(app);
    let corner: Element<'a, AppMsg> = if cards.is_empty() {
        space().into()
    } else {
        container(Column::with_children(cards).spacing(SPACE_2))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(SPACE_5)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .into()
    };
    stack![base, corner].into()
}
