//! Agent availability (`docs/omnichannel.md` §3): the toggle in the status
//! menu and the indicator next to the account name. The source of truth is
//! `me.statusLivechat` (re-read from `/me` after the server's `userData`
//! event), never the local click.

use iced::widget::{button, container, row, text, toggler};
use iced::{Alignment, Element, Length};
use rocketchat::models::{AgentStatus, UserStatus};
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::omnichannel::permissions::{self as perm, settings};
use rustket_core::permissions::has_permission;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, ellipsis};

use crate::component::{Action, Ctx, Effect};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    /// Switch to the given availability.
    Set(AgentStatus),
    /// Wanted status, [`Workspace::me_generation`] when it was sent, answer.
    Done(AgentStatus, u64, Res<AgentStatus>),
    /// Open the Contact Center (handled by the sidebar / app).
    ContactCenter,
}

/// Whether the Contact Center entry is offered (`Livechat_enabled`,
/// `view-omnichannel-contact-center`).
pub fn contact_center_allowed(ws: &Workspace) -> bool {
    ws.settings().bool_or(settings::ENABLED, false)
        && has_permission(ws, perm::VIEW_OMNICHANNEL_CONTACT_CENTER, None)
}

/// The availability to show a toggle for: only for agents (§2: admins with
/// `view-l-room` get "Agent not found") while omnichannel is accessible.
pub fn availability(ws: &Workspace) -> Option<AgentStatus> {
    let me = ws.me()?;
    let accessible = ws.settings().bool_or(settings::ENABLED, false)
        && has_permission(ws, perm::VIEW_L_ROOM, None);
    (accessible && me.is_livechat_agent()).then(|| match me.agent_status() {
        AgentStatus::Available => AgentStatus::Available,
        _ => AgentStatus::NotAvailable,
    })
}

/// Whether the user is an agent who currently receives chats (sign-out makes
/// such an agent unavailable, like the web client's DDP logout).
pub fn is_available_agent(ws: &Workspace) -> bool {
    availability(ws) == Some(AgentStatus::Available)
}

/// Stores the availability `livechat/agent.status` answered in `me`.
fn apply_status(ws: &mut Workspace, status: AgentStatus) {
    if status == AgentStatus::Unknown {
        return;
    }
    if let Some(mut me) = ws.me().cloned()
        && me.status_livechat != Some(status)
    {
        me.status_livechat = Some(status);
        ws.apply_me(me);
    }
}

/// Applies the `agent.status` answer `got` to a request sent at `me`
/// generation `sent`, unless a newer `/me` (the source of truth) arrived since.
/// Returns whether it was applied.
fn apply_answer(ws: &mut Workspace, sent: u64, got: AgentStatus) -> bool {
    if ws.me_generation() != sent {
        return false;
    }
    apply_status(ws, got);
    true
}

pub fn update(msg: Msg, ctx: &mut Ctx<'_>) -> Action<Msg> {
    match msg {
        // Setting an explicit status is idempotent: retry once after a
        // transient failure (e.g. a pooled connection the server closed).
        Msg::Set(status) => {
            let sent = ctx.workspace.me_generation();
            Action::task(ctx.rest_task(
                move |rest| async move {
                    match rest.livechat_agent_status(Some(status)).await {
                        Err(e) if e.is_transient() => {
                            tracing::debug!(error = %e, "agent.status failed; retrying");
                            rest.livechat_agent_status(Some(status)).await
                        }
                        r => r,
                    }
                },
                move |r| Msg::Done(status, sent, r),
            ))
        }
        Msg::Done(wanted, sent, Ok(got)) => {
            // The server's answer to the user's own request is the current
            // availability: shown even when the `/me` re-read after its
            // `userData` fails, or when nothing changed (no `userData` then).
            // The session's cached user takes it too, so a later `/me` with
            // another availability is not mistaken for "unchanged".
            if apply_answer(ctx.workspace, sent, got)
                && got != AgentStatus::Unknown
                && let Some(session) = ctx.session
            {
                session.session().note_agent_status(got);
            }
            if got != wanted {
                Action::effect(Effect::Toast(
                    BannerKind::Warning,
                    t!("omnichannel.availability_failed").into_owned(),
                    t!("omnichannel.availability_unchanged").into_owned(),
                ))
            } else {
                Action::none()
            }
        }
        Msg::Done(_, _, Err(f)) => Action::effect(Effect::Toast(
            BannerKind::Error,
            t!("omnichannel.availability_failed").into_owned(),
            crate::omnichannel::error_text(&f),
        )),
        // Intercepted by the sidebar before it reaches the status menu.
        Msg::ContactCenter => Action::none(),
    }
}

