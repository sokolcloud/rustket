//! Edit room information (`docs/omnichannel.md` §8.1, web `RoomEdit`): topic,
//! tags and the room custom fields. Always sends the full tag list (omitted
//! tags are cleared by the server).

use std::collections::BTreeMap;

use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::SaveRoomInfo;
use rust_i18n::t;
use rustket_core::omnichannel::permissions as perm;
use rustket_core::omnichannel::room::can_edit_room_info;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_SMALL};
use rustket_ui::widget::{
    ChipKind, chip, divider, icon_button, primary_button, secondary_button, text_field,
};

use super::PanelKind;
use crate::error::Res;
use crate::room::panel::common;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx, panel};

#[derive(Debug, Clone)]
pub enum Msg {
    Topic(String),
    TagInput(String),
    AddTag,
    RemoveTag(String),
    Field(String, String),
    Save,
    Cancel,
    Saved(Res<()>),
}

#[derive(Default)]
pub struct EditPanel {
    loaded: bool,
    topic: String,
    tags: Vec<String>,
    tag_input: String,
    /// Room custom field values by field id.
    fields: BTreeMap<String, String>,
    busy: bool,
    error: Option<String>,
}

impl EditPanel {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(room) = ctx.workspace.room(ctx.rid).and_then(|e| e.room.as_ref()) {
            self.topic = room.topic.clone().unwrap_or_default();
            self.tags = room.tags.clone().unwrap_or_default();
            if let Some(obj) = room
                .livechat_data
                .as_ref()
                .and_then(serde_json::Value::as_object)
            {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        self.fields.insert(k.clone(), s.to_owned());
                    }
                }
            }
            self.loaded = true;
        }
        Action::none()
    }

    fn add_tag(&mut self) {
        let tag = self.tag_input.trim().to_owned();
        if !tag.is_empty() && !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
        self.tag_input.clear();
    }

    /// The request body for `room` (custom fields only when they may be shown).
    pub fn body(&self, room: &rocketchat::models::Room, with_fields: bool) -> SaveRoomInfo {
        let mut b = SaveRoomInfo::from_room(room);
        b.room.topic = Some(self.topic.trim().to_owned());
        let mut tags = self.tags.clone();
        tags.sort();
        b.room.tags = tags;
        if with_fields && !self.fields.is_empty() {
            b.room.livechat_data = Some(self.fields.clone());
        }
        b
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Topic(s) => self.topic = s,
            Msg::TagInput(s) => self.tag_input = s,
            Msg::AddTag => self.add_tag(),
            Msg::RemoveTag(tag) => self.tags.retain(|t| *t != tag),
            Msg::Field(k, v) => {
                self.fields.insert(k, v);
            }
            Msg::Cancel => {
                return Action::effect(Effect::OpenPanel(panel::Kind::Omnichannel(
                    PanelKind::Info,
                )));
            }
            Msg::Save if !self.busy => {
                if !self.tag_input.trim().is_empty() {
                    self.add_tag();
                }
                let Some(room) = ctx.workspace.room(ctx.rid).and_then(|e| e.room.clone()) else {
                    return Action::none();
                };
                let with_fields = has_permission(
                    ctx.workspace,
                    perm::VIEW_LIVECHAT_ROOM_CUSTOMFIELDS,
                    Some(ctx.rid),
                );
                let body = self.body(&room, with_fields);
                self.busy = true;
                self.error = None;
                return Action::task(ctx.rest_task(
                    move |rest| async move { rest.livechat_save_room_info(&body).await },
                    Msg::Saved,
                ));
            }
            Msg::Save => {}
            Msg::Saved(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        return Action::effect(common::ok_toast(
                            t!("omnichannel.saved").into_owned(),
                        ))
                        .with(Effect::OpenPanel(panel::Kind::Omnichannel(PanelKind::Info)));
                    }
                    Err(f) => self.error = Some(crate::omnichannel::error_text(&f)),
                }
            }
        }
        Action::none()
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let Some(room) = ws.room(ctx.rid).and_then(|e| e.room.as_ref()) else {
            return common::loading();
        };
        if !self.loaded {
            return common::loading();
        }
        let allowed = can_edit_room_info(ws, room);
        let enabled = allowed && !self.busy;
        let mut col: Column<'a, Msg> = column![
            text_field(
                t!("omnichannel.info.topic").into_owned(),
                &self.topic,
                Msg::Topic
            )
            .disabled(!enabled),
        ]
        .spacing(SPACE_4)
        .padding(SPACE_4);
        let mut tags_row = row![].spacing(SPACE_1).align_y(Alignment::Center);
        for tag in &self.tags {
            tags_row = tags_row.push(
                row![
                    chip(tag.clone(), ChipKind::Neutral),
                    icon_button(
                        Icon::X,
                        enabled.then(|| Msg::RemoveTag(tag.clone())),
                        t!("omnichannel.remove_tag"),
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }
        col = col.push(
            column![
                row![
                    text_field(
                        t!("omnichannel.info.tags").into_owned(),
                        &self.tag_input,
                        Msg::TagInput
                    )
                    .placeholder(t!("omnichannel.tag_placeholder"))
                    .on_submit(Msg::AddTag)
                    .disabled(!enabled),
                    secondary_button(
                        t!("omnichannel.add_tag").into_owned(),
                        (enabled && !self.tag_input.trim().is_empty()).then_some(Msg::AddTag)
                    ),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::End),
                tags_row.wrap(),
            ]
            .spacing(SPACE_1),
        );
        if has_permission(ws, perm::VIEW_LIVECHAT_ROOM_CUSTOMFIELDS, Some(ctx.rid)) {
            let omni = crate::omnichannel::room::omni_of(ctx.server_id);
            for f in omni
                .custom_fields
                .iter()
                .filter(|f| f.is_visible_room_field())
            {
                let id = f.id.clone();
                let value = self.fields.get(&f.id).map(String::as_str).unwrap_or("");
                col = col.push(
                    text_field(f.label.clone(), value, move |v| Msg::Field(id.clone(), v))
                        .disabled(!enabled),
                );
            }
        }
        if !allowed {
            col = col.push(
                text(t!("omnichannel.edit_not_allowed").into_owned())
                    .size(TEXT_SMALL)
                    .style(style::text_hint),
            );
        }
        if let Some(e) = &self.error {
            col = col.push(text(e.clone()).size(TEXT_SMALL).style(style::text_danger));
        }
        column![
            scrollable(col)
                .height(Length::Fill)
                .style(style::scrollable),
            divider(),
            container(
                row![
                    iced::widget::space().width(Length::Fill),
                    secondary_button(t!("omnichannel.cancel").into_owned(), Some(Msg::Cancel)),
                    primary_button(
                        t!("omnichannel.save").into_owned(),
                        enabled.then_some(Msg::Save)
                    )
                    .loading(self.busy),
                ]
                .spacing(SPACE_2)
            )
            .padding(SPACE_3),
        ]
        .height(Length::Fill)
        .into()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn body_keeps_all_tags() {
        let room: rocketchat::models::Room = serde_json::from_value(json!({
            "_id":"r","t":"l","topic":"old","tags":["b","a"],"v":{"_id":"vis"}
        }))
        .unwrap();
        let mut p = EditPanel {
            topic: " new ".into(),
            tags: vec!["b".into(), "a".into()],
            ..Default::default()
        };
        p.tag_input = "c".into();
        p.add_tag();
        let b = serde_json::to_value(p.body(&room, false)).unwrap();
        assert_eq!(b["roomData"]["tags"], json!(["a", "b", "c"]));
        assert_eq!(b["roomData"]["topic"], "new");
        assert_eq!(b["guestData"], json!({"_id": "vis"}));
        p.tags.clear();
        let b = serde_json::to_value(p.body(&room, false)).unwrap();
        assert_eq!(b["roomData"]["tags"], json!([]), "sent even when empty");
    }
}
