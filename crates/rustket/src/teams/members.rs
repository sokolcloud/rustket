//! Team members (`teams.members`, §2.9, §5.5): team roles, who added each
//! member; add members (`channels|groups.invite` on the main room, which
//! creates the membership), role changes through the room role endpoints of
//! the main room (they post the system messages and broadcast `roles-change`,
//! unlike `teams.updateMember`), and the two-step "remove from team" dialog.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{Paginated, RoomType, TeamMember, UserStatus};
use rocketchat::rest::{MemberFilter, Page, TeamRef};
use rust_i18n::t;
use rustket_core::teams;
use rustket_core::workspace::Changes;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{FONT_MEDIUM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{
    ChipKind, MenuEntry, MenuItem, Placement, chip, dialog, ellipsis, icon_button_with,
    popover_menu, presence_dot, primary_button, secondary_button, text_field,
};

use super::chooser::{self, Chooser, Flow, Outcome};
use crate::error::Res;
use crate::room::panel::api::{self, Role};
use crate::room::panel::paged::{LoadState, PageReq, Paged};
use crate::room::panel::{Kind, common, debounce};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

/// Results per field of a member search.
const SEARCH_LIMIT: u64 = 100;

/// Username matches, then name matches not already listed.
fn merge_members(a: Vec<TeamMember>, b: Vec<TeamMember>) -> Paginated<TeamMember> {
    let mut items = a;
    for m in b {
        if !items.iter().any(|x| x.user.id == m.user.id) {
            items.push(m);
        }
    }
    let n = items.len();
    Paginated {
        items,
        count: n as u64,
        offset: 0,
        total: n as u64,
        raw_len: n,
        dropped: 0,
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Query(String),
    Search(u64),
    Page(PageReq, Res<Paginated<TeamMember>>),
    More,
    Retry,
    Menu(Option<String>),
    Profile(String),
    SetRole(String, Role, bool),
    RoleDone(Res<()>),
    Remove(String),
    Chooser(chooser::Msg),
    ShowAdd,
    AddInput(String),
    AddConfirm,
    Added(Res<()>),
    CancelAdd,
}

#[derive(Debug, Default)]
struct AddDialog {
    input: String,
    busy: bool,
    error: Option<String>,
}

#[derive(Default)]
pub struct Members {
    query: String,
    query_seq: u64,
    list: Paged<TeamMember>,
    menu: Option<String>,
    chooser: Option<Chooser>,
    add: Option<AddDialog>,
}

/// Usernames typed into the add field (`@` and separators stripped).
pub fn parse_usernames(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim().trim_start_matches('@'))
        .filter(|s| !s.is_empty())
    {
        if !out.iter().any(|x| x == u) {
            out.push(u.to_owned());
        }
    }
    out
}

fn main_info(ctx: &UpdateCtx<'_>) -> Option<(String, String, RoomType)> {
    let team = teams::team_id_of(ctx.workspace, ctx.rid)?.to_owned();
    let main = super::main_room(ctx.workspace, ctx.rid).unwrap_or_else(|| ctx.rid.to_owned());
    let t = ctx
        .workspace
        .room(&main)
        .map_or(RoomType::Channel, |e| e.room_type());
    Some((team, main, t))
}

impl Members {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.reload(ctx)
    }

    fn reload(&mut self, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let req = self.list.reset();
        self.fetch(req, ctx)
    }

    fn fetch(&mut self, req: PageReq, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let Some((team, _, _)) = main_info(ctx) else {
            self.list.finish(req, Ok(Paginated::default()));
            return Action::none();
        };
        let q = self.query.trim().to_owned();
        Action::task(ctx.rest_task(
            move |rest| async move {
                if q.is_empty() {
                    return rest
                        .teams_members(
                            TeamRef::Id(&team),
                            &MemberFilter::default(),
                            Page::new(req.offset, super::PAGE),
                        )
                        .await;
                }
                // `username` and `name` are ANDed by the server: search each
                // and merge (one page of results).
                if req.offset > 0 {
                    return Ok(Paginated::default());
                }
                let by_username = MemberFilter {
                    username: Some(q.clone()),
                    ..Default::default()
                };
                let by_name = MemberFilter {
                    name: Some(q),
                    ..Default::default()
                };
                let page = Page::new(0, SEARCH_LIMIT);
                let (a, b) = futures::future::join(
                    rest.teams_members(TeamRef::Id(&team), &by_username, page),
                    rest.teams_members(TeamRef::Id(&team), &by_name, page),
                )
                .await;
                Ok(merge_members(a?.items, b?.items))
            },
            move |r| Msg::Page(req, r),
        ))
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        // Membership changes post system messages in the main room.
        let main = super::main_room(ctx.workspace, ctx.rid);
        if main.as_ref().is_some_and(|m| changes.timelines.contains(m))
            && !self.list.is_busy()
            && self.chooser.is_none()
            && self.add.is_none()
        {
            let last_system = main
                .as_deref()
                .and_then(|m| ctx.workspace.timeline(m))
                .and_then(|t| t.iter().last())
                .and_then(|i| i.message.t.clone());
            if matches!(
                last_system.as_deref(),
                Some(
                    "added-user-to-team"
                        | "removed-user-from-team"
                        | "ujt"
                        | "ult"
                        | "subscription-role-added"
                        | "subscription-role-removed"
                )
            ) {
                return self.reload(ctx);
            }
        }
        Action::none()
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
            Msg::Page(req, r) => {
                self.list.finish(req, r);
                Action::none()
            }
            Msg::More => match self.list.begin_more() {
                Some(req) => self.fetch(req, ctx),
                None => Action::none(),
            },
            Msg::Retry => self.reload(ctx),
            Msg::Menu(m) => {
                self.menu = m;
                Action::none()
            }
            Msg::Profile(username) => {
                self.menu = None;
                Action::effect(Effect::OpenPanel(Kind::UserInfo(username)))
            }
            Msg::SetRole(uid, role, add) => {
                self.menu = None;
                let Some((_, main, t)) = main_info(ctx) else {
                    return Action::none();
                };
                let call = api::Call::Post(
                    role.endpoint(t, add),
                    serde_json::json!({ "roomId": main, "userId": uid }),
                );
                Action::task(ctx.rest_task(move |rest| api::run_call(rest, call), Msg::RoleDone))
            }
            Msg::RoleDone(r) => match r {
                Ok(()) => self.reload(ctx),
                Err(e) => {
                    Action::effect(super::error_toast(t!("teams.role_failed").into_owned(), &e))
                }
            },
            Msg::Remove(uid) => {
                self.menu = None;
                let Some((team, main, _)) = main_info(ctx) else {
                    return Action::none();
                };
                let name = self
                    .list
                    .items
                    .iter()
                    .find(|m| m.user.id == uid)
                    .map(|m| {
                        m.user
                            .name
                            .clone()
                            .or(m.user.username.clone())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                let team_name = ctx
                    .workspace
                    .room(&main)
                    .map(|e| e.fname().unwrap_or(e.name()).to_owned())
                    .unwrap_or_default();
                let c = Chooser::new(Flow::RemoveMember { uid, name }, &team, &main, &team_name);
                let task = c.load(ctx).map(Msg::Chooser);
                self.chooser = Some(c);
                Action::task(task)
            }
            Msg::Chooser(m) => {
                let Some(c) = self.chooser.as_mut() else {
                    return Action::none();
                };
                let (task, outcome) = c.update(m, ctx);
                match outcome {
                    Outcome::None => Action::task(task.map(Msg::Chooser)),
                    Outcome::Cancelled => {
                        self.chooser = None;
                        Action::none()
                    }
                    Outcome::Finished => {
                        self.chooser = None;
                        self.reload(ctx)
                            .with(super::ok_toast(t!("teams.member_removed").into_owned()))
                    }
                }
            }
            Msg::ShowAdd => {
                self.add = Some(AddDialog::default());
                Action::none()
            }
            Msg::CancelAdd => {
                if !self.add.as_ref().is_some_and(|a| a.busy) {
                    self.add = None;
                }
                Action::none()
            }
            Msg::AddInput(s) => {
                if let Some(a) = &mut self.add {
                    a.input = s;
                    a.error = None;
                }
                Action::none()
            }
            Msg::AddConfirm => {
                let Some(a) = &mut self.add else {
                    return Action::none();
                };
                let names = parse_usernames(&a.input);
                if a.busy || names.is_empty() {
                    return Action::none();
                }
                let Some((_, main, t)) = main_info(ctx) else {
                    return Action::none();
                };
                a.busy = true;
                Action::task(ctx.rest_task(
                    move |rest| api::invite_users(rest, t, main, names),
                    Msg::Added,
                ))
            }
            Msg::Added(r) => match r {
                Ok(()) => {
                    self.add = None;
                    self.reload(ctx)
                        .with(super::ok_toast(t!("teams.members_added").into_owned()))
                }
                Err(e) => {
                    if let Some(a) = &mut self.add {
                        a.busy = false;
                        a.error = Some(super::error_text(&e));
                    }
                    Action::none()
                }
            },
        }
    }

    pub fn overlay<'a>(&'a self, _ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        if let Some(c) = &self.chooser {
            return Some((
                c.view().map(Msg::Chooser),
                Msg::Chooser(chooser::Msg::Cancel),
            ));
        }
        let a = self.add.as_ref()?;
        let body = column![
            text(t!("teams.add_members_hint").into_owned())
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            text_field(t!("teams.usernames").into_owned(), &a.input, Msg::AddInput)
                .placeholder("@alice, @bob")
                .error(a.error.clone())
                .on_submit(Msg::AddConfirm),
        ]
        .spacing(SPACE_3);
        Some((
            dialog(
                t!("teams.add_members").into_owned(),
                body,
                vec![
                    secondary_button(t!("teams.cancel").into_owned(), Some(Msg::CancelAdd)).into(),
                    primary_button(
                        t!("teams.add").into_owned(),
                        (!parse_usernames(&a.input).is_empty()).then_some(Msg::AddConfirm),
                    )
                    .loading(a.busy)
                    .into(),
                ],
                Some(Msg::CancelAdd),
            ),
            Msg::CancelAdd,
        ))
    }

    fn row_menu<'a>(&'a self, m: &'a TeamMember, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let uid = m.user.id.clone();
        let mut entries: Vec<MenuEntry<Msg>> = Vec::new();
        if let Some(u) = m.user.username.clone() {
            entries.push(
                MenuItem::new(t!("teams.view_profile").into_owned(), Msg::Profile(u))
                    .icon(Icon::User)
                    .into(),
            );
        }
        for role in [Role::Owner, Role::Moderator, Role::Leader] {
            if !teams::can_change_team_role(ws, ctx.rid, role.id()) {
                continue;
            }
            let has = m.has_role(role.id());
            let key = match (role, has) {
                (Role::Owner, false) => "teams.set_owner",
                (Role::Owner, true) => "teams.unset_owner",
                (Role::Moderator, false) => "teams.set_moderator",
                (Role::Moderator, true) => "teams.unset_moderator",
                (Role::Leader, false) => "teams.set_leader",
                (Role::Leader, true) => "teams.unset_leader",
            };
            entries.push(
                MenuItem::new(t!(key).into_owned(), Msg::SetRole(uid.clone(), role, !has))
                    .icon(Icon::Shield)
                    .into(),
            );
        }
        let me = ws.user_id() == Some(uid.as_str());
        if !me && teams::can_manage_team_members(ws, ctx.rid) {
            entries.push(MenuEntry::Separator);
            entries.push(
                MenuItem::new(
                    t!("teams.remove_from_team").into_owned(),
                    Msg::Remove(uid.clone()),
                )
                .icon(Icon::UserMinus)
                .danger(true)
                .into(),
            );
        }
        let open = self.menu.as_deref() == Some(uid.as_str());
        popover_menu(
            icon_button_with(
                Icon::EllipsisVertical,
                Some(Msg::Menu(if open { None } else { Some(uid) })),
                t!("teams.more").into_owned(),
                open,
                28.0,
            ),
            &entries,
            open,
            Msg::Menu(None),
            Placement::BelowEnd,
        )
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let ws = ctx.workspace;
        let can_add = teams::can_add_team_members(ws, ctx.rid)
            || super::main_room(ws, ctx.rid)
                .is_some_and(|m| rustket_core::permissions::can_add_users(ws, &m));
        let add = can_add.then(|| {
            secondary_button(t!("teams.add_members").into_owned(), Some(Msg::ShowAdd))
                .icon(Icon::UserPlus)
                .into()
        });
        // The button has a slot of its own: permissions changing must not
        // move the search field (and its focus).
        let top = rustket_ui::widget::above(
            add,
            SPACE_2,
            common::search_field(
                &self.query,
                t!("teams.search_members").into_owned(),
                Msg::Query,
            ),
        )
        .padding([SPACE_3, SPACE_4]);
        let body: Element<'a, Msg> = match &self.list.state {
            LoadState::Loading => common::loading(),
            LoadState::Failed(e) if self.list.items.is_empty() => {
                common::error_state(e, Msg::Retry)
            }
            _ if self.list.loaded && self.list.items.is_empty() => {
                common::empty(Icon::Users, t!("teams.no_members").into_owned())
            }
            _ => {
                let mut col = column![].spacing(0);
                if self.list.total > 0 {
                    col = col.push(
                        container(
                            text(super::count_text("teams.members_total", self.list.total))
                                .size(TEXT_CAPTION)
                                .style(style::text_hint),
                        )
                        .padding([0.0, SPACE_4]),
                    );
                }
                let use_real = common::use_real_names(ctx);
                for m in &self.list.items {
                    let username = m.user.username.clone().unwrap_or_default();
                    let name =
                        common::user_display(ws, use_real, &username, m.user.name.as_deref());
                    let status = ws
                        .presence()
                        .get_by_username(&username)
                        .map(|p| p.status)
                        .or(m.user.status)
                        .unwrap_or(UserStatus::Offline);
                    // Role chips follow the name (not pushed to the far edge).
                    let mut title = row![
                        ellipsis(name.clone())
                            .width(Length::Shrink)
                            .size(TEXT_BODY)
                            .font(FONT_MEDIUM),
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center);
                    for r in &m.roles {
                        title = title.push(chip(common::role_label(r), ChipKind::Neutral));
                    }
                    let mut sub = format!("@{username}");
                    if let Some(by) = m.created_by.as_ref().and_then(|u| u.username.clone())
                        && by != username
                    {
                        sub.push_str(" · ");
                        sub.push_str(&t!("teams.added_by", user = by.as_str()));
                    }
                    let line = row![
                        iced::widget::stack![
                            common::user_avatar(ctx, &username, &name, 32.0),
                            container(presence_dot(common::presence_of(status), 9.0))
                                .align_right(32.0)
                                .align_bottom(32.0),
                        ],
                        column![
                            title,
                            ellipsis(sub)
                                .size(TEXT_CAPTION)
                                .style(|t: &iced::Theme| Some(
                                    rustket_ui::theme::tokens(t).text_hint
                                )),
                        ]
                        .width(Length::Fill),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center);
                    let open = (!username.is_empty()).then(|| Msg::Profile(username.clone()));
                    col = col.push(
                        row![
                            common::list_row(line, open),
                            container(self.row_menu(m, ctx)).padding([0.0, SPACE_2]),
                        ]
                        .align_y(Alignment::Center),
                    );
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
    use super::*;

    #[test]
    fn usernames_are_parsed() {
        assert_eq!(
            parse_usernames(" @alice, bob  @alice\ncarol,"),
            vec!["alice", "bob", "carol"]
        );
        assert!(parse_usernames(" , @ ").is_empty());
    }
}
