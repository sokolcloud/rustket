//! Rooms (docs/admin.md §5): the admin room list (search, type filter, sort,
//! pages), the edit dialog (`rooms.saveRoomSettings` with dirty fields plus
//! `default`/`favorite`, archive as a separate call) and deletion.

use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Task};
use rocketchat::models::admin::{AdminRoom, AdminRoomType, FavoriteSetting, RoomSettingsUpdate};
use rocketchat::models::{Paginated, RoomType};
use rust_i18n::t;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL, font_medium};
use rustket_ui::widget::danger_button;

use super::common::{
    Confirm, Cx, List, ListMsg, Step, date, err_text, field, form_dialog, input, kv, list_view,
    muted, num, search_bar, small, sortable, switch, table, title_row, yes_no,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F {
    Name,
    Topic,
    Announcement,
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Private,
    ReadOnly,
    Archived,
    Default,
    Favorite,
    Featured,
}

#[derive(Debug, Clone)]
pub enum Msg {
    List(ListMsg),
    Loaded(u64, Res<Paginated<AdminRoom>>),
    Type(AdminRoomType, bool),
    Open(String),
    Room(Res<Box<AdminRoom>>),
    CloseDialog,
    Field(F, String),
    Flag(Flag, bool),
    Save,
    Saved(Res<()>),
    Delete,
    DeleteNow(String, Option<String>),
    Deleted(Res<()>),
}

#[derive(Debug, Clone, Default)]
pub struct Form {
    pub room: AdminRoom,
    pub name: String,
    /// The value `name` was pre-filled with.
    pub base_name: Option<String>,
    pub topic: String,
    pub announcement: String,
    pub description: String,
    pub private: bool,
    pub read_only: bool,
    pub archived: bool,
    pub default: bool,
    pub favorite: bool,
    pub featured: bool,
    pub busy: bool,
    pub error: Option<String>,
}

/// The editable room name like the web's `roomName`: `fname` for
/// discussions and federated rooms, `fname || name` when special-character
/// names are allowed, else `name`.
fn base_name(r: &AdminRoom, special_chars: bool) -> Option<String> {
    if r.prid.is_some() || r.federated == Some(true) {
        r.fname.clone()
    } else if special_chars {
        r.fname
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| r.name.clone())
    } else {
        r.name.clone()
    }
}

impl Form {
    pub fn of(r: AdminRoom, special_chars: bool) -> Self {
        let base = base_name(&r, special_chars);
        Self {
            name: base.clone().unwrap_or_default(),
            base_name: base,
            topic: r.topic.clone().unwrap_or_default(),
            announcement: r.announcement.clone().unwrap_or_default(),
            description: r.description.clone().unwrap_or_default(),
            private: r.t == RoomType::Private,
            read_only: r.ro.unwrap_or(false),
            archived: r.archived.unwrap_or(false),
            default: r.default.unwrap_or(false),
            favorite: r.favorite.unwrap_or(false),
            featured: r.featured.unwrap_or(false),
            room: r,
            busy: false,
            error: None,
        }
    }

    fn is_dm(&self) -> bool {
        matches!(self.room.t, RoomType::Direct | RoomType::Livechat)
    }

    /// Dirty fields, plus `default` and `favorite` always (like the web).
    pub fn update(&self) -> RoomSettingsUpdate {
        let r = &self.room;
        let ch = |new: &str, old: &Option<String>| {
            (new.trim() != old.as_deref().unwrap_or("").trim()).then(|| new.trim().to_owned())
        };
        let mut up = RoomSettingsUpdate {
            room_name: ch(&self.name, &self.base_name),
            room_topic: ch(&self.topic, &r.topic),
            room_announcement: ch(&self.announcement, &r.announcement),
            room_description: ch(&self.description, &r.description),
            default: Some(self.default),
            favorite: Some(FavoriteSetting {
                default_value: self.default,
                favorite: self.default && self.favorite,
            }),
            ..RoomSettingsUpdate::default()
        };
        if self.is_dm() {
            up.room_name = None;
        }
        if self.private != (r.t == RoomType::Private) && !self.is_dm() {
            up.room_type = Some(if self.private { "p" } else { "c" }.into());
        }
        if self.read_only != r.ro.unwrap_or(false) && r.broadcast != Some(true) {
            up.read_only = Some(self.read_only);
        }
        if self.featured != r.featured.unwrap_or(false) {
            up.featured = Some(self.featured);
        }
        up
    }

    pub fn archive_change(&self) -> Option<bool> {
        (self.archived != self.room.archived.unwrap_or(false)).then_some(self.archived)
    }
}

