//! Room-level E2E UI: the header key icon, the room menu entries with their
//! enable/disable/reset dialogs (§7.9), the room body states shown instead
//! of the timeline (§10) and the composer notices (§7.6).

use iced::widget::{column, container, row, text, tooltip};
use iced::{Alignment, Element, Length};
use rocketchat::models::RoomType;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::e2e::{AccountE2eState, E2eManagerError, RoomE2eState};
use rustket_core::permissions::has_permission;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_SMALL, TEXT_SUBTITLE,
};
use rustket_ui::widget::{
    BannerKind, MenuEntry, MenuItem, danger_button, dialog, ghost_button, primary_button,
    secondary_button,
};

use super::RoomRequest;
use super::outgoing::{self, Mode};
use crate::room::{Action, Effect, UpdateCtx, ViewCtx};

const TOGGLE_PERMISSION: &str = "toggle-room-e2e-encryption";

/// The room dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    Enable,
    Disable,
    Reset,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Open(Dialog),
    Confirm,
    Cancel,
    Done(Dialog, Result<(), String>),
    /// The account modal (room body buttons).
    OpenAccountModal,
}

/// E2E state of one open room (kept by the header).
#[derive(Debug, Default)]
pub struct RoomE2e {
    pub dialog: Option<Dialog>,
    busy: bool,
}

fn room_type(ws: &Workspace, rid: &str) -> Option<RoomType> {
    ws.room(rid).map(|e| e.room_type())
}

fn encrypted(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid).is_some_and(|e| e.is_encrypted())
}

fn federated(ws: &Workspace, rid: &str) -> bool {
    ws.room(rid)
        .and_then(|e| e.room.as_ref())
        .and_then(|r| r.federated)
        == Some(true)
}

fn account_ready(ws: &Workspace) -> bool {
    ws.e2e().is_some_and(|m| m.account_state().is_ready())
}

/// The key icon of the header (`E2E_Enable && room.encrypted`).
pub fn header_icon<'a, M: 'a>(ws: &Workspace, rid: &str) -> Option<Element<'a, M>> {
    if !outgoing::settings(ws).enabled || !encrypted(ws, rid) {
        return None;
    }
    Some(
        tooltip(
            icon::sized(Icon::KeyRound, 14.0).style(style::text_success),
            container(text(t!("e2e.encrypted_room")).size(TEXT_SMALL))
                .padding([SPACE_1, SPACE_2])
                .style(style::container_popover),
            tooltip::Position::Bottom,
        )
        .into(),
    )
}

/// The room menu shows the encryption toggle (web `useE2EERoomAction`).
pub fn can_toggle(ws: &Workspace, rid: &str) -> bool {
    let Some(t) = room_type(ws, rid) else {
        return false;
    };
    outgoing::settings(ws).enabled
        && matches!(t, RoomType::Private | RoomType::Direct)
        && ws.subscription(rid).is_some()
        && (t == RoomType::Direct
            || (has_permission(ws, "edit-room", Some(rid))
                && has_permission(ws, TOGGLE_PERMISSION, Some(rid))))
        && (account_ready(ws) || encrypted(ws, rid))
}

/// A room key reset is offered (web `canResetRoomKey`; the permission is
/// required in DMs too, §11.18).
pub fn can_reset(ws: &Workspace, rid: &str) -> bool {
    let Some(mgr) = ws.e2e() else {
        return false;
    };
    let has_kid = ws
        .room(rid)
        .and_then(|e| e.room.as_ref())
        .is_some_and(|r| r.e2e_key_id.is_some());
    outgoing::settings(ws).enabled
        && mgr.account_state().is_ready()
        && encrypted(ws, rid)
        && has_kid
        && has_permission(ws, TOGGLE_PERMISSION, Some(rid))
        && matches!(
            mgr.room_state(rid),
            RoomE2eState::NotStarted
                | RoomE2eState::Disabled
                | RoomE2eState::Error(_)
                | RoomE2eState::WaitingKeys
        )
}

