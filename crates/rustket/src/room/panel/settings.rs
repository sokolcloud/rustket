//! Room settings (`rooms.saveRoomSettings`, `rooms.changeArchivationState`),
//! each field gated by the permission the server checks.

use iced::widget::{column, scrollable, text, toggler};
use iced::{Element, Length};
use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::permissions::{self, ids};
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, banner, primary_button, secondary_button, text_field};

use super::api::{self, RoomSettings};
use super::common;
use crate::error::Res;
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Topic,
    Announcement,
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Private,
    ReadOnly,
    ReactWhenReadOnly,
    Archived,
    HideSystemMessages,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Text(Field, String),
    Flag(Flag, bool),
    Reset,
    Save,
    Saved(Res<()>),
}

/// What the current user may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Allowed {
    pub edit: bool,
    pub change_type: bool,
    pub read_only: bool,
    pub react_when_read_only: bool,
    pub archive: bool,
    pub unarchive: bool,
}

impl Allowed {
    /// The web client's `useEditRoomPermissions`.
    pub fn compute(ws: &Workspace, rid: &str) -> Self {
        let Some(entry) = ws.room(rid) else {
            return Self::default();
        };
        let t = entry.room_type();
        let edit = permissions::can_edit_room(ws, rid) && t != RoomType::Direct;
        let is_admin = ws.me().is_some_and(|m| m.has_role("admin"));
        let is_default = entry.room.as_ref().and_then(|r| r.default) == Some(true);
        let change_type = (!is_default || is_admin)
            && ((t == RoomType::Private && permissions::can_create_channel(ws))
                || (t == RoomType::Channel && permissions::can_create_group(ws)));
        Self {
            edit,
            change_type: edit && change_type,
            read_only: edit && permissions::has_permission(ws, "set-readonly", Some(rid)),
            react_when_read_only: edit
                && permissions::has_permission(ws, "set-react-when-readonly", Some(rid)),
            archive: permissions::has_permission(ws, ids::ARCHIVE_ROOM, Some(rid)),
            unarchive: permissions::has_permission(ws, ids::UNARCHIVE_ROOM, Some(rid)),
        }
    }
}

#[derive(Default)]
pub struct Settings {
    form: Option<RoomSettings>,
    before: Option<RoomSettings>,
    saving: bool,
    error: Option<String>,
    name_error: Option<String>,
}

