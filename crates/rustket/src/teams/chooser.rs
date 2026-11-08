//! Two-step "choose child rooms, then confirm" dialog of the leave team,
//! delete team, convert to channel and remove member flows (§2.6–§2.9).
//!
//! The team's main room is never offered and never sent: `convertToChannel`
//! would erase it, and `leave`/`removeMember`/`delete` reject empty lists,
//! which the REST layer handles by omitting the key.

use std::collections::BTreeSet;

use iced::widget::{checkbox, column, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Room, RoomType, TeamRoomOfUser};
use rocketchat::rest::{Page, TeamRef};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::permissions::{has_permission, ids};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{danger_button, dialog, ellipsis, primary_button, secondary_button};

use crate::error::Res;
use crate::room::UpdateCtx;

/// Which flow the dialog runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flow {
    Leave,
    Delete,
    Convert,
    RemoveMember { uid: String, name: String },
}

/// A child room in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub rid: String,
    pub name: String,
    pub t: RoomType,
    /// Shown but not selectable (last owner, cannot be deleted).
    pub locked: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Vec<Choice>>),
    /// Child rooms for the delete flow (deletability is checked on arrival).
    LoadedRooms(Res<Vec<Room>>),
    Toggle(String),
    ToggleAll,
    Next,
    Back,
    Confirm,
    Done(Res<()>),
    Cancel,
}

/// What the owner should do after a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// Closed without changes.
    Cancelled,
    /// The flow finished.
    Finished,
}

#[derive(Debug, Clone)]
pub struct Chooser {
    pub flow: Flow,
    team_id: String,
    main_rid: String,
    team_name: String,
    rooms: Option<Result<Vec<Choice>, String>>,
    /// The user's child rooms are not visible to us (`listRoomsOfUser` needs
    /// `view-all-team-channels`): removal goes on without a room choice.
    rooms_hidden: bool,
    selected: BTreeSet<String>,
    confirm_step: bool,
    busy: bool,
    error: Option<String>,
}

/// `delete-{t}` in the child room and `delete-team-channel|group` in the main
/// room (`eraseRoom`, §2.6), for a room document that may not be in the
/// workspace (then only global roles count, like on the server).
pub fn can_delete_child(ws: &Workspace, room: &Room, main_rid: &str) -> bool {
    if room.federated == Some(true) || room.is_team() || room.id == main_rid {
        return false;
    }
    let (perm, team_perm) = match room.t {
        RoomType::Channel => (ids::DELETE_C, ids::DELETE_TEAM_CHANNEL),
        RoomType::Private => (ids::DELETE_P, ids::DELETE_TEAM_GROUP),
        _ => return false,
    };
    has_permission(ws, perm, Some(&room.id)) && has_permission(ws, team_perm, Some(main_rid))
}

/// The rooms a flow may send: selected, unlocked, never the main room.
pub fn rooms_to_send(
    choices: &[Choice],
    selected: &BTreeSet<String>,
    main_rid: &str,
) -> Vec<String> {
    choices
        .iter()
        .filter(|c| !c.locked && c.rid != main_rid && selected.contains(&c.rid))
        .map(|c| c.rid.clone())
        .collect()
}

/// Page size and upper bound of the child-room lists.
const PAGE: u64 = 100;
const MAX_ROOMS: u64 = 2_000;

/// A permission refusal (HTTP 403 / `unauthorized`), not a failure.
fn is_forbidden(e: &rocketchat::Error) -> bool {
    match e {
        rocketchat::Error::Api { status, error, .. } => {
            *status == 403 || error.as_deref() == Some("unauthorized")
        }
        rocketchat::Error::Status { status, .. } => *status == 403,
        _ => false,
    }
}

fn choice_of(r: &Room, locked: bool) -> Choice {
    Choice {
        rid: r.id.clone(),
        name: r.display_name().to_owned(),
        t: r.t,
        locked,
    }
}

impl Chooser {
    pub fn new(flow: Flow, team_id: &str, main_rid: &str, team_name: &str) -> Self {
        Self {
            flow,
            team_id: team_id.to_owned(),
            main_rid: main_rid.to_owned(),
            team_name: team_name.to_owned(),
            rooms: None,
            rooms_hidden: false,
            selected: BTreeSet::new(),
            confirm_step: false,
            busy: false,
            error: None,
        }
    }

