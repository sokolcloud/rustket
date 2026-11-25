//! "Delete my account" (Security tab), shown only when the server allows
//! it (`Accounts_AllowDeleteOwnAccount`): the password (or, for accounts
//! without one, the username) confirms it; a last owner of rooms is told
//! which rooms change owner or are removed and confirms again
//! (`users.deleteOwnAccount`, `confirmRelinquish`). On success the account
//! is signed out here without revoking its (already deleted) token.

use iced::widget::{column, row, text};
use iced::{Element, Length};
use rocketchat::LastOwnerRooms;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, TEXT_SMALL};
use rustket_ui::widget::{BannerKind, banner, danger_button, secondary_button, text_field};

use super::{Message, section};
use crate::component::{Action, Ctx, Effect};
use crate::error::{Failure, Res};

pub const INPUT_ID: &str = "account-delete-secret";

#[derive(Debug, Clone)]
pub enum Msg {
    Open,
    Cancel,
    Secret(String),
    Submit,
    /// Delete anyway, giving up the rooms listed in the last-owner answer.
    ConfirmRelinquish,
    Done(Res<()>),
}

#[derive(Debug, Default)]
pub struct DeleteAccount {
    /// The form is shown.
    pub open: bool,
    pub secret: String,
    pub busy: bool,
    pub error: Option<String>,
    /// The server answered `user-last-owner`: these rooms change owner or
    /// are removed if the user goes on.
    pub owner_rooms: Option<LastOwnerRooms>,
}

/// Whether the section is shown: the server allows deleting one's account.
pub fn available(ws: &Workspace) -> bool {
    ws.settings().allow_delete_own_account()
}

