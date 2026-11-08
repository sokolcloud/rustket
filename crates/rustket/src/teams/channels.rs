//! Team channels (§5.4): the team's child rooms (`teams.listRooms`) with
//! search and an All / Auto-join filter; per room auto-join, remove from team
//! and delete; "Create new" (`channels|groups.create` with `teamId`) and
//! "Add existing" (`rooms.autocomplete.availableForTeams` → `teams.addRooms`).

use std::collections::BTreeSet;

use iced::widget::{checkbox, column, container, row, scrollable, text, toggler};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Paginated, Room, RoomType};
use rocketchat::rest::{CreateRoom, Page, TeamRef};
use rust_i18n::t;
use rustket_core::teams;
use rustket_core::workspace::Changes;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{
    ChipKind, MenuEntry, MenuItem, Placement, chip, danger_button, dialog, ellipsis,
    icon_button_with, popover_menu, primary_button, secondary_button, text_field,
};

use super::chooser::can_delete_child;
use crate::error::Res;
use crate::room::panel::paged::{LoadState, PageReq, Paged};
use crate::room::panel::{common, debounce};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    AutoJoin,
}

/// A room operation that needs confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Remove,
    Delete,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Filter(Filter),
    Page(PageReq, Res<Paginated<Room>>),
    More,
    Retry,
    Open(String),
    Menu(Option<String>),
    AutoJoin(String, bool),
    AutoJoined(String, bool, Res<()>),
    Ask(String, Op),
    Confirm,
    CancelDialog,
    Done(Op, Res<()>),
    ConfirmJoin,
    Joined(String, Res<()>),
    // Add existing.
    ShowAdd,
    AddQuery(String),
    AddSearch(u64),
    AddResults(u64, Res<Vec<Room>>),
    AddToggle(String),
    AddConfirm,
    Added(Res<Vec<Room>>),
    // Create new.
    ShowCreate,
    CreateName(String),
    CreatePrivate(bool),
    CreateConfirm,
    /// The created room's name.
    Created(Res<String>),
}

#[derive(Debug, Default)]
struct AddDialog {
    query: String,
    seq: u64,
    results: Option<Result<Vec<Room>, String>>,
    selected: BTreeSet<String>,
    names: Vec<(String, String)>,
    busy: bool,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct CreateDialog {
    name: String,
    private: bool,
    busy: bool,
    error: Option<String>,
}

enum Dialog {
    Confirm { rid: String, name: String, op: Op },
    Join { rid: String, name: String },
    Add(AddDialog),
    Create(CreateDialog),
}

#[derive(Default)]
pub struct Channels {
    query: String,
    query_seq: u64,
    filter: Filter,
    list: Paged<Room>,
    menu: Option<String>,
    dialog: Option<Dialog>,
    busy: bool,
    pending_open: Option<String>,
}

fn team_id(ctx: &UpdateCtx<'_>) -> Option<String> {
    teams::team_id_of(ctx.workspace, ctx.rid).map(str::to_owned)
}

fn main_rid(ctx_ws: &rustket_core::Workspace, rid: &str) -> String {
    super::main_room(ctx_ws, rid).unwrap_or_else(|| rid.to_owned())
}

impl Channels {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&mut self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let Some(team) = team_id(ctx) else {
            self.list.finish(req, Ok(Paginated::default()));
            return Action::none();
        };
        let (q, auto) = (
            self.query.trim().to_owned(),
            self.filter == Filter::AutoJoin,
        );
        Action::task(ctx.rest_task(
            move |rest| async move {
                rest.teams_list_rooms(
                    TeamRef::Id(&team),
                    Some(&q),
                    auto,
                    Page::new(req.offset, super::PAGE),
                )
                .await
            },
            move |r| Msg::Page(req, r),
        ))
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(rid) = &self.pending_open
            && changes.rooms.contains(rid)
            && ctx.workspace.room(rid).is_some()
        {
            let rid = self.pending_open.take().unwrap_or_default();
            return Action::effect(Effect::OpenRoom(rid));
        }
        // A room of this team changed (moved in or out, auto-join): refresh.
        let team = team_id(ctx);
        let touched = changes.rooms.iter().any(|rid| {
            self.list.items.iter().any(|r| &r.id == rid)
                || (team.is_some() && teams::team_id_of(ctx.workspace, rid) == team.as_deref())
                    && rid != ctx.rid
        });
        if touched && !self.list.is_busy() && self.dialog.is_none() {
            return self.reload(ctx);
        }
        Action::none()
    }