impl Settings {
    fn load(&mut self, ws: &Workspace, rid: &str) {
        let s = ws
            .room(rid)
            .and_then(|e| e.room.as_ref())
            .map(RoomSettings::from_room);
        self.before = s.clone();
        self.form = s;
        self.error = None;
        self.name_error = None;
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.load(ctx.workspace, ctx.rid);
        Action::none()
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        // Pick up remote changes while the form is untouched.
        if changes.rooms.contains(ctx.rid) && self.form == self.before && !self.saving {
            self.load(ctx.workspace, ctx.rid);
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Text(field, v) => {
                if let Some(f) = &mut self.form {
                    match field {
                        Field::Name => {
                            f.name = v;
                            self.name_error = None;
                        }
                        Field::Topic => f.topic = v,
                        Field::Announcement => f.announcement = v,
                        Field::Description => f.description = v,
                    }
                }
                Action::none()
            }
            Msg::Flag(flag, v) => {
                if let Some(f) = &mut self.form {
                    match flag {
                        Flag::Private => f.private = v,
                        Flag::ReadOnly => f.read_only = v,
                        Flag::ReactWhenReadOnly => f.react_when_read_only = v,
                        Flag::Archived => f.archived = v,
                        Flag::HideSystemMessages => f.hide_system_messages = v,
                    }
                }
                Action::none()
            }
            Msg::Reset => {
                self.load(ctx.workspace, ctx.rid);
                Action::none()
            }
            Msg::Save => {
                let (Some(form), Some(before)) = (self.form.clone(), self.before.clone()) else {
                    return Action::none();
                };
                if self.saving {
                    return Action::none();
                }
                let is_discussion = ctx
                    .workspace
                    .room(ctx.rid)
                    .is_some_and(|e| e.is_discussion());
                if let Some(key) = form.validate(is_discussion) {
                    self.name_error = Some(t!(key).into_owned());
                    return Action::none();
                }
                let payload = form.payload(ctx.rid, &before);
                let archive = (form.archived != before.archived).then_some(form.archived);
                if payload.is_none() && archive.is_none() {
                    return Action::none();
                }
                self.saving = true;
                self.error = None;
                let rid = ctx.rid.to_owned();
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        if let Some(body) = payload {
                            api::save_room_settings(rest.clone(), body).await?;
                        }
                        if let Some(a) = archive {
                            api::set_archived(rest, rid, a).await?;
                        }
                        Ok(())
                    },
                    Msg::Saved,
                ))
            }
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(()) => {
                        self.before = self.form.clone();
                        Action::effect(common::ok_toast(t!("room_panel.room_saved").into_owned()))
                            .with(Effect::OpenPanel(super::Kind::Info))
                    }
                    Err(e) => {
                        let msg = common::failure_text(&e);
                        if matches!(
                            e.error().api_error_code(),
                            Some("error-invalid-room-name" | "error-duplicate-channel-name")
                        ) {
                            self.name_error = Some(msg);
                        } else {
                            self.error = Some(msg);
                        }
                        Action::none()
                    }
                }
            }
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let allowed = Allowed::compute(ctx.workspace, ctx.rid);
        let Some(f) = &self.form else {
            return common::empty(Icon::Settings, t!("room_panel.room_unknown").into_owned());
        };
        if !allowed.edit {
            return common::empty(Icon::Lock, t!("room_panel.err_not_allowed").into_owned());
        }
        let input = |label: String,
                     value: &'a str,
                     field: Field,
                     err: Option<String>|
         -> Element<'a, Msg> {
            text_field(label, value, move |v| Msg::Text(field, v))
                .error(err)
                .into()
        };
        let toggle = |label: String,
                      hint: Option<String>,
                      value: bool,
                      flag: Flag,
                      enabled: bool|
         -> Element<'a, Msg> {
            let mut c = column![
                toggler(value)
                    .label(label)
                    .text_size(TEXT_BODY)
                    .on_toggle_maybe(enabled.then_some(move |v| Msg::Flag(flag, v)))
                    .style(style::toggler)
                    .width(Length::Fill)
            ]
            .spacing(2);
            if let Some(h) = hint {
                c = c.push(text(h).size(TEXT_CAPTION).style(style::text_hint));
            }
            c.into()
        };
        let mut col = column![].spacing(SPACE_3).padding(SPACE_4);
        if let Some(e) = &self.error {
            col = col.push(banner(BannerKind::Error, e.clone(), None::<(String, Msg)>));
        }
        col = col
            .push(input(
                t!("room_panel.name").into_owned(),
                &f.name,
                Field::Name,
                self.name_error.clone(),
            ))
            .push(input(
                t!("room_panel.topic").into_owned(),
                &f.topic,
                Field::Topic,
                None,
            ))
            .push(input(
                t!("room_panel.announcement").into_owned(),
                &f.announcement,
                Field::Announcement,
                None,
            ))
            .push(input(
                t!("room_panel.description").into_owned(),
                &f.description,
                Field::Description,
                None,
            ))
            .push(toggle(
                t!("room_panel.private").into_owned(),
                Some(t!("room_panel.private_hint").into_owned()),
                f.private,
                Flag::Private,
                allowed.change_type,
            ))
            .push(toggle(
                t!("room_panel.read_only").into_owned(),
                Some(t!("room_panel.read_only_hint").into_owned()),
                f.read_only,
                Flag::ReadOnly,
                allowed.read_only,
            ))
            .push(toggle(
                t!("room_panel.react_when_read_only").into_owned(),
                None,
                f.react_when_read_only,
                Flag::ReactWhenReadOnly,
                allowed.react_when_read_only && f.read_only,
            ))
            .push(toggle(
                t!("room_panel.archived").into_owned(),
                None,
                f.archived,
                Flag::Archived,
                if self.before.as_ref().is_some_and(|b| b.archived) {
                    allowed.unarchive
                } else {
                    allowed.archive
                },
            ))
            .push(toggle(
                t!("room_panel.hide_system_messages").into_owned(),
                None,
                f.hide_system_messages,
                Flag::HideSystemMessages,
                true,
            ));
        let dirty = self.form != self.before;
        col = col.push(
            iced::widget::row![
                secondary_button(
                    t!("room_panel.reset").into_owned(),
                    dirty.then_some(Msg::Reset)
                )
                .width(Length::Fill),
                primary_button(
                    t!("room_panel.save").into_owned(),
                    dirty.then_some(Msg::Save)
                )
                .loading(self.saving)
                .width(Length::Fill),
            ]
            .spacing(SPACE_3),
        );
        scrollable(col)
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}
