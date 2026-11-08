//! Contact information (`docs/omnichannel.md` §9): the contact behind the
//! chat (`omnichannel/contacts.get`), its e-mails, phones and channels, and
//! the link to its chat history. Falls back to the visitor
//! (`livechat/visitors.info`) for rooms without a contact.

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Contact, Visitor};
use rust_i18n::t;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL};
use rustket_ui::widget::{ChipKind, chip, divider, ellipsis, secondary_button};

use super::PanelKind;
use super::info::{channel_text, source_icon};
use crate::error::Res;
use crate::room::panel::common::{self, field};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx, panel};

#[derive(Debug, Clone)]
pub enum Msg {
    Contact(Res<Option<Box<Contact>>>),
    Visitor(Res<Box<Visitor>>),
    Retry,
    Open(PanelKind),
}

#[derive(Default)]
pub struct ContactPanel {
    loading: bool,
    contact: Option<Contact>,
    visitor: Option<Visitor>,
    error: Option<String>,
}

impl ContactPanel {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let Some(room) = ctx.workspace.room(ctx.rid).and_then(|e| e.room.clone()) else {
            return Action::none();
        };
        self.loading = true;
        self.error = None;
        match room.contact_id.clone() {
            Some(cid) => Action::task(ctx.rest_task(
                move |rest| async move {
                    rest.omnichannel_contact(&cid)
                        .await
                        .map(|c| c.map(Box::new))
                },
                Msg::Contact,
            )),
            None => {
                match room.visitor().filter(|v| !v.id.is_empty()) {
                    Some(v) => {
                        Action::task(ctx.rest_task(
                            move |rest| async move {
                                rest.livechat_visitor_info(&v.id).await.map(Box::new)
                            },
                            Msg::Visitor,
                        ))
                    }
                    None => {
                        self.loading = false;
                        Action::none()
                    }
                }
            }
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Contact(r) => {
                self.loading = false;
                match r {
                    Ok(c) => self.contact = c.map(|c| *c),
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Visitor(r) => {
                self.loading = false;
                match r {
                    Ok(v) => self.visitor = Some(*v),
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
            Msg::Retry => return self.activate(ctx),
            Msg::Open(k) => {
                return Action::effect(Effect::OpenPanel(panel::Kind::Omnichannel(k)));
            }
        }
        Action::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        if self.loading {
            return common::loading();
        }
        if let Some(e) = &self.error {
            return common::error_state(e, Msg::Retry);
        }
        let (name, emails, phones, unknown) = match (&self.contact, &self.visitor) {
            (Some(c), _) => (
                c.name.clone().unwrap_or_default(),
                c.emails
                    .iter()
                    .map(|e| e.address.clone())
                    .collect::<Vec<_>>(),
                c.phones
                    .iter()
                    .map(|p| p.phone_number.clone())
                    .collect::<Vec<_>>(),
                c.unknown,
            ),
            (None, Some(v)) => (
                v.display_name().to_owned(),
                v.email().map(str::to_owned).into_iter().collect(),
                v.phone_number().map(str::to_owned).into_iter().collect(),
                false,
            ),
            (None, None) => {
                return common::empty(Icon::Contact, t!("omnichannel.no_contact").into_owned());
            }
        };
        let avatar = rustket_ui::widget::avatar(
            rustket_ui::widget::AvatarSource::initials(&name, ctx.rid),
            48.0,
            rustket_ui::widget::AvatarShape::Rounded,
        );
        let mut head = column![
            ellipsis(name.clone())
                .size(TEXT_BODY + 2.0)
                .font(FONT_SEMIBOLD)
        ]
        .spacing(SPACE_1)
        .width(Length::Fill);
        if unknown {
            head = head.push(chip(
                t!("omnichannel.unknown_contact_chip").into_owned(),
                ChipKind::Warning,
            ));
        }
        let mut col: Column<'a, Msg> = column![
            row![avatar, head]
                .spacing(SPACE_3)
                .align_y(Alignment::Center)
        ]
        .spacing(SPACE_4)
        .padding(SPACE_4);
        let list = |items: Vec<String>| -> Element<'a, Msg> {
            let mut c = column![].spacing(SPACE_1);
            for i in items {
                c = c.push(text(i).size(TEXT_BODY));
            }
            c.into()
        };
        if !emails.is_empty() {
            col = col.push(field(t!("omnichannel.emails").into_owned(), list(emails)));
        }
        if !phones.is_empty() {
            col = col.push(field(t!("omnichannel.phones").into_owned(), list(phones)));
        }
        if let Some(c) = &self.contact {
            if !c.channels.is_empty() {
                let mut chans = column![].spacing(SPACE_2);
                for ch in &c.channels {
                    let src = ch
                        .details
                        .clone()
                        .unwrap_or_else(|| ch.visitor.source.clone());
                    let label = channel_text(&rustket_core::omnichannel::room::channel_label(&src));
                    let mut r = row![
                        rustket_ui::icon::sized(source_icon(src.kind), 14.0),
                        text(label).size(TEXT_BODY).width(Length::Fill),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center);
                    if ch.blocked {
                        r = r.push(chip(
                            t!("omnichannel.blocked").into_owned(),
                            ChipKind::Danger,
                        ));
                    }
                    if ch.verified {
                        r = r.push(chip(
                            t!("omnichannel.verified").into_owned(),
                            ChipKind::Success,
                        ));
                    }
                    if let Some(ts) = ch.last_chat.as_ref().and_then(|l| l.ts) {
                        r = r.push(
                            text(common::time_label(Some(ts), ctx))
                                .size(TEXT_SMALL)
                                .style(style::text_secondary),
                        );
                    }
                    chans = chans.push(r);
                }
                col = col.push(field(t!("omnichannel.channels").into_owned(), chans));
            }
            if let Some(ts) = c.last_chat.as_ref().and_then(|l| l.ts) {
                col = col.push(field(
                    t!("omnichannel.last_chat").into_owned(),
                    text(common::date_time_label(ts, ctx)).size(TEXT_BODY),
                ));
            }
        }
        let can_history = self.contact.is_some()
            && has_permission(ctx.workspace, perm::VIEW_LIVECHAT_CONTACT_HISTORY, None);
        let mut actions = row![].spacing(SPACE_2);
        if can_history {
            actions = actions.push(
                secondary_button(
                    t!("omnichannel.history").into_owned(),
                    Some(Msg::Open(PanelKind::ContactHistory)),
                )
                .icon(Icon::History),
            );
        }
        column![
            scrollable(col)
                .height(Length::Fill)
                .style(style::scrollable),
            divider(),
            container(actions).padding(SPACE_3),
        ]
        .height(Length::Fill)
        .into()
    }
}