/// Room menu entries (a separator first when there are any).
pub fn menu_entries(ws: &Workspace, rid: &str) -> Vec<MenuEntry<Msg>> {
    let mut v = Vec::new();
    if can_toggle(ws, rid) {
        let on = encrypted(ws, rid);
        let item = if on {
            MenuItem::new(
                t!("e2e.disable_room").into_owned(),
                Msg::Open(Dialog::Disable),
            )
            .icon(Icon::LockOpen)
        } else {
            MenuItem::new(
                t!("e2e.enable_room").into_owned(),
                Msg::Open(Dialog::Enable),
            )
            .icon(Icon::KeyRound)
        };
        v.push(item.disabled(federated(ws, rid)).into());
    }
    if can_reset(ws, rid) {
        v.push(
            MenuItem::new(
                t!("e2e.reset_room_key").into_owned(),
                Msg::Open(Dialog::Reset),
            )
            .icon(Icon::RefreshCw)
            .danger(true)
            .into(),
        );
    }
    if !v.is_empty() {
        v.insert(0, MenuEntry::Separator);
    }
    v
}

/// [`menu_entries`] with the messages wrapped by `f`.
pub fn menu_entries_with<M>(ws: &Workspace, rid: &str, f: fn(Msg) -> M) -> Vec<MenuEntry<M>> {
    menu_entries(ws, rid)
        .into_iter()
        .map(|e| match e {
            MenuEntry::Item(i) => MenuEntry::Item(MenuItem {
                icon: i.icon,
                label: i.label,
                hint: i.hint,
                danger: i.danger,
                disabled: i.disabled,
                on_select: f(i.on_select),
            }),
            MenuEntry::Header(h) => MenuEntry::Header(h),
            MenuEntry::Separator => MenuEntry::Separator,
        })
        .collect()
}

fn error_text(e: &E2eManagerError) -> String {
    match e {
        E2eManagerError::NoRoomKeyId => t!("e2e.reset_no_key").into_owned(),
        E2eManagerError::NotReady => t!("e2e.reset_not_ready").into_owned(),
        other => super::error_text(other),
    }
}

