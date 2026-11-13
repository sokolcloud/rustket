//! User profile (`users.info`) with direct message / mention and, when
//! permitted in this room, moderation actions.

use chrono::{FixedOffset, Utc};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::{RoomType, User, UserStatus};
use rocketchat::rest::UserLookup;
use rust_i18n::t;
use rustket_core::permissions::{self, ids};
use rustket_core::workspace::{Changes, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_SMALL, TEXT_TITLE, font_semibold,
};
use rustket_ui::widget::{ChipKind, divider, presence_dot};

use super::api::{self, Role, RoomRole, UserAction};
use super::common;
use crate::error::Res;
use crate::room::{Action, ComposeIntent, Effect, UpdateCtx, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Box<User>>),
    Roles(Res<Vec<RoomRole>>),
    Retry,
    RetryRoles,
    DirectMessage,
    DmCreated(Res<String>),
    Mention,
    Action(UserAction),
    Confirm,
    Cancel,
    ActionDone(UserAction, Res<()>),
}

pub struct UserInfo {
    username: String,
    user: Option<Box<User>>,
    loading: bool,
    error: Option<String>,
    /// Room roles of the user (owner, moderator, leader).
    room_roles: Vec<String>,
    roles_state: RolesState,
    confirm: Option<UserAction>,
    busy: bool,
    /// DM room created but not in the workspace yet.
    dm_pending: Option<String>,
}

/// Loading state of the user's room roles: role actions are offered only
/// once they are known, so their labels are right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolesState {
    Loading,
    Loaded,
    Failed(String),
}

/// The web client's `getUserIsMuted`.
pub fn is_muted(ws: &Workspace, rid: &str, username: &str) -> bool {
    let Some(room) = ws.room(rid).and_then(|e| e.room.as_ref()) else {
        return false;
    };
    let has = |l: &Option<Vec<String>>| l.as_ref().is_some_and(|l| l.iter().any(|u| u == username));
    if room.ro == Some(true) {
        if has(&room.unmuted) {
            return false;
        }
        if permissions::has_permission(ws, ids::POST_READONLY, Some(rid)) {
            return has(&room.muted);
        }
        return true;
    }
    has(&room.muted)
}

pub fn is_ignored(ws: &Workspace, rid: &str, uid: &str) -> bool {
    ws.subscription(rid)
        .and_then(|s| s.ignored.as_ref())
        .is_some_and(|l| l.iter().any(|u| u == uid))
}

/// Moderation actions offered for `user` in `rid` (the web client's
/// `useUserInfoActions` rules: channels and groups only). Role actions need
/// the user's room roles (`None`: not loaded).
pub fn available_actions(
    ws: &Workspace,
    rid: &str,
    uid: &str,
    username: &str,
    room_roles: Option<&[String]>,
) -> Vec<UserAction> {
    let Some(entry) = ws.room(rid) else {
        return Vec::new();
    };
    if !matches!(entry.room_type(), RoomType::Channel | RoomType::Private) {
        return Vec::new();
    }
    let me = ws.user_id() == Some(uid);
    let mut out = Vec::new();
    if let Some(room_roles) = room_roles {
        let has_role = |r: &str| room_roles.iter().any(|x| x == r);
        for (role, perm) in [
            (Role::Owner, ids::SET_OWNER),
            (Role::Moderator, ids::SET_MODERATOR),
            (Role::Leader, ids::SET_LEADER),
        ] {
            if permissions::has_permission(ws, perm, Some(rid)) {
                out.push(UserAction::SetRole(role, !has_role(role.id())));
            }
        }
    }
    if !me && permissions::can_mute_users(ws, rid) {
        out.push(UserAction::Mute(!is_muted(ws, rid, username)));
    }
    if !me && permissions::can_remove_users(ws, rid) {
        out.push(UserAction::Remove);
    }
    if !me && entry.sub.is_some() {
        out.push(UserAction::Ignore(!is_ignored(ws, rid, uid)));
    }
    out
}