    /// Loads the child rooms to choose from.
    pub fn load(&self, ctx: &UpdateCtx<'_>) -> iced::Task<Msg> {
        let team = self.team_id.clone();
        let main = self.main_rid.clone();
        let me = ctx.workspace.user_id().unwrap_or_default().to_owned();
        match &self.flow {
            Flow::Delete => ctx.rest_task(
                move |rest| async move {
                    let mut out: Vec<Room> = Vec::new();
                    let mut offset = 0;
                    loop {
                        let p = rest
                            .teams_list_rooms(
                                TeamRef::Id(&team),
                                None,
                                false,
                                Page::new(offset, PAGE),
                            )
                            .await?;
                        offset += p.raw_len as u64;
                        let done = p.raw_len == 0 || offset >= p.total || offset >= MAX_ROOMS;
                        out.extend(p.items.into_iter().filter(|r| r.id != main));
                        if done {
                            break;
                        }
                    }
                    Ok(out)
                },
                Msg::LoadedRooms,
            ),
            Flow::Leave | Flow::Convert | Flow::RemoveMember { .. } => {
                let (uid, can_delete, lock_last_owner) = match &self.flow {
                    Flow::RemoveMember { uid, .. } => (uid.clone(), false, false),
                    Flow::Convert => (me, true, false),
                    _ => (me, false, true),
                };
                ctx.rest_task(
                    move |rest| async move {
                        let mut out = Vec::new();
                        let mut offset = 0;
                        loop {
                            let p: rocketchat::models::Paginated<TeamRoomOfUser> = rest
                                .teams_list_rooms_of_user(
                                    TeamRef::Id(&team),
                                    &uid,
                                    can_delete,
                                    Page::new(offset, PAGE),
                                )
                                .await?;
                            offset += p.raw_len as u64;
                            let done = p.raw_len == 0 || offset >= p.total || offset >= MAX_ROOMS;
                            out.extend(
                                p.items.iter().filter(|r| r.room.id != main).map(|r| {
                                    choice_of(&r.room, lock_last_owner && r.is_last_owner)
                                }),
                            );
                            if done {
                                break;
                            }
                        }
                        Ok(out)
                    },
                    Msg::Loaded,
                )
            }
        }
    }