impl RoomE2e {
    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Open(d) => {
                self.dialog = Some(d);
                self.busy = false;
                Action::none()
            }
            Msg::Cancel => {
                if !self.busy {
                    self.dialog = None;
                }
                Action::none()
            }
            Msg::Confirm => {
                let (Some(d), false) = (self.dialog, self.busy) else {
                    return Action::none();
                };
                let Some(mgr) = ctx.workspace.e2e().cloned() else {
                    self.dialog = None;
                    return Action::effect(toast(BannerKind::Error, t!("e2e.not_running")));
                };
                self.busy = true;
                let rid = ctx.rid.to_owned();
                let task = match d {
                    Dialog::Enable | Dialog::Disable => {
                        let on = d == Dialog::Enable;
                        iced::Task::perform(
                            async move { mgr.toggle_room(&rid, on).await },
                            move |r| Msg::Done(d, r.map_err(|e| error_text(&e))),
                        )
                    }
                    Dialog::Reset => iced::Task::perform(
                        async move { mgr.reset_room_key(&rid).await },
                        move |r| Msg::Done(d, r.map_err(|e| error_text(&e))),
                    ),
                };
                Action::task(task)
            }
            Msg::Done(d, r) => {
                self.busy = false;
                self.dialog = None;
                match r {
                    Ok(()) => {
                        super::shown::keys_changed();
                        let key = match d {
                            Dialog::Enable => "e2e.enabled_for_room",
                            Dialog::Disable => "e2e.disabled_for_room",
                            Dialog::Reset => "e2e.room_key_reset",
                        };
                        Action::effect(toast(BannerKind::Success, t!(key)))
                    }
                    Err(e) => Action::effect(Effect::Toast(
                        BannerKind::Error,
                        t!("e2e.room_action_failed").into_owned(),
                        e,
                    )),
                }
            }
            Msg::OpenAccountModal => Action::effect(Effect::E2e(RoomRequest::OpenModal)),
        }
    }

    /// The open dialog (drawn over the room) and the message that closes it.
    pub fn overlay<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<(Element<'a, Msg>, Msg)> {
        let d = self.dialog?;
        let busy = self.busy;
        let (title, body, confirm): (String, Element<'a, Msg>, Element<'a, Msg>) = match d {
            Dialog::Enable => (
                t!("e2e.enable_room").into_owned(),
                text(t!("e2e.enable_room_body")).size(TEXT_BODY).into(),
                primary_button(
                    t!("e2e.enable_room_confirm"),
                    (!busy).then_some(Msg::Confirm),
                )
                .loading(busy)
                .into(),
            ),
            Dialog::Disable => {
                let mut c =
                    column![text(t!("e2e.disable_room_body")).size(TEXT_BODY)].spacing(SPACE_3);
                if can_reset(ctx.workspace, ctx.rid) {
                    c = c.push(
                        column![
                            text(t!("e2e.disable_room_reset_hint"))
                                .size(TEXT_SMALL)
                                .style(style::text_secondary),
                            row![
                                ghost_button(
                                    t!("e2e.reset_room_key"),
                                    (!busy).then_some(Msg::Open(Dialog::Reset)),
                                )
                                .icon(Icon::RefreshCw),
                            ],
                        ]
                        .spacing(SPACE_1),
                    );
                }
                (
                    t!("e2e.disable_room").into_owned(),
                    c.into(),
                    danger_button(
                        t!("e2e.disable_room_confirm"),
                        (!busy).then_some(Msg::Confirm),
                    )
                    .loading(busy)
                    .into(),
                )
            }
            Dialog::Reset => (
                t!("e2e.reset_room_key").into_owned(),
                text(t!("e2e.reset_room_key_body")).size(TEXT_BODY).into(),
                danger_button(
                    t!("e2e.reset_room_key_confirm"),
                    (!busy).then_some(Msg::Confirm),
                )
                .loading(busy)
                .into(),
            ),
        };
        let el = dialog(
            title,
            body,
            vec![
                secondary_button(t!("e2e.cancel"), (!busy).then_some(Msg::Cancel)).into(),
                confirm,
            ],
            (!busy).then_some(Msg::Cancel),
        );
        Some((el, Msg::Cancel))
    }
}

fn toast(kind: BannerKind, title: impl Into<String>) -> Effect {
    Effect::Toast(kind, title.into(), String::new())
}

/// What replaces the room body (§10): only for encrypted rooms on a server
/// with E2E on that does not allow unencrypted messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyState {
    SavePassword,
    EnterPassword,
    WaitingKeys,
}

pub fn body_state(ws: &Workspace, rid: &str) -> Option<BodyState> {
    let s = outgoing::settings(ws);
    if !s.enabled || s.allow_unencrypted || !encrypted(ws, rid) || ws.subscription(rid).is_none() {
        return None;
    }
    let mgr = ws.e2e()?;
    match mgr.account_state() {
        AccountE2eState::SavePassword => Some(BodyState::SavePassword),
        AccountE2eState::EnterPassword { .. } => Some(BodyState::EnterPassword),
        AccountE2eState::Ready if mgr.room_state(rid) == RoomE2eState::WaitingKeys => {
            Some(BodyState::WaitingKeys)
        }
        _ => None,
    }
}