fn action_label(a: &UserAction, team_main: bool) -> String {
    match a {
        UserAction::SetRole(Role::Owner, true) => t!("room_panel.set_owner"),
        UserAction::SetRole(Role::Owner, false) => t!("room_panel.unset_owner"),
        UserAction::SetRole(Role::Moderator, true) => t!("room_panel.set_moderator"),
        UserAction::SetRole(Role::Moderator, false) => t!("room_panel.unset_moderator"),
        UserAction::SetRole(Role::Leader, true) => t!("room_panel.set_leader"),
        UserAction::SetRole(Role::Leader, false) => t!("room_panel.unset_leader"),
        UserAction::Mute(true) => t!("room_panel.mute_user"),
        UserAction::Mute(false) => t!("room_panel.unmute_user"),
        UserAction::Remove if team_main => t!("room_panel.remove_from_team"),
        UserAction::Remove => t!("room_panel.remove_from_room"),
        UserAction::Ignore(true) => t!("room_panel.ignore"),
        UserAction::Ignore(false) => t!("room_panel.unignore"),
    }
    .into_owned()
}

fn action_icon(a: &UserAction) -> Icon {
    match a {
        UserAction::SetRole(Role::Owner, _) => Icon::ShieldCheck,
        UserAction::SetRole(Role::Moderator, _) => Icon::Shield,
        UserAction::SetRole(Role::Leader, _) => Icon::Crown,
        UserAction::Mute(true) => Icon::MicOff,
        UserAction::Mute(false) => Icon::Mic,
        UserAction::Remove => Icon::UserMinus,
        UserAction::Ignore(_) => Icon::Ban,
    }
}

fn needs_confirm(a: &UserAction) -> bool {
    matches!(a, UserAction::Mute(true) | UserAction::Remove)
}

fn done_text(a: &UserAction, user: &str) -> String {
    match a {
        UserAction::SetRole(Role::Owner, true) => t!("room_panel.done_set_owner", user = user),
        UserAction::SetRole(Role::Owner, false) => t!("room_panel.done_unset_owner", user = user),
        UserAction::SetRole(Role::Moderator, true) => {
            t!("room_panel.done_set_moderator", user = user)
        }
        UserAction::SetRole(Role::Moderator, false) => {
            t!("room_panel.done_unset_moderator", user = user)
        }
        UserAction::SetRole(Role::Leader, true) => t!("room_panel.done_set_leader", user = user),
        UserAction::SetRole(Role::Leader, false) => {
            t!("room_panel.done_unset_leader", user = user)
        }
        UserAction::Mute(true) => t!("room_panel.done_muted", user = user),
        UserAction::Mute(false) => t!("room_panel.done_unmuted", user = user),
        UserAction::Remove => t!("room_panel.done_removed", user = user),
        UserAction::Ignore(true) => t!("room_panel.done_ignored", user = user),
        UserAction::Ignore(false) => t!("room_panel.done_unignored", user = user),
    }
    .into_owned()
}

/// `HH:MM` at the user's UTC offset (hours).
pub fn local_time(offset_hours: f64, clock_24h: bool) -> Option<String> {
    let secs = (offset_hours * 3600.0).round() as i32;
    let tz = FixedOffset::east_opt(secs)?;
    let now = Utc::now().with_timezone(&tz);
    let time = if clock_24h {
        now.format("%H:%M").to_string()
    } else {
        now.format("%-I:%M %p").to_string()
    };
    let sign = if secs < 0 { '-' } else { '+' };
    let (h, m) = (secs.abs() / 3600, (secs.abs() % 3600) / 60);
    Some(if m == 0 {
        format!("{time} (UTC{sign}{h})")
    } else {
        format!("{time} (UTC{sign}{h}:{m:02})")
    })
}

