//! Second-factor confirmation of admin calls (docs/admin.md §2.3): a call
//! fails with `totp-required`, the prompt asks for the code (or the password,
//! sent as its SHA-256), and the same call is replayed with the code. The
//! password/code is dropped right after the retry and never logged.

use iced::widget::{column, text};
use iced::{Element, Task};
use rocketchat::auth::TwoFactorCode;
use rocketchat::models::admin::{AdminUser, SettingChange, UserUpdate};
use rocketchat::{Error as RcError, TwoFactorChallenge, TwoFactorMethod};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, TEXT_BODY};
use rustket_ui::widget::{dialog, primary_button, secondary_button, text_field};

use super::Message;
use crate::component::Ctx;
use crate::error::{Failure, Res};

pub const INPUT_ID: &str = "admin-2fa";

/// A call that may ask for a second factor.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    SaveSettings(Vec<SettingChange>),
    UpdateUser {
        id: String,
        data: Box<UserUpdate>,
        relinquish: bool,
    },
    ResetTotp(String),
    ResetE2e(String),
    LdapSync,
    AddCustomOAuth(String),
    RemoveCustomOAuth(String),
    RefreshOAuth,
}

#[derive(Debug, Clone)]
pub enum Done {
    Ok,
    User(Box<AdminUser>),
    Text(String),
}

pub fn run(ctx: &Ctx<'_>, op: Op, code: Option<TwoFactorCode>) -> Task<Message> {
    let tag = op.clone();
    ctx.rest_task(
        move |rest| async move {
            let c = code.as_ref();
            match op {
                Op::SaveSettings(changes) => {
                    rest.save_settings(&changes, c).await.map(|()| Done::Ok)
                }
                Op::UpdateUser {
                    id,
                    data,
                    relinquish,
                } => rest
                    .admin_user_update(&id, &data, relinquish, c)
                    .await
                    .map(|u| Done::User(Box::new(u))),
                Op::ResetTotp(id) => rest.admin_reset_totp(&id, c).await.map(|()| Done::Ok),
                Op::ResetE2e(id) => rest.admin_reset_e2e_key(&id, c).await.map(|()| Done::Ok),
                Op::LdapSync => rest.ldap_sync_now(c).await.map(Done::Text),
                Op::AddCustomOAuth(name) => rest
                    .settings_add_custom_oauth(&name, c)
                    .await
                    .map(|()| Done::Ok),
                Op::RemoveCustomOAuth(name) => rest
                    .settings_remove_custom_oauth(&name, c)
                    .await
                    .map(|()| Done::Ok),
                Op::RefreshOAuth => rest
                    .settings_refresh_oauth_services(c)
                    .await
                    .map(|()| Done::Ok),
            }
        },
        move |r| Message::Guarded(tag, r),
    )
}

#[derive(Debug, Clone)]
pub enum Msg {
    Code(String),
    Submit,
    Cancel,
}

/// The open prompt for `op`.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub op: Op,
    pub method: TwoFactorMethod,
    pub email_or_username: Option<String>,
    pub code: String,
    pub error: Option<String>,
    pub busy: bool,
}

impl Prompt {
    pub fn new(op: Op, ch: &TwoFactorChallenge) -> Self {
        let method = match &ch.method {
            TwoFactorMethod::Unknown => ch
                .available_methods
                .first()
                .cloned()
                .unwrap_or(TwoFactorMethod::Totp),
            m => m.clone(),
        };
        Self {
            op,
            method,
            email_or_username: ch.email_or_username.clone(),
            code: String::new(),
            error: None,
            busy: false,
        }
    }

    pub fn code(&self) -> TwoFactorCode {
        match self.method {
            TwoFactorMethod::Password => TwoFactorCode::password(&self.code),
            ref m => TwoFactorCode::new(&self.code, m.clone()),
        }
    }
}

/// What to do with the result of a guarded call.
pub enum Outcome {
    Prompt(Box<Prompt>),
    /// Wrong code: the open prompt stays with an error.
    Invalid,
    /// Success or a real failure: close the prompt, hand it to the page.
    Done(Res<Done>),
}

pub fn classify(op: &Op, prompt: Option<&Prompt>, r: Res<Done>) -> Outcome {
    let open = prompt.is_some_and(|p| &p.op == op);
    let Err(f) = &r else {
        return Outcome::Done(r);
    };
    match f.error() {
        RcError::TwoFactorInvalid(_) if open => Outcome::Invalid,
        RcError::TwoFactorRequired(ch) | RcError::TwoFactorInvalid(ch) => {
            let mut p = Prompt::new(op.clone(), ch);
            if matches!(f.error(), RcError::TwoFactorInvalid(_)) {
                p.error = Some(t!("admin.tfa_invalid").into_owned());
            }
            Outcome::Prompt(Box::new(p))
        }
        _ => Outcome::Done(Err(Failure::clone(f))),
    }
}

pub fn view(p: &Prompt) -> Element<'_, Message> {
    let tf = Message::Guard;
    let password = p.method == TwoFactorMethod::Password;
    let (help, label) = match p.method {
        TwoFactorMethod::Email => (
            t!(
                "admin.tfa_email_help",
                who = p.email_or_username.clone().unwrap_or_default()
            ),
            t!("admin.tfa_code"),
        ),
        TwoFactorMethod::Password => (t!("admin.tfa_password_help"), t!("admin.password")),
        _ => (t!("admin.tfa_totp_help"), t!("admin.tfa_code")),
    };
    let body = column![
        text(help).size(TEXT_BODY).style(style::text_secondary),
        text_field(label, &p.code, move |s| tf(Msg::Code(s)))
            .password(password)
            .placeholder(if password { "" } else { "123456" })
            .id(INPUT_ID)
            .error(p.error.clone())
            .on_submit(tf(Msg::Submit)),
    ]
    .spacing(SPACE_3);
    dialog(
        t!("admin.tfa_title"),
        body,
        vec![
            secondary_button(t!("admin.cancel"), Some(tf(Msg::Cancel))).into(),
            primary_button(
                t!("admin.tfa_verify"),
                (!p.code.trim().is_empty() && !p.busy).then_some(tf(Msg::Submit)),
            )
            .loading(p.busy)
            .into(),
        ],
        Some(tf(Msg::Cancel)),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn challenge(method: &str) -> TwoFactorChallenge {
        TwoFactorChallenge::from_details(Some(&serde_json::json!({
            "method": method, "codeGenerated": false, "availableMethods": []
        })))
    }

    #[test]
    fn prompt_flow() {
        let op = Op::ResetTotp("u1".into());
        let r: Res<Done> = Err(Failure::from(RcError::TwoFactorRequired(challenge(
            "password",
        ))));
        let Outcome::Prompt(mut p) = classify(&op, None, r) else {
            panic!("prompt expected");
        };
        assert_eq!(p.method, TwoFactorMethod::Password);
        p.code = "adminpass".into();
        assert_eq!(p.code().code, rocketchat::auth::sha256_hex("adminpass"));
        let r: Res<Done> = Err(Failure::from(RcError::TwoFactorInvalid(challenge(
            "password",
        ))));
        assert!(matches!(classify(&op, Some(&p), r), Outcome::Invalid));
        // Another op's failure does not keep this prompt.
        let other = Op::ResetE2e("u1".into());
        let r: Res<Done> = Err(Failure::from(RcError::Protocol("x".into())));
        assert!(matches!(
            classify(&other, Some(&p), r),
            Outcome::Done(Err(_))
        ));
        assert!(matches!(
            classify(&op, Some(&p), Ok(Done::Ok)),
            Outcome::Done(Ok(_))
        ));
    }
}