    pub fn update(&mut self, msg: Msg, ctx: &UpdateCtx<'_>) -> (iced::Task<Msg>, Outcome) {
        match msg {
            Msg::Loaded(Err(e))
                if matches!(self.flow, Flow::RemoveMember { .. }) && is_forbidden(e.error()) =>
            {
                // Moderators may remove members but not list their rooms.
                self.rooms_hidden = true;
                self.rooms = Some(Ok(Vec::new()));
            }
            Msg::Loaded(r) => {
                self.rooms = Some(r.map_err(|e| super::error_text(&e)));
            }
            Msg::LoadedRooms(r) => {
                let main = self.main_rid.clone();
                self.rooms = Some(
                    r.map(|rooms| {
                        rooms
                            .iter()
                            .map(|room| {
                                choice_of(room, !can_delete_child(ctx.workspace, room, &main))
                            })
                            .collect()
                    })
                    .map_err(|e| super::error_text(&e)),
                );
            }
            Msg::Toggle(rid) => {
                let allowed = self.choices().iter().any(|c| c.rid == rid && !c.locked);
                if allowed && !self.selected.remove(&rid) {
                    self.selected.insert(rid);
                }
            }
            Msg::ToggleAll => {
                let all: BTreeSet<String> = self
                    .choices()
                    .iter()
                    .filter(|c| !c.locked)
                    .map(|c| c.rid.clone())
                    .collect();
                self.selected = if self.selected == all {
                    BTreeSet::new()
                } else {
                    all
                };
            }
            Msg::Next => self.confirm_step = true,
            Msg::Back => {
                if !self.busy {
                    self.confirm_step = false;
                }
            }
            Msg::Cancel => {
                if !self.busy {
                    return (iced::Task::none(), Outcome::Cancelled);
                }
            }
            Msg::Confirm => {
                if self.busy {
                    return (iced::Task::none(), Outcome::None);
                }
                self.busy = true;
                self.error = None;
                return (self.run(ctx), Outcome::None);
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(()) => return (iced::Task::none(), Outcome::Finished),
                    Err(e) => self.error = Some(super::error_text(&e)),
                }
            }
        }
        (iced::Task::none(), Outcome::None)
    }

    fn choices(&self) -> &[Choice] {
        match &self.rooms {
            Some(Ok(v)) => v,
            _ => &[],
        }
    }

    fn selection(&self) -> Vec<String> {
        rooms_to_send(self.choices(), &self.selected, &self.main_rid)
    }

    fn run(&self, ctx: &UpdateCtx<'_>) -> iced::Task<Msg> {
        let team = self.team_id.clone();
        let rooms = self.selection();
        let flow = self.flow.clone();
        ctx.rest_task(
            move |rest| async move {
                let refs: Vec<&str> = rooms.iter().map(String::as_str).collect();
                let t = TeamRef::Id(&team);
                match flow {
                    Flow::Leave => rest.teams_leave(t, &refs).await,
                    Flow::Delete => rest.teams_delete(t, &refs).await,
                    Flow::Convert => rest.teams_convert_to_channel(t, &refs).await,
                    Flow::RemoveMember { uid, .. } => {
                        rest.teams_remove_member(t, &uid, &refs).await
                    }
                }
            },
            Msg::Done,
        )
    }

    fn title(&self) -> String {
        match &self.flow {
            Flow::Leave => t!("teams.leave_title", team = self.team_name.as_str()),
            Flow::Delete => t!("teams.delete_title", team = self.team_name.as_str()),
            Flow::Convert => t!("teams.convert_title", team = self.team_name.as_str()),
            Flow::RemoveMember { name, .. } => {
                t!("teams.remove_member_title", user = name.as_str())
            }
        }
        .into_owned()
    }

    fn choose_hint(&self) -> String {
        match &self.flow {
            Flow::Leave => t!("teams.leave_choose"),
            Flow::Delete => t!("teams.delete_choose"),
            Flow::Convert => t!("teams.convert_choose"),
            Flow::RemoveMember { .. } => t!("teams.remove_member_choose"),
        }
        .into_owned()
    }

    fn locked_hint(&self) -> String {
        match &self.flow {
            Flow::Leave => t!("teams.locked_last_owner"),
            _ => t!("teams.locked_cannot_delete"),
        }
        .into_owned()
    }

    fn confirm_text(&self) -> String {
        let n = self.selection().len();
        let team = self.team_name.as_str();
        match &self.flow {
            Flow::Leave if n == 0 => t!("teams.leave_confirm_none", team = team),
            Flow::Leave => t!("teams.leave_confirm", team = team, count = n),
            Flow::Delete if n == 0 => t!("teams.delete_confirm_none", team = team),
            Flow::Delete => t!("teams.delete_confirm", team = team, count = n),
            Flow::Convert if n == 0 => t!("teams.convert_confirm_none", team = team),
            Flow::Convert => t!("teams.convert_confirm", team = team, count = n),
            Flow::RemoveMember { name, .. } if n == 0 => {
                t!(
                    "teams.remove_member_confirm_none",
                    user = name.as_str(),
                    team = team
                )
            }
            Flow::RemoveMember { name, .. } => t!(
                "teams.remove_member_confirm",
                user = name.as_str(),
                team = team,
                count = n
            ),
        }
        .into_owned()
    }

    fn action_label(&self) -> String {
        match &self.flow {
            Flow::Leave => t!("teams.leave"),
            Flow::Delete => t!("teams.delete"),
            Flow::Convert => t!("teams.convert"),
            Flow::RemoveMember { .. } => t!("teams.remove"),
        }
        .into_owned()
    }

    fn danger(&self) -> bool {
        !matches!(self.flow, Flow::Convert)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut body = column![].spacing(SPACE_3);
        let mut actions: Vec<Element<'_, Msg>> = Vec::new();
        if self.confirm_step {
            body = body.push(text(self.confirm_text()).size(TEXT_BODY));
            let names: Vec<String> = self
                .choices()
                .iter()
                .filter(|c| self.selection().contains(&c.rid))
                .map(|c| c.name.clone())
                .collect();
            if !names.is_empty() {
                body = body.push(
                    text(names.join(", "))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary),
                );
            }
            actions.push(
                secondary_button(
                    t!("teams.back").into_owned(),
                    (!self.busy).then_some(Msg::Back),
                )
                .into(),
            );
            let confirm = if self.danger() {
                danger_button(self.action_label(), Some(Msg::Confirm))
            } else {
                primary_button(self.action_label(), Some(Msg::Confirm))
            };
            actions.push(confirm.loading(self.busy).into());
        } else {
            match &self.rooms {
                None => body = body.push(crate::room::panel::common::loading()),
                Some(Err(e)) => {
                    body = body.push(text(e.clone()).size(TEXT_BODY).style(style::text_danger));
                }
                Some(Ok(list)) if list.is_empty() => {
                    let key = if self.rooms_hidden {
                        "teams.remove_member_rooms_hidden"
                    } else {
                        "teams.no_child_rooms"
                    };
                    body = body.push(
                        text(t!(key).into_owned())
                            .size(TEXT_BODY)
                            .style(style::text_secondary),
                    );
                }
                Some(Ok(list)) => {
                    body = body.push(text(self.choose_hint()).size(TEXT_BODY));
                    let selectable = list.iter().filter(|c| !c.locked).count();
                    if selectable > 1 {
                        let all = self.selected.len() == selectable;
                        body = body.push(
                            checkbox(all)
                                .label(t!("teams.select_all").into_owned())
                                .on_toggle(|_| Msg::ToggleAll)
                                .text_size(TEXT_CAPTION)
                                .style(style::checkbox),
                        );
                    }
                    let mut col = column![].spacing(SPACE_2);
                    for c in list {
                        let glyph = if c.t == RoomType::Private {
                            Icon::Lock
                        } else {
                            Icon::Hash
                        };
                        let mut label = row![
                            icon::sized(glyph, 13.0).style(style::text_secondary),
                            ellipsis(c.name.clone()).size(TEXT_BODY),
                        ]
                        .spacing(SPACE_1)
                        .align_y(Alignment::Center);
                        if c.locked {
                            label = label.push(
                                text(self.locked_hint())
                                    .size(TEXT_CAPTION)
                                    .style(style::text_hint),
                            );
                        }
                        let rid = c.rid.clone();
                        let cb = checkbox(self.selected.contains(&c.rid))
                            .on_toggle_maybe(
                                (!c.locked).then_some(move |_| Msg::Toggle(rid.clone())),
                            )
                            .style(style::checkbox);
                        col = col.push(row![cb, label].spacing(SPACE_2).align_y(Alignment::Center));
                    }
                    body = body.push(
                        scrollable(col)
                            .height(Length::Shrink)
                            .style(style::scrollable),
                    );
                }
            }
            actions
                .push(secondary_button(t!("teams.cancel").into_owned(), Some(Msg::Cancel)).into());
            let ready = self.rooms.as_ref().is_some_and(|r| r.is_ok());
            actions.push(
                primary_button(t!("teams.next").into_owned(), ready.then_some(Msg::Next)).into(),
            );
        }
        if let Some(e) = &self.error {
            body = body.push(text(e.clone()).size(TEXT_CAPTION).style(style::text_danger));
        }
        dialog(self.title(), body, actions, Some(Msg::Cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_is_recognised() {
        let api = |status, error: Option<&str>| rocketchat::Error::Api {
            status,
            error: error.map(str::to_owned),
            error_type: None,
            message: None,
            details: None,
        };
        assert!(is_forbidden(&api(403, None)));
        assert!(is_forbidden(&api(400, Some("unauthorized"))));
        assert!(!is_forbidden(&api(400, Some("error-invalid-team"))));
    }

    #[test]
    fn main_room_is_never_sent() {
        let c = |rid: &str, locked| Choice {
            rid: rid.into(),
            name: rid.into(),
            t: RoomType::Channel,
            locked,
        };
        let choices = vec![c("MAIN", false), c("a", false), c("b", true), c("c", false)];
        let sel: BTreeSet<String> = ["MAIN", "a", "b"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(rooms_to_send(&choices, &sel, "MAIN"), vec!["a"]);
    }
}