impl UserInfo {
    pub fn new(username: &str) -> Self {
        Self {
            username: username.to_owned(),
            user: None,
            loading: false,
            error: None,
            room_roles: Vec::new(),
            roles_state: RolesState::Loading,
            confirm: None,
            busy: false,
            dm_pending: None,
        }
    }

    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        self.loading = true;
        self.error = None;
        let username = self.username.clone();
        let load = ctx.rest_task(
            move |rest| async move {
                rest.users_info(UserLookup::Username(&username))
                    .await
                    .map(Box::new)
            },
            Msg::Loaded,
        );
        Action::task(iced::Task::batch([load, self.load_roles(ctx)]))
    }

    fn load_roles(&mut self, ctx: &UpdateCtx<'_>) -> iced::Task<Msg> {
        let Some(t) = ctx.workspace.room(ctx.rid).map(|e| e.room_type()) else {
            return iced::Task::none();
        };
        if t == RoomType::Direct {
            self.roles_state = RolesState::Loaded;
            return iced::Task::none();
        }
        if self.roles_state != RolesState::Loaded {
            self.roles_state = RolesState::Loading;
        }
        let rid = ctx.rid.to_owned();
        ctx.rest_task(move |rest| api::room_roles(rest, t, rid), Msg::Roles)
    }

    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        if let Some(rid) = &self.dm_pending
            && changes.rooms.contains(rid)
            && ctx.workspace.room(rid).is_some()
        {
            let rid = self.dm_pending.take().unwrap_or_default();
            return Action::effect(Effect::OpenRoom(rid));
        }
        Action::none()
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Loaded(r) => {
                self.loading = false;
                match r {
                    Ok(u) => {
                        self.user = Some(u);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(common::failure_text(&e)),
                }
                Action::none()
            }
            Msg::Roles(r) => {
                match r {
                    Ok(list) => {
                        let username = self.username.as_str();
                        self.room_roles = list
                            .into_iter()
                            .find(|r| r.u.username.as_deref() == Some(username))
                            .map(|r| r.roles)
                            .unwrap_or_default();
                        self.roles_state = RolesState::Loaded;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "room roles failed");
                        self.roles_state = RolesState::Failed(common::failure_text(&e));
                    }
                }
                Action::none()
            }
            Msg::Retry => self.activate(ctx),
            Msg::RetryRoles => Action::task(self.load_roles(ctx)),
            Msg::Mention => Action::effect(Effect::Compose(ComposeIntent::Mention(
                self.username.clone(),
            ))),
            Msg::DirectMessage => {
                if self.busy {
                    return Action::none();
                }
                self.busy = true;
                let username = self.username.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.im_create(&username).await.map(|r| r.id) },
                    Msg::DmCreated,
                ))
            }
            Msg::DmCreated(r) => {
                self.busy = false;
                match r {
                    Ok(rid) if ctx.workspace.room(&rid).is_some() => {
                        Action::effect(Effect::OpenRoom(rid))
                    }
                    Ok(rid) => {
                        self.dm_pending = Some(rid);
                        Action::none()
                    }
                    Err(e) => {
                        Action::effect(common::error_toast(&t!("room_panel.direct_message"), &e))
                    }
                }
            }
            Msg::Action(a) if needs_confirm(&a) => {
                self.confirm = Some(a);
                Action::none()
            }
            Msg::Action(a) => self.run(a, ctx),
            Msg::Confirm => match self.confirm.clone() {
                Some(a) => self.run(a, ctx),
                None => Action::none(),
            },
            Msg::Cancel => {
                self.confirm = None;
                Action::none()
            }
            Msg::ActionDone(a, r) => {
                self.busy = false;
                self.confirm = None;
                match r {
                    Ok(()) => {
                        let reload = self.load_roles(ctx);
                        Action::task(reload).with(common::ok_toast(done_text(&a, &self.username)))
                    }
                    Err(e) => Action::effect(common::error_toast(&action_label(&a, false), &e)),
                }
            }
        }
    }

    fn run(&mut self, a: UserAction, ctx: &UpdateCtx<'_>) -> Action<Msg> {
        let Some(user) = &self.user else {
            return Action::none();
        };
        let Some(entry) = ctx.workspace.room(ctx.rid) else {
            return Action::none();
        };
        if self.busy {
            return Action::none();
        }
        let team = entry
            .is_team()
            .then(|| entry.room.as_ref().and_then(|r| r.team_id.clone()))
            .flatten();
        let call = api::user_action_call(
            &a,
            entry.room_type(),
            ctx.rid,
            team.as_deref(),
            &user.id,
            &self.username,
        );
        self.busy = true;
        Action::task(ctx.rest_task(
            move |rest| api::run_call(rest, call),
            move |r| Msg::ActionDone(a, r),
        ))
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let Some(user) = &self.user else {
            if let Some(e) = &self.error {
                return common::error_state(e, Msg::Retry);
            }
            return common::loading();
        };
        let ws = ctx.workspace;
        let name = user.display_name().to_owned();
        let status = ws
            .presence()
            .get(&user.id)
            .map(|p| p.status)
            .or(user.status)
            .unwrap_or(UserStatus::Offline);
        let status_text = ws
            .presence()
            .get(&user.id)
            .and_then(|p| p.status_text.clone())
            .or(user.status_text.clone())
            .filter(|s| !s.trim().is_empty());

        // Same initials as the header and the sidebar.
        let avatar_name = common::user_display(
            ws,
            common::use_real_names(ctx),
            &self.username,
            user.name.as_deref(),
        );
        let mut col = column![
            container(common::user_avatar(
                ctx,
                &self.username,
                &avatar_name,
                120.0
            ))
            .center_x(Length::Fill),
            container(text(name.clone()).size(TEXT_TITLE).font(font_semibold()))
                .center_x(Length::Fill),
            container(text(format!("@{}", self.username)).style(style::text_secondary))
                .center_x(Length::Fill),
            container(
                row![
                    presence_dot(common::presence_of(status), 10.0),
                    text(status_text.unwrap_or_else(|| common::status_label(status)))
                        .size(TEXT_SMALL)
                        .style(style::text_secondary),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
            )
            .center_x(Length::Fill),
        ]
        .spacing(SPACE_2)
        .padding(SPACE_4);

        let mut roles: Vec<(String, ChipKind)> = self
            .room_roles
            .iter()
            .map(|r| (common::role_label(r), ChipKind::Primary))
            .collect();
        for r in user.roles.iter().flatten() {
            if r != "user" {
                roles.push((common::role_label(r), ChipKind::Neutral));
            }
        }
        if !roles.is_empty() {
            col = col.push(container(common::badges(roles)).center_x(Length::Fill));
        }

        // Primary actions.
        let me = ws.user_id() == Some(user.id.as_str());
        let mut primary = row![].spacing(SPACE_2);
        let in_this_dm =
            crate::room::header::dm_partner(ws, ctx.rid).as_deref() == Some(self.username.as_str());
        if !me && !in_this_dm && permissions::can_create_dm(ws) {
            primary = primary.push(
                rustket_ui::widget::secondary_button(
                    t!("room_panel.direct_message").into_owned(),
                    (!self.busy).then_some(Msg::DirectMessage),
                )
                .icon(Icon::MessageSquare)
                .loading(self.busy && self.dm_pending.is_none() && self.confirm.is_none()),
            );
        }
        if permissions::can_post(ws, ctx.rid) {
            primary = primary.push(
                rustket_ui::widget::secondary_button(
                    t!("room_panel.mention").into_owned(),
                    Some(Msg::Mention),
                )
                .icon(Icon::AtSign),
            );
        }
        col = col.push(container(primary).center_x(Length::Fill));

        if let Some(bio) = user.bio.as_deref().filter(|b| !b.trim().is_empty()) {
            col = col.push(common::field(
                t!("room_panel.bio").into_owned(),
                common::body_text(bio),
            ));
        }
        if let Some(off) = user.utc_offset
            && let Some(lt) = local_time(off, ctx.prefs.clock_24h)
        {
            col = col.push(common::field(
                t!("room_panel.local_time").into_owned(),
                common::body_text(&lt),
            ));
        }
        let emails: Vec<String> = user
            .emails
            .iter()
            .flatten()
            .map(|e| e.address.clone())
            .collect();
        if !emails.is_empty() {
            col = col.push(common::field(
                t!("room_panel.email").into_owned(),
                common::body_text(&emails.join("\n")),
            ));
        }
        if let Some(nick) = user.nickname.as_deref().filter(|n| !n.is_empty()) {
            col = col.push(common::field(
                t!("room_panel.nickname").into_owned(),
                common::body_text(nick),
            ));
        }

        let team_main = ws.room(ctx.rid).is_some_and(|e| e.is_team());
        let known_roles = (self.roles_state == RolesState::Loaded).then_some(&self.room_roles[..]);
        let actions = available_actions(ws, ctx.rid, &user.id, &self.username, known_roles);
        let roles_failed = match &self.roles_state {
            RolesState::Failed(e) => Some(e.clone()),
            _ => None,
        };
        if !actions.is_empty() || roles_failed.is_some() {
            let mut list = column![].spacing(SPACE_1);
            if let Some(e) = roles_failed {
                list = list.push(
                    row![
                        text(t!("room_panel.roles_failed", error = e).into_owned())
                            .size(TEXT_SMALL)
                            .style(style::text_danger)
                            .width(Length::Fill),
                        rustket_ui::widget::secondary_button(
                            t!("room_panel.retry").into_owned(),
                            Some(Msg::RetryRoles)
                        ),
                    ]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center),
                );
            }
            for a in actions {
                list = list.push(common::action_entry(
                    action_icon(&a),
                    action_label(&a, team_main),
                    matches!(a, UserAction::Remove | UserAction::Mute(true)),
                    (!self.busy).then_some(Msg::Action(a)),
                ));
            }
            col = col.push(divider()).push(list);
        }
        if let Some(a) = &self.confirm {
            let room = ws
                .room(ctx.rid)
                .map(|e| e.fname().unwrap_or(e.name()).to_owned())
                .unwrap_or_default();
            let msg = match a {
                UserAction::Remove => t!(
                    "room_panel.confirm_remove",
                    user = self.username,
                    room = room
                ),
                _ => t!("room_panel.confirm_mute", user = self.username, room = room),
            };
            col = col.push(common::confirm_card(
                msg.into_owned(),
                action_label(a, team_main),
                true,
                self.busy,
                Msg::Confirm,
                Msg::Cancel,
            ));
        }
        scrollable(col.spacing(SPACE_3))
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use rocketchat::models::{Permission, Subscription};
    use rustket_core::workspace::Change;
    use serde_json::json;

    fn ws(roles: &[&str], t: &str) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(
            serde_json::from_value(json!({"_id": "me", "username": "me", "roles": ["user"]}))
                .unwrap(),
        );
        let sub: Subscription = serde_json::from_value(json!({
            "_id": "s", "rid": "r", "name": "dev", "t": t, "u": {"_id": "me"}, "roles": roles,
            "ignored": ["ign"]
        }))
        .unwrap();
        ws.apply_subscription(Change::Inserted, sub);
        ws.apply_room(
            Change::Inserted,
            serde_json::from_value(json!({"_id": "r", "t": t, "name": "dev", "muted": ["bob"]}))
                .unwrap(),
        );
        for (p, r) in [
            (ids::SET_OWNER, vec!["owner"]),
            (ids::SET_MODERATOR, vec!["owner"]),
            (ids::SET_LEADER, vec!["owner"]),
            (ids::MUTE_USER, vec!["owner", "moderator"]),
            (ids::REMOVE_USER, vec!["owner", "moderator"]),
        ] {
            ws.apply_permission(
                Change::Updated,
                Permission {
                    id: p.into(),
                    roles: r.into_iter().map(String::from).collect(),
                    updated_at: None,
                },
            );
        }
        ws
    }

    #[test]
    fn owner_gets_all_actions() {
        let ws = ws(&["owner"], "c");
        let a = available_actions(&ws, "r", "u-bob", "bob", Some(&["moderator".into()]));
        assert_eq!(
            a,
            vec![
                UserAction::SetRole(Role::Owner, true),
                UserAction::SetRole(Role::Moderator, false),
                UserAction::SetRole(Role::Leader, true),
                UserAction::Mute(false),
                UserAction::Remove,
                UserAction::Ignore(true),
            ]
        );
        let a = available_actions(&ws, "r", "ign", "x", Some(&[]));
        assert!(a.contains(&UserAction::Ignore(false)));
        assert!(a.contains(&UserAction::Mute(true)));
    }

    #[test]
    fn plain_member_only_ignores_and_never_self() {
        let ws = ws(&[], "p");
        assert_eq!(
            available_actions(&ws, "r", "u-bob", "bob", Some(&[])),
            vec![UserAction::Ignore(true)]
        );
        assert!(available_actions(&ws, "r", "me", "me", Some(&[])).is_empty());
    }

    #[test]
    fn role_actions_wait_for_roles() {
        let ws = ws(&["owner"], "c");
        let a = available_actions(&ws, "r", "u-bob", "bob", None);
        assert!(!a.iter().any(|a| matches!(a, UserAction::SetRole(..))));
        assert!(a.contains(&UserAction::Remove));
    }

    #[test]
    fn no_actions_in_dms() {
        let ws = ws(&["owner"], "d");
        assert!(available_actions(&ws, "r", "u-bob", "bob", Some(&[])).is_empty());
    }

    #[test]
    fn local_time_formats_offset() {
        let s = local_time(5.5, true).unwrap();
        assert!(s.ends_with("(UTC+5:30)"), "{s}");
        let s = local_time(-3.0, true).unwrap();
        assert!(s.ends_with("(UTC-3)"), "{s}");
    }
}