/// The status menu entries: "Available for chats" with a switch (agents),
/// a hint when the user is offline (offline agents get no chats, §3.2), and
/// the Contact Center.
pub fn menu_entry<'a, M: Clone + 'a>(
    ws: &Workspace,
    enabled: bool,
    wrap: fn(Msg) -> M,
) -> Option<Element<'a, M>> {
    let toggle = availability(ws).map(|status| toggle_entry(ws, status, enabled, wrap));
    let cc: Option<Element<'a, M>> = contact_center_allowed(ws).then(|| {
        button(
            row![
                icon::sized(Icon::Contact, 16.0),
                ellipsis(t!("omnichannel.contact_center").into_owned())
                    .size(TEXT_BODY)
                    .width(Length::Fill),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([6, 8])
        .style(style::button_menu_item(false))
        .on_press(wrap(Msg::ContactCenter))
        .into()
    });
    match (toggle, cc) {
        (None, None) => None,
        (t, c) => Some(iced::widget::column![t, c].into()),
    }
}

fn toggle_entry<'a, M: Clone + 'a>(
    ws: &Workspace,
    status: AgentStatus,
    enabled: bool,
    wrap: fn(Msg) -> M,
) -> Element<'a, M> {
    let on = status == AgentStatus::Available;
    let next = if on {
        AgentStatus::NotAvailable
    } else {
        AgentStatus::Available
    };
    let msg = enabled.then(|| wrap(Msg::Set(next)));
    let label = if on {
        t!("omnichannel.available")
    } else {
        t!("omnichannel.not_available")
    };
    let switch = toggler(on)
        .on_toggle_maybe(msg.clone().map(|m| move |_| m.clone()))
        .size(16)
        .style(style::toggler);
    let mut col = iced::widget::column![
        button(
            row![
                icon::sized(
                    if on {
                        Icon::Headset
                    } else {
                        Icon::HeadphoneOff
                    },
                    16.0
                ),
                ellipsis(label.into_owned())
                    .size(TEXT_BODY)
                    .width(Length::Fill),
                switch,
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([6, 8])
        .style(style::button_menu_item(false))
        .on_press_maybe(msg),
    ];
    if on && ws.my_status() == UserStatus::Offline {
        col = col.push(
            container(
                text(t!("omnichannel.offline_hint").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            )
            .padding([SPACE_1, SPACE_2]),
        );
    }
    col.into()
}

/// A small headset next to the account name while the agent is available.
pub fn indicator<'a, M: 'a>(ws: &Workspace) -> Option<Element<'a, M>> {
    (availability(ws)? == AgentStatus::Available).then(|| {
        icon::sized(Icon::Headset, 14.0)
            .style(style::text_success)
            .into()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Permission, Setting, User};
    use rustket_core::Change;
    use serde_json::json;

    pub(crate) fn agent_ws(roles: &[&str], status: &str) -> Workspace {
        let mut ws = Workspace::new();
        let me: User = serde_json::from_value(json!({
            "_id": "me", "username": "me", "roles": roles, "statusLivechat": status
        }))
        .unwrap();
        ws.apply_me(me);
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(true),
                ..Default::default()
            },
        );
        let p: Permission = serde_json::from_value(
            json!({"_id": perm::VIEW_L_ROOM, "roles": ["livechat-agent", "admin"]}),
        )
        .unwrap();
        ws.apply_permission(Change::Inserted, p);
        ws
    }

    /// The `agent.status` answer is shown without waiting for `/me`.
    #[test]
    fn agent_status_answer_updates_the_toggle() {
        let mut ws = agent_ws(&["user", "livechat-agent"], "not-available");
        let g = ws.me_generation();
        apply_status(&mut ws, AgentStatus::Available);
        assert_eq!(availability(&ws), Some(AgentStatus::Available));
        assert!(ws.me_generation() > g);
        apply_status(&mut ws, AgentStatus::Unknown);
        assert_eq!(availability(&ws), Some(AgentStatus::Available));
    }

    /// A late answer does not overwrite a newer `/me`.
    #[test]
    fn late_agent_status_answer_yields_to_newer_me() {
        let mut ws = agent_ws(&["user", "livechat-agent"], "not-available");
        let sent = ws.me_generation();
        assert!(apply_answer(&mut ws, sent, AgentStatus::Available));
        assert_eq!(availability(&ws), Some(AgentStatus::Available));

        let sent = ws.me_generation();
        let mut me = ws.me().cloned().unwrap();
        me.status_livechat = Some(AgentStatus::NotAvailable);
        ws.apply_me(me);
        assert!(!apply_answer(&mut ws, sent, AgentStatus::Available));
        assert_eq!(availability(&ws), Some(AgentStatus::NotAvailable));
    }

    #[test]
    fn toggle_only_for_agents() {
        let ws = agent_ws(&["user", "livechat-agent"], "available");
        assert_eq!(availability(&ws), Some(AgentStatus::Available));
        assert!(is_available_agent(&ws));
        let ws = agent_ws(&["user", "livechat-agent"], "not-available");
        assert_eq!(availability(&ws), Some(AgentStatus::NotAvailable));
        // Admin with `view-l-room` but no agent role: no toggle.
        let ws = agent_ws(&["user", "admin"], "available");
        assert_eq!(availability(&ws), None);
        assert!(!is_available_agent(&ws));
        // Livechat disabled.
        let mut ws = agent_ws(&["livechat-agent"], "available");
        ws.apply_setting(
            Change::Updated,
            Setting {
                id: settings::ENABLED.into(),
                value: json!(false),
                ..Default::default()
            },
        );
        assert_eq!(availability(&ws), None);
    }
}