/// The room body replacement, if any.
pub fn body<'a>(ctx: &ViewCtx<'a>, room_name: String) -> Option<Element<'a, Msg>> {
    let state = body_state(ctx.workspace, ctx.rid)?;
    let (ic, title, subtitle, action) = match state {
        BodyState::SavePassword => (
            Icon::KeyRound,
            t!("e2e.body_encrypted_title", room = room_name).into_owned(),
            t!("e2e.body_save_password").into_owned(),
            Some(t!("e2e.banner_save_password_action").into_owned()),
        ),
        BodyState::EnterPassword => (
            Icon::KeyRound,
            t!("e2e.body_encrypted_title", room = room_name).into_owned(),
            t!("e2e.body_enter_password").into_owned(),
            Some(t!("e2e.banner_enter_password_action").into_owned()),
        ),
        BodyState::WaitingKeys => (
            Icon::Clock,
            t!("e2e.body_waiting_title").into_owned(),
            t!("e2e.body_waiting", room = room_name).into_owned(),
            None,
        ),
    };
    let mut c = column![
        icon::sized(ic, 48.0).style(style::text_hint),
        text(title).size(TEXT_SUBTITLE).font(FONT_SEMIBOLD),
        text(subtitle)
            .size(TEXT_BODY)
            .style(style::text_secondary)
            .align_x(iced::alignment::Horizontal::Center),
    ]
    .spacing(SPACE_2)
    .align_x(Alignment::Center)
    .max_width(420);
    if let Some(label) = action {
        c = c.push(primary_button(label, Some(Msg::OpenAccountModal)));
    }
    Some(container(c).center(Length::Fill).padding(SPACE_3).into())
}

/// The composer notice replacing the editor while sending is blocked
/// (encrypted room without its key, unencrypted messages not allowed).
pub fn composer_block<'a, M: 'a>(ws: &Workspace, rid: &str) -> Option<Element<'a, M>> {
    if !matches!(outgoing::mode(ws, rid), Mode::Blocked) {
        return None;
    }
    let key = match ws.e2e().map(|m| m.account_state()) {
        Some(AccountE2eState::EnterPassword { .. }) => "e2e.composer_locked",
        Some(s) if s.is_ready() => "e2e.composer_waiting",
        _ => "e2e.composer_unavailable",
    };
    Some(notice(Icon::Lock, t!(key).into_owned()))
}

/// A hint above the editor: messages go out unencrypted (§7.6).
pub fn composer_hint<'a, M: 'a>(ws: &Workspace, rid: &str) -> Option<Element<'a, M>> {
    matches!(outgoing::mode(ws, rid), Mode::Unencrypted)
        .then(|| notice(Icon::LockOpen, t!("e2e.composer_unencrypted").into_owned()))
}

/// Shown while the text starts with `/` in a room where slash commands do
/// not run (§2.1; the web lists them disabled as "Unavailable in encrypted
/// channels").
pub fn slash_hint<'a, M: 'a>(ws: &Workspace, rid: &str, slash: bool) -> Option<Element<'a, M>> {
    (slash && outgoing::slash_commands_blocked(ws, rid))
        .then(|| notice(Icon::Lock, t!("e2e.slash_unavailable").into_owned()))
}

/// The search panel's note for encrypted rooms (the server cannot search
/// encrypted content, §8).
pub fn search_hint<'a, M: 'a>(ws: &Workspace, rid: &str) -> Option<Element<'a, M>> {
    encrypted(ws, rid).then(|| {
        row![
            icon::sized(Icon::Lock, 12.0).style(style::text_hint),
            text(t!("e2e.search_hint"))
                .size(TEXT_SMALL)
                .style(style::text_hint),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center)
        .into()
    })
}