pub struct State {
    pub list: List<AdminRoom>,
    pub types: Vec<AdminRoomType>,
    pub form: Option<Form>,
    /// Room whose details were requested last.
    pub opening: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            list: List::new(Some(("name", true))),
            types: Vec::new(),
            form: None,
            opening: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        self.fetch(cx)
    }

    fn fetch(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let req = self.list.begin();
        let filter = self.list.query.trim().to_owned();
        // No filter: all six types (the server drops discussions/teams otherwise).
        let types = if self.types.is_empty() {
            AdminRoomType::ALL.to_vec()
        } else {
            self.types.clone()
        };
        let page = self.list.rest_page();
        cx.ctx.rest_task(
            move |r| async move { r.admin_rooms(&filter, &types, &page).await },
            move |r| Message::Rooms(Msg::Loaded(req, r)),
        )
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match m {
            Msg::List(m) => match self.list.update(m) {
                Step::None => Task::none(),
                Step::Wait(t) => t.map(|m| Message::Rooms(Msg::List(m))),
                Step::Load => self.fetch(cx),
            },
            Msg::Loaded(req, r) => {
                if self.list.finish(req, r.map_err(|f| err_text(&f, cx.i18n))) {
                    return self.fetch(cx);
                }
                Task::none()
            }
            Msg::Type(t, on) => {
                self.types.retain(|x| *x != t);
                if on {
                    self.types.push(t);
                }
                self.list.offset = 0;
                self.fetch(cx)
            }
            Msg::Open(rid) => {
                self.opening = Some(rid.clone());
                cx.ctx.rest_task(
                    move |r| async move { r.admin_room(&rid).await.map(Box::new) },
                    |r| Message::Rooms(Msg::Room(r)),
                )
            }
            Msg::Room(r) => {
                // Only the last clicked row opens.
                let wanted = self.opening.take();
                match r {
                    Ok(room) if wanted.as_deref() == Some(room.id.as_str()) => {
                        let special = cx
                            .ctx
                            .workspace
                            .settings()
                            .bool_or("UI_Allow_room_names_with_special_chars", false);
                        self.form = Some(Form::of(*room, special));
                    }
                    Ok(_) => self.opening = wanted,
                    Err(f) => {
                        if wanted.is_some() {
                            cx.fail(&f);
                        }
                    }
                }
                Task::none()
            }
            Msg::CloseDialog => {
                self.form = None;
                Task::none()
            }
            Msg::Field(f, v) => {
                if let Some(form) = &mut self.form {
                    form.error = None;
                    match f {
                        F::Name => form.name = v,
                        F::Topic => form.topic = v,
                        F::Announcement => form.announcement = v,
                        F::Description => form.description = v,
                    }
                }
                Task::none()
            }
            Msg::Flag(f, v) => {
                if let Some(form) = &mut self.form {
                    match f {
                        Flag::Private => form.private = v,
                        Flag::ReadOnly => form.read_only = v,
                        Flag::Archived => form.archived = v,
                        Flag::Default => form.default = v,
                        Flag::Favorite => form.favorite = v,
                        Flag::Featured => form.featured = v,
                    }
                }
                Task::none()
            }
            Msg::Save => {
                let Some(form) = &mut self.form else {
                    return Task::none();
                };
                if !form.is_dm() && form.name.trim().is_empty() {
                    form.error = Some(t!("admin.rooms.name_required").into_owned());
                    return Task::none();
                }
                form.busy = true;
                let rid = form.room.id.clone();
                let up = form.update();
                let archive = form.archive_change();
                // Save first, then archive: clearer errors than the web's parallel calls.
                cx.ctx.rest_task(
                    move |r| async move {
                        r.admin_save_room_settings(&rid, &up).await?;
                        if let Some(a) = archive {
                            r.admin_change_archivation_state(&rid, a).await?;
                        }
                        Ok(())
                    },
                    |r| Message::Rooms(Msg::Saved(r)),
                )
            }
            Msg::Saved(r) => {
                match r {
                    Ok(()) => {
                        cx.ok(t!("admin.rooms.saved").into_owned());
                        self.form = None;
                        return self.fetch(cx);
                    }
                    Err(f) => {
                        let text = err_text(&f, cx.i18n);
                        if let Some(form) = &mut self.form {
                            form.busy = false;
                            form.error = Some(text);
                        }
                    }
                }
                Task::none()
            }
            Msg::Delete => {
                if let Some(form) = &self.form {
                    let team = form
                        .room
                        .is_team_main()
                        .then(|| form.room.team_id.clone())
                        .flatten();
                    let body = if team.is_some() {
                        t!("admin.rooms.delete_team_body")
                    } else {
                        t!("admin.rooms.delete_body")
                    };
                    cx.ask(Confirm::danger(
                        t!("admin.rooms.delete_title", name = form.room.display_name())
                            .into_owned(),
                        body.into_owned(),
                        Message::Rooms(Msg::DeleteNow(form.room.id.clone(), team)),
                    ));
                }
                Task::none()
            }
            Msg::DeleteNow(rid, team) => cx.ctx.rest_task(
                move |r| async move {
                    match team {
                        // The team's main room goes through `teams.delete`.
                        Some(team) => r.admin_delete_team(&team, &[]).await,
                        None => r.rooms_delete(&rid).await,
                    }
                },
                |r| Message::Rooms(Msg::Deleted(r)),
            ),
            Msg::Deleted(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.rooms.deleted").into_owned());
                    self.form = None;
                    return self.fetch(cx);
                }
                Task::none()
            }
        }
    }

    pub fn view<'a>(&'a self, _v: &V<'a>) -> Element<'a, Message> {
        let lm = |m| Message::Rooms(Msg::List(m));
        let mut types = row![muted(t!("admin.rooms.types"))]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        for ty in AdminRoomType::ALL {
            let on = self.types.contains(&ty);
            types = types.push(
                button(text(type_label(ty)).size(TEXT_SMALL))
                    .padding([4, 10])
                    .style(if on {
                        style::button_primary
                    } else {
                        style::button_secondary
                    })
                    .on_press(Message::Rooms(Msg::Type(ty, !on))),
            );
        }
        let rows = self
            .list
            .items
            .iter()
            .map(|r| {
                (
                    vec![
                        row![
                            icon::sized(room_icon(r), 16.0).style(style::text_secondary),
                            text(r.display_name()).size(TEXT_BODY).font(font_medium()),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center)
                        .into(),
                        small(room_kind(r)),
                        small(num(r.users_count)),
                        small(num(r.msgs)),
                        yes_no(r.default.unwrap_or(false)),
                        yes_no(r.featured.unwrap_or(false)),
                    ],
                    Some(Message::Rooms(Msg::Open(r.id.clone()))),
                )
            })
            .collect();
        let list = list_view(
            &self.list,
            move || {
                table(
                    vec![
                        sortable(t!("admin.rooms.name"), 4, "name"),
                        sortable(t!("admin.rooms.type"), 2, "t"),
                        sortable(t!("admin.rooms.users"), 1, "usersCount"),
                        sortable(t!("admin.rooms.msgs"), 1, "msgs"),
                        sortable(t!("admin.rooms.default"), 1, "default"),
                        sortable(t!("admin.rooms.featured"), 1, "featured"),
                    ],
                    self.list.sort,
                    |f| Message::Rooms(Msg::List(ListMsg::Sort(f))),
                    rows,
                )
            },
            lm,
            t!("admin.rooms.empty").into_owned(),
        );
        column![
            title_row(t!("admin.section.rooms").into_owned(), vec![]),
            search_bar(
                &self.list.query,
                t!("admin.rooms.search").into_owned(),
                move |s| lm(ListMsg::Search(s)),
                lm(ListMsg::Reload)
            ),
            types.wrap(),
            list,
        ]
        .spacing(SPACE_4)
        .into()
    }

    pub fn overlay<'a>(&'a self, v: &V<'a>) -> Option<Element<'a, Message>> {
        let f = self.form.as_ref()?;
        let ws = v.ctx.workspace;
        let can = |p: &str| rustket_core::permissions::has_permission(ws, p, Some(&f.room.id));
        let fm = |field: F| Some(move |s| Message::Rooms(Msg::Field(field, s)));
        let flag = |fl: Flag| Some(move |b| Message::Rooms(Msg::Flag(fl, b)));
        let dm = f.is_dm();
        let mut col = column![
            kv(
                t!("admin.rooms.owner").into_owned(),
                f.room
                    .u
                    .as_ref()
                    .and_then(|u| u.username.clone())
                    .unwrap_or_default()
            ),
            kv(t!("admin.rooms.created").into_owned(), date(f.room.ts)),
            kv(t!("admin.rooms.type").into_owned(), room_kind(&f.room)),
        ]
        .spacing(SPACE_3);
        if !dm {
            col = col
                .push(field(
                    t!("admin.rooms.name").into_owned(),
                    None,
                    input(&f.name, fm(F::Name)),
                ))
                .push(field(
                    t!("admin.rooms.topic").into_owned(),
                    None,
                    input(&f.topic, fm(F::Topic)),
                ))
                .push(field(
                    t!("admin.rooms.announcement").into_owned(),
                    None,
                    input(&f.announcement, fm(F::Announcement)),
                ))
                .push(field(
                    t!("admin.rooms.description").into_owned(),
                    None,
                    input(&f.description, fm(F::Description)),
                ))
                .push(switch(
                    t!("admin.rooms.private").into_owned(),
                    f.private,
                    flag(Flag::Private),
                ))
                .push(switch(
                    t!("admin.rooms.read_only").into_owned(),
                    f.read_only,
                    (f.room.broadcast != Some(true))
                        .then_some(move |b| Message::Rooms(Msg::Flag(Flag::ReadOnly, b))),
                ))
                .push(switch(
                    t!("admin.rooms.archived").into_owned(),
                    f.archived,
                    flag(Flag::Archived),
                ));
        }
        col = col
            .push(switch(
                t!("admin.rooms.default").into_owned(),
                f.default,
                flag(Flag::Default),
            ))
            .push(switch(
                t!("admin.rooms.favorite").into_owned(),
                f.default && f.favorite,
                f.default
                    .then_some(move |b| Message::Rooms(Msg::Flag(Flag::Favorite, b))),
            ))
            .push(switch(
                t!("admin.rooms.featured").into_owned(),
                f.featured,
                flag(Flag::Featured),
            ));
        let deletable = f.room.federated != Some(true)
            && (can(&format!("delete-{}", f.room.t.as_str())) || f.room.is_team_main());
        let extra = vec![
            danger_button(
                t!("admin.delete"),
                deletable.then_some(Message::Rooms(Msg::Delete)),
            )
            .icon(Icon::Trash2)
            .into(),
        ];
        Some(form_dialog(
            f.room.display_name(),
            column![col].spacing(SPACE_1).padding([0.0, SPACE_1]).into(),
            f.error.as_deref(),
            f.busy,
            Some(Message::Rooms(Msg::Save)),
            Message::Rooms(Msg::CloseDialog),
            extra,
        ))
    }
}