    fn name_of(&self, rid: &str) -> String {
        self.list
            .items
            .iter()
            .find(|r| r.id == rid)
            .map(|r| r.display_name().to_owned())
            .unwrap_or_default()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Query(q) => {
                self.query = q;
                self.query_seq += 1;
                Action::task(debounce(Msg::Search(self.query_seq)))
            }
            Msg::Search(seq) if seq == self.query_seq => self.reload(ctx),
            Msg::Search(_) => Action::none(),
            Msg::Filter(f) => {
                if self.filter == f {
                    return Action::none();
                }
                self.filter = f;
                self.reload(ctx)
            }
            Msg::Page(req, r) => {
                self.list.finish(req, r);
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Open(rid) => {
                if ctx.workspace.subscription(&rid).is_some() {
                    return Action::effect(Effect::OpenRoom(rid));
                }
                let name = self.name_of(&rid);
                self.dialog = Some(Dialog::Join { rid, name });
                Action::none()
            }
            Msg::Menu(m) => {
                self.menu = m;
                Action::none()
            }
            Msg::AutoJoin(rid, on) => {
                self.menu = None;
                let r2 = rid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.teams_update_room(&r2, on).await.map(drop) },
                    move |r| Msg::AutoJoined(rid, on, r),
                ))
            }
            Msg::AutoJoined(rid, on, r) => match r {
                Ok(_) => {
                    if let Some(room) = self.list.items.iter_mut().find(|x| x.id == rid) {
                        room.team_default = Some(on);
                    }
                    if !on {
                        return Action::none();
                    }
                    // Over the user limit the server stops subscribing (§2.8).
                    let main = main_rid(ctx.workspace, ctx.rid);
                    let users = ctx
                        .workspace
                        .room(&main)
                        .and_then(|e| e.room.as_ref())
                        .and_then(|r| r.users_count)
                        .unwrap_or(0);
                    let limit = ctx
                        .workspace
                        .settings()
                        .i64_or("API_User_Limit", 500)
                        .max(0) as u64;
                    let text = if users > limit {
                        t!("teams.autojoin_limit")
                    } else {
                        t!("teams.autojoin_updated")
                    };
                    Action::effect(super::ok_toast(text.into_owned()))
                }
                Err(e) => Action::effect(super::error_toast(t!("teams.autojoin").into_owned(), &e)),
            },
            Msg::Ask(rid, op) => {
                self.menu = None;
                let name = self.name_of(&rid);
                self.dialog = Some(Dialog::Confirm { rid, name, op });
                Action::none()
            }
            Msg::CancelDialog => {
                if !self.busy {
                    self.dialog = None;
                }
                Action::none()
            }
            Msg::Confirm => {
                let Some(Dialog::Confirm { rid, op, .. }) = &self.dialog else {
                    return Action::none();
                };
                if self.busy {
                    return Action::none();
                }
                let (rid, op) = (rid.clone(), *op);
                let Some(team) = team_id(ctx) else {
                    return Action::none();
                };
                self.busy = true;
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        match op {
                            Op::Remove => rest
                                .teams_remove_room(TeamRef::Id(&team), &rid)
                                .await
                                .map(drop),
                            Op::Delete => rest.rooms_delete(&rid).await,
                        }
                    },
                    move |r| Msg::Done(op, r),
                ))
            }
            Msg::Done(op, r) => {
                self.busy = false;
                self.dialog = None;
                match r {
                    Ok(()) => {
                        let key = match op {
                            Op::Remove => "teams.room_removed",
                            Op::Delete => "teams.room_deleted",
                        };
                        let a = self.reload(ctx);
                        a.with(super::ok_toast(t!(key).into_owned()))
                    }
                    Err(e) => {
                        let key = match op {
                            Op::Remove => "teams.remove_from_team",
                            Op::Delete => "teams.delete_room",
                        };
                        Action::effect(super::error_toast(t!(key).into_owned(), &e))
                    }
                }
            }
            Msg::ConfirmJoin => {
                let Some(Dialog::Join { rid, .. }) = &self.dialog else {
                    return Action::none();
                };
                if self.busy {
                    return Action::none();
                }
                self.busy = true;
                let rid = rid.clone();
                let r2 = rid.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.rooms_join(&r2, None).await.map(drop) },
                    move |r| Msg::Joined(rid, r),
                ))
            }
            Msg::Joined(rid, r) => {
                self.busy = false;
                self.dialog = None;
                match r {
                    Ok(()) if ctx.workspace.subscription(&rid).is_some() => {
                        Action::effect(Effect::OpenRoom(rid))
                    }
                    Ok(()) => {
                        self.pending_open = Some(rid);
                        Action::none()
                    }
                    Err(e) => {
                        Action::effect(super::error_toast(t!("teams.join_failed").into_owned(), &e))
                    }
                }
            }
            Msg::ShowAdd => {
                self.dialog = Some(Dialog::Add(AddDialog::default()));
                self.add_search(ctx)
            }
            Msg::AddQuery(q) => {
                let Some(Dialog::Add(d)) = &mut self.dialog else {
                    return Action::none();
                };
                d.query = q;
                d.seq += 1;
                Action::task(debounce(Msg::AddSearch(d.seq)))
            }
            Msg::AddSearch(seq) => match &self.dialog {
                Some(Dialog::Add(d)) if d.seq == seq => self.add_search(ctx),
                _ => Action::none(),
            },
            Msg::AddResults(seq, r) => {
                if let Some(Dialog::Add(d)) = &mut self.dialog
                    && d.seq == seq
                {
                    d.results = Some(r.map_err(|e| super::error_text(&e)));
                }
                Action::none()
            }
            Msg::AddToggle(rid) => {
                if let Some(Dialog::Add(d)) = &mut self.dialog
                    && !d.selected.remove(&rid)
                {
                    let name = d
                        .results
                        .as_ref()
                        .and_then(|r| r.as_ref().ok())
                        .and_then(|v| v.iter().find(|r| r.id == rid))
                        .map(|r| r.display_name().to_owned())
                        .unwrap_or_default();
                    d.selected.insert(rid.clone());
                    d.names.retain(|(r, _)| r != &rid);
                    d.names.push((rid, name));
                }
                Action::none()
            }
            Msg::AddConfirm => {
                let Some(Dialog::Add(d)) = &mut self.dialog else {
                    return Action::none();
                };
                if d.busy || d.selected.is_empty() {
                    return Action::none();
                }
                let Some(team) = team_id(ctx) else {
                    return Action::none();
                };
                d.busy = true;
                d.error = None;
                let rooms: Vec<String> = d.selected.iter().cloned().collect();
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        let refs: Vec<&str> = rooms.iter().map(String::as_str).collect();
                        rest.teams_add_rooms(TeamRef::Id(&team), &refs).await
                    },
                    Msg::Added,
                ))
            }
            Msg::Added(r) => match r {
                Ok(_) => {
                    self.dialog = None;
                    self.reload(ctx)
                        .with(super::ok_toast(t!("teams.rooms_added").into_owned()))
                }
                Err(e) => {
                    if let Some(Dialog::Add(d)) = &mut self.dialog {
                        d.busy = false;
                        d.error = Some(super::error_text(&e));
                    }
                    Action::none()
                }
            },
            Msg::ShowCreate => {
                let private =
                    !teams::can_create_team_room(ctx.workspace, ctx.rid, RoomType::Channel);
                self.dialog = Some(Dialog::Create(CreateDialog {
                    private,
                    ..Default::default()
                }));
                Action::none()
            }
            Msg::CreateName(n) => {
                if let Some(Dialog::Create(d)) = &mut self.dialog {
                    d.name = n;
                    d.error = None;
                }
                Action::none()
            }
            Msg::CreatePrivate(p) => {
                let t = if p {
                    RoomType::Private
                } else {
                    RoomType::Channel
                };
                if let Some(Dialog::Create(d)) = &mut self.dialog
                    && teams::can_create_team_room(ctx.workspace, ctx.rid, t)
                {
                    d.private = p;
                }
                Action::none()
            }
            Msg::CreateConfirm => {
                let Some(Dialog::Create(d)) = &mut self.dialog else {
                    return Action::none();
                };
                let name = d.name.trim().to_owned();
                if d.busy || name.is_empty() {
                    return Action::none();
                }
                let Some(team) = team_id(ctx) else {
                    return Action::none();
                };
                d.busy = true;
                let private = d.private;
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        let params = CreateRoom {
                            extra_data: Some(serde_json::json!({ "teamId": team })),
                            ..CreateRoom::new(name)
                        };
                        let room = if private {
                            rest.groups_create(&params).await?
                        } else {
                            rest.channels_create(&params).await?
                        };
                        Ok(room.display_name().to_owned())
                    },
                    Msg::Created,
                ))
            }
            Msg::Created(r) => match r {
                Ok(name) => {
                    self.dialog = None;
                    self.reload(ctx).with(super::ok_toast(
                        t!("teams.room_created", room = name.as_str()).into_owned(),
                    ))
                }
                Err(e) => {
                    if let Some(Dialog::Create(d)) = &mut self.dialog {
                        d.busy = false;
                        d.error = Some(super::error_text(&e));
                    }
                    Action::none()
                }
            },
        }
    }

    fn add_search(&self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let Some(Dialog::Add(d)) = &self.dialog else {
            return Action::none();
        };
        let (q, seq) = (d.query.trim().to_owned(), d.seq);
        Action::task(ctx.rest_task(
            move |rest| async move { rest.rooms_autocomplete_available_for_teams(Some(&q)).await },
            move |r| Msg::AddResults(seq, r),
        ))
    }

    pub fn overlay<'a>(&'a self, _ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        let d = self.dialog.as_ref()?;
        let close = Some(Msg::CancelDialog);
        let el: Element<'a, Msg> = match d {
            Dialog::Confirm { name, op, .. } => {
                let (title, body, label) = match op {
                    Op::Remove => (
                        t!("teams.remove_from_team"),
                        t!("teams.remove_room_confirm", room = name.as_str()),
                        t!("teams.remove"),
                    ),
                    Op::Delete => (
                        t!("teams.delete_room"),
                        t!("teams.delete_room_confirm", room = name.as_str()),
                        t!("teams.delete"),
                    ),
                };
                dialog(
                    title.into_owned(),
                    text(body.into_owned()).size(TEXT_BODY),
                    vec![
                        secondary_button(t!("teams.cancel").into_owned(), close.clone()).into(),
                        danger_button(label.into_owned(), Some(Msg::Confirm))
                            .loading(self.busy)
                            .into(),
                    ],
                    close,
                )
            }
            Dialog::Join { name, .. } => dialog(
                t!("teams.join_title").into_owned(),
                text(t!("teams.join_confirm", room = name.as_str()).into_owned()).size(TEXT_BODY),
                vec![
                    secondary_button(t!("teams.cancel").into_owned(), close.clone()).into(),
                    primary_button(t!("teams.join").into_owned(), Some(Msg::ConfirmJoin))
                        .loading(self.busy)
                        .into(),
                ],
                close,
            ),
            Dialog::Add(d) => {
                let mut body = column![
                    text(t!("teams.add_existing_hint").into_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                    common::search_field(
                        &d.query,
                        t!("teams.search_rooms").into_owned(),
                        Msg::AddQuery
                    ),
                ]
                .spacing(SPACE_3);
                match &d.results {
                    None => body = body.push(common::loading()),
                    Some(Err(e)) => {
                        body = body.push(text(e.clone()).size(TEXT_BODY).style(style::text_danger));
                    }
                    Some(Ok(v)) if v.is_empty() && d.selected.is_empty() => {
                        body = body.push(
                            text(t!("teams.no_available_rooms").into_owned())
                                .size(TEXT_BODY)
                                .style(style::text_secondary),
                        );
                    }
                    Some(Ok(v)) => {
                        let mut col = column![].spacing(SPACE_2);
                        // Selected rooms stay listed while the search changes.
                        let mut shown: Vec<(String, String, RoomType)> = d
                            .names
                            .iter()
                            .filter(|(r, _)| d.selected.contains(r))
                            .map(|(r, n)| {
                                let t = v
                                    .iter()
                                    .find(|x| &x.id == r)
                                    .map_or(RoomType::Channel, |x| x.t);
                                (r.clone(), n.clone(), t)
                            })
                            .collect();
                        for r in v {
                            if !shown.iter().any(|(id, _, _)| id == &r.id) {
                                shown.push((r.id.clone(), r.display_name().to_owned(), r.t));
                            }
                        }
                        for (rid, name, t) in shown {
                            let glyph = if t == RoomType::Private {
                                Icon::Lock
                            } else {
                                Icon::Hash
                            };
                            let r2 = rid.clone();
                            col = col.push(
                                row![
                                    checkbox(d.selected.contains(&rid))
                                        .on_toggle(move |_| Msg::AddToggle(r2.clone()))
                                        .style(style::checkbox),
                                    icon::sized(glyph, 13.0).style(style::text_secondary),
                                    ellipsis(name).size(TEXT_BODY),
                                ]
                                .spacing(SPACE_2)
                                .align_y(Alignment::Center),
                            );
                        }
                        body = body.push(col);
                    }
                }
                if let Some(e) = &d.error {
                    body = body.push(text(e.clone()).size(TEXT_CAPTION).style(style::text_danger));
                }
                dialog(
                    t!("teams.add_existing").into_owned(),
                    body,
                    vec![
                        secondary_button(t!("teams.cancel").into_owned(), close.clone()).into(),
                        primary_button(
                            t!("teams.add").into_owned(),
                            (!d.selected.is_empty()).then_some(Msg::AddConfirm),
                        )
                        .loading(d.busy)
                        .into(),
                    ],
                    close,
                )
            }
            Dialog::Create(d) => {
                let body = column![
                    text_field(t!("teams.room_name").into_owned(), &d.name, Msg::CreateName)
                        .error(d.error.clone())
                        .on_submit(Msg::CreateConfirm),
                    toggler(d.private)
                        .label(t!("teams.private_room").into_owned())
                        .on_toggle(Msg::CreatePrivate)
                        .style(style::toggler)
                        .text_size(TEXT_BODY),
                ]
                .spacing(SPACE_3);
                dialog(
                    t!("teams.create_new").into_owned(),
                    body,
                    vec![
                        secondary_button(t!("teams.cancel").into_owned(), close.clone()).into(),
                        primary_button(
                            t!("teams.create").into_owned(),
                            (!d.name.trim().is_empty()).then_some(Msg::CreateConfirm),
                        )
                        .loading(d.busy)
                        .into(),
                    ],
                    close,
                )
            }
        };
        Some((el, Msg::CancelDialog))
    }

    fn row_menu<'a>(&'a self, r: &'a Room, ctx: &ViewCtx<'a>) -> Option<Element<'a, Msg>> {
        let ws = ctx.workspace;
        let main = main_rid(ws, ctx.rid);
        let mut entries: Vec<MenuEntry<Msg>> = Vec::new();
        if teams::can_toggle_autojoin(ws, ctx.rid) {
            let on = r.team_default == Some(true);
            entries.push(
                MenuItem::new(
                    if on {
                        t!("teams.autojoin_off")
                    } else {
                        t!("teams.autojoin_on")
                    }
                    .into_owned(),
                    Msg::AutoJoin(r.id.clone(), !on),
                )
                .icon(Icon::UserPlus)
                .into(),
            );
        }
        if teams::can_remove_team_room(ws, ctx.rid) {
            entries.push(
                MenuItem::new(
                    t!("teams.remove_from_team").into_owned(),
                    Msg::Ask(r.id.clone(), Op::Remove),
                )
                .icon(Icon::LogOut)
                .into(),
            );
        }
        if can_delete_child(ws, r, &main) {
            entries.push(
                MenuItem::new(
                    t!("teams.delete").into_owned(),
                    Msg::Ask(r.id.clone(), Op::Delete),
                )
                .icon(Icon::Trash2)
                .danger(true)
                .into(),
            );
        }
        if entries.is_empty() {
            return None;
        }
        let open = self.menu.as_deref() == Some(r.id.as_str());
        Some(popover_menu(
            icon_button_with(
                Icon::EllipsisVertical,
                Some(Msg::Menu(if open { None } else { Some(r.id.clone()) })),
                t!("teams.more").into_owned(),
                open,
                28.0,
            ),
            &entries,
            open,
            Msg::Menu(None),
            Placement::BelowEnd,
        ))
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let mut tools = row![].spacing(SPACE_2).align_y(Alignment::Center);
        if teams::can_create_any_team_room(ws, ctx.rid) {
            tools = tools.push(
                secondary_button(t!("teams.create_new").into_owned(), Some(Msg::ShowCreate))
                    .icon(Icon::Plus),
            );
        }
        if teams::can_add_team_rooms(ws, ctx.rid) {
            tools = tools.push(
                secondary_button(t!("teams.add_existing").into_owned(), Some(Msg::ShowAdd))
                    .icon(Icon::FolderInput),
            );
        }
        let top = column![
            tools,
            common::search_field(
                &self.query,
                t!("teams.search_rooms").into_owned(),
                Msg::Query
            ),
            common::filter_pills(
                &[
                    (Filter::All, t!("teams.filter_all").into_owned()),
                    (Filter::AutoJoin, t!("teams.filter_autojoin").into_owned()),
                ],
                self.filter,
                Msg::Filter,
            ),
        ]
        .spacing(SPACE_2)
        .padding([SPACE_3, SPACE_4]);
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(Icon::Hash, t!("teams.no_rooms").into_owned())
            }
            _ => {
                let mut col = column![].spacing(0);
                for r in &self.list.items {
                    let name = r.display_name().to_owned();
                    let glyph = if r.t == RoomType::Private {
                        Icon::Lock
                    } else {
                        Icon::Hash
                    };
                    let mut meta = vec![
                        t!("teams.members_count", count = r.users_count.unwrap_or(0)).into_owned(),
                    ];
                    if let Some(topic) = r.topic.clone().filter(|t| !t.trim().is_empty()) {
                        meta.push(topic.lines().next().unwrap_or_default().to_owned());
                    }
                    if ws.subscription(&r.id).is_none() {
                        meta.push(t!("teams.not_joined").into_owned());
                    }
                    let mut title = row![
                        icon::sized(glyph, 14.0).style(style::text_secondary),
                        ellipsis(name.clone()).size(TEXT_BODY).font(FONT_MEDIUM),
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center);
                    if r.team_default == Some(true) {
                        title =
                            title.push(chip(t!("teams.autojoin").into_owned(), ChipKind::Primary));
                    }
                    let line = row![
                        common::room_avatar(ctx, &r.id, r.avatar_etag.clone(), &name, 32.0),
                        column![
                            title,
                            ellipsis(meta.join(" · ")).size(TEXT_CAPTION).style(
                                |t: &iced::Theme| Some(rustket_ui::theme::tokens(t).text_hint)
                            ),
                        ]
                        .width(Length::Fill),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center);
                    let mut item = row![common::list_row(line, Some(Msg::Open(r.id.clone())))]
                        .align_y(Alignment::Center);
                    if let Some(menu) = self.row_menu(r, ctx) {
                        item = item.push(container(menu).padding([0.0, SPACE_2]));
                    }
                    col = col.push(item);
                }
                col = col.push(common::load_more(
                    self.list.state == LoadState::LoadingMore,
                    self.list.has_more(),
                    Msg::More,
                ));
                scrollable(col)
                    .height(Length::Fill)
                    .style(style::scrollable)
                    .into()
            }
        };
        column![top, body].height(Length::Fill).into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::ServerUrl;
    use rustket_core::ServerId;
    use rustket_core::workspace::{Change, Workspace};

    #[test]
    fn open_not_joined_asks_first_and_filter_reloads() {
        let mut ws = Workspace::new();
        let mut main = Room {
            id: "M".into(),
            t: RoomType::Channel,
            name: Some("team".into()),
            team_id: Some("T".into()),
            team_main: Some(true),
            ..Default::default()
        };
        main.users_count = Some(3);
        ws.apply_room(Change::Inserted, main);
        let (sid, url) = (
            ServerId("s".into()),
            ServerUrl::parse("http://localhost:3000").unwrap(),
        );
        let images = crate::images::Images::new(None);
        let prefs = rustket_core::config::UiPrefs::default();
        let mut ctx = UpdateCtx {
            server_id: &sid,
            server: &url,
            rid: "M",
            workspace: &mut ws,
            session: None,
            images: &images,
            prefs: &prefs,
        };
        let mut c = Channels::default();
        let _ = c.activate(&mut ctx);
        assert_eq!(c.list.state, LoadState::Loading);
        let a = c.update(Msg::Open("child".into()), &mut ctx);
        assert!(a.effects.is_empty());
        assert!(matches!(c.dialog, Some(Dialog::Join { .. })));
        let _ = c.update(Msg::CancelDialog, &mut ctx);
        assert!(c.dialog.is_none());
        let _ = c.update(Msg::Filter(Filter::AutoJoin), &mut ctx);
        assert_eq!(c.filter, Filter::AutoJoin);
    }
}