fn notice<'a, M: 'a>(ic: Icon, label: String) -> Element<'a, M> {
    container(
        row![
            icon::sized(ic, 14.0).style(style::text_secondary),
            text(label).size(TEXT_SMALL).style(style::text_secondary),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding([SPACE_2, SPACE_3])
    .width(Length::Fill)
    .into()
}

/// Whether `m` is an end-to-end encrypted message.
pub fn is_encrypted_message(m: &rocketchat::models::Message) -> bool {
    m.t.as_deref() == Some("e2e") || m.content.is_some()
}

/// Whether `m` can be edited as far as encryption goes: an encrypted message
/// only when it is shown decrypted and the room key is ready. The error is
/// the text for a toast.
pub fn check_edit(ws: &Workspace, m: &rocketchat::models::Message) -> Result<(), String> {
    if m.t.as_deref() != Some("e2e") {
        return Ok(());
    }
    if !super::shown::is_decrypted(m) {
        return Err(t!("e2e.edit_unreadable").into_owned());
    }
    match outgoing::mode(ws, &m.rid) {
        Mode::Encrypt(_) => Ok(()),
        _ => Err(t!("e2e.edit_unavailable").into_owned()),
    }
}

/// Message actions not offered for encrypted messages (§10: copy link; the
/// web also disables reply-in-DM and forward, which Rustket does not have),
/// and edits/quotes of messages that are not shown decrypted.
pub fn filter_actions(
    ws: &Workspace,
    m: &rocketchat::models::Message,
    acts: &mut Vec<crate::room::timeline::Act>,
) {
    use crate::room::timeline::Act;
    if m.t.as_deref() != Some("e2e") {
        return;
    }
    let decrypted = super::shown::is_decrypted(m);
    let editable = check_edit(ws, m).is_ok();
    acts.retain(|a| match a {
        Act::CopyLink => false,
        Act::Edit => editable,
        Act::Quote | Act::CopyText => decrypted,
        _ => true,
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Permission, Room, Setting, Subscription};
    use rustket_core::workspace::Change;

    fn ws(t: RoomType, encrypted: bool, perms: bool) -> Workspace {
        let mut w = Workspace::new();
        w.apply_setting(
            Change::Updated,
            Setting {
                id: "E2E_Enable".into(),
                value: serde_json::json!(true),
                ..Default::default()
            },
        );
        w.apply_room(
            Change::Inserted,
            Room {
                id: "r".into(),
                t,
                encrypted: Some(encrypted),
                ..Default::default()
            },
        );
        w.apply_subscription(
            Change::Inserted,
            Subscription {
                rid: "r".into(),
                t,
                roles: Some(vec!["owner".into()]),
                ..Default::default()
            },
        );
        if perms {
            for p in ["edit-room", TOGGLE_PERMISSION] {
                w.apply_permission(
                    Change::Inserted,
                    Permission {
                        id: p.into(),
                        roles: vec!["owner".into()],
                        ..Default::default()
                    },
                );
            }
        }
        w
    }

    #[test]
    fn toggle_gating_follows_the_web() {
        // Encrypted private room, owner with the permissions.
        assert!(can_toggle(&ws(RoomType::Private, true, true), "r"));
        // Not encrypted and the account is not ready: hidden.
        assert!(!can_toggle(&ws(RoomType::Private, false, true), "r"));
        // Without permissions in a group: hidden; DMs need none.
        assert!(!can_toggle(&ws(RoomType::Private, true, false), "r"));
        assert!(can_toggle(&ws(RoomType::Direct, true, false), "r"));
        // Public channels cannot be encrypted.
        assert!(!can_toggle(&ws(RoomType::Channel, true, true), "r"));
        // No reset without a running manager.
        assert!(!can_reset(&ws(RoomType::Private, true, true), "r"));
        let entries = menu_entries(&ws(RoomType::Private, true, true), "r");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn body_and_composer_states_without_manager() {
        let w = ws(RoomType::Private, true, true);
        assert_eq!(body_state(&w, "r"), None);
        assert!(composer_block::<()>(&w, "r").is_some());
        assert!(composer_hint::<()>(&w, "r").is_none());
        assert!(header_icon::<()>(&w, "r").is_some());
        assert!(header_icon::<()>(&ws(RoomType::Private, false, true), "r").is_none());
    }
}