fn type_label(t: AdminRoomType) -> String {
    match t {
        AdminRoomType::Channel => t!("admin.rooms.t_channel"),
        AdminRoomType::Private => t!("admin.rooms.t_private"),
        AdminRoomType::Direct => t!("admin.rooms.t_direct"),
        AdminRoomType::Livechat => t!("admin.rooms.t_livechat"),
        AdminRoomType::Discussions => t!("admin.rooms.t_discussion"),
        AdminRoomType::Teams => t!("admin.rooms.t_team"),
    }
    .into_owned()
}

fn room_kind(r: &AdminRoom) -> String {
    if r.is_team_main() {
        return type_label(AdminRoomType::Teams);
    }
    if r.is_discussion() {
        return type_label(AdminRoomType::Discussions);
    }
    match r.t {
        RoomType::Channel => type_label(AdminRoomType::Channel),
        RoomType::Private => type_label(AdminRoomType::Private),
        RoomType::Direct => type_label(AdminRoomType::Direct),
        RoomType::Livechat => type_label(AdminRoomType::Livechat),
        _ => r.t.as_str().to_owned(),
    }
}

fn room_icon(r: &AdminRoom) -> Icon {
    if r.is_team_main() {
        Icon::Users
    } else if r.is_discussion() {
        Icon::MessagesSquare
    } else {
        match r.t {
            RoomType::Private => Icon::Lock,
            RoomType::Direct => Icon::AtSign,
            RoomType::Livechat => Icon::Headset,
            _ => Icon::Hash,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn room_update_is_dirty_fields_plus_default() {
        let room = AdminRoom {
            id: "r1".into(),
            t: RoomType::Channel,
            name: Some("dev".into()),
            topic: Some("t".into()),
            ..AdminRoom::default()
        };
        let mut f = Form::of(room, false);
        let up = f.update();
        assert!(up.room_name.is_none() && up.room_topic.is_none() && up.room_type.is_none());
        assert_eq!(up.default, Some(false));
        f.topic = "new".into();
        f.private = true;
        f.favorite = true; // ignored while not default
        let up = f.update();
        assert_eq!(up.room_topic.as_deref(), Some("new"));
        assert_eq!(up.room_type.as_deref(), Some("p"));
        assert!(!up.favorite.unwrap().favorite);
        f.archived = true;
        assert_eq!(f.archive_change(), Some(true));
    }

    #[test]
    fn discussion_name_is_fname() {
        let room = AdminRoom {
            id: "d1".into(),
            t: RoomType::Private,
            name: Some("FbWCfgBpLnQuSSSfb".into()),
            fname: Some("Retro agenda".into()),
            prid: Some("p".into()),
            ..AdminRoom::default()
        };
        let mut f = Form::of(room, false);
        assert_eq!(f.name, "Retro agenda");
        assert!(f.update().room_name.is_none());
        f.name = "Retro".into();
        assert_eq!(f.update().room_name.as_deref(), Some("Retro"));
    }
}