/// Whether the user signs in with a local password (else the username
/// confirms), like the web client's `hasLocalPassword`. Unknown: yes.
pub fn has_local_password(ws: &Workspace) -> bool {
    let Some(services) = ws.me().and_then(|m| m.services.as_ref()) else {
        return true;
    };
    services
        .password
        .as_ref()
        .and_then(|p| p.get("exists"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// User-facing text of a failed deletion.
fn error_text(e: &Failure) -> String {
    match e.error().api_error_code() {
        Some("error-invalid-password") => t!("legal.delete_wrong_password").into_owned(),
        Some("error-invalid-username") => t!("legal.delete_wrong_username").into_owned(),
        Some("error-not-allowed") => t!("legal.delete_not_allowed").into_owned(),
        _ => t!("legal.delete_failed", error = e.user_text()).into_owned(),
    }
}

impl DeleteAccount {
    fn submit(&mut self, relinquish: bool, ctx: &Ctx<'_>) -> Action<Message> {
        if self.busy || self.secret.trim().is_empty() {
            return Action::none();
        }
        self.busy = true;
        self.error = None;
        let secret = if has_local_password(ctx.workspace) {
            self.secret.clone()
        } else {
            self.secret.trim().to_owned()
        };
        Action::task(ctx.rest_task(
            move |rest| async move { rest.users_delete_own_account(&secret, relinquish).await },
            |r| Message::Delete(Msg::Done(r)),
        ))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Msg::Open => {
                *self = Self {
                    open: true,
                    ..Self::default()
                };
                Action::task(iced::widget::operation::focus(INPUT_ID))
            }
            Msg::Cancel => {
                *self = Self::default();
                Action::none()
            }
            Msg::Secret(s) => {
                self.secret = s;
                self.error = None;
                self.owner_rooms = None;
                Action::none()
            }
            Msg::Submit => self.submit(false, ctx),
            Msg::ConfirmRelinquish => self.submit(true, ctx),
            Msg::Done(Ok(())) => {
                *self = Self::default();
                Action::effect(Effect::Toast(
                    BannerKind::Success,
                    t!("legal.delete_done").into_owned(),
                    String::new(),
                ))
                .with(Effect::AccountDeleted)
            }
            Msg::Done(Err(e)) => {
                self.busy = false;
                match e.error().last_owner_rooms() {
                    Some(rooms) => self.owner_rooms = Some(rooms),
                    None => self.error = Some(error_text(&e)),
                }
                Action::none()
            }
        }
    }

    /// The section, or nothing when the server does not allow it.
    pub fn view<'a>(&'a self, ws: &Workspace) -> Option<Element<'a, Message>> {
        if !available(ws) {
            return None;
        }
        let d = |m| Message::Delete(m);
        let mut c = column![
            text(t!("legal.delete_help"))
                .size(TEXT_SMALL)
                .style(style::text_secondary)
        ]
        .spacing(SPACE_3);
        if !self.open {
            c = c.push(danger_button(
                t!("legal.delete_open").into_owned(),
                Some(d(Msg::Open)),
            ));
            return Some(section(t!("legal.delete_title").into_owned(), c.into()));
        }
        let password = has_local_password(ws);
        let label = if password {
            t!("legal.delete_password")
        } else {
            t!("legal.delete_username")
        };
        c = c.push(
            text_field(label, &self.secret, move |v| {
                Message::Delete(Msg::Secret(v))
            })
            .password(password)
            .error(self.error.clone())
            .id(INPUT_ID)
            .on_submit(d(Msg::Submit))
            .disabled(self.busy || self.owner_rooms.is_some()),
        );
        let ready = !self.busy && !self.secret.trim().is_empty();
        if let Some(rooms) = &self.owner_rooms {
            let mut lines = vec![t!("legal.delete_last_owner").into_owned()];
            if !rooms.should_change_owner.is_empty() {
                lines.push(
                    t!(
                        "legal.delete_rooms_new_owner",
                        rooms = rooms.should_change_owner.join(", ")
                    )
                    .into_owned(),
                );
            }
            if !rooms.should_be_removed.is_empty() {
                lines.push(
                    t!(
                        "legal.delete_rooms_removed",
                        rooms = rooms.should_be_removed.join(", ")
                    )
                    .into_owned(),
                );
            }
            c = c.push(banner(BannerKind::Warning, lines.join("\n"), None));
            c = c.push(
                row![
                    iced::widget::space().width(Length::Fill),
                    secondary_button(t!("account.cancel"), Some(d(Msg::Cancel))),
                    danger_button(
                        t!("legal.delete_anyway").into_owned(),
                        ready.then_some(d(Msg::ConfirmRelinquish)),
                    )
                    .loading(self.busy),
                ]
                .spacing(SPACE_2),
            );
        } else {
            c = c.push(
                row![
                    iced::widget::space().width(Length::Fill),
                    secondary_button(t!("account.cancel"), Some(d(Msg::Cancel))),
                    danger_button(
                        t!("legal.delete_confirm").into_owned(),
                        ready.then_some(d(Msg::Submit)),
                    )
                    .loading(self.busy),
                ]
                .spacing(SPACE_2),
            );
        }
        Some(section(t!("legal.delete_title").into_owned(), c.into()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn ctx_parts() -> (
        Workspace,
        rustket_core::ServerId,
        rocketchat::ServerUrl,
        crate::images::Images,
        rustket_core::config::AppConfig,
    ) {
        let mut ws = Workspace::new();
        ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "services": {"password": {"exists": true}}
            }))
            .unwrap(),
        );
        (
            ws,
            rustket_core::ServerId("s".into()),
            rocketchat::ServerUrl::parse("http://localhost:3000").unwrap(),
            crate::images::Images::new(None),
            rustket_core::config::AppConfig::default(),
        )
    }

    fn failure(status: u16, body: &str) -> Failure {
        Failure::from(rocketchat::Error::from_response(
            status,
            None,
            body.as_bytes(),
        ))
    }

    #[test]
    fn shown_only_when_allowed() {
        let (mut ws, ..) = ctx_parts();
        let d = DeleteAccount::default();
        assert!(d.view(&ws).is_none());
        ws.apply_setting(
            rustket_core::workspace::Change::Inserted,
            rocketchat::models::Setting {
                id: "Accounts_AllowDeleteOwnAccount".into(),
                value: json!(true),
                ..Default::default()
            },
        );
        assert!(d.view(&ws).is_some());
    }

    #[test]
    fn password_or_username() {
        let (mut ws, ..) = ctx_parts();
        assert!(has_local_password(&ws));
        ws.apply_me(
            serde_json::from_value(json!({"_id": "me", "username": "me", "services": {}})).unwrap(),
        );
        assert!(!has_local_password(&ws));
        ws.apply_me(serde_json::from_value(json!({"_id": "me", "username": "me"})).unwrap());
        assert!(has_local_password(&ws), "unknown: ask for the password");
    }

    #[test]
    fn flow_and_error_states() {
        let (mut ws, sid, url, images, config) = ctx_parts();
        let mut ctx = Ctx {
            server_id: &sid,
            server: &url,
            workspace: &mut ws,
            session: None,
            images: &images,
            config: &config,
        };
        let mut d = DeleteAccount::default();
        let _ = d.update(Msg::Open, &mut ctx);
        assert!(d.open);
        // Nothing typed: no call.
        let _ = d.update(Msg::Submit, &mut ctx);
        assert!(!d.busy);
        let _ = d.update(Msg::Secret("pw".into()), &mut ctx);
        let _ = d.update(Msg::Submit, &mut ctx);
        assert!(d.busy);
        let wrong = failure(
            400,
            include_str!(
                "../../../../rocketchat/tests/fixtures/ugc/users_delete_own_account_invalid_password.json"
            ),
        );
        let _ = d.update(Msg::Done(Err(wrong)), &mut ctx);
        assert!(!d.busy);
        assert_eq!(
            d.error.as_deref(),
            Some(&*t!("legal.delete_wrong_password"))
        );
        // Typing again clears the error.
        let _ = d.update(Msg::Secret("pw2".into()), &mut ctx);
        assert!(d.error.is_none());
        let _ = d.update(Msg::Submit, &mut ctx);
        let owner = failure(
            400,
            include_str!(
                "../../../../rocketchat/tests/fixtures/ugc/users_delete_own_account_last_owner.json"
            ),
        );
        let _ = d.update(Msg::Done(Err(owner)), &mut ctx);
        let rooms = d.owner_rooms.clone().unwrap();
        assert_eq!(rooms.should_change_owner, vec!["rkprobe-g5851".to_owned()]);
        assert!(d.error.is_none());
        let _ = d.update(Msg::ConfirmRelinquish, &mut ctx);
        assert!(d.busy);
        let a = d.update(Msg::Done(Ok(())), &mut ctx);
        // Signed out without a logout call: the server dropped the token.
        assert!(
            a.effects
                .iter()
                .any(|e| matches!(e, Effect::AccountDeleted))
        );
        assert!(!a.effects.iter().any(|e| matches!(e, Effect::Logout { .. })));
        assert!(!d.open && !d.busy);
    }
}
